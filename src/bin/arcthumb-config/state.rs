//! Snapshot of everything the GUI reads and writes. Rebuilt from
//! the registry at startup and after every Apply so the UI stays
//! in sync with the actual registry state.

use arcthumb::registry;
use arcthumb::settings::Settings;

/// Number of extensions ArcThumb can manage. Must match the length
/// of `registry::EXTENSIONS`.
pub const EXT_COUNT: usize = 8;

/// Settings + per-extension enable state. No "is the shell extension
/// installed?" tracking here — installation is the installer's job,
/// not the GUI's. The GUI only toggles per-extension bindings.
#[derive(Debug, Clone)]
pub struct UiModel {
    pub settings: Settings,
    /// Parallel to `registry::EXTENSIONS`. `true` = this extension's
    /// ShellEx key is currently present in the registry.
    pub ext_enabled: [bool; EXT_COUNT],
}

impl UiModel {
    pub fn load() -> Self {
        debug_assert_eq!(registry::EXTENSIONS.len(), EXT_COUNT);

        let settings = Settings::load_from_registry_uncached();
        let ext_enabled: [bool; EXT_COUNT] = std::array::from_fn(|i| {
            registry::is_extension_registered(registry::EXTENSIONS[i])
        });
        Self {
            settings,
            ext_enabled,
        }
    }
}

impl Default for UiModel {
    /// `#[derive(NwgUi)]` requires `Default`. A placeholder model
    /// is fine — we overwrite it with real data right after `build_ui`.
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            ext_enabled: [false; EXT_COUNT],
        }
    }
}
