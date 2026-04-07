//! End-to-end COM integration test for arcthumb.dll.
//!
//! This test exercises the same code path Explorer uses:
//!
//! 1. `LoadLibraryW("arcthumb.dll")`
//! 2. `GetProcAddress("DllGetClassObject")`
//! 3. Call it to obtain `IClassFactory` for `CLSID_ARCTHUMB_PROVIDER`
//! 4. `IClassFactory::CreateInstance` → `IUnknown`
//! 5. `QueryInterface` for `IInitializeWithStream` and `IThumbnailProvider`
//! 6. Wrap an in-memory ZIP (containing a real PNG) in an `IStream`
//!    via `SHCreateMemStream`
//! 7. `IInitializeWithStream::Initialize(stream)`
//! 8. `IThumbnailProvider::GetThumbnail(64)` → `HBITMAP`
//! 9. Verify the bitmap exists and has sane dimensions
//! 10. Free everything
//!
//! Together this covers `lib.rs` (Dll exports), `com.rs` (factory +
//! provider), `stream.rs` (ComStreamReader bridging IStream), most
//! of `bitmap.rs` (from_rgba GDI path), plus the archive + decode
//! pipeline that the unit tests already cover in isolation.
//!
//! The test is conditionally compiled for Windows only — the rest
//! of the crate is too, but cargo would still try to build the test
//! file on other targets, so we gate it explicitly.

#![cfg(windows)]

use std::ffi::{c_void, OsString};
use std::io::Cursor;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use windows::core::{Interface, GUID, HRESULT, PCWSTR};
use windows::Win32::Foundation::{FreeLibrary, HMODULE, S_OK};
use windows::Win32::Graphics::Gdi::{DeleteObject, GetObjectW, BITMAP, HBITMAP, HGDIOBJ};
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, IClassFactory, IStream, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Shell::PropertiesSystem::IInitializeWithStream;
use windows::Win32::UI::Shell::{IThumbnailProvider, SHCreateMemStream, WTS_ALPHATYPE};

use arcthumb::CLSID_ARCTHUMB_PROVIDER;

/// Signature of `DllGetClassObject` as it is exported from `arcthumb.dll`.
type DllGetClassObjectFn = unsafe extern "system" fn(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT;

// =============================================================================
// Helpers
// =============================================================================

/// Locate `arcthumb.dll` relative to the running test executable.
///
/// Cargo lays out test binaries as `target/<profile>/deps/<name>-<hash>.exe`
/// and the cdylib alongside `target/<profile>/arcthumb.dll`. Walking
/// up two parents from the test exe lands us in the profile dir.
///
/// **Important**: some test runners (notably `cargo llvm-cov`) invoke
/// `cargo test --tests`, which only builds test targets and skips the
/// cdylib. We detect that and run `cargo build --lib` explicitly into
/// the *same* target dir, inheriting `RUSTFLAGS` so the cdylib carries
/// the same instrumentation as the test binary. That keeps the COM
/// integration test useful for coverage measurement, not just for
/// black-box pass/fail.
fn locate_dll() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    // <target>/<profile>/deps/com_integration-HASH.exe
    let profile_dir = exe
        .parent() // deps/
        .and_then(|p| p.parent()) // <profile>/
        .expect("test exe should be inside target/<profile>/deps/");
    let target_dir = profile_dir
        .parent() // target/  (or target/llvm-cov-target/)
        .expect("profile dir should have a parent")
        .to_path_buf();
    let profile_name = profile_dir
        .file_name()
        .expect("profile dir name")
        .to_string_lossy()
        .into_owned();

    let candidate = profile_dir.join("arcthumb.dll");
    if candidate.exists() {
        return candidate;
    }

    // Cdylib wasn't built. Force `cargo build --lib` into the same
    // target dir we're already running from, so any instrumentation
    // RUSTFLAGS the parent process set are honoured.
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut cmd = std::process::Command::new(env!("CARGO"));
    cmd.args(["build", "--lib", "--quiet"])
        .current_dir(manifest)
        .env("CARGO_TARGET_DIR", &target_dir);
    if profile_name == "release" {
        cmd.arg("--release");
    }
    let status = cmd.status().expect("failed to spawn cargo build --lib");
    assert!(status.success(), "cargo build --lib failed");

    assert!(
        candidate.exists(),
        "arcthumb.dll still missing after cargo build at {}",
        candidate.display()
    );
    candidate
}

