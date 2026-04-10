//! RAR backend — spools the stream to a temp file because the unrar C
//! library only accepts file paths.

use std::error::Error;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::{limits, settings};

use super::has_image_ext;

pub(super) fn rar_read_first_image<R: Read>(
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
    let target = settings::pick_first_image(candidates).ok_or("archive contains no image files")?;

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

#[cfg(test)]
mod tests {
    use super::super::{read_first_image, tests::make_tiny_png};
    use std::io::Cursor;

    #[test]
    fn detect_rar4() {
        assert_eq!(
            super::super::detect_format(b"Rar!\x1A\x07\x00rest"),
            super::super::Format::Rar
        );
    }

    #[test]
    fn detect_rar5() {
        assert_eq!(
            super::super::detect_format(b"Rar!\x1A\x07\x01\x00rest"),
            super::super::Format::Rar
        );
    }

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
        // HEAD_FLAGS = LHD_LONG_BLOCK (0x8000)
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

    #[test]
    fn rar_fixture_starts_with_rar4_marker() {
        let bytes = build_minimal_rar4("01.png", b"hello");
        assert_eq!(&bytes[..7], b"Rar!\x1A\x07\x00");
        assert_eq!(
            super::super::detect_format(&bytes),
            super::super::Format::Rar
        );
    }

    #[test]
    fn rar_reads_single_image_entry() {
        let png = make_tiny_png();
        let rar = build_minimal_rar4("01.png", &png);
        let (name, bytes) = read_first_image(Cursor::new(rar)).expect("RAR read_first_image");
        assert_eq!(name, "01.png");
        let img = crate::decode::decode_with_limits(&name, &bytes).expect("decode RAR entry");
        assert_eq!(img.width(), 2);
        assert_eq!(img.height(), 2);
    }
}
