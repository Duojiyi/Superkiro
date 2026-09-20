//! Reversible takeover snapshot and one-click official restore (Spec §9, §14.5, P0-3).
//!
//! Features:
//! - Takes a complete, self-contained snapshot before applying any takeover or patch.
//! - Executes 100% clean rollback restoring official Kiro state with zero residue.

use crate::patch::{ExtensionPatcher, PatchError};
use crate::runtime::{detect_kiro_process_state, ProcessState};
use crate::settings::{PriorSettingsState, SettingsError, SettingsManager};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// Default filename for saving the active takeover snapshot.
pub const SNAPSHOT_FILENAME: &str = "kiro-byok-takeover-snapshot.json";

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("Kiro is currently running. Please close Kiro before taking or restoring snapshots.")]
    KiroRunning,

    #[error("Kiro process state cannot be determined safely: {0}")]
    ProcessStateUnknown(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Settings error: {0}")]
    Settings(#[from] SettingsError),

    #[error("Patch error: {0}")]
    Patch(#[from] PatchError),

    #[error("A different or incomplete takeover exists; restore it before activating again")]
    ActiveTakeoverConflict,

    #[error("No active takeover snapshot found on system")]
    NoActiveSnapshot,
}

/// Metadata recorded during takeover to ensure precision rollback.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TakeoverSnapshot {
    pub created_at: u64,
    pub gateway_url: String,
    pub settings_path: PathBuf,
    pub settings_state: PriorSettingsState,
    pub extension_path: Option<PathBuf>,
}

/// Summary of what was restored during official rollback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreSummary {
    pub settings_restored: bool,
    pub extension_restored: bool,
    pub snapshot_removed: bool,
}

/// Manager for taking snapshots and executing one-click restore to official state.
#[derive(Debug, Clone)]
pub struct SnapshotManager {
    snapshot_path: PathBuf,
}

impl Default for SnapshotManager {
    fn default() -> Self {
        let dir = if cfg!(target_os = "windows") {
            env::var("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|_| env::temp_dir())
        } else {
            env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| env::temp_dir())
        };

        Self {
            snapshot_path: dir.join(SNAPSHOT_FILENAME),
        }
    }
}

impl SnapshotManager {
    pub fn at(snapshot_path: impl Into<PathBuf>) -> Self {
        Self {
            snapshot_path: snapshot_path.into(),
        }
    }

    pub fn snapshot_path(&self) -> &Path {
        &self.snapshot_path
    }

    pub fn has_active_snapshot(&self) -> bool {
        self.snapshot_path.exists()
    }

    /// Load active snapshot from disk.
    pub fn load(&self) -> Result<TakeoverSnapshot, SnapshotError> {
        if !self.snapshot_path.exists() {
            return Err(SnapshotError::NoActiveSnapshot);
        }

        let mut file = File::open(&self.snapshot_path)?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;

        let snapshot: TakeoverSnapshot = serde_json::from_str(&content)?;
        Ok(snapshot)
    }

    /// Record a takeover snapshot to disk before applying changes.
    pub fn save(&self, snapshot: &TakeoverSnapshot) -> Result<(), SnapshotError> {
        let data = serde_json::to_string_pretty(snapshot)?;
        crate::token_storage::private_atomic_write(&self.snapshot_path, data.as_bytes())?;
        Ok(())
    }

    /// A repeat operation must match the original durable snapshot and current files.
    pub fn validate_takeover(
        &self,
        settings: &SettingsManager,
        patcher: Option<&ExtensionPatcher>,
        gateway: &str,
    ) -> Result<(), SnapshotError> {
        crate::patch::validate_gateway_url(gateway)?;
        if self.has_active_snapshot() {
            let snapshot = self.load()?;
            if snapshot.gateway_url.trim_end_matches('/') != gateway.trim_end_matches('/')
                || snapshot.settings_path != settings.path()
                || snapshot.extension_path.as_deref() != patcher.map(|p| p.path())
                || !settings.is_byok_active(Some(gateway))
            {
                return Err(SnapshotError::ActiveTakeoverConflict);
            }
            if let Some(p) = patcher {
                p.verify_patched_content()?;
            }
        } else {
            if let Some(p) = patcher {
                if p.status() != crate::patch::PatchStatus::Official {
                    return Err(SnapshotError::ActiveTakeoverConflict);
                }
                p.dry_run()?;
            }
            settings.read_settings()?;
        }
        Ok(())
    }

    fn operation_lock(&self) -> Result<OperationLock, SnapshotError> {
        let path = self.snapshot_path.with_extension("operation-lock");
        Ok(OperationLock::acquire(path)?)
    }

