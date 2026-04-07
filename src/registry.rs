//! Install / uninstall the shell extension into the Windows registry.
//!
//! We write to `HKCU\Software\Classes` rather than `HKLM` so that the
//! DLL can be registered without admin rights. This also means the
//! thumbnail provider is per-user.
//!
//! Registry layout after a successful full install:
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
//!
//! ## Two callers
//!
//! Both the DLL's `DllRegisterServer` and the separate `arcthumb-config`
//! binary share this module. The DLL uses `register()` / `unregister()`
//! which auto-detect their own path via `GetModuleHandleExW`. The config
//! exe uses the individual primitives (`register_clsid(path)`,
//! `register_extension(ext)`, `is_extension_registered(ext)`, …) so it
//! can install selectively and reflect current state in the GUI.

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use winreg::enums::*;
use winreg::RegKey;

/// String form of the ArcThumb thumbnail provider CLSID (defined in
/// `com.rs`). **Never change** — baked into users' registries.
pub const CLSID_STR: &str = "{0F4F5659-D383-4945-A534-01E1EED1D23F}";

/// Standard IID of `IThumbnailProvider`. Explorer looks under
/// `.<ext>\ShellEx\<this IID>` to find the thumbnail handler.
pub const IID_ITHUMBNAILPROVIDER: &str = "{E357FCCD-A995-4576-B01F-234630154E96}";

/// File extensions that ArcThumb handles.
/// The `.cb?` variants are the comic-book archive conventions used by
/// tools like ComicRack — structurally identical to their base format,
/// just with a different extension. `.epub` rides the ZIP backend
/// with an EPUB-aware fast path that consults the OPF metadata for
/// the cover image.
pub const EXTENSIONS: &[&str] = &[
    ".zip", ".cbz",
    ".rar", ".cbr",
    ".7z", ".cb7",
    ".cbt",
    ".epub",
];

// =============================================================================
// Path constants and helpers
// =============================================================================

/// Production parent key for shell extension registrations. We write
/// under `HKCU\Software\Classes` (per-user, no admin needed) rather
/// than `HKLM\Software\Classes` (machine-wide, requires elevation).
const CLASSES_BASE: &str = "Software\\Classes";

/// Production parent key for COM CLSIDs. Sits under `Software\Classes`
/// but we name it explicitly so tests can swap in a fake root without
/// touching the user's real shell registrations.
const CLSID_BASE: &str = "Software\\Classes\\CLSID";

/// Build the registry sub-path for a given extension's ShellEx slot,
/// rooted at `classes_base`. Production callers pass `CLASSES_BASE`;
/// tests pass a unique sandbox root.
fn ext_shellex_path_under(classes_base: &str, ext: &str) -> String {
    format!("{classes_base}\\{ext}\\ShellEx\\{IID_ITHUMBNAILPROVIDER}")
}

/// Build the registry sub-path for the CLSID root, rooted at
/// `clsid_base`. Production callers pass `CLSID_BASE`.
fn clsid_root_path_under(clsid_base: &str, clsid_str: &str) -> String {
    format!("{clsid_base}\\{clsid_str}")
}

/// Production-flavoured wrapper for `ext_shellex_path_under`.
fn ext_shellex_path(ext: &str) -> String {
    ext_shellex_path_under(CLASSES_BASE, ext)
}

/// Production-flavoured wrapper for `clsid_root_path_under`.
fn clsid_root_path() -> String {
    clsid_root_path_under(CLSID_BASE, CLSID_STR)
}

/// Resolve the calling DLL's own path via `GetModuleHandleExW` — only
/// meaningful when this code is running inside `arcthumb.dll`. The
/// config exe must NOT call this; it would return the exe's path.
fn get_dll_path_from_module() -> io::Result<String> {
    unsafe {
        let mut hmodule = HMODULE::default();
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(get_dll_path_from_module as *const () as *const u16),
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

// =============================================================================
// Public primitives (used by both the DLL and the config exe)
// =============================================================================

// =============================================================================
// Path-parameterized internals — production functions are thin wrappers,
// tests pass a sandbox root so they don't touch the real shell extension
// registration.
// =============================================================================

fn register_clsid_at(clsid_root: &str, dll_path: &Path) -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (clsid_key, _) = hkcu.create_subkey(clsid_root)?;
    clsid_key.set_value("", &"ArcThumb Thumbnail Provider")?;

    let (inproc, _) = clsid_key.create_subkey("InprocServer32")?;
    let dll_path_str = dll_path.to_string_lossy().into_owned();
    inproc.set_value("", &dll_path_str)?;
    inproc.set_value("ThreadingModel", &"Apartment")?;
    Ok(())
}

fn unregister_clsid_at(clsid_root: &str) -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let _ = hkcu.delete_subkey_all(clsid_root);
    Ok(())
}

