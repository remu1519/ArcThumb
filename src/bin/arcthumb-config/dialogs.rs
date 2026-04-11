//! Slint sub-dialogs hosted by `arcthumb-config`.
//!
//! Currently three: About, Update, Donation. All three are
//! non-modal Slint windows shown on top of the main settings
//! window, kept alive until the user closes them via one of their
//! buttons.
//!
//! ## Lifetime pattern
//!
//! Slint windows are `!Send`, so we cannot stash them behind a
//! `Mutex`. Each dialog has its own `thread_local!` cell that
//! holds the strong `ComponentHandle` reference while the dialog
//! is visible. When the user clicks a button the callback:
//!
//!   1. Hides the window via the `Weak` handle.
//!   2. Clears the `thread_local` slot, dropping the last strong
//!      reference and letting Slint reclaim the component.
//!
//! `arcthumb-config` only ever has one UI thread (the Slint event
//! loop runs there), so the thread-locals are both safe and the
//! natural home for dialog handles.
//!
//! ## Why a separate module
//!
//! Phase 2 of the refactor pulled these three dialogs out of
//! `ui.rs` to shrink it below 600 lines. The dialogs themselves
//! share nothing with the main settings logic beyond a couple of
//! `update::*` and `locale::Strings` references, so moving them
//! out is a pure file-level split with no behaviour change.

use std::cell::RefCell;

use slint::{ComponentHandle, SharedString};

use crate::locale::Strings;
use crate::ui::{AboutDialog, DonationDialog, UpdateDialog};
use crate::update;

thread_local! {
    static ABOUT_DIALOG: RefCell<Option<AboutDialog>> = const { RefCell::new(None) };
    static UPDATE_DIALOG: RefCell<Option<UpdateDialog>> = const { RefCell::new(None) };
    static DONATION_DIALOG: RefCell<Option<DonationDialog>> = const { RefCell::new(None) };
}

// =============================================================================
// About dialog — Slint window so we can embed `AboutSlint`.
// =============================================================================

pub fn show_about(strings: &Strings) {
    // Already open? Do nothing — Slint will keep the existing window
    // focused. We avoid stacking duplicate dialogs on rapid clicks.
    let already_open = ABOUT_DIALOG.with(|h| h.borrow().is_some());
    if already_open {
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
    dialog.on_close_clicked(move || {
        if let Some(w) = weak.upgrade() {
            let _ = w.hide();
        }
        ABOUT_DIALOG.with(|h| *h.borrow_mut() = None);
    });

    if dialog.show().is_ok() {
        ABOUT_DIALOG.with(|h| *h.borrow_mut() = Some(dialog));
    }
}

// =============================================================================
// Update dialog — Slint window with a "Skip this version" checkbox.
// =============================================================================

pub fn show_update_dialog(info: update::UpdateInfo, strings: &'static Strings) {
    let already_open = UPDATE_DIALOG.with(|h| h.borrow().is_some());
    if already_open {
        return;
    }

    let dialog = match UpdateDialog::new() {
        Ok(d) => d,
        Err(_) => return,
    };

    let header = strings
        .update_available
        .replacen("{}", &info.latest_version, 1)
        .replacen("{}", update::current_version(), 1);

    dialog.set_dialog_title(SharedString::from(strings.update_title));
    dialog.set_message_text(SharedString::from(header));
    dialog.set_skip_checkbox_label(SharedString::from(strings.update_skip_checkbox));
    dialog.set_btn_open(SharedString::from(strings.update_btn_open));
    dialog.set_btn_later(SharedString::from(strings.update_btn_later));

    // Open download page. Honors the skip checkbox so the user can
    // both grab the new version and tell us not to remind them again.
    {
        let weak = dialog.as_weak();
        let release_url = info.release_url.clone();
        let latest_version = info.latest_version.clone();
        dialog.on_open_clicked(move || {
            if let Some(d) = weak.upgrade() {
                if d.get_skip_checked() {
                    update::skip_version(&latest_version);
                }
                let _ = d.hide();
            }
            update::open_url(&release_url);
            UPDATE_DIALOG.with(|h| *h.borrow_mut() = None);
        });
    }

    // Remind me later. Same checkbox handling, no URL.
    {
        let weak = dialog.as_weak();
        let latest_version = info.latest_version.clone();
        dialog.on_later_clicked(move || {
            if let Some(d) = weak.upgrade() {
                if d.get_skip_checked() {
                    update::skip_version(&latest_version);
                }
                let _ = d.hide();
            }
            UPDATE_DIALOG.with(|h| *h.borrow_mut() = None);
        });
    }

    if dialog.show().is_ok() {
        UPDATE_DIALOG.with(|h| *h.borrow_mut() = Some(dialog));
    }
}

// =============================================================================
// Donation dialog — Slint window with a "Don't show again" checkbox.
// =============================================================================

pub fn show_donation_dialog(version: &str, strings: &'static Strings) {
    let already_open = DONATION_DIALOG.with(|h| h.borrow().is_some());
    if already_open {
        return;
    }

    let dialog = match DonationDialog::new() {
        Ok(d) => d,
        Err(_) => return,
    };

    let body = strings.donation_prompt.replacen("{}", version, 1);

    dialog.set_dialog_title(SharedString::from(strings.donation_title));
    dialog.set_message_text(SharedString::from(body));
    dialog.set_dont_show_label(SharedString::from(strings.donation_dont_show_checkbox));
    dialog.set_btn_sponsor(SharedString::from(strings.donation_btn_sponsor));
    dialog.set_btn_later(SharedString::from(strings.donation_btn_later));

    // Open sponsor page. The "don't show again" checkbox is a hard
    // dismissal — record it so the prompt never fires again.
    {
        let weak = dialog.as_weak();
        dialog.on_sponsor_clicked(move || {
            if let Some(d) = weak.upgrade() {
                if d.get_dont_show_checked() {
                    update::dismiss_donation();
                }
                let _ = d.hide();
            }
            update::open_url(update::sponsor_url());
            DONATION_DIALOG.with(|h| *h.borrow_mut() = None);
        });
    }

    // Maybe next time. Honors the dismiss checkbox the same way.
    {
        let weak = dialog.as_weak();
        dialog.on_later_clicked(move || {
            if let Some(d) = weak.upgrade() {
                if d.get_dont_show_checked() {
                    update::dismiss_donation();
                } else {
                    update::record_donation_skip();
                }
                let _ = d.hide();
            }
            DONATION_DIALOG.with(|h| *h.borrow_mut() = None);
        });
    }

    if dialog.show().is_ok() {
        DONATION_DIALOG.with(|h| *h.borrow_mut() = Some(dialog));
    }
}
