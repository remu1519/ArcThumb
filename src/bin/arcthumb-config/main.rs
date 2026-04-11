//! ArcThumb Configuration — dual-mode binary.
//!
//! ## GUI mode (default)
//!
//! Running with no arguments launches a Slint-based settings window
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

mod apply;
mod cache;
mod dialogs;
mod dll_path;
mod extension_model;
mod message_box;
mod locale;
mod state;
mod ui;
mod update;
mod update_check;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("--install") => std::process::exit(cli_install()),
        Some("--uninstall") => std::process::exit(cli_uninstall()),
        _ => {
            if ui::run_gui().is_err() {
                std::process::exit(5);
            }
        }
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