fn register_extension_at(shellex_path: &str, clsid_str: &str) -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu.create_subkey(shellex_path)?;
    key.set_value("", &clsid_str.to_string())?;
    Ok(())
}

fn unregister_extension_at(shellex_path: &str) -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let _ = hkcu.delete_subkey_all(shellex_path);
    Ok(())
}

fn is_subkey_present(path: &str) -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    hkcu.open_subkey(path).is_ok()
}

fn read_inproc_default(clsid_root: &str) -> Option<PathBuf> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey(format!("{clsid_root}\\InprocServer32"))
        .ok()?;
    let path: String = key.get_value("").ok()?;
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

// =============================================================================
// Public production API
// =============================================================================

/// Write the CLSID subtree (`HKCU\Software\Classes\CLSID\{CLSID}`)
/// including the `InprocServer32` entry pointing at `dll_path`.
pub fn register_clsid(dll_path: &Path) -> io::Result<()> {
    register_clsid_at(&clsid_root_path(), dll_path)
}

/// Delete the CLSID subtree. Best effort: succeeds even if the tree
/// was already absent.
pub fn unregister_clsid() -> io::Result<()> {
    unregister_clsid_at(&clsid_root_path())
}

/// Wire a single file extension to our CLSID in the ShellEx slot.
/// `ext` must start with a dot, e.g. `".zip"`.
pub fn register_extension(ext: &str) -> io::Result<()> {
    register_extension_at(&ext_shellex_path(ext), CLSID_STR)
}

/// Remove the ShellEx binding for a single extension. No error if
/// the key is already gone.
pub fn unregister_extension(ext: &str) -> io::Result<()> {
    unregister_extension_at(&ext_shellex_path(ext))
}

/// True iff the ShellEx IID subkey currently exists for this extension.
pub fn is_extension_registered(ext: &str) -> bool {
    is_subkey_present(&ext_shellex_path(ext))
}

/// True iff the CLSID's `InprocServer32` subkey exists (our canonical
/// "shell extension is installed" signal).
pub fn is_clsid_registered() -> bool {
    is_subkey_present(&format!("{}\\InprocServer32", clsid_root_path()))
}

/// Read back `HKCU\Software\Classes\CLSID\{CLSID}\InprocServer32\(Default)`.
/// Used by the config exe as a fallback when the DLL isn't next to it.
pub fn read_registered_dll_path() -> Option<PathBuf> {
    read_inproc_default(&clsid_root_path())
}

// =============================================================================
// Backward-compatible wrappers used by DllRegisterServer / DllUnregisterServer
// =============================================================================

pub fn register() -> io::Result<()> {
    let dll_path_str = get_dll_path_from_module()?;
    register_clsid(Path::new(&dll_path_str))?;
    for ext in EXTENSIONS {
        register_extension(ext)?;
    }
    Ok(())
}

