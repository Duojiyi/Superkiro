//! Read-only Win32 process enumeration, independent of PATH, console and locale.
use std::{ffi::c_void, io, mem::size_of};

#[repr(C)]
struct ProcessEntry {
    size: u32,
    usage: u32,
    pid: u32,
    heap: usize,
    module: u32,
    threads: u32,
    parent: u32,
    priority: i32,
    flags: u32,
    name: [u16; 260],
}

#[repr(C)]
#[derive(Default)]
struct MemoryCounters {
    size: u32,
    faults: u32,
    peak_working_set: usize,
    working_set: usize,
    quota_peak_paged: usize,
    quota_paged: usize,
    quota_peak_nonpaged: usize,
    quota_nonpaged: usize,
    pagefile: usize,
    peak_pagefile: usize,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut c_void;
    fn Process32FirstW(handle: *mut c_void, entry: *mut ProcessEntry) -> i32;
    fn Process32NextW(handle: *mut c_void, entry: *mut ProcessEntry) -> i32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
    fn CloseHandle(handle: *mut c_void) -> i32;
}
#[link(name = "psapi")]
extern "system" {
    fn GetProcessMemoryInfo(handle: *mut c_void, counters: *mut MemoryCounters, size: u32) -> i32;
}

struct Handle(*mut c_void);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

pub(crate) fn enumerate() -> io::Result<Vec<(u32, u32, String)>> {
    let raw = unsafe { CreateToolhelp32Snapshot(2, 0) };
    if raw as isize == -1 {
        return Err(io::Error::last_os_error());
    }
    let handle = Handle(raw);
    let mut entry: ProcessEntry = unsafe { std::mem::zeroed() };
    entry.size = size_of::<ProcessEntry>() as u32;
    let mut result = Vec::new();
    let mut ok = unsafe { Process32FirstW(handle.0, &mut entry) };
    while ok != 0 {
        let end = entry
            .name
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(entry.name.len());
        result.push((
            entry.pid,
            entry.parent,
            String::from_utf16_lossy(&entry.name[..end]),
        ));
        ok = unsafe { Process32NextW(handle.0, &mut entry) };
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(18) {
        // ERROR_NO_MORE_FILES
        return Err(error);
    }
    Ok(result)
}

pub(crate) fn working_set_mb(pid: u32) -> io::Result<u64> {
    let raw = unsafe { OpenProcess(0x0400 | 0x0010, 0, pid) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    let handle = Handle(raw);
    let mut counters = MemoryCounters {
        size: size_of::<MemoryCounters>() as u32,
        ..Default::default()
    };
    if unsafe { GetProcessMemoryInfo(handle.0, &mut counters, size_of::<MemoryCounters>() as u32) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((counters.working_set / 1024 / 1024) as u64)
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_current_test_process_without_shell() {
        let pid = std::process::id();
        assert!(super::enumerate().unwrap().iter().any(|p| p.0 == pid));
        assert!(super::working_set_mb(pid).unwrap() > 0);
    }
}
