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

use crate::limits;

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
        Format::Unknown => Err("unrecognised archive format".into()),
    }
}

// =============================================================================
// ZIP backend
// =============================================================================

fn zip_read_first_image<R: Read + Seek>(
    mut reader: R,
) -> Result<(String, Vec<u8>), Box<dyn Error>> {
    reader.seek(SeekFrom::Start(0))?;
    let mut archive = zip::ZipArchive::new(reader)?;

    // Collect image candidates that also fit under the per-entry size
    // cap. Oversized entries are skipped, not an error — maybe a
    // smaller sibling is usable.
    let mut names: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            let f = archive.by_index(i).ok()?;
            if f.is_file() && has_image_ext(f.name()) && f.size() <= limits::MAX_ENTRY_SIZE {
                Some(f.name().to_string())
            } else {
                None
            }
        })
        .collect();
    names.sort();

    let name = names
        .into_iter()
        .next()
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
    let target: String = sz
        .archive()
        .files
        .iter()
        .filter(|f| {
            !f.is_directory()
                && has_image_ext(&f.name)
                && f.size <= limits::MAX_ENTRY_SIZE
        })
        .map(|f| f.name.clone())
        .min()
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

    // Pass 1: list entries, collect image names, sort, take first.
    let mut names: Vec<String> = Archive::new(&temp_path)
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
    names.sort();
    let target = names
        .into_iter()
        .next()
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
        let mut names: Vec<String> = Vec::new();
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
                names.push(name);
            }
        }
        names
            .into_iter()
            .min()
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
