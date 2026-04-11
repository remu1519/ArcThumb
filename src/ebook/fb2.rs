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

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
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
    if let Some(cover_id) = find_cover_id(&xml)
        && let Some(result) = extract_binary_by_id(&xml, &cover_id)
    {
        return Some(result);
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

    // Simple depth counters rather than a path stack — FB2 only
    // ever has one title-info / coverpage active at a time.
    let mut in_title_info: i32 = 0;
    let mut in_coverpage: i32 = 0;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                update_section_depth(e.name(), 1, &mut in_title_info, &mut in_coverpage);
            }
            Ok(Event::End(e)) => {
                update_section_depth(e.name(), -1, &mut in_title_info, &mut in_coverpage);
            }
            Ok(Event::Empty(e)) if in_title_info > 0 && in_coverpage > 0 => {
                if let Some(id) = cover_image_id(&e, &reader) {
                    return Some(id);
                }
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

/// Adjust the title-info / coverpage nesting counters when a tag of
/// the same name is entered (`delta = 1`) or exited (`delta = -1`).
/// Other tags are ignored.
fn update_section_depth(name: QName, delta: i32, in_title_info: &mut i32, in_coverpage: &mut i32) {
    if qname_local_eq(name, b"title-info") {
        *in_title_info += delta;
    } else if qname_local_eq(name, b"coverpage") {
        *in_coverpage += delta;
    }
}

/// Extract the binary id from a `<image l:href="#id"/>` element.
/// Returns `None` if `e` is not an `<image>`, has no `href`, or
/// resolves to the empty string after stripping the leading `#`.
fn cover_image_id(e: &BytesStart, reader: &Reader<&[u8]>) -> Option<String> {
    if !qname_local_eq(e.name(), b"image") {
        return None;
    }
    let href = attr_value(e, reader, b"href")?;
    // The href is "#binary_id"; strip the leading `#`. Some malformed
    // FB2s omit it, so the call is `unwrap_or` not `?`.
    let id = href.strip_prefix('#').unwrap_or(&href);
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
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
    // We strip whitespace ourselves in `BinaryCollector::finish`.
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut collector = BinaryCollector::default();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => collector.try_enter(&e, &reader, &matcher),
            Ok(Event::End(e)) if collector.try_exit(&e) => break,
            Ok(Event::Text(t)) if collector.is_active() => collector.append_text(&t),
            Ok(Event::CData(t)) if collector.is_active() => collector.append_text(&t),
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    collector.finish()
}

/// Accumulator for the body text of one matching `<binary>` element.
///
/// Owns all the state the previous flat loop had to juggle by hand:
/// whether we are currently inside a target binary, the captured id
/// and content-type, and the appended text. Splitting it out makes
/// `extract_binary_matching` short enough to fit on one screen and
/// gives the post-loop "decode + name" logic an obvious home.
#[derive(Default)]
struct BinaryCollector {
    /// `Some` once we have entered a matching `<binary>`. Stays
    /// `Some` after `</binary>` so `finish()` can use it.
    target_id: Option<String>,
    target_ct: Option<String>,
    /// Raw concatenated `<binary>` body, possibly containing
    /// whitespace from XML pretty-printing. Cleaned in `finish`.
    text: String,
}

impl BinaryCollector {
    /// True while we are inside a matching `<binary>` and should
    /// accumulate `Text` / `CData` events into `text`.
    fn is_active(&self) -> bool {
        self.target_id.is_some()
    }

    /// Try to mark the start of a `<binary>` we care about. Bails
    /// out silently for non-binary tags, already-active state, or
    /// binaries the matcher rejects.
    fn try_enter<F>(&mut self, e: &BytesStart, reader: &Reader<&[u8]>, matcher: &F)
    where
        F: Fn(&str, Option<&str>) -> bool,
    {
        if self.is_active() || !qname_local_eq(e.name(), b"binary") {
            return;
        }
        let Some(id) = attr_value(e, reader, b"id") else {
            return;
        };
        let ct = attr_value(e, reader, b"content-type");
        if !matcher(&id, ct.as_deref()) {
            return;
        }
        self.target_id = Some(id);
        self.target_ct = ct;
        self.text.clear();
    }

    /// Returns `true` if `e` is the closing `</binary>` for the
    /// element we entered, signalling the caller to stop reading.
    fn try_exit(&self, e: &quick_xml::events::BytesEnd) -> bool {
        self.is_active() && qname_local_eq(e.name(), b"binary")
    }

    /// Append a chunk of `Text` or `CData` content. Invalid UTF-8
    /// is silently dropped — base64 is ASCII so this only matters
    /// for malformed inputs.
    fn append_text(&mut self, bytes: &[u8]) {
        if let Ok(s) = std::str::from_utf8(bytes) {
            self.text.push_str(s);
        }
    }

    /// Decode the captured base64 and synthesize a filename. Returns
    /// `None` if no target binary was ever entered or the base64
    /// failed to decode.
    fn finish(self) -> Option<(String, Vec<u8>)> {
        let id = self.target_id?;
        // Strip whitespace before decoding — XML pretty-printers
        // wrap base64 across multiple lines and the standard base64
        // alphabet rejects whitespace.
        let cleaned: String = self
            .text
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        let decoded = BASE64.decode(cleaned.as_bytes()).ok()?;
        let name = synthesize_filename(id, self.target_ct.as_deref());
        Some((name, decoded))
    }
}

/// Build a friendly filename for a decoded binary. If `id` already
/// looks like one (has a dot), use it verbatim; otherwise append an
/// extension derived from `content_type`.
fn synthesize_filename(id: String, content_type: Option<&str>) -> String {
    if id.contains('.') {
        return id;
    }
    let ext = content_type
        .and_then(|ct| ct.strip_prefix("image/"))
        .map(|sub| {
            // Trim any "+xml" suffix etc. and pick a filename-safe
            // extension. Common cases: jpeg, jpg, png, gif, webp.
            let cut = sub.find(['+', ';']).unwrap_or(sub.len());
            sub[..cut].to_string()
        })
        .unwrap_or_else(|| "img".to_string());
    format!("{id}.{ext}")
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

    // =========================================================================
    // Helper-level tests added with the Phase 3 refactor.
    //
    // The tests above exercise `try_extract_cover` end-to-end via small
    // FB2 strings. The tests below pin the contract of the individual
    // helpers (`synthesize_filename`, `update_section_depth`,
    // `BinaryCollector`) so a future regression in any one of them
    // shows up directly with the failing helper named, rather than as
    // a vague "the cover came back wrong" failure two layers up.
    // =========================================================================

    // ---- synthesize_filename --------------------------------------

    #[test]
    fn synthesize_filename_uses_id_verbatim_when_it_has_dot() {
        let name = synthesize_filename("cover.png".to_string(), Some("image/jpeg"));
        // The id already looks like a filename — content-type is ignored.
        assert_eq!(name, "cover.png");
    }

    #[test]
    fn synthesize_filename_appends_extension_from_image_jpeg() {
        let name = synthesize_filename("front".to_string(), Some("image/jpeg"));
        assert_eq!(name, "front.jpeg");
    }

    #[test]
    fn synthesize_filename_appends_extension_from_image_png() {
        let name = synthesize_filename("c".to_string(), Some("image/png"));
        assert_eq!(name, "c.png");
    }

    #[test]
    fn synthesize_filename_strips_xml_suffix() {
        // image/svg+xml → svg, not "svg+xml"
        let name = synthesize_filename("logo".to_string(), Some("image/svg+xml"));
        assert_eq!(name, "logo.svg");
    }

    #[test]
    fn synthesize_filename_strips_content_type_parameters() {
        // image/png; charset=utf-8 → png
        let name = synthesize_filename("c".to_string(), Some("image/png; charset=utf-8"));
        assert_eq!(name, "c.png");
    }

    #[test]
    fn synthesize_filename_falls_back_when_content_type_missing() {
        let name = synthesize_filename("cover".to_string(), None);
        assert_eq!(name, "cover.img");
    }

    #[test]
    fn synthesize_filename_falls_back_when_content_type_not_image() {
        // No image/ prefix → use the generic "img" extension.
        let name = synthesize_filename("doc".to_string(), Some("text/plain"));
        assert_eq!(name, "doc.img");
    }

    // ---- update_section_depth -------------------------------------

    /// Build a `QName` from a literal byte string for use in tests.
    /// `QName::from(...)` accepts a `&[u8]` so this is just a thin
    /// wrapper that lets the test bodies stay readable.
    fn qname(s: &[u8]) -> QName<'_> {
        QName(s)
    }

    #[test]
    fn update_section_depth_increments_title_info() {
        let mut ti = 0;
        let mut cp = 0;
        update_section_depth(qname(b"title-info"), 1, &mut ti, &mut cp);
        assert_eq!(ti, 1);
        assert_eq!(cp, 0);
    }

    #[test]
    fn update_section_depth_decrements_title_info() {
        let mut ti = 1;
        let mut cp = 0;
        update_section_depth(qname(b"title-info"), -1, &mut ti, &mut cp);
        assert_eq!(ti, 0);
        assert_eq!(cp, 0);
    }

    #[test]
    fn update_section_depth_tracks_coverpage_independently() {
        let mut ti = 0;
        let mut cp = 0;
        update_section_depth(qname(b"coverpage"), 1, &mut ti, &mut cp);
        assert_eq!(ti, 0);
        assert_eq!(cp, 1);
        update_section_depth(qname(b"coverpage"), -1, &mut ti, &mut cp);
        assert_eq!(cp, 0);
    }

    #[test]
    fn update_section_depth_ignores_other_tags() {
        let mut ti = 5;
        let mut cp = 7;
        update_section_depth(qname(b"body"), 1, &mut ti, &mut cp);
        update_section_depth(qname(b"section"), -1, &mut ti, &mut cp);
        update_section_depth(qname(b"image"), 1, &mut ti, &mut cp);
        assert_eq!(ti, 5);
        assert_eq!(cp, 7);
    }

    #[test]
    fn update_section_depth_namespace_prefix_is_handled_by_qname_eq() {
        // qname_local_eq strips namespaces, so a `fb:title-info`
        // event still bumps the counter.
        let mut ti = 0;
        let mut cp = 0;
        update_section_depth(qname(b"fb:title-info"), 1, &mut ti, &mut cp);
        assert_eq!(ti, 1);
    }

    // ---- BinaryCollector ------------------------------------------

    /// Parse a one-element XML snippet and run `f` with the resulting
    /// `BytesStart`. Lifted out so the per-test setup is one line.
    fn with_first_start<R>(xml: &str, f: impl FnOnce(&BytesStart, &Reader<&[u8]>) -> R) -> R {
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(false);
        let mut buf = Vec::new();
        loop {
            let event = reader.read_event_into(&mut buf).expect("xml");
            match event {
                Event::Start(ref e) | Event::Empty(ref e) => return f(e, &reader),
                Event::Eof => panic!("no Start event in test xml"),
                _ => {}
            }
        }
    }

    #[test]
    fn binary_collector_default_is_inactive_and_empty() {
        let collector = BinaryCollector::default();
        assert!(!collector.is_active());
        assert!(collector.finish().is_none());
    }

    #[test]
    fn binary_collector_try_enter_ignores_non_binary_tags() {
        let mut collector = BinaryCollector::default();
        with_first_start(r#"<title-info></title-info>"#, |e, r| {
            collector.try_enter(e, r, &|_, _| true);
        });
        assert!(!collector.is_active());
    }

    #[test]
    fn binary_collector_try_enter_ignores_binary_when_matcher_rejects() {
        let mut collector = BinaryCollector::default();
        with_first_start(
            r#"<binary id="other.jpg" content-type="image/jpeg"></binary>"#,
            |e, r| collector.try_enter(e, r, &|id, _| id == "wanted.jpg"),
        );
        assert!(!collector.is_active());
    }

    #[test]
    fn binary_collector_try_enter_captures_id_and_ct_when_matcher_accepts() {
        let mut collector = BinaryCollector::default();
        with_first_start(
            r#"<binary id="cover.jpg" content-type="image/jpeg"></binary>"#,
            |e, r| collector.try_enter(e, r, &|_, _| true),
        );
        assert!(collector.is_active());
        assert_eq!(collector.target_id.as_deref(), Some("cover.jpg"));
        assert_eq!(collector.target_ct.as_deref(), Some("image/jpeg"));
    }

    #[test]
    fn binary_collector_try_enter_is_noop_while_already_active() {
        let mut collector = BinaryCollector::default();
        with_first_start(
            r#"<binary id="first.jpg" content-type="image/jpeg"></binary>"#,
            |e, r| collector.try_enter(e, r, &|_, _| true),
        );
        // Second try_enter on a different binary must NOT overwrite
        // the first one — once we're locked onto a target, we stay
        // locked until try_exit fires.
        with_first_start(
            r#"<binary id="second.jpg" content-type="image/png"></binary>"#,
            |e, r| collector.try_enter(e, r, &|_, _| true),
        );
        assert_eq!(collector.target_id.as_deref(), Some("first.jpg"));
        assert_eq!(collector.target_ct.as_deref(), Some("image/jpeg"));
    }

    #[test]
    fn binary_collector_try_enter_skips_binary_without_id() {
        let mut collector = BinaryCollector::default();
        with_first_start(r#"<binary content-type="image/jpeg"></binary>"#, |e, r| {
            collector.try_enter(e, r, &|_, _| true)
        });
        assert!(!collector.is_active());
    }

    #[test]
    fn binary_collector_append_text_concatenates_chunks() {
        let mut collector = BinaryCollector::default();
        with_first_start(
            r#"<binary id="x.png" content-type="image/png"></binary>"#,
            |e, r| collector.try_enter(e, r, &|_, _| true),
        );
        collector.append_text(b"abc");
        collector.append_text(b"def");
        assert_eq!(collector.text, "abcdef");
    }

    #[test]
    fn binary_collector_append_text_drops_invalid_utf8_silently() {
        let mut collector = BinaryCollector::default();
        with_first_start(
            r#"<binary id="x.png" content-type="image/png"></binary>"#,
            |e, r| collector.try_enter(e, r, &|_, _| true),
        );
        collector.append_text(b"good");
        collector.append_text(&[0xFF, 0xFE]); // invalid UTF-8
        collector.append_text(b"bytes");
        // The invalid chunk is dropped wholesale; the surrounding
        // valid chunks survive.
        assert_eq!(collector.text, "goodbytes");
    }

    #[test]
    fn binary_collector_finish_returns_none_when_never_entered() {
        let collector = BinaryCollector::default();
        assert!(collector.finish().is_none());
    }

    #[test]
    fn binary_collector_finish_decodes_clean_base64() {
        let mut collector = BinaryCollector::default();
        with_first_start(
            r#"<binary id="hi.bin" content-type="image/png"></binary>"#,
            |e, r| collector.try_enter(e, r, &|_, _| true),
        );
        // Base64 of b"hello"
        collector.append_text(b"aGVsbG8=");
        let (name, bytes) = collector.finish().expect("decoded");
        assert_eq!(name, "hi.bin");
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn binary_collector_finish_strips_whitespace_in_base64() {
        let mut collector = BinaryCollector::default();
        with_first_start(
            r#"<binary id="hi.bin" content-type="image/png"></binary>"#,
            |e, r| collector.try_enter(e, r, &|_, _| true),
        );
        // XML pretty-printers love wrapping base64 across lines.
        // The collector strips spaces / newlines / tabs before
        // handing the payload to base64.
        collector.append_text(b"aGVs\n  bG8=\t");
        let (_, bytes) = collector.finish().expect("decoded");
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn binary_collector_finish_returns_none_for_invalid_base64() {
        let mut collector = BinaryCollector::default();
        with_first_start(
            r#"<binary id="x.bin" content-type="image/png"></binary>"#,
            |e, r| collector.try_enter(e, r, &|_, _| true),
        );
        // `!!!` is not valid base64 alphabet.
        collector.append_text(b"!!!");
        assert!(collector.finish().is_none());
    }

    #[test]
    fn binary_collector_finish_uses_synthesize_filename_for_extensionless_id() {
        let mut collector = BinaryCollector::default();
        with_first_start(
            r#"<binary id="cover" content-type="image/png"></binary>"#,
            |e, r| collector.try_enter(e, r, &|_, _| true),
        );
        collector.append_text(b"aGVsbG8=");
        let (name, _) = collector.finish().expect("decoded");
        // id had no dot, so synthesize_filename appended the
        // content-type-derived extension.
        assert_eq!(name, "cover.png");
    }
}
