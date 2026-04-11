//! Slint `VecModel` wrapper for the per-extension toggle list.
//!
//! Owns the canonical row data for the container extensions the
//! Settings dialog can toggle on or off, and bridges it to the
//! `ExtensionEntry` Slint struct that `MainWindow` iterates over.
//!
//! ## Why a wrapper
//!
//! Before Phase 1 of the refactor, the 12 supported extensions were
//! hand-listed in four places: `state::EXT_COUNT`, `ui::push_model`,
//! `ui::collect_from_ui`, and `ui/main.slint`. Adding `.tar.gz`
//! meant editing all four. With this wrapper, the GUI list is built
//! from `arcthumb::registry::EXTENSIONS` at runtime — the registry
//! layer becomes the single source of truth and adding a new
//! extension is a one-line change there.
//!
//! ## Test note
//!
//! These tests intentionally do **not** call
//! `i_slint_backend_testing::init_no_event_loop`. The Slint runtime
//! `VecModel` and `ExtensionEntry` types are plain Rust data with
//! no platform dependency, so they work in any test thread without
//! the testing backend. Tests that touch `MainWindow` (which
//! requires the platform) live in `ui::tests` instead.

use std::rc::Rc;

use arcthumb::registry;
use slint::{Model, ModelRc, SharedString, VecModel};

use crate::ui::ExtensionEntry;

/// Owns the row data for the per-extension toggle list. Cheap to
/// clone — the underlying `VecModel` is held behind an `Rc`.
#[derive(Clone)]
pub struct ExtensionModel {
    inner: Rc<VecModel<ExtensionEntry>>,
}

impl ExtensionModel {
    /// Build a fresh model from a bool slice. The order matches
    /// `arcthumb::registry::EXTENSIONS`, so `enabled[i]` is whether
    /// the extension at `EXTENSIONS[i]` should start checked.
    ///
    /// Panics in debug builds if `enabled.len() != EXTENSIONS.len()`
    /// — the two are kept in lockstep by `state::EXT_COUNT` and the
    /// existing `debug_assert_eq!` in `UiModel::load`.
    pub fn from_enabled(enabled: &[bool]) -> Self {
        debug_assert_eq!(
            enabled.len(),
            registry::EXTENSIONS.len(),
            "enabled[] length must match registry::EXTENSIONS"
        );
        let entries: Vec<ExtensionEntry> = registry::EXTENSIONS
            .iter()
            .zip(enabled.iter())
            .map(|(&name, &on)| ExtensionEntry {
                name: SharedString::from(name),
                enabled: on,
            })
            .collect();
        Self {
            inner: Rc::new(VecModel::from(entries)),
        }
    }

    /// Flip the enabled flag at `index`. Called from the Slint
    /// `toggle_extension` callback when the user clicks a checkbox.
    /// Out-of-range indices are ignored — the callback comes from
    /// the UI so a stale index after a model rebuild would otherwise
    /// crash the app.
    pub fn toggle(&self, index: usize) {
        let Some(mut entry) = self.inner.row_data(index) else {
            return;
        };
        entry.enabled = !entry.enabled;
        self.inner.set_row_data(index, entry);
    }

    /// Replace every row's `enabled` flag with the values from
    /// `enabled`. Names are not touched. Used by `apply_changes`
    /// after a successful Apply to refresh the UI from the freshly
    /// reloaded `UiModel` without throwing the model away (which
    /// would force `MainWindow::set_extensions` to be called again
    /// on every Apply).
    pub fn replace_enabled(&self, enabled: &[bool]) {
        debug_assert_eq!(
            enabled.len(),
            self.inner.row_count(),
            "replace_enabled length mismatch"
        );
        for (i, &on) in enabled.iter().enumerate() {
            if let Some(mut entry) = self.inner.row_data(i)
                && entry.enabled != on
            {
                entry.enabled = on;
                self.inner.set_row_data(i, entry);
            }
        }
    }

    /// Snapshot the current enabled flags as a fixed-size array.
    /// Used by `collect_from_ui` to feed `apply::compute_apply_plan`.
    /// Returns the array padded with `false` if the row count is
    /// shorter than `N`, or truncated if longer — neither case is
    /// expected at runtime, but a debug assert catches it.
    pub fn enabled_array<const N: usize>(&self) -> [bool; N] {
        debug_assert_eq!(
            self.inner.row_count(),
            N,
            "enabled_array<N>: row count mismatch"
        );
        let mut out = [false; N];
        for (i, slot) in out.iter_mut().enumerate() {
            if let Some(entry) = self.inner.row_data(i) {
                *slot = entry.enabled;
            }
        }
        out
    }

