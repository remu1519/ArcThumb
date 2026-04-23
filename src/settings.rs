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
//!     SortOrder         REG_SZ    "natural" | "alphabetical"
//!     PreferCoverNames  REG_DWORD 0 | 1
//!     EnabledImageExts  REG_DWORD bitmask over SUPPORTED_IMAGE_EXTS
//! ```
//!
//! Users can tweak these by hand in `regedit` until a proper config
//! GUI (Phase 4f.2) exists.

use std::cmp::Ordering;
use std::sync::OnceLock;

use winreg::RegKey;
use winreg::enums::*;

/// How to order image files within an archive before picking the
/// "first" one for the thumbnail.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    /// Plain byte-wise sort. `page10.jpg` comes before `page2.jpg`.
    Alphabetical,
    /// Natural sort: runs of digits compared numerically so
    /// `page2.jpg` comes before `page10.jpg`. Default because
    /// page2 < page10 is what users expect for comic archives.
    #[default]
    Natural,
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

/// Image extensions ArcThumb can decode when extracted from an
/// archive. This is the fixed compile-time *supported set*; the
/// user-facing `Settings::enabled_image_exts_mask` picks a subset.
/// Order is load-bearing: bit `i` of the mask refers to index `i`
/// here, so never reorder or delete entries — append only.
pub const SUPPORTED_IMAGE_EXTS: &[&str] = &[
    ".jpg",
    ".jpeg",
    ".png",
    ".gif",
    ".bmp",
    ".tiff",
    ".tif",
    ".webp",
    ".ico",
    #[cfg(feature = "jxl")]
    ".jxl",
];

/// All supported extensions enabled. Used as the factory default and
/// as the fallback when the registry key is missing or malformed.
pub const fn default_enabled_image_exts_mask() -> u32 {
    let n = SUPPORTED_IMAGE_EXTS.len();
    if n >= 32 { u32::MAX } else { (1u32 << n) - 1 }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub sort_order: SortOrder,
    /// When true, files named `cover.*`, `folder.*`, `thumb.*`,
    /// `thumbnail.*`, or `front.*` are preferred over the first
    /// image by sort order.
    pub prefer_cover_names: bool,
    /// Bitmask over `SUPPORTED_IMAGE_EXTS`: bit `i` set = extension
    /// at index `i` is eligible as a thumbnail source inside
    /// archives. Only bits < `SUPPORTED_IMAGE_EXTS.len()` are
    /// meaningful; higher bits are ignored.
    pub enabled_image_exts_mask: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sort_order: SortOrder::Natural,
            prefer_cover_names: true,
            enabled_image_exts_mask: default_enabled_image_exts_mask(),
        }
    }
}

/// Registry subkey under `HKCU` where settings live in production.
const SETTINGS_SUBKEY: &str = "Software\\ArcThumb";

impl Settings {
    /// Read settings from `HKCU\Software\ArcThumb` without touching the
    /// process-wide cache. The config GUI uses this so each "Apply"
    /// round sees fresh registry state.
    pub fn load_from_registry_uncached() -> Self {
        Self::load_from_subkey(SETTINGS_SUBKEY)
    }

    /// Core load routine, parameterised by subkey so tests can
    /// round-trip through a throwaway path without stomping on the
    /// user's real settings.
    fn load_from_subkey(subkey: &str) -> Self {
        let mut out = Self::default();
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let Ok(key) = hkcu.open_subkey(subkey) else {
            return out;
        };

        if let Ok(s) = key.get_value::<String, _>("SortOrder")
            && let Some(order) = SortOrder::from_registry_value(&s)
        {
            out.sort_order = order;
        }
        if let Ok(v) = key.get_value::<u32, _>("PreferCoverNames") {
            out.prefer_cover_names = v != 0;
        }
        if let Ok(v) = key.get_value::<u32, _>("EnabledImageExts") {
            // Mask unused high bits so a stale value from a build
            // with more supported formats can't light up phantom
            // entries after a downgrade.
            out.enabled_image_exts_mask = v & default_enabled_image_exts_mask();
        }
        out
    }

    /// Write every setting to `HKCU\Software\ArcThumb`. Creates the
    /// key if missing. Leaves other values (e.g. `Language`) untouched.
    pub fn save_to_registry(&self) -> std::io::Result<()> {
        self.save_to_subkey(SETTINGS_SUBKEY)
    }

    /// Is `name` a candidate image under the current settings?
    /// Combines the compile-time supported set with the user's
    /// `enabled_image_exts_mask`. Case-insensitive.
    pub fn accepts_image_ext(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        SUPPORTED_IMAGE_EXTS.iter().enumerate().any(|(i, ext)| {
            (self.enabled_image_exts_mask & (1u32 << i)) != 0 && lower.ends_with(ext)
        })
    }

    /// Pick the "best" image name from a list of candidates according
    /// to this settings snapshot. Applies `sort_order` and
    /// `prefer_cover_names`.
    pub fn pick_first_image(&self, mut names: Vec<String>) -> Option<String> {
        if names.is_empty() {
            return None;
        }
        match self.sort_order {
            SortOrder::Alphabetical => names.sort(),
            SortOrder::Natural => names.sort_by(|a, b| natural_cmp(a, b)),
        }
        if self.prefer_cover_names
            && let Some(cover) = names.iter().find(|n| is_cover_name(n))
        {
            return Some(cover.clone());
        }
        names.into_iter().next()
    }

