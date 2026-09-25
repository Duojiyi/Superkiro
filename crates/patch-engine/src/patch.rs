//! Extension binary patching and launcher environment injection (Spec §2.1, §2.5, §9, P0-2, P0-3).
//!
//! Features:
//! - Launcher environment variable injection (`KIRO_AUTH_PORTAL_URL`, `KIRO_DISABLE_*`).
//! - Extension patcher (`extension.js` runtime endpoint rewrite with marker & backup).
//! - Runtime safety check (`is_kiro_running()`) preventing patching while Kiro is active.
//! - Idempotent application and clean rollback from `.kpatch-backup`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use thiserror::Error;

/// Header marker placed at the beginning of patched `extension.js`.
pub const PATCH_MARKER_V1: &str = "/* @patched-kiro-byok v1 */";
/// Version-agnostic prefix every patch marker starts with.
pub const PATCH_MARKER_PREFIX: &str = "/* @patched-kiro-byok ";

/// Backup file suffix for original `extension.js`.
pub const BACKUP_SUFFIX: &str = ".kpatch-backup";

/// Target runtime endpoint string to be rewritten in `extension.js`.
pub const RUNTIME_ENDPOINT_NEEDLE: &str = "https://runtime.${t}.kiro.dev";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PatchError {
    #[error(
        "Kiro IDE is currently running. Please close Kiro before applying or reverting patches."
    )]
    KiroRunning,

    #[error("Target extension file not found at '{0}'")]
    FileNotFound(PathBuf),

    #[error("Target file is not a valid kiro-agent bundle: runtime endpoint needle not found")]
    NeedleNotFound,

    #[error("Extension changed after patching or was upgraded; original backup retained, automatic overwrite refused")]
    ExtensionChanged,

    #[error("JavaScript validation failed: {0}")]
    InvalidJavaScript(String),

    #[error("Invalid gateway URL: {0}")]
    InvalidUrl(String),

    #[error("Kiro process state cannot be determined safely: {0}")]
    ProcessStateUnknown(String),

    #[error("I/O error on '{0}': {1}")]
    Io(PathBuf, String),
}

/// Generate recommended launcher environment variables for Kiro BYOK process.
pub fn get_launcher_env(gateway_url: &str) -> HashMap<String, String> {
    let gw = gateway_url.trim_end_matches('/');
    let mut envs = HashMap::new();

    // 1. Redirect auth portal
    envs.insert("KIRO_AUTH_PORTAL_URL".to_string(), gw.to_string());

    // No AWS_ENDPOINT_URL. AWS SDKs and CLI v2 read it as an override for every service,
    // and every terminal, task, debug session and extension inside Kiro inherits it — so a
    // customer's own `aws` commands, and AWS extensions, sent their signed requests,
    // session tokens and payloads to the gateway. Kiro does not need it: its model
    // endpoints come from `codewhisperer.config` in user settings, and its runtime
    // endpoint from the patch below.

    // 3. Disable auxiliary LLM surfaces to control credit costs (Spec §2.5, P0-7)
    envs.insert(
        "KIRO_DISABLE_SESSION_TITLE_LLM".to_string(),
        "true".to_string(),
    );
    envs.insert("KIRO_DISABLE_RECAP".to_string(), "true".to_string());

    // 4. Custom gateway URL reference for patched code
    envs.insert("KIRO_GATEWAY_URL".to_string(), gw.to_string());

    if let Ok(url) = reqwest::Url::parse(gw) {
        if let Some(host) = url.host_str() {
            let mut bypass = std::env::var("no_proxy")
                .or_else(|_| std::env::var("NO_PROXY"))
                .unwrap_or_default();
            if !bypass.split(',').any(|entry| entry.trim() == host) {
                if !bypass.is_empty() {
                    bypass.push(',');
                }
                bypass.push_str(host);
            }
            envs.insert("NO_PROXY".into(), bypass.clone());
            envs.insert("no_proxy".into(), bypass);
        }
    }
    envs
}

/// Server-distributed patch recipe for dynamic Kiro extension rewriting (Spec §10, P4-1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PatchRecipe {
    pub recipe_id: String,
    pub marker: String,
    pub needle: String,
    pub replacement: String,
    #[serde(default)]
    pub extra_envs: HashMap<String, String>,
}

impl Default for PatchRecipe {
    fn default() -> Self {
        Self {
            recipe_id: "v1-standard".to_string(),
            marker: PATCH_MARKER_V1.to_string(),
            needle: RUNTIME_ENDPOINT_NEEDLE.to_string(),
            replacement: "http://127.0.0.1:44040/runtime".to_string(),
            extra_envs: HashMap::new(),
        }
    }
}

/// Status of extension bundle patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatchStatus {
    /// File does not exist.
    NotFound,
    /// Official unmodified extension.
    Official,
    /// Successfully patched by BYOK.
    Patched,
    /// Was previously patched, but Kiro upgrade overwrote the file (backup exists, target lacks marker).
    UpgradeDetected,
}

#[derive(Serialize, Deserialize)]
struct PatchState {
    original_len: u64,
    original_hash: String,
    patched_hash: String,
    #[serde(default)]
    previous_patched_hash: Option<String>,
    /// Who patched it: [`current_owner`]. An install shared by several users of the
    /// computer can carry another user's live takeover, which is theirs to undo.
    /// Absent in state written before owners were recorded.
    #[serde(default)]
    owner: Option<String>,
}

/// This user, as a patch's owner: their home directory.
pub(crate) fn current_owner() -> Option<String> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(|home| home.to_string_lossy().to_lowercase())
}

/// Whose patch this is, as far as its files tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchOwnership {
    /// No patch marker.
    Unpatched,
    /// Patched by this user, or before owners were recorded.
    Ours,
    /// Patched by another user of this computer.
    Theirs,
}

/// A patch rendered and checked by [`ExtensionPatcher::prepare`]: the bundle it was made
/// from, and the patched bytes that passed, by hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedPatch {
    source: String,
    patched: String,
}

/// Helper for inspecting and modifying `extension.js`.
#[derive(Debug, Clone)]
pub struct ExtensionPatcher {
    extension_path: PathBuf,
}

