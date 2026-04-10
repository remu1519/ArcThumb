//! ArcThumb Configuration — dual-mode binary.
//!
//! ## GUI mode (default)
//!
//! Running with no arguments launches a small native Windows dialog
//! where the user can enable/disable individual file extensions and
//! tweak the thumbnail selection behaviour (sort order, cover-name
//! preference).
//!
//! ## CLI mode
//!
//! ```text
//! arcthumb-config.exe --install
//!     Write the full shell-extension registration to HKCU
//!     (CLSID key + ShellEx bindings for every supported extension).
//!     Called by the Inno Setup installer as a post-install step.
//!
//! arcthumb-config.exe --uninstall
//!     Remove every ShellEx binding and the CLSID key.
//!     Called by the uninstaller as a pre-uninstall step.
//! ```
//!
//! Exit codes:
//! - `0` success
//! - `2` DLL not found (for --install)
//! - `3` CLSID registration failed
//! - `4` extension binding failed
//! - `5` GUI init failed (very rare)

// Hide the console on release builds. Debug builds keep the console
// so `cargo run` output is visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cache;
mod dll_path;
mod state;
mod strings;
mod ui;
mod update;

use native_windows_gui as nwg;
use nwg::NativeUi;

use crate::state::UiModel;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("--install") => std::process::exit(cli_install()),
        Some("--uninstall") => std::process::exit(cli_uninstall()),
        _ => run_gui(),
    }
}

fn cli_install() -> i32 {
    let dll_path = match dll_path::resolve_dll_path() {
        Ok(p) => p,
        Err(_) => return 2,
    };
    // Both COM classes (thumbnail provider + preview handler) are
    // registered together by the installer so the user gets both
    // features by default. The GUI's "Enable preview pane" checkbox
    // can later be unchecked to remove just the preview handler.
    if arcthumb::registry::register_clsid(&dll_path).is_err() {
        return 3;
    }
    if arcthumb::registry::register_preview_clsid(&dll_path).is_err() {
        return 3;
    }
    for ext in arcthumb::registry::EXTENSIONS {
        if arcthumb::registry::register_extension(ext).is_err() {
            return 4;
        }
        if arcthumb::registry::register_preview_extension(ext).is_err() {
            return 4;
        }
    }
    // Tell Explorer to drop its icon/thumbnail cache so the freshly
    // registered handlers take effect without a reboot — this is what
    // Microsoft's shell extension docs require us to do.
    arcthumb::registry::notify_assoc_changed();
    0
}

fn cli_uninstall() -> i32 {
    for ext in arcthumb::registry::EXTENSIONS {
        let _ = arcthumb::registry::unregister_extension(ext);
        let _ = arcthumb::registry::unregister_preview_extension(ext);
    }
    let _ = arcthumb::registry::unregister_clsid();
    let _ = arcthumb::registry::unregister_preview_clsid();
    arcthumb::registry::notify_assoc_changed();
    0
}

fn run_gui() {
    if nwg::init().is_err() {
        std::process::exit(5);
    }

    // Match the Windows Property-sheet look: Segoe UI, ~14 logical
    // units high (close to 11pt @ 96 DPI). Setting this BEFORE
    // building controls applies it to every control we create.
    let mut font = nwg::Font::default();
    if nwg::Font::builder()
        .family("Segoe UI")
        .size(14)
        .build(&mut font)
        .is_ok()
    {
        nwg::Font::set_global_default(Some(font));
    }

    ui::set_strings(strings::current());
    let model = UiModel::load();

    let app = ui::ConfigApp::build_ui(Default::default()).expect("failed to build UI");
    app.set_initial_model(model);
    app.refresh_from_model();

    // Donation prompt — synchronous, runs before the event loop.
    // Only fires when the user has just upgraded to a newer version.
    if let Some(ver) = update::should_show_donation() {
        app.show_donation_dialog(&ver);
        update::record_donation_shown();
    }

    // Background update check — non-blocking. The Notice control
    // signals the UI thread when the result is ready.
    app.start_update_check();

    nwg::dispatch_thread_events();
    drop(app);
}
