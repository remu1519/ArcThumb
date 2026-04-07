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

use std::error::Error;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::{ebook, limits, settings};

/// Image extensions we recognise inside archives. Case-insensitive.
/// Must match what `decode::decode_with_limits` can actually handle —
/// listing an extension we can't decode would cause us to pick an
/// unreadable file as the "first image" and fail to produce a
/// thumbnail at all.
const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "bmp", "tiff", "tif", "webp", "ico",
    #[cfg(feature = "jxl")]
    "jxl",
];

fn has_image_ext(name: &str) -> bool {
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
        Format::Zip => zip_read_first_image(reader),
        Format::SevenZ => sevenz_read_first_image(reader),
        Format::Rar => rar_read_first_image(reader),
        Format::Tar => tar_read_first_image(reader),
        Format::Fb2 => fb2_read_first_image(reader),
        Format::Unknown => Err("unrecognised archive format".into()),
    }
}

/// Look for an `.fb2` entry inside an already-opened ZIP archive
/// (the `.fb2.zip` distribution convention). Returns `None` if no
/// `.fb2` entry exists or the FB2 cover extraction fails.
fn try_extract_fb2_from_zip<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Option<(String, Vec<u8>)> {
    // First pass: find the first `.fb2` entry's name. We can't hold
    // a borrow into the archive across the second access so we copy
    // the name out.
    let fb2_name: String = (0..archive.len()).find_map(|i| {
        let f = archive.by_index(i).ok()?;
        if !f.is_file() {
            return None;
        }
        if !f.name().to_ascii_lowercase().ends_with(".fb2") {
            return None;
        }
        if f.size() > limits::MAX_ENTRY_SIZE {
            return None;
        }
        Some(f.name().to_string())
    })?;

    // Second pass: extract that entry's bytes and pass to the FB2
    // cover extractor.
    let mut entry = archive.by_name(&fb2_name).ok()?;
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut bytes).ok()?;
    ebook::fb2::try_extract_cover(&bytes)
}

// =============================================================================
// FB2 backend (raw XML, not an archive)
//
// FB2 is single XML document where images live as base64 inside
// `<binary>` elements. The actual decoding lives in `ebook::fb2`;
// this function just slurps the file (already size-checked above)
// and dispatches.
// =============================================================================

fn fb2_read_first_image<R: Read + Seek>(
    mut reader: R,
) -> Result<(String, Vec<u8>), Box<dyn Error>> {
    reader.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    ebook::fb2::try_extract_cover(&bytes)
        .ok_or_else(|| "FB2 has no embedded cover image".into())
}

// =============================================================================
// ZIP backend
// =============================================================================

fn zip_read_first_image<R: Read + Seek>(
    mut reader: R,
) -> Result<(String, Vec<u8>), Box<dyn Error>> {
    reader.seek(SeekFrom::Start(0))?;
    let mut archive = zip::ZipArchive::new(reader)?;

    // EPUB fast path: if the archive carries `META-INF/container.xml`,
    // we can pull the cover from the OPF metadata directly. On any
    // failure (broken XML, missing manifest entry, etc.) we fall
    // through to the generic image scan so slightly malformed EPUBs
    // still produce a thumbnail. Non-EPUB ZIPs cost essentially
    // nothing here — `by_name` returns immediately when missing.
    if let Some(result) = ebook::epub::try_extract_cover(&mut archive) {
        return Ok(result);
    }

    // FB2.zip fast path: many FB2s are distributed wrapped in a ZIP
    // (the `.fb2.zip` convention). If we find an `.fb2` entry inside
    // the ZIP, route through the FB2 cover-extraction logic instead
    // of the generic image scan. Falls through on failure.
    if let Some(result) = try_extract_fb2_from_zip(&mut archive) {
        return Ok(result);
    }

    // Collect image candidates that also fit under the per-entry size
    // cap. Oversized entries are skipped, not an error — maybe a
    // smaller sibling is usable.
    let candidates: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            let f = archive.by_index(i).ok()?;
            if f.is_file() && has_image_ext(f.name()) && f.size() <= limits::MAX_ENTRY_SIZE {
                Some(f.name().to_string())
            } else {
                None
            }
        })
        .collect();

    let name = settings::pick_first_image(candidates)
        .ok_or("archive contains no (small enough) image files")?;

    let mut file = archive.by_name(&name)?;
    let mut buf = Vec::with_capacity(file.size() as usize);
    file.read_to_end(&mut buf)?;

    Ok((name, buf))
}

