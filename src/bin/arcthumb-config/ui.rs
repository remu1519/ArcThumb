//! The ArcThumb settings dialog, built on Slint.
//!
//! The layout and widget tree live in `ui/main.slint`. This module
//! wires the Slint window to the rest of the binary: loads the
//! initial model from the registry, pushes it into the Slint
//! properties, hooks up the button callbacks, and handles modal
//! prompts via native Win32 `MessageBoxW`.

use std::cell::RefCell;
use std::rc::Rc;

use arcthumb::registry;
use arcthumb::settings::{Settings, SortOrder};
use slint::{ComponentHandle, SharedString};

use crate::cache;
use crate::dialog::{self, DialogResult};
use crate::dll_path;
use crate::locale::{self, Strings};
use crate::state::{EXT_COUNT, UiModel};
use crate::update;

slint::include_modules!();

/// Launch the settings GUI. Blocks on the Slint event loop.
pub fn run_gui() -> Result<(), slint::PlatformError> {
    let strings: &'static Strings = locale::current();
    let window = MainWindow::new()?;
    apply_strings(&window, strings);

    let initial_model = UiModel::load();
    push_model(&window, &initial_model);
    let state = Rc::new(RefCell::new(initial_model));

    // Holder keeps the optional About dialog alive while the user is
    // looking at it. Cleared when the dialog is closed.
    let about_holder: Rc<RefCell<Option<AboutDialog>>> = Rc::new(RefCell::new(None));

    // OK: apply and close on success.
    {
        let weak = window.as_weak();
        let state = Rc::clone(&state);
        window.on_ok_clicked(move || {
            if let Some(w) = weak.upgrade()
                && apply_changes(&w, &state, strings)
            {
                let _ = w.hide();
            }
        });
    }

    // Apply: apply but keep the window open.
    {
        let weak = window.as_weak();
        let state = Rc::clone(&state);
        window.on_apply_clicked(move || {
            if let Some(w) = weak.upgrade() {
                let _ = apply_changes(&w, &state, strings);
            }
        });
    }

    // Cancel: hide without touching the registry.
    {
        let weak = window.as_weak();
        window.on_cancel_clicked(move || {
            if let Some(w) = weak.upgrade() {
                let _ = w.hide();
            }
        });
    }

    // Regenerate: wipe the Windows thumbnail cache after confirmation.
    window.on_regenerate_clicked(move || {
        handle_regenerate(strings);
    });

    // About: open the Slint attribution dialog.
    {
        let holder = Rc::clone(&about_holder);
        window.on_about_clicked(move || {
            show_about(strings, &holder);
        });
    }

    // Donation prompt — synchronous, runs before the event loop. Only
    // fires when the user has just upgraded to a newer version.
    if let Some(ver) = update::should_show_donation() {
        show_donation_dialog(&ver, strings);
        update::record_donation_shown();
    }

    // Background update check — non-blocking. The result is marshalled
    // back onto the UI thread via `slint::invoke_from_event_loop`.
    start_update_check(strings);

    window.run()?;
    Ok(())
}

// =============================================================================
// Model ⇄ Slint properties
// =============================================================================

fn apply_strings(window: &MainWindow, s: &Strings) {
    window.set_window_title(SharedString::from(s.window_title));
    window.set_group_extensions(SharedString::from(s.group_extensions));
    window.set_group_sort(SharedString::from(s.group_sort));
    window.set_sort_natural_label(SharedString::from(s.sort_natural));
    window.set_sort_alpha_label(SharedString::from(s.sort_alphabetical));
    window.set_prefer_cover_label(SharedString::from(s.cb_prefer_cover));
    window.set_enable_preview_label(SharedString::from(s.cb_enable_preview));
    window.set_btn_ok(SharedString::from(s.btn_ok));
    window.set_btn_cancel(SharedString::from(s.btn_cancel));
    window.set_btn_apply(SharedString::from(s.btn_apply));
    window.set_btn_regenerate(SharedString::from(s.btn_regenerate));
    window.set_btn_about(SharedString::from(s.btn_about));
}

