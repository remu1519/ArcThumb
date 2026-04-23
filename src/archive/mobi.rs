//! MOBI / AZW / AZW3 backend (PalmDB container with embedded images).

use std::error::Error;
use std::io::{Read, Seek, SeekFrom};

use crate::ebook;

pub(super) fn mobi_read_first_image<R: Read + Seek>(
    mut reader: R,
) -> Result<(String, Vec<u8>), Box<dyn Error>> {
    reader.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    ebook::mobi::try_extract_cover(&bytes)
        .ok_or_else(|| "MOBI has no extractable cover image".into())
}

#[cfg(test)]
mod tests {
    use super::super::{read_first_image, tests::make_tiny_png};
    use crate::settings::Settings;
    use std::io::Cursor;

    /// Build a 68-byte stub that has the BOOKMOBI signature at the
    /// expected PalmDB offset (60..68). Bytes before are zero.
    fn mobi_stub_bytes() -> Vec<u8> {
        let mut v = vec![0u8; 68];
        v[60..68].copy_from_slice(b"BOOKMOBI");
        v
    }

    /// Build a minimal MOBI file with a single image record.
    fn build_minimal_mobi(image: &[u8]) -> Vec<u8> {
        let num_records: u16 = 3;
        let record_0_offset: u32 = 78 + 3 * 8 + 2; // 104
        let palmdoc_len: u32 = 16;
        let mobi_header_len: u32 = 232;
        let exth_len: u32 = 24;
        let record_1_offset: u32 = record_0_offset + palmdoc_len + mobi_header_len + exth_len;
        let record_2_offset: u32 = record_1_offset + 1;

        let mut out = Vec::with_capacity(record_2_offset as usize + image.len());

        // ---- PalmDB header (78 bytes) ----
        let mut name = [0u8; 32];
        name[..4].copy_from_slice(b"test");
        out.extend_from_slice(&name);
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(b"BOOK");
        out.extend_from_slice(b"MOBI");
        out.extend_from_slice(&3u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&num_records.to_be_bytes());
        debug_assert_eq!(out.len(), 78);

        // ---- PdbRecords list (3 × 8 + 2 = 26 bytes) ----
        out.extend_from_slice(&record_0_offset.to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&record_1_offset.to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&record_2_offset.to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 2]);
        out.extend_from_slice(&[0, 0]);
        debug_assert_eq!(out.len() as u32, record_0_offset);

        // ---- PalmDoc header (16 bytes) ----
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&4096u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());

        // ---- MOBI header (232 bytes total) ----
        out.extend_from_slice(b"MOBI");
        out.extend_from_slice(&232u32.to_be_bytes());
        out.extend_from_slice(&2u32.to_be_bytes());
        out.extend_from_slice(&65001u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&6u32.to_be_bytes());
        out.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        out.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        out.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        out.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        for _ in 0..6 {
            out.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        }
        out.extend_from_slice(&2u32.to_be_bytes()); // first_non_book_index
        out.extend_from_slice(&272u32.to_be_bytes()); // name_offset
        out.extend_from_slice(&0u32.to_be_bytes()); // name_length
        out.extend_from_slice(&0u16.to_be_bytes()); // unused
        out.push(0); // locale
        out.push(9); // language_code = English
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&6u32.to_be_bytes());
        out.extend_from_slice(&2u32.to_be_bytes()); // first_image_index
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0x40u32.to_be_bytes()); // exth_flags = HAS_EXTH
        out.extend_from_slice(&[0u8; 32]);
        out.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&[0u8; 8]);
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(&0u64.to_be_bytes());
        out.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        out.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());

        // ---- EXTH header (24 bytes) ----
        out.extend_from_slice(b"EXTH");
        out.extend_from_slice(&24u32.to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(&201u32.to_be_bytes()); // CoverOffset
        out.extend_from_slice(&12u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // offset 0 → first_image_index
        debug_assert_eq!(out.len() as u32, record_1_offset);

        // ---- Record 1: 1-byte text placeholder ----
        out.push(0u8);
        debug_assert_eq!(out.len() as u32, record_2_offset);

        // ---- Record 2: image bytes ----
        out.extend_from_slice(image);

        out
    }

    #[test]
    fn detect_mobi_via_bookmobi() {
        assert_eq!(
            super::super::detect_format(&mobi_stub_bytes()),
            super::super::Format::Mobi
        );
    }

    #[test]
    fn detect_mobi_requires_offset_60() {
        // BOOKMOBI elsewhere in the buffer must NOT trigger MOBI
        // detection — only the canonical PalmDB offset counts.
        let mut v = vec![0u8; 68];
        v[0..8].copy_from_slice(b"BOOKMOBI");
        assert_eq!(
            super::super::detect_format(&v),
            super::super::Format::Unknown
        );
    }

    #[test]
    fn detect_short_input_not_mobi() {
        // Less than 68 bytes can't possibly carry a PalmDB header.
        let v = vec![0u8; 60];
        assert_eq!(
            super::super::detect_format(&v),
            super::super::Format::Unknown
        );
    }

    #[test]
    fn mobi_fixture_passes_detect_format() {
        let png = make_tiny_png();
        let mobi = build_minimal_mobi(&png);
        assert_eq!(
            super::super::detect_format(&mobi),
            super::super::Format::Mobi
        );
    }

    #[test]
    fn mobi_extracts_cover_via_exth_201() {
        let png = make_tiny_png();
        let mobi = build_minimal_mobi(&png);
        let (name, bytes) =
            read_first_image(Cursor::new(mobi), &Settings::default()).expect("MOBI read");
        assert_eq!(name, "cover.png");
        let img = crate::decode::decode_with_limits(&name, &bytes).expect("decode MOBI cover");
        assert_eq!(img.width(), 2);
        assert_eq!(img.height(), 2);
    }

    #[test]
    fn mobi_garbage_returns_error() {
        let mut bytes = mobi_stub_bytes();
        bytes.extend_from_slice(&[0u8; 256]);
        assert!(read_first_image(Cursor::new(bytes), &Settings::default()).is_err());
    }
}
