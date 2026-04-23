//! FB2 backend (raw XML, not an archive).
//!
//! FB2 is a single XML document where images live as base64 inside
//! `<binary>` elements. The actual decoding lives in `ebook::fb2`;
//! this function just slurps the file (already size-checked by the
//! caller) and dispatches.

use std::error::Error;
use std::io::{Read, Seek, SeekFrom};

use crate::ebook;

pub(super) fn fb2_read_first_image<R: Read + Seek>(
    mut reader: R,
) -> Result<(String, Vec<u8>), Box<dyn Error>> {
    reader.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    ebook::fb2::try_extract_cover(&bytes).ok_or_else(|| "FB2 has no embedded cover image".into())
}

#[cfg(test)]
mod tests {
    use super::super::{
        read_first_image,
        tests::{build_fb2, make_tiny_png},
    };
    use crate::settings::Settings;
    use std::io::Cursor;

    #[test]
    fn detect_fb2_with_xml_decl() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0">"#;
        assert_eq!(super::super::detect_format(xml), super::super::Format::Fb2);
    }

    #[test]
    fn detect_fb2_without_xml_decl() {
        assert_eq!(
            super::super::detect_format(b"<FictionBook>"),
            super::super::Format::Fb2
        );
    }

    #[test]
    fn detect_random_text_xml_is_unknown() {
        let xml = br#"<?xml version="1.0"?><html><body>not fb2</body></html>"#;
        assert_eq!(
            super::super::detect_format(xml),
            super::super::Format::Unknown
        );
    }

    #[test]
    fn fb2_raw_extracts_cover() {
        let png = make_tiny_png();
        let fb2 = build_fb2("cover.png", &png);
        let (name, bytes) =
            read_first_image(Cursor::new(fb2), &Settings::default()).expect("FB2 read");
        assert_eq!(name, "cover.png");
        let img = crate::decode::decode_with_limits(&name, &bytes).expect("decode FB2 cover");
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
        assert!(read_first_image(Cursor::new(fb2.to_vec()), &Settings::default()).is_err());
    }
}