// =============================================================================
// 7z backend
// =============================================================================

fn sevenz_read_first_image<R: Read + Seek>(
    mut reader: R,
) -> Result<(String, Vec<u8>), Box<dyn Error>> {
    use sevenz_rust::{Password, SevenZReader};

    let size = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;

    let mut sz = SevenZReader::new(reader, size, Password::empty())?;

    // The 7z metadata lives in the footer, which SevenZReader::new has
    // already parsed — so we can list all entry names without reading
    // any compressed data.
    let candidates: Vec<String> = sz
        .archive()
        .files
        .iter()
        .filter(|f| {
            !f.is_directory()
                && has_image_ext(&f.name)
                && f.size <= limits::MAX_ENTRY_SIZE
        })
        .map(|f| f.name.clone())
        .collect();
    let target = settings::pick_first_image(candidates)
        .ok_or("archive contains no (small enough) image files")?;

    // Second phase: stream through entries until we reach the target,
    // buffer it, then stop.
    let target_for_callback = target.clone();
    let mut captured: Option<Vec<u8>> = None;
    sz.for_each_entries(|entry, r| {
        if entry.name == target_for_callback {
            let mut buf = Vec::with_capacity(entry.size as usize);
            r.read_to_end(&mut buf)?;
            captured = Some(buf);
            Ok(false) // stop iteration
        } else {
            Ok(true) // skip (sevenz-rust drains internally)
        }
    })?;

    let data = captured.ok_or("7z entry found in metadata but not in stream")?;
    Ok((target, data))
}

// =============================================================================
// RAR backend — spools the stream to a temp file because the unrar C
// library only accepts file paths.
// =============================================================================

fn rar_read_first_image<R: Read>(
    mut reader: R,
) -> Result<(String, Vec<u8>), Box<dyn Error>> {
    // Opportunistic cleanup of orphaned temp files from previous runs
    // where Explorer may have crashed mid-extraction.
    cleanup_stale_temp_files();

    let temp_path = make_temp_path("rar");
    // RAII: always delete the temp file when this function returns.
    struct TempGuard(PathBuf);
    impl Drop for TempGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _guard = TempGuard(temp_path.clone());

    // Spool the whole archive to disk.
    {
        let mut f = std::fs::File::create(&temp_path)?;
        std::io::copy(&mut reader, &mut f)?;
        f.flush()?;
    }

    use unrar::Archive;

    // Pass 1: list entries, collect image names, apply user sort + cover pick.
    let candidates: Vec<String> = Archive::new(&temp_path)
        .open_for_listing()?
        .filter_map(|entry| {
            let e = entry.ok()?;
            if e.is_directory() {
                return None;
            }
            if e.unpacked_size > limits::MAX_ENTRY_SIZE {
                return None;
            }
            let name = e.filename.to_string_lossy().into_owned();
            if has_image_ext(&name) {
                Some(name)
            } else {
                None
            }
        })
        .collect();
    let target = settings::pick_first_image(candidates)
        .ok_or("archive contains no image files")?;

    // Pass 2: walk the archive again with read access, extract the target.
    let mut archive = Archive::new(&temp_path).open_for_processing()?;
    while let Some(header) = archive.read_header()? {
        let name = header.entry().filename.to_string_lossy().into_owned();
        if name == target {
            let (data, _next) = header.read()?;
            return Ok((target, data));
        }
        archive = header.skip()?;
    }

    Err("RAR target not found on second pass".into())
}

// =============================================================================
// TAR / CBT backend
// =============================================================================

fn tar_read_first_image<R: Read + Seek>(
    mut reader: R,
) -> Result<(String, Vec<u8>), Box<dyn Error>> {
    // Pass 1: walk the archive and collect image entry names.
    // The block scope drops the `tar::Archive` (and its borrow of
    // `reader`) before we seek for pass 2.
    let target: String = {
        reader.seek(SeekFrom::Start(0))?;
        let mut archive = tar::Archive::new(&mut reader);
        let mut candidates: Vec<String> = Vec::new();
        for entry in archive.entries()? {
            let entry = entry?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            if entry.size() > limits::MAX_ENTRY_SIZE {
                continue;
            }
            let path = entry.path()?;
            let name = path.to_string_lossy().into_owned();
            if has_image_ext(&name) {
                candidates.push(name);
            }
        }
        settings::pick_first_image(candidates)
            .ok_or("archive contains no (small enough) image files")?
    };

    // Pass 2: walk again, extract the target.
    reader.seek(SeekFrom::Start(0))?;
    let mut archive = tar::Archive::new(&mut reader);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let name = path.to_string_lossy().into_owned();
        if name == target {
            let mut buf = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut buf)?;
            return Ok((target, buf));
        }
    }

    Err("tar target not found on second pass".into())
}

