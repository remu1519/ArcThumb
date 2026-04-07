//! Build script: embed a Windows resource file containing the app
//! manifest for `arcthumb-config.exe`.
//!
//! The manifest declares a dependency on Common Controls v6, which
//! `native-windows-gui` needs at runtime (`GetWindowSubclass` and
//! friends live in `ComCtl32.dll` v6). Without it, the exe won't
//! launch on any modern Windows machine.
//!
//! Cargo links the compiled `.res` into every output artifact, so
//! `arcthumb.dll` also carries the manifest. This is harmless —
//! the shell extension DLL ignores its own manifest.

fn main() {
    // Only rerun when the resources change.
    println!("cargo:rerun-if-changed=resources/arcthumb-config.rc");
    println!("cargo:rerun-if-changed=resources/arcthumb-config.manifest");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    // On non-Windows targets this is a no-op so `cargo check` on
    // other platforms still works.
    #[cfg(target_os = "windows")]
    embed_resource::compile("resources/arcthumb-config.rc", embed_resource::NONE);
}
