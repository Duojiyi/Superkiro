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
use jsonc_parser::JsonValue;
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

    /// Where the file stops being readable, so the customer knows what to fix.
    #[error(
        "settings.json has a syntax error at line {line}, column {column}; fix that line and retry"
    )]
    Syntax { line: usize, column: usize },
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
        let prior = self.capture_prior_state()?;
        self.atomic_write_bytes(&self.plan_merge(gateway_url)?)?;
        Ok(prior)
    }

    /// The file [`merge_byok`](Self::merge_byok) would write, built and read back in
    /// memory; nothing is written. A takeover works this out before it closes Kiro or
    /// touches the token, so a file it cannot edit is refused while nothing has changed.
    pub fn plan_merge(&self, gateway_url: &str) -> Result<Vec<u8>, SettingsError> {
        let region = REDIRECTED_REGION;
        let raw = self.read_raw()?;
        let text = SettingsText::parse(&raw, Reading::Kiro)?;
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
        verified_bytes(&text, &map)
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
        match self.plan_revert(prior, gateway_url)? {
            Rollback::Write(bytes) => self.atomic_write_bytes(&bytes),
            Rollback::Delete => {
                if self.settings_path.exists() {
                    fs::remove_file(&self.settings_path)?;
                }
                Ok(())
            }
        }
    }

    /// What [`revert`](Self::revert) would do, worked out without writing anything, so a
    /// restore can find out that the settings cannot be rolled back before it touches
    /// any other file.
    ///
    /// A file with a typo Kiro reads past (a missing comma) is rolled back as Kiro reads
    /// it: only the managed keys change and the typo stays where the customer left it.
    /// One not even that reading accepts is a [`SettingsError::Syntax`].
    pub(crate) fn plan_revert(
        &self,
        prior: &PriorSettingsState,
        gateway_url: &str,
    ) -> Result<Rollback, SettingsError> {
        let raw = self.read_raw()?;
        let (text, mut map) = read_for_rollback(&raw)?;
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
                return Ok(Rollback::Delete);
            }
            return verified_bytes(&text, &map).map(Rollback::Write);
        }
        // The original bytes would also quietly repair a typo made since, which is the
        // customer's own edit and stays.
        if text.reading == Reading::Kiro {
            if let Some(original) = prior.prior_raw.as_deref() {
                if parse_settings_bytes(original).is_ok_and(|original_map| original_map == map) {
                    return Ok(Rollback::Write(original.to_vec()));
                }
            }
        }
        verified_bytes(&text, &map).map(Rollback::Write)
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

    /// Profiles other than Default that a Kiro window or workspace is set to use.
    ///
    /// A profile reads its own `profiles/<id>/settings.json`, which takeover does not
    /// write. A window on one would get the gateway's token with the official
    /// endpoints, so takeover is refused while any is in use. Read from Kiro's
    /// `globalStorage/storage.json`; an unreadable file reports none.
    pub fn profiles_in_use(&self) -> Vec<String> {
        let Some(storage) = self
            .settings_path
            .parent()
            .map(|user| user.join("globalStorage").join("storage.json"))
        else {
            return Vec::new();
        };
        let Some(state) = fs::read(storage)
            .ok()
            .and_then(|raw| parse_settings_bytes(&raw).ok())
        else {
            return Vec::new();
        };
        let mut profiles: Vec<String> = ["workspaces", "emptyWindows"]
            .iter()
            .filter_map(|kind| state.get("profileAssociations")?.get(*kind)?.as_object())
            .flat_map(|associations| associations.values())
            .filter_map(Value::as_str)
            .filter(|profile| *profile != DEFAULT_PROFILE)
            .map(str::to_owned)
            .collect();
        profiles.sort();
        profiles.dedup();
        profiles
    }

    /// Whether the redirection keys still send Kiro to one of `gateway_hosts`, read as
    /// tolerantly as Kiro reads them. A file with a syntax error even that reading stops
    /// at counts as yes when it names one of them anywhere: Kiro may well still act on
    /// it, and nothing else would ever find that takeover. A file that cannot be read at
    /// all tells nothing and counts as no.
    pub fn names_gateway(&self, gateway_hosts: &[String]) -> bool {
        let Ok(raw) = self.read_raw() else {
            return false;
        };
        match parse_settings_bytes(&raw).or_else(|_| parse_tolerant(&raw)) {
            Ok(map) => REDIRECTION_KEYS.iter().any(|key| {
                map.get(*key)
                    .is_some_and(|value| names_host(value, gateway_hosts))
            }),
            Err(SettingsError::Syntax { .. }) => {
                mentions_host(&String::from_utf8_lossy(&raw), gateway_hosts)
            }
            Err(_) => false,
        }
    }

    /// Undo a takeover whose rollback record is lost, as far as the file itself shows.
    /// The redirection keys go if they name one of `gateway_hosts`, which also leave the
    /// proxy bypass list. A frozen `update.mode` is released too: without the record it
    /// cannot be told from the user's own choice, and a Kiro that never updates again,
    /// security fixes included, is the worse mistake. Returns whether anything changed.
    pub fn remove_orphaned_takeover(
        &self,
        gateway_hosts: &[String],
    ) -> Result<bool, SettingsError> {
        let Some(bytes) = self.plan_orphan_removal(gateway_hosts)? else {
            return Ok(false);
        };
        self.atomic_write_bytes(&bytes)?;
        Ok(true)
    }

    /// The file [`remove_orphaned_takeover`](Self::remove_orphaned_takeover) would write,
    /// or none when it has nothing to change; nothing is written. A typo Kiro reads past
    /// is kept, as in [`plan_revert`](Self::plan_revert).
    pub(crate) fn plan_orphan_removal(
        &self,
        gateway_hosts: &[String],
    ) -> Result<Option<Vec<u8>>, SettingsError> {
        let raw = self.read_raw()?;
        if raw.is_empty() {
            return Ok(None);
        }
        let (text, mut map) = read_for_rollback(&raw)?;
        let mut changed = false;
        for key in REDIRECTION_KEYS {
            if map
                .get(key)
                .is_some_and(|value| names_host(value, gateway_hosts))
            {
                text.remove(key);
                map.remove(key);
                changed = true;
            }
        }
        if let Some(Value::Array(list)) = map.get("http.noProxy") {
            let kept: Vec<Value> = list
                .iter()
                .filter(|entry| {
                    !entry
                        .as_str()
                        .is_some_and(|host| gateway_hosts.iter().any(|g| g == host))
                })
                .cloned()
                .collect();
            if kept.len() != list.len() {
                changed = true;
                if kept.is_empty() {
                    text.remove("http.noProxy");
                    map.remove("http.noProxy");
                } else {
                    let kept = Value::Array(kept);
                    text.set("http.noProxy", &kept);
                    map.insert("http.noProxy".into(), kept);
                }
            }
        }
        if changed && map.get("update.mode") == Some(&json!("none")) {
            text.remove("update.mode");
            map.remove("update.mode");
        }
        if !changed {
            return Ok(None);
        }
        verified_bytes(&text, &map).map(Some)
    }

    /// The file's bytes, or none when it does not exist.
    fn read_raw(&self) -> Result<Vec<u8>, SettingsError> {
        match fs::read(&self.settings_path) {
            Ok(raw) => Ok(raw),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error.into()),
        }
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
    let value = serde_json::from_str::<Value>(&strip_jsonc(content)).map_err(|error| {
        // A position in the stripped text is off by every comment taken out; the
        // editor's parser reads the file as it is.
        match jsonc_parser::parse_to_value(content, &Reading::Kiro.options()) {
            Err(error) => syntax_error(&error),
            Ok(_) => SettingsError::Json(error),
        }
    })?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(SettingsError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "settings.json must contain an object",
        ))),
    }
}

