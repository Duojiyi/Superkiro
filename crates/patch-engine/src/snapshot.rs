//! Reversible takeover snapshot and one-click official restore (Spec §9, §14.5, P0-3).
//!
//! Features:
//! - Takes a complete, self-contained snapshot before applying any takeover or patch.
//! - Executes 100% clean rollback restoring official Kiro state with zero residue.

use crate::patch::{ExtensionPatcher, PatchError, PreparedPatch};
use crate::runtime::{detect_kiro_process_state, ProcessState};
use crate::settings::{PriorSettingsState, Profile, SettingsError, SettingsManager};
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

    #[error("Kiro's extension is still modified and its backup is gone or no longer matches; reinstall Kiro to replace it, then restore again")]
    ReinstallRequired,

    #[error("Kiro's installation changed after it was checked, likely an update installed as Kiro closed; nothing was changed. Open Kiro once, then try again")]
    InstallationChanged,
}

/// What a takeover will write, worked out by [`SnapshotManager::plan_takeover`] before
/// Kiro is closed: the settings edit it makes, and the patched bundle Kiro's own runtime
/// has passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeoverPlan {
    patch: Option<PreparedPatch>,
}

/// Metadata recorded during takeover to ensure precision rollback.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TakeoverSnapshot {
    pub created_at: u64,
    pub gateway_url: String,
    pub settings_path: PathBuf,
    pub settings_state: PriorSettingsState,
    pub extension_path: Option<PathBuf>,
    /// The settings.json of each of Kiro's other profiles the takeover configured, as it
    /// found them. Records written before profiles were configured have none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<ProfileRecord>,
}

impl TakeoverSnapshot {
    /// Put the settings back as the takeover found them: Default's, then each profile's.
    fn revert_settings(&self) -> Result<(), SettingsError> {
        SettingsManager::at(&self.settings_path).revert(&self.settings_state, &self.gateway_url)?;
        self.profiles
            .iter()
            .try_for_each(|profile| profile.revert(&self.gateway_url))
    }
}

/// A profile's settings.json as the takeover found it, for its rollback.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileRecord {
    pub name: String,
    pub settings_path: PathBuf,
    pub settings_state: PriorSettingsState,
    /// Folders the takeover made for the file, outermost first. The rollback removes
    /// each it finds empty; one Kiro has put something in since is Kiro's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created_folders: Vec<PathBuf>,
}

impl ProfileRecord {
    /// `profile` as it is now, before the takeover writes its settings.
    fn capture(profile: &Profile, gateway_url: &str) -> Result<Self, SettingsError> {
        let path = profile.settings.path();
        let mut created_folders: Vec<PathBuf> = path
            .ancestors()
            .skip(1)
            .take_while(|folder| !folder.exists())
            .map(Path::to_path_buf)
            .collect();
        created_folders.reverse();
        let settings_state = profile
            .settings
            .capture_prior_state()
            .map_err(|error| error.in_profile(&profile.name))?
            .without_takeover_of(gateway_url);
        Ok(Self {
            name: profile.name.clone(),
            settings_path: path.to_path_buf(),
            settings_state,
            created_folders,
        })
    }

    fn settings(&self) -> SettingsManager {
        SettingsManager::at(&self.settings_path)
    }

    /// What the rollback does to the file, worked out without writing anything; nothing
    /// when the file is gone (the profile removed in Kiro), and none of ours with it.
    fn plan_revert(&self, gateway_url: &str) -> Result<(), SettingsError> {
        if self.settings_path.exists() {
            self.settings()
                .plan_revert(&self.settings_state, gateway_url)
                .map_err(|error| error.in_profile(&self.name))?;
        }
        Ok(())
    }

    /// Put the file back as the takeover found it, and take away the folders it made.
    fn revert(&self, gateway_url: &str) -> Result<(), SettingsError> {
        if self.settings_path.exists() {
            self.settings()
                .revert(&self.settings_state, gateway_url)
                .map_err(|error| error.in_profile(&self.name))?;
        }
        for folder in self.created_folders.iter().rev() {
            // Removes only an empty folder, and there is nothing to do for one that is
            // gone or no longer empty.
            let _ = fs::remove_dir(folder);
        }
        Ok(())
    }
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

