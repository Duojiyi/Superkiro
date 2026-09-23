//! Kiro 1.0.x auth token disk persistence, atomic update, and hot-reload driver (Spec §4.1, P0-4).
//!
//! Kiro IDE extension monitors `%USERPROFILE%\.aws\sso\cache\kiro-auth-token.json` via
//! `fs.watchFile`. Atomic writing ensures no partial read or corruption by Kiro.

use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// File name for Kiro SSO token cache.
pub const KIRO_TOKEN_FILENAME: &str = "kiro-auth-token.json";

#[derive(Debug, Error)]
pub enum TokenStorageError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Home directory not found")]
    HomeNotFound,

    #[allow(dead_code)]
    #[error("Invalid token format: {0}")]
    InvalidToken(String),
}

/// Token schema expected by Kiro 1.0.x extension (`extension.js` `TokenStorage`).
///
/// Verified with P0-4 authentic token sample (`docs/p0/samples/auth_token_sample.json`).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroAuthToken {
    pub access_token: String,
    pub refresh_token: String,
    pub profile_arn: String,
    pub expires_at: String,
    pub auth_method: String,
    pub provider: String,
}

impl std::fmt::Debug for KiroAuthToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KiroAuthToken")
            .field("access_token", &"[redacted]")
            .field("refresh_token", &"[redacted]")
            .field("profile_arn", &self.profile_arn)
            .field("expires_at", &self.expires_at)
            .field("auth_method", &self.auth_method)
            .field("provider", &self.provider)
            .finish()
    }
}

impl KiroAuthToken {
    /// Create a standard token object with specified credentials.
    pub fn new(
        access_token: impl Into<String>,
        refresh_token: impl Into<String>,
        profile_arn: impl Into<String>,
        expires_at: impl Into<String>,
    ) -> Self {
        Self {
            access_token: access_token.into(),
            refresh_token: refresh_token.into(),
            profile_arn: profile_arn.into(),
            expires_at: expires_at.into(),
            auth_method: "social".to_string(),
            provider: "Google".to_string(),
        }
    }
}

/// Default system token storage path:
/// Windows: `%USERPROFILE%\.aws\sso\cache\kiro-auth-token.json`
/// Unix/macOS: `$HOME/.aws/sso/cache/kiro-auth-token.json`
pub fn default_token_path() -> Result<PathBuf, TokenStorageError> {
    let home = if cfg!(windows) {
        std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME"))
    } else {
        std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"))
    }
    .map_err(|_| TokenStorageError::HomeNotFound)?;

    let mut path = PathBuf::from(home);
    path.push(".aws");
    path.push("sso");
    path.push("cache");
    path.push(KIRO_TOKEN_FILENAME);
    Ok(path)
}

/// Manager for atomic disk persistence of Kiro auth tokens.
#[derive(Debug, Clone)]
pub struct TokenStorage {
    target_path: PathBuf,
}

impl Default for TokenStorage {
    fn default() -> Self {
        let path = default_token_path().unwrap_or_else(|_| PathBuf::from(KIRO_TOKEN_FILENAME));
        Self { target_path: path }
    }
}

impl TokenStorage {
    /// Construct token storage targeting a specific absolute path (for isolated testing or custom setups).
    pub fn at(target_path: impl Into<PathBuf>) -> Self {
        Self {
            target_path: target_path.into(),
        }
    }

    /// Target path of the token file.
    pub fn path(&self) -> &Path {
        &self.target_path
    }

    /// Check if the token file exists on disk.
    pub fn exists(&self) -> bool {
        self.target_path.exists()
    }

    /// Atomically persist a `KiroAuthToken` to disk.
    ///
    /// Writes first to a temp file in the same directory (`.tmp.<pid>.<ts>`), flushes,
    /// and renames to the target path. This guarantees atomic replacement on all OSes
    /// and ensures Kiro's `fs.watchFile` never reads incomplete JSON.
    pub fn save(&self, token: &KiroAuthToken) -> Result<(), TokenStorageError> {
        private_atomic_write(&self.target_path, &serde_json::to_vec_pretty(token)?)?;
        Ok(())
    }

    /// Read and parse token from disk.
    pub fn load(&self) -> Result<KiroAuthToken, TokenStorageError> {
        if !self.target_path.exists() {
            return Err(TokenStorageError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Token file not found at {:?}", self.target_path),
            )));
        }

        let mut file = File::open(&self.target_path)?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;

        let token: KiroAuthToken = serde_json::from_str(&content)?;
        Ok(token)
    }

    /// Safely delete the token file (logout / clean state).
    pub fn clear(&self) -> Result<bool, TokenStorageError> {
        if self.target_path.exists() {
            fs::remove_file(&self.target_path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Check if the stored token has expired or is expiring within `buffer_secs`.
    ///
    /// Parses ISO-8601 timestamps (e.g. `2026-09-10T06:21:02.273Z` or `2026-09-10T06:21:02Z`).
    pub fn is_expired(&self, buffer_secs: u64) -> bool {
        let token = match self.load() {
            Ok(t) => t,
            Err(_) => return true,
        };

        parse_iso8601_to_epoch(&token.expires_at)
            .map(|exp| {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                now.saturating_add(buffer_secs) >= exp
            })
            .unwrap_or(true)
    }
}

/// Restrict a newly created, empty staging entry before writing any secrets.
fn restrict_private(path: &Path, directory: bool) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
        )?;
    }
    #[cfg(windows)]
    {
        // Replace the entire DACL: icacls /inheritance:r /grant:r leaves
        // unrelated explicit ACEs intact on some Windows temp directories.
        crate::windows_security::restrict_to_current_user(path, directory)?;
    }
    Ok(())
}

