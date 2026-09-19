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
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use thiserror::Error;

/// Header marker placed at the beginning of patched `extension.js`.
pub const PATCH_MARKER_V1: &str = "/* @patched-kiro-byok v1 */";

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

    #[error("JavaScript validation failed (Node.js is required): {0}")]
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

    // 2. AWS SDK fallback endpoint
    envs.insert("AWS_ENDPOINT_URL".to_string(), gw.to_string());

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

    /// Determine current patch status.
    pub fn status(&self) -> PatchStatus {
        if !self.extension_path.exists() {
            return PatchStatus::NotFound;
        }

        let is_marked = match fs::read_to_string(&self.extension_path) {
            Ok(content) => content.starts_with("/* @patched-kiro-byok "),
            Err(_) => false,
        };

        if is_marked {
            PatchStatus::Patched
        } else if self.backup_path().exists() {
            PatchStatus::UpgradeDetected
        } else {
            PatchStatus::Official
        }
    }

    /// Dry run: verify whether patch can be cleanly applied without writing to disk.
    pub fn dry_run(&self) -> Result<bool, PatchError> {
        if !self.extension_path.exists() {
            return Err(PatchError::FileNotFound(self.extension_path.clone()));
        }

        let content = fs::read_to_string(&self.extension_path)
            .map_err(|e| PatchError::Io(self.extension_path.clone(), e.to_string()))?;

        if content.starts_with("/* @patched-kiro-byok ") {
            return Ok(true); // Already patched
        }

        if self.backup_path().exists() {
            return Err(PatchError::ExtensionChanged);
        }
        let rendered = render_patch(&content, "https://gateway.invalid", &PatchRecipe::default())?;
        validate_javascript(&rendered)?;
        Ok(true)
    }

    /// Apply the BYOK patch to `extension.js` using default recipe.
    pub fn apply(&self, gateway_url: &str) -> Result<(), PatchError> {
        self.apply_with_recipe(gateway_url, &PatchRecipe::default())
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

        if self.status() == PatchStatus::Patched {
            self.verify_patched_content()?;
            let repaired = repair_proxy_tls(&content);
            if repaired != content {
                validate_javascript(&repaired)?;
                self.atomic_write(&repaired)?;
                fs::write(self.state_path(), content_hash(repaired.as_bytes()))
                    .map_err(|e| PatchError::Io(self.state_path(), e.to_string()))?;
            }
            return Ok(());
        }
        if self.backup_path().exists() {
            return Err(PatchError::ExtensionChanged);
        }
        let full_patched = render_patch(&content, gateway_url, recipe)?;
        validate_javascript(&full_patched)?;

        // No backup or live mutation until both the URL and complete JS parse pass.
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
        fs::write(self.state_path(), content_hash(full_patched.as_bytes()))
            .map_err(|e| PatchError::Io(self.state_path(), e.to_string()))?;
        self.atomic_write(&full_patched)?;
        Ok(())
    }

    fn state_path(&self) -> PathBuf {
        self.extension_path.with_extension("js.kpatch-state")
    }

    pub fn verify_patched_content(&self) -> Result<(), PatchError> {
        let expected =
            fs::read_to_string(self.state_path()).map_err(|_| PatchError::ExtensionChanged)?;
        let actual = fs::read(&self.extension_path)
            .map_err(|e| PatchError::Io(self.extension_path.clone(), e.to_string()))?;
        if expected != content_hash(&actual) || !self.backup_path().is_file() {
            return Err(PatchError::ExtensionChanged);
        }
        Ok(())
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

        let backup_path = self.backup_path();
        if !backup_path.exists() {
            return Ok(false);
        }

        // An upgrade already restored an official bundle. Never write the old
        // version over it. Retain its backup as evidence for manual recovery.
        if self.status() == PatchStatus::UpgradeDetected {
            return Ok(false);
        }
        self.verify_patched_content()?;

        // Restore through the same atomic path used for patching.  A direct
        // copy can leave a truncated JavaScript bundle if the process dies.
        let original = fs::read(&backup_path)
            .map_err(|e| PatchError::Io(self.extension_path.clone(), e.to_string()))?;
        self.atomic_write_bytes(&original)?;

        // Remove backup
        fs::remove_file(&backup_path).map_err(|e| PatchError::Io(backup_path, e.to_string()))?;
        let _ = fs::remove_file(self.state_path());
        Ok(true)
    }

    fn atomic_write(&self, content: &str) -> Result<(), PatchError> {
        self.atomic_write_bytes(content.as_bytes())
    }

    fn atomic_write_bytes(&self, content: &[u8]) -> Result<(), PatchError> {
        let temp_file = self.extension_path.with_file_name(format!(
            "{}.tmp.{}.{}",
            self.extension_path
                .file_name()
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

        if let Err(e) = atomic_replace(&temp_file, &self.extension_path) {
            let _ = fs::remove_file(&temp_file);
            return Err(PatchError::Io(self.extension_path.clone(), e.to_string()));
        }

        Ok(())
    }
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
            "(process.env.KIRO_GATEWAY_URL||process.env.AWS_ENDPOINT_URL||{})",
            serde_json::to_string(&gateway).unwrap()
        )
    } else {
        let url = validate_gateway_url(&recipe.replacement.replace("{}", &gateway))?;
        serde_json::to_string(&url).unwrap()
    };
    // Match the whole literal, never its contents. This includes the runtime
    // template literal whose ${t} must disappear along with its backticks.
    let mut body = content.to_string();
    let mut replaced = 0;
    for quote in ['"', '\'', '`'] {
        let literal = format!("{quote}{}{quote}", recipe.needle);
        replaced += body.matches(&literal).count();
        body = body.replace(&literal, &replacement);
    }
    if replaced == 0 || body.contains(&recipe.needle) {
        return Err(PatchError::NeedleNotFound);
    }
    Ok(format!("{}\n{}", recipe.marker, repair_proxy_tls(&body)))
}

fn validate_javascript(content: &str) -> Result<(), PatchError> {
    let mut command = Command::new("node");
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

/// Kiro 1.1.14 bundles https-proxy-agent with an IP tunnel identity bug:
/// removing host while SNI is unset makes Node verify "localhost" instead.
/// Preserve host for normal certificate verification; do not override TLS checks.
fn repair_proxy_tls(content: &str) -> String {
    content
        .replace(
            "M5o(N5o(r),\"host\",\"path\",\"port\")",
            "M5o(N5o(r),\"path\",\"port\")",
        )
        .replace(
            "qyl($yl(r),\"host\",\"path\",\"port\")",
            "qyl($yl(r),\"path\",\"port\")",
        )
}

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
}