/// UTF-16 NUL-terminated form of a `Path`, suitable for `LoadLibraryW`.
fn to_wide(path: &std::path::Path) -> Vec<u16> {
    OsString::from(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Build a tiny in-memory PNG so we have realistic image bytes for
/// the archive entry. Using the `image` crate keeps the fixture
/// self-contained and reproducible across CI machines.
fn make_tiny_png() -> Vec<u8> {
    use image::{ImageBuffer, Rgba};
    let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_fn(4, 4, |_, _| Rgba([255, 0, 0, 255]));
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

/// Wrap a single PNG in an in-memory ZIP and return the bytes.
fn make_test_zip() -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let png = make_tiny_png();
    let mut buf = Vec::new();
    {
        let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        w.start_file("01.png", opts).unwrap();
        std::io::Write::write_all(&mut w, &png).unwrap();
        w.finish().unwrap();
    }
    buf
}

/// RAII guard that calls `FreeLibrary` on drop, so a panicking test
/// doesn't leak the loaded DLL handle.
struct LoadedDll(HMODULE);
impl Drop for LoadedDll {
    fn drop(&mut self) {
        unsafe {
            let _ = FreeLibrary(self.0);
        }
    }
}

/// RAII wrapper that calls `CoUninitialize` on drop.
struct ComApartment;
impl ComApartment {
    fn enter() -> Self {
        unsafe {
            // Shell extensions live in STA. Test runs in its own
            // thread so this is hermetic.
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            assert!(hr.is_ok(), "CoInitializeEx failed: {hr:?}");
        }
        Self
    }
}
impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// Free an HBITMAP on drop.
struct OwnedHBitmap(HBITMAP);
impl Drop for OwnedHBitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.0.0));
        }
    }
}

// =============================================================================
// The actual integration test
// =============================================================================

#[test]
fn end_to_end_thumbnail_via_dll() {
    let _com = ComApartment::enter();

    let dll_path = locate_dll();
    assert!(
        dll_path.exists(),
        "arcthumb.dll not found at {}; run `cargo build` first",
        dll_path.display()
    );

    // ---- Step 1: load the DLL ----------------------------------
    let wide = to_wide(&dll_path);
    let module = unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }
        .expect("LoadLibraryW failed");
    let _dll_guard = LoadedDll(module);

    // ---- Step 2: resolve DllGetClassObject ---------------------
    let proc = unsafe { GetProcAddress(module, windows::core::s!("DllGetClassObject")) }
        .expect("DllGetClassObject not exported");
    let dll_get_class_object: DllGetClassObjectFn =
        unsafe { std::mem::transmute(proc) };

    // ---- Step 3: ask for IClassFactory -------------------------
    let mut factory_ptr: *mut c_void = std::ptr::null_mut();
    let hr = unsafe {
        dll_get_class_object(
            &CLSID_ARCTHUMB_PROVIDER,
            &IClassFactory::IID,
            &mut factory_ptr,
        )
    };
    assert_eq!(hr, S_OK, "DllGetClassObject failed: {hr:?}");
    assert!(!factory_ptr.is_null(), "factory pointer is null");

    let factory: IClassFactory =
        unsafe { IClassFactory::from_raw(factory_ptr) };

    // ---- Step 4: factory creates a thumbnail provider ----------
    let provider_unknown: windows::core::IUnknown = unsafe {
        factory
            .CreateInstance(None)
            .expect("CreateInstance failed")
    };

    // ---- Step 5: QueryInterface for the two interfaces we need
    let init_with_stream: IInitializeWithStream =
        provider_unknown.cast().expect("cast to IInitializeWithStream");
    let thumb_provider: IThumbnailProvider =
        provider_unknown.cast().expect("cast to IThumbnailProvider");

    // ---- Step 6: build a fake archive stream -------------------
    let zip_bytes = make_test_zip();
    let stream: IStream = unsafe {
        SHCreateMemStream(Some(&zip_bytes))
    }
    .expect("SHCreateMemStream returned None");

    // ---- Step 7: Initialize(stream) ----------------------------
    unsafe {
        init_with_stream
            .Initialize(&stream, 0)
            .expect("Initialize failed");
    }

    // ---- Step 8: GetThumbnail(64) ------------------------------
    let mut hbmp = HBITMAP::default();
    let mut alpha: WTS_ALPHATYPE = WTS_ALPHATYPE(0);
    unsafe {
        thumb_provider
            .GetThumbnail(64, &mut hbmp, &mut alpha)
            .expect("GetThumbnail failed");
    }
    let _bmp_guard = OwnedHBitmap(hbmp);

    // ---- Step 9: validate the returned bitmap ------------------
    assert!(!hbmp.is_invalid(), "HBITMAP is invalid");

    // Inspect the DIB header. The provider resizes to fit inside
    // 64×64 while preserving aspect ratio; for our 4×4 source the
    // result is exactly 64×64.
    let mut bm = BITMAP::default();
    let written = unsafe {
        GetObjectW(
            HGDIOBJ(hbmp.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut _ as *mut _),
        )
    };
    assert!(written > 0, "GetObjectW returned 0");
    assert!(bm.bmWidth > 0 && bm.bmWidth <= 64, "width = {}", bm.bmWidth);
    assert!(bm.bmHeight > 0 && bm.bmHeight <= 64, "height = {}", bm.bmHeight);
    assert_eq!(bm.bmBitsPixel, 32, "expected 32bpp DIB");
    // The provider always returns ARGB so Explorer can composite.
    assert_eq!(alpha.0, 2 /* WTSAT_ARGB */);
}

