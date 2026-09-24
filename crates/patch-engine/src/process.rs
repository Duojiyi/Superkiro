//! Kiro process lifecycle, launcher injection, and safe restart (Spec §4.1, §9).
//!
//! Features:
//! - Launches Kiro IDE detached with full environment variable injection.
//! - Checks and enforces running state boundary (no modifying files while running).
//! - Graceful process termination and restart assistant.

use crate::detect::KiroInstallation;
use crate::patch::get_launcher_env;
use crate::runtime::ProcessState;
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

    /// A Kiro window is still on screen, most likely its save prompt. Only the user may
    /// decide to discard what it is asking about.
    #[error("Kiro is still open; it may be asking whether to save changes")]
    StillOpen,

    /// Kiro runs with higher privileges than this client, so it can be neither asked to
    /// close nor ended from here.
    #[error("Kiro is running as administrator and cannot be closed from here; close it yourself and retry")]
    Elevated,

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
    for key in inherited_editor_variables(std::env::vars_os().map(|(key, _)| key)) {
        cmd.env_remove(key);
    }
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

/// Variables that make sense only inside the editor or Electron host that set them.
///
/// The client can itself be started from an editor's terminal. Inherited, these turn
/// Kiro into something else: `ELECTRON_RUN_AS_NODE` makes Kiro.exe run as plain Node and
/// exit at once, and `VSCODE_*` carries another editor's IPC hook, portable-mode flag and
/// profile paths. VS Code strips the same prefixes before launching its own children.
pub(crate) fn inherited_editor_variables(
    keys: impl Iterator<Item = std::ffi::OsString>,
) -> Vec<std::ffi::OsString> {
    keys.filter(|key| {
        let key = key.to_string_lossy().to_ascii_uppercase();
        key.starts_with("ELECTRON_") || key.starts_with("VSCODE_")
    })
    .collect()
}

/// How long a freshly launched Kiro must stay up before it counts as launched.
const LAUNCH_SURVIVAL: Duration = Duration::from_secs(3);

/// Spawn Kiro and confirm it is still running a moment later.
///
/// A launch used to be reported as done as soon as the process was created. A Kiro that
/// exits immediately — a broken installation, or one started as plain Node — then looked
/// like a success, and the customer was told to go and use an editor that was not there.
pub fn spawn_and_confirm(cmd: &mut Command) -> Result<(), ProcessError> {
    spawn_and_confirm_within(cmd, LAUNCH_SURVIVAL, || {
        crate::runtime::detect_kiro_process_state() == ProcessState::Running
    })
}

fn spawn_and_confirm_within(
    cmd: &mut Command,
    survival: Duration,
    kiro_running: impl Fn() -> bool,
) -> Result<(), ProcessError> {
    let mut child = cmd.spawn()?;
    let deadline = Instant::now() + survival;
    while Instant::now() < deadline {
        match child.try_wait()? {
            None => thread::sleep(Duration::from_millis(50)),
            // A second instance hands its arguments to the running one and exits cleanly.
            Some(status) if status.success() && kiro_running() => return Ok(()),
            Some(status) => {
                return Err(ProcessError::InvalidConfiguration(format!(
                    "Kiro exited immediately after launch ({status})"
                )))
            }
        }
    }
    Ok(())
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

/// Ask Kiro to close the way its own close button would, and wait up to `timeout`.
/// Never forces: an editor that is still open when time runs out stays open.
pub fn stop_kiro(timeout: Duration) -> Result<(), ProcessError> {
    let deadline = Instant::now() + timeout;
    match crate::runtime::detect_kiro_process_state_until(deadline) {
        ProcessState::Stopped => return Ok(()),
        ProcessState::Unknown => return Err(ProcessError::UnknownState),
        ProcessState::Running => {}
    }
    request_graceful_close(deadline)?;
    while Instant::now() < deadline {
        match crate::runtime::detect_kiro_process_state_until(deadline) {
            ProcessState::Stopped => return Ok(()),
            ProcessState::Unknown => return Err(ProcessError::UnknownState),
            ProcessState::Running => thread::sleep(
                Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now())),
            ),
        }
    }
    Err(ProcessError::TerminateTimeout)
}

