//! Apply pipeline for the config GUI.
//!
//! Splits *what changed* from *actually mutate the registry* so the
//! diff logic can be unit-tested without touching real shell state.
//!
//! ## Architecture
//!
//! 1. [`compute_apply_plan`] is a pure function that takes the
//!    previous and desired state and emits a list of [`ApplyAction`]s
//!    to perform.
//! 2. [`apply_plan`] drives those actions through a [`RegistryOps`]
//!    trait. Production uses [`RealRegistryOps`]; tests inject a
//!    recording mock so they can assert on the calls without
//!    touching `HKCU`.
//!
//! ## Why a separate module
//!
//! Before this split, the only way to test "Apply does the right
//! thing when the user toggles `.cbz`" was to drive a real Slint
//! window AND let the test case write to `HKCU\Software\Classes`.
//! Both are too costly to run on every `cargo test` invocation.
//! Now the diff logic is a 30-line pure function, the side-effect
//! layer is a six-method trait, and both have their own test suites.

use arcthumb::registry;
use arcthumb::settings::Settings;

use crate::dll_path;
use crate::state::{EXT_COUNT, UiModel};

/// One side-effecting registry operation that Apply may need to
/// perform. Order within a `Vec<ApplyAction>` matches the order
/// returned by [`compute_apply_plan`] and is the order in which
/// [`apply_plan`] runs them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyAction {
    /// User changed sort order or cover preference. Always emitted
    /// first when present, so a registry-write failure aborts before
    /// shell registrations are touched.
    SaveSettings(Settings),
    /// Bind the thumbnail provider to one extension.
    RegisterExtension(&'static str),
    /// Unbind the thumbnail provider from one extension.
    UnregisterExtension(&'static str),
    /// Register the preview-handler CLSID and bind it to every
    /// supported extension.
    EnablePreview,
    /// Unbind the preview handler everywhere and remove its CLSID.
    DisablePreview,
}

/// Compute the registry mutations needed to move from `old` to the
/// desired UI state. Pure function — no I/O, no logging, safe to
/// call from tests.
pub fn compute_apply_plan(
    old: &UiModel,
    new_settings: Settings,
    new_ext_enabled: [bool; EXT_COUNT],
    new_preview_enabled: bool,
) -> Vec<ApplyAction> {
    let mut actions = Vec::new();

    if new_settings != old.settings {
        actions.push(ApplyAction::SaveSettings(new_settings));
    }

    for (i, &ext) in registry::EXTENSIONS.iter().enumerate() {
        match (old.ext_enabled[i], new_ext_enabled[i]) {
            (false, true) => actions.push(ApplyAction::RegisterExtension(ext)),
            (true, false) => actions.push(ApplyAction::UnregisterExtension(ext)),
            _ => {}
        }
    }

    if !old.preview_enabled && new_preview_enabled {
        actions.push(ApplyAction::EnablePreview);
    } else if old.preview_enabled && !new_preview_enabled {
        actions.push(ApplyAction::DisablePreview);
    }

    actions
}

/// Outcome of running an apply plan: which actions failed and
/// whether anything we did affects the shell.
#[derive(Debug, Default)]
pub struct ApplyOutcome {
    /// `Some(error_text)` iff `SaveSettings` failed. When this is
    /// set, [`apply_plan`] aborts the rest of the plan early so we
    /// don't leave the shell in a half-mutated state.
    pub settings_save_error: Option<String>,
    /// Extensions whose register/unregister call failed. Per-action
    /// failures are recorded but don't stop the rest of the plan.
    pub failed_extensions: Vec<&'static str>,
    /// `Some(error_text)` iff the preview-pane toggle failed.
    pub preview_error: Option<String>,
    /// `true` iff at least one shell registration was actually
    /// touched. Used to decide whether to call
    /// [`RegistryOps::notify_assoc_changed`] at the end.
    pub shell_state_changed: bool,
}

impl ApplyOutcome {
    pub fn is_ok(&self) -> bool {
        self.settings_save_error.is_none()
            && self.failed_extensions.is_empty()
            && self.preview_error.is_none()
    }
}

/// The set of registry-mutating operations the apply pipeline can
/// perform. Real runs use [`RealRegistryOps`]; tests use a
/// recording mock so they can assert on the calls.
pub trait RegistryOps {
    fn save_settings(&self, settings: &Settings) -> std::io::Result<()>;
    fn register_extension(&self, ext: &'static str) -> std::io::Result<()>;
    fn unregister_extension(&self, ext: &'static str) -> std::io::Result<()>;
    fn enable_preview(&self) -> std::io::Result<()>;
    fn disable_preview(&self) -> std::io::Result<()>;
    fn notify_assoc_changed(&self);
}

/// Production implementation that talks directly to `HKCU` via the
/// `arcthumb::registry` module.
pub struct RealRegistryOps;

impl RegistryOps for RealRegistryOps {
    fn save_settings(&self, settings: &Settings) -> std::io::Result<()> {
        settings.save_to_registry()
    }

    fn register_extension(&self, ext: &'static str) -> std::io::Result<()> {
        registry::register_extension(ext)
    }

    fn unregister_extension(&self, ext: &'static str) -> std::io::Result<()> {
        registry::unregister_extension(ext)
    }

    fn enable_preview(&self) -> std::io::Result<()> {
        let dll = dll_path::resolve_dll_path().map_err(std::io::Error::other)?;
        registry::register_preview_clsid(&dll)?;
        for ext in registry::EXTENSIONS {
            registry::register_preview_extension(ext)?;
        }
        Ok(())
    }

    fn disable_preview(&self) -> std::io::Result<()> {
        for ext in registry::EXTENSIONS {
            let _ = registry::unregister_preview_extension(ext);
        }
        let _ = registry::unregister_preview_clsid();
        Ok(())
    }

    fn notify_assoc_changed(&self) {
        registry::notify_assoc_changed();
    }
}

/// Execute the plan via the given registry ops.
///
/// * `SaveSettings` failure aborts the rest of the plan to avoid
///   half-mutated shell state.
/// * Per-extension and preview failures are recorded but don't
///   abort, mirroring the original `apply_changes` behaviour where
///   a single broken extension didn't block the others.
pub fn apply_plan(plan: &[ApplyAction], ops: &dyn RegistryOps) -> ApplyOutcome {
    let mut out = ApplyOutcome::default();

    for action in plan {
        match action {
            ApplyAction::SaveSettings(settings) => {
                if let Err(e) = ops.save_settings(settings) {
                    out.settings_save_error = Some(e.to_string());
                    return out;
                }
            }
            ApplyAction::RegisterExtension(ext) => {
                if ops.register_extension(ext).is_err() {
                    out.failed_extensions.push(ext);
                } else {
                    out.shell_state_changed = true;
                }
            }
            ApplyAction::UnregisterExtension(ext) => {
                if ops.unregister_extension(ext).is_err() {
                    out.failed_extensions.push(ext);
                } else {
                    out.shell_state_changed = true;
                }
            }
            ApplyAction::EnablePreview => {
                if let Err(e) = ops.enable_preview() {
                    out.preview_error = Some(e.to_string());
                } else {
                    out.shell_state_changed = true;
                }
            }
            ApplyAction::DisablePreview => {
                if let Err(e) = ops.disable_preview() {
                    out.preview_error = Some(e.to_string());
                } else {
                    out.shell_state_changed = true;
                }
            }
        }
    }

    if out.shell_state_changed {
        ops.notify_assoc_changed();
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcthumb::settings::SortOrder;
    use std::cell::RefCell;

    fn baseline_model() -> UiModel {
        // Two extensions on, the rest off, natural sort, cover prio
        // on, preview off. Mid-spectrum so any direction the test
        // mutates produces a meaningful diff.
        let mut ext = [false; EXT_COUNT];
        ext[0] = true; // .zip
        ext[1] = true; // .cbz
        UiModel {
            settings: Settings {
                sort_order: SortOrder::Natural,
                prefer_cover_names: true,
            },
            ext_enabled: ext,
            preview_enabled: false,
        }
    }

    // ----- compute_apply_plan ---------------------------------------------

    #[test]
    fn empty_plan_when_nothing_changes() {
        let model = baseline_model();
        let plan = compute_apply_plan(
            &model,
            model.settings,
            model.ext_enabled,
            model.preview_enabled,
        );
        assert!(plan.is_empty());
    }

    #[test]
    fn settings_change_emits_save_settings() {
        let model = baseline_model();
        let new_settings = Settings {
            sort_order: SortOrder::Alphabetical,
            prefer_cover_names: true,
        };
        let plan = compute_apply_plan(&model, new_settings, model.ext_enabled, false);
        assert_eq!(plan, vec![ApplyAction::SaveSettings(new_settings)]);
    }

    #[test]
    fn cover_preference_change_emits_save_settings() {
        let model = baseline_model();
        let new_settings = Settings {
            sort_order: model.settings.sort_order,
            prefer_cover_names: !model.settings.prefer_cover_names,
        };
        let plan = compute_apply_plan(&model, new_settings, model.ext_enabled, false);
        assert_eq!(plan, vec![ApplyAction::SaveSettings(new_settings)]);
    }

    #[test]
    fn enabling_extension_emits_register() {
        let model = baseline_model();
        let mut new_ext = model.ext_enabled;
        new_ext[2] = true; // turn .rar on
        let plan = compute_apply_plan(&model, model.settings, new_ext, false);
        assert_eq!(
            plan,
            vec![ApplyAction::RegisterExtension(registry::EXTENSIONS[2])]
        );
    }

    #[test]
    fn disabling_extension_emits_unregister() {
        let model = baseline_model();
        let mut new_ext = model.ext_enabled;
        new_ext[0] = false; // turn .zip off
        let plan = compute_apply_plan(&model, model.settings, new_ext, false);
        assert_eq!(
            plan,
            vec![ApplyAction::UnregisterExtension(registry::EXTENSIONS[0])]
        );
    }

    #[test]
    fn extension_diff_preserves_index_order() {
        let model = baseline_model();
        let mut new_ext = [false; EXT_COUNT];
        new_ext[3] = true; // .cbr
        new_ext[7] = true; // .epub
        let plan = compute_apply_plan(&model, model.settings, new_ext, false);
        // Baseline had .zip and .cbz on; turning everything except
        // .cbr and .epub off, then turning those two on, should emit
        // unregister .zip, unregister .cbz, register .cbr, register
        // .epub — in index order.
        assert_eq!(
            plan,
            vec![
                ApplyAction::UnregisterExtension(registry::EXTENSIONS[0]),
                ApplyAction::UnregisterExtension(registry::EXTENSIONS[1]),
                ApplyAction::RegisterExtension(registry::EXTENSIONS[3]),
                ApplyAction::RegisterExtension(registry::EXTENSIONS[7]),
            ]
        );
    }

    #[test]
    fn enabling_preview_emits_enable_preview() {
        let model = baseline_model();
        let plan = compute_apply_plan(&model, model.settings, model.ext_enabled, true);
        assert_eq!(plan, vec![ApplyAction::EnablePreview]);
    }

    #[test]
    fn disabling_preview_emits_disable_preview() {
        let mut model = baseline_model();
        model.preview_enabled = true;
        let plan = compute_apply_plan(&model, model.settings, model.ext_enabled, false);
        assert_eq!(plan, vec![ApplyAction::DisablePreview]);
    }

    #[test]
    fn combined_changes_produce_combined_plan() {
        let model = baseline_model();
        let new_settings = Settings {
            sort_order: SortOrder::Alphabetical,
            prefer_cover_names: false,
        };
        let mut new_ext = model.ext_enabled;
        new_ext[0] = false; // disable .zip
        new_ext[7] = true; // enable .epub
        let plan = compute_apply_plan(&model, new_settings, new_ext, true);

        assert_eq!(plan.len(), 4);
        // Settings always come first.
        assert_eq!(plan[0], ApplyAction::SaveSettings(new_settings));
        // Extensions next, in index order.
        assert_eq!(
            plan[1],
            ApplyAction::UnregisterExtension(registry::EXTENSIONS[0])
        );
        assert_eq!(
            plan[2],
            ApplyAction::RegisterExtension(registry::EXTENSIONS[7])
        );
        // Preview last.
        assert_eq!(plan[3], ApplyAction::EnablePreview);
    }

    // ----- MockRegistryOps + apply_plan ----------------------------------

    #[derive(Default)]
    struct MockRegistryOps {
        calls: RefCell<Vec<String>>,
        fail_on: RefCell<Vec<String>>,
        notify_called: RefCell<bool>,
    }

    impl MockRegistryOps {
        fn fail_on(self, action: &str) -> Self {
            self.fail_on.borrow_mut().push(action.to_string());
            self
        }

        fn record(&self, name: String) -> std::io::Result<()> {
            let fail = self.fail_on.borrow().contains(&name);
            self.calls.borrow_mut().push(name);
            if fail {
                Err(std::io::Error::other("mock failure"))
            } else {
                Ok(())
            }
        }
    }

    impl RegistryOps for MockRegistryOps {
        fn save_settings(&self, _settings: &Settings) -> std::io::Result<()> {
            self.record("save_settings".into())
        }
        fn register_extension(&self, ext: &'static str) -> std::io::Result<()> {
            self.record(format!("register_extension:{ext}"))
        }
        fn unregister_extension(&self, ext: &'static str) -> std::io::Result<()> {
            self.record(format!("unregister_extension:{ext}"))
        }
        fn enable_preview(&self) -> std::io::Result<()> {
            self.record("enable_preview".into())
        }
        fn disable_preview(&self) -> std::io::Result<()> {
            self.record("disable_preview".into())
        }
        fn notify_assoc_changed(&self) {
            *self.notify_called.borrow_mut() = true;
        }
    }

    #[test]
    fn apply_plan_executes_actions_in_order() {
        let plan = vec![
            ApplyAction::RegisterExtension(".zip"),
            ApplyAction::RegisterExtension(".cbz"),
            ApplyAction::EnablePreview,
        ];
        let ops = MockRegistryOps::default();
        let outcome = apply_plan(&plan, &ops);
        assert!(outcome.is_ok());
        assert!(outcome.shell_state_changed);
        assert!(*ops.notify_called.borrow());
        assert_eq!(
            *ops.calls.borrow(),
            vec![
                "register_extension:.zip".to_string(),
                "register_extension:.cbz".to_string(),
                "enable_preview".to_string(),
            ]
        );
    }

    #[test]
    fn apply_plan_records_settings_save_failure() {
        let plan = vec![ApplyAction::SaveSettings(Settings::default())];
        let ops = MockRegistryOps::default().fail_on("save_settings");
        let outcome = apply_plan(&plan, &ops);
        assert!(!outcome.is_ok());
        assert!(outcome.settings_save_error.is_some());
    }

    #[test]
    fn settings_save_failure_aborts_remaining_plan() {
        let plan = vec![
            ApplyAction::SaveSettings(Settings::default()),
            ApplyAction::RegisterExtension(".zip"),
            ApplyAction::EnablePreview,
        ];
        let ops = MockRegistryOps::default().fail_on("save_settings");
        let outcome = apply_plan(&plan, &ops);
        // Only the settings save was attempted.
        assert_eq!(*ops.calls.borrow(), vec!["save_settings".to_string()]);
        assert!(outcome.settings_save_error.is_some());
        // No registrations were attempted, so no notify either.
        assert!(!*ops.notify_called.borrow());
    }

    #[test]
    fn apply_plan_records_extension_failures_but_continues() {
        let plan = vec![
            ApplyAction::RegisterExtension(".zip"),
            ApplyAction::RegisterExtension(".cbz"),
            ApplyAction::RegisterExtension(".rar"),
        ];
        let ops = MockRegistryOps::default().fail_on("register_extension:.cbz");
        let outcome = apply_plan(&plan, &ops);
        assert!(!outcome.is_ok());
        assert_eq!(outcome.failed_extensions, vec![".cbz"]);
        // The .rar registration was still attempted.
        assert_eq!(ops.calls.borrow().len(), 3);
        // .zip and .rar succeeded, so shell state did change.
        assert!(outcome.shell_state_changed);
        assert!(*ops.notify_called.borrow());
    }

    #[test]
    fn apply_plan_records_preview_error() {
        let plan = vec![ApplyAction::EnablePreview];
        let ops = MockRegistryOps::default().fail_on("enable_preview");
        let outcome = apply_plan(&plan, &ops);
        assert!(!outcome.is_ok());
        assert!(outcome.preview_error.is_some());
        assert!(!outcome.shell_state_changed);
        assert!(!*ops.notify_called.borrow());
    }

    #[test]
    fn apply_plan_does_not_notify_when_only_settings_changed() {
        let plan = vec![ApplyAction::SaveSettings(Settings::default())];
        let ops = MockRegistryOps::default();
        let outcome = apply_plan(&plan, &ops);
        assert!(outcome.is_ok());
        // Settings save alone doesn't change shell state.
        assert!(!outcome.shell_state_changed);
        assert!(!*ops.notify_called.borrow());
    }

    #[test]
    fn apply_plan_notifies_when_extension_changed() {
        let plan = vec![ApplyAction::RegisterExtension(".zip")];
        let ops = MockRegistryOps::default();
        let outcome = apply_plan(&plan, &ops);
        assert!(outcome.shell_state_changed);
        assert!(*ops.notify_called.borrow());
    }

    #[test]
    fn apply_plan_does_not_notify_when_only_extension_failed() {
        let plan = vec![ApplyAction::RegisterExtension(".zip")];
        let ops = MockRegistryOps::default().fail_on("register_extension:.zip");
        let outcome = apply_plan(&plan, &ops);
        assert!(!outcome.shell_state_changed);
        assert!(!*ops.notify_called.borrow());
    }

    #[test]
    fn empty_plan_is_a_no_op() {
        let ops = MockRegistryOps::default();
        let outcome = apply_plan(&[], &ops);
        assert!(outcome.is_ok());
        assert!(!outcome.shell_state_changed);
        assert!(ops.calls.borrow().is_empty());
        assert!(!*ops.notify_called.borrow());
    }
}
