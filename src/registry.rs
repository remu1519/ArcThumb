//! Install / uninstall the shell extension into the Windows registry.
//!
//! We write to `HKCU\Software\Classes` rather than `HKLM` so that the
//! DLL can be registered without admin rights. This also means the
//! thumbnail provider is per-user — fine for Phase 1.
//!
//! Registry layout after a successful `register()`:
//!
//! ```text
//! HKCU\Software\Classes\CLSID\{CLSID_ARCTHUMB}
//!     (Default)                = "ArcThumb Thumbnail Provider"
//!     InprocServer32\
//!         (Default)            = "C:\path\to\arcthumb.dll"
//!         ThreadingModel       = "Apartment"
//!
//! HKCU\Software\Classes\.zip\ShellEx\{IID_IThumbnailProvider}
//!     (Default)                = "{CLSID_ARCTHUMB}"
//! ```

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;

use windows::core::PCWSTR;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use winreg::enums::*;
use winreg::RegKey;

/// Hardcoded string form of `CLSID_ARCTHUMB_PROVIDER` (defined in `com.rs`).
/// Kept in sync manually — CLSIDs never change once shipped.
const CLSID_STR: &str = "{0F4F5659-D383-4945-A534-01E1EED1D23F}";

/// IID of `IThumbnailProvider`. Explorer looks under
/// `.<ext>\ShellEx\<this IID>` to find the thumbnail handler.
const IID_ITHUMBNAILPROVIDER: &str = "{E357FCCD-A995-4576-B01F-234630154E96}";

/// File extensions that ArcThumb handles.
/// The `.cb?` variants are the comic-book archive conventions used by
/// tools like ComicRack — they are structurally identical to their
/// base format, just with a different extension.
const EXTENSIONS: &[&str] = &[
    ".zip", ".cbz",
    ".rar", ".cbr",
    ".7z", ".cb7",
];

/// Resolve our own DLL path by asking Windows "what module is this
/// function address inside of?" — avoids needing a `DllMain`.
fn get_dll_path() -> io::Result<String> {
    unsafe {
        let mut hmodule = HMODULE::default();
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(get_dll_path as *const () as *const u16),
            &mut hmodule,
        )
        .map_err(|e| io::Error::other(format!("GetModuleHandleExW failed: {e}")))?;

        let mut buf = vec![0u16; 32768];
        let len = GetModuleFileNameW(hmodule, &mut buf) as usize;
        if len == 0 {
            return Err(io::Error::other("GetModuleFileNameW returned 0"));
        }
        Ok(OsString::from_wide(&buf[..len])
            .to_string_lossy()
            .into_owned())
    }
}

pub fn register() -> io::Result<()> {
    let dll_path = get_dll_path()?;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    // --- HKCU\Software\Classes\CLSID\{CLSID} ---
    let (clsid_key, _) =
        hkcu.create_subkey(format!("Software\\Classes\\CLSID\\{CLSID_STR}"))?;
    clsid_key.set_value("", &"ArcThumb Thumbnail Provider")?;

    let (inproc, _) = clsid_key.create_subkey("InprocServer32")?;
    inproc.set_value("", &dll_path)?;
    inproc.set_value("ThreadingModel", &"Apartment")?;

    // --- HKCU\Software\Classes\<ext>\ShellEx\{IThumbnailProvider IID} ---
    for ext in EXTENSIONS {
        let path =
            format!("Software\\Classes\\{ext}\\ShellEx\\{IID_ITHUMBNAILPROVIDER}");
        let (key, _) = hkcu.create_subkey(&path)?;
        key.set_value("", &CLSID_STR.to_string())?;
    }

    Ok(())
}

pub fn unregister() -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    for ext in EXTENSIONS {
        let path =
            format!("Software\\Classes\\{ext}\\ShellEx\\{IID_ITHUMBNAILPROVIDER}");
        let _ = hkcu.delete_subkey_all(&path);
    }

    let _ = hkcu.delete_subkey_all(format!("Software\\Classes\\CLSID\\{CLSID_STR}"));

    Ok(())
}
