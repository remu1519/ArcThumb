//! FictionBook (FB2) cover-image extraction.
//!
//! FB2 is a single XML document. Images are embedded as base64 inside
//! `<binary>` elements at the document root, and the cover is named
//! by an `xlink:href="#binary-id"` reference inside
//! `<description><title-info><coverpage>`.
//!
//! Document layout:
//!
//! ```xml
//! <FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"
//!              xmlns:l="http://www.w3.org/1999/xlink">
//!   <description>
//!     <title-info>
//!       <coverpage>
//!         <image l:href="#cover.jpg"/>
//!       </coverpage>
//!     </title-info>
//!   </description>
//!   <body>...</body>
//!   <binary id="cover.jpg" content-type="image/jpeg">/9j/4AAQ...</binary>
//! </FictionBook>
//! ```
//!
//! Strategy:
//! 1. **Pass 1** scans for `<image l:href="#id"/>` inside coverpage.
//! 2. **Pass 2** locates the `<binary id="...">` whose id matches and
//!    captures its base64 text content.
//! 3. **Fallback**: if no coverpage, no matching id, or the binary
//!    can't be decoded, return the first `<binary>` whose content-type
//!    starts with `image/`.
//!
//! Two passes are clearer than one stateful pass and the cost is
//! negligible — quick-xml runs at hundreds of MB/s on plain text.
//!
//! `.fb2` files are typically UTF-8 today but the spec also allows
//! `windows-1251` for Russian text. We side-step this by using
//! `String::from_utf8_lossy` — the XML structure characters are
//! ASCII, our attribute values (`id`, `content-type`, `href`) are
//! ASCII in practice, and the base64 binary text is ASCII by
//! definition. Only Russian narrative text in `<body>` would be
//! garbled, and we never read it.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::QName;
use quick_xml::reader::Reader;

/// Try to extract the cover image from raw FB2 document bytes.
///
/// Returns `Some((synthetic_name, image_bytes))` if we found a usable
/// cover, or `None` if the document has no embedded image at all
/// (in which case the caller should treat the FB2 as having no
/// thumbnail and let Explorer fall back to the default icon).
pub fn try_extract_cover(xml_bytes: &[u8]) -> Option<(String, Vec<u8>)> {
    let xml = String::from_utf8_lossy(xml_bytes);

    // Strategy 1: explicit cover via coverpage → matching binary id.
    if let Some(cover_id) = find_cover_id(&xml) {
        if let Some(result) = extract_binary_by_id(&xml, &cover_id) {
            return Some(result);
        }
    }

    // Strategy 2: first binary that *looks* like an image. Catches
    // FB2s without a coverpage and FB2s with a dangling cover id.
    extract_first_image_binary(&xml)
}

// =============================================================================
// XML parsing helpers
// =============================================================================

/// Strip an XML namespace prefix (`l:href` → `href`).
fn strip_namespace(name: &[u8]) -> &[u8] {
    match name.iter().position(|&b| b == b':') {
        Some(idx) => &name[idx + 1..],
        None => name,
    }
}

/// True if the local name (with any namespace prefix removed) of
/// `e`'s tag matches `expected`. Takes a `QName` so it works for
/// both `BytesStart` (open tag) and `BytesEnd` (close tag).
fn qname_local_eq(qname: QName, expected: &[u8]) -> bool {
    strip_namespace(qname.as_ref()) == expected
}

/// Look up an attribute value by local name. Decodes XML character
/// entities so e.g. `&amp;` survives the round-trip.
fn attr_value(e: &BytesStart, reader: &Reader<&[u8]>, key: &[u8]) -> Option<String> {
    for attr in e.attributes().flatten() {
        let qname = attr.key;
        let local = strip_namespace(qname.as_ref());
        if local == key {
            return attr
                .decode_and_unescape_value(reader.decoder())
                .ok()
                .map(|cow| cow.into_owned());
        }
    }
    None
}

// =============================================================================
// Pass 1: find the cover image's binary id from <coverpage>
// =============================================================================