impl ExtensionPatcher {
    pub fn new(extension_path: impl Into<PathBuf>) -> Self {
        Self {
            extension_path: extension_path.into(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.extension_path
    }

    pub fn backup_path(&self) -> PathBuf {
        let mut p = self.extension_path.clone().into_os_string();
        p.push(BACKUP_SUFFIX);
        PathBuf::from(p)
    }

    /// Whether the file starts with the patch marker, reading only as many bytes. The
    /// bundle is over 10 MB, and status is polled.
    pub fn marker_present(&self) -> std::io::Result<bool> {
        use std::io::Read;
        let mut head = Vec::with_capacity(PATCH_MARKER_PREFIX.len());
        fs::File::open(&self.extension_path)?
            .take(PATCH_MARKER_PREFIX.len() as u64)
            .read_to_end(&mut head)?;
        Ok(head == PATCH_MARKER_PREFIX.as_bytes())
    }

    /// Whose patch this file carries. Unreadable state counts as ours: it is then this
    /// user's own rollback that cannot proceed, never another user's patch undone.
    pub fn ownership(&self) -> PatchOwnership {
        if !self.marker_present().unwrap_or(false) {
            return PatchOwnership::Unpatched;
        }
        match self.read_state().ok().and_then(|state| state.owner) {
            Some(owner) if current_owner().as_ref() != Some(&owner) => PatchOwnership::Theirs,
            _ => PatchOwnership::Ours,
        }
    }

    /// Positive proof that this patch can never be rolled back from its own material:
    /// the file carries the marker, and what a rollback needs (the state that
    /// authenticates it, and the backup of the original) is gone or reads as something
    /// else, or the live file is not what the patch wrote. Only replacing the file, by
    /// reinstalling or updating Kiro, undoes it then.
    ///
    /// A read that fails proves nothing: a virus scan can lock the backup for a moment,
    /// and telling the customer to reinstall Kiro over that would be wrong.
    pub fn restore_material_is_lost(&self) -> bool {
        let gone = |error: std::io::Error| error.kind() == std::io::ErrorKind::NotFound;
        if !self.marker_present().unwrap_or(false) {
            return false;
        }
        let state: PatchState = match fs::read(self.state_path()) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(state) => state,
                Err(_) => return true,
            },
            Err(error) => return gone(error),
        };
        let Ok(current) = fs::read(&self.extension_path) else {
            return false;
        };
        let current = content_hash(&current);
        if current != state.patched_hash && state.previous_patched_hash.as_ref() != Some(&current) {
            return true;
        }
        match fs::read(self.backup_path()) {
            Ok(backup) => {
                backup.len() as u64 != state.original_len
                    || content_hash(&backup) != state.original_hash
            }
            Err(error) => gone(error),
        }
    }

    /// Determine current patch status.
    pub fn status(&self) -> PatchStatus {
        if !self.extension_path.exists() {
            return PatchStatus::NotFound;
        }

        let is_marked = self.marker_present().unwrap_or(false);

        if is_marked {
            PatchStatus::Patched
        } else if self.backup_path().exists() {
            PatchStatus::UpgradeDetected
        } else {
            PatchStatus::Official
        }
    }

    /// Positive proof that the patch is no longer on disk: the file was read and
    /// carries no marker.
    ///
    /// `status()` cannot answer this. Every read failure there resolves to "not
    /// marked" — an unreadable file reports `UpgradeDetected` or `Official` — so
    /// using it to decide whether a patch is still live lets a transient I/O
    /// error masquerade as a completed rollback. A read that fails proves nothing.
    pub fn patch_is_provably_absent(&self) -> bool {
        fs::read(&self.extension_path)
            .map(|content| !content.starts_with(PATCH_MARKER_PREFIX.as_bytes()))
            .unwrap_or(false)
    }