pub fn unregister() -> io::Result<()> {
    for ext in EXTENSIONS {
        let _ = unregister_extension(ext);
    }
    let _ = unregister_clsid();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_path_format() {
        assert_eq!(
            ext_shellex_path(".zip"),
            "Software\\Classes\\.zip\\ShellEx\\{E357FCCD-A995-4576-B01F-234630154E96}"
        );
    }

    #[test]
    fn clsid_root_format() {
        assert_eq!(
            clsid_root_path(),
            "Software\\Classes\\CLSID\\{0F4F5659-D383-4945-A534-01E1EED1D23F}"
        );
    }

    #[test]
    fn extensions_constant_is_non_empty_and_dotted() {
        assert!(!EXTENSIONS.is_empty());
        for ext in EXTENSIONS {
            assert!(ext.starts_with('.'), "{ext} must start with .");
            assert!(ext.len() >= 3, "{ext} too short");
        }
    }

    // ---------------------------------------------------------------
    // Hermetic registry roundtrips. Each test uses a unique sandbox
    // root under HKCU so it can't collide with the user's real shell
    // extension state or with parallel test runs.
    // ---------------------------------------------------------------

    /// Build a registry sandbox path that is guaranteed unique per
    /// test invocation. We embed the PID + a high-resolution clock
    /// reading + a tag, then verify the key doesn't already exist
    /// before the test writes anything.
    fn unique_sandbox(tag: &str) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("Software\\ArcThumbTest_{tag}_{pid}_{nanos}")
    }

    /// RAII helper that wipes a sandbox subtree on Drop, even if the
    /// test panics partway through.
    struct SandboxGuard(String);
    impl Drop for SandboxGuard {
        fn drop(&mut self) {
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let _ = hkcu.delete_subkey_all(&self.0);
        }
    }

    #[test]
    fn extension_register_roundtrip() {
        let sandbox = unique_sandbox("ext");
        let _guard = SandboxGuard(sandbox.clone());
        let path = ext_shellex_path_under(&sandbox, ".zip");

        // Pre-condition: the sandbox is empty.
        assert!(!is_subkey_present(&path));

        // Register and verify the value was written.
        register_extension_at(&path, CLSID_STR).expect("register");
        assert!(is_subkey_present(&path));

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let key = hkcu.open_subkey(&path).expect("open after register");
        let val: String = key.get_value("").expect("read default value");
        assert_eq!(val, CLSID_STR);

        // Unregister and verify the key is gone.
        unregister_extension_at(&path).expect("unregister");
        assert!(!is_subkey_present(&path));
    }

    #[test]
    fn clsid_register_roundtrip() {
        let sandbox = unique_sandbox("clsid");
        let _guard = SandboxGuard(sandbox.clone());
        // The sandbox holds a fake CLSID root so the production CLSID
        // is never touched.
        let clsid_root = format!("{sandbox}\\{{TEST-CLSID}}");

        assert!(!is_subkey_present(&clsid_root));

        let dll_path = std::path::PathBuf::from(r"C:\fake\arcthumb.dll");
        register_clsid_at(&clsid_root, &dll_path).expect("register");

        // InprocServer32 default value should match what we wrote.
        let inproc_path = format!("{clsid_root}\\InprocServer32");
        assert!(is_subkey_present(&inproc_path));

        let read_back = read_inproc_default(&clsid_root).expect("read back");
        assert_eq!(read_back, dll_path);

        // ThreadingModel must be Apartment for shell extension hosts.
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let inproc = hkcu.open_subkey(&inproc_path).expect("open inproc");
        let threading: String = inproc
            .get_value("ThreadingModel")
            .expect("read ThreadingModel");
        assert_eq!(threading, "Apartment");

        unregister_clsid_at(&clsid_root).expect("unregister");
        assert!(!is_subkey_present(&clsid_root));
    }

    #[test]
    fn unregister_missing_extension_is_noop() {
        let sandbox = unique_sandbox("missing_ext");
        let _guard = SandboxGuard(sandbox.clone());
        let path = ext_shellex_path_under(&sandbox, ".doesnotexist");
        // Deleting a key that was never created must succeed silently.
        unregister_extension_at(&path).expect("noop unregister");
    }

    #[test]
    fn unregister_missing_clsid_is_noop() {
        let sandbox = unique_sandbox("missing_clsid");
        let _guard = SandboxGuard(sandbox.clone());
        let clsid_root = format!("{sandbox}\\{{NOPE}}");
        unregister_clsid_at(&clsid_root).expect("noop unregister");
    }

    #[test]
    fn read_inproc_default_returns_none_when_missing() {
        let sandbox = unique_sandbox("read_missing");
        let _guard = SandboxGuard(sandbox.clone());
        let clsid_root = format!("{sandbox}\\{{ABSENT}}");
        assert!(read_inproc_default(&clsid_root).is_none());
    }

    #[test]
    fn full_install_uninstall_for_all_extensions() {
        // Exercises the EXTENSIONS list against the sandbox so a
        // future addition (e.g. .epub) is automatically covered.
        let sandbox = unique_sandbox("full");
        let _guard = SandboxGuard(sandbox.clone());

        // Register the fake CLSID + every extension under the sandbox.
        let clsid_root = format!("{sandbox}\\CLSID\\{CLSID_STR}");
        register_clsid_at(&clsid_root, std::path::Path::new(r"C:\fake.dll"))
            .expect("clsid register");

        for ext in EXTENSIONS {
            let path = ext_shellex_path_under(&sandbox, ext);
            register_extension_at(&path, CLSID_STR).expect(ext);
            assert!(is_subkey_present(&path), "{ext} not present after register");
        }

        // Tear down in reverse order of install (production order).
        for ext in EXTENSIONS {
            let path = ext_shellex_path_under(&sandbox, ext);
            unregister_extension_at(&path).expect(ext);
            assert!(!is_subkey_present(&path), "{ext} still present after unregister");
        }
        unregister_clsid_at(&clsid_root).expect("clsid unregister");
    }

    #[test]
    fn ext_shellex_path_under_uses_provided_root() {
        let p = ext_shellex_path_under("Foo\\Bar", ".zip");
        assert_eq!(
            p,
            "Foo\\Bar\\.zip\\ShellEx\\{E357FCCD-A995-4576-B01F-234630154E96}"
        );
    }

    #[test]
    fn clsid_root_path_under_uses_provided_root() {
        let p = clsid_root_path_under("Foo\\CLSID", "{XYZ}");
        assert_eq!(p, "Foo\\CLSID\\{XYZ}");
    }
}
