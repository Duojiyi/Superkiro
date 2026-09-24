//! Kiro `settings.json` safe merge, takeover, and rollback (Spec §2.5, §9, §14.5, P0-3).
//!
//! Features:
//! - Safely injects only BYOK-managed configuration keys without touching user preferences.
//! - Disables Tab Autocomplete (`kiroAgent.enableTabAutocomplete: false`) to prevent credit bleed.
//! - Freezes auto-updates (`update.mode: "none"`) to prevent silent patch breakage.
//! - Provides 100% clean rollback with zero leftover configuration residue.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Could not determine user configuration directory")]
    ConfigDirNotFound,
}

/// Keys managed and modified by Kiro BYOK in `settings.json`.
pub const MANAGED_KEYS: &[&str] = &[
    "kiroAuthConfig",
    "codewhisperer.config",
    "kiroAgent.enableTabAutocomplete",
    "update.mode",
    "telemetry.telemetryLevel",
    "http.noProxy",
];

/// Resolve the default system path to Kiro's `User/settings.json`.
pub fn default_settings_path() -> Result<PathBuf, SettingsError> {
    if cfg!(target_os = "windows") {
        let appdata = env::var("APPDATA").map_err(|_| SettingsError::ConfigDirNotFound)?;
        Ok(PathBuf::from(appdata)
            .join("Kiro")
            .join("User")
            .join("settings.json"))
    } else if cfg!(target_os = "macos") {
        let home = env::var("HOME").map_err(|_| SettingsError::ConfigDirNotFound)?;
        Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("Kiro")
            .join("User")
            .join("settings.json"))
    } else {
        // Linux / Unix
        if let Ok(config_home) = env::var("XDG_CONFIG_HOME") {
            Ok(PathBuf::from(config_home)
                .join("Kiro")
                .join("User")
                .join("settings.json"))
        } else {
            let home = env::var("HOME").map_err(|_| SettingsError::ConfigDirNotFound)?;
            Ok(PathBuf::from(home)
                .join(".config")
                .join("Kiro")
                .join("User")
                .join("settings.json"))
        }
    }
}

/// Snapshot of prior settings before BYOK modification (for precision rollback).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PriorSettingsState {
    pub had_settings_file: bool,
    #[serde(default)]
    pub proxy_bypass_managed: bool,
    pub prior_values: Map<String, Value>,
    /// Original bytes, retained so a rollback can be byte-for-byte exact when
    /// the user did not change unrelated settings during takeover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_raw: Option<Vec<u8>>,
}

/// Manager for safe reading, merging, and rolling back Kiro's `settings.json`.
#[derive(Debug, Clone)]
pub struct SettingsManager {
    settings_path: PathBuf,
}

impl Default for SettingsManager {
    fn default() -> Self {
        let path = default_settings_path().unwrap_or_else(|_| PathBuf::from("settings.json"));
        Self {
            settings_path: path,
        }
    }
}

/// The only region takeover redirects. Kiro picks its endpoints by the region in the
/// token's profile ARN and falls back to the real service for any region without an
/// override, so a token for another region would send the gateway's bearer to real Kiro.
pub(crate) const REDIRECTED_REGION: &str = "us-east-1";

/// Whether a profile ARN (`arn:aws:codewhisperer:<region>:...`) is in the redirected region.
pub(crate) fn in_redirected_region(profile_arn: &str) -> bool {
    profile_arn.split(':').nth(3) == Some(REDIRECTED_REGION)
}

impl SettingsManager {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            settings_path: path.into(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.settings_path
    }

    /// Read raw settings JSON map from disk. Returns empty map if file does not exist.
    pub fn read_settings(&self) -> Result<Map<String, Value>, SettingsError> {
        if !self.settings_path.exists() {
            return Ok(Map::new());
        }

        parse_settings_bytes(&fs::read(&self.settings_path)?)
    }

    /// Capture only the keys owned by the BYOK integration without changing
    /// the user's settings file.  SnapshotManager uses this before applying
    /// any mutation so a crash cannot leave an unrecorded half-takeover.
    pub fn capture_prior_state(&self) -> Result<PriorSettingsState, SettingsError> {
        let had_settings_file = self.settings_path.exists();
        let prior_raw = if had_settings_file {
            Some(fs::read(&self.settings_path)?)
        } else {
            None
        };
        let map = self.read_settings()?;
        let mut prior_values = Map::new();
        for &key in MANAGED_KEYS {
            if let Some(existing) = map.get(key) {
                prior_values.insert(key.to_string(), existing.clone());
            }
        }
        Ok(PriorSettingsState {
            had_settings_file,
            proxy_bypass_managed: true,
            prior_values,
            prior_raw,
        })
    }

