//! COM objects: `ArcThumbClassFactory` and `ArcThumbProvider`.
//!
//! - `ArcThumbClassFactory` implements `IClassFactory`. It is the thing
//!   `DllGetClassObject` hands back, and its only job is to create
//!   fresh `ArcThumbProvider` instances on demand.
//!
//! - `ArcThumbProvider` implements `IInitializeWithStream` (Explorer
//!   gives us a stream over the target file) and `IThumbnailProvider`
//!   (Explorer asks us for an HBITMAP of a given size).
//!
//! Phase 1 ignores the stream entirely and always returns a solid-color
//! dummy bitmap. Phase 2 will actually parse the ZIP from the stream
//! and decode the first image.

use std::cell::RefCell;
use std::error::Error as StdError;
use std::ffi::c_void;

use windows::Win32::Foundation::{BOOL, CLASS_E_NOAGGREGATION, E_FAIL, E_POINTER};
use windows::Win32::Graphics::Gdi::HBITMAP;
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl, IStream};
use windows::Win32::UI::Shell::PropertiesSystem::{
    IInitializeWithStream, IInitializeWithStream_Impl,
};
use windows::Win32::UI::Shell::{
    IThumbnailProvider, IThumbnailProvider_Impl, WTS_ALPHATYPE, WTSAT_ARGB,
};
use windows::core::{GUID, IUnknown, Interface, Result, implement};

use crate::{alog, archive, bitmap, decode, limits, settings, stream::ComStreamReader};

/// End-to-end: stream → archive → first image bytes → decode → resize → HBITMAP.
///
/// Any failure propagates as `Err`; the caller logs it and returns
/// `E_FAIL` so Explorer falls back to the default icon.
fn try_generate_thumbnail(
    stream: IStream,
    cx: u32,
) -> std::result::Result<HBITMAP, Box<dyn StdError>> {
    let reader = ComStreamReader::new(stream);
    let (name, bytes) = archive::read_first_image(reader, settings::current())?;
    alog!("  picked: {name} ({} bytes)", bytes.len());

    // Format-dispatching decoder with pre-decode size guards against
    // decompression bombs.
    let img = decode::decode_with_limits(&name, &bytes)?;
    alog!("  decoded: {}x{}", img.width(), img.height());

    // Preserve aspect ratio, fit inside cx × cx. `Triangle` (bilinear)
    // is a good default — fast and visually fine at thumbnail sizes.
    let resized = img
        .resize(cx, cx, image::imageops::FilterType::Triangle)
        .to_rgba8();
    alog!("  resized: {}x{}", resized.width(), resized.height());

    let hbmp = bitmap::from_rgba(&resized)?;
    Ok(hbmp)
}

/// CLSID for the ArcThumb thumbnail provider COM class.
/// **DO NOT CHANGE** — baked into users' registries on install.
pub const CLSID_ARCTHUMB_PROVIDER: GUID = GUID::from_u128(0x0F4F5659_D383_4945_A534_01E1EED1D23F);

// =============================================================================
// IClassFactory
// =============================================================================

#[implement(IClassFactory)]
pub struct ArcThumbClassFactory;

impl IClassFactory_Impl for ArcThumbClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Option<&IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> Result<()> {
        // COM aggregation is an advanced feature we don't support.
        if punkouter.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        if ppvobject.is_null() || riid.is_null() {
            return Err(E_POINTER.into());
        }

        unsafe {
            *ppvobject = std::ptr::null_mut();
            // Create a fresh provider and hand it to the caller under
            // whatever interface they asked for (QueryInterface).
            let provider = ArcThumbProvider::default();
            let unknown: IUnknown = provider.into();
            unknown.query(&*riid, ppvobject).ok()
        }
    }

    fn LockServer(&self, _flock: BOOL) -> Result<()> {
        // No-op: we don't care whether the server is locked.
        Ok(())
    }
}

// =============================================================================
// ArcThumbProvider — IThumbnailProvider + IInitializeWithStream
// =============================================================================

/// The COM object Explorer actually talks to for each thumbnail request.
///
/// `stream` is populated by `IInitializeWithStream::Initialize`, then
/// consumed (eventually) by `IThumbnailProvider::GetThumbnail`. Phase 1
/// stores it but never reads from it.
#[implement(IThumbnailProvider, IInitializeWithStream)]
#[derive(Default)]
pub struct ArcThumbProvider {
    stream: RefCell<Option<IStream>>,
}

impl IInitializeWithStream_Impl for ArcThumbProvider_Impl {
    fn Initialize(&self, pstream: Option<&IStream>, _grfmode: u32) -> Result<()> {
        // Initialize is trivial but we still guard it: the #[implement]
        // glue calls it across the COM ABI, so a panic here would be UB.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            *self.this.stream.borrow_mut() = pstream.cloned();
            Ok(())
        }));
        match result {
            Ok(r) => r,
            Err(_) => {
                alog!("PANIC caught in Initialize");
                Err(windows::core::Error::from_hresult(E_FAIL))
            }
        }
    }
}

impl IThumbnailProvider_Impl for ArcThumbProvider_Impl {
    fn GetThumbnail(
        &self,
        cx: u32,
        phbmp: *mut HBITMAP,
        pdwalpha: *mut WTS_ALPHATYPE,
    ) -> Result<()> {
        // catch_unwind turns any panic inside our code (image decoder,
        // archive parser, allocator, …) into a clean COM error instead
        // of undefined behaviour across the C ABI boundary.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.get_thumbnail_inner(cx, phbmp, pdwalpha)
        }));
        match result {
            Ok(r) => r,
            Err(_) => {
                alog!("PANIC caught in GetThumbnail");
                Err(windows::core::Error::from_hresult(E_FAIL))
            }
        }
    }
}

