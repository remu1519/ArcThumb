//! Image decoding with format-specific dispatch.
//!
//! Most formats (JPEG/PNG/GIF/BMP/TIFF/ICO/WebP) go through the
//! `image` crate's `ImageReader`, which auto-detects format from
//! magic bytes and enforces pre-decode dimension/allocation limits.
//!
//! **JXL** (JPEG XL) is gated behind the `jxl` Cargo feature. When
//! enabled, decoding uses `jxl-oxide` — a pure-Rust JPEG XL decoder
//! — via its `image` integration. Disabled by default because it
//! adds ~2.3 MB to the DLL for a format that is not yet widely
//! deployed. Build with `cargo build --release --features jxl` to
//! enable.
//!
//! **AVIF / HEIC** are intentionally not supported. Both require C
//! library dependencies (`libavif`, `libheif`) that conflict with
//! the "cargo build alone, no system libraries" philosophy of this
//! project. They may return in a later phase if pure-Rust decoders
//! mature enough for production use.

use std::error::Error;
use std::io::Cursor;

use image::{DynamicImage, ImageReader, Limits};
#[cfg(feature = "jxl")]
use image::ImageDecoder;

use crate::limits;

/// Decode image bytes into a `DynamicImage`, dispatching by filename
/// extension. The format is still verified by the underlying decoder,
/// so a mislabeled file fails cleanly with a decode error rather
/// than misbehaving.
pub fn decode_with_limits(name: &str, bytes: &[u8]) -> Result<DynamicImage, Box<dyn Error>> {
    #[cfg(feature = "jxl")]
    if name.to_ascii_lowercase().ends_with(".jxl") {
        return decode_jxl(bytes);
    }
    let _ = name; // only used by the `jxl` branch
    decode_via_image_crate(bytes)
}

fn make_limits() -> Limits {
    let mut l = Limits::default();
    l.max_image_width = Some(limits::MAX_IMAGE_DIMENSION);
    l.max_image_height = Some(limits::MAX_IMAGE_DIMENSION);
    l.max_alloc = Some(limits::MAX_IMAGE_ALLOC);
    l
}

fn decode_via_image_crate(bytes: &[u8]) -> Result<DynamicImage, Box<dyn Error>> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    reader.limits(make_limits());
    Ok(reader.decode()?)
}

#[cfg(feature = "jxl")]
fn decode_jxl(bytes: &[u8]) -> Result<DynamicImage, Box<dyn Error>> {
    use jxl_oxide::integration::JxlDecoder;

    let decoder = JxlDecoder::new(Cursor::new(bytes))?;
    let (w, h) = decoder.dimensions();

    // Pre-decode size guard — `image::Limits` isn't honoured by every
    // third-party decoder, so we check manually before committing.
    if w > limits::MAX_IMAGE_DIMENSION || h > limits::MAX_IMAGE_DIMENSION {
        return Err(format!("JXL dimensions too large: {w}x{h}").into());
    }
    let pixel_bytes = (w as u64)
        .saturating_mul(h as u64)
        .saturating_mul(4);
    if pixel_bytes > limits::MAX_IMAGE_ALLOC {
        return Err(format!(
            "JXL would allocate {pixel_bytes} bytes, exceeds limit"
        )
        .into());
    }

    Ok(DynamicImage::from_decoder(decoder)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    /// Encode a tiny solid-colour image to PNG bytes via the `image`
    /// crate. Used as a known-good fixture for round-tripping through
    /// `decode_with_limits`.
    fn make_png(w: u32, h: u32) -> Vec<u8> {
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_fn(w, h, |_, _| Rgba([10, 20, 30, 255]));
        let mut out = Vec::new();
        DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    fn make_jpeg(w: u32, h: u32) -> Vec<u8> {
        let img: ImageBuffer<image::Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(w, h, |_, _| image::Rgb([200, 100, 50]));
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Jpeg)
            .unwrap();
        out
    }

    #[test]
    fn decode_png_roundtrip() {
        let bytes = make_png(8, 5);
        let img = decode_with_limits("foo.png", &bytes).expect("decode");
        assert_eq!(img.width(), 8);
        assert_eq!(img.height(), 5);
    }

    #[test]
    fn decode_jpeg_roundtrip() {
        let bytes = make_jpeg(16, 9);
        let img = decode_with_limits("foo.jpg", &bytes).expect("decode");
        assert_eq!(img.width(), 16);
        assert_eq!(img.height(), 9);
    }

    #[test]
    fn decode_uses_content_not_filename() {
        // The file is named .jpg but the bytes are PNG. Decoding
        // should still succeed because the image crate sniffs the
        // magic bytes rather than trusting the filename.
        let bytes = make_png(4, 4);
        let img = decode_with_limits("lying.jpg", &bytes).expect("decode");
        assert_eq!(img.width(), 4);
    }

    #[test]
    fn decode_rejects_garbage() {
        let bytes = b"this is not an image at all";
        assert!(decode_with_limits("foo.png", bytes).is_err());
    }

    #[test]
    fn decode_rejects_empty() {
        assert!(decode_with_limits("foo.png", b"").is_err());
    }

    #[test]
    fn limits_are_set_from_module_constants() {
        // Guard against a future refactor that forgets to wire the
        // image crate's Limits to our `limits` module.
        let l = make_limits();
        assert_eq!(l.max_image_width, Some(limits::MAX_IMAGE_DIMENSION));
        assert_eq!(l.max_image_height, Some(limits::MAX_IMAGE_DIMENSION));
        assert_eq!(l.max_alloc, Some(limits::MAX_IMAGE_ALLOC));
    }
}