    /// Safely merge BYOK redirection and optimization keys into `settings.json`.
    ///
    /// Preserves all other user settings (theme, font, other extensions).
    /// Returns `PriorSettingsState` to enable 100% reversible rollback.
    pub fn merge_byok(&self, gateway_url: &str) -> Result<PriorSettingsState, SettingsError> {
        let region = REDIRECTED_REGION;
        let prior = self.capture_prior_state()?;
        let mut map = self.read_settings()?;

        let gw = gateway_url.trim_end_matches('/');

        // 1. kiroAuthConfig
        map.insert(
            "kiroAuthConfig".to_string(),
            json!({
                "portalUrl": gw,
                "endpoint": gw
            }),
        );

        // 2. codewhisperer.config (KRS + CPS + general endpoints)
        map.insert(
            "codewhisperer.config".to_string(),
            json!({
                "krsEndpoints": [{ "region": region, "endpoint": gw }],
                "cpsEndpoints": [{ "region": region, "endpoint": gw }],
                "endpoints": [{ "region": region, "endpoint": gw }]
            }),
        );

        // 3. Tab Autocomplete: disable to save user tokens (Spec §2.5)
        map.insert("kiroAgent.enableTabAutocomplete".to_string(), json!(false));

        // 4. Update mode: none (freeze auto-updates to prevent silent patch breakage)
        map.insert("update.mode".to_string(), json!("none"));

        // 5. Telemetry: off
        map.insert("telemetry.telemetryLevel".to_string(), json!("off"));

        // Kiro's core proxy agent drops IP identity on TLS tunnels. Bypass only
        // this gateway; keep the user's other proxy exceptions and TLS checks.
        let host = reqwest::Url::parse(gw)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned));
        if let Some(host) = host {
            let mut bypass = match map.get("http.noProxy") {
                Some(Value::Array(values)) => values.clone(),
                None => Vec::new(),
                _ => {
                    return Err(SettingsError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "http.noProxy must be an array",
                    )))
                }
            };
            if !bypass.iter().any(|value| value.as_str() == Some(&host)) {
                bypass.push(json!(host));
            }
            map.insert("http.noProxy".into(), Value::Array(bypass));
        }
        self.atomic_write(&map)?;

        Ok(PriorSettingsState { ..prior })
    }

    /// Revert BYOK settings back to original state using `PriorSettingsState`.
    ///
    /// If the settings file did not exist before BYOK, deletes the file.
    /// Otherwise restores prior values of managed keys or removes them if they didn't exist.
    pub fn revert(&self, prior: &PriorSettingsState) -> Result<(), SettingsError> {
        let mut map = self.read_settings()?;

        if !prior.had_settings_file {
            // Spec §9, T08: If the user subsequently added their own custom settings
            // while BYOK was active, do NOT delete the entire file. Remove only BYOK managed keys.
            for &key in MANAGED_KEYS {
                if key == "http.noProxy" && !prior.proxy_bypass_managed {
                    continue;
                }
                map.remove(key);
            }
            if map.is_empty() {
                if self.settings_path.exists() {
                    fs::remove_file(&self.settings_path)?;
                }
            } else {
                self.atomic_write(&map)?;
            }
            return Ok(());
        }

        // If no unrelated setting changed, restore the original bytes. This
        // preserves JSONC comments, whitespace, ordering, and line endings.
        // If the user changed/added an unrelated key, merge only managed keys
        // so that their live edit is retained.
        if let Some(raw) = prior.prior_raw.as_deref() {
            let original_map = parse_settings_bytes(raw)?;
            if non_managed_values(&map) == non_managed_values(&original_map)
                && (prior.proxy_bypass_managed
                    || map.get("http.noProxy") == original_map.get("http.noProxy"))
            {
                self.atomic_write_bytes(raw)?;
                return Ok(());
            }
        }

        for &key in MANAGED_KEYS {
            if key == "http.noProxy" && !prior.proxy_bypass_managed {
                continue;
            }
            if let Some(orig) = prior.prior_values.get(key) {
                map.insert(key.to_string(), orig.clone());
            } else {
                map.remove(key);
            }
        }

        self.atomic_write(&map)?;
        Ok(())
    }

    /// Check if BYOK redirection keys are currently active in `settings.json`.
    pub fn is_byok_active(&self, gateway_url: Option<&str>) -> bool {
        let map = match self.read_settings() {
            Ok(m) => m,
            Err(_) => return false,
        };

        let auto_comp = map
            .get("kiroAgent.enableTabAutocomplete")
            .and_then(|v| v.as_bool());
        if auto_comp != Some(false) {
            return false;
        }

        if let Some(expected_url) = gateway_url {
            let gw = expected_url.trim_end_matches('/');
            let auth_conf = map.get("kiroAuthConfig");
            let matches_auth = auth_conf
                .and_then(|v| v.get("portalUrl"))
                .and_then(|v| v.as_str())
                .map(|s| s.trim_end_matches('/') == gw)
                .unwrap_or(false);

            matches_auth
        } else {
            map.contains_key("kiroAuthConfig") && map.contains_key("codewhisperer.config")
        }
    }

    fn atomic_write(&self, map: &Map<String, Value>) -> Result<(), SettingsError> {
        let json_str = serde_json::to_string_pretty(map)?;
        self.atomic_write_bytes(json_str.as_bytes())
    }

    fn atomic_write_bytes(&self, bytes: &[u8]) -> Result<(), SettingsError> {
        if let Some(parent) = self.settings_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let temp_file = self.settings_path.with_file_name(format!(
            "{}.tmp.{}.{}",
            self.settings_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("settings.json"),
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));

        {
            let mut file = File::create(&temp_file)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }

        if let Err(e) = atomic_replace(&temp_file, &self.settings_path) {
            let _ = fs::remove_file(&temp_file);
            return Err(SettingsError::Io(e));
        }

        Ok(())
    }
}

