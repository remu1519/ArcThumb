//! Archive reading: dispatch by detected magic bytes to a format-specific
//! backend, return the first image file as `(name, bytes)`.
//!
//! Supported formats:
//! - **ZIP** (`PK\x03\x04`) — via `zip` crate, direct Read+Seek
//! - **7z**  (`7z\xBC\xAF\x27\x1C`) — via `sevenz-rust`, direct Read+Seek
//! - **RAR** (`Rar!\x1A\x07\x00` / `Rar!\x1A\x07\x01\x00`) — via `unrar`,
//!   which insists on a file path, so we spool the stream to `%TEMP%`.
//!
//! "First image" is defined as the alphabetically smallest file whose
//! extension is in `IMAGE_EXTS`.

use std::error::Error;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Image extensions we recognise inside archives. Case-insensitive.
const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "bmp", "tiff", "tif", "webp", "avif", "ico",
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
    Format::Unknown
}

/// Open an archive stream, pick the first image, return `(name, bytes)`.
pub fn read_first_image<R: Read + Seek>(
    mut reader: R,
) -> Result<(String, Vec<u8>), Box<dyn Error>> {
    reader.seek(SeekFrom::Start(0))?;
    let mut magic = [0u8; 16];
    let n = reader.read(&mut magic)?;
    reader.seek(SeekFrom::Start(0))?;

    match detect_format(&magic[..n]) {
        Format::Zip => zip_read_first_image(reader),
        Format::SevenZ => sevenz_read_first_image(reader),
        Format::Rar => rar_read_first_image(reader),
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

    let mut names: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            let f = archive.by_index(i).ok()?;
            if f.is_file() && has_image_ext(f.name()) {
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
        .ok_or("archive contains no image files")?;

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
        .filter(|f| !f.is_directory() && has_image_ext(&f.name))
        .map(|f| f.name.clone())
        .min()
        .ok_or("archive contains no image files")?;

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

/// Build a unique-ish temp file path for spooling an archive.
/// We don't need cryptographic uniqueness, just collision avoidance
/// between concurrent Explorer threads.
fn make_temp_path(ext: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = format!("arcthumb_{pid}_{nanos}.{ext}");
    Path::new(&std::env::temp_dir()).join(name)
}
