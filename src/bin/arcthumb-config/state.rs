//! Snapshot of everything the GUI reads and writes. Rebuilt from
//! the registry at startup and after every Apply so the UI stays
//! in sync with the actual registry state.

use arcthumb::registry;
use arcthumb::settings::Settings;

/// Number of extensions ArcThumb can manage. Must match the length
/// of `registry::EXTENSIONS`.
pub const EXT_COUNT: usize = 12;

/// Settings + per-extension enable state. No "is the shell extension
/// installed?" tracking here — installation is the installer's job,
/// not the GUI's. The GUI only toggles per-extension bindings and the
/// global preview-pane handler.
#[derive(Debug, Clone)]
pub struct UiModel {
    pub settings: Settings,
    /// Parallel to `registry::EXTENSIONS`. `true` = this extension's
    /// ShellEx key is currently present in the registry.
    pub ext_enabled: [bool; EXT_COUNT],
    /// `true` iff the preview-handler CLSID is currently registered.
    /// When the user toggles this, ApplyChanges (un)registers the
    /// CLSID and binds/unbinds *all* extensions in one batch.
    pub preview_enabled: bool,
}

impl UiModel {
    pub fn load() -> Self {
        debug_assert_eq!(registry::EXTENSIONS.len(), EXT_COUNT);

        let settings = Settings::load_from_registry_uncached();
        let ext_enabled: [bool; EXT_COUNT] =
            std::array::from_fn(|i| registry::is_extension_registered(registry::EXTENSIONS[i]));
        let preview_enabled = registry::is_preview_enabled();
        Self {
            settings,
            ext_enabled,
            preview_enabled,
        }
    }
}
