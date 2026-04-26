//! The ArcThumb settings dialog, built on Slint.
//!
//! The layout and widget tree live in `ui/main.slint`. This module
//! wires the Slint window to the rest of the binary: loads the
//! initial model from the registry, pushes it into the Slint
//! properties, hooks up the menu and button callbacks, and
//! delegates sub-dialogs (About / Update / Donation) to the
//! `dialogs` module and background update polling to
//! `update_check`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use arcthumb::settings::{SUPPORTED_IMAGE_EXTS, Settings, SortOrder};
use slint::{ComponentHandle, SharedString, Timer};

use crate::apply::{self, RealRegistryOps};
use crate::cache;
use crate::dialogs;
use crate::extension_model::ExtensionModel;
use crate::locale::{self, Strings};
use crate::message_box;
use crate::state::{self, EXT_COUNT, UiModel};
use crate::update;
use crate::update_check;

slint::include_modules!();

// `slint::include_modules!()` emits `pub` types directly into this
// module, so sibling modules access them as `crate::ui::ExtensionEntry`
// (and `crate::ui::AboutDialog` etc.) without an explicit re-export.

/// Launch the settings GUI. Blocks on the Slint event loop.
pub fn run_gui() -> Result<(), slint::PlatformError> {
    let strings: &'static Strings = locale::current();
    let window = MainWindow::new()?;
    apply_strings(&window, strings);

    let initial_model = UiModel::load();
    let lists = ExtensionLists::from_model(&initial_model);
    lists.bind(&window);
    push_model(&window, &initial_model);
    let state = Rc::new(RefCell::new(initial_model));

    // OK
    {
        let weak = window.as_weak();
        let state = Rc::clone(&state);
        let lists = lists.clone();
        window.on_ok_clicked(move || {
            if let Some(w) = weak.upgrade()
                && apply_changes(&w, &state, &lists, strings)
            {
                let _ = w.hide();
            }
        });
    }

    // Apply
    {
        let weak = window.as_weak();
        let state = Rc::clone(&state);
        let lists = lists.clone();
        window.on_apply_clicked(move || {
            if let Some(w) = weak.upgrade() {
                let _ = apply_changes(&w, &state, &lists, strings);
            }
        });
    }

    // Cancel
    {
        let weak = window.as_weak();
        window.on_cancel_clicked(move || {
            if let Some(w) = weak.upgrade() {
                let _ = w.hide();
            }
        });
    }

    // Regenerate
    window.on_regenerate_clicked(move || {
        handle_regenerate(strings);
    });

    // Help → About
    window.on_about_clicked(move || {
        dialogs::show_about(strings);
    });

    // File → Exit
    {
        let weak = window.as_weak();
        window.on_exit_clicked(move || {
            if let Some(w) = weak.upgrade() {
                let _ = w.hide();
            }
        });
    }

    // Donation prompt — fires once after the event loop starts so we
    // can show a Slint window (which only paints while the loop is
    // running). `Timer::single_shot` is the associated form that
    // self-manages: do NOT use `Timer::default().start(...)` here,
    // because that returns an owned `Timer` whose `Drop` cancels the
    // timer immediately when the value goes out of scope.
    if let Some(ver) = update::should_show_donation() {
        Timer::single_shot(Duration::ZERO, move || {
            dialogs::show_donation_dialog(&ver, strings);
            update::record_donation_shown();
        });
    }

    // Background update check — non-blocking. The result is marshalled
    // back onto the UI thread via `slint::invoke_from_event_loop`.
    update_check::start_update_check(strings);

    window.run()?;
    Ok(())
}

// =============================================================================
// Extension-list bundle
// =============================================================================

/// Both toggle lists ArcThumb exposes in the GUI: the per-archive
/// shell registration list and the per-image-format thumbnail
/// eligibility list. Bundled so `run_gui` doesn't have to clone and
/// pass two `ExtensionModel`s through every callback.
#[derive(Clone)]
struct ExtensionLists {
    archive: ExtensionModel,
    image: ExtensionModel,
}

impl ExtensionLists {
    fn from_model(m: &UiModel) -> Self {
        Self {
            archive: ExtensionModel::from_enabled(&m.ext_enabled),
            image: ExtensionModel::from_names_and_enabled(
                SUPPORTED_IMAGE_EXTS,
                &m.image_ext_enabled,
            ),
        }
    }

