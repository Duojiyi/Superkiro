//! Kiro process lifecycle, launcher injection, and safe restart (Spec §4.1, §9).
//!
//! Features:
//! - Launches Kiro IDE detached with full environment variable injection.
//! - Checks and enforces running state boundary (no modifying files while running).
//! - Graceful process termination and restart assistant.

use crate::detect::KiroInstallation;
use crate::patch::get_launcher_env;
use crate::runtime::{detect_kiro_process_state, ProcessState};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("Kiro executable not found at '{0}'")]
    ExecutableNotFound(String),

    #[error("I/O error spawning Kiro process: {0}")]
    SpawnFailed(#[from] std::io::Error),

    #[error("Timed out waiting for Kiro process to terminate")]
    TerminateTimeout,

    #[error("Cannot safely determine Kiro process state")]
    UnknownState,

    #[error("Invalid launch configuration: {0}")]
    InvalidConfiguration(String),
}

/// Launch Kiro IDE detached with injected BYOK redirection environment variables.
pub fn launch_kiro(
    installation: &KiroInstallation,
    gateway_url: &str,
    additional_args: &[&str],
) -> Result<u32, ProcessError> {
    let mut cmd = prepare_kiro_launch(installation, gateway_url, additional_args)?;
    let child = cmd.spawn()?;
    Ok(child.id())
}

/// Validate trust and build the launch command before stopping or modifying Kiro.
pub fn prepare_kiro_launch(
    installation: &KiroInstallation,
    gateway_url: &str,
    additional_args: &[&str],
) -> Result<Command, ProcessError> {
    if !installation.executable_path.is_file() {
        return Err(ProcessError::ExecutableNotFound(
            installation.executable_path.display().to_string(),
        ));
    }
    let gateway = crate::patch::validate_gateway_url(gateway_url)
        .map_err(|e| ProcessError::InvalidConfiguration(e.to_string()))?;
    let mut cmd = Command::new(&installation.executable_path);
    // The desktop bridge captures patch-cli output. IDE children must not keep
    // those pipe handles open after patch-cli exits.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    prevent_stdio_inheritance()?;
    cmd.envs(get_launcher_env(&gateway));
    if let Some(path) = std::env::var_os("KIRO_GATEWAY_CA_CERT") {
        let path = std::fs::canonicalize(path)?;
        let pem = std::fs::read(&path)?;
        let _ = crate::http::add_ca(reqwest::Client::builder(), &pem)
            .map_err(ProcessError::InvalidConfiguration)?;
        cmd.env("NODE_EXTRA_CA_CERTS", path);
    }
    cmd.args(additional_args);
    Ok(cmd)
}

// Windows may inherit other inheritable handles even when the child's standard
// streams are redirected. Clear inheritance on the bridge's original pipe handles.
#[cfg(windows)]
fn prevent_stdio_inheritance() -> std::io::Result<()> {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn SetHandleInformation(handle: *mut c_void, mask: u32, flags: u32) -> i32;
    }
    for which in [-10_i32, -11, -12] {
        let handle = unsafe { GetStdHandle(which as u32) };
        if !handle.is_null()
            && handle as isize != -1
            && unsafe { SetHandleInformation(handle, 1, 0) } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Attempt to stop running Kiro IDE processes across platforms.
///
/// Sends termination signal and waits up to `timeout` for all Kiro instances to exit.
pub fn stop_kiro(timeout: Duration) -> Result<(), ProcessError> {
    match detect_kiro_process_state() {
        ProcessState::Stopped => return Ok(()),
        ProcessState::Unknown => return Err(ProcessError::UnknownState),
        ProcessState::Running => {}
    }
    if cfg!(target_os = "windows") {
        // WM_CLOSE lets the editor present its save/cancel prompt. Never force-kill.
        let mut cmd = Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command",
            "Get-Process -Name Kiro -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | ForEach-Object { [void]$_.CloseMainWindow() }"]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        cmd.output()?;
    } else if cfg!(target_os = "macos") {
        // Native quit permits the editor to ask about unsaved work; never force-kill.
        let output = Command::new("osascript")
            .args(["-e", "tell application \"Kiro\" to quit"])
            .output()?;
        if !output.status.success() {
            return Err(ProcessError::TerminateTimeout);
        }
    } else {
        Command::new("pkill")
            .args(["-TERM", "-x", "kiro"])
            .output()?;
    }
    let start = Instant::now();
    while start.elapsed() < timeout {
        match detect_kiro_process_state() {
            ProcessState::Stopped => return Ok(()),
            ProcessState::Unknown => return Err(ProcessError::UnknownState),
            ProcessState::Running => thread::sleep(Duration::from_millis(200)),
        }
    }
    Err(ProcessError::TerminateTimeout)
}

/// Safe restart: terminates running Kiro (with timeout), then launches with BYOK environment.
pub fn restart_kiro(
    installation: &KiroInstallation,
    gateway_url: &str,
    additional_args: &[&str],
    timeout: Duration,
) -> Result<u32, ProcessError> {
    let mut command = prepare_kiro_launch(installation, gateway_url, additional_args)?;
    stop_kiro(timeout)?;
    Ok(command.spawn()?.id())
}
#[cfg(test)]
mod tests {
    use super::*;

    // Deliberately outlive this helper to reproduce captured-pipe inheritance.
    #[allow(clippy::zombie_processes)]
    #[test]
    fn detached_child_helper() {
        if std::env::var_os("KIRO_STDIO_REGRESSION").is_none() {
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let installation: KiroInstallation = serde_json::from_value(serde_json::json!({
            "install_dir": exe.parent().unwrap(), "executable_path": exe,
            "product_json_path": "unused", "version": "test", "win32_mutex_name": "test",
            "is_user_level": true
        }))
        .unwrap();
        let mut command = prepare_kiro_launch(
            &installation,
            "https://gateway.invalid",
            &["--exact", "process::tests::sleeping_child_helper"],
        )
        .unwrap();
        command.spawn().unwrap();
    }

    #[test]
    fn sleeping_child_helper() {
        if std::env::var_os("KIRO_STDIO_REGRESSION").is_some() {
            thread::sleep(Duration::from_secs(4));
        }
    }

    #[test]
    fn launched_child_does_not_hold_captured_cli_output_open() {
        let started = Instant::now();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "process::tests::detached_child_helper"])
            .env("KIRO_STDIO_REGRESSION", "1")
            .env_remove("KIRO_GATEWAY_CA_CERT")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "IDE child retained the CLI pipe"
        );
    }
}