/// Negative-path test: passing the wrong CLSID must yield
/// `CLASS_E_CLASSNOTAVAILABLE` (0x80040111) and **not** crash the
/// loader thread.
#[test]
fn dll_get_class_object_rejects_unknown_clsid() {
    let _com = ComApartment::enter();

    let dll_path = locate_dll();
    let wide = to_wide(&dll_path);
    let module = unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }
        .expect("LoadLibraryW failed");
    let _dll_guard = LoadedDll(module);

    let proc = unsafe { GetProcAddress(module, windows::core::s!("DllGetClassObject")) }
        .expect("DllGetClassObject not exported");
    let dll_get_class_object: DllGetClassObjectFn =
        unsafe { std::mem::transmute(proc) };

    // Random GUID we definitely don't host.
    let bogus = GUID::from_u128(0xDEAD_BEEF_CAFE_BABE_0102_0304_0506_0708);
    let mut out: *mut c_void = std::ptr::null_mut();
    let hr = unsafe {
        dll_get_class_object(&bogus, &IClassFactory::IID, &mut out)
    };
    // CLASS_E_CLASSNOTAVAILABLE
    assert_eq!(hr.0, 0x80040111u32 as i32, "expected CLASS_E_CLASSNOTAVAILABLE, got {hr:?}");
    assert!(out.is_null());
}

/// Negative-path test: `DllCanUnloadNow` should return `S_FALSE` so
/// COM keeps us loaded for the lifetime of Explorer.
#[test]
fn dll_can_unload_now_returns_s_false() {
    let dll_path = locate_dll();
    let wide = to_wide(&dll_path);
    let module = unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }
        .expect("LoadLibraryW failed");
    let _dll_guard = LoadedDll(module);

    let proc = unsafe { GetProcAddress(module, windows::core::s!("DllCanUnloadNow")) }
        .expect("DllCanUnloadNow not exported");
    type Fn0 = unsafe extern "system" fn() -> HRESULT;
    let f: Fn0 = unsafe { std::mem::transmute(proc) };
    let hr = unsafe { f() };
    // S_FALSE = 0x00000001
    assert_eq!(hr.0, 1, "expected S_FALSE, got {hr:?}");
}

// Suppress dead-code warnings for the unused OsStringExt import on
// some configurations.
#[allow(dead_code)]
fn _suppress_unused() {
    let _ = OsString::from_wide(&[]);
}
