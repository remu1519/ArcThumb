//! Hard safety limits.
//!
//! Shell extensions run inside `explorer.exe` — any OOM, hang, or
//! panic here crashes Windows Explorer. These constants cap the
//! worst-case resource usage of a single thumbnail request so that
//! a malicious or malformed archive can't take the user's desktop
//! down with it.

/// Maximum archive size we're willing to process. Archives larger
/// than this are rejected outright, before any parsing happens.
pub const MAX_ARCHIVE_SIZE: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB

/// Maximum per-entry (compressed *or* uncompressed) size for files
/// considered as thumbnail candidates. Larger entries are skipped
/// during listing — we'd rather pick a smaller sibling image than
/// spend a minute decoding a 1 GB TIFF.
pub const MAX_ENTRY_SIZE: u64 = 500 * 1024 * 1024; // 500 MiB

/// Maximum decoded image dimension (width or height, in pixels).
/// Enforced via `image::Limits` before full decode.
pub const MAX_IMAGE_DIMENSION: u32 = 32_768;

/// Maximum bytes the image decoder is allowed to allocate. Defends
/// against "decompression bomb" images (tiny compressed source that
/// expands to gigabytes of pixel data).
pub const MAX_IMAGE_ALLOC: u64 = 512 * 1024 * 1024; // 512 MiB

/// Orphaned temp files (left over from the RAR backend if Explorer
/// was killed mid-extraction) older than this are deleted on the
/// next RAR thumbnail request.
pub const TEMP_FILE_MAX_AGE_SECS: u64 = 3600; // 1 hour

/// Minimum thumbnail side we'll produce. Explorer will never ask for
/// less than 16, but we clamp defensively.
pub const MIN_THUMBNAIL_SIZE: u32 = 16;

/// Maximum thumbnail side. 2560 is Windows's largest standard icon
/// bucket (Extra Large Icons × high DPI).
pub const MAX_THUMBNAIL_SIZE: u32 = 2560;

/// Maximum size of the debug log file at `%TEMP%\arcthumb.log`.
/// When the file exceeds this, it gets truncated on the next write
/// so a forgotten debug session doesn't fill the disk.
pub const MAX_LOG_FILE_SIZE: u64 = 1024 * 1024; // 1 MiB
