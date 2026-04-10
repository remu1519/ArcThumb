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
//! extension is in `IMAGE_EXTS`.

mod fb2;
mod mobi;
mod rar;
mod sevenz;
mod tar;
mod zip;

use std::error::Error;
use std::io::{Read, Seek, SeekFrom};

use crate::limits;

/// Image extensions we recognise inside archives. Case-insensitive.
/// Must match what `decode::decode_with_limits` can actually handle —
/// listing an extension we can't decode would cause us to pick an
/// unreadable file as the "first image" and fail to produce a
/// thumbnail at all.
const IMAGE_EXTS: &[&str] = &[
    "jpg",
    "jpeg",
    "png",
    "gif",
    "bmp",
    "tiff",
    "tif",
    "webp",
    "ico",
    #[cfg(feature = "jxl")]
    "jxl",
];

pub(crate) fn has_image_ext(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    IMAGE_EXTS
        .iter()
        .any(|ext| lower.ends_with(&format!(".{ext}")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Zip,
    SevenZ,
    Rar,
    Tar,
    /// FictionBook 2 raw XML. Detected by searching for the literal
    /// `FictionBook` in the first 512 bytes — XML declarations may
    /// appear before the root element so we can't anchor at start.
    Fb2,
    /// Amazon Kindle MOBI / AZW / AZW3. All three are PalmDB
    /// containers with the type `BOOK` + creator `MOBI` at offset
    /// 60..68 inside the PalmDB header.
    Mobi,
    Unknown,
}

fn detect_format(magic: &[u8]) -> Format {
    // ZIP: "PK" followed by \x03\x04 (local file header), \x05\x06 (empty),
    // or \x07\x08 (spanned).
    if magic.len() >= 4 && &magic[..2] == b"PK" {
        let m2 = magic[2];
        let m3 = magic[3];
        if (m2 == 3 && m3 == 4) || (m2 == 5 && m3 == 6) || (m2 == 7 && m3 == 8) {
            return Format::Zip;
        }
    }
    // 7z: "7z\xBC\xAF\x27\x1C"
    if magic.len() >= 6 && &magic[..6] == b"7z\xBC\xAF\x27\x1C" {
        return Format::SevenZ;
    }
    // RAR 4: "Rar!\x1A\x07\x00"; RAR 5: "Rar!\x1A\x07\x01\x00"
    if magic.len() >= 7 && &magic[..7] == b"Rar!\x1A\x07\x00" {
        return Format::Rar;
    }
    if magic.len() >= 8 && &magic[..8] == b"Rar!\x1A\x07\x01\x00" {
        return Format::Rar;
    }
    // TAR (ustar): the string "ustar" lives at byte offset 257 inside the
    // 512-byte header. This covers POSIX ustar and pax archives, which is
    // what modern tools (including 7-Zip, tar, bsdtar) produce.
    if magic.len() >= 262 && &magic[257..262] == b"ustar" {
        return Format::Tar;
    }
    // FB2: a single XML document with the literal `FictionBook` root
    // element. The token is unique enough that false positives are
    // effectively impossible — no other widely-deployed format mentions
    // `FictionBook` in its first 512 bytes.
    if magic.windows(11).any(|w| w == b"FictionBook") {
        return Format::Fb2;
    }
    // MOBI / AZW / AZW3: PalmDB header has type "BOOK" at offset 60
    // and creator "MOBI" at offset 64. The combined "BOOKMOBI" string
    // at byte 60 uniquely identifies the format.
    if magic.len() >= 68 && &magic[60..68] == b"BOOKMOBI" {
        return Format::Mobi;
    }
    Format::Unknown
}

/// Open an archive stream, pick the first image, return `(name, bytes)`.
pub fn read_first_image<R: Read + Seek>(
    mut reader: R,
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
        Format::Zip => zip::zip_read_first_image(reader),
        Format::SevenZ => sevenz::sevenz_read_first_image(reader),
        Format::Rar => rar::rar_read_first_image(reader),
        Format::Tar => tar::tar_read_first_image(reader),
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
    // has_image_ext
    // ---------------------------------------------------------------

    #[test]
    fn image_ext_recognised_lowercase() {
        for ext in &[
            "jpg", "jpeg", "png", "gif", "bmp", "tiff", "tif", "webp", "ico",
        ] {
            assert!(has_image_ext(&format!("foo.{ext}")), "ext={ext}");
        }
    }

    #[test]
    fn image_ext_case_insensitive() {
        assert!(has_image_ext("foo.JPG"));
        assert!(has_image_ext("foo.PnG"));
        assert!(has_image_ext("comic/CHAPTER1/01.WEBP"));
    }

    #[test]
    fn image_ext_rejects_non_images() {
        assert!(!has_image_ext("foo.txt"));
        assert!(!has_image_ext("foo.zip"));
        assert!(!has_image_ext("README"));
        assert!(!has_image_ext(""));
    }

    #[test]
    fn image_ext_does_not_match_substring() {
        // The extension check requires a literal "." separator,
        // so "foopng" must not be treated as a PNG.
        assert!(!has_image_ext("foopng"));
        assert!(!has_image_ext("imagejpg"));
    }

    #[test]
    fn unknown_format_errors_cleanly() {
        let bytes = b"this is plain text, definitely not an archive".to_vec();
        let result = read_first_image(Cursor::new(bytes));
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
