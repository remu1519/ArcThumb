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

#[cfg(feature = "jxl")]
use image::ImageDecoder;
use image::{DynamicImage, ImageReader, Limits};

use crate::limits;

/// Decode image bytes into a full-resolution `DynamicImage`,
/// dispatching by filename extension. The format is still verified
/// by the underlying decoder, so a mislabeled file fails cleanly
/// with a decode error rather than misbehaving.
///
/// This always decodes at full resolution. For the thumbnail path
/// (where only a small target size is needed) prefer
/// [`decode_for_thumbnail`], which skips a large fraction of the
/// JPEG decode cost by asking libjpeg for a pre-scaled output.
pub fn decode_with_limits(name: &str, bytes: &[u8]) -> Result<DynamicImage, Box<dyn Error>> {
    #[cfg(feature = "jxl")]
    if name.to_ascii_lowercase().ends_with(".jxl") {
        return decode_jxl(bytes);
    }
    let _ = name; // only used by the `jxl` branch
    decode_via_image_crate(bytes)
}

/// Decode with an intent to resize down to `target_px` on the
/// longest side. For JPEG inputs, this uses the codec's native
/// 1/2 / 1/4 / 1/8 DCT scaling so a 2000×3000 comic page is
/// delivered at ~512×768 (scale=1/4) instead of being decoded at
/// full resolution only to be thrown away by a subsequent
/// `resize`. For other formats — PNG, WebP, GIF, etc. — returns
/// the full-resolution image, because their decoders do not
/// support sub-resolution decoding in any meaningful form.
///
/// The caller is still expected to run a final high-quality
/// `resize` pass to hit the exact target; this function only
/// shrinks the *input* cost.
pub fn decode_for_thumbnail(
    name: &str,
    bytes: &[u8],
    target_px: u32,
) -> Result<DynamicImage, Box<dyn Error>> {
    #[cfg(feature = "jxl")]
    if name.to_ascii_lowercase().ends_with(".jxl") {
        return decode_jxl(bytes);
    }
    let _ = name;

    if let Some(img) = try_decode_jpeg_scaled(bytes, target_px)? {
        return Ok(img);
    }
    decode_via_image_crate(bytes)
}

/// JPEG magic bytes: every JPEG starts with `FF D8 FF`. Sniffing
/// the content (not the filename) means a `.png` that is actually
/// a JPEG still hits the fast path, and — more importantly — a
/// `.jpg` that is actually a PNG falls through instead of crashing
/// the JPEG decoder.
fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.len() >= 3 && &bytes[..3] == b"\xFF\xD8\xFF"
}