/// [`parse_settings_bytes`], reading past a missing comma as Kiro does. A different
/// parser from the editor's, so an edit that both read the same way is what Kiro reads.
fn parse_tolerant(raw: &[u8]) -> Result<Map<String, Value>, SettingsError> {
    let content =
        std::str::from_utf8(raw).map_err(|_| invalid_data("settings.json is not UTF-8"))?;
    let content = content.strip_prefix(BOM).unwrap_or(content);
    match jsonc_parser::parse_to_value(content, &Reading::Tolerant.options())
        .map_err(|error| syntax_error(&error))?
    {
        None => Ok(Map::new()),
        Some(JsonValue::Object(object)) => object
            .into_iter()
            .map(|(key, value)| Ok((key.into_owned(), serde_value(value)?)))
            .collect(),
        Some(_) => Err(invalid_data("settings.json must contain an object")),
    }
}

fn serde_value(value: JsonValue) -> Result<Value, SettingsError> {
    Ok(match value {
        JsonValue::Null => Value::Null,
        JsonValue::Boolean(value) => Value::Bool(value),
        // The literal as written, read by the same reader as the rest of the file.
        JsonValue::Number(text) => serde_json::from_str(text)?,
        JsonValue::String(text) => Value::String(text.into_owned()),
        JsonValue::Array(items) => Value::Array(
            items
                .into_iter()
                .map(serde_value)
                .collect::<Result<_, _>>()?,
        ),
        JsonValue::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| Ok((key.into_owned(), serde_value(value)?)))
                .collect::<Result<_, SettingsError>>()?,
        ),
    })
}

fn syntax_error(error: &jsonc_parser::errors::ParseError) -> SettingsError {
    SettingsError::Syntax {
        line: error.line_display(),
        column: error.column_display(),
    }
}

/// The file, opened for a rollback: read as takeover reads it or, failing that, as
/// tolerantly as Kiro itself does.
fn read_for_rollback(raw: &[u8]) -> Result<(SettingsText, Map<String, Value>), SettingsError> {
    SettingsText::parse(raw, Reading::Kiro)
        .and_then(|text| Ok((text, parse_settings_bytes(raw)?)))
        .or_else(|_| {
            Ok((
                SettingsText::parse(raw, Reading::Tolerant)?,
                parse_tolerant(raw)?,
            ))
        })
}

