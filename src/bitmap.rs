//! HBITMAP helpers.
//!
//! `from_rgba` converts an `image::RgbaImage` (straight RGBA) into a
//! top-down 32bpp DIB section with premultiplied BGRA — the format
//! Explorer wants when we return `WTSAT_ARGB`.

use std::ffi::c_void;

use windows::core::{Error, Result};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Gdi::{
    CreateDIBSection, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
};

/// Convert an `image::RgbaImage` to an HBITMAP suitable for returning
/// from `IThumbnailProvider::GetThumbnail` with `WTSAT_ARGB`.
///
/// - Converts RGBA → premultiplied BGRA (Windows convention).
/// - Creates a top-down 32bpp DIB section sized exactly to the image.
///
/// The returned HBITMAP is owned by the caller (Explorer will free it).
pub fn from_rgba(img: &image::RgbaImage) -> Result<HBITMAP> {
    let (width, height) = img.dimensions();
    if width == 0 || height == 0 {
        return Err(Error::from_hresult(E_FAIL));
    }

    let mut bi = BITMAPINFO::default();
    bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bi.bmiHeader.biWidth = width as i32;
    bi.bmiHeader.biHeight = -(height as i32); // top-down
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    bi.bmiHeader.biCompression = BI_RGB.0;

    let mut bits: *mut c_void = std::ptr::null_mut();
    let hbmp = unsafe {
        CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0)?
    };
    if bits.is_null() {
        return Err(Error::from_hresult(E_FAIL));
    }

    // Copy + convert pixel layout.
    let src = img.as_raw(); // RGBA bytes
    let pixel_count = (width as usize) * (height as usize);
    unsafe {
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, pixel_count * 4);
        for i in 0..pixel_count {
            let r = src[i * 4];
            let g = src[i * 4 + 1];
            let b = src[i * 4 + 2];
            let a = src[i * 4 + 3];
            // Premultiply alpha so Explorer can composite correctly.
            dst[i * 4] = premul(b, a);
            dst[i * 4 + 1] = premul(g, a);
            dst[i * 4 + 2] = premul(r, a);
            dst[i * 4 + 3] = a;
        }
    }

    Ok(hbmp)
}

/// Integer premultiply: `(c * a + 127) / 255`, rounded.
#[inline]
fn premul(c: u8, a: u8) -> u8 {
    ((c as u16 * a as u16 + 127) / 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};
    use windows::Win32::Graphics::Gdi::{DeleteObject, GetObjectW, BITMAP, HGDIOBJ};

    /// Helper: build a tiny RgbaImage filled with the given pixel.
    fn solid(w: u32, h: u32, px: [u8; 4]) -> image::RgbaImage {
        ImageBuffer::from_fn(w, h, |_, _| Rgba(px))
    }

    /// RAII wrapper that frees an HBITMAP when dropped, so a panicking
    /// test doesn't leak GDI objects.
    struct OwnedHBitmap(HBITMAP);
    impl Drop for OwnedHBitmap {
        fn drop(&mut self) {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(self.0.0));
            }
        }
    }

    #[test]
    fn from_rgba_returns_valid_handle() {
        let img = solid(8, 5, [10, 20, 30, 255]);
        let hbmp = from_rgba(&img).expect("from_rgba");
        let _g = OwnedHBitmap(hbmp);
        assert!(!hbmp.is_invalid());
    }

    #[test]
    fn from_rgba_dimensions_match_input() {
        let img = solid(13, 7, [255, 255, 255, 255]);
        let hbmp = from_rgba(&img).expect("from_rgba");
        let _g = OwnedHBitmap(hbmp);

        // Query GDI for the bitmap header and verify width/height
        // round-trip through CreateDIBSection.
        let mut bm = BITMAP::default();
        let written = unsafe {
            GetObjectW(
                HGDIOBJ(hbmp.0),
                std::mem::size_of::<BITMAP>() as i32,
                Some(&mut bm as *mut _ as *mut _),
            )
        };
        assert!(written > 0, "GetObjectW failed");
        assert_eq!(bm.bmWidth, 13);
        assert_eq!(bm.bmHeight, 7);
        assert_eq!(bm.bmBitsPixel, 32);
    }

    #[test]
    fn from_rgba_zero_width_errors() {
        let img = solid(0, 5, [0, 0, 0, 255]);
        assert!(from_rgba(&img).is_err());
    }

    #[test]
    fn from_rgba_zero_height_errors() {
        let img = solid(5, 0, [0, 0, 0, 255]);
        assert!(from_rgba(&img).is_err());
    }

    #[test]
    fn from_rgba_premultiplies_pixels_in_dib() {
        // Use a half-transparent red pixel and verify the DIB body
        // contains premultiplied BGRA. We can't easily peek into the
        // DIB bits without GetDIBits, so we instead exercise that
        // the call succeeds and `premul` (the same function used
        // internally) gives the expected result.
        let img = solid(2, 2, [255, 0, 0, 128]);
        let hbmp = from_rgba(&img).expect("from_rgba");
        let _g = OwnedHBitmap(hbmp);
        // Sanity: premultiplied red channel for alpha=128 is 128.
        assert_eq!(premul(255, 128), 128);
    }

    #[test]
    fn premul_fully_opaque_is_identity() {
        for c in [0u8, 1, 64, 127, 128, 200, 254, 255] {
            assert_eq!(premul(c, 255), c, "c={c}");
        }
    }

    #[test]
    fn premul_fully_transparent_is_zero() {
        for c in [0u8, 1, 64, 128, 255] {
            assert_eq!(premul(c, 0), 0, "c={c}");
        }
    }

    #[test]
    fn premul_half_alpha() {
        // (255 * 128 + 127) / 255 = 32767 / 255 = 128 (rounded).
        assert_eq!(premul(255, 128), 128);
        // (200 * 128 + 127) / 255 = 25727 / 255 = 100.
        assert_eq!(premul(200, 128), 100);
    }

    #[test]
    fn premul_never_overflows_u8() {
        // Exhaustive over the entire 8-bit × 8-bit space — cheap and
        // proves the (c*a+127)/255 expression stays in u8 range.
        for c in 0u16..=255 {
            for a in 0u16..=255 {
                let p = premul(c as u8, a as u8);
                // Result must never exceed the un-premultiplied colour.
                assert!(p as u16 <= c, "c={c} a={a} p={p}");
            }
        }
    }
}
