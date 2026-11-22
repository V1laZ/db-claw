//! Windows security helpers for the control pipe.
//!
//! A named pipe created with default security is readable by Everyone and
//! anonymous users. The control pipe instead gets a DACL with a single entry:
//! full access for the current user. Clients additionally check that the
//! process serving the pipe runs as the same user, so a process that grabbed
//! the pipe name first (pipe squatting) can't impersonate DB Claw.

use std::ffi::c_void;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce, GetTokenInformation,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// `ACCESS_ALLOWED_ACE_TYPE`
const ACCESS_ALLOWED: u8 = 0;

/// Closes a handle on drop.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseHandle(self.0) };
        }
    }
}

fn sid_to_string(sid: PSID) -> io::Result<String> {
    let mut wide: *mut u16 = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut wide) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let len = (0..).take_while(|&i| unsafe { *wide.add(i) } != 0).count();
    let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(wide, len) });
    unsafe { LocalFree(wide as *mut c_void) };
    Ok(s)
}

fn token_user_sid(token: HANDLE) -> io::Result<String> {
    let mut len = 0u32;
    unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64 storage keeps TOKEN_USER suitably aligned.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    if unsafe { GetTokenInformation(token, TokenUser, buf.as_mut_ptr() as *mut c_void, len, &mut len) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    sid_to_string(user.User.Sid)
}

fn process_user_sid(process: HANDLE) -> io::Result<String> {
    let mut token: HANDLE = null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = OwnedHandle(token);
    token_user_sid(token.0)
}

/// SID of the user running this process, e.g. `S-1-5-21-…-1001`.
pub fn current_user_sid() -> io::Result<String> {
    process_user_sid(unsafe { GetCurrentProcess() })
}

/// A self-relative security descriptor granting full access to the current
/// user only (`D:P(A;;GA;;;<sid>)`: protected DACL, no inherited entries).
pub struct OwnerOnly {
    sd: PSECURITY_DESCRIPTOR,
}

// The descriptor is immutable after creation and only read by the OS.
unsafe impl Send for OwnerOnly {}
unsafe impl Sync for OwnerOnly {}

impl OwnerOnly {
    pub fn new() -> io::Result<OwnerOnly> {
        let sddl = format!("D:P(A;;GA;;;{})", current_user_sid()?);
        let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(wide.as_ptr(), SDDL_REVISION_1, &mut sd, null_mut())
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(OwnerOnly { sd })
    }

    /// `SECURITY_ATTRIBUTES` pointing at this descriptor (valid while `self` lives).
    pub fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.sd,
            bInheritHandle: 0,
        }
    }
}

impl Drop for OwnerOnly {
    fn drop(&mut self) {
        unsafe { LocalFree(self.sd) };
    }
}

/// Is the process serving this pipe (client handle) running as the current
/// user? `false` also when its owner can't be determined.
pub fn pipe_server_is_current_user(pipe: &impl AsRawHandle) -> io::Result<bool> {
    let pipe = pipe.as_raw_handle() as HANDLE;
    let mut pid = 0u32;
    if unsafe { GetNamedPipeServerProcessId(pipe, &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Ok(false);
    }
    let process = OwnedHandle(process);
    match process_user_sid(process.0) {
        Ok(sid) => Ok(sid == current_user_sid()?),
        Err(_) => Ok(false),
    }
}

/// The DACL of a kernel object as `(allowed, sid)` pairs — used by tests to
/// verify the pipe is owner-only.
pub fn dacl_entries(object: &impl AsRawHandle) -> io::Result<Vec<(bool, String)>> {
    let handle = object.as_raw_handle() as HANDLE;
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    let err = unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    if err != 0 {
        return Err(io::Error::from_raw_os_error(err as i32));
    }
    let mut out = Vec::new();
    if !dacl.is_null() {
        let count = unsafe { (*dacl).AceCount };
        for i in 0..count as u32 {
            let mut ace: *mut c_void = null_mut();
            if unsafe { GetAce(dacl, i, &mut ace) } == 0 {
                continue;
            }
            let header = unsafe { &*(ace as *const ACE_HEADER) };
            let allowed = header.AceType == ACCESS_ALLOWED;
            // Allowed and denied ACEs share the layout: header, mask, SID.
            let sid = unsafe { &(*(ace as *const ACCESS_ALLOWED_ACE)).SidStart } as *const u32 as PSID;
            out.push((allowed, sid_to_string(sid)?));
        }
    }
    unsafe { LocalFree(sd) };
    Ok(out)
}
