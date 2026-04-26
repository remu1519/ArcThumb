//! Find `arcthumb.dll` relative to this config exe.
//!
//! Lookup order:
//! 1. Next to the current executable (the expected deployment layout).
//! 2. The path in `HKLM\Software\Classes\CLSID\{CLSID}\InprocServer32\(Default)`
//!    (per-machine install).
//! 3. The path in `HKCU\...` for the same key (per-user install). HKLM
//!    wins when both exist because that matches COM's lookup order
//!    under elevated callers.
//! 4. Otherwise: `Err` with a message the UI shows to the user.

use std::path::PathBuf;

use arcthumb::registry::{self, Scope};

pub fn resolve_dll_path() -> Result<PathBuf, String> {
    if let Some(p) = exe_neighbour_dll().filter(|p| p.is_file()) {
        return Ok(p);
    }
    if let Some(p) = registry::read_registered_dll_path(Scope::ALL).filter(|p| p.is_file()) {
        return Ok(p);
    }
    Err("arcthumb.dll not found next to the exe or in the registry".into())
}

/// `arcthumb.dll` placed next to the current executable. Returns
/// the candidate path even if the file does not actually exist —
/// the existence check is the caller's job.
fn exe_neighbour_dll() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    Some(dir.join("arcthumb.dll"))
}
