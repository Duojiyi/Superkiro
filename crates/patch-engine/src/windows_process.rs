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
    fn GetProcessTimes(
        process: *mut c_void,
        creation: *mut u64,
        exit: *mut u64,
        kernel: *mut u64,
        user: *mut u64,
    ) -> i32;
}
type EnumWindowsProc = unsafe extern "system" fn(window: *mut c_void, context: isize) -> i32;
#[link(name = "user32")]
extern "system" {
    fn EnumWindows(callback: EnumWindowsProc, context: isize) -> i32;
    fn GetWindowThreadProcessId(window: *mut c_void, pid: *mut u32) -> u32;
    fn IsWindowVisible(window: *mut c_void) -> i32;
    /// A window whose thread has not pumped messages for about five seconds. Windows then
    /// hides it and paints a DWM-owned "ghost" in its place, so `IsWindowVisible` reports
    /// false for a window the user can still see and whose buffers are still unsaved.
    fn IsHungAppWindow(window: *mut c_void) -> i32;
    fn IsWindowEnabled(window: *mut c_void) -> i32;
    fn GetWindow(window: *mut c_void, command: u32) -> *mut c_void;
    fn PostMessageW(window: *mut c_void, message: u32, wparam: usize, lparam: isize) -> i32;
}

/// A process of this user, as the file-safety gate and the stop policy see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UserProcess {
    pub pid: u32,
    /// In the client's own session, the only one whose windows it can ask to close.
    pub own_session: bool,
}

/// Processes named `image` that belong to this user, in any of the user's sessions.
///
/// Owners come from `WTSEnumerateProcessesW`, which reports every process without opening
/// it. Asking per process fails with access denied for anything the client cannot open, an
/// elevated Kiro included, and treating that as "not ours" would let the client rewrite
/// files under a live editor; an owner that is not reported counts as this user for the
/// same reason. The same user's Kiro in a disconnected session still writes this user's
/// settings and token, so it counts. Another user's Kiro never touches them, so it neither
/// blocks this user nor is closed or ended by them.
pub(crate) fn user_processes(image: &str) -> io::Result<Vec<UserProcess>> {
    let mut own_session = 0u32;
    if unsafe { ProcessIdToSessionId(std::process::id(), &mut own_session) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let me = crate::windows_security::current_user_sid()?;
    let (mut info, mut count) = (std::ptr::null_mut(), 0u32);
    if unsafe { WTSEnumerateProcessesW(std::ptr::null_mut(), 0, 1, &mut info, &mut count) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let entries = unsafe { std::slice::from_raw_parts(info, count as usize) };
    let found = entries
        .iter()
        .filter(|entry| !entry.name.is_null())
        .filter(|entry| {
            let name = unsafe {
                let length = (0..).take_while(|&i| *entry.name.add(i) != 0).count();
                String::from_utf16_lossy(std::slice::from_raw_parts(entry.name, length))
            };
            name.eq_ignore_ascii_case(image)
        })
        .filter(|entry| {
            entry.sid.is_null()
                || unsafe { crate::windows_security::sid_to_string(entry.sid) }
                    .map_or(true, |sid| sid == me)
        })
        .map(|entry| UserProcess {
            pid: entry.pid,
            own_session: entry.session == own_session,
        })
        .collect();
    unsafe { WTSFreeMemory(info.cast()) };
    Ok(found)
}

/// Every process's parent, from one snapshot.
pub(crate) fn parents() -> io::Result<std::collections::HashMap<u32, u32>> {
    Ok(enumerate()?
        .into_iter()
        .map(|(pid, parent, _)| (pid, parent))
        .collect())
}

/// When `pid` was created. With the PID it names one process for good: a PID is reused,
/// the pair is not.
pub(crate) fn creation_time(pid: u32) -> io::Result<u64> {
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    let handle = Handle(raw);
    handle_creation_time(&handle)
}

fn handle_creation_time(handle: &Handle) -> io::Result<u64> {
    let (mut created, mut exited, mut kernel, mut user) = (0u64, 0u64, 0u64, 0u64);
    if unsafe { GetProcessTimes(handle.0, &mut created, &mut exited, &mut kernel, &mut user) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(created)
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
                // A hung window counts as on screen: the user sees its ghost, and its
                // unsaved work is exactly what must not be discarded.
                visible: IsWindowVisible(window) != 0 || IsHungAppWindow(window) != 0,
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

/// Terminate `pid` only if it is still the process named `image` created at `created`.
/// Both are checked through the handle that is then used to terminate, which pins the
/// process object, so a PID recycled after it was observed cannot be hit instead.
pub(crate) fn terminate_if_same(pid: u32, image: &str, created: u64) -> io::Result<()> {
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
    if !name.eq_ignore_ascii_case(image) || handle_creation_time(&handle)? != created {
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
    fn user_processes_include_this_process_and_exclude_other_users() {
        let own = std::env::current_exe().unwrap();
        let image = own.file_name().unwrap().to_string_lossy().into_owned();
        let found = super::user_processes(&image).unwrap();
        assert!(found.contains(&super::UserProcess {
            pid: std::process::id(),
            own_session: true
        }));
        // lsass runs as LocalSystem: another user's process.
        assert!(super::user_processes("lsass.exe").unwrap().is_empty());
        // System (PID 4) has no reported owner, which counts as this user: for the gate,
        // over-counting is the safe mistake.
        assert!(super::user_processes("System")
            .unwrap()
            .iter()
            .any(|p| p.pid == 4 && !p.own_session));
    }

    #[test]
    fn terminate_refuses_another_name_and_another_creation_time() {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 5 127.0.0.1 >NUL"])
            .spawn()
            .unwrap();
        let created = super::creation_time(child.id()).unwrap();
        super::terminate_if_same(child.id(), "Kiro.exe", created).unwrap();
        super::terminate_if_same(child.id(), "cmd.exe", created + 1).unwrap();
        assert!(
            child.try_wait().unwrap().is_none(),
            "a process that was not the one observed was killed"
        );
        super::terminate_if_same(child.id(), "cmd.exe", created).unwrap();
        assert!(child.wait().is_ok());
    }

    #[test]
    fn reads_current_test_process_without_shell() {
        let pid = std::process::id();
        assert!(super::enumerate().unwrap().iter().any(|p| p.0 == pid));
        assert!(super::working_set_mb(pid).unwrap() > 0);
    }
}