    /// Everything a takeover will do, checked without writing anything: see
    /// [`TakeoverPlan`].
    pub fn validate_takeover(
        &self,
        settings: &SettingsManager,
        patcher: Option<&ExtensionPatcher>,
        gateway: &str,
    ) -> Result<(), SnapshotError> {
        self.plan_takeover(settings, patcher, gateway).map(drop)
    }

    /// Work out and check what a takeover will write, writing nothing: the settings edit
    /// is built and read back in memory, and the patch is rendered for `gateway` and run
    /// past Kiro's own runtime. Done before Kiro is closed and before the token changes,
    /// a file the takeover cannot edit is refused while nothing has been touched.
    pub fn plan_takeover(
        &self,
        settings: &SettingsManager,
        patcher: Option<&ExtensionPatcher>,
        gateway: &str,
    ) -> Result<TakeoverPlan, SnapshotError> {
        self.check_records(settings, patcher, gateway)?;
        self.plan_settings(settings, gateway)?;
        Ok(TakeoverPlan {
            patch: patcher.map(|p| p.prepare(gateway)).transpose()?,
        })
    }

    /// Whether `plan` still holds for this machine: the same rollback records, a
    /// settings edit that can still be made, and extension.js the very bundle checked.
    /// Kiro's runtime does not run again.
    pub fn confirm_plan(
        &self,
        settings: &SettingsManager,
        patcher: Option<&ExtensionPatcher>,
        gateway: &str,
        plan: &TakeoverPlan,
    ) -> Result<(), SnapshotError> {
        self.check_records(settings, patcher, gateway)?;
        self.plan_settings(settings, gateway)?;
        let unchanged = match (patcher, &plan.patch) {
            (Some(p), Some(prepared)) => p.is_as_prepared(prepared)?,
            (None, None) => true,
            _ => false,
        };
        if !unchanged {
            return Err(SnapshotError::InstallationChanged);
        }
        Ok(())
    }

    /// The settings edit the takeover makes: all of it the first time, the keys it
    /// re-asserts on a machine already taken over. The same for each of Kiro's other
    /// profiles with settings of their own, all of it for one made since the takeover; a
    /// profile whose settings cannot be edited is named in the error.
    fn plan_settings(
        &self,
        settings: &SettingsManager,
        gateway: &str,
    ) -> Result<(), SnapshotError> {
        let recorded = if self.has_active_snapshot() {
            settings.plan_reassert(gateway)?;
            self.load()?.profiles
        } else {
            settings.plan_merge(gateway)?;
            Vec::new()
        };
        for profile in settings.profiles()? {
            let planned = if recorded
                .iter()
                .any(|r| r.settings_path == profile.settings.path())
            {
                profile.settings.plan_reassert(gateway)
            } else {
                profile.settings.plan_merge(gateway)
            };
            planned.map_err(|error| error.in_profile(&profile.name))?;
        }
        Ok(())
    }

    /// A repeat operation must match the original durable snapshot and current files;
    /// a first one starts from Kiro's official bundle.
    fn check_records(
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
        } else if let Some(p) = patcher {
            if p.status() != crate::patch::PatchStatus::Official {
                return Err(SnapshotError::ActiveTakeoverConflict);
            }
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
        self.takeover_with(settings_mgr, patcher, gateway_url, None)
    }

    /// [`takeover`](Self::takeover) once Kiro has been closed, writing only what `plan`
    /// checked before. Closing Kiro is when an update waiting for it installs itself, so
    /// an installation that is no longer the one checked is refused before anything is
    /// written. Kiro's own runtime is not run again on the bytes it already passed.
    pub fn takeover_planned(
        &self,
        settings_mgr: &SettingsManager,
        patcher: Option<&ExtensionPatcher>,
        gateway_url: &str,
        plan: &TakeoverPlan,
    ) -> Result<TakeoverSnapshot, SnapshotError> {
        self.takeover_with(settings_mgr, patcher, gateway_url, Some(plan))
    }

