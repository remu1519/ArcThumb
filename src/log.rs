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
use std::path::Path;
use std::sync::OnceLock;

use crate::limits;

/// Decide once whether logging is on, cache the result.
fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        cfg!(debug_assertions) || std::env::var_os("ARCTHUMB_LOG").is_some()
    })
}

/// Append `msg` (with newline) to the log file at `path`. Truncates
/// the file first if it has grown past `MAX_LOG_FILE_SIZE`.
///
/// Always silent on I/O failure — logging must never break the
/// thumbnail pipeline.
fn log_to(path: &Path, msg: &str) {
    // Truncate-if-oversized: if a forgotten debug session has let
    // the log grow past the cap, blow it away before appending.
    if let Ok(metadata) = std::fs::metadata(path) {
        if metadata.len() > limits::MAX_LOG_FILE_SIZE {
            let _ = std::fs::remove_file(path);
        }
    }

    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{msg}");
    }
}

pub fn log(msg: &str) {
    if !enabled() {
        return;
    }
    let path = std::env::temp_dir().join("arcthumb.log");
    log_to(&path, msg);
}

#[macro_export]
macro_rules! alog {
    ($($arg:tt)*) => {{
        $crate::log::log(&format!($($arg)*));
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Build a unique log path under the OS temp dir. We can't share
    /// `arcthumb.log` between tests because they may run in parallel.
    fn unique_log_path(tag: &str) -> std::path::PathBuf {
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("arcthumb_test_{tag}_{pid}_{nanos}.log"))
    }

    /// RAII cleanup so a panicking test doesn't litter %TEMP%.
    struct LogGuard(std::path::PathBuf);
    impl Drop for LogGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn log_to_creates_file_and_appends() {
        let path = unique_log_path("create");
        let _g = LogGuard(path.clone());
        assert!(!path.exists());

        log_to(&path, "first line");
        log_to(&path, "second line");

        let body = std::fs::read_to_string(&path).expect("read log");
        assert_eq!(body, "first line\nsecond line\n");
    }

    #[test]
    fn log_to_truncates_oversized_file() {
        let path = unique_log_path("truncate");
        let _g = LogGuard(path.clone());

        // Pre-fill the file past the size cap so the next log call
        // is forced to delete-and-recreate.
        let big = vec![b'X'; (limits::MAX_LOG_FILE_SIZE + 1024) as usize];
        std::fs::write(&path, &big).expect("write big");
        assert!(std::fs::metadata(&path).unwrap().len() > limits::MAX_LOG_FILE_SIZE);

        log_to(&path, "fresh");

        // After truncation only the new line should remain.
        let body = std::fs::read_to_string(&path).expect("read log");
        assert_eq!(body, "fresh\n");
    }

    #[test]
    fn log_to_handles_unwritable_parent_silently() {
        // A path inside a directory that cannot exist must NOT panic
        // and must NOT propagate an error. The thumbnail pipeline
        // calls `alog!` from hot paths and a logging failure must
        // never abort it.
        let bad = std::path::PathBuf::from(
            r"Z:\definitely\not\a\real\directory\arcthumb_test.log",
        );
        log_to(&bad, "this should be silently dropped");
        assert!(!bad.exists());
    }

    #[test]
    fn log_to_handles_empty_message() {
        let path = unique_log_path("empty");
        let _g = LogGuard(path.clone());
        log_to(&path, "");
        let body = std::fs::read_to_string(&path).expect("read log");
        assert_eq!(body, "\n");
    }

    #[test]
    fn log_to_appends_across_calls() {
        let path = unique_log_path("append");
        let _g = LogGuard(path.clone());
        for i in 0..5 {
            log_to(&path, &format!("line {i}"));
        }
        let body = std::fs::read_to_string(&path).expect("read log");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0], "line 0");
        assert_eq!(lines[4], "line 4");
    }

    #[test]
    fn log_macro_compiles_and_does_not_panic() {
        // We can't easily verify the side-effect (the gated path
        // depends on env at test start) but we can at least ensure
        // the macro expands and runs without exploding.
        crate::alog!("hello {}", "world");
    }
}