/// Deliver the editor's own close request, once. Never re-sent: a user who cancels a
/// save prompt has answered, and asking again would only put the prompt back.
fn request_graceful_close(deadline: Instant) -> Result<(), ProcessError> {
    #[cfg(windows)]
    {
        let _ = deadline;
        let pids = crate::windows_process::session_processes("Kiro.exe")
            .map_err(|_| ProcessError::UnknownState)?;
        let (mut delivered, mut denied) = (false, false);
        for window in crate::windows_process::top_level_windows(&pids) {
            // Unowned, because an owned window is a dialog, and closing a save prompt is
            // pressing Cancel. Enabled, because a disabled window has a modal open, and
            // what happens to that is the dialog's business. Every editor window is
            // asked at once: that is what quitting does.
            if window.visible && !window.owned && window.enabled {
                match crate::windows_process::request_close(&window) {
                    Ok(()) => delivered = true,
                    Err(error) if error.raw_os_error() == Some(5) => denied = true,
                    Err(_) => {}
                }
            }
        }
        if denied && !delivered {
            return Err(ProcessError::Elevated);
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        // An application-level quit lets the editor keep unsaved work as it does on exit.
        run_helper_until(
            Command::new("/usr/bin/osascript").args(["-e", "tell application \"Kiro\" to quit"]),
            deadline,
        )
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        run_helper_until(
            Command::new("pkill").args(["-TERM", "-x", "kiro"]),
            deadline,
        )
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = deadline;
        Err(ProcessError::UnknownState)
    }
}

/// Whether a Kiro window is on screen. `Some(false)` only on positive evidence that none
/// is; read twice a second apart, because Electron creates windows hidden until they
/// are ready to show.
///
/// Elsewhere it cannot be proven: macOS does not count minimized or other-Space windows
/// as on screen, and Wayland has no global window list. `None` counts as visible.
fn kiro_window_visible() -> Option<bool> {
    #[cfg(windows)]
    {
        let visible = || -> Option<bool> {
            let pids = crate::windows_process::session_processes("Kiro.exe").ok()?;
            Some(
                crate::windows_process::top_level_windows(&pids)
                    .iter()
                    .any(|window| window.visible),
            )
        };
        if visible()? {
            return Some(true);
        }
        thread::sleep(Duration::from_secs(1));
        visible()
    }
    #[cfg(not(windows))]
    None
}

/// How long a single observation may spend retrying before it is genuinely
/// unobservable. Kept well clear of every wait budget so the two never conflate.
const OBSERVATION_BUDGET: Duration = Duration::from_secs(2);
/// Only long enough to deliver the close request.
const SIGNAL_BUDGET: Duration = Duration::from_secs(10);

/// Timings for [`stop_with`]. The worst case stays well inside the client's 125s.
pub(crate) struct StopTimings {
    /// How long the editor gets to close itself, save prompt included.
    pub grace: Duration,
    /// How long the process count may sit still before a wait counts as stalled.
    /// Teardown scales with workspace size, extensions and disk; a measured shutdown
    /// of this editor took 8-11s to leave the process table after its window closed.
    pub stall: Duration,
    /// How long headless teardown may run before it is ended.
    pub headless_ceiling: Duration,
    /// How long to wait for processes to disappear once they have been ended.
    pub force_ceiling: Duration,
}

const TIMINGS: StopTimings = StopTimings {
    grace: Duration::from_secs(30),
    stall: Duration::from_secs(10),
    headless_ceiling: Duration::from_secs(30),
    force_ceiling: Duration::from_secs(20),
};

/// What the stop policy needs from the system. A seam, so every branch of the policy
/// can be tested without a real editor.
pub(crate) trait KiroControl {
    fn observe(&mut self) -> StopObservation;
    fn request_close(&mut self) -> Result<(), ProcessError>;
    /// `Some(false)` only when it is certain no Kiro window is on screen.
    fn window_visible(&mut self) -> Option<bool>;
    fn force(&mut self) -> Result<(), ProcessError>;
}

struct System;

impl KiroControl for System {
    fn observe(&mut self) -> StopObservation {
        restore_process_observation(Instant::now() + OBSERVATION_BUDGET)
    }
    fn request_close(&mut self) -> Result<(), ProcessError> {
        request_graceful_close(Instant::now() + SIGNAL_BUDGET)
    }
    fn window_visible(&mut self) -> Option<bool> {
        kiro_window_visible()
    }
    fn force(&mut self) -> Result<(), ProcessError> {
        force_kiro_for_restore(Instant::now() + SIGNAL_BUDGET)
    }
}

/// Close Kiro for a restore or unbind.
///
/// `force_confirmed` is the user's second, specific confirmation, given only after being
/// told Kiro would not close, that it may be ended even with a window open.
pub fn stop_kiro_for_restore(force_confirmed: bool) -> Result<(), ProcessError> {
    stop_with(&mut System, force_confirmed, &TIMINGS)
}

/// Close Kiro before a takeover. Never ends an editor with a window on screen; only the
/// headless remains of one that has already closed.
pub fn stop_kiro_for_takeover() -> Result<(), ProcessError> {
    stop_with(&mut System, false, &TIMINGS)
}

/// One observation of the Kiro processes. `Running` carries the count so a
/// shutdown still making progress can be told apart from one that has stalled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StopObservation {
    Stopped,
    Running(usize),
    Unobservable,
}