impl ArcThumbProvider_Impl {
    fn get_thumbnail_inner(
        &self,
        cx: u32,
        phbmp: *mut HBITMAP,
        pdwalpha: *mut WTS_ALPHATYPE,
    ) -> Result<()> {
        if phbmp.is_null() || pdwalpha.is_null() {
            return Err(E_POINTER.into());
        }

        alog!("---- GetThumbnail cx={cx} ----");

        // Clamp to Windows's standard icon range. Explorer's largest
        // bucket is 2560 (Extra Large × high DPI); the lower bound is
        // defensive.
        let size = clamp_thumbnail_size(cx);

        let stream = self.this.stream.borrow().clone().ok_or_else(|| {
            alog!("  no stream attached");
            windows::core::Error::from_hresult(E_FAIL)
        })?;

        // On any failure (not-an-archive, no images inside, decode
        // error, …) we return an error HRESULT. Explorer then falls
        // back to the built-in handler's icon, which is the right UX:
        // archives without images should look like normal zips, not
        // like broken thumbnails.
        let hbmp = try_generate_thumbnail(stream, size).map_err(|e| {
            alog!("  no thumbnail: {e}");
            windows::core::Error::from_hresult(E_FAIL)
        })?;

        unsafe {
            *phbmp = hbmp;
            *pdwalpha = WTSAT_ARGB;
        }
        Ok(())
    }
}

/// Clamp a requested thumbnail size to the allowed range.
fn clamp_thumbnail_size(cx: u32) -> u32 {
    cx.clamp(limits::MIN_THUMBNAIL_SIZE, limits::MAX_THUMBNAIL_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_within_range_is_identity() {
        assert_eq!(clamp_thumbnail_size(64), 64);
        assert_eq!(clamp_thumbnail_size(256), 256);
        assert_eq!(
            clamp_thumbnail_size(limits::MIN_THUMBNAIL_SIZE),
            limits::MIN_THUMBNAIL_SIZE
        );
        assert_eq!(
            clamp_thumbnail_size(limits::MAX_THUMBNAIL_SIZE),
            limits::MAX_THUMBNAIL_SIZE
        );
    }

    #[test]
    fn clamp_below_minimum() {
        assert_eq!(clamp_thumbnail_size(0), limits::MIN_THUMBNAIL_SIZE);
        assert_eq!(clamp_thumbnail_size(1), limits::MIN_THUMBNAIL_SIZE);
        assert_eq!(
            clamp_thumbnail_size(limits::MIN_THUMBNAIL_SIZE - 1),
            limits::MIN_THUMBNAIL_SIZE
        );
    }

    #[test]
    fn clamp_above_maximum() {
        assert_eq!(clamp_thumbnail_size(u32::MAX), limits::MAX_THUMBNAIL_SIZE);
        assert_eq!(
            clamp_thumbnail_size(limits::MAX_THUMBNAIL_SIZE + 1),
            limits::MAX_THUMBNAIL_SIZE
        );
        assert_eq!(clamp_thumbnail_size(10000), limits::MAX_THUMBNAIL_SIZE);
    }

    #[test]
    fn clamp_standard_explorer_sizes() {
        // Explorer's common thumbnail size buckets.
        for size in [16, 32, 48, 64, 96, 128, 256, 512, 1024, 2560] {
            let clamped = clamp_thumbnail_size(size);
            assert_eq!(clamped, size, "standard size {size} should pass through");
        }
    }

    #[test]
    fn try_generate_thumbnail_rejects_garbage_stream() {
        use windows::Win32::UI::Shell::SHCreateMemStream;
        let garbage = b"this is not an archive at all";
        let stream: IStream =
            unsafe { SHCreateMemStream(Some(garbage)) }.expect("SHCreateMemStream");
        let result = try_generate_thumbnail(stream, 64);
        assert!(result.is_err(), "garbage data should fail");
    }

    #[test]
    fn try_generate_thumbnail_rejects_empty_stream() {
        use windows::Win32::UI::Shell::SHCreateMemStream;
        let stream: IStream = unsafe { SHCreateMemStream(Some(&[])) }.expect("SHCreateMemStream");
        let result = try_generate_thumbnail(stream, 64);
        assert!(result.is_err(), "empty stream should fail");
    }

    #[test]
    fn try_generate_thumbnail_succeeds_with_valid_zip() {
        use std::io::Cursor;
        use windows::Win32::Graphics::Gdi::{DeleteObject, HGDIOBJ};
        use windows::Win32::UI::Shell::SHCreateMemStream;

        // Build a valid ZIP containing a PNG
        let png = {
            use image::{DynamicImage, ImageBuffer, Rgba};
            let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
                ImageBuffer::from_fn(4, 4, |_, _| Rgba([255, 0, 0, 255]));
            let mut out = Vec::new();
            DynamicImage::ImageRgba8(img)
                .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
                .unwrap();
            out
        };
        let zip_bytes = {
            use zip::write::SimpleFileOptions;
            let mut buf = Vec::new();
            {
                let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
                let opts =
                    SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
                w.start_file("test.png", opts).unwrap();
                std::io::Write::write_all(&mut w, &png).unwrap();
                w.finish().unwrap();
            }
            buf
        };

        let stream: IStream =
            unsafe { SHCreateMemStream(Some(&zip_bytes)) }.expect("SHCreateMemStream");
        let hbmp = try_generate_thumbnail(stream, 64).expect("should succeed");
        assert!(!hbmp.is_invalid());
        // Clean up
        unsafe {
            let _ = DeleteObject(HGDIOBJ(hbmp.0));
        }
    }
}
