//! Owner-only access control for private files, without spawning a shell.
//!
//! This used to rewrite the DACL through PowerShell, once for the staging directory and
//! once for the file, on every private write. That cost about half a second per call and
//! failed outright under Constrained Language Mode or AppLocker, which block constructing
//! `System.Security.AccessControl` objects — making takeover impossible on hardened machines.
use std::{ffi::c_void, io, os::windows::ffi::OsStrExt, path::Path, ptr::null_mut};

#[repr(C)]
struct SidAndAttributes {
    sid: *mut c_void,
    attributes: u32,
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
    fn GetTokenInformation(
        token: *mut c_void,
        class: u32,
        info: *mut c_void,
        length: u32,
        needed: *mut u32,
    ) -> i32;
    fn ConvertSidToStringSidW(sid: *mut c_void, text: *mut *mut u16) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        sddl: *const u16,
        revision: u32,
        descriptor: *mut *mut c_void,
        size: *mut u32,
    ) -> i32;
    fn GetSecurityDescriptorDacl(
        descriptor: *mut c_void,
        present: *mut i32,
        dacl: *mut *mut c_void,
        defaulted: *mut i32,
    ) -> i32;
    fn SetNamedSecurityInfoW(
        name: *const u16,
        kind: u32,
        info: u32,
        owner: *mut c_void,
        group: *mut c_void,
        dacl: *mut c_void,
        sacl: *mut c_void,
    ) -> u32;
}
#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
}

const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_USER: u32 = 1;
const SDDL_REVISION_1: u32 = 1;
const SE_FILE_OBJECT: u32 = 1;
const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;

/// Frees memory the security APIs allocate with `LocalAlloc`.
struct Local(*mut c_void);
impl Drop for Local {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0) };
        }
    }
}

fn current_user_sid() -> io::Result<String> {
    unsafe {
        let mut token = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut needed = 0u32;
        GetTokenInformation(token, TOKEN_USER, null_mut(), 0, &mut needed);
        // u64 storage keeps the SID pointer inside the buffer correctly aligned.
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(1)];
        let ok = GetTokenInformation(
            token,
            TOKEN_USER,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 8) as u32,
            &mut needed,
        );
        CloseHandle(token);
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let user = &*(buffer.as_ptr() as *const SidAndAttributes);
        let mut text = null_mut();
        if ConvertSidToStringSidW(user.sid, &mut text) == 0 {
            return Err(io::Error::last_os_error());
        }
        let text = Local(text.cast());
        let start = text.0 as *const u16;
        let length = (0..).take_while(|&i| *start.add(i) != 0).count();
        Ok(String::from_utf16_lossy(std::slice::from_raw_parts(
            start, length,
        )))
    }
}

/// Replace the whole DACL with a single protected ACE granting the current user full
/// control. Protected means no inherited ACEs survive, and replacing (rather than
/// editing) means explicit grants already on the object are removed too.
pub(crate) fn restrict_to_current_user(path: &Path, directory: bool) -> io::Result<()> {
    let sid = current_user_sid()?;
    // FA = FILE_ALL_ACCESS. OICI makes the grant inherit into a directory's children,
    // so files created inside a private staging directory start private.
    let inherit = if directory { "OICI" } else { "" };
    let sddl: Vec<u16> = format!("D:P(A;{inherit};FA;;;{sid})")
        .encode_utf16()
        .chain([0])
        .collect();
    let name: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    unsafe {
        let mut descriptor = null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let descriptor = Local(descriptor);
        let (mut present, mut defaulted, mut dacl) = (0, 0, null_mut());
        if GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) == 0 {
            return Err(io::Error::last_os_error());
        }
        if present == 0 || dacl.is_null() {
            return Err(io::Error::other(
                "owner-only security descriptor has no DACL",
            ));
        }
        match SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl,
            null_mut(),
        ) {
            0 => Ok(()),
            code => Err(io::Error::from_raw_os_error(code as i32)),
        }
    }
}