    /// Hand the underlying model to Slint via
    /// `MainWindow::set_extensions`.
    pub fn as_model(&self) -> ModelRc<ExtensionEntry> {
        ModelRc::from(self.inner.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline_enabled() -> Vec<bool> {
        // 12 entries (matching registry::EXTENSIONS), with a
        // recognisable pattern so flipped indices are detectable.
        let mut v = vec![false; registry::EXTENSIONS.len()];
        v[0] = true; // .zip
        v[2] = true; // .rar
        v[7] = true; // .epub
        v
    }

    #[test]
    fn from_enabled_creates_one_entry_per_registry_extension() {
        let model = ExtensionModel::from_enabled(&baseline_enabled());
        assert_eq!(model.inner.row_count(), registry::EXTENSIONS.len());
    }

    #[test]
    fn from_enabled_preserves_names_in_registry_order() {
        let model = ExtensionModel::from_enabled(&baseline_enabled());
        for (i, expected) in registry::EXTENSIONS.iter().enumerate() {
            let entry = model.inner.row_data(i).expect("row exists");
            assert_eq!(
                entry.name.as_str(),
                *expected,
                "name mismatch at index {i}"
            );
        }
    }

    #[test]
    fn from_enabled_preserves_enabled_flags() {
        let enabled = baseline_enabled();
        let model = ExtensionModel::from_enabled(&enabled);
        for (i, &expected) in enabled.iter().enumerate() {
            let entry = model.inner.row_data(i).expect("row exists");
            assert_eq!(entry.enabled, expected, "enabled mismatch at index {i}");
        }
    }

    #[test]
    fn toggle_flips_enabled_at_index() {
        let model = ExtensionModel::from_enabled(&vec![false; registry::EXTENSIONS.len()]);
        model.toggle(5);
        assert!(model.inner.row_data(5).unwrap().enabled);
        model.toggle(5);
        assert!(!model.inner.row_data(5).unwrap().enabled);
    }

    #[test]
    fn toggle_does_not_affect_other_indices() {
        let model = ExtensionModel::from_enabled(&vec![false; registry::EXTENSIONS.len()]);
        model.toggle(3);
        for i in 0..registry::EXTENSIONS.len() {
            let on = model.inner.row_data(i).unwrap().enabled;
            assert_eq!(on, i == 3, "index {i} unexpected");
        }
    }

    #[test]
    fn toggle_out_of_range_is_a_noop() {
        // Stale indices from the UI should not panic. The model
        // simply ignores rows that no longer exist.
        let model = ExtensionModel::from_enabled(&vec![false; registry::EXTENSIONS.len()]);
        model.toggle(999);
        // Nothing changed.
        for i in 0..registry::EXTENSIONS.len() {
            assert!(!model.inner.row_data(i).unwrap().enabled);
        }
    }

    #[test]
    fn enabled_array_round_trips_through_from_enabled() {
        const N: usize = 12;
        let original = baseline_enabled();
        let model = ExtensionModel::from_enabled(&original);
        let snapshot: [bool; N] = model.enabled_array::<N>();
        for (i, &expected) in original.iter().enumerate() {
            assert_eq!(snapshot[i], expected, "round-trip mismatch at index {i}");
        }
    }

    #[test]
    fn replace_enabled_updates_only_changed_rows() {
        let model = ExtensionModel::from_enabled(&baseline_enabled());
        let mut new_enabled = baseline_enabled();
        new_enabled[1] = true; // .cbz
        new_enabled[7] = false; // .epub
        model.replace_enabled(&new_enabled);

        let snapshot = model.enabled_array::<12>();
        assert!(snapshot[0]); // .zip still on
        assert!(snapshot[1]); // .cbz newly on
        assert!(snapshot[2]); // .rar still on
        assert!(!snapshot[7]); // .epub newly off
        // Spot-check the rest stayed off.
        assert!(!snapshot[3]);
        assert!(!snapshot[11]);
    }
}
