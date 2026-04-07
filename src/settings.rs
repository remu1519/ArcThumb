//! User-tweakable settings, read from `HKCU\Software\ArcThumb`.
//!
//! All keys are optional. Missing or malformed keys fall back to the
//! built-in defaults. Settings are loaded once per Explorer process
//! and cached — changes take effect after restarting Explorer.
//!
//! ## Registry layout
//!
//! ```text
//! HKEY_CURRENT_USER\Software\ArcThumb
//!     SortOrder        REG_SZ    "natural" | "alphabetical"
//!     PreferCoverNames REG_DWORD 0 | 1
//! ```
//!
//! Users can tweak these by hand in `regedit` until a proper config
//! GUI (Phase 4f.2) exists.

use std::cmp::Ordering;
use std::sync::OnceLock;

use winreg::enums::*;
use winreg::RegKey;

/// How to order image files within an archive before picking the
/// "first" one for the thumbnail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    /// Plain byte-wise sort. `page10.jpg` comes before `page2.jpg`.
    Alphabetical,
    /// Natural sort: runs of digits compared numerically so
    /// `page2.jpg` comes before `page10.jpg`.
    Natural,
}

impl Default for SortOrder {
    fn default() -> Self {
        Self::Natural
    }
}

impl SortOrder {
    fn from_registry_value(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "alphabetical" | "alpha" => Some(Self::Alphabetical),
            "natural" | "nat" => Some(Self::Natural),
            _ => None,
        }
    }

    /// Canonical registry string form. Paired with `from_registry_value`.
    pub fn as_registry_value(self) -> &'static str {
        match self {
            Self::Alphabetical => "alphabetical",
            Self::Natural => "natural",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub sort_order: SortOrder,
    /// When true, files named `cover.*`, `folder.*`, `thumb.*`,
    /// `thumbnail.*`, or `front.*` are preferred over the first
    /// image by sort order.
    pub prefer_cover_names: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sort_order: SortOrder::Natural,
            prefer_cover_names: true,
        }
    }
}

impl Settings {
    /// Read settings from `HKCU\Software\ArcThumb` without touching the
    /// process-wide cache. The config GUI uses this so each "Apply"
    /// round sees fresh registry state.
    pub fn load_from_registry_uncached() -> Self {
        let mut out = Self::default();
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let Ok(key) = hkcu.open_subkey("Software\\ArcThumb") else {
            return out;
        };

        if let Ok(s) = key.get_value::<String, _>("SortOrder") {
            if let Some(order) = SortOrder::from_registry_value(&s) {
                out.sort_order = order;
            }
        }
        if let Ok(v) = key.get_value::<u32, _>("PreferCoverNames") {
            out.prefer_cover_names = v != 0;
        }
        out
    }

    /// Write `sort_order` and `prefer_cover_names` to
    /// `HKCU\Software\ArcThumb`. Creates the key if missing. Leaves
    /// other values (e.g. `Language`) untouched.
    pub fn save_to_registry(&self) -> std::io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu.create_subkey("Software\\ArcThumb")?;
        key.set_value("SortOrder", &self.sort_order.as_registry_value().to_string())?;
        let flag: u32 = if self.prefer_cover_names { 1 } else { 0 };
        key.set_value("PreferCoverNames", &flag)?;
        Ok(())
    }
}

/// Process-wide cached settings. Loaded lazily on first use and
/// held for the lifetime of the Explorer process. Restart Explorer
/// to pick up registry edits.
pub fn current() -> &'static Settings {
    static CACHE: OnceLock<Settings> = OnceLock::new();
    CACHE.get_or_init(Settings::load_from_registry_uncached)
}

// =============================================================================
// Image selection: used by every archive backend to turn a list of
// candidate image filenames into the one we'll decode.
// =============================================================================

/// Pick the "best" image name from a list of candidates according
/// to the current user settings.
pub fn pick_first_image(mut names: Vec<String>) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let s = current();

    match s.sort_order {
        SortOrder::Alphabetical => names.sort(),
        SortOrder::Natural => names.sort_by(|a, b| natural_cmp(a, b)),
    }

    if s.prefer_cover_names {
        if let Some(cover) = names.iter().find(|n| is_cover_name(n)) {
            return Some(cover.clone());
        }
    }

    names.into_iter().next()
}

/// Is this path a well-known cover-image filename? Checks the
/// basename (without extension) against a small allowlist.
fn is_cover_name(path: &str) -> bool {
    // Take whatever's after the last `/` or `\` — archive formats
    // use both depending on origin.
    let basename = path
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(path);
    let stem = basename
        .rsplit_once('.')
        .map(|(s, _)| s)
        .unwrap_or(basename);
    matches!(
        stem.to_ascii_lowercase().as_str(),
        "cover" | "folder" | "thumb" | "thumbnail" | "front"
    )
}

/// Natural sort comparator: runs of ASCII digits compared as
/// integers, everything else compared case-insensitively byte-wise.
/// Non-ASCII characters compare by their UTF-8 byte order, which is
/// consistent (if not linguistically "correct") for Japanese.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < ab.len() && j < bb.len() {
        let (ac, bc) = (ab[i], bb[j]);
        if ac.is_ascii_digit() && bc.is_ascii_digit() {
            // Walk both numeric runs.
            let a_start = i;
            while i < ab.len() && ab[i].is_ascii_digit() {
                i += 1;
            }
            let b_start = j;
            while j < bb.len() && bb[j].is_ascii_digit() {
                j += 1;
            }
            let a_num = strip_leading_zeros(&ab[a_start..i]);
            let b_num = strip_leading_zeros(&bb[b_start..j]);
            // After stripping zeros, longer number = bigger magnitude.
            match a_num.len().cmp(&b_num.len()) {
                Ordering::Equal => match a_num.cmp(b_num) {
                    Ordering::Equal => continue,
                    ord => return ord,
                },
                ord => return ord,
            }
        } else {
            match ac.to_ascii_lowercase().cmp(&bc.to_ascii_lowercase()) {
                Ordering::Equal => {
                    i += 1;
                    j += 1;
                }
                ord => return ord,
            }
        }
    }
    ab.len().cmp(&bb.len())
}

