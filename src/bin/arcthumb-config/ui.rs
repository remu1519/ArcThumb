//! The ArcThumb settings dialog.
//!
//! Fixed-size property-sheet-style window with three sections and
//! standard OK / Cancel / Apply buttons. No install/uninstall UI —
//! that's the installer's job. Apply diffs the current control
//! values against the last loaded `UiModel` and writes only what
//! changed.

use std::cell::{Cell, RefCell};

use native_windows_derive::NwgUi;
use native_windows_gui as nwg;

use arcthumb::registry;
use arcthumb::settings::{Settings, SortOrder};

use crate::state::{UiModel, EXT_COUNT};
use crate::strings::{self, Strings};

// `#[derive(NwgUi)]` needs the struct to be `Default`, and `Default`
// can't pick a language, so the currently-selected strings live in a
// process-wide `OnceLock` populated before `build_ui`.
static STRINGS: std::sync::OnceLock<&'static Strings> = std::sync::OnceLock::new();

pub fn set_strings(s: &'static Strings) {
    let _ = STRINGS.set(s);
}

fn s() -> &'static Strings {
    STRINGS.get().copied().unwrap_or(&strings::EN)
}

// Window layout constants (pixels). Tuned for Segoe UI 14px — the
// default Windows 10/11 property-sheet font.
//
// Layout in detail:
//
//   Window: 444 x 304 (client area)
//
//   y=10   [Enabled extensions]   ← label at (14, 10)
//   y=18             +------------------------------...+   ← frame top
//   y=34             | [x] .zip [x] .cbz [x] .rar [x] .cbr  |
//   y=62             | [x] .7z  [x] .cb7 [x] .cbt [x] .epub |
//   y=90             | [x] .fb2                              |
//                    +------------------------------...+   ← frame bottom y=130
//   y=140  [Sort order]
//   y=148            +--------------------+
//                    | (o) Natural ...    |
//                    | ( ) Alphabetical   |
//                    +--------------------+   ← ends y=220
//   y=232  [x] Prefer cover ...
//   y=268  [ OK ] [ Cancel ] [ Apply ]
//
// Group "title labels" overlap the frame's top border. Because a
// plain nwg::Label inherits the parent window's COLOR_3DFACE
// background, it paints over the border line and creates the
// classic Win32 group-box "title sits on the border" look.

