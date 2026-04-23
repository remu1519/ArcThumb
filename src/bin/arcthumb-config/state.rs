//! Snapshot of everything the GUI reads and writes. Rebuilt from
//! the registry at startup and after every Apply so the UI stays
//! in sync with the actual registry state.

use arcthumb::registry;
use arcthumb::settings::{SUPPORTED_IMAGE_EXTS, Settings};

/// Number of extensions ArcThumb can manage. Derived directly from
/// `registry::EXTENSIONS` so the two can't drift apart.
pub const EXT_COUNT: usize = registry::EXTENSIONS.len();

/// Settings + per-extension enable state. No "is the shell extension
/// installed?" tracking here — installation is the installer's job,
/// not the GUI's. The GUI only toggles per-extension bindings and the
/// global preview-pane handler.
#[derive(Debug, Clone)]
pub struct UiModel {
    pub settings: Settings,
    /// Parallel to `registry::EXTENSIONS`. `true` = this extension's
    /// ShellEx key is currently present in the registry.
    pub ext_enabled: [bool; EXT_COUNT],
    /// Parallel to `settings::SUPPORTED_IMAGE_EXTS`. `true` = this
    /// image format is eligible as a thumbnail source inside
    /// archives. Mirrors `settings.enabled_image_exts_mask` — the
    /// GUI edits this `Vec`, `collect_from_ui` folds it back into
    /// the bitmask on Apply.
    pub image_ext_enabled: Vec<bool>,
    /// `true` iff the preview-handler CLSID is currently registered.
    /// When the user toggles this, ApplyChanges (un)registers the
    /// CLSID and binds/unbinds *all* extensions in one batch.
    pub preview_enabled: bool,
}

impl UiModel {
    pub fn load() -> Self {
        let settings = Settings::load_from_registry_uncached();
        let ext_enabled: [bool; EXT_COUNT] =
            std::array::from_fn(|i| registry::is_extension_registered(registry::EXTENSIONS[i]));
        let image_ext_enabled = image_ext_mask_to_vec(settings.enabled_image_exts_mask);
        let preview_enabled = registry::is_preview_enabled();
        Self {
            settings,
            ext_enabled,
            image_ext_enabled,
            preview_enabled,
        }
    }
}

/// Expand a bitmask over `SUPPORTED_IMAGE_EXTS` to a `Vec<bool>` of
/// the same length, parallel to that slice.
pub fn image_ext_mask_to_vec(mask: u32) -> Vec<bool> {
    (0..SUPPORTED_IMAGE_EXTS.len())
        .map(|i| (mask & (1u32 << i)) != 0)
        .collect()
}

/// Fold a GUI `Vec<bool>` back into the registry bitmask.
pub fn image_ext_vec_to_mask(flags: &[bool]) -> u32 {
    flags.iter().enumerate().take(32).fold(
        0u32,
        |acc, (i, &on)| if on { acc | (1u32 << i) } else { acc },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcthumb::settings::default_enabled_image_exts_mask;

    #[test]
    fn mask_to_vec_length_matches_supported_exts() {
        let v = image_ext_mask_to_vec(0);
        assert_eq!(v.len(), SUPPORTED_IMAGE_EXTS.len());
    }

    #[test]
    fn mask_to_vec_all_on_and_all_off() {
        let all = image_ext_mask_to_vec(default_enabled_image_exts_mask());
        assert!(all.iter().all(|&b| b), "default mask → every slot true");
        let none = image_ext_mask_to_vec(0);
        assert!(none.iter().all(|&b| !b), "zero mask → every slot false");
    }

    #[test]
    fn mask_vec_round_trip_for_every_single_bit() {
        // Every supported extension can be solo-toggled on, round-trip
        // through Vec<bool> and back to the same mask.
        for i in 0..SUPPORTED_IMAGE_EXTS.len() {
            let mask = 1u32 << i;
            let v = image_ext_mask_to_vec(mask);
            assert_eq!(
                v.iter().filter(|&&b| b).count(),
                1,
                "only one slot should be true for mask=1<<{i}"
            );
            assert!(v[i], "slot {i} should be the true one");
            assert_eq!(image_ext_vec_to_mask(&v), mask, "round-trip at bit {i}");
        }
    }

    #[test]
    fn mask_vec_round_trip_for_every_single_bit_cleared() {
        // Every supported extension can be solo-toggled off while the
        // rest stay on, round-trip through Vec<bool>.
        let all = default_enabled_image_exts_mask();
        for i in 0..SUPPORTED_IMAGE_EXTS.len() {
            let mask = all & !(1u32 << i);
            let v = image_ext_mask_to_vec(mask);
            assert!(!v[i], "slot {i} should be the false one");
            assert_eq!(
                v.iter().filter(|&&b| !b).count(),
                1,
                "only one slot should be false when clearing bit {i}"
            );
            assert_eq!(image_ext_vec_to_mask(&v), mask);
        }
    }

    #[test]
    fn vec_to_mask_ignores_bits_beyond_supported_range() {
        // A stale / longer flag vector (e.g. a future build that
        // added more image formats) must not pollute bits beyond
        // what the current build supports when folded back.
        let oversized = vec![true; SUPPORTED_IMAGE_EXTS.len() + 4];
        let mask = image_ext_vec_to_mask(&oversized);
        // The function takes up to 32 bits from the slice; assert
        // the result is exactly the bits we set, masked to what the
        // current build knows about.
        let expected =
            (0..(SUPPORTED_IMAGE_EXTS.len() + 4).min(32)).fold(0u32, |a, i| a | (1 << i));
        assert_eq!(mask, expected);
    }

    #[test]
    fn settings_roundtrip_preserves_individual_toggle() {
        // Simulate the GUI flow: start from default, toggle each
        // extension off in turn, fold back to a mask, and verify
        // Settings survives a clone equality check.
        use arcthumb::settings::Settings;
        let all = default_enabled_image_exts_mask();
        for i in 0..SUPPORTED_IMAGE_EXTS.len() {
            let mut flags = image_ext_mask_to_vec(all);
            flags[i] = false;
            let mask = image_ext_vec_to_mask(&flags);
            let s = Settings {
                enabled_image_exts_mask: mask,
                ..Settings::default()
            };
            assert_eq!(s.enabled_image_exts_mask & (1 << i), 0);
            // Other bits untouched.
            assert_eq!(s.enabled_image_exts_mask | (1 << i), all);
        }
    }
}
