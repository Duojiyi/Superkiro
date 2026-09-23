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
    let deadline = Instant::now() + timeout;
    match crate::runtime::detect_kiro_process_state_until(deadline) {
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
        run_helper_until(&mut cmd, deadline)?;
    } else if cfg!(target_os = "macos") {
        // Native quit permits the editor to ask about unsaved work; never force-kill.
        run_helper_until(
            Command::new("osascript").args(["-e", "tell application \"Kiro\" to quit"]),
            deadline,
        )?;
    } else {
        run_helper_until(
            Command::new("pkill").args(["-TERM", "-x", "kiro"]),
            deadline,
        )?;
    }
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

/// The editor gets the same budget to close itself as the activate path allows
/// (`desktop.rs`); it is the same operation and the asymmetry only ever cost the
/// user unsaved work.
const RESTORE_GRACE: Duration = Duration::from_secs(30);
/// After a force-kill, give up only once the process count has stopped falling
/// for this long. Teardown scales with workspace size, extension count and disk,
/// so no fixed budget is right for every machine: a measured 11-process shutdown
/// took 15.7s to leave the process table on one developer machine.
const RESTORE_STALL: Duration = Duration::from_secs(10);
/// Absolute ceiling, so a machine that never finishes still returns an answer.
const RESTORE_CEILING: Duration = Duration::from_secs(60);
/// Only long enough to deliver WM_CLOSE; `wait_for_exit` owns the waiting, so the
/// helper's own budget must not double the user's grace window.
const RESTORE_SIGNAL_BUDGET: Duration = Duration::from_secs(10);
/// How long a single observation may spend retrying before it is genuinely
/// unobservable. Kept well clear of every wait budget so the two never conflate.
const OBSERVATION_BUDGET: Duration = Duration::from_secs(2);

/// Stop Kiro after explicit restore confirmation. Unlike restart, this may discard
/// unsaved work. Callers must obtain confirmation before invoking it.
pub fn stop_kiro_for_restore() -> Result<(), ProcessError> {
    stop_for_restore_with(
        restore_process_observation,
        || stop_kiro(RESTORE_SIGNAL_BUDGET),
        force_kiro_for_restore,
        RESTORE_GRACE,
        RESTORE_STALL,
        RESTORE_CEILING,
    )
}

/// One observation of the Kiro processes. `Running` carries the count so a
/// shutdown still making progress can be told apart from one that has stalled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StopObservation {
    Stopped,
    Running(usize),
    Unobservable,
}

