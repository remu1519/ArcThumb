//! EPUB cover-image extraction.
//!
//! EPUB is a ZIP archive with a documented metadata layer that names
//! the cover image. We follow the standard chain:
//!
//! 1. Read `META-INF/container.xml` → find the OPF file's path
//! 2. Read the OPF (XML) → find the cover image's manifest entry
//! 3. Resolve the manifest `href` relative to the OPF's directory
//! 4. Read that file from the ZIP
//!
//! Two ways the cover can be declared in the OPF:
//!
//! - **EPUB 2**: `<metadata>` contains `<meta name="cover" content="cover-id"/>`,
//!   then `<manifest>` has `<item id="cover-id" href="..."/>`.
//! - **EPUB 3**: `<manifest>` has `<item properties="cover-image" href="..."/>`,
//!   no metadata indirection. EPUB 3 takes priority when both are present.
//!
//! Both forms exist in the wild — many publishers ship EPUB 2 even
//! today — so we look for both in a single pass over the OPF.
//!
//! On any failure (missing container.xml, broken XML, missing manifest
//! entry, missing image file), this module returns `None` to signal
//! "fall back to the generic ZIP image scan". That keeps slightly
//! malformed EPUBs viewable instead of icon-only.

use std::collections::HashMap;
use std::io::{Read, Seek};

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

/// Try to extract the EPUB cover from an already-opened ZIP archive.
///
/// Returns `Some((zip_path, bytes))` if a cover was found via OPF
/// metadata, `None` if this isn't an EPUB or the metadata chain
/// couldn't be resolved (in which case the caller should fall back
/// to its own scan).
pub fn try_extract_cover<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Option<(String, Vec<u8>)> {
    // Step 1: container.xml → OPF path
    let container_xml = read_entry_to_string(archive, "META-INF/container.xml")?;
    let opf_path = parse_container_xml(&container_xml)?;

    // Step 2: OPF → cover href
    let opf_xml = read_entry_to_string(archive, &opf_path)?;
    let cover_href = find_cover_href(&opf_xml)?;

    // Step 3: resolve cover href relative to the OPF's directory
    let opf_dir = parent_dir(&opf_path);
    let zip_path = join_zip_path(opf_dir, &cover_href);

    // Step 4: read the cover file from the ZIP
    let mut entry = archive.by_name(&zip_path).ok()?;
    let size = entry.size() as usize;
    let mut bytes = Vec::with_capacity(size);
    entry.read_to_end(&mut bytes).ok()?;

    Some((zip_path, bytes))
}

// =============================================================================
// XML parsing helpers
// =============================================================================

/// Read a UTF-8 text entry out of a ZIP archive. Returns `None` on
/// any I/O failure or if the entry is missing.
fn read_entry_to_string<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Option<String> {
    let mut entry = archive.by_name(name).ok()?;
    let mut s = String::new();
    entry.read_to_string(&mut s).ok()?;
    Some(s)
}

/// Strip an XML namespace prefix (`opf:item` → `item`). EPUB OPF
/// files often use a namespace prefix and we don't want to do real
/// namespace resolution for two-tag matching.
fn strip_namespace(name: &[u8]) -> &[u8] {
    match name.iter().position(|&b| b == b':') {
        Some(idx) => &name[idx + 1..],
        None => name,
    }
}

/// True if `e`'s tag name (with any namespace prefix stripped) equals
/// `expected`. Inlined as a helper because the borrow checker needs
/// the `QName` to live in a named local while we look at its bytes.
fn local_name_eq(e: &BytesStart, expected: &[u8]) -> bool {
    let qname = e.name();
    strip_namespace(qname.as_ref()) == expected
}