    fn bind(&self, window: &MainWindow) {
        window.set_extensions(self.archive.as_model());
        window.set_image_extensions(self.image.as_model());
        let archive = self.archive.clone();
        window.on_toggle_extension(move |i| archive.toggle(i as usize));
        let image = self.image.clone();
        window.on_toggle_image_extension(move |i| image.toggle(i as usize));
    }

    fn refresh_from(&self, m: &UiModel) {
        self.archive.replace_enabled(&m.ext_enabled);
        self.image.replace_enabled(&m.image_ext_enabled);
    }
}

// =============================================================================
// Model ⇄ Slint properties
// =============================================================================

fn apply_strings(window: &MainWindow, s: &Strings) {
    window.set_window_title(SharedString::from(s.window_title));
    window.set_menu_file(SharedString::from(s.menu_file));
    window.set_menu_file_exit(SharedString::from(s.menu_file_exit));
    window.set_menu_help(SharedString::from(s.menu_help));
    window.set_menu_help_about(SharedString::from(s.menu_help_about));
    window.set_group_extensions(SharedString::from(s.group_extensions));
    window.set_group_image_exts(SharedString::from(s.group_image_exts));
    window.set_group_sort(SharedString::from(s.group_sort));
    window.set_sort_natural_label(SharedString::from(s.sort_natural));
    window.set_sort_alpha_label(SharedString::from(s.sort_alphabetical));
    window.set_prefer_cover_label(SharedString::from(s.cb_prefer_cover));
    window.set_enable_preview_label(SharedString::from(s.cb_enable_preview));
    window.set_btn_ok(SharedString::from(s.btn_ok));
    window.set_btn_cancel(SharedString::from(s.btn_cancel));
    window.set_btn_apply(SharedString::from(s.btn_apply));
    window.set_btn_regenerate(SharedString::from(s.btn_regenerate));
}

/// Push the non-extension parts of `model` into the Slint window.
/// Extensions are handled separately by `ExtensionModel::replace_enabled`
/// because they live in a `VecModel` rather than scalar properties.
fn push_model(window: &MainWindow, model: &UiModel) {
    window.set_sort_natural(matches!(model.settings.sort_order, SortOrder::Natural));
    window.set_prefer_cover(model.settings.prefer_cover_names);
    window.set_enable_preview(model.preview_enabled);
}

fn collect_from_ui(
    window: &MainWindow,
    lists: &ExtensionLists,
) -> (Settings, [bool; EXT_COUNT], bool) {
    let ext_enabled = lists.archive.enabled_array::<EXT_COUNT>();
    let sort_order = if window.get_sort_natural() {
        SortOrder::Natural
    } else {
        SortOrder::Alphabetical
    };
    let image_mask = state::image_ext_vec_to_mask(&lists.image.enabled_vec());
    let settings = Settings {
        sort_order,
        prefer_cover_names: window.get_prefer_cover(),
        enabled_image_exts_mask: image_mask,
    };
    (settings, ext_enabled, window.get_enable_preview())
}

// =============================================================================
// Apply
// =============================================================================

fn apply_changes(
    window: &MainWindow,
    state: &Rc<RefCell<UiModel>>,
    lists: &ExtensionLists,
    strings: &Strings,
) -> bool {
    let (new_settings, new_ext_enabled, new_preview_enabled) = collect_from_ui(window, lists);

    let plan = apply::compute_apply_plan(
        &state.borrow(),
        new_settings,
        new_ext_enabled,
        new_preview_enabled,
    );
    // Mutate whichever hive the loaded model came from, so an Apply
    // on a per-machine install doesn't silently bifurcate into HKCU.
    let outcome = apply::apply_plan(&plan, &RealRegistryOps::new(state.borrow().scope));

    if let Some(detail) = &outcome.settings_save_error {
        message_box::error(
            strings.error_title,
            &format!("{}\n\n{detail}", strings.error_save),
        );
    }
    if !outcome.failed_extensions.is_empty() {
        message_box::error(
            strings.error_title,
            &format!(
                "{}\n\nfailed: {}",
                strings.error_register,
                outcome.failed_extensions.join(", ")
            ),
        );
    }
    if let Some(detail) = &outcome.preview_error {
        message_box::error(
            strings.error_title,
            &format!("{}\n\n{detail}", strings.error_register),
        );
    }

    let reloaded = UiModel::load();
    push_model(window, &reloaded);
    lists.refresh_from(&reloaded);
    *state.borrow_mut() = reloaded;

    outcome.is_ok()
}