#[derive(Default, NwgUi)]
pub struct ConfigApp {
    #[nwg_control(
        size: (444, 304),
        position: (300, 200),
        title: "ArcThumb Configuration",
        flags: "WINDOW|VISIBLE"
    )]
    #[nwg_events( OnWindowClose: [ConfigApp::on_close] )]
    window: nwg::Window,

    // ------------------------------------------------------------------
    // Extensions group  (3 rows × 4 columns = up to 12 slots)
    // ------------------------------------------------------------------
    #[nwg_control(size: (424, 112), position: (10, 18), flags: "VISIBLE|BORDER")]
    ext_frame: nwg::Frame,

    // Title label — positioned AFTER the frame in z-order so it
    // paints over the frame's top border. The text itself carries
    // 4 leading spaces of left padding so there's a noticeable
    // gap between the label background's left edge and the visible
    // text. The width is tight against the trailing edge to keep
    // the right-side background minimal.
    #[nwg_control(text: "    Enabled extensions ", size: (134, 16), position: (14, 10))]
    ext_title: nwg::Label,

    #[nwg_control(parent: ext_frame, text: ".zip", size: (96, 22), position: (14, 16))]
    cb_zip: nwg::CheckBox,
    #[nwg_control(parent: ext_frame, text: ".cbz", size: (96, 22), position: (114, 16))]
    cb_cbz: nwg::CheckBox,
    #[nwg_control(parent: ext_frame, text: ".rar", size: (96, 22), position: (214, 16))]
    cb_rar: nwg::CheckBox,
    #[nwg_control(parent: ext_frame, text: ".cbr", size: (96, 22), position: (314, 16))]
    cb_cbr: nwg::CheckBox,
    #[nwg_control(parent: ext_frame, text: ".7z", size: (96, 22), position: (14, 44))]
    cb_7z: nwg::CheckBox,
    #[nwg_control(parent: ext_frame, text: ".cb7", size: (96, 22), position: (114, 44))]
    cb_cb7: nwg::CheckBox,
    #[nwg_control(parent: ext_frame, text: ".cbt", size: (96, 22), position: (214, 44))]
    cb_cbt: nwg::CheckBox,
    #[nwg_control(parent: ext_frame, text: ".epub", size: (96, 22), position: (314, 44))]
    cb_epub: nwg::CheckBox,
    #[nwg_control(parent: ext_frame, text: ".fb2", size: (96, 22), position: (14, 72))]
    cb_fb2: nwg::CheckBox,

    // ------------------------------------------------------------------
    // Sort order group
    // ------------------------------------------------------------------
    // Same width as ext_frame (424) so the two boxes line up
    // visually in a stack. Radio buttons leave the right side
    // of the frame empty — that's the Windows convention.
    #[nwg_control(size: (424, 72), position: (10, 148), flags: "VISIBLE|BORDER")]
    sort_frame: nwg::Frame,

    #[nwg_control(text: "    Sort order ", size: (84, 16), position: (14, 140))]
    sort_title: nwg::Label,

    #[nwg_control(
        parent: sort_frame,
        text: "Natural (page2 < page10)",
        size: (260, 22),
        position: (14, 12),
        flags: "VISIBLE|GROUP"
    )]
    rb_natural: nwg::RadioButton,

    #[nwg_control(parent: sort_frame, text: "Alphabetical", size: (260, 22), position: (14, 40))]
    rb_alpha: nwg::RadioButton,

    // ------------------------------------------------------------------
    // Cover preference (top-level checkbox, no group)
    // ------------------------------------------------------------------
    #[nwg_control(
        text: "Prefer cover / folder / thumb / thumbnail / front",
        size: (420, 22),
        position: (14, 232)
    )]
    cb_prefer_cover: nwg::CheckBox,

    // ------------------------------------------------------------------
    // OK / Cancel / Apply — right-aligned with 12 px right margin,
    // 6 px gaps between buttons, 80 px each.
    //     Window width   = 444
    //     Right margin   = 12  → Apply right edge = 432, left = 352
    //     6 px gap       → Cancel right = 346, left = 266
    //     6 px gap       → OK right = 260, left = 180
    // ------------------------------------------------------------------
    #[nwg_control(text: "OK", size: (80, 26), position: (180, 268))]
    #[nwg_events( OnButtonClick: [ConfigApp::on_ok] )]
    ok_btn: nwg::Button,

    #[nwg_control(text: "Cancel", size: (80, 26), position: (266, 268))]
    #[nwg_events( OnButtonClick: [ConfigApp::on_cancel] )]
    cancel_btn: nwg::Button,

    #[nwg_control(text: "Apply", size: (80, 26), position: (352, 268))]
    #[nwg_events( OnButtonClick: [ConfigApp::on_apply] )]
    apply_btn: nwg::Button,

    // Non-control state.
    model: RefCell<UiModel>,
    should_close: Cell<bool>,
}

impl ConfigApp {
    pub fn set_initial_model(&self, model: UiModel) {
        *self.model.borrow_mut() = model;
    }

    /// Push the current `model` into every control.
    pub fn refresh_from_model(&self) {
        let strings = s();
        let model = self.model.borrow();

        self.window.set_text(strings.window_title);
        self.ext_title.set_text(strings.group_extensions);
        self.sort_title.set_text(strings.group_sort);

        // Extensions
        let checkboxes = self.extension_checkboxes();
        for (i, cb) in checkboxes.iter().enumerate() {
            cb.set_check_state(bool_to_check(model.ext_enabled[i]));
        }

        // Sort order
        self.rb_natural.set_text(strings.sort_natural);
        self.rb_alpha.set_text(strings.sort_alphabetical);
        match model.settings.sort_order {
            SortOrder::Natural => {
                self.rb_natural.set_check_state(nwg::RadioButtonState::Checked);
                self.rb_alpha.set_check_state(nwg::RadioButtonState::Unchecked);
            }
            SortOrder::Alphabetical => {
                self.rb_natural.set_check_state(nwg::RadioButtonState::Unchecked);
                self.rb_alpha.set_check_state(nwg::RadioButtonState::Checked);
            }
        }

        // Cover preference
        self.cb_prefer_cover.set_text(strings.cb_prefer_cover);
        self.cb_prefer_cover
            .set_check_state(bool_to_check(model.settings.prefer_cover_names));

        // Bottom buttons
        self.ok_btn.set_text(strings.btn_ok);
        self.cancel_btn.set_text(strings.btn_cancel);
        self.apply_btn.set_text(strings.btn_apply);
    }