fn stop_with(
    control: &mut impl KiroControl,
    force_confirmed: bool,
    timings: &StopTimings,
) -> Result<(), ProcessError> {
    match control.observe() {
        StopObservation::Stopped => return Ok(()),
        StopObservation::Unobservable => return Err(ProcessError::UnknownState),
        StopObservation::Running(_) => {}
    }
    if force_confirmed {
        // The user has seen that Kiro would not close and chose to end it. Asking again
        // would only bring the prompt back, and waiting would only make them wait.
        return force_until_gone(control, timings);
    }
    control.request_close()?;
    match wait_for_exit(control, None, timings.grace) {
        Ok(()) => return Ok(()),
        Err(ProcessError::TerminateTimeout) => {}
        // Unable to look: with no evidence of what is on screen, nothing is forced.
        Err(error) => return Err(error),
    }
    // A window still on screen is an editor still in use, most likely showing its save
    // prompt. Ending it would discard exactly what the prompt is asking about.
    if control.window_visible() != Some(false) {
        return Err(ProcessError::StillOpen);
    }
    // Every window is gone, so what remains is teardown: extensions deactivating,
    // terminals and language servers shutting down. Let it finish, and end it only once
    // it has stopped making progress.
    match wait_for_exit(control, Some(timings.stall), timings.headless_ceiling) {
        Ok(()) => return Ok(()),
        Err(ProcessError::TerminateTimeout) => {}
        Err(error) => return Err(error),
    }
    // The user may have opened Kiro again in the meantime.
    if control.window_visible() != Some(false) {
        return Err(ProcessError::StillOpen);
    }
    force_until_gone(control, timings)
}

fn force_until_gone(
    control: &mut impl KiroControl,
    timings: &StopTimings,
) -> Result<(), ProcessError> {
    let deadline = Instant::now() + timings.force_ceiling;
    let mut result = control.force();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match wait_for_exit(control, Some(timings.stall), remaining) {
            Ok(()) => return Ok(()),
            // A process born after the sweep survives it: sweep again while there is time.
            Err(ProcessError::TerminateTimeout) if remaining > timings.stall => {
                result = result.and(control.force());
            }
            // The final state, not whether each termination call succeeded, decides.
            Err(error) => return result.and(Err(error)),
        }
    }
}

