//! The ArcThumb settings dialog.
//!
//! Fixed-size property-sheet-style window with three sections and
//! standard OK / Cancel / Apply buttons. No install/uninstall UI —
//! that's the installer's job. Apply diffs the current control
//! values against the last loaded `UiModel` and writes only what
//! changed.

use std::cell::{Cell, RefCell};
use std::sync::{Arc, Mutex};

use native_windows_derive::NwgUi;
use native_windows_gui as nwg;

use arcthumb::registry;
use arcthumb::settings::{Settings, SortOrder};

use crate::state::{EXT_COUNT, UiModel};
use crate::strings::{self, Strings};
use crate::update;

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
//   y=34             | [x] .zip [x] .cbz  [x] .rar [x] .cbr  |
//   y=62             | [x] .7z  [x] .cb7  [x] .cbt [x] .epub |
//   y=90             | [x] .fb2 [x] .mobi [x] .azw [x] .azw3 |
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
        size: (444, 336),
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
    #[nwg_control(parent: ext_frame, text: ".mobi", size: (96, 22), position: (114, 72))]
    cb_mobi: nwg::CheckBox,
    #[nwg_control(parent: ext_frame, text: ".azw", size: (96, 22), position: (214, 72))]
    cb_azw: nwg::CheckBox,
    #[nwg_control(parent: ext_frame, text: ".azw3", size: (96, 22), position: (314, 72))]
    cb_azw3: nwg::CheckBox,

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

    // Global toggle for the IPreviewHandler implementation. When
    // checked, applying the dialog (un)registers the preview-handler
    // CLSID and (un)binds it on every supported extension. Independent
    // of the per-extension thumbnail toggles above.
    #[nwg_control(
        text: "Enable preview pane (Alt+P)",
        size: (420, 22),
        position: (14, 260)
    )]
    cb_enable_preview: nwg::CheckBox,

    // ------------------------------------------------------------------
    // Bottom row.
    //
    // Left side: utility action — "Regenerate thumbnails" — at
    // (14, 300), 160 px wide. Wide enough for both the English label
    // and the Japanese "サムネイルを再生成". Visually separated from
    // the OK/Cancel/Apply triplet by the 6 px gap to OK at x=180.
    //
    // Right side: OK / Cancel / Apply — right-aligned with 12 px
    // right margin, 6 px gaps between buttons, 80 px each.
    //     Window width   = 444
    //     Right margin   = 12  → Apply right edge = 432, left = 352
    //     6 px gap       → Cancel right = 346, left = 266
    //     6 px gap       → OK right = 260, left = 180
    // ------------------------------------------------------------------
    #[nwg_control(text: "Regenerate thumbnails", size: (160, 26), position: (14, 300))]
    #[nwg_events( OnButtonClick: [ConfigApp::on_regenerate] )]
    regen_btn: nwg::Button,

    #[nwg_control(text: "OK", size: (80, 26), position: (180, 300))]
    #[nwg_events( OnButtonClick: [ConfigApp::on_ok] )]
    ok_btn: nwg::Button,

    #[nwg_control(text: "Cancel", size: (80, 26), position: (266, 300))]
    #[nwg_events( OnButtonClick: [ConfigApp::on_cancel] )]
    cancel_btn: nwg::Button,

    #[nwg_control(text: "Apply", size: (80, 26), position: (352, 300))]
    #[nwg_events( OnButtonClick: [ConfigApp::on_apply] )]
    apply_btn: nwg::Button,

    // ------------------------------------------------------------------
    // Background update check
    // ------------------------------------------------------------------
    #[nwg_control]
    #[nwg_events(OnNotice: [ConfigApp::on_update_check_complete])]
    update_notice: nwg::Notice,

    // Non-control state.
    model: RefCell<UiModel>,
    should_close: Cell<bool>,
    /// Filled by the background update-check thread; consumed by the
    /// Notice handler on the UI thread.
    pub update_result: Arc<Mutex<Option<update::UpdateInfo>>>,
}

impl ConfigApp {
    pub fn set_initial_model(&self, model: UiModel) {
        *self.model.borrow_mut() = model;
    }