    fn collect_from_ui(&self) -> (Settings, [bool; EXT_COUNT]) {
        let sort_order = if self.rb_natural.check_state() == nwg::RadioButtonState::Checked {
            SortOrder::Natural
        } else {
            SortOrder::Alphabetical
        };
        let prefer_cover_names = check_to_bool(self.cb_prefer_cover.check_state());
        let settings = Settings {
            sort_order,
            prefer_cover_names,
        };

        let mut ext_enabled = [false; EXT_COUNT];
        let checkboxes = self.extension_checkboxes();
        for (i, cb) in checkboxes.iter().enumerate() {
            ext_enabled[i] = check_to_bool(cb.check_state());
        }
        (settings, ext_enabled)
    }

    fn extension_checkboxes(&self) -> [&nwg::CheckBox; EXT_COUNT] {
        [
            &self.cb_zip,
            &self.cb_cbz,
            &self.cb_rar,
            &self.cb_cbr,
            &self.cb_7z,
            &self.cb_cb7,
            &self.cb_cbt,
            &self.cb_epub,
            &self.cb_fb2,
        ]
    }

    // ------------------------------------------------------------------
    // Event handlers
    // ------------------------------------------------------------------

    fn on_close(&self) {
        nwg::stop_thread_dispatch();
    }

    fn on_cancel(&self) {
        self.should_close.set(true);
        nwg::stop_thread_dispatch();
    }

    fn on_ok(&self) {
        if self.apply_changes() {
            self.should_close.set(true);
            nwg::stop_thread_dispatch();
        }
    }

    fn on_apply(&self) {
        let _ = self.apply_changes();
    }

    fn apply_changes(&self) -> bool {
        let strings = s();
        let (new_settings, new_ext_enabled) = self.collect_from_ui();
        let mut ok = true;

        // --- Settings (sort order + prefer cover)
        let old_settings = self.model.borrow().settings;
        if new_settings != old_settings {
            if let Err(e) = new_settings.save_to_registry() {
                self.error(strings.error_save, &format!("{e}"));
                return false;
            }
        }

        // --- Per-extension shell binding diff
        let old_ext = self.model.borrow().ext_enabled;
        let mut failures: Vec<&'static str> = Vec::new();
        for i in 0..EXT_COUNT {
            let ext = registry::EXTENSIONS[i];
            match (old_ext[i], new_ext_enabled[i]) {
                (false, true) => {
                    if registry::register_extension(ext).is_err() {
                        failures.push(ext);
                        ok = false;
                    }
                }
                (true, false) => {
                    if registry::unregister_extension(ext).is_err() {
                        failures.push(ext);
                        ok = false;
                    }
                }
                _ => {}
            }
        }
        if !failures.is_empty() {
            self.error(
                strings.error_register,
                &format!("failed: {}", failures.join(", ")),
            );
        }

        self.reload_model();
        ok
    }

    fn reload_model(&self) {
        *self.model.borrow_mut() = UiModel::load();
        self.refresh_from_model();
    }

    fn error(&self, msg: &str, detail: &str) {
        let body = if detail.is_empty() {
            msg.to_string()
        } else {
            format!("{msg}\n\n{detail}")
        };
        nwg::modal_error_message(&self.window, s().error_title, &body);
    }
}

// =============================================================================
// bool ↔ nwg::CheckBoxState
// =============================================================================

fn bool_to_check(b: bool) -> nwg::CheckBoxState {
    if b {
        nwg::CheckBoxState::Checked
    } else {
        nwg::CheckBoxState::Unchecked
    }
}

fn check_to_bool(c: nwg::CheckBoxState) -> bool {
    matches!(c, nwg::CheckBoxState::Checked)
}
