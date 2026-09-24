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

#[repr(C)]
struct WtsProcessInfo {
    session: u32,
    pid: u32,
    name: *mut u16,
    sid: *mut c_void,
}

#[link(name = "wtsapi32")]
extern "system" {
    fn WTSEnumerateProcessesW(
        server: *mut c_void,
        reserved: u32,
        version: u32,
        info: *mut *mut WtsProcessInfo,
        count: *mut u32,
    ) -> i32;
    fn WTSFreeMemory(memory: *mut c_void);
}
#[link(name = "kernel32")]
extern "system" {
    fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
    fn QueryFullProcessImageNameW(
        process: *mut c_void,
        flags: u32,
        name: *mut u16,
        size: *mut u32,
    ) -> i32;
    fn TerminateProcess(process: *mut c_void, code: u32) -> i32;
}
type EnumWindowsProc = unsafe extern "system" fn(window: *mut c_void, context: isize) -> i32;
#[link(name = "user32")]
extern "system" {
    fn EnumWindows(callback: EnumWindowsProc, context: isize) -> i32;
    fn GetWindowThreadProcessId(window: *mut c_void, pid: *mut u32) -> u32;
    fn IsWindowVisible(window: *mut c_void) -> i32;
    fn IsWindowEnabled(window: *mut c_void) -> i32;
    fn GetWindow(window: *mut c_void, command: u32) -> *mut c_void;
    fn PostMessageW(window: *mut c_void, message: u32, wparam: usize, lparam: isize) -> i32;
}

/// PIDs of processes named `image` in this client's own session.
///
/// Sessions come from `WTSEnumerateProcessesW`, which reports every process without
/// opening it. Asking per process (`ProcessIdToSessionId`) fails with access denied for
/// anything the client cannot open — an elevated Kiro included — and treating that as
/// "not ours" would let the client rewrite files under a live editor. Another session's
/// Kiro, on the other hand, belongs to someone else: it must neither block this user nor
/// be closed or killed by them.
pub(crate) fn session_processes(image: &str) -> io::Result<Vec<u32>> {
    let mut own = 0u32;
    if unsafe { ProcessIdToSessionId(std::process::id(), &mut own) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let (mut info, mut count) = (std::ptr::null_mut(), 0u32);
    if unsafe { WTSEnumerateProcessesW(std::ptr::null_mut(), 0, 1, &mut info, &mut count) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let entries = unsafe { std::slice::from_raw_parts(info, count as usize) };
    let pids = entries
        .iter()
        .filter(|entry| entry.session == own && !entry.name.is_null())
        .filter(|entry| {
            let name = unsafe {
                let length = (0..).take_while(|&i| *entry.name.add(i) != 0).count();
                String::from_utf16_lossy(std::slice::from_raw_parts(entry.name, length))
            };
            name.eq_ignore_ascii_case(image)
        })
        .map(|entry| entry.pid)
        .collect();
    unsafe { WTSFreeMemory(info.cast()) };
    Ok(pids)
}

/// A top-level window belonging to one of the given processes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TopLevelWindow {
    handle: *mut c_void,
    pub visible: bool,
    /// Owned windows are dialogs: an editor's save prompt is owned by its window.
    pub owned: bool,
    /// A window is disabled while a modal dialog it owns is open.
    pub enabled: bool,
}

pub(crate) fn top_level_windows(pids: &[u32]) -> Vec<TopLevelWindow> {
    struct Context<'a> {
        pids: &'a [u32],
        found: Vec<TopLevelWindow>,
    }
    unsafe extern "system" fn collect(window: *mut c_void, context: isize) -> i32 {
        let context = &mut *(context as *mut Context);
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, &mut pid);
        if context.pids.contains(&pid) {
            context.found.push(TopLevelWindow {
                handle: window,
                visible: IsWindowVisible(window) != 0,
                owned: !GetWindow(window, 4 /* GW_OWNER */).is_null(),
                enabled: IsWindowEnabled(window) != 0,
            });
        }
        1
    }
    let mut context = Context {
        pids,
        found: Vec::new(),
    };
    unsafe { EnumWindows(collect, &mut context as *mut Context as isize) };
    context.found
}

/// Ask a window to close, as its own close button would.
pub(crate) fn request_close(window: &TopLevelWindow) -> io::Result<()> {
    const WM_CLOSE: u32 = 0x0010;
    if unsafe { PostMessageW(window.handle, WM_CLOSE, 0, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Terminate `pid` only if it is still a process named `image`. The handle pins the
/// process object, so a PID recycled after enumeration cannot be hit instead.
pub(crate) fn terminate_if_named(pid: u32, image: &str) -> io::Result<()> {
    const PROCESS_TERMINATE: u32 = 0x0001;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    let raw = unsafe {
        OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        )
    };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    let handle = Handle(raw);
    let mut buffer = [0u16; 32_768];
    let mut size = buffer.len() as u32;
    if unsafe { QueryFullProcessImageNameW(handle.0, 0, buffer.as_mut_ptr(), &mut size) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let path = String::from_utf16_lossy(&buffer[..size as usize]);
    let name = path.rsplit(['\\', '/']).next().unwrap_or_default();
    if !name.eq_ignore_ascii_case(image) {
        return Ok(());
    }
    if unsafe { TerminateProcess(handle.0, 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
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
    fn sessions_include_this_process_and_exclude_nothing_it_cannot_open() {
        let own = std::env::current_exe().unwrap();
        let image = own.file_name().unwrap().to_string_lossy().into_owned();
        let pids = super::session_processes(&image).unwrap();
        assert!(pids.contains(&std::process::id()));
        // System (PID 4) lives in session 0, which is never this test's session.
        assert!(!super::session_processes("System").unwrap().contains(&4));
    }

    #[test]
    fn terminate_refuses_a_process_with_another_name() {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 5 127.0.0.1 >NUL"])
            .spawn()
            .unwrap();
        super::terminate_if_named(child.id(), "Kiro.exe").unwrap();
        assert!(
            child.try_wait().unwrap().is_none(),
            "a non-Kiro process was killed"
        );
        super::terminate_if_named(child.id(), "cmd.exe").unwrap();
        assert!(child.wait().is_ok());
    }

    #[test]
    fn reads_current_test_process_without_shell() {
        let pid = std::process::id();
        assert!(super::enumerate().unwrap().iter().any(|p| p.0 == pid));
        assert!(super::working_set_mb(pid).unwrap() > 0);
    }
}
