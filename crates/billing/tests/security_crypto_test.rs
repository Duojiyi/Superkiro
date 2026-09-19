use billing::crypto::{
    mask_provider_key, rotate_all_provider_keys, rotate_provider_key, CryptoError, MasterKek,
};

#[test]
fn test_master_kek_injection_from_hex_and_random() {
    let kek_random = MasterKek::generate_random().unwrap();
    // Verify Debug formatting does NOT leak raw key bytes
    let debug_str = format!("{:?}", kek_random);
    assert_eq!(debug_str, "MasterKek([REDACTED])");

    // 64 hex characters (32 bytes)
    let valid_hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let kek_hex = MasterKek::from_hex(valid_hex).unwrap();
    assert_eq!(format!("{:?}", kek_hex), "MasterKek([REDACTED])");

    // Invalid hex length
    let invalid_hex = "0123456789abcdef";
    assert!(matches!(
        MasterKek::from_hex(invalid_hex),
        Err(CryptoError::InvalidKekLength { .. })
    ));
}

#[test]
fn test_master_kek_injection_from_env_and_file() {
    let valid_hex = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    std::env::set_var("TEST_KIRO_KEK", valid_hex);

    let from_env = MasterKek::from_env("TEST_KIRO_KEK").unwrap();
    assert_eq!(format!("{:?}", from_env), "MasterKek([REDACTED])");

    // File injection
    let temp_path = std::env::temp_dir().join(format!("kiro_test_kek_{}.txt", std::process::id()));
    std::fs::write(&temp_path, valid_hex).unwrap();
    let from_file = MasterKek::from_file(&temp_path).unwrap();
    let _ = std::fs::remove_file(&temp_path);
    assert_eq!(format!("{:?}", from_file), "MasterKek([REDACTED])");
}

#[test]
fn test_aes_256_gcm_authenticated_encryption_and_decryption() {
    let kek = MasterKek::generate_random().unwrap();
    let raw_key = "sk-ant-api03-abcdef1234567890-SECRET-KEY-TOKEN";

    let cipher = kek.encrypt(raw_key).unwrap();
    assert!(cipher.starts_with("v1:"));
    let parts: Vec<&str> = cipher.split(':').collect();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], "v1");
    // 12 bytes nonce = 24 hex characters
    assert_eq!(parts[1].len(), 24);

    let decrypted = kek.decrypt(&cipher).unwrap();
    assert_eq!(decrypted, raw_key);
}

#[test]
fn test_tampered_ciphertext_or_wrong_kek_fails() {
    let kek_a = MasterKek::generate_random().unwrap();
    let kek_b = MasterKek::generate_random().unwrap();

    let raw_key = "sk-live-0987654321";
    let cipher = kek_a.encrypt(raw_key).unwrap();

    // Decrypting with wrong KEK must fail authentication
    assert!(matches!(
        kek_b.decrypt(&cipher),
        Err(CryptoError::DecryptionError)
    ));

    // Tampered payload fails authentication
    let mut parts: Vec<String> = cipher.split(':').map(|s| s.to_string()).collect();
    // Invert first hex char of ciphertext
    let mut payload = parts[2].clone();
    let first = payload.remove(0);
    let new_first = if first == 'a' { 'b' } else { 'a' };
    payload.insert(0, new_first);
    parts[2] = payload;
    let tampered = parts.join(":");

    assert!(matches!(
        kek_a.decrypt(&tampered),
        Err(CryptoError::DecryptionError)
    ));
}

#[test]
fn test_master_kek_rotation_single_and_batch() {
    let old_kek = MasterKek::generate_random().unwrap();
    let new_kek = MasterKek::generate_random().unwrap();

    let key_1 = "sk-ant-key-one-111111";
    let key_2 = "sk-openai-key-two-222222";

    let cipher_1 = old_kek.encrypt(key_1).unwrap();
    let cipher_2 = old_kek.encrypt(key_2).unwrap();

    let mut batch = vec![cipher_1.clone(), cipher_2.clone()];
    let rotated_count = rotate_all_provider_keys(&old_kek, &new_kek, &mut batch).unwrap();
    assert_eq!(rotated_count, 2);

    // Old KEK can no longer decrypt rotated keys
    assert!(old_kek.decrypt(&batch[0]).is_err());
    assert!(old_kek.decrypt(&batch[1]).is_err());

    // New KEK decrypts perfectly
    assert_eq!(new_kek.decrypt(&batch[0]).unwrap(), key_1);
    assert_eq!(new_kek.decrypt(&batch[1]).unwrap(), key_2);

    // Single key rotation
    let single_rotated = rotate_provider_key(&old_kek, &new_kek, &cipher_1).unwrap();
    assert_eq!(new_kek.decrypt(&single_rotated).unwrap(), key_1);
}

#[test]
fn test_provider_key_masking_for_ui_and_logs() {
    assert_eq!(
        mask_provider_key("sk-ant-api03-abcdef1234567890"),
        "sk-ant...7890"
    );
    assert_eq!(
        mask_provider_key("sk-proj-1234567890abcdefghij"),
        "sk-pro...ghij"
    );
    assert_eq!(mask_provider_key("short-key-16char"), "sh***ar");
    assert_eq!(mask_provider_key("tiny"), "***");
}