    /// Drop rollback material for a patch that is provably gone. Kiro replaced
    /// extension.js with a newer official build, so the backup describes a version
    /// that is no longer installed; keeping it only blocks re-activation.
    pub fn discard_obsolete_material(&self) -> Result<(), PatchError> {
        for path in [self.backup_path(), self.state_path()] {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(PatchError::Io(path, e.to_string())),
            }
        }
        Ok(())
    }

    /// Dry run: verify whether patch can be cleanly applied without writing to disk.
    pub fn dry_run(&self) -> Result<bool, PatchError> {
        if !self.extension_path.exists() {
            return Err(PatchError::FileNotFound(self.extension_path.clone()));
        }

        let content = fs::read_to_string(&self.extension_path)
            .map_err(|e| PatchError::Io(self.extension_path.clone(), e.to_string()))?;

        if content.starts_with(PATCH_MARKER_PREFIX) {
            return Ok(true); // Already patched
        }

        if self.backup_path().exists() {
            return Err(PatchError::ExtensionChanged);
        }
        let rendered = render_patch(&content, "https://gateway.invalid", &PatchRecipe::default())?;
        validate_javascript(&self.extension_path, &rendered)?;
        Ok(true)
    }

    /// Render the patch for `gateway_url` and have Kiro's own runtime check it, writing
    /// nothing. The result names the bundle read and the patched bytes that passed, so
    /// the takeover, once Kiro has been closed, can write exactly those without running
    /// Kiro again.
    pub fn prepare(&self, gateway_url: &str) -> Result<PreparedPatch, PatchError> {
        if !self.extension_path.exists() {
            return Err(PatchError::FileNotFound(self.extension_path.clone()));
        }
        let content = fs::read_to_string(&self.extension_path)
            .map_err(|e| PatchError::Io(self.extension_path.clone(), e.to_string()))?;
        let patched = if content.starts_with(PATCH_MARKER_PREFIX) {
            self.verify_patched_content()?;
            let repaired = self.updated_patch(&content, gateway_url, &PatchRecipe::default());
            if repaired != content {
                validate_javascript(&self.extension_path, &repaired)?;
            }
            repaired
        } else {
            if self.backup_path().exists() {
                return Err(PatchError::ExtensionChanged);
            }
            let rendered = render_patch(&content, gateway_url, &PatchRecipe::default())?;
            validate_javascript(&self.extension_path, &rendered)?;
            rendered
        };
        Ok(PreparedPatch {
            source: content_hash(content.as_bytes()),
            patched: content_hash(patched.as_bytes()),
        })
    }

    /// Whether extension.js is still the bundle `prepared` was made from.
    pub fn is_as_prepared(&self, prepared: &PreparedPatch) -> Result<bool, PatchError> {
        let content = fs::read(&self.extension_path)
            .map_err(|e| PatchError::Io(self.extension_path.clone(), e.to_string()))?;
        Ok(content_hash(&content) == prepared.source)
    }

    /// Apply the BYOK patch to `extension.js` using default recipe.
    pub fn apply(&self, gateway_url: &str) -> Result<(), PatchError> {
        self.apply_checked(gateway_url, &PatchRecipe::default(), None)
    }

    /// [`apply`](Self::apply), taking the check `prepared` records as done for exactly
    /// the bytes it names; anything else is checked again.
    pub fn apply_prepared(
        &self,
        gateway_url: &str,
        prepared: &PreparedPatch,
    ) -> Result<(), PatchError> {
        self.apply_checked(gateway_url, &PatchRecipe::default(), Some(prepared))
    }

    /// Apply the BYOK patch to `extension.js` using a server-distributed recipe.
    ///
    /// Refuses to patch if Kiro is running to prevent file corruption.
    /// Creates a `.kpatch-backup` before applying changes.
    pub fn apply_with_recipe(
        &self,
        gateway_url: &str,
        recipe: &PatchRecipe,
    ) -> Result<(), PatchError> {
        self.apply_checked(gateway_url, recipe, None)
    }

    fn apply_checked(
        &self,
        gateway_url: &str,
        recipe: &PatchRecipe,
        prepared: Option<&PreparedPatch>,
    ) -> Result<(), PatchError> {
        match crate::runtime::detect_kiro_process_state() {
            crate::runtime::ProcessState::Running => return Err(PatchError::KiroRunning),
            crate::runtime::ProcessState::Unknown => {
                return Err(PatchError::ProcessStateUnknown(
                    "Cannot verify whether Kiro is running; modification forbidden".to_string(),
                ))
            }
            crate::runtime::ProcessState::Stopped => {}
        }

        if !self.extension_path.exists() {
            return Err(PatchError::FileNotFound(self.extension_path.clone()));
        }

        let content = fs::read_to_string(&self.extension_path)
            .map_err(|e| PatchError::Io(self.extension_path.clone(), e.to_string()))?;
        // Kiro's runtime has already passed exactly these bytes, made from exactly this
        // bundle, when `prepared` says so.
        let check = |patched: &str| {
            let passed = prepared.is_some_and(|prepared| {
                prepared.source == content_hash(content.as_bytes())
                    && prepared.patched == content_hash(patched.as_bytes())
            });
            if passed {
                Ok(())
            } else {
                validate_javascript(&self.extension_path, patched)
            }
        };

        if self.status() == PatchStatus::Patched {
            self.verify_patched_content()?;
            let repaired = self.updated_patch(&content, gateway_url, recipe);
            if repaired != content {
                check(&repaired)?;
                let mut state = self.read_state()?;
                state.previous_patched_hash = Some(content_hash(content.as_bytes()));
                state.patched_hash = content_hash(repaired.as_bytes());
                self.write_state(&state)?;
                self.atomic_write(&repaired)?;
            }
            return Ok(());
        }
        if self.backup_path().exists() {
            return Err(PatchError::ExtensionChanged);
        }
        let full_patched = render_patch(&content, gateway_url, recipe)?;
        check(&full_patched)?;

        // No backup or live mutation until both the URL and complete JS parse pass.
        // Persist original identity before the first backup write. A failed/partial
        // backup must never be inferred to be trustworthy from its mere existence.
        self.write_state(&PatchState {
            original_len: content.len() as u64,
            original_hash: content_hash(content.as_bytes()),
            patched_hash: content_hash(full_patched.as_bytes()),
            previous_patched_hash: None,
            owner: current_owner(),
        })?;
        let backup_path = self.backup_path();
        let mut backup = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup_path)
            .map_err(|e| PatchError::Io(backup_path.clone(), e.to_string()))?;
        backup
            .write_all(content.as_bytes())
            .and_then(|_| backup.sync_all())
            .map_err(|e| PatchError::Io(backup_path.clone(), e.to_string()))?;
        self.atomic_write(&full_patched)?;
        Ok(())
    }

    fn state_path(&self) -> PathBuf {
        self.extension_path.with_extension("js.kpatch-state")
    }

    /// The already-patched bundle as the current rules would patch it, rendered afresh
    /// from the verified original in the backup; None when the backup cannot vouch for
    /// itself, and only the incremental repairs apply then. A patch written by an earlier
    /// client (one that missed Kiro 1.1.70's `${n}` runtime endpoint, say) is so brought up
    /// to date the next time it is applied, instead of keeping the old rules for good.
    fn rerendered(&self, content: &str, gateway_url: &str, recipe: &PatchRecipe) -> Option<String> {
        let state = self.read_state().ok()?;
        let backup = fs::read(self.backup_path()).ok()?;
        if backup.len() as u64 != state.original_len || content_hash(&backup) != state.original_hash
        {
            return None;
        }
        let original = String::from_utf8(backup).ok()?;
        let owner = state.owner.or_else(current_owner);
        let fresh = render_patch_for(&original, gateway_url, recipe, owner.as_deref()).ok()?;
        (fresh != content).then_some(fresh)
    }

    /// What an already-patched bundle should become: rendered afresh from its original when
    /// that is possible, else the incremental repairs; equal to `content` when current.
    fn updated_patch(&self, content: &str, gateway_url: &str, recipe: &PatchRecipe) -> String {
        self.rerendered(content, gateway_url, recipe)
            .unwrap_or_else(|| repair_credit_display(&repair_proxy_tls(content)))
    }

    fn read_state(&self) -> Result<PatchState, PatchError> {
        serde_json::from_slice(
            &fs::read(self.state_path()).map_err(|_| PatchError::ExtensionChanged)?,
        )
        .map_err(|_| PatchError::ExtensionChanged)
    }
    fn write_state(&self, state: &PatchState) -> Result<(), PatchError> {
        atomic_write_file(&self.state_path(), &serde_json::to_vec(state).unwrap())
    }
    pub fn verify_patched_content(&self) -> Result<(), PatchError> {
        let state = self.read_state()?;
        let actual = fs::read(&self.extension_path).map_err(|_| PatchError::ExtensionChanged)?;
        let actual_hash = content_hash(&actual);
        if state.patched_hash != actual_hash
            && state.previous_patched_hash.as_ref() != Some(&actual_hash)
        {
            return Err(PatchError::ExtensionChanged);
        }
        self.restore_material()?;
        Ok(())
    }
    /// Preflight before any settings or extension rollback. Unknown legacy backups
    /// deliberately require manual recovery; do not bless them with a new digest.
    pub fn verify_restore(&self) -> Result<(), PatchError> {
        self.restore_material().map(|_| ())
    }
    fn restore_material(&self) -> Result<Option<Vec<u8>>, PatchError> {
        if !self.state_path().exists() && !self.backup_path().exists() {
            return if self.status() == PatchStatus::Patched {
                Err(PatchError::ExtensionChanged)
            } else {
                Ok(None)
            };
        }
        let state = self.read_state()?;
        let current = fs::read(&self.extension_path).map_err(|_| PatchError::ExtensionChanged)?;
        let original_matches = |bytes: &[u8]| {
            bytes.len() as u64 == state.original_len && content_hash(bytes) == state.original_hash
        };
        // A live file that is the original needs nothing rolled back, whatever became of
        // the backup: a crash while it was being written leaves it truncated, and an
        // antivirus scan can lock it. Only a patched live file needs the backup to prove
        // itself, and it is never replaced by anything that does not.
        let backup = if original_matches(&current) {
            current.clone()
        } else {
            match fs::read(self.backup_path()) {
                Ok(bytes) if original_matches(&bytes) => bytes,
                _ => return Err(PatchError::ExtensionChanged),
            }
        };
        let current_hash = content_hash(&current);
        if !original_matches(&current)
            && current_hash != state.patched_hash
            && state.previous_patched_hash.as_ref() != Some(&current_hash)
        {
            return Err(PatchError::ExtensionChanged);
        }
        Ok(Some(backup))
    }

    /// Restore the original unpatched `extension.js` from backup.
    pub fn restore(&self) -> Result<bool, PatchError> {
        match crate::runtime::detect_kiro_process_state() {
            crate::runtime::ProcessState::Running => return Err(PatchError::KiroRunning),
            crate::runtime::ProcessState::Unknown => {
                return Err(PatchError::ProcessStateUnknown(
                    "Cannot verify whether Kiro is running; modification forbidden".to_string(),
                ))
            }
            crate::runtime::ProcessState::Stopped => {}
        }

        self.restore_validated()
    }

    fn restore_validated(&self) -> Result<bool, PatchError> {
        let Some(original) = self.restore_material()? else {
            return Ok(false);
        };
        self.atomic_write_bytes(&original)?;
        // Removal order is retryable: state authenticates already-restored bytes
        // even if the process dies after deleting the backup.
        match fs::remove_file(self.backup_path()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(PatchError::Io(self.backup_path(), e.to_string())),
        }
        fs::remove_file(self.state_path())
            .map_err(|e| PatchError::Io(self.state_path(), e.to_string()))?;
        Ok(true)
    }

    fn atomic_write(&self, content: &str) -> Result<(), PatchError> {
        self.atomic_write_bytes(content.as_bytes())
    }

    fn atomic_write_bytes(&self, content: &[u8]) -> Result<(), PatchError> {
        atomic_write_file(&self.extension_path, content)
    }
}