    /// Execute takeover: takes snapshot, merges settings, and applies patch.
    pub fn takeover(
        &self,
        settings_mgr: &SettingsManager,
        patcher: Option<&ExtensionPatcher>,
        gateway_url: &str,
    ) -> Result<TakeoverSnapshot, SnapshotError> {
        match detect_kiro_process_state() {
            ProcessState::Running => return Err(SnapshotError::KiroRunning),
            ProcessState::Unknown => {
                return Err(SnapshotError::ProcessStateUnknown(
                    "Cannot verify whether Kiro is running; modification forbidden".to_string(),
                ))
            }
            ProcessState::Stopped => {}
        }

        let _lock = self.operation_lock()?;
        self.validate_takeover(settings_mgr, patcher, gateway_url)?;
        if self.has_active_snapshot() {
            let mut snapshot = self.load()?;
            if !snapshot.settings_state.proxy_bypass_managed {
                let current = settings_mgr.read_settings()?;
                if let Some(value) = current.get("http.noProxy") {
                    snapshot
                        .settings_state
                        .prior_values
                        .insert("http.noProxy".into(), value.clone());
                }
                snapshot.settings_state.proxy_bypass_managed = true;
                snapshot.settings_state.prior_raw = None;
                // Persist rollback data before introducing the new managed key.
                self.save(&snapshot)?;
            }
            settings_mgr.merge_byok(gateway_url)?;
            if let Some(p) = patcher {
                p.apply(gateway_url)?;
            }
            return Ok(snapshot);
        }

        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // 1. Capture the rollback record and publish it before any mutation.
        let prior_settings = settings_mgr.capture_prior_state()?;
        let planned_snapshot = TakeoverSnapshot {
            created_at: now_secs,
            gateway_url: gateway_url.to_string(),
            settings_path: settings_mgr.path().to_path_buf(),
            settings_state: prior_settings.clone(),
            extension_path: patcher.map(|p| p.path().to_path_buf()),
        };
        self.save(&planned_snapshot)?;

        // 2. Apply settings and extension only after the rollback record is durable.
        if let Err(error) = settings_mgr.merge_byok(gateway_url) {
            let _ = fs::remove_file(&self.snapshot_path);
            return Err(SnapshotError::Settings(error));
        }

        // 2. Patch extension if provided
        let ext_path = if let Some(p) = patcher {
            if let Err(e) = p.apply(gateway_url) {
                // Keep the recovery record if rollback itself fails.
                if settings_mgr.revert(&prior_settings).is_ok() && !p.backup_path().exists() {
                    let _ = fs::remove_file(&self.snapshot_path);
                }
                return Err(SnapshotError::Patch(e));
            }
            Some(p.path().to_path_buf())
        } else {
            None
        };

        let mut snapshot = planned_snapshot;
        snapshot.extension_path = ext_path;
        // The initial record already contains the path; rewrite metadata only
        // if the patch operation succeeded and keep the durable rollback data.
        self.save(&snapshot)?;
        Ok(snapshot)
    }

    /// One-click official restore: exact rollback of settings and extension patch.
    pub fn restore_official(&self) -> Result<RestoreSummary, SnapshotError> {
        match detect_kiro_process_state() {
            ProcessState::Running => return Err(SnapshotError::KiroRunning),
            ProcessState::Unknown => {
                return Err(SnapshotError::ProcessStateUnknown(
                    "Cannot verify whether Kiro is running; modification forbidden".to_string(),
                ))
            }
            ProcessState::Stopped => {}
        }

        let _lock = self.operation_lock()?;
        let snapshot = self.load()?;
        if let Some(path) = &snapshot.extension_path {
            let patcher = ExtensionPatcher::new(path);
            patcher.verify_restore()?;
        }

        // 1. Revert settings.json
        let settings_mgr = SettingsManager::at(&snapshot.settings_path);
        settings_mgr.revert(&snapshot.settings_state)?;

        // 2. Revert extension.js if it was patched
        let mut extension_restored = false;
        if let Some(ref ext_path) = snapshot.extension_path {
            let patcher = ExtensionPatcher::new(ext_path);
            extension_restored = patcher.restore()?;
        }

        // 3. Remove snapshot file
        let snapshot_removed = if self.snapshot_path.exists() {
            fs::remove_file(&self.snapshot_path)?;
            true
        } else {
            false
        };

        Ok(RestoreSummary {
            settings_restored: true,
            extension_restored,
            snapshot_removed,
        })
    }
}

// Keep the inode in place: unlinking a lock file allows two independent owners.
pub(crate) struct OperationLock(#[allow(dead_code)] File);
impl OperationLock {
    pub(crate) fn acquire(path: PathBuf) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.try_lock().map_err(std::io::Error::from)?;
        Ok(Self(file))
    }
}

pub(crate) fn atomic_replace(temp: &Path, target: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        const REPLACE: u32 = 0x1;
        const WRITE_THROUGH: u32 = 0x8;
        #[link(name = "kernel32")]
        extern "system" {
            fn MoveFileExW(existing: *const u16, target: *const u16, flags: u32) -> i32;
        }
        let source: Vec<u16> = temp.as_os_str().encode_wide().chain([0]).collect();
        let destination: Vec<u16> = target.as_os_str().encode_wide().chain([0]).collect();
        if unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                REPLACE | WRITE_THROUGH,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::rename(temp, target)
    }
}

#[cfg(test)]
mod os_lock_tests {
    use super::*;
    #[test]
    fn lock_child() {
        let Some(path) = std::env::var_os("SUPERKIRO_LOCK_FIXTURE") else {
            return;
        };
        let path = PathBuf::from(path);
        let _lock = OperationLock::acquire(path.clone()).unwrap();
        fs::write(path.with_extension("ready"), b"ready").unwrap();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    #[test]
    fn process_death_releases_lock_without_unlinking() {
        let root = std::env::temp_dir().join(format!("os-lock-fixture-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("operation.lock");
        fs::write(&path, b"legacy abandoned lock").unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "snapshot::os_lock_tests::lock_child"])
            .env("SUPERKIRO_LOCK_FIXTURE", &path)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !path.with_extension("ready").exists() {
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("child did not acquire fixture lock");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(OperationLock::acquire(path.clone()).is_err());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(path.exists());
        let lock = OperationLock::acquire(path.clone()).unwrap();
        assert!(OperationLock::acquire(path.clone()).is_err());
        drop(lock);
        drop(OperationLock::acquire(path.clone()).unwrap());
        fs::remove_file(path.with_extension("ready")).unwrap();
        fs::remove_file(path).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