    fn takeover_with(
        &self,
        settings_mgr: &SettingsManager,
        patcher: Option<&ExtensionPatcher>,
        gateway_url: &str,
        plan: Option<&TakeoverPlan>,
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
        let plan = match plan {
            Some(plan) => {
                self.confirm_plan(settings_mgr, patcher, gateway_url, plan)?;
                plan.clone()
            }
            None => self.plan_takeover(settings_mgr, patcher, gateway_url)?,
        };
        let apply = |p: &ExtensionPatcher| match &plan.patch {
            Some(prepared) => p.apply_prepared(gateway_url, prepared),
            None => p.apply(gateway_url),
        };
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
            // A profile made in Kiro since the takeover gets all of it, recorded first as
            // the others were: until now its windows had the gateway's token without the
            // gateway's endpoints.
            let profiles = settings_mgr.profiles()?;
            let recorded = snapshot.profiles.len();
            for profile in &profiles {
                if !snapshot
                    .profiles
                    .iter()
                    .any(|record| record.settings_path == profile.settings.path())
                {
                    snapshot
                        .profiles
                        .push(ProfileRecord::capture(profile, gateway_url)?);
                }
            }
            if snapshot.profiles.len() > recorded {
                self.save(&snapshot)?;
            }
            settings_mgr.reassert_byok(gateway_url)?;
            for profile in &profiles {
                let new = snapshot.profiles[recorded..]
                    .iter()
                    .any(|record| record.settings_path == profile.settings.path());
                if new {
                    profile.settings.merge_byok(gateway_url).map(drop)
                } else {
                    profile.settings.reassert_byok(gateway_url)
                }
                .map_err(|error| error.in_profile(&profile.name))?;
            }
            if let Some(p) = patcher {
                apply(p)?;
            }
            return Ok(snapshot);
        }

        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // 1. Capture the rollback record and publish it before any mutation.
        let prior_settings = settings_mgr.capture_prior_state()?;
        let profiles = settings_mgr.profiles()?;
        let planned_snapshot = TakeoverSnapshot {
            created_at: now_secs,
            gateway_url: gateway_url.to_string(),
            settings_path: settings_mgr.path().to_path_buf(),
            settings_state: prior_settings,
            extension_path: patcher.map(|p| p.path().to_path_buf()),
            profiles: profiles
                .iter()
                .map(|profile| ProfileRecord::capture(profile, gateway_url))
                .collect::<Result<_, _>>()?,
        };
        self.save(&planned_snapshot)?;

        // 2. Apply settings and extension only after the rollback record is durable.
        if let Err(error) = settings_mgr.merge_byok(gateway_url) {
            let _ = fs::remove_file(&self.snapshot_path);
            return Err(SnapshotError::Settings(error));
        }
        for profile in &profiles {
            if let Err(error) = profile.settings.merge_byok(gateway_url) {
                // Keep the recovery record if rollback itself fails.
                if planned_snapshot.revert_settings().is_ok() {
                    let _ = fs::remove_file(&self.snapshot_path);
                }
                return Err(SnapshotError::Settings(error.in_profile(&profile.name)));
            }
        }