    /// Spawn a background thread that checks for updates. When it
    /// finishes, the `update_notice` fires and the result is picked
    /// up by `on_update_check_complete`.
    pub fn start_update_check(&self) {
        let sender = self.update_notice.sender();
        let slot = Arc::clone(&self.update_result);
        std::thread::spawn(move || {
            if update::should_check_now() {
                if let Some(info) = update::check_for_update() {
                    if !update::is_version_skipped(&info.latest_version) {
                        *slot.lock().unwrap() = Some(info);
                    }
                }
            }
            sender.notice();
        });
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
                self.rb_natural
                    .set_check_state(nwg::RadioButtonState::Checked);
                self.rb_alpha
                    .set_check_state(nwg::RadioButtonState::Unchecked);
            }
            SortOrder::Alphabetical => {
                self.rb_natural
                    .set_check_state(nwg::RadioButtonState::Unchecked);
                self.rb_alpha
                    .set_check_state(nwg::RadioButtonState::Checked);
            }
        }

        // Cover preference
        self.cb_prefer_cover.set_text(strings.cb_prefer_cover);
        self.cb_prefer_cover
            .set_check_state(bool_to_check(model.settings.prefer_cover_names));

        // Preview pane toggle
        self.cb_enable_preview.set_text(strings.cb_enable_preview);
        self.cb_enable_preview
            .set_check_state(bool_to_check(model.preview_enabled));

        // Bottom buttons
        self.regen_btn.set_text(strings.btn_regenerate);
        self.ok_btn.set_text(strings.btn_ok);
        self.cancel_btn.set_text(strings.btn_cancel);
        self.apply_btn.set_text(strings.btn_apply);
    }

    fn collect_from_ui(&self) -> (Settings, [bool; EXT_COUNT], bool) {
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

        let preview_enabled = check_to_bool(self.cb_enable_preview.check_state());
        (settings, ext_enabled, preview_enabled)
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
            &self.cb_mobi,
            &self.cb_azw,
            &self.cb_azw3,
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

    /// "Regenerate thumbnails" button. Confirms with the user, then
    /// kills Explorer, wipes the Windows thumbnail/icon cache files,
    /// and restarts Explorer. The safety net for the case where
    /// `SHChangeNotify(SHCNE_ASSOCCHANGED)` from Apply / install
    /// wasn't enough — typically when the user opened an archive
    /// before installing ArcThumb and Explorer cached "no thumbnail"
    /// for it.
    fn on_regenerate(&self) {
        let strings = s();

        let params = nwg::MessageParams {
            title: strings.error_title,
            content: strings.regen_confirm,
            buttons: nwg::MessageButtons::OkCancel,
            icons: nwg::MessageIcons::Warning,
        };
        if nwg::modal_message(&self.window, &params) != nwg::MessageChoice::Ok {
            return;
        }

        match crate::cache::wipe_thumbnail_cache() {
            Ok(report) if report.failed.is_empty() => {
                nwg::modal_info_message(&self.window, strings.error_title, strings.regen_done);
            }
            Ok(_) => {
                nwg::modal_error_message(&self.window, strings.error_title, strings.regen_partial);
            }
            Err(e) => {
                self.error(strings.regen_partial, &e);
            }
        }
    }

    fn apply_changes(&self) -> bool {
        let strings = s();
        let (new_settings, new_ext_enabled, new_preview_enabled) = self.collect_from_ui();
        let mut ok = true;
        // Tracks whether anything in the registry actually changed.
        // We use this to decide whether to ask the Shell to drop its
        // icon/thumbnail cache at the end — there's no point poking
        // Explorer when the user clicked Apply without changing
        // anything that affects shell registrations.
        let mut shell_state_changed = false;

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
            self.error(
                strings.error_register,
                &format!("failed: {}", failures.join(", ")),
            );
        }

        // --- Preview pane handler (global toggle)
        let old_preview = self.model.borrow().preview_enabled;
        if old_preview != new_preview_enabled {
            match apply_preview_toggle(new_preview_enabled) {
                Ok(()) => shell_state_changed = true,
                Err(e) => {
                    self.error(strings.error_register, &format!("{e}"));
                    ok = false;
                }
            }
        }

        // Whenever we touched shell registrations, ask Explorer to
        // invalidate its icon/thumbnail cache so the change takes
        // effect immediately. Without this, newly enabled extensions
        // would still show the old "no thumbnail" cache entry until
        // the user logs out or wipes thumbcache_*.db by hand.
        if shell_state_changed {
            registry::notify_assoc_changed();
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

    // ------------------------------------------------------------------
    // Update check handlers
    // ------------------------------------------------------------------

    fn on_update_check_complete(&self) {
        let info = self.update_result.lock().unwrap().take();
        if let Some(info) = info {
            self.show_update_dialog(&info);
        }
    }

    fn show_update_dialog(&self, info: &update::UpdateInfo) {
        let strings = s();
        let title = strings
            .update_available
            .replacen("{}", &info.latest_version, 1)
            .replacen("{}", update::current_version(), 1);
        let content = format!("{title}\n\n{}", strings.update_prompt);
        let params = nwg::MessageParams {
            title: "ArcThumb",
            content: &content,
            buttons: nwg::MessageButtons::YesNoCancel,
            icons: nwg::MessageIcons::Info,
        };
        match nwg::modal_message(&self.window, &params) {
            nwg::MessageChoice::Yes => {
                update::open_url(&info.release_url);
            }
            nwg::MessageChoice::No => {
                update::skip_version(&info.latest_version);
            }
            _ => {} // Cancel = remind later, do nothing
        }
    }

    pub fn show_donation_dialog(&self, version: &str) {
        let strings = s();
        let body = strings.donation_prompt.replacen("{}", version, 1);
        let params = nwg::MessageParams {
            title: strings.donation_title,
            content: &body,
            buttons: nwg::MessageButtons::YesNoCancel,
            icons: nwg::MessageIcons::Info,
        };
        match nwg::modal_message(&self.window, &params) {
            nwg::MessageChoice::Yes => {
                update::open_url(update::sponsor_url());
            }
            nwg::MessageChoice::No => {
                update::record_donation_skip();
            }
            _ => {
                // Cancel = don't show again
                update::dismiss_donation();
            }
        }
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

/// Register or unregister the preview-handler CLSID + bind/unbind it
/// across every supported extension. Called when the user flips the
/// "Enable preview pane" checkbox and clicks Apply.
fn apply_preview_toggle(enable: bool) -> std::io::Result<()> {
    if enable {
        let dll = crate::dll_path::resolve_dll_path().map_err(std::io::Error::other)?;
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