/// Wait for every Kiro process to exit. `stall` bounds how long the count may sit still;
/// `None` waits out the whole `ceiling`, which is what a save prompt needs.
fn wait_for_exit(
    control: &mut impl KiroControl,
    stall: Option<Duration>,
    ceiling: Duration,
) -> Result<(), ProcessError> {
    let start = Instant::now();
    let mut previous = None;
    let mut last_progress = start;
    loop {
        match control.observe() {
            StopObservation::Stopped => return Ok(()),
            StopObservation::Unobservable => return Err(ProcessError::UnknownState),
            StopObservation::Running(remaining) => {
                // Any change is progress. A measured shutdown went 9 -> 11 before falling.
                if previous != Some(remaining) {
                    previous = Some(remaining);
                    last_progress = Instant::now();
                }
            }
        }
        let now = Instant::now();
        if now.duration_since(start) >= ceiling
            || stall.is_some_and(|stall| now.duration_since(last_progress) >= stall)
        {
            // Out of time is not the same as unobservable: we watched the whole way.
            return Err(ProcessError::TerminateTimeout);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn restore_process_observation(deadline: Instant) -> StopObservation {
    #[cfg(unix)]
    loop {
        // One failed `ps` is not evidence about the system; retry within the budget, as
        // the Windows observation does.
        match restore_pids(deadline) {
            Ok(pids) if pids.is_empty() => return StopObservation::Stopped,
            Ok(pids) => return StopObservation::Running(pids.len()),
            Err(_) if Instant::now() + Duration::from_millis(50) < deadline => {
                thread::sleep(Duration::from_millis(50))
            }
            Err(_) => return StopObservation::Unobservable,
        }
    }
    #[cfg(windows)]
    match crate::runtime::process_count_until("Kiro.exe", deadline) {
        Some(0) => StopObservation::Stopped,
        Some(remaining) => StopObservation::Running(remaining),
        None => StopObservation::Unobservable,
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = deadline;
        StopObservation::Unobservable
    }
}

// Match executable identity only, never argv, generic Electron/node, or process trees.
#[cfg(any(unix, test))]
fn restore_target(name: &str, macos: bool) -> bool {
    if !macos {
        return matches!(name, "kiro" | "Kiro");
    }
    if name.ends_with("/Kiro.app/Contents/MacOS/Kiro")
        || name.ends_with("/Kiro.app/Contents/MacOS/Electron")
    {
        return true;
    }
    let Some((_, helper)) = name.rsplit_once("/Kiro.app/Contents/Frameworks/") else {
        return false;
    };
    [
        "Kiro Helper",
        "Kiro Helper (GPU)",
        "Kiro Helper (Renderer)",
        "Kiro Helper (Plugin)",
    ]
    .iter()
    .any(|name| helper == format!("{name}.app/Contents/MacOS/{name}"))
}

#[cfg(any(unix, test))]
fn parse_restore_pids(text: &str, macos: bool) -> Result<Vec<u32>, ProcessError> {
    let mut pids = Vec::new();
    let mut found = false;
    for line in text.lines().map(str::trim).filter(|s| !s.is_empty()) {
        let (pid, name) = line
            .split_once(char::is_whitespace)
            .ok_or(ProcessError::UnknownState)?;
        let pid: u32 = pid.parse().map_err(|_| ProcessError::UnknownState)?;
        if name.trim().is_empty() {
            return Err(ProcessError::UnknownState);
        }
        found = true;
        if restore_target(name.trim(), macos) {
            if pid <= 1 {
                return Err(ProcessError::UnknownState);
            }
            pids.push(pid);
        }
    }
    if !found {
        return Err(ProcessError::UnknownState);
    }
    Ok(pids)
}

#[cfg(unix)]
fn restore_pids(deadline: Instant) -> Result<Vec<u32>, ProcessError> {
    let bytes = capture_helper_until(
        Command::new("/bin/ps").args(["-A", "-ww", "-o", "pid=,comm="]),
        deadline,
    )?;
    parse_restore_pids(
        std::str::from_utf8(&bytes).map_err(|_| ProcessError::UnknownState)?,
        cfg!(target_os = "macos"),
    )
}

fn force_kiro_for_restore(deadline: Instant) -> Result<(), ProcessError> {
    #[cfg(windows)]
    {
        let _ = deadline;
        // This session's Kiro only, each checked by image name through a handle that
        // pins the process, so neither another user's editor nor a recycled PID is hit.
        let pids = crate::windows_process::session_processes("Kiro.exe")
            .map_err(|_| ProcessError::UnknownState)?;
        let mut result = Ok(());
        for pid in pids {
            match crate::windows_process::terminate_if_named(pid, "Kiro.exe") {
                Ok(()) => {}
                Err(error) if error.raw_os_error() == Some(5) => {
                    result = Err(ProcessError::Elevated)
                }
                // Exited between enumeration and termination; the final state decides.
                Err(_) => {}
            }
        }
        result
    }
    #[cfg(unix)]
    {
        let pids = restore_pids(deadline)?;
        if pids.is_empty() {
            return Ok(());
        }
        run_helper_until(
            Command::new("/bin/kill")
                .arg("-KILL")
                .args(pids.iter().map(u32::to_string)),
            deadline,
        )
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = deadline;
        Err(ProcessError::UnknownState)
    }
}

/// Bound the helper itself, not just the subsequent IDE wait. Only the helper
/// is killed on timeout; the IDE remains free to present its save/cancel dialog.
#[cfg(any(unix, test))]
fn run_helper_until(command: &mut Command, deadline: Instant) -> Result<(), ProcessError> {
    if Instant::now() >= deadline {
        return Err(ProcessError::TerminateTimeout);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    loop {
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        };
        if let Some(status) = status {
            return if status.success() {
                Ok(())
            } else {
                Err(ProcessError::TerminateTimeout)
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ProcessError::TerminateTimeout);
        }
        thread::sleep(
            Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

// Drain stdout concurrently so a full ps pipe cannot defeat the deadline.
#[cfg(any(unix, test))]
pub(crate) fn capture_helper_until(
    command: &mut Command,
    deadline: Instant,
) -> Result<Vec<u8>, ProcessError> {
    use std::io::Read;
    if Instant::now() >= deadline {
        return Err(ProcessError::TerminateTimeout);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let pipe = child.stdout.take().expect("piped stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = pipe
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = tx.send(result);
    });
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return Err(ProcessError::UnknownState),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ProcessError::TerminateTimeout);
        }
        thread::sleep(
            Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    let bytes = rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| ProcessError::TerminateTimeout)??;
    if bytes.len() > 1024 * 1024 {
        return Err(ProcessError::UnknownState);
    }
    Ok(bytes)
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

#[cfg(test)]
mod deadline_tests {
    use super::*;
    #[test]
    fn hanging_helper() {
        if std::env::var_os("SUPERKIRO_DEADLINE_FIXTURE").is_some() {
            thread::sleep(Duration::from_secs(60));
        }
    }
    #[test]
    fn helper_deadline_bounds_hang_and_reaps_child() {
        let start = Instant::now();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "process::deadline_tests::hanging_helper"])
            .env("SUPERKIRO_DEADLINE_FIXTURE", "1");
        assert!(matches!(
            run_helper_until(&mut command, start + Duration::from_millis(150)),
            Err(ProcessError::TerminateTimeout)
        ));
        assert!(start.elapsed() < Duration::from_secs(3));
        assert!(matches!(
            capture_helper_until(&mut command, Instant::now() + Duration::from_millis(100)),
            Err(ProcessError::TerminateTimeout)
        ));
        assert!(matches!(
            run_helper_until(&mut command, Instant::now()),
            Err(ProcessError::TerminateTimeout)
        ));
        let mut cancelled = Command::new(std::env::current_exe().unwrap());
        cancelled.arg("--not-a-valid-harness-argument");
        assert!(matches!(
            run_helper_until(&mut cancelled, Instant::now() + Duration::from_secs(3)),
            Err(ProcessError::TerminateTimeout)
        ));
    }
}

#[cfg(test)]
mod launch_tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn editor_host_variables_are_not_passed_to_kiro() {
        let parent = [
            "ELECTRON_RUN_AS_NODE",
            "electron_no_attach_console",
            "VSCODE_IPC_HOOK_CLI",
            "VSCODE_PORTABLE",
            "PATH",
            "AWS_PROFILE",
            "KIRO_HOME",
        ]
        .map(OsString::from);
        let removed: Vec<_> = inherited_editor_variables(parent.into_iter())
            .into_iter()
            .map(|key| key.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            removed,
            [
                "ELECTRON_RUN_AS_NODE",
                "electron_no_attach_console",
                "VSCODE_IPC_HOOK_CLI",
                "VSCODE_PORTABLE"
            ]
        );
    }

    fn exits_with(code: i32) -> Command {
        if cfg!(windows) {
            let mut command = Command::new("cmd");
            command.args(["/C", &format!("exit {code}")]);
            command
        } else {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", &format!("exit {code}")]);
            command
        }
    }

    fn keeps_running() -> Command {
        if cfg!(windows) {
            let mut command = Command::new("cmd");
            command.args(["/C", "ping -n 3 127.0.0.1 >NUL"]);
            command
        } else {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "sleep 2"]);
            command
        }
    }

    /// A Kiro that dies on start used to be reported as launched, so the customer was told
    /// to use an editor that was not there.
    #[test]
    fn a_kiro_that_exits_at_once_is_not_a_successful_launch() {
        let survival = Duration::from_millis(800);
        assert!(spawn_and_confirm_within(&mut exits_with(1), survival, || true).is_err());
        // Exiting cleanly is only a hand-off if another Kiro is there to receive it.
        assert!(spawn_and_confirm_within(&mut exits_with(0), survival, || false).is_err());
        assert!(spawn_and_confirm_within(&mut exits_with(0), survival, || true).is_ok());
        assert!(spawn_and_confirm_within(&mut keeps_running(), survival, || false).is_ok());
    }
}