        // 2. Patch extension if provided
        let ext_path = if let Some(p) = patcher {
            if let Err(e) = apply(p) {
                // Keep the recovery record if rollback itself fails.
                if planned_snapshot.revert_settings().is_ok() && !p.backup_path().exists() {
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
        let settings_mgr = SettingsManager::at(&snapshot.settings_path);

        // Settings that cannot be rolled back (a syntax error the customer made while
        // taken over, say) must fail the restore before any file changes. Found only
        // after the extension was rolled back, they left Kiro on its official bundle
        // with the gateway's settings and token, and every retry failed the same way.
        // Each profile's settings are checked the same way.
        settings_mgr.plan_revert(&snapshot.settings_state, &snapshot.gateway_url)?;
        for profile in &snapshot.profiles {
            profile.plan_revert(&snapshot.gateway_url)?;
        }

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
                // The patch is live and what would roll it back is gone or no longer
                // matches, so only reinstalling Kiro replaces the file. Until then the
                // settings and the token stay as they are: rolled back under a live
                // patch, the customer's own token would go to the gateway through the
                // patched runtime endpoint. Once Kiro is reinstalled, this same restore
                // completes.
                Err(PatchError::ExtensionChanged) if patcher.restore_material_is_lost() => {
                    return Err(SnapshotError::ReinstallRequired)
                }
                // The patch is live. Abort before mutating anything.
                Err(error) => return Err(SnapshotError::Patch(error)),
            }
        }

        snapshot.revert_settings()?;

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
mod plan_tests {
    use super::*;
    use crate::patch::{PatchStatus, JAVASCRIPT_CHECKS, RUNTIME_ENDPOINT_NEEDLE};

    const GATEWAY: &str = "https://gw.plan.test";
    const SETTINGS: &str = "{\n  \"editor.fontSize\": 14\n}\n";

    fn machine(name: &str) -> (PathBuf, SettingsManager, ExtensionPatcher, SnapshotManager) {
        let dir = env::temp_dir().join(format!("kiro-plan-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("settings.json"), SETTINGS).unwrap();
        fs::write(
            dir.join("extension.js"),
            format!("const e = \"{RUNTIME_ENDPOINT_NEEDLE}\";"),
        )
        .unwrap();
        (
            dir.clone(),
            SettingsManager::at(dir.join("settings.json")),
            ExtensionPatcher::new(dir.join("extension.js")),
            SnapshotManager::at(dir.join("snapshot.json")),
        )
    }

    fn checks() -> usize {
        JAVASCRIPT_CHECKS.with(|checks| checks.get())
    }

    /// Kiro's own runtime checks the patched bundle once, before Kiro is closed. After
    /// the close the takeover writes exactly those bytes without running it again: that
    /// executable is what an update waiting for Kiro to close replaces.
    #[test]
    fn a_planned_takeover_does_not_run_kiro_again() {
        let (dir, settings, patcher, snapshots) = machine("once");
        let before = checks();
        let plan = snapshots
            .plan_takeover(&settings, Some(&patcher), GATEWAY)
            .unwrap();
        assert_eq!(checks(), before + 1);
        snapshots
            .takeover_planned(&settings, Some(&patcher), GATEWAY, &plan)
            .unwrap();
        assert_eq!(
            checks(),
            before + 1,
            "Kiro's runtime ran again after the close"
        );
        assert_eq!(patcher.status(), PatchStatus::Patched);
        assert!(settings.is_byok_active(Some(GATEWAY)));
        snapshots.restore_official().unwrap();
        assert_eq!(fs::read_to_string(settings.path()).unwrap(), SETTINGS);
        let _ = fs::remove_dir_all(dir);
    }

    /// A bundle that is no longer the one checked (an update that installed itself as
    /// Kiro closed) is refused before anything is written.
    #[test]
    fn a_bundle_changed_since_the_check_is_refused_before_anything_is_written() {
        let (dir, settings, patcher, snapshots) = machine("changed");
        let plan = snapshots
            .plan_takeover(&settings, Some(&patcher), GATEWAY)
            .unwrap();
        let updated = format!("/* 2.0 */ const e = \"{RUNTIME_ENDPOINT_NEEDLE}\";");
        fs::write(patcher.path(), &updated).unwrap();

        let error = snapshots
            .takeover_planned(&settings, Some(&patcher), GATEWAY, &plan)
            .unwrap_err();
        assert!(
            matches!(error, SnapshotError::InstallationChanged),
            "{error:?}"
        );
        assert_eq!(fs::read_to_string(settings.path()).unwrap(), SETTINGS);
        assert_eq!(fs::read_to_string(patcher.path()).unwrap(), updated);
        assert!(!snapshots.has_active_snapshot());
        assert!(!patcher.backup_path().exists());
        let _ = fs::remove_dir_all(dir);
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