#[test]
fn test_encrypted_snapshot_save_and_load() {
    use billing::card::Card;
    use billing::engine::BillingEngine;

    let kek = MasterKek::generate_random().unwrap();
    let temp_file =
        std::env::temp_dir().join(format!("kiro_test_snap_{}.json", std::process::id()));

    // 1. Save with MasterKek
    let billing = BillingEngine::new();
    billing.set_master_kek(kek.clone());
    let card = Card::new("card-aead-01", "grp-pro", 1_000_000);
    billing.upsert_card(card);
    billing
        .save_to_file(&temp_file)
        .expect("save_to_file must succeed");

    // 2. Verify file content on disk is an encrypted envelope, NOT raw JSON with cards
    let raw_content = std::fs::read_to_string(&temp_file).unwrap();
    assert!(raw_content.contains("kiro-billing-aead-v1"));
    assert!(raw_content.contains("sha256_checksum"));
    assert!(
        !raw_content.contains("card-aead-01"),
        "Plaintext card_id leaked in snapshot!"
    );

    // 3. Load with same MasterKek
    let billing_restored = BillingEngine::new();
    billing_restored.set_master_kek(kek);
    billing_restored
        .load_from_file(&temp_file)
        .expect("load_from_file must succeed");

    let restored_card = billing_restored
        .get_card("card-aead-01")
        .expect("card must exist");
    assert_eq!(restored_card.id, "card-aead-01");
    assert_eq!(restored_card.credit_total, 1_000_000);

    let _ = std::fs::remove_file(&temp_file);
}

#[test]
fn test_encrypted_snapshot_wrong_kek_or_missing_kek_rejected() {
    use billing::card::Card;
    use billing::engine::BillingEngine;

    let kek_a = MasterKek::generate_random().unwrap();
    let kek_b = MasterKek::generate_random().unwrap();
    let temp_file =
        std::env::temp_dir().join(format!("kiro_test_snap_wrong_{}.json", std::process::id()));

    let billing = BillingEngine::new();
    billing.set_master_kek(kek_a);
    let card = Card::new("card-secret-02", "grp-pro", 500_000);
    billing.upsert_card(card);
    billing.save_to_file(&temp_file).unwrap();

    // 1. Loading with NO kek must fail
    let billing_no_kek = BillingEngine::new();
    assert!(billing_no_kek.load_from_file(&temp_file).is_err());

    // 2. Loading with WRONG kek must fail
    let billing_wrong_kek = BillingEngine::new();
    billing_wrong_kek.set_master_kek(kek_b);
    assert!(billing_wrong_kek.load_from_file(&temp_file).is_err());

    let _ = std::fs::remove_file(&temp_file);
}

#[test]
fn test_encrypted_snapshot_tampered_rejected() {
    use billing::card::Card;
    use billing::engine::{BillingEngine, EncryptedSnapshotEnvelope};

    let kek = MasterKek::generate_random().unwrap();
    let temp_file =
        std::env::temp_dir().join(format!("kiro_test_snap_tamper_{}.json", std::process::id()));

    let billing = BillingEngine::new();
    billing.set_master_kek(kek.clone());
    let card = Card::new("card-tamper-03", "grp-pro", 200_000);
    billing.upsert_card(card);
    billing.save_to_file(&temp_file).unwrap();

    // Tamper with the ciphertext
    let raw_content = std::fs::read_to_string(&temp_file).unwrap();
    let mut envelope: EncryptedSnapshotEnvelope = serde_json::from_str(&raw_content).unwrap();
    // Invert characters in ciphertext
    envelope.ciphertext.push_str("tampered");
    let tampered = serde_json::to_string(&envelope).unwrap();
    std::fs::write(&temp_file, &tampered).unwrap();
    // Corrupt the committed generation too. A mirror-only corruption is
    // recoverable from the authenticated committed generation by design.
    let anchor = billing::engine::read_snapshot_anchor(&temp_file).unwrap();
    let generation = temp_file
        .parent()
        .unwrap()
        .join(anchor.generation_file.unwrap());
    std::fs::write(&generation, &tampered).unwrap();

    // Loading tampered snapshot must fail
    let billing_verify = BillingEngine::new();
    billing_verify.set_master_kek(kek);
    assert!(billing_verify.load_from_file(&temp_file).is_err());

    let _ = std::fs::remove_file(&temp_file);
}

#[test]
fn test_plain_snapshot_backward_compatibility() {
    use billing::card::Card;
    use billing::engine::BillingEngine;

    let temp_file =
        std::env::temp_dir().join(format!("kiro_test_snap_plain_{}.json", std::process::id()));

    // 1. Save unencrypted snapshot
    let billing_plain = BillingEngine::new();
    let card = Card::new("card-plain-04", "grp-pro", 300_000);
    billing_plain.upsert_card(card);
    billing_plain.save_to_file(&temp_file).unwrap();

    // Verify it's plain JSON
    let content = std::fs::read_to_string(&temp_file).unwrap();
    assert!(content.contains("card-plain-04"));

    // 2. Load into new billing engine without KEK
    let billing_restored = BillingEngine::new();
    billing_restored.load_from_file(&temp_file).unwrap();
    assert_eq!(
        billing_restored
            .get_card("card-plain-04")
            .unwrap()
            .credit_total,
        300_000
    );

    // 3. Now upgrade with KEK and save
    let kek = MasterKek::generate_random().unwrap();
    billing_restored.set_master_kek(kek.clone());
    billing_restored.save_to_file(&temp_file).unwrap();

    // File is now encrypted!
    let encrypted_content = std::fs::read_to_string(&temp_file).unwrap();
    assert!(encrypted_content.contains("kiro-billing-aead-v1"));
    assert!(!encrypted_content.contains("card-plain-04"));

    let _ = std::fs::remove_file(&temp_file);
}