/// Build a unique-ish temp file path for spooling an archive.
/// We don't need cryptographic uniqueness, just collision avoidance
/// between concurrent Explorer threads.
fn make_temp_path(ext: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = format!("arcthumb_{pid}_{nanos}.{ext}");
    Path::new(&std::env::temp_dir()).join(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ---------------------------------------------------------------
    // detect_format
    // ---------------------------------------------------------------

    #[test]
    fn detect_zip_local_header() {
        assert_eq!(detect_format(b"PK\x03\x04rest"), Format::Zip);
    }

    #[test]
    fn detect_zip_empty_archive() {
        assert_eq!(detect_format(b"PK\x05\x06rest"), Format::Zip);
    }

    #[test]
    fn detect_zip_spanned() {
        assert_eq!(detect_format(b"PK\x07\x08rest"), Format::Zip);
    }

    #[test]
    fn detect_zip_rejects_other_pk_variants() {
        // "PK" but not one of the valid 3rd/4th bytes.
        assert_eq!(detect_format(b"PK\x01\x02xxxx"), Format::Unknown);
    }

    #[test]
    fn detect_sevenz() {
        assert_eq!(
            detect_format(b"7z\xBC\xAF\x27\x1Crest"),
            Format::SevenZ
        );
    }

    #[test]
    fn detect_rar4() {
        assert_eq!(detect_format(b"Rar!\x1A\x07\x00rest"), Format::Rar);
    }

    #[test]
    fn detect_rar5() {
        assert_eq!(detect_format(b"Rar!\x1A\x07\x01\x00rest"), Format::Rar);
    }

    #[test]
    fn detect_tar_ustar_at_257() {
        let mut buf = vec![0u8; 512];
        buf[257..262].copy_from_slice(b"ustar");
        assert_eq!(detect_format(&buf), Format::Tar);
    }

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
        for ext in &["jpg", "jpeg", "png", "gif", "bmp", "tiff", "tif", "webp", "ico"] {
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

    // ---------------------------------------------------------------
    // end-to-end: ZIP backend
    // ---------------------------------------------------------------

    /// Build an in-memory ZIP containing the named entries with the
    /// given (uncompressed) bodies. Returns a Cursor ready for
    /// `read_first_image`.
    fn build_zip(entries: &[(&str, &[u8])]) -> Cursor<Vec<u8>> {
        use zip::write::SimpleFileOptions;
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (name, body) in entries {
                w.start_file(*name, opts).unwrap();
                std::io::Write::write_all(&mut w, body).unwrap();
            }
            w.finish().unwrap();
        }
        Cursor::new(buf)
    }

    #[test]
    fn zip_picks_cover_when_present() {
        let zip = build_zip(&[
            ("page01.jpg", b"AAA"),
            ("page02.jpg", b"BBB"),
            ("cover.jpg", b"COVER"),
            ("readme.txt", b"ignore me"),
        ]);
        let (name, bytes) = read_first_image(zip).expect("read_first_image");
        assert_eq!(name, "cover.jpg");
        assert_eq!(bytes, b"COVER");
    }

    #[test]
    fn zip_natural_sort_picks_page1() {
        // No cover, but natural sort should put page1 ahead of page10.
        let zip = build_zip(&[
            ("page10.jpg", b"TEN"),
            ("page2.jpg", b"TWO"),
            ("page1.jpg", b"ONE"),
        ]);
        let (name, bytes) = read_first_image(zip).expect("read_first_image");
        assert_eq!(name, "page1.jpg");
        assert_eq!(bytes, b"ONE");
    }

    #[test]
    fn zip_skips_non_image_files() {
        let zip = build_zip(&[
            ("notes.txt", b"text"),
            ("only.png", b"PNG_BYTES"),
        ]);
        let (name, _) = read_first_image(zip).expect("read_first_image");
        assert_eq!(name, "only.png");
    }

    #[test]
    fn zip_with_no_images_errors() {
        let zip = build_zip(&[
            ("a.txt", b"text"),
            ("b.md", b"md"),
        ]);
        let result = read_first_image(zip);
        assert!(result.is_err(), "expected error, got {:?}", result.map(|(n, _)| n));
    }

    #[test]
    fn unknown_format_errors_cleanly() {
        let bytes = b"this is plain text, definitely not an archive".to_vec();
        let result = read_first_image(Cursor::new(bytes));
        assert!(result.is_err());
    }

    // ---------------------------------------------------------------
    // end-to-end: TAR backend
    // ---------------------------------------------------------------

    fn build_tar(entries: &[(&str, &[u8])]) -> Cursor<Vec<u8>> {
        let mut buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut buf);
            for (name, body) in entries {
                let mut header = tar::Header::new_ustar();
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append_data(&mut header, name, *body).unwrap();
            }
            builder.finish().unwrap();
        }
        Cursor::new(buf)
    }

    #[test]
    fn tar_picks_first_image_natural_order() {
        let tar = build_tar(&[
            ("page10.png", b"TEN"),
            ("page2.png", b"TWO"),
            ("page1.png", b"ONE"),
            ("notes.txt", b"text"),
        ]);
        let (name, bytes) = read_first_image(tar).expect("read_first_image");
        assert_eq!(name, "page1.png");
        assert_eq!(bytes, b"ONE");
    }

    #[test]
    fn tar_picks_cover_over_sort() {
        let tar = build_tar(&[
            ("aaa.jpg", b"A"),
            ("cover.jpg", b"COVER"),
            ("zzz.jpg", b"Z"),
        ]);
        let (name, _) = read_first_image(tar).expect("read_first_image");
        assert_eq!(name, "cover.jpg");
    }

    // ---------------------------------------------------------------
    // end-to-end: 7z backend
    //
    // We generate the fixture archive in-process using sevenz-rust's
    // SevenZWriter so the test stays hermetic — no committed binary
    // fixtures, no external tools, no network. The same writer that
    // tools like 7-Zip Manager produce on disk.
    // ---------------------------------------------------------------

    /// Build a tiny PNG via the `image` crate so the 7z/RAR fixtures
    /// contain plausible image bytes (the archive backends only care
    /// about the *name* extension, but the integration story is
    /// stronger when the bytes really are decodable).
    fn make_tiny_png() -> Vec<u8> {
        use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba};
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_fn(2, 2, |_, _| Rgba([0, 128, 255, 255]));
        let mut out = Vec::new();
        DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    fn build_7z(entries: &[(&str, &[u8])]) -> Cursor<Vec<u8>> {
        use sevenz_rust::{SevenZArchiveEntry, SevenZWriter};
        let mut buf = Vec::new();
        {
            let mut sz = SevenZWriter::new(Cursor::new(&mut buf)).unwrap();
            for (name, body) in entries {
                let mut entry = SevenZArchiveEntry::new();
                entry.name = (*name).to_string();
                entry.has_stream = true;
                entry.size = body.len() as u64;
                sz.push_archive_entry(entry, Some(&mut Cursor::new(*body)))
                    .unwrap();
            }
            sz.finish().unwrap();
        }
        Cursor::new(buf)
    }

    #[test]
    fn sevenz_picks_first_image_natural_order() {
        let png = make_tiny_png();
        let sz = build_7z(&[
            ("page10.png", &png),
            ("page2.png", &png),
            ("page1.png", &png),
            ("notes.txt", b"text"),
        ]);
        let (name, bytes) = read_first_image(sz).expect("7z read_first_image");
        assert_eq!(name, "page1.png");
        // Round-trip the bytes through the decoder to prove they
        // survived the 7z compression cycle intact.
        let img = crate::decode::decode_with_limits(&name, &bytes)
            .expect("decode 7z entry");
        assert_eq!(img.width(), 2);
        assert_eq!(img.height(), 2);
    }

    #[test]
    fn sevenz_picks_cover_over_sort() {
        let png = make_tiny_png();
        let sz = build_7z(&[
            ("aaa.jpg", &png),
            ("cover.jpg", &png),
            ("zzz.jpg", &png),
        ]);
        let (name, _) = read_first_image(sz).expect("7z read_first_image");
        assert_eq!(name, "cover.jpg");
    }

    #[test]
    fn sevenz_with_no_images_errors() {
        let sz = build_7z(&[("readme.txt", b"hello"), ("notes.md", b"# md")]);
        assert!(read_first_image(sz).is_err());
    }

    // ---------------------------------------------------------------
    // end-to-end: RAR backend
    //
    // RAR is a proprietary format and no Rust crate can write it.
    // Instead of committing a binary fixture (which would be opaque
    // and unreviewable), we hand-craft a minimal RAR4 archive in
    // memory using the documented format. The structure is:
    //
    //   [Marker 7B]
    //   [Main archive header 13B with CRC]
    //   [File header (32 + name_size)B with CRC]
    //   [File data — stored, uncompressed]
    //   [End-of-archive block 7B with CRC]
    //
    // Reference: RarLab technote.txt (rarlab.com/technote.htm).
    // ---------------------------------------------------------------

    /// CRC-32 of `data`, returning only the low 16 bits — RAR4 stores
    /// header CRCs as a u16 truncation of the standard CRC-32.
    fn rar_crc16(data: &[u8]) -> u16 {
        let mut h = crc32fast::Hasher::new();
        h.update(data);
        (h.finalize() & 0xFFFF) as u16
    }

    fn rar_crc32(data: &[u8]) -> u32 {
        let mut h = crc32fast::Hasher::new();
        h.update(data);
        h.finalize()
    }

    /// Build an in-memory RAR4 archive containing a single file
    /// `name` with `data` stored uncompressed (METHOD=0x30).
    fn build_minimal_rar4(name: &str, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();

        // ---- Marker (7 bytes, identifies RAR4) ----
        out.extend_from_slice(b"Rar!\x1A\x07\x00");

        // ---- Main archive header ----
        // HEAD_SIZE includes the 2-byte HEAD_CRC, so the body we
        // CRC over is HEAD_SIZE - 2 = 11 bytes.
        let mut main = Vec::with_capacity(11);
        main.push(0x73); // HEAD_TYPE = main archive header
        main.extend_from_slice(&0u16.to_le_bytes()); // HEAD_FLAGS
        main.extend_from_slice(&13u16.to_le_bytes()); // HEAD_SIZE
        main.extend_from_slice(&0u16.to_le_bytes()); // HIGH_POS_AV
        main.extend_from_slice(&0u32.to_le_bytes()); // POS_AV
        out.extend_from_slice(&rar_crc16(&main).to_le_bytes());
        out.extend_from_slice(&main);

        // ---- File header ----
        let name_bytes = name.as_bytes();
        let header_size = (32 + name_bytes.len()) as u16;
        let pack_size = data.len() as u32;
        let unp_size = pack_size; // stored: packed == unpacked
        let file_crc = rar_crc32(data);

        let mut file_hdr = Vec::with_capacity(30 + name_bytes.len());
        file_hdr.push(0x74); // HEAD_TYPE = file header
        // HEAD_FLAGS = LHD_LONG_BLOCK (0x8000): "additional data
        // (the file body) follows the header". Required for any
        // file block, even with stored compression.
        file_hdr.extend_from_slice(&0x8000u16.to_le_bytes());
        file_hdr.extend_from_slice(&header_size.to_le_bytes());
        file_hdr.extend_from_slice(&pack_size.to_le_bytes());
        file_hdr.extend_from_slice(&unp_size.to_le_bytes());
        file_hdr.push(0x02); // HOST_OS = Win32
        file_hdr.extend_from_slice(&file_crc.to_le_bytes());
        file_hdr.extend_from_slice(&0u32.to_le_bytes()); // FTIME
        file_hdr.push(0x14); // UNP_VER = 20 (RAR 2.0)
        file_hdr.push(0x30); // METHOD = 0x30 (stored)
        file_hdr.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        file_hdr.extend_from_slice(&0x20u32.to_le_bytes()); // ATTR (archive)
        file_hdr.extend_from_slice(name_bytes);
        out.extend_from_slice(&rar_crc16(&file_hdr).to_le_bytes());
        out.extend_from_slice(&file_hdr);

        // ---- File data (stored, no transformation) ----
        out.extend_from_slice(data);

        // ---- End of archive block ----
        let mut end = Vec::with_capacity(5);
        end.push(0x7B); // HEAD_TYPE = end-of-archive
        end.extend_from_slice(&0u16.to_le_bytes()); // HEAD_FLAGS
        end.extend_from_slice(&7u16.to_le_bytes()); // HEAD_SIZE
        out.extend_from_slice(&rar_crc16(&end).to_le_bytes());
        out.extend_from_slice(&end);

        out
    }

    // ---------------------------------------------------------------
    // detect_format: FB2
    // ---------------------------------------------------------------

    #[test]
    fn detect_fb2_with_xml_decl() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0">"#;
        assert_eq!(detect_format(xml), Format::Fb2);
    }

    #[test]
    fn detect_fb2_without_xml_decl() {
        // Some FB2s skip the XML declaration. The token check
        // doesn't care about the prefix.
        assert_eq!(detect_format(b"<FictionBook>"), Format::Fb2);
    }

    #[test]
    fn detect_random_text_xml_is_unknown() {
        let xml = br#"<?xml version="1.0"?><html><body>not fb2</body></html>"#;
        assert_eq!(detect_format(xml), Format::Unknown);
    }

    // ---------------------------------------------------------------
    // end-to-end: FB2 backend (raw .fb2 file)
    // ---------------------------------------------------------------

    /// Build a minimal valid FB2 document containing a single
    /// base64-encoded image binary referenced by the coverpage.
    fn build_fb2(cover_id: &str, png_bytes: &[u8]) -> Vec<u8> {
        use base64::engine::general_purpose::STANDARD as B64;
        use base64::Engine;
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

    #[test]
    fn fb2_raw_extracts_cover() {
        let png = make_tiny_png();
        let fb2 = build_fb2("cover.png", &png);
        let (name, bytes) = read_first_image(Cursor::new(fb2)).expect("FB2 read");
        assert_eq!(name, "cover.png");
        // Round-trip verification: the decoded base64 must still be
        // a valid PNG that the image crate can decode.
        let img = crate::decode::decode_with_limits(&name, &bytes)
            .expect("decode FB2 cover");
        assert_eq!(img.width(), 2);
        assert_eq!(img.height(), 2);
    }

    #[test]
    fn fb2_raw_without_cover_errors() {
        let fb2 = b"<?xml version=\"1.0\"?>\n\
<FictionBook>\n\
  <description><title-info><book-title>X</book-title></title-info></description>\n\
  <body><section><p>text only</p></section></body>\n\
</FictionBook>";
        assert!(read_first_image(Cursor::new(fb2.to_vec())).is_err());
    }

    // ---------------------------------------------------------------
    // end-to-end: FB2 inside ZIP (`.fb2.zip` distribution variant)
    // ---------------------------------------------------------------

    #[test]
    fn fb2_inside_zip_is_extracted() {
        let png = make_tiny_png();
        let fb2 = build_fb2("c.png", &png);
        // Wrap the FB2 in a ZIP. The ZIP backend's FB2 fast path
        // should find the .fb2 entry, decode the cover, and bypass
        // the generic image scan.
        let zip = build_zip(&[("book.fb2", &fb2)]);
        let (name, bytes) = read_first_image(zip).expect("fb2.zip read");
        assert_eq!(name, "c.png");
        let img = crate::decode::decode_with_limits(&name, &bytes)
            .expect("decode fb2.zip cover");
        assert_eq!(img.width(), 2);
    }

    #[test]
    fn fb2_inside_zip_skips_unrelated_zip_images() {
        // The ZIP also contains an image alongside the FB2. The
        // FB2 fast path should win — the FB2 cover is more
        // authoritative than a stray image in the same ZIP.
        let png = make_tiny_png();
        let fb2 = build_fb2("inside.png", &png);
        let zip = build_zip(&[
            ("book.fb2", &fb2),
            ("zzz.png", b"not really a png"),
        ]);
        let (name, _) = read_first_image(zip).expect("fb2.zip read");
        assert_eq!(name, "inside.png");
    }

    #[test]
    fn zip_without_fb2_or_epub_still_uses_generic_scan() {
        // Regression: a plain ZIP must still flow through the
        // existing image scan after the EPUB and FB2 fast paths
        // both decline.
        let zip = build_zip(&[("page1.jpg", b"data")]);
        let (name, _) = read_first_image(zip).expect("plain ZIP read");
        assert_eq!(name, "page1.jpg");
    }

    #[test]
    fn rar_fixture_starts_with_rar4_marker() {
        // Sanity-check the hand-crafted bytes before we even hand
        // them to unrar. If this drifts the higher-level tests will
        // be very confusing to debug.
        let bytes = build_minimal_rar4("01.png", b"hello");
        assert_eq!(&bytes[..7], b"Rar!\x1A\x07\x00");
        assert_eq!(detect_format(&bytes), Format::Rar);
    }

    // ---------------------------------------------------------------
    // end-to-end: EPUB fast path through the ZIP backend
    //
    // EPUB is a ZIP, so it goes through `zip_read_first_image`. The
    // EPUB-aware fast path consults `META-INF/container.xml` and the
    // OPF manifest before falling back to the generic image scan.
    // These tests build EPUBs in-memory using the same `zip` writer
    // the ZIP fixtures use.
    // ---------------------------------------------------------------

    /// Build an in-memory EPUB ZIP. `extras` lets each test inject
    /// arbitrary additional files (e.g. a `.png` cover beside the
    /// OPF, or a stray image for fallback testing).
    fn build_epub(
        container_xml: &str,
        opf_path: &str,
        opf_xml: &str,
        extras: &[(&str, &[u8])],
    ) -> Cursor<Vec<u8>> {
        use zip::write::SimpleFileOptions;
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);

            // EPUB spec: `mimetype` MUST be the first entry, stored
            // (not compressed), with the literal content below.
            w.start_file("mimetype", opts).unwrap();
            std::io::Write::write_all(&mut w, b"application/epub+zip").unwrap();

            w.start_file("META-INF/container.xml", opts).unwrap();
            std::io::Write::write_all(&mut w, container_xml.as_bytes()).unwrap();

            w.start_file(opf_path, opts).unwrap();
            std::io::Write::write_all(&mut w, opf_xml.as_bytes()).unwrap();

            for (name, body) in extras {
                w.start_file(*name, opts).unwrap();
                std::io::Write::write_all(&mut w, body).unwrap();
            }
            w.finish().unwrap();
        }
        Cursor::new(buf)
    }

    /// Tiny container.xml pointing at `OEBPS/content.opf`.
    fn standard_container_xml() -> &'static str {
        r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#
    }

    #[test]
    fn epub2_meta_cover_extracted() {
        let png = make_tiny_png();
        let opf = r#"<?xml version="1.0"?>
<package version="2.0" xmlns="http://www.idpf.org/2007/opf">
  <metadata>
    <meta name="cover" content="cover-image"/>
  </metadata>
  <manifest>
    <item id="cover-image" href="images/front.png" media-type="image/png"/>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
</package>"#;
        let epub = build_epub(
            standard_container_xml(),
            "OEBPS/content.opf",
            opf,
            &[
                // The image lives at OEBPS/images/front.png in the ZIP
                // because hrefs are relative to the OPF directory.
                ("OEBPS/images/front.png", &png),
                // Decoy: a different image that should NOT be picked.
                ("OEBPS/images/zzz.png", b"NOT THIS ONE"),
            ],
        );
        let (name, bytes) = read_first_image(epub).expect("EPUB read");
        assert_eq!(name, "OEBPS/images/front.png");
        let img = crate::decode::decode_with_limits(&name, &bytes)
            .expect("decode EPUB cover");
        assert_eq!(img.width(), 2);
        assert_eq!(img.height(), 2);
    }

    #[test]
    fn epub3_properties_cover_image_extracted() {
        let png = make_tiny_png();
        let opf = r#"<?xml version="1.0"?>
<package version="3.0" xmlns="http://www.idpf.org/2007/opf">
  <metadata/>
  <manifest>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="cov" href="img/cover.png" media-type="image/png" properties="cover-image"/>
  </manifest>
</package>"#;
        let epub = build_epub(
            standard_container_xml(),
            "OEBPS/content.opf",
            opf,
            &[("OEBPS/img/cover.png", &png)],
        );
        let (name, _) = read_first_image(epub).expect("EPUB read");
        assert_eq!(name, "OEBPS/img/cover.png");
    }

    #[test]
    fn epub_fallback_when_no_metadata() {
        // Container says there's an OPF, but the OPF declares no
        // cover at all. We should fall through to the generic image
        // scan and pick the natural-sort first image (or `cover.*`
        // if `prefer_cover_names` is on).
        let png = make_tiny_png();
        let opf = r#"<package>
  <metadata/>
  <manifest>
    <item id="ch1" href="ch1.xhtml"/>
  </manifest>
</package>"#;
        let epub = build_epub(
            standard_container_xml(),
            "OEBPS/content.opf",
            opf,
            &[
                ("OEBPS/page1.png", &png),
                ("OEBPS/page2.png", &png),
            ],
        );
        // The generic scan should pick page1.png via natural sort.
        let (name, _) = read_first_image(epub).expect("EPUB read");
        assert!(
            name.ends_with("page1.png"),
            "expected page1.png fallback, got {name}"
        );
    }

    #[test]
    fn epub_fallback_when_opf_points_to_missing_image() {
        // OPF declares a cover, but the manifest href doesn't exist
        // in the ZIP. The fast path returns None and we fall back.
        let png = make_tiny_png();
        let opf = r#"<package version="2.0">
  <metadata>
    <meta name="cover" content="cover-image"/>
  </metadata>
  <manifest>
    <item id="cover-image" href="images/MISSING.jpg" media-type="image/jpeg"/>
  </manifest>
</package>"#;
        let epub = build_epub(
            standard_container_xml(),
            "OEBPS/content.opf",
            opf,
            &[("OEBPS/cover.png", &png)],
        );
        // Fallback finds OEBPS/cover.png via the cover-name heuristic.
        let (name, _) = read_first_image(epub).expect("EPUB read");
        assert!(name.ends_with("cover.png"));
    }

    #[test]
    fn epub_fallback_when_container_xml_is_garbage() {
        // Container.xml exists but is malformed. Don't crash; fall
        // back to the generic scan.
        let png = make_tiny_png();
        let epub = build_epub(
            "this is not xml",
            "OEBPS/content.opf",
            "<package/>",
            &[("OEBPS/cover.png", &png)],
        );
        let (name, _) = read_first_image(epub).expect("EPUB read");
        assert!(name.ends_with("cover.png"));
    }

    #[test]
    fn epub_with_root_level_opf() {
        // Some EPUBs put the OPF at the ZIP root. Make sure the
        // empty parent_dir case still resolves hrefs correctly.
        let png = make_tiny_png();
        let container = r#"<container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>"#;
        let opf = r#"<package version="3.0">
  <manifest>
    <item id="c" href="cover.png" properties="cover-image"/>
  </manifest>
</package>"#;
        let epub = build_epub(
            container,
            "content.opf",
            opf,
            &[("cover.png", &png)],
        );
        let (name, _) = read_first_image(epub).expect("EPUB read");
        assert_eq!(name, "cover.png");
    }

    #[test]
    fn plain_zip_still_works_after_epub_fast_path() {
        // Regression: a plain ZIP without container.xml must not be
        // affected by the EPUB code path.
        let zip = build_zip(&[
            ("page01.jpg", b"AAA"),
            ("page02.jpg", b"BBB"),
        ]);
        let (name, _) = read_first_image(zip).expect("plain ZIP read");
        assert_eq!(name, "page01.jpg");
    }

    #[test]
    fn rar_reads_single_image_entry() {
        let png = make_tiny_png();
        let rar = build_minimal_rar4("01.png", &png);
        let (name, bytes) = read_first_image(Cursor::new(rar))
            .expect("RAR read_first_image");
        assert_eq!(name, "01.png");
        // The bytes round-tripped through unrar should still decode
        // as a valid PNG with the dimensions we put in.
        let img = crate::decode::decode_with_limits(&name, &bytes)
            .expect("decode RAR entry");
        assert_eq!(img.width(), 2);
        assert_eq!(img.height(), 2);
    }
}

/// Delete `arcthumb_*` temp files older than `TEMP_FILE_MAX_AGE_SECS`.
///
/// Normally the `TempGuard` RAII in `rar_read_first_image` cleans up
/// after itself. But if Explorer is killed (task manager, crash)
/// mid-extraction, the temp file leaks. This function sweeps those
/// up on the next RAR thumbnail request — best-effort, all errors
/// are silently ignored.
fn cleanup_stale_temp_files() {
    let temp_dir = std::env::temp_dir();
    let now = SystemTime::now();
    let max_age = Duration::from_secs(limits::TEMP_FILE_MAX_AGE_SECS);

    let Ok(entries) = std::fs::read_dir(&temp_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name_str) = file_name.to_str() else {
            continue;
        };
        if !name_str.starts_with("arcthumb_") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if let Ok(age) = now.duration_since(modified) {
            if age > max_age {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}
