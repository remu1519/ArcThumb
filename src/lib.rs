//! ArcThumb — Windows Shell Extension (IThumbnailProvider)
//!
//! Phase 1: working thumbnail provider that returns a solid-color dummy
//! bitmap for .zip files. Archive reading and real image decoding come
//! in Phase 2.

#![allow(non_snake_case)]

mod archive;
mod bitmap;
mod com;
mod decode;
mod ebook;
mod limits;
mod log;
pub mod registry;
pub mod settings;
mod stream;

use std::panic::catch_unwind;

use windows::core::{Interface, GUID, HRESULT};
use windows::Win32::Foundation::{E_FAIL, E_POINTER, S_FALSE, S_OK};
use windows::Win32::System::Com::IClassFactory;

pub use com::CLSID_ARCTHUMB_PROVIDER;

/// COM error: "no class factory for the requested CLSID".
const CLASS_E_CLASSNOTAVAILABLE: HRESULT = HRESULT(0x80040111u32 as i32);

/// Catch any panic inside `f` and turn it into `E_FAIL`.
///
/// Rust panics propagating across `extern "system"` are undefined
/// behaviour — on Windows they'd crash Explorer. Every COM entry
/// point in this DLL funnels through this helper.
fn guard<F: FnOnce() -> HRESULT + std::panic::UnwindSafe>(f: F) -> HRESULT {
    match catch_unwind(f) {
        Ok(hr) => hr,
        Err(_) => {
            crate::log::log("PANIC caught at DLL entry point");
            E_FAIL
        }
    }
}

/// Called by COM when a client asks this DLL for a class factory.
///
/// Our job:
/// 1. Check the requested CLSID is ours (we only host one class).
/// 2. Hand back an `IClassFactory` that knows how to create `ArcThumbProvider`s.
#[unsafe(no_mangle)]
pub extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut core::ffi::c_void,
) -> HRESULT {
    guard(|| {
        if rclsid.is_null() || riid.is_null() || ppv.is_null() {
            return E_POINTER;
        }
        unsafe {
            if *rclsid != CLSID_ARCTHUMB_PROVIDER {
                return CLASS_E_CLASSNOTAVAILABLE;
            }
            let factory: IClassFactory = com::ArcThumbClassFactory.into();
            factory.query(&*riid, ppv)
        }
    })
}

/// Called by COM to ask whether this DLL can be unloaded.
///
/// Returning `S_FALSE` tells the host "please keep me loaded". Tracking
/// live object counts correctly is finicky; for a shell extension this
/// is a reasonable default — Explorer unloads us when it shuts down
/// or when the DLL idle timer fires.
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    guard(|| S_FALSE)
}

/// Called by `regsvr32 arcthumb.dll`.
#[unsafe(no_mangle)]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    guard(|| match registry::register() {
        Ok(()) => S_OK,
        Err(_) => E_FAIL,
    })
}

/// Called by `regsvr32 /u arcthumb.dll`.
#[unsafe(no_mangle)]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    guard(|| match registry::unregister() {
        Ok(()) => S_OK,
        Err(_) => E_FAIL,
    })
}