#[cfg(test)]
mod restore_tests {
    use super::*;

    /// Scripted system: `counts` is what each observation reports (the last one repeats),
    /// `visible` what each window check reports (the last one repeats).
    struct Fake {
        counts: Vec<StopObservation>,
        visible: Vec<Option<bool>>,
        close_result: Result<(), ProcessError>,
        /// After this many forces, every later observation reports Stopped.
        stops_after_forces: Option<usize>,
        calls: Vec<&'static str>,
        forces: usize,
    }

    impl Fake {
        fn new(counts: &[StopObservation], visible: &[Option<bool>]) -> Self {
            Self {
                counts: counts.to_vec(),
                visible: visible.to_vec(),
                close_result: Ok(()),
                stops_after_forces: None,
                calls: Vec::new(),
                forces: 0,
            }
        }
    }

    impl KiroControl for Fake {
        fn observe(&mut self) -> StopObservation {
            self.calls.push("observe");
            if self.stops_after_forces.is_some_and(|n| self.forces >= n) {
                return StopObservation::Stopped;
            }
            if self.counts.len() > 1 {
                self.counts.remove(0)
            } else {
                self.counts[0]
            }
        }
        fn request_close(&mut self) -> Result<(), ProcessError> {
            self.calls.push("close");
            std::mem::replace(&mut self.close_result, Ok(()))
        }
        fn window_visible(&mut self) -> Option<bool> {
            self.calls.push("visible");
            if self.visible.len() > 1 {
                self.visible.remove(0)
            } else {
                self.visible[0]
            }
        }
        fn force(&mut self) -> Result<(), ProcessError> {
            self.calls.push("force");
            self.forces += 1;
            Ok(())
        }
    }