/// Remove what earlier writes to `path` left behind when they died between writing their
/// temporary sibling and renaming it: `<name>.tmp.<pid>.<nanos>`, 13 MB each for Kiro's
/// bundle, inside Kiro's own installation. Writes run one at a time under the operation
/// lock, so any such sibling found before a write is a leftover. Best effort.
pub(crate) fn remove_stale_temps(path: &Path) {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
        return;
    };
    let prefix = format!("{name}.tmp.");
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(rest) = file_name
            .to_str()
            .and_then(|n| n.strip_prefix(prefix.as_str()))
        else {
            continue;
        };
        let leftover = rest.split_once('.').is_some_and(|(pid, nanos)| {
            !pid.is_empty()
                && !nanos.is_empty()
                && pid.bytes().all(|b| b.is_ascii_digit())
                && nanos.bytes().all(|b| b.is_ascii_digit())
        });
        if leftover && entry.file_type().is_ok_and(|kind| kind.is_file()) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn atomic_write_file(path: &Path, content: &[u8]) -> Result<(), PatchError> {
    remove_stale_temps(path);
    let temp_file = path.with_file_name(format!(
        "{}.tmp.{}.{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("extension.js"),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));

    {
        let mut file = File::create(&temp_file)
            .map_err(|e| PatchError::Io(temp_file.clone(), e.to_string()))?;
        file.write_all(content)
            .map_err(|e| PatchError::Io(temp_file.clone(), e.to_string()))?;
        file.sync_all()
            .map_err(|e| PatchError::Io(temp_file.clone(), e.to_string()))?;
    }

    if let Err(e) = atomic_replace(&temp_file, path) {
        let _ = fs::remove_file(&temp_file);
        return Err(PatchError::Io(path.to_path_buf(), e.to_string()));
    }

    Ok(())
}
fn content_hash(content: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, content)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn validate_gateway_url(url: &str) -> Result<String, PatchError> {
    let parsed =
        reqwest::Url::parse(url).map_err(|_| PatchError::InvalidUrl("Malformed URL".into()))?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.host_str().is_none()
        || !(parsed.scheme() == "https"
            || (parsed.scheme() == "http"
                && matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))))
        || url
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '\\' | '"' | '\'' | '`'))
    {
        return Err(PatchError::InvalidUrl(
            "Use HTTPS (HTTP only for loopback), without credentials, query or fragment".into(),
        ));
    }
    Ok(parsed.as_str().trim_end_matches('/').to_string())
}

fn render_patch(content: &str, gateway: &str, recipe: &PatchRecipe) -> Result<String, PatchError> {
    render_patch_for(content, gateway, recipe, current_owner().as_deref())
}