fn parse_settings_bytes(raw: &[u8]) -> Result<Map<String, Value>, SettingsError> {
    let content = std::str::from_utf8(raw).map_err(|error| {
        serde_json::Error::io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    })?;
    if content.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(&strip_jsonc(content))? {
        Value::Object(map) => Ok(map),
        _ => Err(SettingsError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "settings.json must contain an object",
        ))),
    }
}

fn non_managed_values(map: &Map<String, Value>) -> Map<String, Value> {
    map.iter()
        .filter(|(key, _)| !MANAGED_KEYS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
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

fn strip_jsonc(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if c == '/' {
            match chars.peek() {
                Some('/') => {
                    for nc in chars.by_ref() {
                        if nc == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                }
                Some('*') => {
                    chars.next();
                    loop {
                        match chars.next() {
                            Some('*') if chars.peek() == Some(&'/') => {
                                chars.next();
                                break;
                            }
                            None => break,
                            _ => {}
                        }
                    }
                }
                _ => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    // Only remove commas outside strings, including whitespace/comment gaps.
    let mut result = String::with_capacity(out.len());
    let mut chars = out.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            result.push(c);
            if c == '\\' {
                if let Some(next) = chars.next() {
                    result.push(next);
                }
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            result.push(c);
        } else if c != ',' || !matches!(chars.clone().find(|c| !c.is_whitespace()), Some('}' | ']'))
        {
            result.push(c);
        }
    }
    result
}

#[cfg(test)]
mod shape_tests {
    use super::*;

    #[test]
    fn non_object_settings_are_never_overwritten_or_deleted() {
        let root = env::temp_dir().join(format!("settings-shape-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let manager = SettingsManager::at(root.join("settings.json"));
        for bytes in [
            b"[\"user data\"]".as_slice(),
            b"null",
            b"42",
            b"true",
            b"\"text\"",
        ] {
            fs::write(manager.path(), bytes).unwrap();
            assert!(manager.merge_byok("https://fixture.invalid").is_err());
            assert_eq!(fs::read(manager.path()).unwrap(), bytes);
            for had_settings_file in [false, true] {
                let prior = PriorSettingsState {
                    had_settings_file,
                    prior_raw: had_settings_file.then(|| b"{}".to_vec()),
                    ..Default::default()
                };
                assert!(manager.revert(&prior).is_err());
                assert_eq!(fs::read(manager.path()).unwrap(), bytes);
            }
        }
        fs::remove_file(manager.path()).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