    const FAST: StopTimings = StopTimings {
        grace: Duration::from_millis(150),
        stall: Duration::from_millis(100),
        headless_ceiling: Duration::from_millis(300),
        force_ceiling: Duration::from_millis(600),
    };
    const RUNNING: StopObservation = StopObservation::Running(3);

    #[test]
    fn nothing_running_or_nothing_observable_means_nothing_is_sent() {
        let mut fake = Fake::new(&[StopObservation::Stopped], &[Some(false)]);
        assert!(stop_with(&mut fake, false, &FAST).is_ok());
        assert_eq!(fake.calls, ["observe"]);
        let mut fake = Fake::new(&[StopObservation::Unobservable], &[Some(false)]);
        assert!(matches!(
            stop_with(&mut fake, false, &FAST),
            Err(ProcessError::UnknownState)
        ));
        assert_eq!(fake.calls, ["observe"]);
    }

    #[test]
    fn an_editor_that_closes_itself_is_never_forced() {
        let mut fake = Fake::new(
            &[RUNNING, RUNNING, StopObservation::Stopped],
            &[Some(false)],
        );
        assert!(stop_with(&mut fake, false, &FAST).is_ok());
        assert!(!fake.calls.contains(&"force"));
        assert_eq!(fake.calls.iter().filter(|c| **c == "close").count(), 1);
    }