fn push_model(window: &MainWindow, model: &UiModel) {
    window.set_ext_zip(model.ext_enabled[0]);
    window.set_ext_cbz(model.ext_enabled[1]);
    window.set_ext_rar(model.ext_enabled[2]);
    window.set_ext_cbr(model.ext_enabled[3]);
    window.set_ext_7z(model.ext_enabled[4]);
    window.set_ext_cb7(model.ext_enabled[5]);
    window.set_ext_cbt(model.ext_enabled[6]);
    window.set_ext_epub(model.ext_enabled[7]);
    window.set_ext_fb2(model.ext_enabled[8]);
    window.set_ext_mobi(model.ext_enabled[9]);
    window.set_ext_azw(model.ext_enabled[10]);
    window.set_ext_azw3(model.ext_enabled[11]);
    window.set_sort_natural(matches!(model.settings.sort_order, SortOrder::Natural));
    window.set_prefer_cover(model.settings.prefer_cover_names);
    window.set_enable_preview(model.preview_enabled);
}

fn collect_from_ui(window: &MainWindow) -> (Settings, [bool; EXT_COUNT], bool) {
    let ext_enabled = [
        window.get_ext_zip(),
        window.get_ext_cbz(),
        window.get_ext_rar(),
        window.get_ext_cbr(),
        window.get_ext_7z(),
        window.get_ext_cb7(),
        window.get_ext_cbt(),
        window.get_ext_epub(),
        window.get_ext_fb2(),
        window.get_ext_mobi(),
        window.get_ext_azw(),
        window.get_ext_azw3(),
    ];
    let sort_order = if window.get_sort_natural() {
        SortOrder::Natural
    } else {
        SortOrder::Alphabetical
    };
    let settings = Settings {
        sort_order,
        prefer_cover_names: window.get_prefer_cover(),
    };
    (settings, ext_enabled, window.get_enable_preview())
}

// =============================================================================
// Apply
// =============================================================================

fn apply_changes(window: &MainWindow, state: &Rc<RefCell<UiModel>>, strings: &Strings) -> bool {
    let (new_settings, new_ext_enabled, new_preview_enabled) = collect_from_ui(window);
    let mut ok = true;
    // Tracks whether anything in the registry actually changed. We
    // use this to decide whether to poke the Shell about its icon/
    // thumbnail cache at the end — there's no point nagging Explorer
    // when the user clicked Apply without changing anything that
    // affects shell registrations.
    let mut shell_state_changed = false;

    // --- Settings (sort order + prefer cover)
    let old_settings = state.borrow().settings;
    if new_settings != old_settings
        && let Err(e) = new_settings.save_to_registry()
    {
        dialog::error(
            strings.error_title,
            &format!("{}\n\n{e}", strings.error_save),
        );
        return false;
    }

    // --- Per-extension shell binding diff
    let old_ext = state.borrow().ext_enabled;
    let mut failures: Vec<&'static str> = Vec::new();
    for i in 0..EXT_COUNT {
        let ext = registry::EXTENSIONS[i];
        match (old_ext[i], new_ext_enabled[i]) {
            (false, true) => {
                if registry::register_extension(ext).is_err() {
                    failures.push(ext);
                    ok = false;
                } else {
                    shell_state_changed = true;
                }
            }
            (true, false) => {
                if registry::unregister_extension(ext).is_err() {
                    failures.push(ext);
                    ok = false;
                } else {
                    shell_state_changed = true;
                }
            }
            _ => {}
        }
    }
    if !failures.is_empty() {
        dialog::error(
            strings.error_title,
            &format!(
                "{}\n\nfailed: {}",
                strings.error_register,
                failures.join(", ")
            ),
        );
    }

    // --- Preview pane handler (global toggle)
    let old_preview = state.borrow().preview_enabled;
    if old_preview != new_preview_enabled {
        match apply_preview_toggle(new_preview_enabled) {
            Ok(()) => shell_state_changed = true,
            Err(e) => {
                dialog::error(
                    strings.error_title,
                    &format!("{}\n\n{e}", strings.error_register),
                );
                ok = false;
            }
        }
    }

    // Whenever we touched shell registrations, ask Explorer to
    // invalidate its icon/thumbnail cache so the change takes effect
    // immediately. Without this, newly enabled extensions would still
    // show the old "no thumbnail" cache entry until the user logs out
    // or wipes thumbcache_*.db by hand.
    if shell_state_changed {
        registry::notify_assoc_changed();
    }

    let reloaded = UiModel::load();
    push_model(window, &reloaded);
    *state.borrow_mut() = reloaded;

    ok
}

