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
}

#[derive(Debug, Clone, Copy)]
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
    fn load_from_registry() -> Self {
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
}

/// Process-wide cached settings. Loaded lazily on first use and
/// held for the lifetime of the Explorer process. Restart Explorer
/// to pick up registry edits.
pub fn current() -> &'static Settings {
    static CACHE: OnceLock<Settings> = OnceLock::new();
    CACHE.get_or_init(Settings::load_from_registry)
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
    fn cover_wins_over_sort() {
        let names = vec![
            "aaa.jpg".to_string(),
            "cover.jpg".to_string(),
            "zzz.jpg".to_string(),
        ];
        // With default settings (cover priority on), cover wins.
        assert_eq!(pick_first_image(names), Some("cover.jpg".to_string()));
    }
}