/// The patched bundle, redirecting only `owner`, the user taking over, as
/// [`current_owner`] names them. An installation can be shared by every user of the
/// computer (`/Applications/Kiro.app`, say) while a takeover is one user's: anyone
/// else's Kiro keeps its official runtime endpoint, and their own token with it. With
/// no owner known, the redirect applies to everyone, as it always did.
fn render_patch_for(
    content: &str,
    gateway: &str,
    recipe: &PatchRecipe,
    owner: Option<&str>,
) -> Result<String, PatchError> {
    let gateway = validate_gateway_url(gateway)?;
    if !recipe.marker.starts_with("/* @patched-kiro-byok ")
        || !recipe.marker.ends_with(" */")
        || recipe.marker[..recipe.marker.len() - 3].contains("*/")
        || recipe.marker.contains(['\n', '\r'])
    {
        return Err(PatchError::InvalidJavaScript("Invalid marker".into()));
    }
    let replacement = if recipe.replacement == PatchRecipe::default().replacement {
        format!(
            // A user's own AWS_ENDPOINT_URL (LocalStack, say) must not redirect Kiro's
            // gateway traffic, bearer token included.
            "(process.env.KIRO_GATEWAY_URL||{})",
            serde_json::to_string(&gateway).unwrap()
        )
    } else {
        let url = validate_gateway_url(&recipe.replacement.replace("{}", &gateway))?;
        serde_json::to_string(&url).unwrap()
    };
    // Whether the bundle runs as the owner: their home directory, read and compared the
    // way `current_owner` reads it.
    let is_owner = owner.map(|owner| {
        format!(
            "String(process.env.{}||\"\").toLowerCase()==={}",
            if cfg!(windows) { "USERPROFILE" } else { "HOME" },
            serde_json::to_string(owner).unwrap()
        )
    });
    // Match the whole literal, never its contents. This includes the runtime
    // template literal whose ${t} must disappear along with its backticks: every
    // occurrence of the needle has to be one, or the patch would miss a use.
    let mut body = String::with_capacity(content.len());
    let mut copied = 0;
    let mut replaced = 0;
    for found in needle_occurrences(content, &recipe.needle) {
        let quote = content[..found.start].chars().next_back();
        let whole =
            quote.filter(|q| matches!(q, '"' | '\'' | '`') && content[found.end..].starts_with(*q));
        let Some(quote) = whole else {
            if found.exact {
                return Err(PatchError::NeedleNotFound);
            }
            // Another variable's form outside a whole literal was never patched before;
            // left as it is, it cannot refuse a bundle earlier versions accepted.
            continue;
        };
        let (start, end) = (found.start - quote.len_utf8(), found.end + quote.len_utf8());
        // Two literals cannot share a quote; text that looks so is not JavaScript we know.
        if start < copied {
            return Err(PatchError::NeedleNotFound);
        }
        body.push_str(&content[copied..start]);
        match &is_owner {
            // Anyone else keeps the literal as Kiro wrote it.
            Some(is_owner) => body.push_str(&format!(
                "({is_owner}?{replacement}:{})",
                &content[start..end]
            )),
            None => body.push_str(&replacement),
        }
        copied = end;
        replaced += 1;
    }
    if replaced == 0 {
        return Err(PatchError::NeedleNotFound);
    }
    body.push_str(&content[copied..]);
    Ok(format!(
        "{}\n{}",
        recipe.marker,
        repair_credit_display(&repair_proxy_tls(&body))
    ))
}

/// One place the needle occurs, as a byte range of the bundle.
struct NeedleOccurrence {
    start: usize,
    end: usize,
    /// Written exactly as the needle, `${t}` and all.
    exact: bool,
}

/// Where `needle` occurs in `content`. Its `${t}` stands for any identifier: Kiro names
/// the region variable differently from one use to the next, and 1.1.70 builds its
/// activity publisher's endpoint as `https://runtime.${n}.kiro.dev`.
fn needle_occurrences(content: &str, needle: &str) -> Vec<NeedleOccurrence> {
    let Some((head, tail)) = needle.split_once("${t}") else {
        return content
            .match_indices(needle)
            .map(|(start, found)| NeedleOccurrence {
                start,
                end: start + found.len(),
                exact: true,
            })
            .collect();
    };
    let (head, tail) = (format!("{head}${{"), format!("}}{tail}"));
    content
        .match_indices(&head)
        .filter_map(|(start, _)| {
            let rest = &content[start + head.len()..];
            let name = rest
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$'))
                .count();
            (name > 0 && !rest.as_bytes()[0].is_ascii_digit() && rest[name..].starts_with(&tail))
                .then(|| NeedleOccurrence {
                    start,
                    end: start + head.len() + name + tail.len(),
                    exact: &rest[..name] == "t",
                })
        })
        .collect()
}

// Only the runtime belonging to this extension may validate a real installation.
fn javascript_command(extension_path: &Path) -> Result<Command, PatchError> {
    let extension = fs::canonicalize(extension_path)
        .map_err(|e| PatchError::InvalidJavaScript(e.to_string()))?;
    let suffix = Path::new("app/extensions/kiro.kiro-agent/dist/extension.js");
    if !extension.ends_with(suffix) {
        // Standalone fixture bundles do not have an Electron installation.
        return Ok(Command::new("node"));
    }
    let invalid = || {
        PatchError::InvalidJavaScript(
            "Invalid Kiro installation or unavailable RunAsNode runtime".into(),
        )
    };
    let app = extension.ancestors().nth(4).ok_or_else(invalid)?;
    let resources = app.parent().ok_or_else(invalid)?;
    if !resources
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("resources"))
    {
        return Err(invalid());
    }
    let root = resources.parent().ok_or_else(invalid)?;
    let product: serde_json::Value =
        serde_json::from_slice(&fs::read(app.join("product.json")).map_err(|_| invalid())?)
            .map_err(|_| invalid())?;
    if product["applicationName"].as_str() != Some("kiro") {
        return Err(invalid());
    }
    let executable = if cfg!(windows) {
        root.join("Kiro.exe")
    } else if cfg!(target_os = "macos") {
        crate::detect::resolve_macos_executable(root).map_err(|_| invalid())?
    } else {
        root.join("kiro")
    };
    let executable = fs::canonicalize(executable).map_err(|_| invalid())?;
    let expected_parent = if cfg!(target_os = "macos") {
        root.join("MacOS")
    } else {
        root.to_path_buf()
    };
    if !executable.is_file() || executable.parent() != Some(expected_parent.as_path()) {
        return Err(invalid());
    }
    let fuse_binary = if cfg!(target_os = "macos") {
        macos_fuse_binary(root)?
    } else {
        executable.clone()
    };
    verify_run_as_node(&fuse_binary)?;
    let mut command = Command::new(executable);
    command.env("ELECTRON_RUN_AS_NODE", "1");
    Ok(command)
}

