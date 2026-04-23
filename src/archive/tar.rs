//! TAR / CBT backend — via `tar` crate, Read only (we use Seek to
//! rewind between listing and extraction passes).

use std::error::Error;
use std::io::{Read, Seek, SeekFrom};

use crate::limits;
use crate::settings::Settings;

pub(super) fn tar_read_first_image<R: Read + Seek>(
    mut reader: R,
    settings: &Settings,
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
            if settings.accepts_image_ext(&name) {
                candidates.push(name);
            }
        }
        settings
            .pick_first_image(candidates)
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

#[cfg(test)]
mod tests {
    use super::super::read_first_image;
    use crate::settings::Settings;
    use std::io::Cursor;

    #[test]
    fn detect_tar_ustar_at_257() {
        let mut buf = vec![0u8; 512];
        buf[257..262].copy_from_slice(b"ustar");
        assert_eq!(super::super::detect_format(&buf), super::super::Format::Tar);
    }

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
        let (name, bytes) = read_first_image(tar, &Settings::default()).expect("read_first_image");
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
        let (name, _) = read_first_image(tar, &Settings::default()).expect("read_first_image");
        assert_eq!(name, "cover.jpg");
    }

    // ---------------------------------------------------------------
    // end-to-end: image-extension mask gating
    // ---------------------------------------------------------------

    #[test]
    fn tar_mask_excludes_disabled_image_extension() {
        use crate::settings::{SUPPORTED_IMAGE_EXTS, Settings, default_enabled_image_exts_mask};

        let jpg_idx = SUPPORTED_IMAGE_EXTS
            .iter()
            .position(|&e| e == ".jpg")
            .unwrap();
        let tar = build_tar(&[("a.jpg", b"JPG"), ("b.png", b"PNG")]);
        let settings = Settings {
            enabled_image_exts_mask: !(1u32 << jpg_idx) & default_enabled_image_exts_mask(),
            prefer_cover_names: false,
            ..Settings::default()
        };
        let (name, _) = read_first_image(tar, &settings).expect("mask excludes jpg");
        assert_eq!(name, "b.png");
    }

    #[test]
    fn tar_mask_of_zero_rejects_all_images() {
        use crate::settings::Settings;

        let tar = build_tar(&[("only.png", b"PNG")]);
        let settings = Settings {
            enabled_image_exts_mask: 0,
            ..Settings::default()
        };
        assert!(read_first_image(tar, &settings).is_err());
    }

    #[test]
    fn tar_every_supported_extension_round_trips_when_enabled_alone() {
        use crate::settings::{SUPPORTED_IMAGE_EXTS, Settings};

        for (i, ext) in SUPPORTED_IMAGE_EXTS.iter().enumerate() {
            let entry = format!("file{ext}");
            let tar = build_tar(&[(&entry, b"BODY")]);
            let settings = Settings {
                enabled_image_exts_mask: 1u32 << i,
                prefer_cover_names: false,
                ..Settings::default()
            };
            let (name, _) = read_first_image(tar, &settings)
                .unwrap_or_else(|e| panic!("tar ext {ext} solo-enabled failed: {e}"));
            assert_eq!(name, entry);
        }
    }
}
