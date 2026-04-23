//! Archive reading: dispatch by detected magic bytes to a format-specific
//! backend, return the first image file as `(name, bytes)`.
//!
//! Supported formats:
//! - **ZIP** (`PK\x03\x04`) — via `zip` crate, direct Read+Seek
//! - **7z**  (`7z\xBC\xAF\x27\x1C`) — via `sevenz-rust`, direct Read+Seek
//! - **RAR** (`Rar!\x1A\x07\x00` / `Rar!\x1A\x07\x01\x00`) — via `unrar`,
//!   which insists on a file path, so we spool the stream to `%TEMP%`.
//! - **TAR/CBT** (`ustar` at offset 257) — via `tar` crate, Read only
//!   (we use Seek to rewind between listing and extraction passes)
//!
//! "First image" is defined as the alphabetically smallest file whose
//! extension is in `settings::SUPPORTED_IMAGE_EXTS` AND whose bit is
//! set in the user's `enabled_image_exts_mask`.

mod detect;
mod fb2;
mod mobi;
mod rar;
mod sevenz;
mod tar;
mod zip;

use std::error::Error;
use std::io::{Read, Seek, SeekFrom};

use crate::limits;
#[cfg(test)]
use crate::settings::SUPPORTED_IMAGE_EXTS;
use crate::settings::Settings;

use detect::{Format, detect_format};

