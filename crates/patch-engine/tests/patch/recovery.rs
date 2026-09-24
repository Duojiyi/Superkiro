use super::*;

#[test]
fn recovery_validates_backups_and_every_durable_boundary() {
    let root = std::env::temp_dir().join(format!("patch-recovery-fixture-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let patcher = ExtensionPatcher::new(root.join("extension.js"));
    let original = b"const original = true;";
    let patched = b"/* @patched-kiro-byok v1 */ const modified = true;";
    let state = PatchState {
        original_len: original.len() as u64,
        original_hash: content_hash(original),
        patched_hash: content_hash(patched),
        previous_patched_hash: None,
        owner: None,
    };
    // Persisted metadata, partial/complete backup, patch publication, restored
    // publication, backup cleanup, metadata cleanup: retries cover every boundary.
    for stage in 0..6 {
        fs::write(
            patcher.path(),
            if stage == 2 {
                patched.as_slice()
            } else {
                original.as_slice()
            },
        )
        .unwrap();
        patcher.write_state(&state).unwrap();
        if (1..=3).contains(&stage) {
            fs::write(patcher.backup_path(), original).unwrap();
        }
        if stage == 5 {
            fs::remove_file(patcher.state_path()).unwrap();
        }
        assert!(patcher.verify_restore().is_ok(), "stage {stage}");
        // Execute the real restore implementation against local fixtures only.
        assert_eq!(patcher.restore_validated().unwrap(), stage != 5);
        assert!(!patcher.restore_validated().unwrap());
        assert_eq!(fs::read(patcher.path()).unwrap(), original);
        assert!(patcher.restore_material().unwrap().is_none());
    }
    // Repair metadata can be published before the replacement; either known
    // patched revision remains recoverable at that crash boundary.
    let staged = PatchState {
        original_len: state.original_len,
        original_hash: state.original_hash.clone(),
        patched_hash: content_hash(b"new repair"),
        previous_patched_hash: Some(content_hash(patched)),
        owner: None,
    };
    patcher.write_state(&staged).unwrap();
    fs::write(patcher.path(), patched).unwrap();
    fs::write(patcher.backup_path(), original).unwrap();
    patcher.verify_patched_content().unwrap();
    assert!(patcher.restore_validated().unwrap());
    for corrupt in [b"".as_slice(), b"truncated", b"const original = evil;"] {
        fs::write(patcher.path(), patched).unwrap();
        patcher.write_state(&state).unwrap();
        fs::write(patcher.backup_path(), corrupt).unwrap();
        assert!(patcher.verify_restore().is_err());
        assert!(patcher.restore_validated().is_err());
        assert_eq!(fs::read(patcher.path()).unwrap(), patched);
        assert_eq!(fs::read(patcher.backup_path()).unwrap(), corrupt);
    }
    fs::write(patcher.backup_path(), original).unwrap();
    fs::write(patcher.path(), b"upgraded official bundle").unwrap();
    assert!(patcher.verify_restore().is_err());
    assert!(patcher.restore_validated().is_err());
    assert!(patcher.backup_path().exists());
    // Legacy hash-only metadata cannot authenticate the original backup.
    fs::write(patcher.state_path(), content_hash(patched)).unwrap();
    assert!(patcher.verify_restore().is_err());
    assert!(patcher.restore_validated().is_err());
    for path in [
        patcher.path().to_path_buf(),
        patcher.backup_path(),
        patcher.state_path(),
    ] {
        fs::remove_file(path).unwrap();
    }
    fs::remove_dir(root).unwrap();
}