fn strip_leading_zeros(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|&c| c != b'0').unwrap_or(s.len());
    &s[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_sort_pages() {
        let mut v = vec!["page10.jpg", "page2.jpg", "page1.jpg"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["page1.jpg", "page2.jpg", "page10.jpg"]);
    }

    #[test]
    fn natural_sort_leading_zeros() {
        // Leading zeros should not affect numeric ordering: 02 == 2.
        // After stripping zeros, equal-magnitude numbers fall back to
        // continuing past the run, so "page02.jpg" and "page2.jpg"
        // resolve by the rest of the string (here, identical → equal).
        let mut v = vec!["page002.jpg", "page1.jpg", "page03.jpg"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["page1.jpg", "page002.jpg", "page03.jpg"]);
    }

    #[test]
    fn natural_sort_case_insensitive() {
        let mut v = vec!["B.jpg", "a.jpg", "C.jpg"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["a.jpg", "B.jpg", "C.jpg"]);
    }

    #[test]
    fn natural_sort_mixed_text_and_numbers() {
        let mut v = vec![
            "ch10_page2.jpg",
            "ch2_page10.jpg",
            "ch10_page1.jpg",
            "ch2_page2.jpg",
        ];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            v,
            vec![
                "ch2_page2.jpg",
                "ch2_page10.jpg",
                "ch10_page1.jpg",
                "ch10_page2.jpg",
            ]
        );
    }

    #[test]
    fn natural_sort_equal_strings() {
        assert_eq!(natural_cmp("page01.jpg", "page01.jpg"), Ordering::Equal);
    }

    #[test]
    fn natural_sort_one_is_prefix() {
        // Shorter string compares less when it is a prefix of the other.
        assert_eq!(natural_cmp("page1", "page1.jpg"), Ordering::Less);
    }

    #[test]
    fn strip_leading_zeros_basic() {
        assert_eq!(strip_leading_zeros(b"0042"), b"42");
        assert_eq!(strip_leading_zeros(b"42"), b"42");
        assert_eq!(strip_leading_zeros(b"0000"), b"");
        assert_eq!(strip_leading_zeros(b""), b"");
    }

    #[test]
    fn cover_name_detection() {
        assert!(is_cover_name("cover.jpg"));
        assert!(is_cover_name("Cover.PNG"));
        assert!(is_cover_name("comic/cover.webp"));
        assert!(is_cover_name("folder.jpg"));
        assert!(is_cover_name("thumbnail.png"));
        assert!(!is_cover_name("page01.jpg"));
        assert!(!is_cover_name("recover.jpg"));
    }

    #[test]
    fn cover_name_handles_both_separators() {
        // Archive entries can use either / or \ depending on origin.
        assert!(is_cover_name("a/b/cover.jpg"));
        assert!(is_cover_name("a\\b\\cover.jpg"));
        assert!(is_cover_name("a/b\\cover.jpg"));
    }

    #[test]
    fn cover_name_no_extension() {
        assert!(is_cover_name("cover"));
        assert!(is_cover_name("FOLDER"));
        assert!(!is_cover_name("page1"));
    }

    #[test]
    fn cover_name_all_aliases() {
        for stem in &["cover", "folder", "thumb", "thumbnail", "front"] {
            assert!(is_cover_name(&format!("{stem}.jpg")), "stem={stem}");
            assert!(
                is_cover_name(&format!("{}.jpg", stem.to_uppercase())),
                "uppercase stem={stem}"
            );
        }
    }

    #[test]
    fn cover_wins_over_sort() {
        let names = vec![
            "aaa.jpg".to_string(),
            "cover.jpg".to_string(),
            "zzz.jpg".to_string(),
        ];
        // With default settings (cover priority on), cover wins.
        assert_eq!(pick_first_image(names), Some("cover.jpg".to_string()));
    }

    #[test]
    fn pick_first_image_empty() {
        assert_eq!(pick_first_image(vec![]), None);
    }

    #[test]
    fn sort_order_parse_aliases() {
        assert_eq!(
            SortOrder::from_registry_value("alphabetical"),
            Some(SortOrder::Alphabetical)
        );
        assert_eq!(
            SortOrder::from_registry_value("ALPHA"),
            Some(SortOrder::Alphabetical)
        );
        assert_eq!(
            SortOrder::from_registry_value("Natural"),
            Some(SortOrder::Natural)
        );
        assert_eq!(
            SortOrder::from_registry_value("NAT"),
            Some(SortOrder::Natural)
        );
        assert_eq!(SortOrder::from_registry_value("garbage"), None);
        assert_eq!(SortOrder::from_registry_value(""), None);
    }

    #[test]
    fn sort_order_registry_value_roundtrip() {
        for order in [SortOrder::Alphabetical, SortOrder::Natural] {
            let s = order.as_registry_value();
            assert_eq!(SortOrder::from_registry_value(s), Some(order));
        }
    }

    #[test]
    fn settings_default_matches_documented_behaviour() {
        // The defaults are user-visible (they kick in when the registry
        // key is missing) so a regression here would silently change
        // every fresh install.
        let s = Settings::default();
        assert_eq!(s.sort_order, SortOrder::Natural);
        assert!(s.prefer_cover_names);
    }
}
