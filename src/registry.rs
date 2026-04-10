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

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};
use windows::core::PCWSTR;
use winreg::RegKey;
use winreg::enums::*;

// =============================================================================
// Constants
// =============================================================================

/// String form of the ArcThumb thumbnail provider CLSID (defined in
/// `com.rs`). **Never change** — baked into users' registries.
pub const CLSID_STR: &str = "{0F4F5659-D383-4945-A534-01E1EED1D23F}";

/// String form of the ArcThumb preview handler CLSID (defined in
/// `preview.rs`). **Never change** — baked into users' registries.
pub const PREVIEW_CLSID_STR: &str = "{8C7C1E5F-3D4A-4E2B-9F1A-7B5D6E8F9A0C}";

/// Standard IID of `IThumbnailProvider`. Explorer looks under
/// `.<ext>\ShellEx\<this IID>` to find the thumbnail handler.
const IID_ITHUMBNAILPROVIDER: &str = "{E357FCCD-A995-4576-B01F-234630154E96}";

/// Standard IID of `IPreviewHandler`. Explorer looks under
/// `.<ext>\ShellEx\<this IID>` to find the preview handler.
const IID_IPREVIEWHANDLER: &str = "{8895B1C6-B41F-4C1C-A562-0D564250836F}";

/// AppID of the standard preview-host surrogate. Setting this on
/// the CLSID key tells COM to load our DLL inside `prevhost.exe`
/// (per-user, no admin needed; isolation handled by Windows).
const PREVHOST_APPID: &str = "{534A1E02-D58F-44f0-B58B-36CBED287C7C}";

/// File extensions that ArcThumb handles.
pub const EXTENSIONS: &[&str] = &[
    ".zip", ".cbz", ".rar", ".cbr", ".7z", ".cb7", ".cbt", ".epub", ".fb2", ".mobi", ".azw",
    ".azw3",
];

/// Production parent key for shell extension registrations.
const CLASSES_BASE: &str = "Software\\Classes";

/// Production parent key for COM CLSIDs.
const CLSID_BASE: &str = "Software\\Classes\\CLSID";

// =============================================================================
// HandlerKind — captures the difference between thumbnail and preview
// =============================================================================

/// Describes one of the two COM handlers ArcThumb registers.
/// All handler-specific logic is parameterized over this, eliminating
/// the thumbnail/preview code duplication.
struct HandlerKind {
    clsid_str: &'static str,
    iid_shellex: &'static str,
    display_name: &'static str,
    app_id: Option<&'static str>,
}

const THUMBNAIL: HandlerKind = HandlerKind {
    clsid_str: CLSID_STR,
    iid_shellex: IID_ITHUMBNAILPROVIDER,
    display_name: "ArcThumb Thumbnail Provider",
    app_id: None,
};

const PREVIEW: HandlerKind = HandlerKind {
    clsid_str: PREVIEW_CLSID_STR,
    iid_shellex: IID_IPREVIEWHANDLER,
    display_name: "ArcThumb Preview Handler",
    app_id: Some(PREVHOST_APPID),
};

// =============================================================================
// Path builders
// =============================================================================

/// Build the registry sub-path for a given extension's ShellEx slot.
fn ext_shellex_path_under(classes_base: &str, ext: &str, iid: &str) -> String {
    format!("{classes_base}\\{ext}\\ShellEx\\{iid}")
}

/// Build the registry sub-path for the CLSID root.
fn clsid_root_path_under(clsid_base: &str, clsid_str: &str) -> String {
    format!("{clsid_base}\\{clsid_str}")
}

impl HandlerKind {
    fn clsid_root(&self) -> String {
        clsid_root_path_under(CLSID_BASE, self.clsid_str)
    }

    fn ext_shellex_path(&self, ext: &str) -> String {
        ext_shellex_path_under(CLASSES_BASE, ext, self.iid_shellex)
    }
}

// =============================================================================
// Registry primitives (parameterized for testability)
// =============================================================================