/// Register or unregister the preview-handler CLSID and bind/unbind
/// it across every supported extension. Called when the user flips
/// the "Enable preview pane" checkbox and clicks Apply.
fn apply_preview_toggle(enable: bool) -> std::io::Result<()> {
    if enable {
        let dll = dll_path::resolve_dll_path().map_err(std::io::Error::other)?;
        registry::register_preview_clsid(&dll)?;
        for ext in registry::EXTENSIONS {
            registry::register_preview_extension(ext)?;
        }
    } else {
        for ext in registry::EXTENSIONS {
            let _ = registry::unregister_preview_extension(ext);
        }
        let _ = registry::unregister_preview_clsid();
    }
    Ok(())
}

// =============================================================================
// Regenerate thumbnails
// =============================================================================

fn handle_regenerate(strings: &Strings) {
    if !dialog::confirm_warning(strings.error_title, strings.regen_confirm) {
        return;
    }
    match cache::wipe_thumbnail_cache() {
        Ok(report) if report.failed.is_empty() => {
            dialog::info(strings.error_title, strings.regen_done);
        }
        Ok(_) => {
            dialog::error(strings.error_title, strings.regen_partial);
        }
        Err(e) => {
            dialog::error(
                strings.error_title,
                &format!("{}\n\n{e}", strings.regen_partial),
            );
        }
    }
}

// =============================================================================
// About dialog — Slint window so we can embed `AboutSlint`.
// =============================================================================

fn show_about(strings: &Strings, holder: &Rc<RefCell<Option<AboutDialog>>>) {
    // If one is already open, bring focus to it rather than stacking.
    if holder.borrow().is_some() {
        return;
    }

    let dialog = match AboutDialog::new() {
        Ok(d) => d,
        Err(_) => return,
    };
    dialog.set_dialog_title(SharedString::from(strings.about_title));
    dialog.set_body_text(SharedString::from(strings.about_body));
    dialog.set_btn_close(SharedString::from(strings.btn_close));

    let weak = dialog.as_weak();
    let holder_clone = Rc::clone(holder);
    dialog.on_close_clicked(move || {
        if let Some(w) = weak.upgrade() {
            let _ = w.hide();
        }
        // Drop our strong reference so the dialog is reclaimed.
        *holder_clone.borrow_mut() = None;
    });

    if dialog.show().is_ok() {
        *holder.borrow_mut() = Some(dialog);
    }
}

// =============================================================================
// Update check / donation prompt — MessageBox-based.
// =============================================================================

fn show_donation_dialog(version: &str, strings: &Strings) {
    let body = strings.donation_prompt.replacen("{}", version, 1);
    match dialog::yes_no_cancel(strings.donation_title, &body) {
        DialogResult::Yes => update::open_url(update::sponsor_url()),
        DialogResult::No => update::record_donation_skip(),
        DialogResult::Cancel => update::dismiss_donation(),
    }
}

fn start_update_check(strings: &'static Strings) {
    std::thread::spawn(move || {
        if !update::should_check_now() {
            return;
        }
        let Some(info) = update::check_for_update() else {
            return;
        };
        if update::is_version_skipped(&info.latest_version) {
            return;
        }
        // Marshal the prompt back to the UI thread so the MessageBox
        // is owned by the main thread like the rest of the GUI.
        let _ = slint::invoke_from_event_loop(move || {
            show_update_dialog(&info, strings);
        });
    });
}

fn show_update_dialog(info: &update::UpdateInfo, strings: &Strings) {
    let header = strings
        .update_available
        .replacen("{}", &info.latest_version, 1)
        .replacen("{}", update::current_version(), 1);
    let content = format!("{header}\n\n{}", strings.update_prompt);
    match dialog::yes_no_cancel("ArcThumb", &content) {
        DialogResult::Yes => update::open_url(&info.release_url),
        DialogResult::No => update::skip_version(&info.latest_version),
        DialogResult::Cancel => {}
    }
}