    /// The case that used to lose work: the editor is showing its save prompt when time
    /// runs out. A window on screen is never ended without the user saying so.
    #[test]
    fn a_window_still_on_screen_is_reported_not_killed() {
        let mut fake = Fake::new(&[RUNNING], &[Some(true)]);
        assert!(matches!(
            stop_with(&mut fake, false, &FAST),
            Err(ProcessError::StillOpen)
        ));
        assert!(!fake.calls.contains(&"force"));
        assert_eq!(
            fake.calls.iter().filter(|c| **c == "close").count(),
            1,
            "the close request must never be repeated"
        );
    }

    /// Where windows cannot be seen (macOS, Linux) there is no evidence Kiro is headless.
    #[test]
    fn unknown_visibility_counts_as_visible() {
        let mut fake = Fake::new(&[RUNNING], &[None]);
        assert!(matches!(
            stop_with(&mut fake, false, &FAST),
            Err(ProcessError::StillOpen)
        ));
        assert!(!fake.calls.contains(&"force"));
    }

    /// Unable to look after asking Kiro to close: no evidence, no force.
    #[test]
    fn losing_sight_of_the_processes_never_leads_to_a_kill() {
        let mut fake = Fake::new(&[RUNNING, StopObservation::Unobservable], &[Some(false)]);
        assert!(matches!(
            stop_with(&mut fake, false, &FAST),
            Err(ProcessError::UnknownState)
        ));
        assert!(!fake.calls.contains(&"force"));
    }

    /// Every window is gone and the remaining processes have stopped making progress:
    /// ending that teardown loses no document.
    #[test]
    fn stalled_headless_teardown_is_ended() {
        let mut fake = Fake::new(&[RUNNING], &[Some(false)]);
        fake.stops_after_forces = Some(1);
        assert!(stop_with(&mut fake, false, &FAST).is_ok());
        assert_eq!(fake.forces, 1);
    }

    /// Teardown still making progress is waited for, not cut short.
    #[test]
    fn headless_teardown_making_progress_is_left_to_finish() {
        let mut counts: Vec<_> = std::iter::repeat_n(RUNNING, 4).collect();
        counts.extend([4usize, 3, 2, 1].map(StopObservation::Running));
        counts.push(StopObservation::Stopped);
        // Hold at 3 through the grace window, then count down after it.
        let mut fake = Fake::new(&counts, &[Some(false)]);
        let timings = StopTimings {
            grace: Duration::from_millis(120),
            ..FAST
        };
        assert!(stop_with(&mut fake, false, &timings).is_ok());
        assert!(!fake.calls.contains(&"force"));
    }