fn find_cover_id(xml: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    // Use simple depth counters rather than a path stack — FB2 only
    // ever has one title-info / coverpage active at a time.
    let mut in_title_info: i32 = 0;
    let mut in_coverpage: i32 = 0;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if qname_local_eq(e.name(), b"title-info") {
                    in_title_info += 1;
                } else if qname_local_eq(e.name(), b"coverpage") {
                    in_coverpage += 1;
                }
            }
            Ok(Event::End(e)) => {
                if qname_local_eq(e.name(), b"title-info") {
                    in_title_info -= 1;
                } else if qname_local_eq(e.name(), b"coverpage") {
                    in_coverpage -= 1;
                }
            }
            Ok(Event::Empty(e)) => {
                if qname_local_eq(e.name(), b"image") && in_title_info > 0 && in_coverpage > 0 {
                    if let Some(href) = attr_value(&e, &reader, b"href") {
                        // The href is "#binary_id"; strip the leading
                        // `#`. Some malformed FB2s omit it.
                        let id = href.strip_prefix('#').unwrap_or(&href).to_string();
                        if !id.is_empty() {
                            return Some(id);
                        }
                    }
                }
            }
            Ok(Event::Eof) => return None,
            Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

// =============================================================================
// Pass 2: locate a <binary> element and decode its base64 payload
// =============================================================================

/// Find `<binary id="target_id" content-type="...">` and decode its
/// text. Returns `(synthetic_name, decoded_bytes)`.
fn extract_binary_by_id(xml: &str, target_id: &str) -> Option<(String, Vec<u8>)> {
    extract_binary_matching(xml, |id, _ct| id == target_id)
}

/// Find the first `<binary>` whose `content-type` starts with `image/`,
/// regardless of any cover declaration.
fn extract_first_image_binary(xml: &str) -> Option<(String, Vec<u8>)> {
    extract_binary_matching(xml, |_id, ct| {
        ct.map(|c| c.starts_with("image/")).unwrap_or(false)
    })
}

