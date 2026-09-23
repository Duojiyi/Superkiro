//! Kiro runtime process detection and client single-instance locking (Spec §9, P0-7).
//!
//! Features:
//! - Process-level runtime detection (`is_kiro_running`) across Windows, macOS, and Linux.
//! - Anti-collision Single Instance Lock (`SingleInstanceLock`) backed by an OS file lock.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use thiserror::Error;

// OS locks belong to a handle, not a process, so guards in one process share the handle
// that holds the lock and count themselves; the lock is released when the last goes.
static PROCESS_LOCKS: LazyLock<Mutex<HashMap<PathBuf, (fs::File, usize)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SingleInstanceError {
    #[error("Another client instance is already running with PID {0}")]
    AlreadyRunning(u32),

    #[error("Failed to access lock file '{0}': {1}")]
    LockIo(PathBuf, String),
}

/// Tri-state process status for Kiro IDE (Spec §9, T08).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Running,
    Stopped,
    Unknown,
}

impl std::fmt::Display for ProcessState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessState::Running => write!(f, "Running"),
            ProcessState::Stopped => write!(f, "Stopped"),
            ProcessState::Unknown => write!(f, "Unknown"),
        }
    }
}

/// Detect Kiro process state with full Running/Stopped/Unknown tri-state semantics (Spec §9, T08).
///
/// If detection tool fails, non-zero exits, or permissions are denied, returns `Unknown`.
/// Modifying Kiro files is strictly forbidden unless state is `Stopped`.
pub fn detect_kiro_process_state() -> ProcessState {
    detect_kiro_process_state_until(std::time::Instant::now() + std::time::Duration::from_secs(3))
}

pub(crate) fn detect_kiro_process_state_until(deadline: std::time::Instant) -> ProcessState {
    if std::time::Instant::now() >= deadline {
        return ProcessState::Unknown;
    }
    #[cfg(unix)]
    {
        unix_process_state_until(
            |name| is_kiro_executable(name, cfg!(target_os = "macos")),
            deadline,
        )
    }
    #[cfg(not(unix))]
    check_single_process_state_until("Kiro.exe", deadline)
}

/// Count the live processes with this image name, or `None` if the system could
/// not be observed before `deadline`.
#[cfg(windows)]
pub(crate) fn process_count_until(image_name: &str, deadline: std::time::Instant) -> Option<usize> {
    loop {
        match crate::windows_process::enumerate() {
            Ok(entries) => {
                return Some(
                    entries
                        .iter()
                        .filter(|p| p.2.eq_ignore_ascii_case(image_name))
                        .count(),
                )
            }
            // `CreateToolhelp32Snapshot` fails transiently while the process list
            // is churning, which is exactly the moment after a force-kill. One
            // failed sample is not evidence about the system, so retry rather
            // than reporting a state we never observed.
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            Err(_) => return None,
        }
    }
}

#[cfg(windows)]
fn check_single_process_state_until(
    image_name: &str,
    deadline: std::time::Instant,
) -> ProcessState {
    match process_count_until(image_name, deadline) {
        Some(0) => ProcessState::Stopped,
        Some(_) => ProcessState::Running,
        None => ProcessState::Unknown,
    }
}

