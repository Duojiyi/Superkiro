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
    /// Set when the patch was already gone and its rollback could not run, so
    /// the customer can be told what residue is left instead of being handed a
    /// success that silently skipped half the work.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub extension_unrestorable: Option<String>,
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
                if settings_mgr.revert(&prior_settings, gateway_url).is_ok()
                    && !p.backup_path().exists()
                {
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
        self.restore_official_with(ExtensionPatcher::restore)
    }

    /// `restore_extension` is a seam: the extension rollback is the only step that
    /// can fail after the process gate has passed, and its ordering against the
    /// settings revert is the property worth pinning.
    pub(crate) fn restore_official_with(
        &self,
        restore_extension: impl FnOnce(&ExtensionPatcher) -> Result<bool, PatchError>,
    ) -> Result<RestoreSummary, SnapshotError> {
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

        // Unwind in the reverse of takeover's order. `merge_byok` freezes Kiro's
        // auto-update (`update.mode: "none"`) precisely so an update cannot
        // overwrite the patch; reverting settings first re-arms the updater while
        // extension.js is still patched. An update landing in that window leaves
        // a file that matches neither the original nor any known patched hash, so
        // `restore_material` refuses it and both restore and re-activation are off
        // the table for good. Rolling the extension back first keeps the freeze in
        // place until the patch is provably gone.
        //
        // `restore` re-reads the rollback material before it writes anything, so a
        // failure here still leaves the machine untouched and the retry clean.
        let mut extension_restored = false;
        let mut extension_unrestorable = None;
        if let Some(ref ext_path) = snapshot.extension_path {
            let patcher = ExtensionPatcher::new(ext_path);
            match restore_extension(&patcher) {
                Ok(restored) => extension_restored = restored,
                // Kiro replaced extension.js with a newer official build, so the
                // backup no longer describes what is installed and the rollback
                // can never succeed. Nothing is redirecting the IDE any more and
                // only the settings still name the gateway, so refusing the whole
                // operation would stand between the customer and the half that is
                // provably safe to undo — and leave them no exit at all.
                //
                // The guard must be positive proof. `status()` resolves every read
                // failure to "not marked", so using it here would let a transient
                // I/O error during the force-kill look like a completed rollback
                // and re-arm the updater over a live patch. Only a successful read
                // showing no marker counts, and only for the one error that
                // actually means the material no longer matches.
                Err(PatchError::ExtensionChanged) if patcher.patch_is_provably_absent() => {
                    patcher.discard_obsolete_material()?;
                    extension_unrestorable = Some(
                        "Kiro replaced extension.js before it could be rolled back;                          the stale patch backup has been discarded"
                            .to_string(),
                    );
                }
                // The patch is live. Abort before mutating anything.
                Err(error) => return Err(SnapshotError::Patch(error)),
            }
        }

        let settings_mgr = SettingsManager::at(&snapshot.settings_path);
        settings_mgr.revert(&snapshot.settings_state, &snapshot.gateway_url)?;

        // The takeover is over either way: the settings are reverted and the patch
        // is either rolled back or provably gone along with its material. Keeping
        // the record back would leave `recovery_pending` true for good — activate
        // refuses while a snapshot exists, and every retry lands in the same arm.
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
            extension_unrestorable,
        })
    }
}

// Keep the inode in place: unlinking a lock file allows two independent owners.
pub(crate) struct OperationLock(File);
impl Drop for OperationLock {
    fn drop(&mut self) {
        // A forked child can briefly retain the open file description before exec.
        // Release ownership explicitly instead of waiting for its last handle to close.
        let _ = self.0.unlock();
    }
}
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

    /// `acquire`, waiting up to `wait` while another owner holds the lock.
    pub(crate) fn acquire_within(
        path: PathBuf,
        wait: std::time::Duration,
    ) -> std::io::Result<Self> {
        let deadline = std::time::Instant::now() + wait;
        loop {
            match Self::acquire(path.clone()) {
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                result => return result,
            }
        }
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
    fn a_waiting_owner_gets_a_released_lock_and_gives_up_after_its_wait() {
        use std::time::{Duration, Instant};
        let path = std::env::temp_dir().join(format!("os-lock-wait-{}", std::process::id()));
        let held = OperationLock::acquire(path.clone()).unwrap();
        let started = Instant::now();
        let refused = OperationLock::acquire_within(path.clone(), Duration::from_millis(200));
        assert_eq!(
            refused.err().map(|error| error.kind()),
            Some(std::io::ErrorKind::WouldBlock)
        );
        assert!(started.elapsed() >= Duration::from_millis(200));
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(held);
        });
        let acquired = OperationLock::acquire_within(path.clone(), Duration::from_secs(10));
        release.join().unwrap();
        assert!(acquired.is_ok());
        drop(acquired);
        fs::remove_file(path).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn dropping_owner_unlocks_even_with_a_duplicated_descriptor() {
        let path = std::env::temp_dir().join(format!("os-lock-duplicate-{}", std::process::id()));
        let lock = OperationLock::acquire(path.clone()).unwrap();
        // dup shares the open file description, just as inheritance across fork does.
        let duplicate = lock.0.try_clone().unwrap();
        assert!(OperationLock::acquire(path.clone()).is_err());
        drop(lock);
        let next = OperationLock::acquire(path.clone()).unwrap();
        drop(duplicate);
        assert!(OperationLock::acquire(path.clone()).is_err());
        drop(next);
        fs::remove_file(path).unwrap();
    }

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