    /// A window that reappears while teardown was being waited for is the user opening
    /// Kiro again.
    #[test]
    fn a_reopened_window_cancels_the_force() {
        let mut fake = Fake::new(&[RUNNING], &[Some(false), Some(true)]);
        assert!(matches!(
            stop_with(&mut fake, false, &FAST),
            Err(ProcessError::StillOpen)
        ));
        assert!(!fake.calls.contains(&"force"));
    }

    /// With the user's explicit confirmation there is no second close request and no
    /// second wait.
    #[test]
    fn a_confirmed_force_does_not_ask_again() {
        let mut fake = Fake::new(&[RUNNING], &[Some(true)]);
        fake.stops_after_forces = Some(1);
        assert!(stop_with(&mut fake, true, &FAST).is_ok());
        assert!(!fake.calls.contains(&"close"));
        assert!(!fake.calls.contains(&"visible"));
        assert_eq!(fake.forces, 1);
    }

    /// An elevated Kiro cannot be asked to close; say so at once instead of waiting.
    #[test]
    fn an_elevated_editor_is_reported_immediately() {
        let mut fake = Fake::new(&[RUNNING], &[Some(true)]);
        fake.close_result = Err(ProcessError::Elevated);
        let started = Instant::now();
        assert!(matches!(
            stop_with(&mut fake, false, &FAST),
            Err(ProcessError::Elevated)
        ));
        assert!(started.elapsed() < FAST.grace);
    }

    /// A process born after a sweep survives it; sweep again while there is time.
    #[test]
    fn a_survivor_of_the_first_sweep_is_swept_again() {
        let mut fake = Fake::new(&[StopObservation::Running(1)], &[Some(false)]);
        fake.stops_after_forces = Some(2);
        let timings = StopTimings {
            force_ceiling: Duration::from_millis(900),
            ..FAST
        };
        assert!(stop_with(&mut fake, true, &timings).is_ok());
        assert!(fake.forces >= 2);
    }

    /// Running out of time is not the same as being unable to look.
    #[test]
    fn exhausting_every_wait_reports_termination_timeout() {
        let mut fake = Fake::new(&[RUNNING], &[Some(false)]);
        assert!(matches!(
            stop_with(&mut fake, true, &FAST),
            Err(ProcessError::TerminateTimeout)
        ));
    }

    /// The worst case must fit inside the 125s the client waits before giving up.
    #[test]
    fn the_whole_sequence_fits_inside_the_client_timeout() {
        let t = &TIMINGS;
        let worst = t.grace
            + t.headless_ceiling
            + t.force_ceiling
            + SIGNAL_BUDGET
            + Duration::from_secs(2 * 2); // two double-read visibility checks
        assert!(worst < Duration::from_secs(110), "{worst:?}");
    }

    #[test]
    fn unix_targets_exclude_arguments_and_other_electron_apps() {
        assert_eq!(
            parse_restore_pids(
                "1 init\n20 kiro\n21 Kiro\n22 superkiro\n23 node\n24 /usr/bin/python kiro\n",
                false
            )
            .unwrap(),
            [20, 21]
        );
        let listing = "0 kernel_task\n1 /sbin/launchd\n20 /Applications/Kiro.app/Contents/MacOS/Electron\n21 /Volumes/My Disk/Kiro.app/Contents/Frameworks/Kiro Helper (GPU).app/Contents/MacOS/Kiro Helper (GPU)\n22 /Applications/Other.app/Contents/MacOS/Electron\n23 /usr/bin/python /Applications/Kiro.app/example\n24 /Applications/Kiro.app/Contents/Frameworks/Other.app/Contents/MacOS/Other\n";
        assert_eq!(parse_restore_pids(listing, true).unwrap(), [20, 21]);
        for invalid in ["", "oops", "no kiro", "0 kiro", "22 "] {
            assert!(parse_restore_pids(invalid, false).is_err());
        }
    }
}