/// Common machinery for both binary lookups. The closure decides
/// whether a given `<binary>` element is the one we want.
fn extract_binary_matching<F>(xml: &str, matcher: F) -> Option<(String, Vec<u8>)>
where
    F: Fn(&str, Option<&str>) -> bool,
{
    let mut reader = Reader::from_str(xml);
    // We must NOT trim text inside <binary> or quick-xml will
    // collapse whitespace runs in ways that change base64 padding.
    // We strip whitespace ourselves below.
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();

    let mut in_target = false;
    let mut found_target = false;
    let mut target_id: Option<String> = None;
    let mut target_ct: Option<String> = None;
    let mut target_text = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if !in_target && qname_local_eq(e.name(), b"binary") {
                    let id = attr_value(&e, &reader, b"id");
                    let ct = attr_value(&e, &reader, b"content-type");
                    if let Some(id_val) = id.as_ref() {
                        if matcher(id_val, ct.as_deref()) {
                            in_target = true;
                            found_target = true;
                            target_id = Some(id_val.clone());
                            target_ct = ct;
                            target_text.clear();
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                if in_target && qname_local_eq(e.name(), b"binary") {
                    break;
                }
            }
            Ok(Event::Text(t)) if in_target => {
                let bytes: &[u8] = &t;
                if let Ok(s) = std::str::from_utf8(bytes) {
                    target_text.push_str(s);
                }
            }
            Ok(Event::CData(t)) if in_target => {
                if let Ok(s) = std::str::from_utf8(&t) {
                    target_text.push_str(s);
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    if !found_target {
        return None;
    }

    // Strip whitespace before decoding — XML pretty-printers love
    // wrapping base64 across multiple lines, and the standard base64
    // alphabet rejects whitespace.
    let cleaned: String = target_text
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let decoded = BASE64.decode(cleaned.as_bytes()).ok()?;

    // Synthesize a filename. If the id already looks like one (has
    // a dot), use it as-is. Otherwise tack on an extension derived
    // from the content-type.
    let id = target_id.unwrap_or_else(|| "cover".to_string());
    let name = if id.contains('.') {
        id
    } else {
        let ext = target_ct
            .as_deref()
            .and_then(|ct| ct.strip_prefix("image/"))
            .map(|sub| {
                // Trim any "+xml" suffix etc. and pick a filename-safe
                // extension. Common cases: jpeg, jpg, png, gif, webp.
                let cut = sub.find(['+', ';']).unwrap_or(sub.len());
                sub[..cut].to_string()
            })
            .unwrap_or_else(|| "img".to_string());
        format!("{id}.{ext}")
    };

    Some((name, decoded))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn b64_encode(bytes: &[u8]) -> String {
        BASE64.encode(bytes)
    }

    /// Build a minimal valid FB2 document with one binary.
    fn fb2_with_cover(cover_id: &str, content_type: &str, payload: &[u8]) -> String {
        let b64 = b64_encode(payload);
        format!(
            r##"<?xml version="1.0" encoding="UTF-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink">
  <description>
    <title-info>
      <coverpage>
        <image l:href="#{cover_id}"/>
      </coverpage>
    </title-info>
  </description>
  <body><section><p>text</p></section></body>
  <binary id="{cover_id}" content-type="{content_type}">{b64}</binary>
</FictionBook>"##
        )
    }

    // ---- find_cover_id -----------------------------------------

    #[test]
    fn cover_id_basic() {
        let xml = fb2_with_cover("cover.jpg", "image/jpeg", b"hello");
        assert_eq!(find_cover_id(&xml), Some("cover.jpg".into()));
    }

    #[test]
    fn cover_id_strips_hash_prefix() {
        let xml = r##"<FictionBook>
  <description><title-info><coverpage>
    <image href="#abc-123"/>
  </coverpage></title-info></description>
</FictionBook>"##;
        assert_eq!(find_cover_id(xml), Some("abc-123".into()));
    }

    #[test]
    fn cover_id_handles_namespace_prefix() {
        // l:href is the standard FB2 form (xlink namespace).
        let xml = r##"<FictionBook xmlns:l="http://www.w3.org/1999/xlink">
  <description><title-info><coverpage>
    <image l:href="#named-cover"/>
  </coverpage></title-info></description>
</FictionBook>"##;
        assert_eq!(find_cover_id(xml), Some("named-cover".into()));
    }

    #[test]
    fn cover_id_returns_none_when_no_coverpage() {
        let xml = r##"<FictionBook>
  <description><title-info><book-title>X</book-title></title-info></description>
</FictionBook>"##;
        assert_eq!(find_cover_id(xml), None);
    }

    #[test]
    fn cover_id_image_outside_coverpage_is_ignored() {
        // An <image> in <body> (very common in FB2!) must NOT be
        // mistaken for the cover.
        let xml = r##"<FictionBook>
  <description><title-info></title-info></description>
  <body><image href="#body-image"/></body>
</FictionBook>"##;
        assert_eq!(find_cover_id(xml), None);
    }

    #[test]
    fn cover_id_handles_garbage() {
        assert_eq!(find_cover_id("not xml"), None);
        assert_eq!(find_cover_id(""), None);
    }

    // ---- extract_binary_by_id ----------------------------------

    #[test]
    fn binary_extraction_decodes_payload() {
        let payload = b"hello world\x00\x01\x02";
        let xml = fb2_with_cover("c.png", "image/png", payload);
        let (name, bytes) = extract_binary_by_id(&xml, "c.png").expect("found");
        assert_eq!(name, "c.png");
        assert_eq!(bytes, payload);
    }

    #[test]
    fn binary_extraction_strips_whitespace_in_base64() {
        // Pretty-printed base64 (with line wraps and indentation)
        // is the common form. quick-xml's text events will hand us
        // the raw whitespace; we have to strip it before decoding.
        let payload = b"hello world!!";
        let b64 = BASE64.encode(payload);
        let half = b64.len() / 2;
        let (a, b) = b64.split_at(half);
        let xml = format!(
            r##"<FictionBook>
  <binary id="x" content-type="image/jpeg">
    {a}
    {b}
  </binary>
</FictionBook>"##
        );
        let (_name, bytes) = extract_binary_by_id(&xml, "x").expect("found");
        assert_eq!(bytes, payload);
    }

    #[test]
    fn binary_extraction_returns_none_for_unknown_id() {
        let xml = fb2_with_cover("cover.jpg", "image/jpeg", b"x");
        assert_eq!(extract_binary_by_id(&xml, "different-id"), None);
    }

    #[test]
    fn binary_extraction_synthesizes_extension_from_content_type() {
        // id has no dot → we tack on an extension from content-type.
        let xml = format!(
            r##"<FictionBook><binary id="img1" content-type="image/png">{}</binary></FictionBook>"##,
            BASE64.encode(b"abc")
        );
        let (name, _) = extract_binary_by_id(&xml, "img1").expect("found");
        assert_eq!(name, "img1.png");
    }

    #[test]
    fn binary_extraction_handles_jpeg_extension() {
        let xml = format!(
            r##"<FictionBook><binary id="i" content-type="image/jpeg">{}</binary></FictionBook>"##,
            BASE64.encode(b"abc")
        );
        let (name, _) = extract_binary_by_id(&xml, "i").expect("found");
        assert_eq!(name, "i.jpeg");
    }

    #[test]
    fn binary_extraction_strips_content_type_parameters() {
        // Content-types may carry parameters like `; charset=binary`.
        let xml = format!(
            r##"<FictionBook><binary id="i" content-type="image/png; charset=binary">{}</binary></FictionBook>"##,
            BASE64.encode(b"abc")
        );
        let (name, _) = extract_binary_by_id(&xml, "i").expect("found");
        assert_eq!(name, "i.png");
    }

    // ---- extract_first_image_binary ----------------------------

    #[test]
    fn fallback_picks_first_image_binary() {
        // Two binaries: a font (skipped) and an image (picked).
        let png_b64 = BASE64.encode(b"PNGDATA");
        let xml = format!(
            r##"<FictionBook>
  <binary id="font.ttf" content-type="application/x-font-truetype">AAAA</binary>
  <binary id="cover.png" content-type="image/png">{png_b64}</binary>
</FictionBook>"##
        );
        let (name, bytes) = extract_first_image_binary(&xml).expect("found");
        assert_eq!(name, "cover.png");
        assert_eq!(bytes, b"PNGDATA");
    }

    #[test]
    fn fallback_returns_none_when_no_image_binary() {
        let xml = r##"<FictionBook>
  <binary id="font.ttf" content-type="application/x-font-truetype">AAAA</binary>
</FictionBook>"##;
        assert_eq!(extract_first_image_binary(xml), None);
    }

    // ---- public try_extract_cover ------------------------------

    #[test]
    fn full_path_via_coverpage() {
        let payload = b"\x89PNG\r\n\x1a\nfake-png-bytes";
        let xml = fb2_with_cover("cover.png", "image/png", payload);
        let (name, bytes) = try_extract_cover(xml.as_bytes()).expect("cover");
        assert_eq!(name, "cover.png");
        assert_eq!(bytes, payload);
    }

    #[test]
    fn falls_back_when_cover_id_dangling() {
        // Cover declares "missing-id" but no such binary exists.
        // We should fall back to the first image binary.
        let png_b64 = BASE64.encode(b"PNG");
        let xml = format!(
            r##"<FictionBook xmlns:l="http://www.w3.org/1999/xlink">
  <description><title-info><coverpage>
    <image l:href="#missing-id"/>
  </coverpage></title-info></description>
  <binary id="other.png" content-type="image/png">{png_b64}</binary>
</FictionBook>"##
        );
        let (name, bytes) = try_extract_cover(xml.as_bytes()).expect("cover");
        assert_eq!(name, "other.png");
        assert_eq!(bytes, b"PNG");
    }

    #[test]
    fn falls_back_when_no_coverpage() {
        // No coverpage at all but there's an image binary.
        let png_b64 = BASE64.encode(b"PNG");
        let xml = format!(
            r##"<FictionBook>
  <description><title-info><book-title>X</book-title></title-info></description>
  <binary id="img.png" content-type="image/png">{png_b64}</binary>
</FictionBook>"##
        );
        let (name, _) = try_extract_cover(xml.as_bytes()).expect("cover");
        assert_eq!(name, "img.png");
    }

    #[test]
    fn returns_none_when_no_image_anywhere() {
        let xml = r##"<FictionBook>
  <description><title-info><book-title>X</book-title></title-info></description>
  <body><section><p>text only</p></section></body>
</FictionBook>"##;
        assert_eq!(try_extract_cover(xml.as_bytes()), None);
    }

    #[test]
    fn handles_non_utf8_input_gracefully() {
        // Build a "FictionBook" XML and inject a stray non-UTF-8
        // byte in the body. The structure parser should still find
        // the cover binary because the XML metacharacters and the
        // attribute values we care about are all ASCII.
        let mut xml_bytes = fb2_with_cover("c.png", "image/png", b"PIX").into_bytes();
        // Find the body text and replace one ASCII char with 0xFF.
        if let Some(pos) = xml_bytes.windows(4).position(|w| w == b"text") {
            xml_bytes[pos] = 0xFF;
        }
        let (name, bytes) = try_extract_cover(&xml_bytes).expect("cover");
        assert_eq!(name, "c.png");
        assert_eq!(bytes, b"PIX");
    }
}