fn create_private_parents(path: &Path) -> std::io::Result<()> {
    if path.as_os_str().is_empty() || path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        create_private_parents(parent)?;
    }
    #[allow(unused_mut)]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    restrict_private(path, true)
}

pub(crate) fn private_atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        create_private_parents(parent)?;
    }
    // Never chmod shared HOME/cache directories. A private sibling directory protects
    // staging data; atomic rename preserves the file's private mode/DACL at destination.
    let staging = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    #[allow(unused_mut)]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&staging)?;
    let temp = staging.join("data");
    let result = (|| {
        restrict_private(&staging, true)?;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        restrict_private(&temp, false)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        crate::snapshot::atomic_replace(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    let _ = fs::remove_dir(&staging);
    result
}

/// Simple RFC 3339 / ISO 8601 date-time parser to Unix timestamp (seconds).
///
/// Avoids extra datetime dependencies by extracting year, month, day, hour, min, sec.
pub fn parse_iso8601_to_epoch(s: &str) -> Option<u64> {
    // Expected format: YYYY-MM-DDTHH:MM:SS...
    if !s.is_ascii() || s.len() < 19 || s.as_bytes()[10] != b'T' {
        return None;
    }

    let year: i64 = s[0..4].parse().ok()?;
    let month: u64 = s[5..7].parse().ok()?;
    let day: u64 = s[8..10].parse().ok()?;
    let hour: u64 = s[11..13].parse().ok()?;
    let minute: u64 = s[14..16].parse().ok()?;
    let second: u64 = s[17..19].parse().ok()?;

    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }

    // Days since Unix epoch (1970-01-01)
    let days = days_from_civil(year, month, day);
    if days < 0 {
        return None;
    }

    let mut total_seconds = (days as u64) * 86_400 + hour * 3600 + minute * 60 + second;

    if let Some(plus_idx) = s[19..].find('+').map(|i| i + 19) {
        if s.len() < plus_idx + 6 {
            return None;
        }
        if let (Ok(oh), Ok(om)) = (
            s[plus_idx + 1..plus_idx + 3].parse::<u64>(),
            s[plus_idx + 4..plus_idx + 6].parse::<u64>(),
        ) {
            total_seconds = total_seconds.saturating_sub(oh * 3600 + om * 60);
        }
    } else if let Some(minus_idx) = s[19..].find('-').map(|i| i + 19) {
        if s.len() < minus_idx + 6 {
            return None;
        }
        if let (Ok(oh), Ok(om)) = (
            s[minus_idx + 1..minus_idx + 3].parse::<u64>(),
            s[minus_idx + 4..minus_idx + 6].parse::<u64>(),
        ) {
            total_seconds = total_seconds.saturating_add(oh * 3600 + om * 60);
        }
    }
    // ponytail: naive tz offset parse, no IANA database

    Some(total_seconds)
}

/// Days from 1970-01-01 using Euclidean affine algorithm (Howard Hinnant formula).
fn days_from_civil(year: i64, month: u64, day: u64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let m = if month <= 2 { month + 9 } else { month - 3 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + (doe as i64) - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iso8601_parser() {
        // 2026-09-10T06:21:02Z
        let epoch = parse_iso8601_to_epoch("2026-09-10T06:21:02Z").unwrap();
        assert!(epoch > 1_700_000_000);

        // 1970-01-01T00:00:00Z -> 0
        assert_eq!(parse_iso8601_to_epoch("1970-01-01T00:00:00Z"), Some(0));

        // Malformed input
        assert_eq!(parse_iso8601_to_epoch("invalid"), None);
    }

    #[test]
    fn test_token_storage_lifecycle_atomic() {
        let temp_dir =
            std::env::temp_dir().join(format!("kiro_test_storage_{}", std::process::id()));
        let token_file = temp_dir.join(KIRO_TOKEN_FILENAME);

        let storage = TokenStorage::at(&token_file);
        assert!(!storage.exists());

        let token = KiroAuthToken::new(
            "jwt-access-123",
            "refresh-token-456",
            "arn:aws:codewhisperer:us-east-1:123456789012:profile/BYOK",
            "2030-01-01T00:00:00Z",
        );

        // Save
        storage.save(&token).expect("Save token must succeed");
        assert!(storage.exists());

        // Load
        let loaded = storage.load().expect("Load token must succeed");
        assert_eq!(loaded, token);
        assert_eq!(loaded.auth_method, "social");
        assert_eq!(loaded.provider, "Google");

        // Expiration check (2030 is not expired)
        assert!(!storage.is_expired(0));

        // Clear
        assert!(storage.clear().expect("Clear token must succeed"));
        assert!(!storage.exists());

        // Cleanup test directory
        let _ = fs::remove_dir_all(temp_dir);
    }
}

#[cfg(test)]
#[path = "../tests/privacy/audit.rs"]
mod audit_privacy;