/// Wait for every Kiro process to exit.
///
/// `stall` bounds how long the count may sit still before we give up; passing
/// `None` waits out the whole `ceiling` regardless. That distinction matters: a
/// count that stops falling during graceful shutdown most likely means the editor
/// is holding a save/cancel prompt in front of the user, which is the one moment
/// we must not treat as a stalled shutdown.
fn wait_for_exit(
    state: &mut impl FnMut(Instant) -> StopObservation,
    stall: Option<Duration>,
    ceiling: Duration,
) -> Result<(), ProcessError> {
    let start = Instant::now();
    let mut previous = None;
    let mut last_progress = start;
    loop {
        // Each observation gets its own budget, so a transient enumeration failure
        // is retried without the overall wait ever being reported as "cannot
        // observe". Running out of time is the loop's decision, not a sample's.
        match state(Instant::now() + OBSERVATION_BUDGET) {
            StopObservation::Stopped => return Ok(()),
            StopObservation::Unobservable => return Err(ProcessError::UnknownState),
            StopObservation::Running(remaining) => {
                // Any change means the shutdown is still moving. A measured
                // shutdown of this editor went 9 -> 11 before falling, so treating
                // only a decrease as progress would start the stall clock while
                // the system was plainly still working.
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
            // Out of time is not the same as unobservable: we watched the whole
            // way and the processes were still there.
            return Err(ProcessError::TerminateTimeout);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn stop_for_restore_with(
    mut state: impl FnMut(Instant) -> StopObservation,
    graceful: impl FnOnce() -> Result<(), ProcessError>,
    mut force: impl FnMut(Instant) -> Result<(), ProcessError>,
    grace: Duration,
    stall: Duration,
    ceiling: Duration,
) -> Result<(), ProcessError> {
    // One deadline for the whole sequence. Each phase draws from it, so a slow
    // shutdown cannot add its phases together and outlast the caller's own
    // patience — the client gives up at 125s and would blame the network.
    let overall = Instant::now() + ceiling;
    match state(Instant::now() + OBSERVATION_BUDGET) {
        StopObservation::Stopped => return Ok(()),
        StopObservation::Unobservable => return Err(ProcessError::UnknownState),
        StopObservation::Running(_) => {}
    }
    // A cancelled save prompt or failed graceful helper does not revoke consent.
    let _ = graceful();
    // The helper returns once WM_CLOSE has been delivered, not once Kiro is gone,
    // so the editor needs its own window to shut down before force is justified.
    let remaining = overall.saturating_duration_since(Instant::now());
    if wait_for_exit(&mut state, None, grace.min(remaining)).is_ok() {
        return Ok(());
    }
    // Being unable to look is not a reason to abandon a close the user has already
    // confirmed: the force needs no observation, and every write downstream is
    // still gated on its own verified stop. Returning here left the editor
    // half-closed with the takeover still applied.
    // `taskkill /F /IM` enumerates its targets once, and the editor spawns
    // processes while shutting down, so a survivor born after the sweep would sit
    // there until the stall expired. Re-issue against whatever is left rather
    // than refusing a close that one more sweep would finish.
    let mut force_result = force(overall);
    loop {
        let remaining = overall.saturating_duration_since(Instant::now());
        match wait_for_exit(&mut state, Some(stall), remaining) {
            Ok(()) => return Ok(()),
            Err(ProcessError::TerminateTimeout) if remaining > stall => {
                force_result = force_result.and(force(overall));
            }
            Err(error) => return force_result.and(Err(error)),
        }
    }
}

fn restore_process_observation(deadline: Instant) -> StopObservation {
    #[cfg(unix)]
    {
        match restore_pids(deadline) {
            Ok(pids) if pids.is_empty() => StopObservation::Stopped,
            Ok(pids) => StopObservation::Running(pids.len()),
            Err(_) => StopObservation::Unobservable,
        }
    }
    #[cfg(not(unix))]
    match crate::runtime::process_count_until("Kiro.exe", deadline) {
        Some(0) => StopObservation::Stopped,
        Some(remaining) => StopObservation::Running(remaining),
        None => StopObservation::Unobservable,
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

#[cfg(any(windows, test))]
fn restore_force_windows_command() -> Command {
    let mut command = Command::new("taskkill.exe");
    // No /T: unrelated children must not be terminated.
    command.args(["/F", "/IM", "Kiro.exe"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}

fn force_kiro_for_restore(deadline: Instant) -> Result<(), ProcessError> {
    #[cfg(windows)]
    {
        run_helper_until(&mut restore_force_windows_command(), deadline)
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
mod restore_tests {
    use super::*;
    use std::cell::RefCell;

    fn scenario(
        states: &[StopObservation],
        graceful_ok: bool,
        force_ok: bool,
    ) -> (Result<(), ProcessError>, Vec<&'static str>) {
        scenario_with(
            states,
            graceful_ok,
            force_ok,
            Duration::ZERO,
            Duration::ZERO,
        )
    }

    fn scenario_with(
        states: &[StopObservation],
        graceful_ok: bool,
        force_ok: bool,
        grace: Duration,
        ceiling: Duration,
    ) -> (Result<(), ProcessError>, Vec<&'static str>) {
        let mut states = states.iter().copied();
        let calls = RefCell::new(Vec::new());
        let result = stop_for_restore_with(
            |_| {
                calls.borrow_mut().push("state");
                states.next().expect("unexpected state check")
            },
            || {
                calls.borrow_mut().push("graceful");
                if graceful_ok {
                    Ok(())
                } else {
                    Err(ProcessError::TerminateTimeout)
                }
            },
            |_| {
                calls.borrow_mut().push("force");
                if force_ok {
                    Ok(())
                } else {
                    Err(ProcessError::TerminateTimeout)
                }
            },
            grace,
            Duration::ZERO,
            ceiling,
        );
        (result, calls.into_inner())
    }

    const RUNNING: StopObservation = StopObservation::Running(1);

    #[test]
    fn stopped_and_unknown_never_signal() {
        let (result, calls) = scenario(&[StopObservation::Stopped], true, true);
        assert!(result.is_ok());
        assert_eq!(calls, ["state"]);
        let (result, calls) = scenario(&[StopObservation::Unobservable], true, true);
        assert!(matches!(result, Err(ProcessError::UnknownState)));
        assert_eq!(calls, ["state"]);
    }

    #[test]
    fn graceful_exit_does_not_force() {
        let (result, calls) = scenario(&[RUNNING, StopObservation::Stopped], true, true);
        assert!(result.is_ok());
        assert_eq!(calls, ["state", "graceful", "state"]);
    }

    #[test]
    fn force_after_short_grace_requires_verified_exit() {
        for graceful_ok in [true, false] {
            for force_ok in [true, false] {
                let (result, calls) = scenario(
                    &[RUNNING, RUNNING, StopObservation::Stopped],
                    graceful_ok,
                    force_ok,
                );
                assert!(result.is_ok());
                assert_eq!(calls, ["state", "graceful", "state", "force", "state"]);
            }
        }
        for last in [RUNNING, StopObservation::Unobservable] {
            for force_ok in [true, false] {
                assert!(scenario(&[RUNNING, RUNNING, last], false, force_ok)
                    .0
                    .is_err());
            }
        }
        // Being unable to observe *after* the close was signalled must not abandon
        // it. The user already confirmed the force, `taskkill` needs no
        // observation, and every write downstream is gated on its own verified
        // stop — aborting here left the editor half-closed with the takeover
        // still applied. An unobservable system before anything is signalled is
        // a different matter, and is covered above.
        let (result, calls) = scenario(
            &[
                RUNNING,
                StopObservation::Unobservable,
                StopObservation::Stopped,
            ],
            false,
            true,
        );
        assert!(result.is_ok(), "{result:?}");
        assert!(calls.contains(&"force"));
    }

    /// Running out of time is not the same as being unable to look. Reporting a
    /// still-terminating editor as "cannot safely determine process state" both
    /// tells the user the wrong thing and — because every caller treats unknown as
    /// fatal — makes the one error that describes the real condition unreachable.
    #[test]
    fn exhausting_the_wait_reports_termination_timeout_not_unknown_state() {
        // Mimics the real observer, which reports a state it never sampled once
        // the deadline it was handed has passed.
        let observe = |deadline: Instant| {
            if Instant::now() >= deadline {
                StopObservation::Unobservable
            } else {
                RUNNING
            }
        };
        let result = stop_for_restore_with(
            observe,
            || Ok(()),
            |_| Ok(()),
            Duration::from_millis(60),
            Duration::from_secs(30),
            Duration::from_millis(120),
        );
        assert!(
            matches!(result, Err(ProcessError::TerminateTimeout)),
            "expected a termination timeout, got {result:?}"
        );
    }

    /// The graceful phase must wait out its whole window. A process count that
    /// stops falling there most likely means the editor is holding a save/cancel
    /// prompt in front of the user, which is the one moment not to force-kill.
    #[test]
    fn a_stalled_count_during_grace_does_not_shortcut_to_force() {
        let calls = RefCell::new(Vec::new());
        let started = Instant::now();
        let result = stop_for_restore_with(
            |_| {
                calls.borrow_mut().push("state");
                // Never exits, and never makes progress either.
                StopObservation::Running(3)
            },
            || Ok(()),
            |_| {
                calls.borrow_mut().push("force");
                Ok(())
            },
            Duration::from_millis(250),
            Duration::from_millis(10),
            Duration::from_millis(400),
        );
        assert!(result.is_err());
        let calls = calls.into_inner();
        let first_force = calls.iter().position(|c| *c == "force").expect("force");
        assert!(
            first_force > 2,
            "forced after {first_force} observations; the grace window was skipped"
        );
        assert!(started.elapsed() >= Duration::from_millis(250));
    }

    /// A shutdown still making progress must not be cut short, however long it
    /// takes: teardown scales with workspace size, extensions and disk.
    #[test]
    fn steady_progress_keeps_the_stall_clock_from_expiring() {
        let remaining = RefCell::new(8usize);
        let result = stop_for_restore_with(
            |_| {
                let mut remaining = remaining.borrow_mut();
                if *remaining == 0 {
                    return StopObservation::Stopped;
                }
                *remaining -= 1;
                StopObservation::Running(*remaining)
            },
            || Err(ProcessError::TerminateTimeout),
            |_| Ok(()),
            Duration::ZERO,
            Duration::from_millis(80),
            Duration::from_secs(30),
        );
        assert!(result.is_ok(), "{result:?}");
    }

    /// The measured shutdown went 9 -> 11 before it fell. A count that rises, or
    /// holds while one process blocks the rest, is a shutdown still working — not
    /// a stalled one — and cutting it off refuses a restore that was about to
    /// succeed, with most of the ceiling unspent.
    #[test]
    fn a_rising_then_falling_count_is_progress_not_a_stall() {
        let counts = RefCell::new(vec![9usize, 10, 11, 11, 10, 6, 2].into_iter());
        let result = stop_for_restore_with(
            |_| match counts.borrow_mut().next() {
                Some(remaining) => StopObservation::Running(remaining),
                None => StopObservation::Stopped,
            },
            || Err(ProcessError::TerminateTimeout),
            |_| Ok(()),
            Duration::ZERO,
            Duration::from_millis(120),
            Duration::from_secs(30),
        );
        assert!(result.is_ok(), "{result:?}");
    }

    /// `taskkill /F /IM` enumerates its targets once, so a helper spawned after
    /// that sweep survives it. Refusing the whole restore rather than sweeping
    /// again would leave the customer unable to restore at all.
    #[test]
    fn a_survivor_spawned_after_the_sweep_is_swept_again() {
        let forces = RefCell::new(0usize);
        let observations = RefCell::new(0usize);
        let result = stop_for_restore_with(
            |_| {
                *observations.borrow_mut() += 1;
                // One straggler outlives the first sweep and never moves; only a
                // second sweep clears it.
                if *forces.borrow() >= 2 {
                    StopObservation::Stopped
                } else {
                    StopObservation::Running(1)
                }
            },
            || Err(ProcessError::TerminateTimeout),
            |_| {
                *forces.borrow_mut() += 1;
                Ok(())
            },
            Duration::ZERO,
            Duration::from_millis(60),
            Duration::from_millis(900),
        );
        assert!(result.is_ok(), "{result:?}");
        assert!(*forces.borrow() >= 2, "the sweep was never repeated");
        assert!(*observations.borrow() > 2);
    }

    #[test]
    fn windows_force_is_exact_image_without_process_tree() {
        let command = restore_force_windows_command();
        assert_eq!(command.get_program(), "taskkill.exe");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["/F", "/IM", "Kiro.exe"]
        );
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
