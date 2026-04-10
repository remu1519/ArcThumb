//! TAR / CBT backend — via `tar` crate, Read only (we use Seek to
//! rewind between listing and extraction passes).

use std::error::Error;
use std::io::{Read, Seek, SeekFrom};

use crate::{limits, settings};

use super::has_image_ext;

pub(super) fn tar_read_first_image<R: Read + Seek>(
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

#[cfg(test)]
mod tests {
    use super::super::read_first_image;
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
}
