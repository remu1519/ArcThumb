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