#[cfg(test)]
mod restore_order_tests {
    use super::*;
    use crate::patch::PatchStatus;
    use serde_json::json;

    /// A restore whose extension rollback fails must leave the machine exactly as
    /// it found it. What is at stake is `update.mode: "none"`: takeover sets it so
    /// a Kiro auto-update cannot overwrite the patch, and reverting settings before
    /// the patch is gone hands the updater a window in which it can replace
    /// extension.js with a file matching neither the original nor any known patched
    /// hash — after which `restore_material` refuses it and neither restore nor
    /// re-activation is ever possible again.
    ///
    /// The seam stands for every way the rollback can fail once the process gate has
    /// passed: a transient process-enumeration failure at `ExtensionPatcher::restore`,
    /// or any I/O error writing extension.js back.
    #[test]
    fn a_failed_extension_rollback_leaves_the_auto_update_freeze_in_place() {
        let dir = env::temp_dir().join(format!("kiro-restore-order-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let settings_file = dir.join("settings.json");
        let ext_file = dir.join("extension.js");
        fs::write(
            &settings_file,
            r#"{"editor.tabSize":2,"update.mode":"manual"}"#,
        )
        .unwrap();
        fs::write(
            &ext_file,
            format!("const e = \"{}\";", crate::patch::RUNTIME_ENDPOINT_NEEDLE),
        )
        .unwrap();

        let settings_mgr = SettingsManager::at(&settings_file);
        let patcher = ExtensionPatcher::new(&ext_file);
        let snapshots = SnapshotManager::at(dir.join("snapshot.json"));
        snapshots
            .takeover(&settings_mgr, Some(&patcher), "https://gw.order.test")
            .unwrap();
        assert_eq!(patcher.status(), PatchStatus::Patched);

        let error = snapshots
            .restore_official_with(|_| Err(PatchError::ExtensionChanged))
            .expect_err("a failed extension rollback must fail the restore");
        assert!(matches!(error, SnapshotError::Patch(_)), "{error:?}");

        let after = settings_mgr.read_settings().unwrap();
        assert_eq!(
            after.get("update.mode"),
            Some(&json!("none")),
            "auto-update was re-armed while extension.js was still patched"
        );
        assert!(
            after.contains_key("kiroAuthConfig"),
            "settings were rolled back while the patch was still live"
        );
        assert_eq!(patcher.status(), PatchStatus::Patched);
        assert!(snapshots.has_active_snapshot());

        // Nothing was mutated, so the retry is clean.
        let summary = snapshots.restore_official().unwrap();
        assert!(summary.settings_restored && summary.extension_restored);
        assert_eq!(patcher.status(), PatchStatus::Official);
        assert_eq!(
            settings_mgr.read_settings().unwrap().get("update.mode"),
            Some(&json!("manual"))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// The tolerant arm must not be fooled by the failure it is guarding.
    ///
    /// `status()` resolves every read failure to "not marked" — a missing or
    /// unreadable extension.js reports `NotFound`/`UpgradeDetected`, never
    /// `Patched`. Deciding "the patch is gone, so it is safe to revert settings"
    /// from that reading means a transient I/O error during the force-kill can
    /// re-arm Kiro's updater over a patch that is still live, which is the exact
    /// disaster the reordering exists to prevent.
    #[test]
    fn an_unreadable_extension_is_not_proof_that_the_patch_is_gone() {
        let dir = env::temp_dir().join(format!("kiro-restore-proof-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let settings_file = dir.join("settings.json");
        let ext_file = dir.join("extension.js");
        fs::write(
            &settings_file,
            r#"{"editor.tabSize":2,"update.mode":"manual"}"#,
        )
        .unwrap();
        fs::write(
            &ext_file,
            format!("const e = \"{}\";", crate::patch::RUNTIME_ENDPOINT_NEEDLE),
        )
        .unwrap();

        let settings_mgr = SettingsManager::at(&settings_file);
        let patcher = ExtensionPatcher::new(&ext_file);
        let snapshots = SnapshotManager::at(dir.join("snapshot.json"));
        snapshots
            .takeover(&settings_mgr, Some(&patcher), "https://gw.proof.test")
            .unwrap();

        // Stands in for every way the file can become unreadable mid-restore:
        // `status()` reports NotFound, which is not Patched, yet nothing proves
        // the patch was rolled back.
        fs::remove_file(&ext_file).unwrap();
        assert_ne!(patcher.status(), PatchStatus::Patched);
        assert!(!patcher.patch_is_provably_absent());

        let error = snapshots
            .restore_official_with(|_| Err(PatchError::ExtensionChanged))
            .expect_err("an unreadable extension must not be read as a finished rollback");
        assert!(matches!(error, SnapshotError::Patch(_)), "{error:?}");

        let after = settings_mgr.read_settings().unwrap();
        assert_eq!(
            after.get("update.mode"),
            Some(&json!("none")),
            "auto-update was re-armed on the strength of a file that could not be read"
        );
        assert!(after.contains_key("kiroAuthConfig"));
        assert!(
            snapshots.has_active_snapshot(),
            "the rollback record must outlive a restore that changed nothing"
        );
        assert!(
            patcher.backup_path().exists(),
            "rollback material must not be discarded without proof it is obsolete"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