    fn save_to_subkey(&self, subkey: &str) -> std::io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu.create_subkey(subkey)?;
        key.set_value("SortOrder", &self.sort_order.as_registry_value())?;
        let flag: u32 = if self.prefer_cover_names { 1 } else { 0 };
        key.set_value("PreferCoverNames", &flag)?;
        let mask = self.enabled_image_exts_mask & default_enabled_image_exts_mask();
        key.set_value("EnabledImageExts", &mask)?;
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
// Image selection helpers (used by Settings::pick_first_image)
// =============================================================================

/// Is this path a well-known cover-image filename? Checks the
/// basename (without extension) against a small allowlist.
fn is_cover_name(path: &str) -> bool {
    // Take whatever's after the last `/` or `\` — archive formats
    // use both depending on origin.
    let basename = path.rsplit(['/', '\\']).next().unwrap_or(path);
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
        assert_eq!(
            Settings::default().pick_first_image(names),
            Some("cover.jpg".to_string())
        );
    }

    #[test]
    fn pick_first_image_empty() {
        assert_eq!(Settings::default().pick_first_image(vec![]), None);
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
        assert_eq!(
            s.enabled_image_exts_mask,
            default_enabled_image_exts_mask(),
            "default must enable every supported image extension"
        );
    }

    /// RAII helper that picks a unique throwaway subkey under
    /// `HKCU\Software\ArcThumb_test\...` and deletes it on drop so
    /// parallel tests don't stomp on each other or leak state.
    struct ScratchSubkey(String);

    impl ScratchSubkey {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let path = format!("Software\\ArcThumb_test\\{tag}_{pid}_{n}");
            // Ensure a clean slate.
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let _ = hkcu.delete_subkey_all(&path);
            Self(path)
        }
        fn path(&self) -> &str {
            &self.0
        }
    }

    impl Drop for ScratchSubkey {
        fn drop(&mut self) {
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let _ = hkcu.delete_subkey_all(&self.0);
        }
    }

    #[test]
    fn settings_registry_round_trip_preserves_all_fields() {
        let scratch = ScratchSubkey::new("roundtrip");
        let original = Settings {
            sort_order: SortOrder::Alphabetical,
            prefer_cover_names: false,
            enabled_image_exts_mask: 0b1010_1010,
        };
        original
            .save_to_subkey(scratch.path())
            .expect("save to scratch subkey");
        let loaded = Settings::load_from_subkey(scratch.path());
        // The mask is ANDed with the default on save, so compare
        // against the AND-ed form.
        let expected_mask = 0b1010_1010 & default_enabled_image_exts_mask();
        assert_eq!(loaded.sort_order, SortOrder::Alphabetical);
        assert!(!loaded.prefer_cover_names);
        assert_eq!(loaded.enabled_image_exts_mask, expected_mask);
    }

    #[test]
    fn settings_load_missing_subkey_returns_defaults() {
        let scratch = ScratchSubkey::new("missing");
        // Scratch doesn't exist (never saved); loading should yield
        // defaults rather than panicking or returning junk.
        let loaded = Settings::load_from_subkey(scratch.path());
        assert_eq!(loaded, Settings::default());
    }

    #[test]
    fn settings_load_masks_out_of_range_high_bits() {
        // Simulate a future build that set bits beyond our supported
        // set. Those must be silently cleared on load so downgrades
        // don't enable phantom extensions.
        let scratch = ScratchSubkey::new("highbits");
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu.create_subkey(scratch.path()).unwrap();
        let stale: u32 = 0xFFFF_FFFF;
        key.set_value("EnabledImageExts", &stale).unwrap();

        let loaded = Settings::load_from_subkey(scratch.path());
        assert_eq!(
            loaded.enabled_image_exts_mask,
            default_enabled_image_exts_mask(),
            "high bits beyond the supported range must be cleared"
        );
    }

    #[test]
    fn settings_round_trip_every_single_image_ext_toggle() {
        // End-to-end: for every supported image extension, flip just
        // that extension's bit off, round-trip through the registry,
        // and verify the remaining bits survived intact.
        let all = default_enabled_image_exts_mask();
        for i in 0..SUPPORTED_IMAGE_EXTS.len() {
            let scratch = ScratchSubkey::new(&format!("bit{i}"));
            let original = Settings {
                enabled_image_exts_mask: all & !(1u32 << i),
                ..Settings::default()
            };
            original.save_to_subkey(scratch.path()).unwrap();
            let loaded = Settings::load_from_subkey(scratch.path());
            assert_eq!(
                loaded.enabled_image_exts_mask, original.enabled_image_exts_mask,
                "bit {i} round-trip ({})",
                SUPPORTED_IMAGE_EXTS[i]
            );
        }
    }

    #[test]
    fn default_image_mask_covers_exactly_supported_length() {
        let n = SUPPORTED_IMAGE_EXTS.len();
        let expected = if n >= 32 { u32::MAX } else { (1u32 << n) - 1 };
        assert_eq!(default_enabled_image_exts_mask(), expected);
        // Every bit below `n` is set, and every bit at or above `n`
        // is cleared. The upper-bound check guards against future
        // silent overflow if the supported list grows past 32.
        for i in 0..n {
            assert!(
                default_enabled_image_exts_mask() & (1u32 << i) != 0,
                "bit {i} should be set in the default mask"
            );
        }
        for i in n..32 {
            assert_eq!(
                default_enabled_image_exts_mask() & (1u32 << i),
                0,
                "bit {i} should be clear (beyond supported length)"
            );
        }
    }
}
