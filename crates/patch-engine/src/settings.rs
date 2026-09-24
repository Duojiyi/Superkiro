//! Kiro `settings.json` safe merge, takeover, and rollback (Spec §2.5, §9, §14.5, P0-3).
//!
//! Features:
//! - Safely injects only BYOK-managed configuration keys without touching user preferences.
//! - Disables Tab Autocomplete (`kiroAgent.enableTabAutocomplete: false`) to prevent credit bleed.
//! - Freezes auto-updates (`update.mode: "none"`) to prevent silent patch breakage.
//! - Provides 100% clean rollback with zero leftover configuration residue.
//! - Edits the file in place: comments, key order, indentation, line endings and a
//!   byte-order mark survive both takeover and rollback.

use jsonc_parser::cst::{CstInputValue, CstObject, CstObjectProp, CstRootNode};
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
        let raw = self.read_raw()?;
        let text = SettingsText::parse(&raw)?;
        let mut map = parse_settings_bytes(&raw)?;

        let gw = gateway_url.trim_end_matches('/');
        let mut values = vec![
            ("kiroAuthConfig", json!({ "portalUrl": gw, "endpoint": gw })),
            // KRS + CPS + general endpoints
            (
                "codewhisperer.config",
                json!({
                    "krsEndpoints": [{ "region": region, "endpoint": gw }],
                    "cpsEndpoints": [{ "region": region, "endpoint": gw }],
                    "endpoints": [{ "region": region, "endpoint": gw }]
                }),
            ),
        ];
        values.extend(our_preferences());

        // Kiro's core proxy agent drops IP identity on TLS tunnels. Bypass only
        // this gateway; keep the user's other proxy exceptions and TLS checks.
        if let Some(host) = gateway_host(gw) {
            let mut bypass = match map.get("http.noProxy") {
                Some(Value::Array(values)) => values.clone(),
                None => Vec::new(),
                _ => return Err(invalid_data("http.noProxy must be an array")),
            };
            if !bypass.iter().any(|value| value.as_str() == Some(&host)) {
                bypass.push(json!(host));
            }
            values.push(("http.noProxy", Value::Array(bypass)));
        }

        for (key, value) in values {
            text.set(key, &value);
            map.insert(key.to_string(), value);
        }
        self.write_text(&text, &map)?;
        Ok(prior)
    }

    /// Revert BYOK settings back to original state using `PriorSettingsState`.
    ///
    /// `gateway_url` is the gateway the takeover pointed Kiro at. What each managed key
    /// returns to is decided by [`reverted_values`]. When the result is exactly the file
    /// as it was, its original bytes are restored; otherwise only the managed keys are
    /// edited in place, so anything the user changed meanwhile is kept, comments
    /// included. A file that did not exist before and would be left empty is deleted.
    pub fn revert(
        &self,
        prior: &PriorSettingsState,
        gateway_url: &str,
    ) -> Result<(), SettingsError> {
        let raw = self.read_raw()?;
        let text = SettingsText::parse(&raw)?;
        let mut map = parse_settings_bytes(&raw)?;
        let host = gateway_host(gateway_url.trim_end_matches('/'));
        for (key, value) in reverted_values(prior, &map, host.as_deref()) {
            match value {
                Some(value) => {
                    text.set(key, &value);
                    map.insert(key.to_string(), value);
                }
                None => {
                    text.remove(key);
                    map.remove(key);
                }
            }
        }

        if !prior.had_settings_file {
            // Spec §9, T08: settings the user added while BYOK was active keep the file.
            if map.is_empty() {
                if self.settings_path.exists() {
                    fs::remove_file(&self.settings_path)?;
                }
                return Ok(());
            }
            return self.write_text(&text, &map);
        }
        if let Some(original) = prior.prior_raw.as_deref() {
            if parse_settings_bytes(original).is_ok_and(|original_map| original_map == map) {
                return self.atomic_write_bytes(original);
            }
        }
        self.write_text(&text, &map)
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

    /// The file's bytes, or none when it does not exist.
    fn read_raw(&self) -> Result<Vec<u8>, SettingsError> {
        match fs::read(&self.settings_path) {
            Ok(raw) => Ok(raw),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error.into()),
        }
    }

    /// Write an edited file, but only if it reads back as exactly `expected`. The
    /// editor and the reader are different parsers, and a file they disagree on is
    /// refused rather than written.
    fn write_text(
        &self,
        text: &SettingsText,
        expected: &Map<String, Value>,
    ) -> Result<(), SettingsError> {
        let bytes = text.to_bytes();
        if &parse_settings_bytes(&bytes)? != expected {
            return Err(invalid_data(
                "settings.json could not be edited in place without changing other settings",
            ));
        }
        self.atomic_write_bytes(&bytes)
    }

    fn atomic_write_bytes(&self, bytes: &[u8]) -> Result<(), SettingsError> {
        if let Some(parent) = self.settings_path.parent() {
            fs::create_dir_all(parent)?;
        }
        crate::patch::remove_stale_temps(&self.settings_path);

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
    // Editors such as Notepad save UTF-8 with a byte-order mark; Kiro reads through it.
    let content = content.strip_prefix(BOM).unwrap_or(content);
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

/// Preferences takeover sets, with the values it sets them to.
fn our_preferences() -> [(&'static str, Value); 3] {
    [
        // Tab Autocomplete off, to save the user's credits (Spec §2.5).
        ("kiroAgent.enableTabAutocomplete", json!(false)),
        // Auto-update frozen, so an update cannot silently replace the patch.
        ("update.mode", json!("none")),
        ("telemetry.telemetryLevel", json!("off")),
    ]
}

/// What each managed key becomes on rollback; `None` removes it.
///
/// The redirection keys always return to their prior values: anything left naming the
/// gateway would keep sending the customer's own traffic to it. A preference takeover
/// set returns to its prior value only while it still holds ours; a different value
/// is the user's own choice, made while taken over, and stays. From the proxy bypass
/// list only the gateway's host is removed, and only if takeover added it.
fn reverted_values(
    prior: &PriorSettingsState,
    current: &Map<String, Value>,
    gateway_host: Option<&str>,
) -> Vec<(&'static str, Option<Value>)> {
    let mut values: Vec<(&'static str, Option<Value>)> = ["kiroAuthConfig", "codewhisperer.config"]
        .into_iter()
        .map(|key| (key, prior.prior_values.get(key).cloned()))
        .collect();
    for (key, ours) in our_preferences() {
        let value = if current.get(key) == Some(&ours) {
            prior.prior_values.get(key).cloned()
        } else {
            current.get(key).cloned()
        };
        values.push((key, value));
    }
    if prior.proxy_bypass_managed {
        let prior_list = prior.prior_values.get("http.noProxy");
        let value = match (current.get("http.noProxy"), gateway_host) {
            (Some(Value::Array(list)), Some(host)) => {
                let added_by_us = !prior_list
                    .and_then(Value::as_array)
                    .is_some_and(|list| list.iter().any(|entry| entry.as_str() == Some(host)));
                let mut list = list.clone();
                if added_by_us {
                    list.retain(|entry| entry.as_str() != Some(host));
                }
                (!list.is_empty() || prior_list.is_some()).then_some(Value::Array(list))
            }
            // Nothing to reason from: return the list as it was.
            _ => prior_list.cloned(),
        };
        values.push(("http.noProxy", value));
    }
    values
}

fn gateway_host(gateway_url: &str) -> Option<String> {
    reqwest::Url::parse(gateway_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
}

fn invalid_data(message: &str) -> SettingsError {
    SettingsError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message.to_string(),
    ))
}

const BOM: &str = "\u{feff}";

/// What Kiro's settings reader accepts: comments and trailing commas, nothing looser.
/// Parsing more loosely could edit a file Kiro itself rejects into one it reads.
fn kiro_parse_options() -> jsonc_parser::ParseOptions {
    jsonc_parser::ParseOptions {
        allow_comments: true,
        allow_trailing_commas: true,
        allow_loose_object_property_names: false,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
    }
}

/// A settings file edited in place. Only the members named change; every other byte
/// (comments, order, indentation, line endings, a byte-order mark) stays as it was.
struct SettingsText {
    bom: bool,
    root: CstRootNode,
    object: CstObject,
}

impl SettingsText {
    /// Refuses a file whose root is not an object, rather than replacing it.
    fn parse(raw: &[u8]) -> Result<Self, SettingsError> {
        parse_settings_bytes(raw)?;
        let text =
            std::str::from_utf8(raw).map_err(|_| invalid_data("settings.json is not UTF-8"))?;
        let (bom, body) = match text.strip_prefix(BOM) {
            Some(body) => (true, body),
            None => (false, text),
        };
        let body = if body.trim().is_empty() { "{}" } else { body };
        let root = CstRootNode::parse(body, &kiro_parse_options())
            .map_err(|error| invalid_data(&format!("settings.json: {error}")))?;
        let object = root
            .object_value()
            .ok_or_else(|| invalid_data("settings.json must contain an object"))?;
        Ok(Self { bom, root, object })
    }

    /// Every top-level member named `key`. Kiro reads the last of duplicates.
    fn named(&self, key: &str) -> Vec<CstObjectProp> {
        self.object
            .properties()
            .into_iter()
            .filter(|prop| {
                prop.name()
                    .and_then(|name| name.decoded_value().ok())
                    .is_some_and(|name| name == key)
            })
            .collect()
    }

    /// Leave exactly one `key`, holding `value`: the last occurrence keeps its place,
    /// earlier duplicates go, and a missing key is added at the end.
    fn set(&self, key: &str, value: &Value) {
        let mut props = self.named(key);
        match props.pop() {
            Some(last) => {
                for earlier in props {
                    earlier.remove();
                }
                last.set_value(cst_value(value));
            }
            None => {
                self.object.append(key, cst_value(value));
            }
        }
    }

    fn remove(&self, key: &str) {
        for prop in self.named(key) {
            prop.remove();
        }
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut out = String::new();
        if self.bom {
            out.push_str(BOM);
        }
        out.push_str(&self.root.to_string());
        out.into_bytes()
    }
}

fn cst_value(value: &Value) -> CstInputValue {
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(value) => CstInputValue::Bool(*value),
        Value::Number(value) => CstInputValue::Number(value.to_string()),
        Value::String(value) => CstInputValue::String(value.clone()),
        Value::Array(items) => CstInputValue::Array(items.iter().map(cst_value).collect()),
        Value::Object(map) => CstInputValue::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), cst_value(value)))
                .collect(),
        ),
    }
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
                assert!(manager.revert(&prior, "https://fixture.invalid").is_err());
                assert_eq!(fs::read(manager.path()).unwrap(), bytes);
            }
        }
        fs::remove_file(manager.path()).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
