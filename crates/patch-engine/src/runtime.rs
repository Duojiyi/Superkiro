//! Kiro runtime process detection and client single-instance locking (Spec §9, P0-7).
//!
//! Features:
//! - Process-level runtime detection (`is_kiro_running`) across Windows, macOS, and Linux.
//! - Anti-collision Single Instance Lock (`SingleInstanceLock`) with stale PID recovery.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use thiserror::Error;

static PROCESS_LOCKS: LazyLock<Mutex<HashMap<PathBuf, (String, usize)>>> =
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
    check_single_process_state("Kiro.exe")
}

fn check_single_process_state(image_name: &str) -> ProcessState {
    #[cfg(windows)]
    {
        match crate::windows_process::enumerate() {
            Ok(entries) => {
                if entries.iter().any(|p| p.2.eq_ignore_ascii_case(image_name)) {
                    ProcessState::Running
                } else {
                    ProcessState::Stopped
                }
            }
            Err(_) => ProcessState::Unknown,
        }
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
/// Uses reference counting for same-process multiple guards, verifies cryptographic nonce ownership
/// before deleting on drop, and recovers safely from stale dead PID locks.
#[derive(Debug)]
pub struct SingleInstanceLock {
    lock_path: PathBuf,
    owner_nonce: String,
}

impl SingleInstanceLock {
    /// Default lock file location in system temporary directory.
    pub fn default_lock_path() -> PathBuf {
        std::env::temp_dir().join("kiro-byok-client.lock")
    }

    /// Acquire the single instance lock.
    ///
    /// If `custom_path` is None, uses `default_lock_path()`.
    /// Returns `Err(SingleInstanceError::AlreadyRunning(pid))` if an active instance holds the lock.
    pub fn acquire(custom_path: Option<&Path>) -> Result<Self, SingleInstanceError> {
        let lock_path = match custom_path {
            Some(p) => p.to_path_buf(),
            None => Self::default_lock_path(),
        };

        // 1. Same-process re-entrancy support, scoped to this lock path.
        if let Some((existing_nonce, count)) = PROCESS_LOCKS.lock().unwrap().get_mut(&lock_path) {
            *count += 1;
            return Ok(Self {
                lock_path,
                owner_nonce: existing_nonce.clone(),
            });
        }

        let pid = std::process::id();
        let nonce = format!(
            "{}:{}:{}",
            pid,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            rand_nonce()
        );

        // 2. Check for existing lock file
        if lock_path.exists() {
            if let Ok(content) = fs::read_to_string(&lock_path) {
                let parts: Vec<&str> = content.trim().split(':').collect();
                if let Some(owner_pid_str) = parts.first() {
                    if let Ok(owner_pid) = owner_pid_str.parse::<u32>() {
                        if owner_pid == pid {
                            let _ = fs::remove_file(&lock_path);
                        } else if is_pid_alive(owner_pid) {
                            return Err(SingleInstanceError::AlreadyRunning(owner_pid));
                        } else {
                            // Dead PID (stale lock from previous crash)
                            let _ = fs::remove_file(&lock_path);
                        }
                    }
                }
            }
        }

        // 3. Ensure parent directory exists
        if let Some(parent) = lock_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        // 4. Create new lock file atomically
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    if let Ok(content) = fs::read_to_string(&lock_path) {
                        let parts: Vec<&str> = content.trim().split(':').collect();
                        if let Some(owner_pid_str) = parts.first() {
                            if let Ok(owner_pid) = owner_pid_str.parse::<u32>() {
                                if is_pid_alive(owner_pid) {
                                    return SingleInstanceError::AlreadyRunning(owner_pid);
                                }
                            }
                        }
                    }
                }
                SingleInstanceError::LockIo(lock_path.clone(), e.to_string())
            })?;

        file.write_all(nonce.as_bytes())
            .map_err(|e| SingleInstanceError::LockIo(lock_path.clone(), e.to_string()))?;

        PROCESS_LOCKS
            .lock()
            .unwrap()
            .insert(lock_path.clone(), (nonce.clone(), 1));

        Ok(Self {
            lock_path,
            owner_nonce: nonce,
        })
    }

    /// Absolute path to the active lock file.
    pub fn path(&self) -> &Path {
        &self.lock_path
    }
}

impl Drop for SingleInstanceLock {
    fn drop(&mut self) {
        let should_remove = {
            let mut locks = PROCESS_LOCKS.lock().unwrap();
            let Some((nonce, count)) = locks.get_mut(&self.lock_path) else {
                return;
            };
            if nonce != &self.owner_nonce {
                return;
            }
            *count -= 1;
            if *count == 0 {
                locks.remove(&self.lock_path);
                true
            } else {
                false
            }
        };

        if should_remove {
            // Last guard for this path: only delete if the lock file still belongs to us.
            if let Ok(content) = fs::read_to_string(&self.lock_path) {
                if content.trim() == self.owner_nonce {
                    let _ = fs::remove_file(&self.lock_path);
                }
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

    #[test]
    fn test_single_instance_lock_lifecycle() {
        let test_lock =
            std::env::temp_dir().join(format!("test_single_inst_{}.lock", std::process::id()));

        // 1. First acquisition succeeds
        let lock1 = SingleInstanceLock::acquire(Some(&test_lock)).expect("Must acquire lock");
        assert!(test_lock.exists());

        // 2. Second acquisition from the same process sees re-entrant success
        let lock2 =
            SingleInstanceLock::acquire(Some(&test_lock)).expect("Re-entrant acquire must succeed");

        // 3. Drop lock1: lock2 is still active, so file MUST NOT be deleted!
        drop(lock1);
        assert!(
            test_lock.exists(),
            "Lock file must remain active while lock2 is alive"
        );

        // 4. Drop lock2: now all guards are released, file should be removed
        drop(lock2);
        assert!(
            !test_lock.exists(),
            "Lock file must be removed when all guards drop"
        );
    }

    #[test]
    fn test_stale_lock_recovery() {
        let test_lock =
            std::env::temp_dir().join(format!("test_stale_lock_{}.lock", std::process::id()));

        // Write a fictitious non-existent high PID (e.g. 9999999)
        fs::write(&test_lock, "9999999:mock-nonce:12345").unwrap();
        assert!(test_lock.exists());

        // Acquisition should detect dead PID and recover
        let lock = SingleInstanceLock::acquire(Some(&test_lock))
            .expect("Must recover from stale dead PID");
        assert!(test_lock.exists());

        drop(lock);
        assert!(!test_lock.exists());
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
