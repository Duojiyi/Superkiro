//! Tests for device fingerprint generation and validation (Task P3-1).

use patch_engine::device::{
    generate_device_fingerprint, is_valid_device_fingerprint, DEVICE_ID_PREFIX,
};

#[test]
fn test_fingerprint_is_deterministic() {
    let id1 = generate_device_fingerprint(None);
    let id2 = generate_device_fingerprint(None);
    assert_eq!(id1, id2);
    assert!(is_valid_device_fingerprint(&id1));
}

#[test]
fn test_fingerprint_structure_and_length() {
    let id = generate_device_fingerprint(None);
    assert!(id.starts_with(DEVICE_ID_PREFIX));
    assert_eq!(id.len(), DEVICE_ID_PREFIX.len() + 32);
    assert!(id[DEVICE_ID_PREFIX.len()..]
        .chars()
        .all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn test_fingerprint_salting_isolation() {
    let id_a = generate_device_fingerprint(Some("salt-client-a"));
    let id_b = generate_device_fingerprint(Some("salt-client-b"));
    assert_ne!(id_a, id_b);
    assert!(is_valid_device_fingerprint(&id_a));
    assert!(is_valid_device_fingerprint(&id_b));
}

#[test]
fn test_is_valid_device_fingerprint() {
    assert!(is_valid_device_fingerprint(
        "dev_0123456789abcdef0123456789abcdef"
    ));
    assert!(!is_valid_device_fingerprint(
        "dev_0123456789abcdef0123456789abcde"
    )); // 31 chars
    assert!(!is_valid_device_fingerprint(
        "dev_0123456789abcdef0123456789abcdef0"
    )); // 33 chars
    assert!(!is_valid_device_fingerprint(
        "xyz_0123456789abcdef0123456789abcdef"
    )); // wrong prefix
    assert!(!is_valid_device_fingerprint(
        "dev_0123456789abcdef0123456789abcdeg"
    )); // invalid hex 'g'
}
