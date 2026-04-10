//! 7z backend — via `sevenz-rust`, direct Read+Seek.

use std::error::Error;
use std::io::{Read, Seek, SeekFrom};

use crate::{limits, settings};

use super::has_image_ext;

pub(super) fn sevenz_read_first_image<R: Read + Seek>(
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
        .filter(|f| !f.is_directory() && has_image_ext(&f.name) && f.size <= limits::MAX_ENTRY_SIZE)
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

#[cfg(test)]
mod tests {
    use super::super::read_first_image;
    use super::super::tests::make_tiny_png;
    use std::io::Cursor;

    #[test]
    fn detect_sevenz() {
        assert_eq!(
            super::super::detect_format(b"7z\xBC\xAF\x27\x1Crest"),
            super::super::Format::SevenZ
        );
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
        let img = crate::decode::decode_with_limits(&name, &bytes).expect("decode 7z entry");
        assert_eq!(img.width(), 2);
        assert_eq!(img.height(), 2);
    }

    #[test]
    fn sevenz_picks_cover_over_sort() {
        let png = make_tiny_png();
        let sz = build_7z(&[("aaa.jpg", &png), ("cover.jpg", &png), ("zzz.jpg", &png)]);
        let (name, _) = read_first_image(sz).expect("7z read_first_image");
        assert_eq!(name, "cover.jpg");
    }

    #[test]
    fn sevenz_with_no_images_errors() {
        let sz = build_7z(&[("readme.txt", b"hello"), ("notes.md", b"# md")]);
        assert!(read_first_image(sz).is_err());
    }
}
