//! Thumbnail / icon cache wipe — the second-stage safety net behind
//! `SHChangeNotify(SHCNE_ASSOCCHANGED)`.
//!
//! `SHChangeNotify` tells the Shell to drop *most* of its icon and
//! thumbnail cache, but in practice Explorer can still hold on to a
//! "this file has no thumbnail" negative cache entry for archives the
//! user opened *before* installing ArcThumb. The only reliable cure
//! is to delete `thumbcache_*.db` / `iconcache_*.db` from disk and
//! restart Explorer so it rebuilds them on demand.
//!
//! Surfaced in the GUI as the "Regenerate thumbnails" button. Not
//! exposed via the CLI yet — adding it there is trivial if a future
//! installer or scripted-deploy use case ever needs it.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;

/// `CREATE_NO_WINDOW` from `winbase.h`. Without this flag the
/// `taskkill` invocation flashes a black console window in front of
/// the user — annoying for a button that's supposed to feel like a
/// native control.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Outcome of a cache wipe attempt. The GUI surfaces `failed` so the
/// user knows whether to retry — an empty `failed` means every cache
/// db was deleted (or none were present to begin with, which is also
/// a successful outcome).
pub struct WipeReport {
    pub failed: Vec<PathBuf>,
}

/// Kill `explorer.exe`, delete every `thumbcache_*.db` and
/// `iconcache_*.db` under `%LOCALAPPDATA%\Microsoft\Windows\Explorer`,
/// then relaunch `explorer.exe`.
///
/// Best effort throughout. Individual delete failures are reported in
/// `WipeReport.failed` instead of aborting the wipe; if explorer was
/// already not running, that is fine; if relaunching explorer fails,
/// Windows usually starts it again on its own at next logon.
///
/// Returns `Err` only when the wipe could not even start — i.e. the
/// `LOCALAPPDATA` environment variable is missing or the Explorer
/// directory does not exist on disk.
pub fn wipe_thumbnail_cache() -> Result<WipeReport, String> {
    let local_appdata = std::env::var("LOCALAPPDATA")
        .map_err(|_| "LOCALAPPDATA environment variable is not set".to_string())?;
    let explorer_dir = PathBuf::from(local_appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Explorer");
    if !explorer_dir.is_dir() {
        return Err(format!("{} does not exist", explorer_dir.display()));
    }

    // 1. Stop Explorer so it releases its handles on the cache files.
    //    `/F` forces termination, `/IM explorer.exe` matches every
    //    running instance. We ignore the exit status: if Explorer was
    //    not running for some reason, the rest of the wipe still
    //    makes sense.
    let _ = Command::new("taskkill")
        .args(["/F", "/IM", "explorer.exe"])
        .creation_flags(CREATE_NO_WINDOW)
        .status();

    // 2. Wait briefly so Windows actually releases the file handles.
    //    Without this pause, the very first delete attempt below
    //    almost always fails with "file is in use".
    thread::sleep(Duration::from_millis(400));

    // 3. Delete every cache db. Two passes: handles that were still
    //    held during the first pass usually clear after another wait.
    let mut failed: Vec<PathBuf> = Vec::new();
    for attempt in 0..2 {
        failed.clear();
        let entries = match std::fs::read_dir(&explorer_dir) {
            Ok(it) => it,
            Err(e) => return Err(format!("read_dir failed: {e}")),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_cache_file(&path) {
                continue;
            }
            if std::fs::remove_file(&path).is_err() {
                failed.push(path);
            }
        }
        if failed.is_empty() {
            break;
        }
        if attempt == 0 {
            thread::sleep(Duration::from_millis(400));
        }
    }

    // 4. Bring Explorer back. Without this the user is left staring
    //    at a black wallpaper — Windows does not auto-restart shells
    //    that were killed by `taskkill /F`.
    let _ = Command::new("explorer.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();

    Ok(WipeReport { failed })
}

/// True if `path`'s file name matches `thumbcache_*.db` or
/// `iconcache_*.db`. Pulled out so the unit test can exercise the
/// matcher without touching the real cache directory.
fn is_cache_file(path: &std::path::Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    (name.starts_with("thumbcache_") || name.starts_with("iconcache_")) && name.ends_with(".db")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn matches_thumbcache_dbs() {
        assert!(is_cache_file(Path::new("thumbcache_32.db")));
        assert!(is_cache_file(Path::new("thumbcache_1024.db")));
        assert!(is_cache_file(Path::new("thumbcache_idx.db")));
    }

    #[test]
    fn matches_iconcache_dbs() {
        assert!(is_cache_file(Path::new("iconcache_32.db")));
        assert!(is_cache_file(Path::new("iconcache_idx.db")));
    }

    #[test]
    fn rejects_unrelated_files() {
        assert!(!is_cache_file(Path::new("thumbcache.db"))); // no underscore
        assert!(!is_cache_file(Path::new("thumbcache_32.txt")));
        assert!(!is_cache_file(Path::new("explorer.log")));
        assert!(!is_cache_file(Path::new("ThumbsDB.db")));
    }

    #[test]
    fn matches_full_paths() {
        let p =
            Path::new(r"C:\Users\foo\AppData\Local\Microsoft\Windows\Explorer\thumbcache_96.db");
        assert!(is_cache_file(p));
    }
}
