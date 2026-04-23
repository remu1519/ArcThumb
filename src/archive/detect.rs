//! Archive format detection from magic bytes.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
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

pub(super) fn detect_format(magic: &[u8]) -> Format {
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