/// Find the value of an attribute by local name (namespace prefixes
/// stripped). Decodes XML character entities so paths containing
/// `&amp;` round-trip correctly.
fn attr_value(e: &BytesStart, reader: &Reader<&[u8]>, key: &[u8]) -> Option<String> {
    for attr in e.attributes().flatten() {
        let attr_qname = attr.key;
        let attr_local = strip_namespace(attr_qname.as_ref());
        if attr_local == key {
            return attr
                .decode_and_unescape_value(reader.decoder())
                .ok()
                .map(|cow| cow.into_owned());
        }
    }
    None
}

/// Parse `META-INF/container.xml` and return the `full-path` of the
/// first `<rootfile>` (the OPF). Container.xml can technically list
/// multiple rootfiles for "renditions" but in practice EPUBs use
/// just one and we always pick the first.
fn parse_container_xml(xml: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(e) | Event::Start(e)) => {
                if let Some(path) = rootfile_full_path(&e) {
                    return Some(path);
                }
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

/// Return the `full-path` attribute of a `<rootfile>` element, or
/// `None` if `e` is not a `<rootfile>` or has no `full-path`. Lifted
/// out of `parse_container_xml` so the caller's match arm stays flat.
fn rootfile_full_path(e: &BytesStart) -> Option<String> {
    if !local_name_eq(e, b"rootfile") {
        return None;
    }
    attr_value(e, &Reader::from_str(""), b"full-path")
}

/// Single-pass scan of an OPF document. Collects manifest items,
/// remembers the EPUB 2 `cover` meta indirection, and remembers any
/// EPUB 3 `properties="cover-image"` direct hit. EPUB 3 wins when
/// both are present.
fn find_cover_href(xml: &str) -> Option<String> {
    let mut items: HashMap<String, String> = HashMap::new();
    let mut epub2_cover_id: Option<String> = None;
    let mut epub3_cover_href: Option<String> = None;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) => {
                if local_name_eq(&e, b"item") {
                    handle_item(&e, &reader, &mut items, &mut epub3_cover_href);
                } else if local_name_eq(&e, b"meta") {
                    handle_meta(&e, &reader, &mut epub2_cover_id);
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    // EPUB 3 takes priority — it's the more specific mechanism.
    if let Some(href) = epub3_cover_href {
        return Some(href);
    }
    // EPUB 2: indirect via cover-id → manifest item href.
    if let Some(id) = epub2_cover_id
        && let Some(href) = items.get(&id)
    {
        return Some(href.clone());
    }
    None
}

fn handle_item(
    e: &BytesStart,
    reader: &Reader<&[u8]>,
    items: &mut HashMap<String, String>,
    epub3_cover_href: &mut Option<String>,
) {
    let id = attr_value(e, reader, b"id");
    let href = attr_value(e, reader, b"href");
    let properties = attr_value(e, reader, b"properties");

    if let (Some(id), Some(href)) = (id, href.clone()) {
        items.insert(id, href.clone());
    }
    // EPUB 3: properties is a space-separated token list. Match
    // exactly one of the tokens, not a substring (so a hypothetical
    // `super-cover-image` wouldn't false-positive).
    if let (Some(props), Some(href)) = (properties, href)
        && props.split_ascii_whitespace().any(|p| p == "cover-image")
        && epub3_cover_href.is_none()
    {
        *epub3_cover_href = Some(href);
    }
}

fn handle_meta(e: &BytesStart, reader: &Reader<&[u8]>, epub2_cover_id: &mut Option<String>) {
    let name = attr_value(e, reader, b"name");
    let content = attr_value(e, reader, b"content");
    if let (Some(n), Some(c)) = (name, content)
        && n == "cover"
        && epub2_cover_id.is_none()
    {
        *epub2_cover_id = Some(c);
    }
}

// =============================================================================
// Path helpers
// =============================================================================

/// Return the directory portion of a ZIP entry path, including the
/// trailing `/`. Returns the empty string if the path has no slash.
fn parent_dir(path: &str) -> &str {
    match path.rfind('/') {
        Some(idx) => &path[..idx + 1],
        None => "",
    }
}

/// Join an OPF directory and a manifest `href` into a ZIP entry path.
///
/// EPUB hrefs are relative to the OPF file's location. They almost
/// always use forward slashes (the EPUB spec mandates it), but we
/// also handle:
/// - hrefs starting with `/` → treat as absolute from ZIP root
/// - hrefs containing `../` → resolve them by collapsing path components
fn join_zip_path(opf_dir: &str, href: &str) -> String {
    if let Some(stripped) = href.strip_prefix('/') {
        return stripped.to_string();
    }
    let combined = format!("{opf_dir}{href}");
    normalize_path(&combined)
}

/// Collapse `..` and `.` segments in a forward-slash path. Used to
/// turn `OEBPS/text/../images/cover.jpg` into `OEBPS/images/cover.jpg`.
fn normalize_path(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => continue,
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out.join("/")
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- low-level helpers -------------------------------------

    #[test]
    fn parent_dir_root() {
        assert_eq!(parent_dir("content.opf"), "");
    }

    #[test]
    fn parent_dir_one_level() {
        assert_eq!(parent_dir("OEBPS/content.opf"), "OEBPS/");
    }

    #[test]
    fn parent_dir_nested() {
        assert_eq!(parent_dir("a/b/c/file.opf"), "a/b/c/");
    }

    #[test]
    fn join_zip_path_simple() {
        assert_eq!(
            join_zip_path("OEBPS/", "images/cover.jpg"),
            "OEBPS/images/cover.jpg"
        );
    }

    #[test]
    fn join_zip_path_root_opf() {
        // OPF at root: parent_dir is empty.
        assert_eq!(join_zip_path("", "cover.png"), "cover.png");
    }

    #[test]
    fn join_zip_path_absolute_href() {
        // Hrefs starting with / are taken as ZIP-root absolute,
        // overriding the OPF directory.
        assert_eq!(
            join_zip_path("OEBPS/", "/images/cover.jpg"),
            "images/cover.jpg"
        );
    }

    #[test]
    fn join_zip_path_with_parent_traversal() {
        // .. should collapse properly so we don't mis-name the entry.
        assert_eq!(
            join_zip_path("OEBPS/text/", "../images/cover.jpg"),
            "OEBPS/images/cover.jpg"
        );
    }

    // ---- container.xml parsing ---------------------------------

    #[test]
    fn container_xml_finds_rootfile() {
        let xml = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#;
        assert_eq!(parse_container_xml(xml), Some("OEBPS/content.opf".into()));
    }

    #[test]
    fn container_xml_root_level_opf() {
        let xml =
            r#"<container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>"#;
        assert_eq!(parse_container_xml(xml), Some("content.opf".into()));
    }

    #[test]
    fn container_xml_picks_first_rootfile() {
        // Multiple renditions: we always take the first one.
        let xml = r#"<container>
  <rootfiles>
    <rootfile full-path="alt/main.opf"/>
    <rootfile full-path="other.opf"/>
  </rootfiles>
</container>"#;
        assert_eq!(parse_container_xml(xml), Some("alt/main.opf".into()));
    }

    #[test]
    fn container_xml_handles_namespace_prefix() {
        // Some EPUBs use a namespace prefix on the rootfile element.
        let xml = r#"<ocf:container xmlns:ocf="urn:oasis:names:tc:opendocument:xmlns:container">
  <ocf:rootfiles>
    <ocf:rootfile full-path="content.opf"/>
  </ocf:rootfiles>
</ocf:container>"#;
        assert_eq!(parse_container_xml(xml), Some("content.opf".into()));
    }

    #[test]
    fn container_xml_returns_none_when_empty() {
        assert_eq!(parse_container_xml("<container/>"), None);
    }

    #[test]
    fn container_xml_returns_none_for_garbage() {
        assert_eq!(parse_container_xml("not xml at all"), None);
    }

    // ---- OPF parsing -------------------------------------------

    #[test]
    fn opf_epub2_meta_cover() {
        let opf = r#"<?xml version="1.0"?>
<package version="2.0" xmlns="http://www.idpf.org/2007/opf">
  <metadata>
    <meta name="cover" content="cover-image"/>
    <meta name="something-else" content="ignore-me"/>
  </metadata>
  <manifest>
    <item id="cover-image" href="images/cover.jpg" media-type="image/jpeg"/>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
</package>"#;
        assert_eq!(find_cover_href(opf), Some("images/cover.jpg".into()));
    }

    #[test]
    fn opf_epub3_properties_cover_image() {
        let opf = r#"<?xml version="1.0"?>
<package version="3.0" xmlns="http://www.idpf.org/2007/opf">
  <metadata/>
  <manifest>
    <item id="cover" href="img/front.png" media-type="image/png" properties="cover-image"/>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
</package>"#;
        assert_eq!(find_cover_href(opf), Some("img/front.png".into()));
    }

    #[test]
    fn opf_epub3_priority_over_epub2() {
        // Some EPUBs declare both for backward compatibility. The
        // EPUB 3 properties match should win.
        let opf = r#"<package>
  <metadata>
    <meta name="cover" content="legacy"/>
  </metadata>
  <manifest>
    <item id="legacy" href="old.jpg"/>
    <item id="modern" href="new.jpg" properties="cover-image"/>
  </manifest>
</package>"#;
        assert_eq!(find_cover_href(opf), Some("new.jpg".into()));
    }

    #[test]
    fn opf_properties_token_list_matches() {
        // properties is a space-separated token list. We must match
        // a whole token, not a substring.
        let opf = r#"<package>
  <manifest>
    <item id="x" href="a.jpg" properties="svg cover-image scripted"/>
  </manifest>
</package>"#;
        assert_eq!(find_cover_href(opf), Some("a.jpg".into()));
    }

    #[test]
    fn opf_properties_substring_does_not_match() {
        // "super-cover-image" should NOT count as a cover-image hit.
        let opf = r#"<package>
  <manifest>
    <item id="x" href="a.jpg" properties="super-cover-image"/>
  </manifest>
</package>"#;
        assert_eq!(find_cover_href(opf), None);
    }

    #[test]
    fn opf_returns_none_when_no_cover() {
        let opf = r#"<package>
  <metadata/>
  <manifest>
    <item id="ch1" href="ch1.xhtml"/>
    <item id="img1" href="random.jpg"/>
  </manifest>
</package>"#;
        assert_eq!(find_cover_href(opf), None);
    }

    #[test]
    fn opf_meta_cover_dangling_id() {
        // The meta points at an id the manifest doesn't have. We
        // should return None rather than guessing.
        let opf = r#"<package>
  <metadata>
    <meta name="cover" content="missing-id"/>
  </metadata>
  <manifest>
    <item id="other" href="other.jpg"/>
  </manifest>
</package>"#;
        assert_eq!(find_cover_href(opf), None);
    }

    #[test]
    fn opf_handles_namespace_prefixes() {
        // EPUB 2 OPFs sometimes use opf:meta etc.
        let opf = r#"<opf:package xmlns:opf="http://www.idpf.org/2007/opf">
  <opf:metadata>
    <opf:meta name="cover" content="c"/>
  </opf:metadata>
  <opf:manifest>
    <opf:item id="c" href="cov.png"/>
  </opf:manifest>
</opf:package>"#;
        assert_eq!(find_cover_href(opf), Some("cov.png".into()));
    }

    #[test]
    fn opf_decodes_xml_entities_in_href() {
        // hrefs containing & must be encoded as &amp; in valid XML.
        let opf = r#"<package>
  <manifest>
    <item id="c" href="dir/file&amp;name.jpg" properties="cover-image"/>
  </manifest>
</package>"#;
        assert_eq!(find_cover_href(opf), Some("dir/file&name.jpg".into()));
    }
}
