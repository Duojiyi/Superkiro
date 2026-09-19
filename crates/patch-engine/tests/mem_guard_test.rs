use patch_engine::mem_guard::{MemoryGuard, ProcessCategory};
use std::fs::File;

#[test]
fn test_classify_process() {
    assert_eq!(
        MemoryGuard::classify_process("Kiro.exe"),
        ProcessCategory::MainIde
    );
    assert_eq!(
        MemoryGuard::classify_process("cursor.exe"),
        ProcessCategory::MainIde
    );
    assert_eq!(
        MemoryGuard::classify_process("Code.exe"),
        ProcessCategory::MainIde
    );
    assert_eq!(
        MemoryGuard::classify_process("node.exe"),
        ProcessCategory::AgentSubprocess
    );
    assert_eq!(
        MemoryGuard::classify_process("python.exe"),
        ProcessCategory::AgentSubprocess
    );
    assert_eq!(
        MemoryGuard::classify_process("cmd.exe"),
        ProcessCategory::AgentSubprocess
    );
    assert_eq!(
        MemoryGuard::classify_process("powershell.exe"),
        ProcessCategory::AgentSubprocess
    );
    assert_eq!(
        MemoryGuard::classify_process("tsserver.js"),
        ProcessCategory::LanguageServer
    );
    assert_eq!(
        MemoryGuard::classify_process("notepad.exe"),
        ProcessCategory::Other
    );
}

#[test]
fn test_sample_memory_snapshot() {
    let snapshot = MemoryGuard::sample_memory();
    // System should have running processes
    println!(
        "Sampled memory: total_process_count={}",
        snapshot.total_process_count
    );
    assert!(snapshot.processes.len() == snapshot.total_process_count);
}

#[test]
fn test_trim_working_set_on_self() {
    let my_pid = std::process::id();
    let res = MemoryGuard::trim_working_set(Some(&[my_pid]));
    println!("Trim result on self PID {}: {:?}", my_pid, res);
    #[cfg(target_os = "windows")]
    {
        assert_eq!(res.success_count, 1);
        assert_eq!(res.failed_count, 0);
    }
}

#[test]
fn test_clean_cache_folders() {
    let temp_dir = std::env::temp_dir().join(format!("kiro_test_cache_{}", std::process::id()));
    let cache_dir = temp_dir.join("Cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    let old_file = cache_dir.join("old_cache.bin");
    File::create(&old_file).unwrap();

    // Clean with max_age_days = 0 should remove old files
    let res = MemoryGuard::clean_cache_folders(Some(&temp_dir), 0);
    assert!(res.cleaned_files_count >= 1);
    assert!(!old_file.exists());

    let _ = std::fs::remove_dir_all(&temp_dir);
}
