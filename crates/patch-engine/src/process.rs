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

    /// This user's Kiro is also open in another Windows session, where this client cannot
    /// ask it to close. It still reads and writes the files being changed, so nothing is
    /// closed here either: that would cost the user their editor for nothing.
    #[error("Kiro is open in another Windows session of this user; close it there and retry")]
    OtherSession,

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
/// exit at once, and `VSCODE_*` carries another editor's IPC hook, portable-mode data
/// directory and code-cache paths. VS Code strips the same prefixes for its own children.
///
/// Kept: settings a customer sets themselves and Kiro honours. `VSCODE_APPDATA` and
/// `VSCODE_EXTENSIONS` relocate Kiro's user data (hot-exit backups included) and its
/// extensions; stripped, a Kiro started by the client opened another profile, and unsaved
/// work appeared lost. `ELECTRON_OZONE_PLATFORM_HINT` picks X11 or Wayland on Linux.
pub(crate) fn inherited_editor_variables(
    keys: impl Iterator<Item = std::ffi::OsString>,
) -> Vec<std::ffi::OsString> {
    const USER_SETTINGS: [&str; 3] = [
        "VSCODE_APPDATA",
        "VSCODE_EXTENSIONS",
        "ELECTRON_OZONE_PLATFORM_HINT",
    ];
    keys.filter(|key| {
        let key = key.to_string_lossy().to_ascii_uppercase();
        (key.starts_with("ELECTRON_") || key.starts_with("VSCODE_"))
            && !USER_SETTINGS.contains(&key.as_str())
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
        close_editor_windows(&own_session_kiro().map_err(|_| ProcessError::UnknownState)?)
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
        // The main process only. The renderers and the GPU and utility processes share
        // its name, and a signal reaching them is a crash, not a request to close.
        let main: Vec<String> = restore_pids(deadline)?
            .iter()
            .filter(|target| target.main)
            .map(|target| target.pid.to_string())
            .collect();
        if main.is_empty() {
            return Ok(());
        }
        run_helper_until(Command::new("/bin/kill").arg("-TERM").args(main), deadline)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = deadline;
        Err(ProcessError::UnknownState)
    }
}

#[cfg(windows)]
fn close_editor_windows(pids: &[u32]) -> Result<(), ProcessError> {
    let (mut delivered, mut denied) = (false, false);
    for window in crate::windows_process::top_level_windows(pids) {
        // Unowned, because an owned window is a dialog, and closing a save prompt is
        // pressing Cancel. Enabled, because a disabled window has a modal open, and what
        // happens to that is the dialog's business. Every editor window is asked, not one
        // per process: all of a Kiro's windows belong to one process.
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

/// This user's Kiro in this session: the only one whose windows can be asked to close.
#[cfg(windows)]
fn own_session_kiro() -> std::io::Result<Vec<u32>> {
    Ok(crate::windows_process::user_processes("Kiro.exe")?
        .into_iter()
        .filter(|process| process.own_session)
        .map(|process| process.pid)
        .collect())
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
            let pids = own_session_kiro().ok()?;
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

/// Timings for [`stop_with`]. The worst case stays well inside the client's 180s.
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
    /// Whether this user's Kiro also runs in another session, where it cannot be asked
    /// to close.
    fn elsewhere(&mut self) -> bool;
    /// Remember the processes being stopped. Only they, and processes they spawn while
    /// shutting down, may ever be ended: a Kiro started after this is someone using it.
    fn pin(&mut self);
    fn request_close(&mut self) -> Result<(), ProcessError>;
    /// `Some(false)` only when it is certain no Kiro window is on screen.
    fn window_visible(&mut self) -> Option<bool>;
    /// End the pinned processes; `StillOpen`, ending nothing, if a Kiro outside them runs.
    fn force(&mut self) -> Result<(), ProcessError>;
}

#[derive(Default)]
struct System {
    /// (pid, creation time) of the processes being stopped.
    pinned: Vec<ProcessKey>,
}

impl KiroControl for System {
    fn observe(&mut self) -> StopObservation {
        restore_process_observation(Instant::now() + OBSERVATION_BUDGET)
    }
    fn elsewhere(&mut self) -> bool {
        #[cfg(windows)]
        {
            crate::windows_process::user_processes("Kiro.exe")
                .is_ok_and(|found| found.iter().any(|process| !process.own_session))
        }
        #[cfg(not(windows))]
        false
    }
    fn pin(&mut self) {
        // A process whose creation time cannot be read is left out, so it is never ended.
        #[cfg(windows)]
        {
            self.pinned = own_session_kiro()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|pid| Some((pid, crate::windows_process::creation_time(pid).ok()?)))
                .collect();
        }
    }
    fn request_close(&mut self) -> Result<(), ProcessError> {
        let result = request_graceful_close(Instant::now() + SIGNAL_BUDGET);
        // A refused quit request (Automation permission denied or unanswered) says
        // nothing about whether the user can still quit Kiro, so wait for them.
        #[cfg(unix)]
        {
            let _ = result;
            Ok(())
        }
        #[cfg(not(unix))]
        result
    }
    fn window_visible(&mut self) -> Option<bool> {
        kiro_window_visible()
    }
    fn force(&mut self) -> Result<(), ProcessError> {
        force_kiro_for_restore(Instant::now() + SIGNAL_BUDGET, &self.pinned)
    }
}

/// Close Kiro for a restore or unbind.
///
/// `force_confirmed` is the user's second, specific confirmation, given only after being
/// told Kiro would not close, that it may be ended even with a window open.
pub fn stop_kiro_for_restore(force_confirmed: bool) -> Result<(), ProcessError> {
    stop_with(&mut System::default(), force_confirmed, &TIMINGS)
}

/// Close Kiro before a takeover. Never ends an editor with a window on screen; only the
/// headless remains of one that has already closed.
pub fn stop_kiro_for_takeover() -> Result<(), ProcessError> {
    stop_with(&mut System::default(), false, &TIMINGS)
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
    // Decided before anything is sent: closing this session's editor achieves nothing
    // while the other one keeps the files open.
    if control.elsewhere() {
        return Err(ProcessError::OtherSession);
    }
    control.pin();
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
        // A Kiro started since the stop began: the user is using it again.
        if matches!(result, Err(ProcessError::StillOpen)) {
            return result;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        match wait_for_exit(control, Some(timings.stall), remaining) {
            Ok(()) => return Ok(()),
            // A process spawned during shutdown survives the sweep: sweep again while
            // there is time.
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

/// One of this user's Kiro processes, from `ps -o pid=,ppid=,uid=,comm=`.
#[cfg(any(unix, test))]
#[derive(Debug, PartialEq, Eq)]
struct RestoreTarget {
    pid: u32,
    /// The main process: its parent is not Kiro. Renderers and helpers are its children.
    main: bool,
}

#[cfg(any(unix, test))]
fn next_number(rest: &mut &str) -> Result<u32, ProcessError> {
    let (value, tail) = rest
        .split_once(char::is_whitespace)
        .ok_or(ProcessError::UnknownState)?;
    *rest = tail.trim_start();
    value.parse().map_err(|_| ProcessError::UnknownState)
}

/// This user's Kiro processes. Another user's are neither counted nor signalled: they
/// never read or write this user's files, and signalling them fails anyway.
#[cfg(any(unix, test))]
fn parse_restore_pids(
    text: &str,
    macos: bool,
    uid: u32,
) -> Result<Vec<RestoreTarget>, ProcessError> {
    let mut kiro = Vec::new();
    let mut found = false;
    for line in text.lines().map(str::trim).filter(|s| !s.is_empty()) {
        let mut rest = line;
        let (pid, ppid, owner) = (
            next_number(&mut rest)?,
            next_number(&mut rest)?,
            next_number(&mut rest)?,
        );
        if rest.is_empty() {
            return Err(ProcessError::UnknownState);
        }
        found = true;
        if restore_target(rest, macos) {
            if pid <= 1 {
                return Err(ProcessError::UnknownState);
            }
            kiro.push((pid, ppid, owner));
        }
    }
    if !found {
        return Err(ProcessError::UnknownState);
    }
    let pids: std::collections::HashSet<u32> = kiro.iter().map(|entry| entry.0).collect();
    Ok(kiro
        .into_iter()
        .filter(|entry| entry.2 == uid)
        .map(|(pid, ppid, _)| RestoreTarget {
            pid,
            main: !pids.contains(&ppid),
        })
        .collect())
}

#[cfg(unix)]
fn restore_pids(deadline: Instant) -> Result<Vec<RestoreTarget>, ProcessError> {
    let bytes = capture_helper_until(
        Command::new("/bin/ps").args(["-A", "-ww", "-o", "pid=,ppid=,uid=,comm="]),
        deadline,
    )?;
    parse_restore_pids(
        std::str::from_utf8(&bytes).map_err(|_| ProcessError::UnknownState)?,
        cfg!(target_os = "macos"),
        crate::runtime::own_uid(),
    )
}

/// One process for good: its PID and creation time. A PID is reused; the pair is not.
type ProcessKey = (u32, u64);

/// Split `live` processes into those descended from `pinned` — the processes asked to
/// close, or children they spawned while shutting down — and the rest, which were started
/// since and belong to someone using Kiro. A parent must have been created no later than
/// its child, so a recycled PID cannot link a stranger into the lineage.
#[cfg(any(windows, test))]
fn split_lineage(
    pinned: &[ProcessKey],
    live: &[ProcessKey],
    parents: &std::collections::HashMap<u32, u32>,
) -> (Vec<ProcessKey>, Vec<ProcessKey>) {
    live.iter().partition(|&&process| {
        let mut current = process;
        // Depth-bounded: a parent map read from a snapshot can, in principle, hold a cycle.
        for _ in 0..64 {
            if pinned.contains(&current) {
                return true;
            }
            let Some(&parent) = parents.get(&current.0) else {
                return false;
            };
            // The parent itself may have exited already; what was pinned still counts.
            if pinned
                .iter()
                .any(|&(pid, created)| pid == parent && created <= current.1)
            {
                return true;
            }
            match live
                .iter()
                .find(|&&(pid, created)| pid == parent && created <= current.1)
            {
                Some(&next) => current = next,
                None => return false,
            }
        }
        false
    })
}

fn force_kiro_for_restore(deadline: Instant, pinned: &[ProcessKey]) -> Result<(), ProcessError> {
    #[cfg(windows)]
    {
        let _ = deadline;
        let parents = crate::windows_process::parents().map_err(|_| ProcessError::UnknownState)?;
        let mut live = Vec::new();
        for pid in own_session_kiro().map_err(|_| ProcessError::UnknownState)? {
            match crate::windows_process::creation_time(pid) {
                Ok(created) => live.push((pid, created)),
                Err(error) if error.raw_os_error() == Some(5) => {
                    return Err(ProcessError::Elevated)
                }
                // Exited since it was listed.
                Err(_) => {}
            }
        }
        let (lineage, started_since) = split_lineage(pinned, &live, &parents);
        if !started_since.is_empty() {
            return Err(ProcessError::StillOpen);
        }
        let mut result = Ok(());
        for (pid, created) in lineage {
            match crate::windows_process::terminate_if_same(pid, "Kiro.exe", created) {
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
        // Ended only with the user's explicit confirmation: windows cannot be seen here,
        // so the stop policy never reaches this on its own.
        let _ = pinned;
        let pids = restore_pids(deadline)?;
        if pids.is_empty() {
            return Ok(());
        }
        run_helper_until(
            Command::new("/bin/kill")
                .arg("-KILL")
                .args(pids.iter().map(|target| target.pid.to_string())),
            deadline,
        )
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (deadline, pinned);
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

/// The filter that decides which of Kiro's windows are asked to close, against real
/// windows. Asking one window per process lost the second window's work, and closing an
/// owned save prompt is pressing Cancel on it.
#[cfg(all(test, windows))]
mod window_filter_tests {
    use std::ffi::c_void;
    use std::sync::{mpsc, Mutex};

    #[repr(C)]
    struct WndClassW {
        style: u32,
        wnd_proc: unsafe extern "system" fn(*mut c_void, u32, usize, isize) -> isize,
        cls_extra: i32,
        wnd_extra: i32,
        instance: *mut c_void,
        icon: *mut c_void,
        cursor: *mut c_void,
        background: *mut c_void,
        menu_name: *const u16,
        class_name: *const u16,
    }
    #[link(name = "user32")]
    extern "system" {
        fn RegisterClassW(class: *const WndClassW) -> u16;
        #[allow(clippy::too_many_arguments)]
        fn CreateWindowExW(
            ex_style: u32,
            class: *const u16,
            title: *const u16,
            style: u32,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            parent: *mut c_void,
            menu: *mut c_void,
            instance: *mut c_void,
            param: *mut c_void,
        ) -> *mut c_void;
        fn DefWindowProcW(window: *mut c_void, message: u32, wparam: usize, lparam: isize)
            -> isize;
        fn EnableWindow(window: *mut c_void, enable: i32) -> i32;
        fn DestroyWindow(window: *mut c_void) -> i32;
        fn PeekMessageW(msg: *mut u64, window: *mut c_void, min: u32, max: u32, remove: u32)
            -> i32;
        fn DispatchMessageW(msg: *const u64) -> isize;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    }

    static CLOSED: Mutex<Vec<usize>> = Mutex::new(Vec::new());

    unsafe extern "system" fn record(
        window: *mut c_void,
        message: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        if message == 0x0010 {
            // WM_CLOSE: note it, and stay open like an editor showing its save prompt.
            CLOSED.lock().unwrap().push(window as usize);
            return 0;
        }
        DefWindowProcW(window, message, wparam, lparam)
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    #[test]
    fn every_editor_window_is_asked_and_no_dialog_or_hidden_window_is() {
        const WS_POPUP: u32 = 0x8000_0000;
        const WS_VISIBLE: u32 = 0x1000_0000;
        const WS_EX_TOOLWINDOW: u32 = 0x80; // off the taskbar
        let (created, windows) = mpsc::channel();
        let (stop, stopped) = mpsc::channel::<()>();
        let pump = std::thread::spawn(move || unsafe {
            let class = wide("SuperkiroWindowFilterTest");
            let instance = GetModuleHandleW(std::ptr::null());
            let definition = WndClassW {
                style: 0,
                wnd_proc: record,
                cls_extra: 0,
                wnd_extra: 0,
                instance,
                icon: std::ptr::null_mut(),
                cursor: std::ptr::null_mut(),
                background: std::ptr::null_mut(),
                menu_name: std::ptr::null(),
                class_name: class.as_ptr(),
            };
            assert_ne!(RegisterClassW(&definition), 0);
            // Off screen and one pixel: visible to the window manager, not to a person.
            let make = |style: u32, owner: *mut c_void| {
                let window = CreateWindowExW(
                    WS_EX_TOOLWINDOW,
                    class.as_ptr(),
                    class.as_ptr(),
                    WS_POPUP | style,
                    -32000,
                    -32000,
                    1,
                    1,
                    owner,
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null_mut(),
                );
                assert!(!window.is_null());
                window
            };
            let first = make(WS_VISIBLE, std::ptr::null_mut());
            let second = make(WS_VISIBLE, std::ptr::null_mut());
            let dialog = make(WS_VISIBLE, first);
            let modal_owner = make(WS_VISIBLE, std::ptr::null_mut());
            EnableWindow(modal_owner, 0);
            let hidden = make(0, std::ptr::null_mut());
            let all = [first, second, dialog, modal_owner, hidden];
            created.send(all.map(|w| w as usize)).unwrap();
            let mut msg = [0u64; 8];
            while stopped.try_recv().is_err() {
                while PeekMessageW(msg.as_mut_ptr(), std::ptr::null_mut(), 0, 0, 1) != 0 {
                    DispatchMessageW(msg.as_ptr());
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            for window in all {
                DestroyWindow(window);
            }
        });
        let [first, second, ..] = windows.recv().unwrap();
        super::close_editor_windows(&[std::process::id()]).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while CLOSED.lock().unwrap().len() < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Anything queued for the other windows would have been dispatched by now too.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let mut closed = CLOSED.lock().unwrap().clone();
        closed.sort();
        let mut expected = vec![first, second];
        expected.sort();
        stop.send(()).unwrap();
        pump.join().unwrap();
        assert_eq!(
            closed, expected,
            "exactly the two unowned, enabled, visible windows"
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
            // The customer's own settings, which Kiro honours.
            "VSCODE_APPDATA",
            "vscode_extensions",
            "ELECTRON_OZONE_PLATFORM_HINT",
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

    /// The launch command itself drops them, not only the filter that names them.
    #[test]
    fn the_launch_command_removes_inherited_editor_variables() {
        std::env::set_var("VSCODE_PID", "4242");
        std::env::set_var("VSCODE_APPDATA", "D:\\KiroData");
        let executable = std::env::current_exe().unwrap();
        let installation = crate::detect::KiroInstallation {
            install_dir: executable.parent().unwrap().to_path_buf(),
            executable_path: executable.clone(),
            product_json_path: executable.clone(),
            version: "1.1.14".into(),
            vscode_version: None,
            commit: None,
            quality: None,
            win32_mutex_name: "kiro".into(),
            agent_extension_dir: None,
            agent_version: None,
            is_user_level: true,
        };
        let command = prepare_kiro_launch(&installation, "https://gateway.example", &[]).unwrap();
        let envs: Vec<_> = command.get_envs().collect();
        assert!(
            envs.contains(&(std::ffi::OsStr::new("VSCODE_PID"), None)),
            "an inherited editor variable reached Kiro"
        );
        assert!(
            !envs.iter().any(|(key, _)| *key == "VSCODE_APPDATA"),
            "the customer's own VSCODE_APPDATA was touched"
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
        /// This user's Kiro also runs in another session.
        elsewhere: bool,
        /// A Kiro outside the pinned processes is running, so force refuses.
        relaunched: bool,
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
                elsewhere: false,
                relaunched: false,
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
        fn elsewhere(&mut self) -> bool {
            self.calls.push("elsewhere");
            self.elsewhere
        }
        fn pin(&mut self) {
            self.calls.push("pin");
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
            if self.relaunched {
                return Err(ProcessError::StillOpen);
            }
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
        // Poll every 50ms. This gives 9 observations across 400ms, each reporting progress.
        let mut counts: Vec<_> = std::iter::repeat_n(RUNNING, 4).collect();
        counts.extend([4usize, 3, 2, 1].map(StopObservation::Running));
        counts.push(StopObservation::Stopped);
        let mut fake = Fake::new(&counts, &[Some(false)]);
        // Make the grace short so the test finishes before the counts run out; stall is
        // the real guard.
        let timings = StopTimings {
            grace: Duration::from_millis(50),
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

    /// The worst case must leave room, inside the 180s the client waits for takeover,
    /// restore and unbind, for authentication (up to 20s) and the launch check (3s).
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

    /// The same user's Kiro in another session keeps the files open and cannot be asked
    /// to close from here. Say so before closing this session's editor for nothing.
    #[test]
    fn a_kiro_in_another_session_is_reported_before_anything_is_sent() {
        let mut fake = Fake::new(&[RUNNING], &[Some(false)]);
        fake.elsewhere = true;
        assert!(matches!(
            stop_with(&mut fake, true, &FAST),
            Err(ProcessError::OtherSession)
        ));
        assert_eq!(fake.calls, ["observe", "elsewhere"]);
    }

    /// Kiro opened again while an earlier one was being stopped is someone using it: it is
    /// never ended, not even with the user's confirmation for the earlier one.
    #[test]
    fn a_kiro_started_during_the_stop_is_never_ended() {
        for force_confirmed in [false, true] {
            let mut fake = Fake::new(&[RUNNING], &[Some(false)]);
            fake.relaunched = true;
            assert!(matches!(
                stop_with(&mut fake, force_confirmed, &FAST),
                Err(ProcessError::StillOpen)
            ));
            assert_eq!(fake.forces, 1, "force must not be retried against it");
            let pin = fake.calls.iter().position(|c| *c == "pin").unwrap();
            let close = fake.calls.iter().position(|c| *c == "close");
            assert!(close.is_none_or(|close| pin < close), "pinned after asking");
        }
    }

    #[test]
    fn lineage_is_what_was_asked_to_close_and_what_it_spawned() {
        use std::collections::HashMap;
        // 100 is Kiro's main process when it was asked to close; 101 its renderer.
        let pinned = [(100, 10), (101, 11)];
        let parents = HashMap::from([
            (101, 100),
            (102, 100), // spawned by the closing editor, after pinning
            (103, 102), // and its child
            (200, 4),   // a Kiro the user started since, from Explorer
            (201, 200), // that Kiro's renderer
            (104, 100), // PID 100 exited and was reused below; this child predates reuse
            (300, 999), // parent unknown
        ]);
        let live = [
            (101, 11),
            (102, 20),
            (103, 21),
            (200, 30),
            (201, 31),
            (104, 25),
            (300, 40),
        ];
        let (lineage, started_since) = split_lineage(&pinned, &live, &parents);
        assert_eq!(lineage, [(101, 11), (102, 20), (103, 21), (104, 25)]);
        assert_eq!(started_since, [(200, 30), (201, 31), (300, 40)]);
        // A reused PID: 100 again, created after the pin, is a new process, not the old one.
        let (lineage, started_since) = split_lineage(&pinned, &[(100, 50)], &HashMap::new());
        assert!(lineage.is_empty());
        assert_eq!(started_since, [(100, 50)]);
        // A parent that is younger than its child cannot be its parent: the PID was reused.
        let (lineage, _) = split_lineage(&[(100, 60)], &[(105, 55)], &HashMap::from([(105, 100)]));
        assert!(lineage.is_empty());
    }

    #[test]
    fn unix_targets_exclude_arguments_other_electron_apps_and_other_users() {
        let target = |pid, main| RestoreTarget { pid, main };
        assert_eq!(
            parse_restore_pids(
                "1 0 0 init\n20 1 501 kiro\n21 20 501 Kiro\n22 1 501 superkiro\n23 1 501 node\n24 1 501 /usr/bin/python kiro\n25 1 502 kiro\n",
                false,
                501
            )
            .unwrap(),
            [target(20, true), target(21, false)]
        );
        let listing = "0 0 0 kernel_task\n1 0 0 /sbin/launchd\n20 1 501 /Applications/Kiro.app/Contents/MacOS/Electron\n21 20 501 /Volumes/My Disk/Kiro.app/Contents/Frameworks/Kiro Helper (GPU).app/Contents/MacOS/Kiro Helper (GPU)\n22 1 501 /Applications/Other.app/Contents/MacOS/Electron\n23 1 501 /usr/bin/python /Applications/Kiro.app/example\n24 1 501 /Applications/Kiro.app/Contents/Frameworks/Other.app/Contents/MacOS/Other\n";
        assert_eq!(
            parse_restore_pids(listing, true, 501).unwrap(),
            [target(20, true), target(21, false)]
        );
        for invalid in [
            "",
            "oops",
            "1 1 no-kiro",
            "0 1 501 kiro",
            "22 1 501 ",
            "x 1 501 kiro",
        ] {
            assert!(
                parse_restore_pids(invalid, false, 501).is_err(),
                "{invalid:?}"
            );
        }
    }
}