/// If `bytes` is a JPEG, decode it via `jpeg-decoder` using the
/// largest DCT scale factor (1/1, 1/2, 1/4, 1/8) whose output is
/// still ≥ `target_px * 2`. The ×2 headroom is so the subsequent
/// high-quality resize has enough input to work with — scaling
/// down 2× in the resizer looks much better than scaling up 1.2×
/// from an over-shrunken source.
///
/// `jpeg-decoder` handles the "source already small" case itself:
/// `scale(requested)` returns 1/1 when no sub-resolution factor
/// produces an output ≥ `requested`.
///
/// Returns `Ok(None)` when the input isn't JPEG (magic bytes don't
/// match) or when the source's pixel format isn't one we can map
/// to a `DynamicImage` losslessly — the caller then falls through
/// to the generic `image`-crate decoder. Decode errors propagate.
fn try_decode_jpeg_scaled(
    bytes: &[u8],
    target_px: u32,
) -> Result<Option<DynamicImage>, Box<dyn Error>> {
    if !is_jpeg(bytes) {
        return Ok(None);
    }
    use image::{ImageBuffer, Luma, Rgb};
    use jpeg_decoder::{Decoder, PixelFormat};

    let mut decoder = Decoder::new(Cursor::new(bytes));
    decoder.read_info()?;
    let info = decoder
        .info()
        .ok_or("JPEG info unavailable after read_info")?;

    // Pre-scale dimension guard — matches the cap the `image` crate
    // would enforce via its `Limits` on the full-decode path.
    let src_w = info.width as u32;
    let src_h = info.height as u32;
    if src_w > limits::MAX_IMAGE_DIMENSION || src_h > limits::MAX_IMAGE_DIMENSION {
        return Err(format!("JPEG dimensions too large: {src_w}x{src_h}").into());
    }

    // Ask for target×2. jpeg-decoder picks the largest supported
    // scale (8/4/2/1) whose output on the longer axis is still
    // ≥ requested. Small sources collapse to 1/1 naturally.
    let (out_w, out_h) = if target_px > 0 {
        let requested = target_px.saturating_mul(2).min(u16::MAX as u32) as u16;
        decoder.scale(requested, requested)?
    } else {
        (info.width, info.height)
    };
    let w = out_w as u32;
    let h = out_h as u32;

    // Post-scale allocation guard. Even after a successful scale the
    // output could be enormous if the source was 16000×16000.
    let bpp = match info.pixel_format {
        PixelFormat::L8 => 1u64,
        PixelFormat::L16 => 2,
        PixelFormat::RGB24 => 3,
        PixelFormat::CMYK32 => 4,
    };
    let pixel_bytes = (w as u64).saturating_mul(h as u64).saturating_mul(bpp);
    if pixel_bytes > limits::MAX_IMAGE_ALLOC {
        return Err(
            format!("JPEG decoded buffer would exceed allocation limit: {pixel_bytes}").into(),
        );
    }

    let pixels = decoder.decode()?;

    let img = match info.pixel_format {
        PixelFormat::RGB24 => {
            let buf: ImageBuffer<Rgb<u8>, Vec<u8>> =
                ImageBuffer::from_raw(w, h, pixels).ok_or("JPEG RGB24 buffer size mismatch")?;
            DynamicImage::ImageRgb8(buf)
        }
        PixelFormat::L8 => {
            let buf: ImageBuffer<Luma<u8>, Vec<u8>> =
                ImageBuffer::from_raw(w, h, pixels).ok_or("JPEG L8 buffer size mismatch")?;
            DynamicImage::ImageLuma8(buf)
        }
        // L16 (16-bit grey) and CMYK32 are rare in JPEGs and need
        // a colour-space conversion we'd rather not hand-roll. Fall
        // back to the full-featured `image`-crate decoder for these.
        PixelFormat::L16 | PixelFormat::CMYK32 => return Ok(None),
    };
    Ok(Some(img))
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
    let pixel_bytes = (w as u64).saturating_mul(h as u64).saturating_mul(4);
    if pixel_bytes > limits::MAX_IMAGE_ALLOC {
        return Err(format!("JXL would allocate {pixel_bytes} bytes, exceeds limit").into());
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
    fn decode_for_thumbnail_large_jpeg_is_scaled_down() {
        // Source 2048×2048. Target 256. The JPEG DCT scale options
        // are 1/1, 1/2, 1/4, 1/8 — the smallest that still leaves
        // >= target*2 (512) on the long side is 1/2 (→ 1024) since
        // 1/4 (→ 512) is exactly the threshold. Either 1/2 or 1/4
        // is acceptable here; what we verify is that we got back an
        // image materially smaller than the source.
        let bytes = make_jpeg(2048, 2048);
        let img = decode_for_thumbnail("big.jpg", &bytes, 256).expect("decode_for_thumbnail");
        assert!(
            img.width() < 2048,
            "expected scaled-down decode, got {}×{}",
            img.width(),
            img.height()
        );
        // And it should be >= target*2 so the resizer has enough
        // input quality to work with.
        assert!(
            img.width() >= 512,
            "scaled output too small: {}×{}",
            img.width(),
            img.height()
        );
    }

    #[test]
    fn decode_for_thumbnail_small_jpeg_is_not_scaled() {
        // Source 128×128. Target 256. No scaling possible (source
        // is already smaller than target*2), so we get the full
        // image back.
        let bytes = make_jpeg(128, 128);
        let img = decode_for_thumbnail("small.jpg", &bytes, 256).expect("decode_for_thumbnail");
        assert_eq!(img.width(), 128);
        assert_eq!(img.height(), 128);
    }

    #[test]
    fn decode_for_thumbnail_png_falls_through_to_full_decode() {
        // PNG doesn't support sub-resolution decoding in the image
        // crate, so we expect the full image back even under
        // `decode_for_thumbnail`.
        let bytes = make_png(512, 512);
        let img = decode_for_thumbnail("foo.png", &bytes, 64).expect("decode_for_thumbnail");
        assert_eq!(img.width(), 512);
        assert_eq!(img.height(), 512);
    }

    #[test]
    fn decode_for_thumbnail_png_labeled_jpg_is_decoded_correctly() {
        // Magic-bytes sniffing: a PNG named `.jpg` must NOT hit the
        // JPEG scale path (it would crash the JPEG decoder).
        let bytes = make_png(64, 64);
        let img = decode_for_thumbnail("lying.jpg", &bytes, 32).expect("decode_for_thumbnail");
        assert_eq!(img.width(), 64);
    }

    #[test]
    fn decode_for_thumbnail_jpeg_labeled_png_still_scales() {
        // Inverse: a JPEG named `.png` should still benefit from
        // the scaled path, because we check magic bytes, not name.
        let bytes = make_jpeg(1024, 1024);
        let img = decode_for_thumbnail("lying.png", &bytes, 128).expect("decode_for_thumbnail");
        assert!(img.width() < 1024, "expected scaled-down JPEG");
    }

    #[test]
    fn decode_for_thumbnail_rejects_garbage() {
        assert!(decode_for_thumbnail("foo.jpg", b"not an image", 64).is_err());
    }

    #[test]
    fn decode_for_thumbnail_target_zero_is_full_decode() {
        // Defensive: a target of 0 shouldn't crash. We just decode
        // the source at full resolution.
        let bytes = make_jpeg(64, 64);
        let img = decode_for_thumbnail("foo.jpg", &bytes, 0).expect("decode_for_thumbnail");
        assert_eq!(img.width(), 64);
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
