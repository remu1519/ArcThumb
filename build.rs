//! Build script: compile the Slint UI for `arcthumb-config.exe` and
//! embed a Windows resource file containing the app manifest + icon.
//!
//! The manifest declares Per-Monitor DPI v2 and Common Controls v6
//! so `arcthumb-config.exe` scales correctly on mixed-DPI setups and
//! picks up the modern visual style.
//!
//! Cargo links the compiled `.res` into every output artifact, so
//! `arcthumb.dll` also carries the manifest. This is harmless —
//! the shell extension DLL ignores its own manifest.

fn main() {
    // Only rerun when the resources change.
    println!("cargo:rerun-if-changed=resources/arcthumb-config.rc");
    println!("cargo:rerun-if-changed=resources/arcthumb-config.manifest");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=ui/main.slint");

    // On non-Windows targets this is a no-op so `cargo check` on
    // other platforms still works.
    #[cfg(target_os = "windows")]
    embed_resource::compile("resources/arcthumb-config.rc", embed_resource::NONE);

    // Compile the Slint UI for arcthumb-config. Generated Rust code
    // lands in OUT_DIR and is pulled in by `slint::include_modules!()`.
    slint_build::compile("ui/main.slint").expect("failed to compile Slint UI");
}
