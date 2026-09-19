//! Cross-platform device fingerprint generation and validation (Spec §4.1, §14.5).
//!
//! Generates deterministic, irreversible hardware fingerprints for device binding
//! and rebind tracking.

use ring::digest::{digest, SHA256};
use std::env;

/// Prefix for standardized device identifiers.
pub const DEVICE_ID_PREFIX: &str = "dev_";

/// Generate a deterministic, irreversible device fingerprint for the current machine.
///
/// Collects stable hardware and environment indicators across platforms:
/// - Windows: `COMPUTERNAME`, `USERNAME`, `PROCESSOR_IDENTIFIER`, `NUMBER_OF_PROCESSORS`, `OS`
/// - Unix/Linux/macOS: `HOSTNAME`, `USER`, hardware/machine hints
/// - Optional application-level salt to isolate domains
pub fn generate_device_fingerprint(salt: Option<&str>) -> String {
    let mut raw_markers = String::with_capacity(512);

    if let Some(s) = salt {
        raw_markers.push_str(s);
        raw_markers.push('|');
    }

    // macOS hardware identity distinguishes machines with identical user/host labels.
    // Preserve existing Windows fingerprints so upgrades do not unbind paid cards.
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("/usr/sbin/ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
        {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                if let Some(uuid) = text.lines().find_map(|line| {
                    let (key, value) = line.split_once('=')?;
                    key.contains("IOPlatformUUID")
                        .then(|| value.trim().trim_matches('"'))
                }) {
                    raw_markers.push_str(uuid);
                    raw_markers.push('|');
                }
            }
        }
    }

    // Windows indicators
    if let Ok(val) = env::var("COMPUTERNAME") {
        raw_markers.push_str(&val);
        raw_markers.push('|');
    }
    if let Ok(val) = env::var("USERNAME") {
        raw_markers.push_str(&val);
        raw_markers.push('|');
    }
    if let Ok(val) = env::var("PROCESSOR_IDENTIFIER") {
        raw_markers.push_str(&val);
        raw_markers.push('|');
    }
    if let Ok(val) = env::var("NUMBER_OF_PROCESSORS") {
        raw_markers.push_str(&val);
        raw_markers.push('|');
    }
    if let Ok(val) = env::var("OS") {
        raw_markers.push_str(&val);
        raw_markers.push('|');
    }

    // Unix indicators
    if let Ok(val) = env::var("HOSTNAME") {
        raw_markers.push_str(&val);
        raw_markers.push('|');
    }
    if let Ok(val) = env::var("USER") {
        raw_markers.push_str(&val);
        raw_markers.push('|');
    }

    // Architecture & OS family fallback (guarantees non-empty marker string)
    raw_markers.push_str(env::consts::OS);
    raw_markers.push('|');
    raw_markers.push_str(env::consts::ARCH);

    // Compute SHA-256
    let hash = digest(&SHA256, raw_markers.as_bytes());
    let mut hex = String::with_capacity(64);
    for &b in hash.as_ref() {
        hex.push(char::from(b"0123456789abcdef"[(b >> 4) as usize]));
        hex.push(char::from(b"0123456789abcdef"[(b & 0xf) as usize]));
    }

    // Format as dev_<first 32 hex chars>
    format!("{}{}", DEVICE_ID_PREFIX, &hex[..32])
}

/// Validate whether a given string is a valid device identifier format (`dev_[0-9a-f]{32}`).
pub fn is_valid_device_fingerprint(id: &str) -> bool {
    if !id.starts_with(DEVICE_ID_PREFIX) {
        return false;
    }
    let hex_part = &id[DEVICE_ID_PREFIX.len()..];
    if hex_part.len() != 32 {
        return false;
    }
    hex_part.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_fingerprint_deterministic() {
        let fp1 = generate_device_fingerprint(None);
        let fp2 = generate_device_fingerprint(None);
        assert_eq!(fp1, fp2);
        assert!(is_valid_device_fingerprint(&fp1));
    }

    #[test]
    fn test_device_fingerprint_salt_isolation() {
        let fp_default = generate_device_fingerprint(None);
        let fp_salt_a = generate_device_fingerprint(Some("salt-app-a"));
        let fp_salt_b = generate_device_fingerprint(Some("salt-app-b"));

        assert_ne!(fp_default, fp_salt_a);
        assert_ne!(fp_salt_a, fp_salt_b);
        assert!(is_valid_device_fingerprint(&fp_salt_a));
        assert!(is_valid_device_fingerprint(&fp_salt_b));
    }

    #[test]
    fn test_is_valid_device_fingerprint() {
        assert!(is_valid_device_fingerprint(
            "dev_0123456789abcdef0123456789abcdef"
        ));
        assert!(!is_valid_device_fingerprint("dev_short"));
        assert!(!is_valid_device_fingerprint(
            "0123456789abcdef0123456789abcdef"
        ));
        assert!(!is_valid_device_fingerprint(
            "dev_0123456789abcdef0123456789abcdeg"
        )); // 'g' is invalid hex
    }
}