/// The edited file, but only if it reads back as exactly `expected`. The editor and the
/// reader are different parsers, and a file they disagree on is refused rather than
/// written.
fn verified_bytes(
    text: &SettingsText,
    expected: &Map<String, Value>,
) -> Result<Vec<u8>, SettingsError> {
    let bytes = text.to_bytes();
    let actual = match text.reading {
        Reading::Kiro => parse_settings_bytes(&bytes)?,
        Reading::Tolerant => parse_tolerant(&bytes)?,
    };
    if &actual != expected {
        return Err(invalid_data(
            "settings.json could not be edited in place without changing other settings",
        ));
    }
    Ok(bytes)
}

/// A settings rollback worked out in memory.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Rollback {
    Write(Vec<u8>),
    /// The file did not exist before takeover and nothing else is in it now.
    Delete,
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
    let mut values: Vec<(&'static str, Option<Value>)> = REDIRECTION_KEYS
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

/// How Kiro names the Default profile in its window associations.
const DEFAULT_PROFILE: &str = "__default__profile__";

/// The keys that point Kiro's own traffic at an endpoint.
const REDIRECTION_KEYS: [&str; 2] = ["kiroAuthConfig", "codewhisperer.config"];

/// Whether any URL inside `value` is on one of `hosts`.
fn names_host(value: &Value, hosts: &[String]) -> bool {
    match value {
        Value::String(text) => gateway_host(text).is_some_and(|host| hosts.contains(&host)),
        Value::Array(items) => items.iter().any(|item| names_host(item, hosts)),
        Value::Object(map) => map.values().any(|item| names_host(item, hosts)),
        _ => false,
    }
}

/// Whether `text` holds a URL on one of `hosts`, for a file too broken to parse.
fn mentions_host(text: &str, hosts: &[String]) -> bool {
    hosts.iter().any(|host| {
        let url = format!("://{host}");
        text.match_indices(&url).any(|(at, _)| {
            !text[at + url.len()..]
                .starts_with(|c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '.'))
        })
    })
}

pub(crate) fn gateway_host(gateway_url: &str) -> Option<String> {
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

/// How far a read of settings.json goes past what strict JSON allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reading {
    /// What takeover accepts: comments and trailing commas, nothing looser. Parsing more
    /// loosely could edit a file Kiro itself rejects into one it reads.
    Kiro,
    /// Also a missing comma, which Kiro's own reader reports and reads past, so a
    /// customer can leave one while taken over and never notice. Only a rollback reads
    /// this way: it takes out what takeover put in and leaves the rest as it is.
    Tolerant,
}

impl Reading {
    fn options(self) -> jsonc_parser::ParseOptions {
        jsonc_parser::ParseOptions {
            allow_comments: true,
            allow_trailing_commas: true,
            allow_loose_object_property_names: false,
            allow_missing_commas: self == Reading::Tolerant,
            allow_single_quoted_strings: false,
            allow_hexadecimal_numbers: false,
            allow_unary_plus_numbers: false,
        }
    }
}

/// A settings file edited in place. Only the members named change; every other byte
/// (comments, order, indentation, line endings, a byte-order mark) stays as it was.
struct SettingsText {
    bom: bool,
    reading: Reading,
    root: CstRootNode,
    object: CstObject,
}

impl SettingsText {
    /// Refuses a file whose root is not an object, rather than replacing it.
    fn parse(raw: &[u8], reading: Reading) -> Result<Self, SettingsError> {
        if reading == Reading::Kiro {
            parse_settings_bytes(raw)?;
        }
        let text =
            std::str::from_utf8(raw).map_err(|_| invalid_data("settings.json is not UTF-8"))?;
        let (bom, body) = match text.strip_prefix(BOM) {
            Some(body) => (true, body),
            None => (false, text),
        };
        let body = if body.trim().is_empty() { "{}" } else { body };
        let root =
            CstRootNode::parse(body, &reading.options()).map_err(|error| syntax_error(&error))?;
        let object = root
            .object_value()
            .ok_or_else(|| invalid_data("settings.json must contain an object"))?;
        Ok(Self {
            bom,
            reading,
            root,
            object,
        })
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
                    self.remove_member(earlier);
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
            self.remove_member(prop);
        }
    }

    /// Take out one member and the one comma that separates it. The editor takes the
    /// member's own comma or, when it has none, the nearest one before it; next to a
    /// missing comma (a typo Kiro reads past) that is the customer's, and their typo
    /// would move onto their own line. So first give the right member a comma of its
    /// own: this one when another member follows it, else the one before it.
    fn remove_member(&self, prop: CstObjectProp) {
        if prop.trailing_comma().is_none() {
            if prop.next_property().is_some() {
                self.give_comma(&prop);
            } else if let Some(previous) = prop
                .previous_property()
                .filter(|previous| previous.trailing_comma().is_none())
            {
                self.give_comma(&previous);
            }
        }
        prop.remove();
    }

    /// A member inserted right after `prop` makes the editor give `prop` a comma, and
    /// removing that member again (with its own comma) leaves `prop`'s in place.
    fn give_comma(&self, prop: &CstObjectProp) {
        self.object
            .insert(prop.property_index() + 1, "", CstInputValue::Null)
            .remove();
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