fn register_clsid_at(clsid_root: &str, dll_path: &Path, handler: &HandlerKind) -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (clsid_key, _) = hkcu.create_subkey(clsid_root)?;
    clsid_key.set_value("", &handler.display_name)?;

    if let Some(app_id) = handler.app_id {
        clsid_key.set_value("AppID", &app_id.to_string())?;
    }

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

/// Resolve the calling DLL's own path via `GetModuleHandleExW` — only
/// meaningful when this code is running inside `arcthumb.dll`. The
/// config exe must NOT call this; it would return the exe's path.
fn get_dll_path_from_module() -> io::Result<String> {
    unsafe {
        let mut hmodule = HMODULE::default();
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
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
// Public API — thumbnail provider
// =============================================================================

/// Write the CLSID subtree (`HKCU\Software\Classes\CLSID\{CLSID}`)
/// including the `InprocServer32` entry pointing at `dll_path`.
pub fn register_clsid(dll_path: &Path) -> io::Result<()> {
    register_clsid_at(&THUMBNAIL.clsid_root(), dll_path, &THUMBNAIL)
}

/// Delete the CLSID subtree. Best effort: succeeds even if the tree
/// was already absent.
pub fn unregister_clsid() -> io::Result<()> {
    unregister_clsid_at(&THUMBNAIL.clsid_root())
}

/// Wire a single file extension to our CLSID in the ShellEx slot.
/// `ext` must start with a dot, e.g. `".zip"`.
pub fn register_extension(ext: &str) -> io::Result<()> {
    register_extension_at(&THUMBNAIL.ext_shellex_path(ext), CLSID_STR)
}

/// Remove the ShellEx binding for a single extension. No error if
/// the key is already gone.
pub fn unregister_extension(ext: &str) -> io::Result<()> {
    unregister_extension_at(&THUMBNAIL.ext_shellex_path(ext))
}

/// True iff the ShellEx IID subkey currently exists for this extension.
pub fn is_extension_registered(ext: &str) -> bool {
    is_subkey_present(&THUMBNAIL.ext_shellex_path(ext))
}

/// True iff the CLSID's `InprocServer32` subkey exists (our canonical
/// "shell extension is installed" signal).
pub fn is_clsid_registered() -> bool {
    is_subkey_present(&format!("{}\\InprocServer32", THUMBNAIL.clsid_root()))
}

/// Read back `HKCU\Software\Classes\CLSID\{CLSID}\InprocServer32\(Default)`.
/// Used by the config exe as a fallback when the DLL isn't next to it.
pub fn read_registered_dll_path() -> Option<PathBuf> {
    read_inproc_default(&THUMBNAIL.clsid_root())
}

// =============================================================================
// Public API — preview handler
// =============================================================================

/// Write the preview-handler CLSID subtree, including the AppID
/// pointing at prevhost.exe and the InprocServer32 entry.
pub fn register_preview_clsid(dll_path: &Path) -> io::Result<()> {
    register_clsid_at(&PREVIEW.clsid_root(), dll_path, &PREVIEW)
}

/// Delete the preview-handler CLSID subtree. Best effort.
pub fn unregister_preview_clsid() -> io::Result<()> {
    unregister_clsid_at(&PREVIEW.clsid_root())
}

/// Wire one extension to the preview-handler CLSID via its
/// `IPreviewHandler` ShellEx slot.
pub fn register_preview_extension(ext: &str) -> io::Result<()> {
    register_extension_at(&PREVIEW.ext_shellex_path(ext), PREVIEW_CLSID_STR)
}

/// Remove the preview-handler ShellEx binding for an extension.
pub fn unregister_preview_extension(ext: &str) -> io::Result<()> {
    unregister_extension_at(&PREVIEW.ext_shellex_path(ext))
}

/// True iff the preview-handler `InprocServer32` subkey exists. Used
/// as the source of truth for the GUI's "Enable preview pane" toggle.
pub fn is_preview_enabled() -> bool {
    is_subkey_present(&format!("{}\\InprocServer32", PREVIEW.clsid_root()))
}

// =============================================================================
// Backward-compatible wrappers used by DllRegisterServer / DllUnregisterServer
// =============================================================================

pub fn register() -> io::Result<()> {
    let dll_path_str = get_dll_path_from_module()?;
    let dll_path = Path::new(&dll_path_str);
    register_clsid(dll_path)?;
    register_preview_clsid(dll_path)?;
    for ext in EXTENSIONS {
        register_extension(ext)?;
        register_preview_extension(ext)?;
    }
    notify_assoc_changed();
    Ok(())
}

pub fn unregister() -> io::Result<()> {
    for ext in EXTENSIONS {
        let _ = unregister_extension(ext);
        let _ = unregister_preview_extension(ext);
    }
    let _ = unregister_clsid();
    let _ = unregister_preview_clsid();
    notify_assoc_changed();
    Ok(())
}

/// Tell the Shell that file-type associations changed so it invalidates
/// its icon and thumbnail caches and picks up our newly registered (or
/// removed) handlers without a reboot.
pub fn notify_assoc_changed() {
    use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------
    // Path format tests
    // ---------------------------------------------------------------

    #[test]
    fn thumbnail_extension_path_format() {
        assert_eq!(
            THUMBNAIL.ext_shellex_path(".zip"),
            "Software\\Classes\\.zip\\ShellEx\\{E357FCCD-A995-4576-B01F-234630154E96}"
        );
    }

    #[test]
    fn thumbnail_clsid_root_format() {
        assert_eq!(
            THUMBNAIL.clsid_root(),
            "Software\\Classes\\CLSID\\{0F4F5659-D383-4945-A534-01E1EED1D23F}"
        );
    }

    #[test]
    fn preview_extension_path_format() {
        assert_eq!(
            PREVIEW.ext_shellex_path(".zip"),
            "Software\\Classes\\.zip\\ShellEx\\{8895B1C6-B41F-4C1C-A562-0D564250836F}"
        );
    }

    #[test]
    fn preview_clsid_root_format() {
        assert_eq!(
            PREVIEW.clsid_root(),
            "Software\\Classes\\CLSID\\{8C7C1E5F-3D4A-4E2B-9F1A-7B5D6E8F9A0C}"
        );
    }

    #[test]
    fn ext_shellex_path_under_uses_provided_root() {
        let p = ext_shellex_path_under("Foo\\Bar", ".zip", IID_ITHUMBNAILPROVIDER);
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

    #[test]
    fn extensions_constant_is_non_empty_and_dotted() {
        assert!(!EXTENSIONS.is_empty());
        for ext in EXTENSIONS {
            assert!(ext.starts_with('.'), "{ext} must start with .");
            assert!(ext.len() >= 3, "{ext} too short");
        }
    }

    // ---------------------------------------------------------------
    // Hermetic registry roundtrips
    // ---------------------------------------------------------------

    fn unique_sandbox(tag: &str) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("Software\\ArcThumbTest_{tag}_{pid}_{nanos}")
    }

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
        let path = ext_shellex_path_under(&sandbox, ".zip", THUMBNAIL.iid_shellex);

        assert!(!is_subkey_present(&path));

        register_extension_at(&path, CLSID_STR).expect("register");
        assert!(is_subkey_present(&path));

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let key = hkcu.open_subkey(&path).expect("open after register");
        let val: String = key.get_value("").expect("read default value");
        assert_eq!(val, CLSID_STR);

        unregister_extension_at(&path).expect("unregister");
        assert!(!is_subkey_present(&path));
    }

    #[test]
    fn thumbnail_clsid_register_roundtrip() {
        let sandbox = unique_sandbox("thumb_clsid");
        let _guard = SandboxGuard(sandbox.clone());
        let clsid_root = format!("{sandbox}\\{{TEST-CLSID}}");

        assert!(!is_subkey_present(&clsid_root));

        let dll_path = std::path::PathBuf::from(r"C:\fake\arcthumb.dll");
        register_clsid_at(&clsid_root, &dll_path, &THUMBNAIL).expect("register");

        let inproc_path = format!("{clsid_root}\\InprocServer32");
        assert!(is_subkey_present(&inproc_path));

        let read_back = read_inproc_default(&clsid_root).expect("read back");
        assert_eq!(read_back, dll_path);

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let inproc = hkcu.open_subkey(&inproc_path).expect("open inproc");
        let threading: String = inproc
            .get_value("ThreadingModel")
            .expect("read ThreadingModel");
        assert_eq!(threading, "Apartment");

        // Thumbnail handler has no AppID.
        let clsid_key = hkcu.open_subkey(&clsid_root).expect("open clsid");
        assert!(
            clsid_key.get_value::<String, _>("AppID").is_err(),
            "thumbnail should not have AppID"
        );

        unregister_clsid_at(&clsid_root).expect("unregister");
        assert!(!is_subkey_present(&clsid_root));
    }

    #[test]
    fn preview_clsid_register_roundtrip() {
        let sandbox = unique_sandbox("preview_clsid");
        let _guard = SandboxGuard(sandbox.clone());
        let clsid_root = format!("{sandbox}\\{{TEST-PREVIEW}}");

        assert!(!is_subkey_present(&clsid_root));

        let dll_path = std::path::PathBuf::from(r"C:\fake\arcthumb.dll");
        register_clsid_at(&clsid_root, &dll_path, &PREVIEW).expect("register");

        let read_back = read_inproc_default(&clsid_root).expect("read back");
        assert_eq!(read_back, dll_path);

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let inproc = hkcu
            .open_subkey(format!("{clsid_root}\\InprocServer32"))
            .expect("open inproc");
        let threading: String = inproc
            .get_value("ThreadingModel")
            .expect("read ThreadingModel");
        assert_eq!(threading, "Apartment");

        // Preview handler has AppID pointing at prevhost.exe.
        let clsid_key = hkcu.open_subkey(&clsid_root).expect("open clsid");
        let app_id: String = clsid_key.get_value("AppID").expect("read AppID");
        assert_eq!(app_id, PREVHOST_APPID);

        unregister_clsid_at(&clsid_root).expect("unregister");
        assert!(!is_subkey_present(&clsid_root));
    }

    #[test]
    fn unregister_missing_extension_is_noop() {
        let sandbox = unique_sandbox("missing_ext");
        let _guard = SandboxGuard(sandbox.clone());
        let path = ext_shellex_path_under(&sandbox, ".doesnotexist", THUMBNAIL.iid_shellex);
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
        let sandbox = unique_sandbox("full");
        let _guard = SandboxGuard(sandbox.clone());

        for handler in [&THUMBNAIL, &PREVIEW] {
            let clsid_root = clsid_root_path_under(&sandbox, handler.clsid_str);
            register_clsid_at(&clsid_root, Path::new(r"C:\fake.dll"), handler)
                .expect("clsid register");

            for ext in EXTENSIONS {
                let path = ext_shellex_path_under(&sandbox, ext, handler.iid_shellex);
                register_extension_at(&path, handler.clsid_str).expect(ext);
                assert!(is_subkey_present(&path), "{ext} not present after register");
            }

            for ext in EXTENSIONS {
                let path = ext_shellex_path_under(&sandbox, ext, handler.iid_shellex);
                unregister_extension_at(&path).expect(ext);
                assert!(
                    !is_subkey_present(&path),
                    "{ext} still present after unregister"
                );
            }
            unregister_clsid_at(&clsid_root).expect("clsid unregister");
        }
    }

    #[test]
    fn preview_extension_register_roundtrip() {
        let sandbox = unique_sandbox("preview_ext");
        let _guard = SandboxGuard(sandbox.clone());
        let path = ext_shellex_path_under(&sandbox, ".epub", PREVIEW.iid_shellex);

        assert!(!is_subkey_present(&path));

        register_extension_at(&path, PREVIEW_CLSID_STR).expect("register");
        assert!(is_subkey_present(&path));

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let key = hkcu.open_subkey(&path).expect("open after register");
        let val: String = key.get_value("").expect("read default");
        assert_eq!(val, PREVIEW_CLSID_STR);

        unregister_extension_at(&path).expect("unregister");
        assert!(!is_subkey_present(&path));
    }
}
