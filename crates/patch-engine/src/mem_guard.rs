//! Kiro-only memory sampling and optional maintenance.
//!
//! Windows working-set trimming does not guarantee a fixed reduction; pages can
//! be loaded again immediately. A single snapshot cannot establish orphan
//! ownership, so unrelated processes are never selected for cleanup.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::process::Command;
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessCategory {
    MainIde,
    RendererOrGpu,
    ExtensionHost,
    AgentSubprocess,
    LanguageServer,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessMemoryInfo {
    pub pid: u32,
    pub parent_pid: u32,
    pub name: String,
    pub memory_mb: u64,
    pub category: ProcessCategory,
    pub is_orphan: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemorySnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub total_memory_mb: u64,
    pub ide_memory_mb: u64,
    pub agent_memory_mb: u64,
    pub total_process_count: usize,
    pub agent_process_count: usize,
    pub orphan_process_count: usize,
    pub orphan_memory_mb: u64,
    pub peak_process_mb: u64,
    pub processes: Vec<ProcessMemoryInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrimResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub success_count: usize,
    pub failed_count: usize,
    pub initial_memory_mb: u64,
    pub current_memory_mb: u64,
    pub released_memory_mb: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheCleanResult {
    pub cleaned_files_count: usize,
    pub freed_bytes: u64,
    pub directories_scanned: Vec<String>,
}

#[allow(dead_code)] // ponytail: config struct for future auto-trim integration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemGuardConfig {
    pub auto_trim_enabled: bool,
    pub total_threshold_mb: u64,
    pub agent_threshold_mb: u64,
    pub auto_clean_cache_days: u64,
}

impl Default for MemGuardConfig {
    fn default() -> Self {
        Self {
            auto_trim_enabled: true,
            total_threshold_mb: 2500, // 2.5 GB
            agent_threshold_mb: 800,  // 800 MB
            auto_clean_cache_days: 7,
        }
    }
}

pub struct MemoryGuard;

impl MemoryGuard {
    /// Classify a process based on binary name and parentage.
    pub fn classify_process(name: &str) -> ProcessCategory {
        let lower = name.to_ascii_lowercase();
        if Self::is_kiro_root(name) {
            ProcessCategory::MainIde
        } else if lower == "node.exe"
            || lower == "node"
            || lower.starts_with("python")
            || lower == "uv.exe"
            || lower == "uvx.exe"
            || lower == "cmd.exe"
            || lower == "powershell.exe"
            || lower == "pwsh.exe"
            || lower == "bash"
        {
            ProcessCategory::AgentSubprocess
        } else if lower.contains("tsserver") || lower.contains("language") {
            ProcessCategory::LanguageServer
        } else {
            ProcessCategory::Other
        }
    }

    /// Sample the current process tree and gather memory metrics.
    pub fn sample_memory() -> MemorySnapshot {
        match Self::collect_processes() {
            Ok(processes) => Self::summarize(processes),
            Err(error) => MemorySnapshot {
                error: Some(error.to_string()),
                ..Default::default()
            },
        }
    }

    fn is_kiro_root(name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        matches!(lower.as_str(), "kiro" | "kiro.exe")
            || lower.ends_with("/kiro.app/contents/macos/kiro")
            || lower.ends_with("/kiro.app/contents/macos/electron")
            || lower.ends_with("/kiro")
    }

    fn summarize(entries: Vec<ProcessMemoryInfo>) -> MemorySnapshot {
        let mut owned: HashSet<u32> = entries
            .iter()
            .filter(|p| Self::is_kiro_root(&p.name))
            .map(|p| p.pid)
            .collect();
        loop {
            let before = owned.len();
            for p in &entries {
                if owned.contains(&p.parent_pid) {
                    owned.insert(p.pid);
                }
            }
            if before == owned.len() {
                break;
            }
        }
        let mut snapshot = MemorySnapshot::default();
        for mut p in entries.into_iter().filter(|p| owned.contains(&p.pid)) {
            // A single snapshot cannot prove ownership of an orphan. Never guess.
            p.is_orphan = false;
            p.category = Self::classify_process(&p.name);
            if p.category == ProcessCategory::Other {
                p.category = ProcessCategory::AgentSubprocess;
            }
            snapshot.total_memory_mb += p.memory_mb;
            snapshot.peak_process_mb = snapshot.peak_process_mb.max(p.memory_mb);
            if p.category == ProcessCategory::MainIde {
                snapshot.ide_memory_mb += p.memory_mb;
            } else {
                snapshot.agent_memory_mb += p.memory_mb;
                snapshot.agent_process_count += 1;
            }
            snapshot.processes.push(p);
        }
        snapshot.total_process_count = snapshot.processes.len();
        snapshot
    }

    /// Compress the working set of all target processes (or all Kiro & agent processes if None).
    ///
    /// Calls Win32 `EmptyWorkingSet`, immediately freeing inactive memory to standby pages.
    pub fn trim_working_set(target_pids: Option<&[u32]>) -> TrimResult {
        let initial_snapshot = Self::sample_memory();
        let pids_to_trim: Vec<u32> = match target_pids {
            Some(pids) => pids.to_vec(),
            None => initial_snapshot.processes.iter().map(|p| p.pid).collect(),
        };

        #[cfg(target_os = "windows")]
        let (success, fail) = {
            let success = pids_to_trim
                .iter()
                .filter(|&&pid| Self::empty_working_set_win32(pid))
                .count();
            (success, pids_to_trim.len() - success)
        };
        // There is no supported remote EmptyWorkingSet equivalent on macOS/Linux.
        // Do not report successful optimization when no operation occurred.
        #[cfg(not(target_os = "windows"))]
        let (success, fail) = (0, pids_to_trim.len());

        let after_snapshot = Self::sample_memory();
        Self::trim_result(initial_snapshot, after_snapshot, success, fail)
    }

    fn trim_result(
        initial_snapshot: MemorySnapshot,
        after_snapshot: MemorySnapshot,
        success: usize,
        fail: usize,
    ) -> TrimResult {
        let error = initial_snapshot
            .error
            .clone()
            .or(after_snapshot.error.clone())
            .or_else(|| {
                (success == 0 && fail > 0).then(|| "No working sets could be trimmed".to_string())
            });
        let released = if error.is_none() {
            initial_snapshot
                .total_memory_mb
                .saturating_sub(after_snapshot.total_memory_mb)
        } else {
            0
        };

        TrimResult {
            error,
            success_count: success,
            failed_count: fail,
            initial_memory_mb: initial_snapshot.total_memory_mb,
            current_memory_mb: after_snapshot.total_memory_mb,
            released_memory_mb: released,
        }
    }

    /// Clean IDE temporary cache directories (Cache, CachedData, Code Cache, logs).
    pub fn clean_cache_folders(
        custom_data_dir: Option<&Path>,
        max_age_days: u64,
    ) -> CacheCleanResult {
        let base_dirs = match custom_data_dir {
            Some(d) => vec![d.to_path_buf()],
            None => Self::get_default_cache_roots(),
        };

        let max_age = Duration::from_secs(max_age_days * 86_400);
        let now = SystemTime::now();

        let mut cleaned_count = 0;
        let mut freed_bytes = 0;
        let mut scanned_dirs = Vec::new();

        for root in base_dirs {
            let targets = ["Cache", "CachedData", "Code Cache", "logs", "Crashpad"];
            for sub in &targets {
                let target_path = root.join(sub);
                if target_path.exists() {
                    scanned_dirs.push(target_path.to_string_lossy().to_string());
                    if let Ok(entries) = std::fs::read_dir(&target_path) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if let Ok(metadata) = entry.metadata() {
                                if let Ok(modified) = metadata.modified() {
                                    if let Ok(age) = now.duration_since(modified) {
                                        if age > max_age {
                                            let size = metadata.len();
                                            if metadata.is_file() {
                                                if std::fs::remove_file(&path).is_ok() {
                                                    cleaned_count += 1;
                                                    freed_bytes += size;
                                                }
                                            } else if metadata.is_dir()
                                                && std::fs::remove_dir_all(&path).is_ok()
                                            {
                                                cleaned_count += 1;
                                                freed_bytes += size;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        CacheCleanResult {
            cleaned_files_count: cleaned_count,
            freed_bytes,
            directories_scanned: scanned_dirs,
        }
    }

    // ---------------------------------------------------------------------------
    // Internal Platform Implementations
    // ---------------------------------------------------------------------------

    fn collect_processes() -> std::io::Result<Vec<ProcessMemoryInfo>> {
        #[cfg(windows)]
        {
            let entries = crate::windows_process::enumerate()?
                .into_iter()
                .map(|(pid, parent_pid, name)| ProcessMemoryInfo {
                    pid,
                    parent_pid,
                    name,
                    memory_mb: 0,
                    category: ProcessCategory::Other,
                    is_orphan: false,
                })
                .collect();
            let mut owned = Self::summarize(entries).processes;
            let mut sampled = Vec::with_capacity(owned.len());
            for mut p in owned.drain(..) {
                match crate::windows_process::working_set_mb(p.pid) {
                    Ok(memory) => p.memory_mb = memory,
                    Err(error) => {
                        // A child may exit between enumeration and sampling. Only
                        // ignore confirmed exits; access failures remain visible.
                        if crate::windows_process::enumerate()?
                            .iter()
                            .any(|entry| entry.0 == p.pid)
                        {
                            return Err(error);
                        }
                        continue;
                    }
                }
                sampled.push(p);
            }
            Ok(sampled)
        }
        #[cfg(not(windows))]
        {
            let out = Command::new("ps")
                .args(["-eo", "pid=,ppid=,rss=,comm="])
                .output()?;
            if !out.status.success() {
                return Err(std::io::Error::other("Process sampling failed"));
            }
            let text = String::from_utf8_lossy(&out.stdout);
            let mut entries = Vec::new();
            for line in text.lines().filter(|line| !line.trim().is_empty()) {
                // ps pads numeric columns, so consume those separately and preserve spaces in comm.
                let mut rest = line.trim();
                let mut numbers = Vec::new();
                for _ in 0..3 {
                    let end = rest
                        .find(char::is_whitespace)
                        .ok_or_else(|| std::io::Error::other("Malformed ps output"))?;
                    numbers.push(rest[..end].parse::<u64>().map_err(std::io::Error::other)?);
                    rest = rest[end..].trim_start();
                }
                entries.push(ProcessMemoryInfo {
                    pid: numbers[0] as u32,
                    parent_pid: numbers[1] as u32,
                    memory_mb: numbers[2] / 1024,
                    name: rest.to_string(),
                    category: ProcessCategory::Other,
                    is_orphan: false,
                });
            }
            Ok(entries)
        }
    }

    #[cfg(target_os = "windows")]
    fn empty_working_set_win32(pid: u32) -> bool {
        #[link(name = "kernel32")]
        #[link(name = "psapi")]
        extern "system" {
            fn OpenProcess(
                dwDesiredAccess: u32,
                bInheritHandle: i32,
                dwProcessId: u32,
            ) -> *mut std::ffi::c_void;
            fn CloseHandle(hObject: *mut std::ffi::c_void) -> i32;
            fn EmptyWorkingSet(hProcess: *mut std::ffi::c_void) -> i32;
        }

        const PROCESS_QUERY_INFORMATION: u32 = 0x0400;
        const PROCESS_SET_QUOTA: u32 = 0x0100;

        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_SET_QUOTA, 0, pid);
            if handle.is_null() {
                return false;
            }
            let success = EmptyWorkingSet(handle) != 0;
            CloseHandle(handle);
            success
        }
    }

    fn get_default_cache_roots() -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let p = PathBuf::from(appdata);
            dirs.push(p.join("Kiro"));
        }
        if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            let p = PathBuf::from(home);
            if cfg!(target_os = "macos") {
                dirs.push(p.join("Library/Application Support/Kiro"));
            } else {
                dirs.push(p.join(".config").join("Kiro"));
            }
        }
        dirs
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    fn process(pid: u32, parent_pid: u32, name: &str) -> ProcessMemoryInfo {
        ProcessMemoryInfo {
            pid,
            parent_pid,
            name: name.into(),
            memory_mb: 100,
            category: ProcessCategory::Other,
            is_orphan: false,
        }
    }
    #[test]
    fn unrelated_editors_and_superkiro_are_not_kiro() {
        let snapshot = MemoryGuard::summarize(vec![
            process(1, 0, "Superkiro.exe"),
            process(2, 0, "Code.exe"),
            process(3, 0, "Cursor.exe"),
            process(4, 0, "node.exe"),
        ]);
        assert_eq!(snapshot.total_memory_mb, 0);
        assert_eq!(snapshot.total_process_count, 0);
    }
    #[test]
    fn only_kiro_process_tree_is_counted_even_out_of_order() {
        let snapshot = MemoryGuard::summarize(vec![
            process(3, 2, "node.exe"),
            process(2, 1, "renderer.exe"),
            process(1, 0, "Kiro.exe"),
            process(4, 0, "node.exe"),
        ]);
        assert_eq!(snapshot.total_memory_mb, 300);
        assert_eq!(snapshot.total_process_count, 3);
        assert_eq!(snapshot.orphan_process_count, 0);
    }
    #[test]
    fn failed_post_trim_sample_never_claims_released_memory() {
        let before = MemorySnapshot {
            total_memory_mb: 1000,
            ..Default::default()
        };
        let after = MemorySnapshot {
            error: Some("sample failed".into()),
            ..Default::default()
        };
        let result = MemoryGuard::trim_result(before, after, 1, 0);
        assert!(result.error.is_some());
        assert_eq!(result.released_memory_mb, 0);
    }
    #[test]
    fn failed_trim_never_claims_success_or_released_memory() {
        let before = MemorySnapshot {
            total_memory_mb: 1000,
            ..Default::default()
        };
        let after = MemorySnapshot {
            total_memory_mb: 200,
            ..Default::default()
        };
        let result = MemoryGuard::trim_result(before, after, 0, 1);
        assert!(result.error.is_some());
        assert_eq!(result.released_memory_mb, 0);
    }
    #[test]
    fn cache_roots_never_include_other_editors() {
        for root in MemoryGuard::get_default_cache_roots() {
            assert_eq!(root.file_name().unwrap(), "Kiro");
        }
    }
}
