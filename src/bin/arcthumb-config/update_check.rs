//! Background update check driver.
//!
//! Spawns a worker thread on GUI startup that hits the GitHub
//! releases API, and if a newer version exists and the user has
//! not opted out of the reminder, marshals the result back onto
//! the Slint event loop so `dialogs::show_update_dialog` can open
//! a real window.
//!
//! The actual HTTP fetch, throttle logic, and "has the user
//! skipped this version" check all live in `update`. This module
//! is the thin glue that turns a `Some(UpdateInfo)` into a visible
//! prompt on the UI thread.
//!
//! ## Why a separate module
//!
//! Phase 2 of the refactor pulled this function out of `ui.rs`
//! alongside the Slint sub-dialogs so each concern has its own
//! short file. The behaviour is unchanged — `start_update_check`
//! is byte-identical to the version that used to live in `ui.rs`,
//! it just imports `show_update_dialog` from its new home.

use crate::dialogs;
use crate::locale::Strings;
use crate::update;

/// Kick off the background update check. Returns immediately.
///
/// The worker thread is fire-and-forget; its only observable side
/// effect is posting a closure onto the Slint event loop that may
/// open an `UpdateDialog`. If the user has disabled update checks,
/// the throttle window hasn't elapsed, no newer release exists, or
/// the user has already hit "Skip this version", the thread exits
/// silently without touching the UI.
pub fn start_update_check(strings: &'static Strings) {
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
        // Marshal the prompt back to the UI thread so the Slint
        // window is owned by the same thread as the rest of the GUI.
        let _ = slint::invoke_from_event_loop(move || {
            dialogs::show_update_dialog(info, strings);
        });
    });
}
