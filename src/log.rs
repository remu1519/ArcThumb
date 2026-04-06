//! Opt-in file-based logger.
//!
//! Shell extensions run inside `explorer.exe`, so `eprintln!` goes
//! nowhere useful. When enabled, this module appends plain text lines
//! to `%TEMP%\arcthumb.log`.
//!
//! **Enabled when**: either this is a debug build, *or* the
//! environment variable `ARCTHUMB_LOG` is set (to anything).
//!
//! Release builds are silent by default — users who want diagnostics
//! set `ARCTHUMB_LOG=1` (system-wide or for the Explorer process)
//! and restart Explorer.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::OnceLock;

use crate::limits;

/// Decide once whether logging is on, cache the result.
fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        cfg!(debug_assertions) || std::env::var_os("ARCTHUMB_LOG").is_some()
    })
}

pub fn log(msg: &str) {
    if !enabled() {
        return;
    }

    let path = std::env::temp_dir().join("arcthumb.log");

    // Truncate-if-oversized: if a forgotten debug session has let
    // the log grow past the cap, blow it away before appending.
    if let Ok(metadata) = std::fs::metadata(&path) {
        if metadata.len() > limits::MAX_LOG_FILE_SIZE {
            let _ = std::fs::remove_file(&path);
        }
    }

    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{msg}");
    }
}

#[macro_export]
macro_rules! alog {
    ($($arg:tt)*) => {{
        $crate::log::log(&format!($($arg)*));
    }};
}