fn check_single_process_state(image_name: &str) -> ProcessState {
    #[cfg(windows)]
    {
        check_single_process_state_until(
            image_name,
            std::time::Instant::now() + std::time::Duration::from_secs(3),
        )
    }
    #[cfg(unix)]
    {
        unix_process_state(|name| {
            Path::new(name).file_name().and_then(|n| n.to_str()) == Some(image_name)
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = image_name;
        ProcessState::Unknown
    }
}

// Read executable names, never command-line arguments or a caller-supplied regex.
#[cfg(unix)]
fn unix_process_state(matches: impl Fn(&str) -> bool) -> ProcessState {
    unix_process_state_until(
        matches,
        std::time::Instant::now() + std::time::Duration::from_secs(3),
    )
}

#[cfg(unix)]
fn unix_process_state_until(
    matches: impl Fn(&str) -> bool,
    deadline: std::time::Instant,
) -> ProcessState {
    match crate::process::capture_helper_until(
        Command::new("/bin/ps").args(["-A", "-ww", "-o", "comm="]),
        deadline,
    ) {
        Ok(bytes) => match std::str::from_utf8(&bytes) {
            Ok(text) => process_names_state(text, matches),
            Err(_) => ProcessState::Unknown,
        },
        Err(_) => ProcessState::Unknown,
    }
}

#[cfg(any(unix, test))]
fn process_names_state(text: &str, matches: impl Fn(&str) -> bool) -> ProcessState {
    let mut found_name = false;
    for name in text.lines().map(str::trim).filter(|name| !name.is_empty()) {
        found_name = true;
        if matches(name) {
            return ProcessState::Running;
        }
    }
    if found_name {
        ProcessState::Stopped
    } else {
        ProcessState::Unknown
    }
}

#[cfg(any(unix, test))]
fn is_kiro_executable(name: &str, macos: bool) -> bool {
    if macos {
        name.ends_with("/Kiro.app/Contents/MacOS/Kiro")
            || name.ends_with("/Kiro.app/Contents/MacOS/Electron")
    } else {
        matches!(name, "kiro" | "Kiro")
    }
}

/// Check whether the Kiro IDE is currently running.
///
/// Spec §9: 接管/恢复/写 token/打补丁前必须检测 Kiro 是否在运行，运行中禁止改文件。
pub fn is_kiro_running() -> bool {
    detect_kiro_process_state() == ProcessState::Running
}

/// Verify that Kiro IDE is definitively stopped before modifying files (Spec §9, T08).
///
/// Fails if Kiro is running OR if process state is unknown.
pub fn ensure_kiro_stopped() -> Result<(), String> {
    match detect_kiro_process_state() {
        ProcessState::Running => Err("Kiro IDE is currently running. Please close Kiro before proceeding.".to_string()),
        ProcessState::Unknown => Err("Kiro IDE process state cannot be determined safely. Modification aborted to protect file integrity.".to_string()),
        ProcessState::Stopped => Ok(()),
    }
}

/// Check whether a process with the given binary image name is currently running.
pub fn is_process_running_by_name(image_name: &str) -> bool {
    check_single_process_state(image_name) == ProcessState::Running
}

/// Check if a specific process ID is currently alive on the host.
pub fn is_pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        // A failed query is not evidence that a lock owner has exited.
        crate::windows_process::enumerate()
            .map(|entries| entries.iter().any(|p| p.0 == pid))
            .unwrap_or(true)
    }
    #[cfg(unix)]
    {
        // Do not pass zero/negative process-group identifiers to kill(2).
        let Ok(pid) = i32::try_from(pid) else {
            return true;
        };
        if pid == 0 {
            return true;
        }
        extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        let result = unsafe { kill(pid, 0) };
        let error = if result == 0 {
            None
        } else {
            std::io::Error::last_os_error().raw_os_error()
        };
        pid_probe_is_alive(result, error)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

#[cfg(any(unix, test))]
fn pid_probe_is_alive(result: i32, errno: Option<i32>) -> bool {
    // ESRCH is 3 on Linux/macOS. EPERM and all other failures are inconclusive.
    result == 0 || errno != Some(3)
}

/// RAII Guard ensuring only one instance of the Kiro BYOK desktop client is running (Spec §9, T08).
///
/// Ownership is an OS file lock, which the kernel releases however the owner exits.
/// It used to be a PID written into the file and trusted while that PID was alive; a
/// crash left the file behind, and once Windows recycled the PID for any unrelated
/// process the client refused to start — silently, since it has no console.
///
/// The file is never deleted. Unlinking a lock file lets a second process lock a new
/// inode while a third still holds the old one, which is two owners at once.
#[derive(Debug)]
pub struct SingleInstanceLock {
    lock_path: PathBuf,
}

impl SingleInstanceLock {
    /// Default lock file location in system temporary directory.
    pub fn default_lock_path() -> PathBuf {
        std::env::temp_dir().join("kiro-byok-client.lock")
    }

    /// Acquire the single instance lock.
    ///
    /// If `custom_path` is None, uses `default_lock_path()`.
    /// Returns `Err(SingleInstanceError::AlreadyRunning(pid))` if another process holds
    /// the lock; `pid` is the owner's recorded PID when it can be read, else 0.
    pub fn acquire(custom_path: Option<&Path>) -> Result<Self, SingleInstanceError> {
        let lock_path = match custom_path {
            Some(p) => p.to_path_buf(),
            None => Self::default_lock_path(),
        };
        let io = |e: std::io::Error| SingleInstanceError::LockIo(lock_path.clone(), e.to_string());

        let mut locks = PROCESS_LOCKS.lock().unwrap();
        // Same-process re-entrancy, scoped to this lock path.
        if let Some((_, count)) = locks.get_mut(&lock_path) {
            *count += 1;
            return Ok(Self { lock_path });
        }

        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent).map_err(io)?;
        }
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(io)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                // Diagnostic only: Windows locks are mandatory, so the owner's record
                // may be unreadable while it holds the lock.
                let owner = fs::read_to_string(&lock_path)
                    .ok()
                    .and_then(|content| content.trim().split(':').next()?.parse().ok())
                    .unwrap_or(0);
                return Err(SingleInstanceError::AlreadyRunning(owner));
            }
            Err(fs::TryLockError::Error(e)) => return Err(io(e)),
        }
        // Record the owner for diagnostics. Nothing reads this to decide ownership.
        file.set_len(0).map_err(io)?;
        file.write_all(format!("{}:{}", std::process::id(), rand_nonce()).as_bytes())
            .map_err(io)?;
        let _ = file.sync_all();

        locks.insert(lock_path.clone(), (file, 1));
        Ok(Self { lock_path })
    }

    /// Absolute path to the active lock file.
    pub fn path(&self) -> &Path {
        &self.lock_path
    }
}

