//! Kiro runtime process detection and client single-instance locking (Spec §9, P0-7).
//!
//! Features:
//! - Process-level runtime detection (`is_kiro_running`) across Windows, macOS, and Linux.
//! - Anti-collision Single Instance Lock (`SingleInstanceLock`) with stale PID recovery.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
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
    if cfg!(target_os = "macos") {
        // Match the Kiro bundle, never unrelated Electron applications.
        return match Command::new("pgrep")
            .args(["-f", r"/Kiro[.]app/Contents/MacOS/(Kiro|Electron)( |$)"])
            .output()
        {
            Ok(output) if output.status.success() && !output.stdout.is_empty() => {
                ProcessState::Running
            }
            Ok(output) if output.status.code() == Some(1) => ProcessState::Stopped,
            _ => ProcessState::Unknown,
        };
    }
    let names = if cfg!(target_os = "windows") {
        vec!["Kiro.exe", "kiro.exe"]
    } else {
        vec!["kiro", "Kiro"]
    };
    for name in names {
        match check_single_process_state(name) {
            ProcessState::Running => return ProcessState::Running,
            ProcessState::Unknown => return ProcessState::Unknown,
            ProcessState::Stopped => continue,
        }
    }
    ProcessState::Stopped
}

fn check_single_process_state(image_name: &str) -> ProcessState {
    if cfg!(target_os = "windows") {
        let filter = format!("IMAGENAME eq {}", image_name);
        match Command::new("tasklist")
            .args(["/FI", &filter, "/NH"])
            .output()
        {
            Ok(output) => {
                if !output.status.success() {
                    return ProcessState::Unknown;
                }
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout
                    .to_ascii_lowercase()
                    .contains(&image_name.to_ascii_lowercase())
                {
                    ProcessState::Running
                } else {
                    ProcessState::Stopped
                }
            }
            Err(_) => ProcessState::Unknown,
        }
    } else {
        match Command::new("pgrep").args(["-x", image_name]).output() {
            Ok(output) => {
                if output.status.success() && !output.stdout.is_empty() {
                    ProcessState::Running
                } else if output.status.code() == Some(1) {
                    ProcessState::Stopped
                } else {
                    ProcessState::Unknown
                }
            }
            Err(_) => ProcessState::Unknown,
        }
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
    if cfg!(target_os = "windows") {
        let filter = format!("PID eq {}", pid);
        let output = Command::new("tasklist")
            .args(["/FI", &filter, "/NH"])
            .output();

        match output {
            Ok(out) => {
                if !out.status.success() {
                    return false;
                }
                let stdout = String::from_utf8_lossy(&out.stdout);
                stdout.contains(&pid.to_string())
            }
            Err(_) => false,
        }
    } else {
        let status = Command::new("kill").args(["-0", &pid.to_string()]).output();
        match status {
            Ok(out) => out.status.success(),
            Err(_) => false,
        }
    }
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
