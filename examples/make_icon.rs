//! One-shot tool to (re)generate `assets/icon.ico` from
//! `assets/icon.png`. Run after the source PNG is updated:
//!
//! ```text
//! cargo run --example make_icon
//! ```
//!
//! Why a one-shot example instead of a `build.rs` step:
//! - The icon source rarely changes; running it on every `cargo
//!   build` would just waste time.
//! - Examples link against the lib's rlib and reuse the existing
//!   `image` dependency, so no extra build-deps are needed.
//!
//! What it produces:
//! - A multi-resolution `.ico` containing 16/24/32/48/64/128/256 px
//!   variants, each PNG-encoded (Vista+ format). 32-bit BGRA so the
//!   icon keeps full alpha in every shell surface that displays it.

use std::io::Cursor;
use std::path::Path;

use image::{imageops::FilterType, DynamicImage, ImageFormat};

const SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256];

fn main() {
    let src = Path::new("assets/icon.png");
    let dst = Path::new("assets/icon.ico");

    let source = image::open(src).unwrap_or_else(|e| {
        panic!("failed to open {}: {e}", src.display());
    });

    // Resize-and-encode each size into a self-contained PNG.
    let sub_pngs: Vec<Vec<u8>> = SIZES
        .iter()
        .map(|&size| {
            let resized: DynamicImage =
                source.resize_exact(size, size, FilterType::Lanczos3);
            let mut buf = Vec::new();
            resized
                .write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
                .expect("encode PNG");
            buf
        })
        .collect();

    // ---- ICONDIR (6 bytes) ----
    // u16 reserved = 0
    // u16 type     = 1 (icon)
    // u16 count    = N
    let n = sub_pngs.len() as u16;
    let mut ico: Vec<u8> = Vec::new();
    ico.extend_from_slice(&0u16.to_le_bytes());
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&n.to_le_bytes());

    // ---- ICONDIRENTRY × N (16 bytes each) ----
    let header_size = 6 + 16 * sub_pngs.len();
    let mut offset: u32 = header_size as u32;
    for (i, png) in sub_pngs.iter().enumerate() {
        let size = SIZES[i];
        // 0 means 256 in the byte fields.
        let dim_byte: u8 = if size >= 256 { 0 } else { size as u8 };
        ico.push(dim_byte); // width
        ico.push(dim_byte); // height
        ico.push(0); // colorCount (0 for >= 256 colors)
        ico.push(0); // reserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bitCount (BGRA)
        ico.extend_from_slice(&(png.len() as u32).to_le_bytes()); // bytesInRes
        ico.extend_from_slice(&offset.to_le_bytes()); // imageOffset
        offset += png.len() as u32;
    }

    // ---- Image data ----
    for png in &sub_pngs {
        ico.extend_from_slice(png);
    }

    std::fs::write(dst, &ico)
        .unwrap_or_else(|e| panic!("write {} failed: {e}", dst.display()));
    println!(
        "wrote {} ({} bytes, {} sub-images)",
        dst.display(),
        ico.len(),
        SIZES.len()
    );
}