// =============================================================================
// Regenerate thumbnails
// =============================================================================

fn handle_regenerate(strings: &Strings) {
    if !message_box::confirm_warning(strings.error_title, strings.regen_confirm) {
        return;
    }
    match cache::wipe_thumbnail_cache() {
        Ok(report) if report.failed.is_empty() => {
            message_box::info(strings.error_title, strings.regen_done);
        }
        Ok(_) => {
            message_box::error(strings.error_title, strings.regen_partial);
        }
        Err(e) => {
            message_box::error(
                strings.error_title,
                &format!("{}\n\n{e}", strings.regen_partial),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    //! Slint glue tests.
    //!
    //! All Slint-touching assertions live inside a single `#[test]`
    //! function on purpose: `i_slint_backend_testing::init_no_event_loop()`
    //! pins the test platform to the thread that first called it,
    //! and the cargo test harness runs tests on independent worker
    //! threads. Splitting these into multiple `#[test]`s causes the
    //! second-and-later tests to land on a different worker and
    //! crash with "The Slint platform was initialized in another
    //! thread".
    //!
    //! Each subsection is wrapped in its own block + comment so
    //! `cargo test` failure messages still pinpoint the failing
    //! assertion. The cost of bundling them is one test entry in
    //! the report; the benefit is reliable execution under the
    //! default parallel test runner.

    use super::*;
    use arcthumb::settings::SortOrder;

    fn baseline_model() -> UiModel {
        let mut ext = [false; EXT_COUNT];
        // A non-trivial subset so a flipped index is detectable.
        ext[0] = true; // .zip
        ext[2] = true; // .rar
        ext[7] = true; // .epub
        let settings = Settings::default();
        UiModel {
            image_ext_enabled: state::image_ext_mask_to_vec(settings.enabled_image_exts_mask),
            settings,
            scope: arcthumb::registry::Scope::PerUser,
            ext_enabled: ext,
            preview_enabled: true,
        }
    }

    #[test]
    fn slint_glue_round_trips_and_localises() {
        i_slint_backend_testing::init_no_event_loop();

        // ---- push_then_collect_round_trips_full_model -----------
        {
            let window = MainWindow::new().expect("create MainWindow");
            let original = baseline_model();
            let lists = ExtensionLists::from_model(&original);
            window.set_extensions(lists.archive.as_model());
            window.set_image_extensions(lists.image.as_model());

            push_model(&window, &original);
            let (settings, ext_enabled, preview) = collect_from_ui(&window, &lists);

            assert_eq!(settings, original.settings, "settings round-trip");
            assert_eq!(ext_enabled, original.ext_enabled, "ext_enabled round-trip");
            assert_eq!(preview, original.preview_enabled, "preview round-trip");
        }

        // ---- push_then_collect_round_trips_alphabetical_no_cover
        {
            let window = MainWindow::new().expect("create MainWindow");
            let settings = Settings {
                sort_order: SortOrder::Alphabetical,
                prefer_cover_names: false,
                ..Settings::default()
            };
            let model = UiModel {
                image_ext_enabled: state::image_ext_mask_to_vec(settings.enabled_image_exts_mask),
                settings,
                scope: arcthumb::registry::Scope::PerUser,
                ext_enabled: [true; EXT_COUNT],
                preview_enabled: false,
            };
            let lists = ExtensionLists::from_model(&model);
            window.set_extensions(lists.archive.as_model());
            window.set_image_extensions(lists.image.as_model());

            push_model(&window, &model);
            let (settings, ext_enabled, preview) = collect_from_ui(&window, &lists);

            assert_eq!(settings.sort_order, SortOrder::Alphabetical);
            assert!(!settings.prefer_cover_names);
            assert_eq!(ext_enabled, [true; EXT_COUNT]);
            assert!(!preview);
        }

        // ---- toggle_extension_callback_path_via_extension_model
        {
            let window = MainWindow::new().expect("create MainWindow");
            let model = UiModel {
                settings: Settings::default(),
                scope: arcthumb::registry::Scope::PerUser,
                ext_enabled: [false; EXT_COUNT],
                image_ext_enabled: state::image_ext_mask_to_vec(
                    Settings::default().enabled_image_exts_mask,
                ),
                preview_enabled: false,
            };
            let lists = ExtensionLists::from_model(&model);
            window.set_extensions(lists.archive.as_model());
            window.set_image_extensions(lists.image.as_model());
            push_model(&window, &model);
            lists.archive.toggle(5); // .cb7
            lists.archive.toggle(11); // .azw3

            let (_, ext, _) = collect_from_ui(&window, &lists);
            assert!(ext[5], ".cb7 should be on (index 5)");
            assert!(ext[11], ".azw3 should be on (index 11)");
            for (i, on) in ext.iter().enumerate() {
                if i != 5 && i != 11 {
                    assert!(!on, "index {i} should be off");
                }
            }
        }

        // ---- apply_strings_populates_every_label_for_english ---
        // Spot-check every property `apply_strings` writes. A
        // future regression that swaps two setters or drops one
        // would leave that property as the empty default.
        {
            let window = MainWindow::new().expect("create MainWindow");
            apply_strings(&window, &locale::EN);

            assert_eq!(window.get_window_title(), locale::EN.window_title);
            assert_eq!(window.get_menu_file(), locale::EN.menu_file);
            assert_eq!(window.get_menu_file_exit(), locale::EN.menu_file_exit);
            assert_eq!(window.get_menu_help(), locale::EN.menu_help);
            assert_eq!(window.get_menu_help_about(), locale::EN.menu_help_about);
            assert_eq!(window.get_group_extensions(), locale::EN.group_extensions);
            assert_eq!(window.get_group_sort(), locale::EN.group_sort);
            assert_eq!(window.get_sort_natural_label(), locale::EN.sort_natural);
            assert_eq!(window.get_sort_alpha_label(), locale::EN.sort_alphabetical);
            assert_eq!(window.get_prefer_cover_label(), locale::EN.cb_prefer_cover);
            assert_eq!(
                window.get_enable_preview_label(),
                locale::EN.cb_enable_preview
            );
            assert_eq!(window.get_btn_ok(), locale::EN.btn_ok);
            assert_eq!(window.get_btn_cancel(), locale::EN.btn_cancel);
            assert_eq!(window.get_btn_apply(), locale::EN.btn_apply);
            assert_eq!(window.get_btn_regenerate(), locale::EN.btn_regenerate);
        }

        // ---- apply_strings_populates_every_label_for_japanese --
        {
            let window = MainWindow::new().expect("create MainWindow");
            apply_strings(&window, &locale::JA);

            assert_eq!(window.get_window_title(), locale::JA.window_title);
            assert_eq!(window.get_menu_help_about(), locale::JA.menu_help_about);
            assert_eq!(window.get_group_extensions(), locale::JA.group_extensions);
            assert_eq!(window.get_btn_regenerate(), locale::JA.btn_regenerate);
            // Make sure the language actually switched.
            assert_ne!(window.get_window_title(), locale::EN.window_title);
        }

        // ---- ok_callback_with_no_changes_produces_empty_plan ---
        // The OK button calls compute_apply_plan against the
        // current `state`. With an unchanged model the plan must
        // be empty so we never touch HKCU on a stray click.
        {
            let window = MainWindow::new().expect("create MainWindow");
            let model = baseline_model();
            let lists = ExtensionLists::from_model(&model);
            window.set_extensions(lists.archive.as_model());
            window.set_image_extensions(lists.image.as_model());
            push_model(&window, &model);
            let (settings, ext_enabled, preview_enabled) = collect_from_ui(&window, &lists);
            let plan = apply::compute_apply_plan(&model, settings, ext_enabled, preview_enabled);
            assert!(
                plan.is_empty(),
                "round-trip with the same model should produce an empty plan, got {plan:?}"
            );
        }
    }
}
