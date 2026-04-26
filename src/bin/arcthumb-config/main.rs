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
//!     Write the full shell-extension registration. Hive is picked
//!     automatically by elevation: HKLM when the process is elevated
//!     (per-machine install), HKCU otherwise (per-user install).
//!     Called by the Inno Setup installer as a post-install step.
//!
//! arcthumb-config.exe --uninstall
//!     Remove every ShellEx binding and the CLSID key from BOTH
//!     hives (best effort) so a per-user → per-machine switch or
//!     vice versa doesn't leave stale entries behind.
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
mod locale;
mod message_box;
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
            // Surface the failure with a native MessageBox before
            // exiting. Release builds run as `windows_subsystem =
            // "windows"`, so without this the user sees nothing —
            // not even a console line — and reports the binary as
            // broken. Reported in microsoft/winget-pkgs#364519.
            if let Err(e) = ui::run_gui() {
                let strings = locale::current();
                message_box::error(
                    strings.error_title,
                    &format!("{}\n\n{e}", strings.error_gui_init),
                );
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
    // HKLM when this process was started elevated (admin Inno install
    // mode), HKCU otherwise. This is what makes the shell extension
    // load under High-Integrity Explorer in Windows Sandbox and
    // enterprise lockdowns where HKCU CLSIDs are ignored.
    let scope = arcthumb::elevation::current_scope();
    // Both COM classes (thumbnail provider + preview handler) are
    // registered together by the installer so the user gets both
    // features by default. The GUI's "Enable preview pane" checkbox
    // can later be unchecked to remove just the preview handler.
    if arcthumb::registry::register_clsid(scope, &dll_path).is_err() {
        return 3;
    }
    if arcthumb::registry::register_preview_clsid(scope, &dll_path).is_err() {
        return 3;
    }
    for ext in arcthumb::registry::EXTENSIONS {
        if arcthumb::registry::register_extension(scope, ext).is_err() {
            return 4;
        }
        if arcthumb::registry::register_preview_extension(scope, ext).is_err() {
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
    // Clean BOTH hives best-effort. The user may have switched modes
    // between versions, or an old per-user install may still be lying
    // around when a new per-machine install is being uninstalled.
    for scope in arcthumb::registry::Scope::ALL.iter().copied() {
        for ext in arcthumb::registry::EXTENSIONS {
            let _ = arcthumb::registry::unregister_extension(scope, ext);
            let _ = arcthumb::registry::unregister_preview_extension(scope, ext);
        }
        let _ = arcthumb::registry::unregister_clsid(scope);
        let _ = arcthumb::registry::unregister_preview_clsid(scope);
    }
    arcthumb::registry::notify_assoc_changed();
    0
}