// macOS keeps the fuse wire in the framework, not the MacOS/Kiro launcher stub.
fn macos_fuse_binary(contents: &Path) -> Result<PathBuf, PatchError> {
    let invalid = || PatchError::InvalidJavaScript("Invalid Kiro Electron framework path".into());
    let contents = fs::canonicalize(contents).map_err(|_| invalid())?;
    let binary = fs::canonicalize(
        contents.join("Frameworks/Electron Framework.framework/Versions/A/Electron Framework"),
    )
    .map_err(|_| invalid())?;
    if !binary.is_file() || !binary.starts_with(&contents) {
        return Err(invalid());
    }
    Ok(binary)
}

fn verify_run_as_node(executable: &Path) -> Result<(), PatchError> {
    let invalid = || PatchError::InvalidJavaScript("Unavailable Electron RunAsNode fuse".into());
    // A disabled Electron fuse ignores ELECTRON_RUN_AS_NODE and could launch the IDE.
    // Read the fuse wire without executing the binary; fail closed for unknown versions.
    let mut file = File::open(executable).map_err(|_| invalid())?;
    let sentinel = b"dL7pKGdnNz796PbbjQWNKmHXBZaB9tsX";
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        let count = file.read(&mut chunk).map_err(|_| invalid())?;
        if count == 0 {
            return Err(invalid());
        }
        buffer.extend_from_slice(&chunk[..count]);
        if let Some(wire) = buffer
            .windows(sentinel.len() + 3)
            .find(|wire| wire.starts_with(sentinel))
        {
            if wire[sentinel.len()] != 1
                || wire[sentinel.len() + 1] == 0
                || wire[sentinel.len() + 2] != b'1'
            {
                return Err(invalid());
            }
            break;
        }
        let keep = buffer.len().saturating_sub(sentinel.len() + 2);
        buffer.drain(..keep);
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// How many bundles this thread has had Kiro's runtime check.
    pub(crate) static JAVASCRIPT_CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn validate_javascript(extension_path: &Path, content: &str) -> Result<(), PatchError> {
    #[cfg(test)]
    JAVASCRIPT_CHECKS.with(|checks| checks.set(checks.get() + 1));
    check_javascript(javascript_command(extension_path)?, content)
}

fn check_javascript(mut command: Command, content: &str) -> Result<(), PatchError> {
    command
        .args(["--check", "--input-type=commonjs"])
        .env_remove("NODE_OPTIONS")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .spawn()
        .map_err(|e| PatchError::InvalidJavaScript(e.to_string()))?;
    let write_result = child.stdin.take().unwrap().write_all(content.as_bytes());
    let output = child
        .wait_with_output()
        .map_err(|e| PatchError::InvalidJavaScript(e.to_string()))?;
    if write_result.is_err() || !output.status.success() {
        // Never include a bundle or embedded credentials from parser stderr.
        return Err(PatchError::InvalidJavaScript(
            "Bundle did not pass node --check".into(),
        ));
    }
    Ok(())
}

fn atomic_replace(temp: &Path, target: &Path) -> std::io::Result<()> {
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

/// Kiro bundles https-proxy-agent and socks-proxy-agent with an IP tunnel identity bug:
/// removing host while SNI is unset makes Node verify "localhost" instead.
/// Preserve host for normal certificate verification; do not override TLS checks.
/// Both open the tunnelled connection as `X.connect({...omit(normalise(r),"host","path",
/// "port"),socket:s})`; the helpers' minified names change with every Kiro build.
fn repair_proxy_tls(content: &str) -> String {
    const OMITTED: &str = ",\"host\",\"path\",\"port\"),socket:";
    let content = content
        .replace(
            "M5o(N5o(r),\"host\",\"path\",\"port\")",
            "M5o(N5o(r),\"path\",\"port\")",
        )
        .replace(
            "qyl($yl(r),\"host\",\"path\",\"port\")",
            "qyl($yl(r),\"path\",\"port\")",
        );
    let mut repaired = String::with_capacity(content.len());
    let mut copied = 0;
    for (at, _) in content.match_indices(OMITTED) {
        if tunnel_options_before(&content[..at]) {
            repaired.push_str(&content[copied..at]);
            repaired.push_str(",\"path\",\"port\"),socket:");
            copied = at + OMITTED.len();
        }
    }
    repaired.push_str(&content[copied..]);
    repaired
}

/// Whether `before` ends in `.connect({...omit(normalise(r)`, whatever the names.
fn tunnel_options_before(before: &str) -> bool {
    /// `text` without the identifier it ends in; None when it ends in none.
    fn identifier(text: &str) -> Option<&str> {
        let name = text
            .bytes()
            .rev()
            .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$'))
            .count();
        (name > 0).then(|| &text[..text.len() - name])
    }
    before
        .strip_suffix(')')
        .and_then(identifier)
        .and_then(|text| text.strip_suffix('('))
        .and_then(identifier)
        .and_then(|text| text.strip_suffix('('))
        .and_then(identifier)
        .is_some_and(|text| text.ends_with(".connect({..."))
}

// Kiro 1.1.14's getItemContent interpolates full-precision usage into the
// status bar. Match only its display return expression: never round the shared
// usage model, percentages, monetary charges, or the gateway/ledger payload.
// Optional compatibility repair: an unknown formatter is left untouched, not an
// injection failure. Do not require exactly one match or guess at other layouts.
fn repair_credit_display(content: &str) -> String {
    content.replace(
        r#"return a&&a.currentUsage<a.usageLimit?`${d} Bonus ${a.currentUsage} / ${a.usageLimit} (${a.daysRemaining} days left)`:n>0||l>0?`${d} Overage ${n} (${r.symbol}${l.toFixed(2)})`:`${d} ${s} / ${u}`"#,
        // Intl defaults to halfExpand (half up for nonnegative credits), avoiding
        // toFixed's binary tie surprises such as 1.15 -> 1.1.
        r#"const creditDisplay=new Intl.NumberFormat("en-US",{minimumFractionDigits:1,maximumFractionDigits:1,useGrouping:false});return a&&a.currentUsage<a.usageLimit?`${d} Bonus ${creditDisplay.format(a.currentUsage)} / ${creditDisplay.format(a.usageLimit)} (${a.daysRemaining} days left)`:n>0||l>0?`${d} Overage ${creditDisplay.format(n)} (${r.symbol}${l.toFixed(2)})`:`${d} ${creditDisplay.format(s)} / ${creditDisplay.format(u)}`"#,
    )
}

#[cfg(test)]
#[path = "../tests/patch/credit_display.rs"]
mod credit_display_tests;

#[cfg(test)]
mod proxy_tls_tests {
    use super::*;

    #[test]
    fn proxy_tls_repair_preserves_ip_identity_and_is_idempotent() {
        let original = "O5o.connect({...M5o(N5o(r),\"host\",\"path\",\"port\"),socket:s})";
        let expected = "O5o.connect({...M5o(N5o(r),\"path\",\"port\"),socket:s})";
        assert_eq!(repair_proxy_tls(original), expected);
        assert_eq!(repair_proxy_tls(expected), expected);
        let bundle = format!("const endpoint=`{RUNTIME_ENDPOINT_NEEDLE}`;{original}");
        let rendered =
            render_patch(&bundle, "https://160.202.47.98", &PatchRecipe::default()).unwrap();
        assert!(rendered.contains(expected));
        assert!(!rendered.contains(original));
    }

    /// Kiro 1.1.70: new names in both proxy agents, the same shape.
    #[test]
    fn proxy_tls_repair_follows_renamed_helpers() {
        for (original, expected) in [
            (
                "r.secureEndpoint?(wLe(\"Upgrading\"),CVo.connect({...TVo(IVo(r),\"host\",\"path\",\"port\"),socket:s})):s",
                "r.secureEndpoint?(wLe(\"Upgrading\"),CVo.connect({...TVo(IVo(r),\"path\",\"port\"),socket:s})):s",
            ),
            (
                "let m=RPl.connect({...MPl(OPl(r),\"host\",\"path\",\"port\"),socket:h});",
                "let m=RPl.connect({...MPl(OPl(r),\"path\",\"port\"),socket:h});",
            ),
            (
                "a.connect({...$b($c(e),\"host\",\"path\",\"port\"),socket:x})",
                "a.connect({...$b($c(e),\"path\",\"port\"),socket:x})",
            ),
        ] {
            assert_eq!(repair_proxy_tls(original), expected);
            assert_eq!(repair_proxy_tls(expected), expected);
        }
        // The same keys anywhere else are not a tunnel's TLS options.
        for untouched in [
            "pick(opts,\"host\",\"path\",\"port\"),socket:s",
            "x={...TVo(IVo(r),\"host\",\"path\",\"port\"),socket:s}",
            "CVo.connect({...TVo(IVo(r,1),\"host\",\"path\",\"port\"),socket:s})",
        ] {
            assert_eq!(repair_proxy_tls(untouched), untouched);
        }
    }
}

#[cfg(test)]
mod rerender_tests {
    use super::*;

    const GATEWAY: &str = "https://gw.test";
    const ORIGINAL: &str =
        "module.exports=[(t)=>`https://runtime.${t}.kiro.dev`,(n)=>`https://runtime.${n}.kiro.dev`];";

    /// A bundle as an earlier client patched it: the `${t}` endpoint redirected, the `${n}`
    /// one Kiro 1.1.70 added left on the official runtime.
    fn older_patch() -> String {
        format!(
            "{PATCH_MARKER_V1}\n{}",
            ORIGINAL.replacen(
                "`https://runtime.${t}.kiro.dev`",
                "(process.env.KIRO_GATEWAY_URL||\"https://gw.test\")",
                1
            )
        )
    }

    fn installed(name: &str, live: &str, backup: &[u8]) -> (PathBuf, ExtensionPatcher) {
        let dir =
            std::env::temp_dir().join(format!("patch-rerender-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let patcher = ExtensionPatcher::new(dir.join("extension.js"));
        fs::write(patcher.path(), live).unwrap();
        fs::write(patcher.backup_path(), backup).unwrap();
        patcher
            .write_state(&PatchState {
                original_len: ORIGINAL.len() as u64,
                original_hash: content_hash(ORIGINAL.as_bytes()),
                patched_hash: content_hash(live.as_bytes()),
                previous_patched_hash: None,
                owner: None,
            })
            .unwrap();
        (dir, patcher)
    }

    #[test]
    fn an_older_patch_is_rerendered_from_its_original() {
        let older = older_patch();
        let (dir, patcher) = installed("current", &older, ORIGINAL.as_bytes());
        let fresh = render_patch(ORIGINAL, GATEWAY, &PatchRecipe::default()).unwrap();
        assert_eq!(
            patcher.updated_patch(&older, GATEWAY, &PatchRecipe::default()),
            fresh
        );
        // The takeover's own check passes it, naming the bundle read and what it becomes.
        let prepared = patcher.prepare(GATEWAY).unwrap();
        assert_eq!(prepared.source, content_hash(older.as_bytes()));
        assert_eq!(prepared.patched, content_hash(fresh.as_bytes()));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_current_patch_is_left_as_it_is() {
        let fresh = render_patch(ORIGINAL, GATEWAY, &PatchRecipe::default()).unwrap();
        let (dir, patcher) = installed("fresh", &fresh, ORIGINAL.as_bytes());
        assert_eq!(
            patcher.updated_patch(&fresh, GATEWAY, &PatchRecipe::default()),
            fresh
        );
        let prepared = patcher.prepare(GATEWAY).unwrap();
        assert_eq!(prepared.source, prepared.patched);
        fs::remove_dir_all(dir).unwrap();
    }

    /// A backup that no longer matches the recorded original cannot vouch for a re-render:
    /// only the incremental repairs apply, as before.
    #[test]
    fn an_unverifiable_backup_falls_back_to_the_repairs() {
        let older = older_patch();
        let (dir, patcher) = installed("tampered", &older, b"not the original");
        assert_eq!(
            patcher.updated_patch(&older, GATEWAY, &PatchRecipe::default()),
            older
        );
        fs::remove_dir_all(dir).unwrap();
    }

    /// Text where two literals would share a quote is refused, not a panic.
    #[test]
    fn literals_sharing_a_quote_are_refused() {
        let source = "a=`https://runtime.${t}.kiro.dev`https://runtime.${t}.kiro.dev`;";
        assert_eq!(
            render_patch(source, GATEWAY, &PatchRecipe::default()),
            Err(PatchError::NeedleNotFound)
        );
    }
}

#[cfg(test)]
mod runtime_endpoint_tests {
    use super::*;

    /// Kiro 1.1.70 builds its activity publisher's endpoint from a variable named `n`.
    #[test]
    fn every_region_variable_is_redirected() {
        let source = "a=c(t=>`https://runtime.${t}.kiro.dev`,\"ji\");\
            let n=this.region,s=this.endpoint||(n?`https://runtime.${n}.kiro.dev`:void 0);\
            b=`https://runtime.${$r_1}.kiro.dev`;";
        let rendered =
            render_patch_for(source, "https://gw.test", &PatchRecipe::default(), None).unwrap();
        assert!(!rendered.contains("https://runtime."), "{rendered}");
        assert_eq!(rendered.matches("\"https://gw.test\"").count(), 3);
        assert!(rendered.contains(
            "s=this.endpoint||(n?(process.env.KIRO_GATEWAY_URL||\"https://gw.test\"):void 0)"
        ));
    }

    /// Anyone but the owner keeps each literal as Kiro wrote it, its own variable included.
    #[test]
    fn the_fallback_keeps_each_literal_verbatim() {
        let source =
            "s=n?`https://runtime.${n}.kiro.dev`:void 0;e=`https://runtime.${t}.kiro.dev`;";
        let rendered = render_patch_for(
            source,
            "https://gw.test",
            &PatchRecipe::default(),
            Some("/home/taker"),
        )
        .unwrap();
        assert!(
            rendered.contains(":`https://runtime.${n}.kiro.dev`)"),
            "{rendered}"
        );
        assert!(
            rendered.contains(":`https://runtime.${t}.kiro.dev`)"),
            "{rendered}"
        );
    }

    /// Forms earlier versions never patched still do not refuse a bundle: another
    /// variable outside a whole literal, or no identifier at all.
    #[test]
    fn unfamiliar_forms_neither_match_nor_refuse() {
        let source = "a=`https://runtime.${t}.kiro.dev`;\
            b=`https://runtime.${n}.kiro.dev/x`;\
            c=`https://runtime.${r.id}.kiro.dev`;\
            d=`https://runtime.${1}.kiro.dev`;";
        let rendered = render_patch(source, "https://gw.test", &PatchRecipe::default()).unwrap();
        assert!(rendered.contains("b=`https://runtime.${n}.kiro.dev/x`"));
        assert!(rendered.contains("c=`https://runtime.${r.id}.kiro.dev`"));
        assert!(rendered.contains("d=`https://runtime.${1}.kiro.dev`"));
        assert!(!rendered.contains("a=`"));
    }

    /// A bundle from an installed Kiro, rendered and parsed as the takeover would:
    /// KIRO_BUNDLE=<copy of extension.js> cargo test -p patch-engine installed -- --ignored
    #[test]
    #[ignore]
    fn installed_bundle_renders_and_parses() {
        let path = std::env::var("KIRO_BUNDLE").expect("set KIRO_BUNDLE");
        let content = fs::read_to_string(path).unwrap();
        let rendered = render_patch_for(
            &content,
            "https://kiro.rent",
            &PatchRecipe::default(),
            Some("c:\\users\\taker"),
        )
        .unwrap();
        // Every runtime literal is owner-gated; no tunnel drops its host any more.
        let literals = needle_occurrences(&content, RUNTIME_ENDPOINT_NEEDLE).len();
        assert_eq!(
            rendered.matches("(process.env.KIRO_GATEWAY_URL||").count(),
            literals
        );
        assert!(!rendered.contains(",\"host\",\"path\",\"port\"),socket:"));
        check_javascript(Command::new("node"), &rendered).unwrap();
        eprintln!("{literals} runtime literals redirected");
    }
}

#[cfg(test)]
mod shared_install_tests {
    use super::*;

    /// An installation every user of the computer shares carries one user's takeover.
    /// Only that user's Kiro is redirected; anyone else's keeps Kiro's official runtime
    /// endpoint, so their own token never reaches the gateway.
    #[test]
    fn only_the_user_who_took_over_is_redirected() {
        let dir = std::env::temp_dir().join(format!("shared-install-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let bundle = dir.join("bundle.js");
        let (home, owner, other) = if cfg!(windows) {
            ("USERPROFILE", "C:\\Users\\Taker", "C:\\Users\\Someone")
        } else {
            ("HOME", "/Users/taker", "/Users/someone")
        };
        let source = format!("module.exports = (t) => `{RUNTIME_ENDPOINT_NEEDLE}`;");
        let rendered = render_patch_for(
            &source,
            "https://gw.shared.test",
            &PatchRecipe::default(),
            Some(&owner.to_lowercase()),
        )
        .unwrap();
        fs::write(&bundle, rendered).unwrap();
        let endpoint = |user: &str, launched_with: Option<&str>| {
            let mut node = Command::new("node");
            node.args([
                "-e",
                "process.stdout.write(require(process.argv[1])('us-east-1'))",
            ])
            .arg(&bundle)
            .env(home, user)
            .env_remove("KIRO_GATEWAY_URL");
            if let Some(gateway) = launched_with {
                node.env("KIRO_GATEWAY_URL", gateway);
            }
            let output = node.output().unwrap();
            assert!(output.status.success(), "{output:?}");
            String::from_utf8(output.stdout).unwrap()
        };
        let official = "https://runtime.us-east-1.kiro.dev";
        assert_eq!(endpoint(owner, None), "https://gw.shared.test");
        assert_eq!(
            endpoint(&owner.to_uppercase(), None),
            "https://gw.shared.test"
        );
        assert_eq!(
            endpoint(owner, Some("https://gw.launch.test")),
            "https://gw.launch.test"
        );
        assert_eq!(endpoint(other, None), official);
        assert_eq!(endpoint(other, Some("https://gw.launch.test")), official);
        assert_eq!(endpoint("", None), official);
        fs::remove_dir_all(dir).unwrap();
    }

    /// Every use of the endpoint is a whole literal, or the patch is refused as before.
    #[test]
    fn a_needle_outside_a_whole_literal_still_refuses_the_patch() {
        let source = format!("a = `{RUNTIME_ENDPOINT_NEEDLE}`; b = `{RUNTIME_ENDPOINT_NEEDLE}/x`;");
        assert_eq!(
            render_patch_for(
                &source,
                "https://gw.test",
                &PatchRecipe::default(),
                Some("/home/taker")
            ),
            Err(PatchError::NeedleNotFound)
        );
    }
}

#[cfg(test)]
#[path = "../tests/patch/javascript_validation.rs"]
mod javascript_validation_tests;

#[cfg(test)]
#[path = "../tests/patch/recovery.rs"]
mod recovery_tests;