/// Open an archive stream, pick the first image, return `(name, bytes)`.
///
/// The caller supplies the [`Settings`] snapshot that governs
/// image-extension filtering and sort order. This keeps the
/// archive module free of global state — the shell extension
/// obtains settings via [`settings::current()`] and passes them in.
pub fn read_first_image<R: Read + Seek>(
    mut reader: R,
    settings: &Settings,
) -> Result<(String, Vec<u8>), Box<dyn Error>> {
    // Size guard: check total stream length before touching any parser.
    let total = reader.seek(SeekFrom::End(0))?;
    if total > limits::MAX_ARCHIVE_SIZE {
        return Err(format!(
            "archive too large ({total} bytes > {} limit)",
            limits::MAX_ARCHIVE_SIZE
        )
        .into());
    }

    // Read enough of the header for the `ustar` magic at offset 257.
    // `Read::read` may return short; `take().read_to_end()` is the
    // idiomatic "read up to N bytes greedily" pattern.
    reader.seek(SeekFrom::Start(0))?;
    let mut magic: Vec<u8> = Vec::with_capacity(512);
    reader.by_ref().take(512).read_to_end(&mut magic)?;
    reader.seek(SeekFrom::Start(0))?;

    match detect_format(&magic) {
        Format::Zip => zip::zip_read_first_image(reader, settings),
        Format::SevenZ => sevenz::sevenz_read_first_image(reader, settings),
        Format::Rar => rar::rar_read_first_image(reader, settings),
        Format::Tar => tar::tar_read_first_image(reader, settings),
        Format::Fb2 => fb2::fb2_read_first_image(reader),
        Format::Mobi => mobi::mobi_read_first_image(reader),
        Format::Unknown => Err("unrecognised archive format".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ---------------------------------------------------------------
    // detect_format (shared / unknown cases)
    // ---------------------------------------------------------------

    #[test]
    fn detect_unknown_for_random_bytes() {
        assert_eq!(detect_format(b"this is not an archive"), Format::Unknown);
    }

    #[test]
    fn detect_unknown_for_short_input() {
        assert_eq!(detect_format(b""), Format::Unknown);
        assert_eq!(detect_format(b"PK"), Format::Unknown);
    }

    // ---------------------------------------------------------------
    // Settings::accepts_image_ext
    // ---------------------------------------------------------------

    fn settings_with_mask(mask: u32) -> Settings {
        Settings {
            enabled_image_exts_mask: mask,
            ..Settings::default()
        }
    }

    #[test]
    fn image_ext_recognised_lowercase() {
        let s = Settings::default();
        for ext in &[
            "jpg", "jpeg", "png", "gif", "bmp", "tiff", "tif", "webp", "ico",
        ] {
            assert!(s.accepts_image_ext(&format!("foo.{ext}")), "ext={ext}");
        }
    }

    #[test]
    fn image_ext_case_insensitive() {
        let s = Settings::default();
        assert!(s.accepts_image_ext("foo.JPG"));
        assert!(s.accepts_image_ext("foo.PnG"));
        assert!(s.accepts_image_ext("comic/CHAPTER1/01.WEBP"));
    }

    #[test]
    fn image_ext_rejects_non_images() {
        let s = Settings::default();
        assert!(!s.accepts_image_ext("foo.txt"));
        assert!(!s.accepts_image_ext("foo.zip"));
        assert!(!s.accepts_image_ext("README"));
        assert!(!s.accepts_image_ext(""));
    }

    #[test]
    fn image_ext_does_not_match_substring() {
        let s = Settings::default();
        assert!(!s.accepts_image_ext("foopng"));
        assert!(!s.accepts_image_ext("imagejpg"));
    }

    #[test]
    fn mask_disables_specific_extensions() {
        assert!(settings_with_mask(0b1).accepts_image_ext("a.jpg"));
        assert!(!settings_with_mask(0b1).accepts_image_ext("a.png"));
        assert!(!settings_with_mask(0).accepts_image_ext("a.jpg"));
        let png_idx = SUPPORTED_IMAGE_EXTS
            .iter()
            .position(|&e| e == ".png")
            .unwrap();
        let s = settings_with_mask(1u32 << png_idx);
        assert!(s.accepts_image_ext("a.png"));
        assert!(!s.accepts_image_ext("a.jpg"));
    }

    #[test]
    fn every_supported_extension_can_be_solo_enabled() {
        for (i, target_ext) in SUPPORTED_IMAGE_EXTS.iter().enumerate() {
            let s = settings_with_mask(1u32 << i);
            let target_name = format!("foo{target_ext}");
            assert!(
                s.accepts_image_ext(&target_name),
                "{target_ext} should be recognised when its own bit (index {i}) is set"
            );
            for (j, other_ext) in SUPPORTED_IMAGE_EXTS.iter().enumerate() {
                if i == j {
                    continue;
                }
                if other_ext.ends_with(target_ext) || target_ext.ends_with(other_ext) {
                    continue;
                }
                let other_name = format!("bar{other_ext}");
                assert!(
                    !s.accepts_image_ext(&other_name),
                    "{other_ext} must NOT match when only {target_ext} (bit {i}) is set"
                );
            }
        }
    }

    #[test]
    fn every_supported_extension_can_be_solo_disabled() {
        let all = crate::settings::default_enabled_image_exts_mask();
        for (i, target_ext) in SUPPORTED_IMAGE_EXTS.iter().enumerate() {
            let mask = all & !(1u32 << i);
            let s = settings_with_mask(mask);
            let target_name = format!("foo{target_ext}");
            // Skip asymmetric suffix overlaps: disabling `.tif`
            // (index 6) doesn't reject `.tiff` because `.tiff` also
            // ends with `.tif`'s longer cousin — but in our slice
            // `.tiff` comes before `.tif`, so a plain `.tif` file
            // can still match the `.tiff` bit. Assert only when no
            // other bit could "catch" this extension via ends_with.
            let another_matches = SUPPORTED_IMAGE_EXTS
                .iter()
                .enumerate()
                .any(|(j, e)| j != i && (mask & (1u32 << j)) != 0 && target_ext.ends_with(e));
            if another_matches {
                continue;
            }
            assert!(
                !s.accepts_image_ext(&target_name),
                "{target_ext} should be rejected when only its bit (index {i}) is cleared"
            );
        }
        // Sanity: default mask accepts every supported extension.
        let default = Settings::default();
        for ext in SUPPORTED_IMAGE_EXTS {
            let name = format!("foo{ext}");
            assert!(
                default.accepts_image_ext(&name),
                "{ext} should match under the default (all-on) mask"
            );
        }
    }

    #[test]
    fn mask_matches_are_case_insensitive() {
        let s = Settings::default();
        for ext in SUPPORTED_IMAGE_EXTS {
            let upper = format!("FOO{}", ext.to_uppercase());
            assert!(
                s.accepts_image_ext(&upper),
                "uppercase {ext} should still match"
            );
        }
    }

    #[test]
    fn unknown_format_errors_cleanly() {
        let bytes = b"this is plain text, definitely not an archive".to_vec();
        let result = read_first_image(Cursor::new(bytes), &Settings::default());
        assert!(result.is_err());
    }

    // ---------------------------------------------------------------
    // Shared test helpers (used by sub-module tests)
    // ---------------------------------------------------------------

    /// Build a tiny PNG via the `image` crate so the fixtures
    /// contain plausible image bytes.
    pub(crate) fn make_tiny_png() -> Vec<u8> {
        use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba};
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_fn(2, 2, |_, _| Rgba([0, 128, 255, 255]));
        let mut out = Vec::new();
        DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    /// Build a minimal valid FB2 document containing a single
    /// base64-encoded image binary referenced by the coverpage.
    pub(crate) fn build_fb2(cover_id: &str, png_bytes: &[u8]) -> Vec<u8> {
        use base64::Engine;
        use base64::engine::general_purpose::STANDARD as B64;
        let b64 = B64.encode(png_bytes);
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<FictionBook xmlns=\"http://www.gribuser.ru/xml/fictionbook/2.0\" \
xmlns:l=\"http://www.w3.org/1999/xlink\">\n\
  <description>\n\
    <title-info>\n\
      <coverpage>\n\
        <image l:href=\"#{cover_id}\"/>\n\
      </coverpage>\n\
    </title-info>\n\
  </description>\n\
  <body><section><p>book text</p></section></body>\n\
  <binary id=\"{cover_id}\" content-type=\"image/png\">{b64}</binary>\n\
</FictionBook>"
        )
        .into_bytes()
    }
}
