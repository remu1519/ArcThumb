//! Detect whether the current process runs elevated.
//!
//! Used by registry installation paths to pick between HKLM (admin
//! install, machine-wide) and HKCU (per-user install). Without this,
//! Explorer running at High Mandatory Integrity in Windows Sandbox
//! and other elevated shells silently ignores HKCU CLSIDs (Microsoft's
//! defence against low-integrity COM hijacking) and our DLL never
//! loads.

use crate::registry::Scope;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// `true` if the current process holds an elevated token (i.e. the
/// user accepted the UAC prompt or is logged in as a built-in
/// administrator without UAC).
///
/// On any Win32 failure we fall back to `false` — treating the
/// process as non-elevated is the safe default since per-user
/// registration cannot fail with an AccessDenied at the registry layer.
pub fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }

        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned: u32 = 0;
        let result = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );

        let _ = CloseHandle(token);

        result.is_ok() && elevation.TokenIsElevated != 0
    }
}

/// Pick the registry scope automatically based on the current process's
/// elevation. Elevated processes write to HKLM (machine-wide); regular
/// users write to HKCU.
pub fn current_scope() -> Scope {
    if is_elevated() {
        Scope::PerMachine
    } else {
        Scope::PerUser
    }
}