impl Drop for SingleInstanceLock {
    fn drop(&mut self) {
        let mut locks = PROCESS_LOCKS.lock().unwrap();
        let Some((_, count)) = locks.get_mut(&self.lock_path) else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            // Dropping the handle releases the OS lock; the file stays.
            if let Some((file, _)) = locks.remove(&self.lock_path) {
                let _ = file.unlock();
            }
        }
    }
}

fn rand_nonce() -> u64 {
    std::process::id() as u64 ^ 0x5a5a5a5a5a5a5a5a
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ownership is the OS lock, not the file. While any guard lives, another handle
    /// cannot take the lock; once the last guard drops the lock is free again, and the
    /// file stays where it is.
    #[test]
    fn test_single_instance_lock_lifecycle() {
        let test_lock =
            std::env::temp_dir().join(format!("test_single_inst_{}.lock", std::process::id()));
        let _ = fs::remove_file(&test_lock);
        let contender = || {
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&test_lock)
                .unwrap()
                .try_lock()
        };

        let lock1 = SingleInstanceLock::acquire(Some(&test_lock)).expect("Must acquire lock");
        let lock2 =
            SingleInstanceLock::acquire(Some(&test_lock)).expect("Re-entrant acquire must succeed");
        assert!(matches!(contender(), Err(fs::TryLockError::WouldBlock)));

        drop(lock1);
        assert!(
            matches!(contender(), Err(fs::TryLockError::WouldBlock)),
            "the lock must stay held while lock2 is alive"
        );

        drop(lock2);
        assert!(test_lock.exists(), "the lock file is never unlinked");
        contender().expect("the lock must be free once every guard has dropped");
        drop(SingleInstanceLock::acquire(Some(&test_lock)).expect("reacquire after release"));
        let _ = fs::remove_file(&test_lock);
    }

    /// A file left behind by a crash names a PID that Windows will eventually hand to
    /// some unrelated process. Trusting that PID made the client refuse to start, with
    /// no console to say why. A PID in the file is only a diagnostic now, so a record
    /// naming a live process that does not hold the lock must not block startup.
    #[test]
    fn test_stale_lock_recovery() {
        let test_lock =
            std::env::temp_dir().join(format!("test_stale_lock_{}.lock", std::process::id()));
        // Always alive, and never this client: System on Windows, init elsewhere.
        let live_unrelated_pid = if cfg!(windows) { 4 } else { 1 };
        assert!(is_pid_alive(live_unrelated_pid));
        fs::write(&test_lock, format!("{live_unrelated_pid}:left-by-a-crash")).unwrap();

        let lock = SingleInstanceLock::acquire(Some(&test_lock))
            .expect("a recycled PID in a stale record must not block startup");
        drop(lock);
        let _ = fs::remove_file(&test_lock);
    }
}

#[cfg(test)]
mod unix_detection_tests {
    use super::*;

    #[test]
    fn executable_matching_excludes_arguments_and_other_editors() {
        for name in [
            "/usr/bin/python3",
            "/Applications/Other.app/Contents/MacOS/Electron",
            "Superkiro",
            "Code",
            "Cursor",
        ] {
            assert!(!is_kiro_executable(name, true));
            assert!(!is_kiro_executable(name, false));
        }
        assert!(is_kiro_executable(
            "/Applications/Kiro.app/Contents/MacOS/Electron",
            true
        ));
        assert!(is_kiro_executable(
            "/Volumes/My Disk/Kiro.app/Contents/MacOS/Kiro",
            true
        ));
        assert!(is_kiro_executable("kiro", false));
        assert!(is_kiro_executable("Kiro", false));
        // ps comm contains only the executable, even when argv mentions the Kiro bundle.
        assert_eq!(
            process_names_state("/sbin/launchd\n/usr/bin/python3\n", |n| is_kiro_executable(
                n, true
            )),
            ProcessState::Stopped
        );
        assert_eq!(
            process_names_state("/Applications/Kiro.app/Contents/MacOS/Electron\n", |n| {
                is_kiro_executable(n, true)
            }),
            ProcessState::Running
        );
        assert_eq!(
            process_names_state("   \n", |_| false),
            ProcessState::Unknown
        );
    }

    #[test]
    fn only_esrch_proves_pid_is_dead() {
        assert!(pid_probe_is_alive(0, None));
        assert!(!pid_probe_is_alive(-1, Some(3)));
        for errno in [Some(1), Some(13), Some(22), None] {
            assert!(pid_probe_is_alive(-1, errno));
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_probe_and_ps_work_on_this_host() {
        assert!(is_pid_alive(std::process::id()));
        assert!(is_pid_alive(0));
        assert!(is_pid_alive(u32::MAX));
        assert_eq!(unix_process_state(|_| true), ProcessState::Running);
    }
}
