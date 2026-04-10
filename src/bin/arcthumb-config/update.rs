//! Background update checker and donation prompt logic.
//!
//! On every GUI launch a background thread hits the GitHub API to see
//! whether a newer release exists. Results and throttle state live in
//! `HKCU\Software\ArcThumb`. All errors are swallowed — the update
//! check is purely opportunistic and must never block or annoy the
//! user.

use std::os::windows::process::CommandExt;
use std::time::{SystemTime, UNIX_EPOCH};

use winreg::RegKey;
use winreg::enums::*;

/// Current version baked in at compile time from `Cargo.toml`.
/// Can be overridden at runtime via `ARCTHUMB_FAKE_VERSION` for
/// testing the update / donation dialogs (e.g. `set ARCTHUMB_FAKE_VERSION=0.0.1`).
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

fn effective_version() -> &'static str {
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            std::env::var("ARCTHUMB_FAKE_VERSION").unwrap_or_else(|_| CURRENT_VERSION.to_string())
        })
        .as_str()
}

const GITHUB_API_URL: &str = "https://api.github.com/repos/citrussoda-com/ArcThumb/releases/latest";

/// Download page opened by the update notification.
const DOWNLOAD_URL: &str = "https://citrussoda.com/arcthumb";

/// Sponsor page opened by the donation prompt.
const SPONSOR_URL: &str = "https://github.com/sponsors/citrussoda-com";

/// Minimum interval between update checks (seconds).
const CHECK_INTERVAL_SECS: u64 = 86_400; // 24 hours

/// After this many donation-prompt dismissals we stop showing it.
const MAX_DONATION_SKIPS: u32 = 3;

// ── public types ─────────────────────────────────────────────────

/// Result of a successful update check where a newer version exists.
pub struct UpdateInfo {
    pub latest_version: String,
    pub release_url: String,
}

// ── version helpers ──────────────────────────────────────────────

fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut parts = s.splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

// ── registry helpers ─────────────────────────────────────────────

fn open_key() -> Option<RegKey> {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Software\\ArcThumb")
        .ok()
}

fn open_or_create_key() -> Option<RegKey> {
    RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey("Software\\ArcThumb")
        .ok()
        .map(|(k, _)| k)
}

fn read_dword(key: &RegKey, name: &str) -> Option<u32> {
    key.get_value::<u32, _>(name).ok()
}

fn read_qword(key: &RegKey, name: &str) -> Option<u64> {
    key.get_value::<u64, _>(name).ok()
}

fn read_string(key: &RegKey, name: &str) -> Option<String> {
    key.get_value::<String, _>(name).ok()
}

// ── update check throttle ────────────────────────────────────────

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Returns `true` if enough time has elapsed since the last check
/// and the user hasn't disabled auto-checking.
pub fn should_check_now() -> bool {
    let Some(key) = open_key() else {
        return true; // no key yet → first run, check
    };
    // UpdateCheckEnabled: absent or 1 = enabled, 0 = disabled
    if read_dword(&key, "UpdateCheckEnabled") == Some(0) {
        return false;
    }
    let last = read_qword(&key, "LastUpdateCheck").unwrap_or(0);
    now_unix_secs().saturating_sub(last) >= CHECK_INTERVAL_SECS
}

// ── GitHub API call ──────────────────────────────────────────────

/// Hit the GitHub releases API and return info about the latest
/// release if it is newer than the running version. Writes
/// `LastUpdateCheck` and `LastSeenVersion` to the registry on
/// success.
pub fn check_for_update() -> Option<UpdateInfo> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .new_agent();

    let body = agent
        .get(GITHUB_API_URL)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", &format!("ArcThumb/{CURRENT_VERSION}"))
        .call()
        .ok()?
        .body_mut()
        .read_to_string()
        .ok()?;

    let tag = extract_json_string(&body, "tag_name")?;

    // Record the check timestamp regardless of whether a new version
    // exists, so we don't hammer the API on every launch.
    if let Some(key) = open_or_create_key() {
        let _ = key.set_value("LastUpdateCheck", &now_unix_secs());
        let version = tag.strip_prefix('v').unwrap_or(&tag);
        let _ = key.set_value("LastSeenVersion", &version.to_string());
    }

    let version = tag.strip_prefix('v').unwrap_or(&tag).to_string();
    if is_newer(&version, effective_version()) {
        Some(UpdateInfo {
            latest_version: version,
            release_url: DOWNLOAD_URL.to_string(),
        })
    } else {
        None
    }
}

// ── skip version ─────────────────────────────────────────────────

pub fn is_version_skipped(version: &str) -> bool {
    open_key()
        .and_then(|k| read_string(&k, "SkippedVersion"))
        .map(|v| v == version)
        .unwrap_or(false)
}

pub fn skip_version(version: &str) {
    if let Some(key) = open_or_create_key() {
        let _ = key.set_value("SkippedVersion", &version.to_string());
    }
}

// ── donation prompt ──────────────────────────────────────────────

/// If the user just updated (current version > LastSeenVersion),
/// returns the current version string for the donation dialog.
/// Returns `None` if the prompt should be suppressed.
pub fn should_show_donation() -> Option<String> {
    let key = open_key()?;

    // Permanently dismissed?
    if read_dword(&key, "DonationDismissed") == Some(1) {
        return None;
    }
    // Too many skips?
    if read_dword(&key, "DonationSkipCount").unwrap_or(0) >= MAX_DONATION_SKIPS {
        return None;
    }

    let last_seen = read_string(&key, "LastSeenVersion")?;
    // Show prompt only when the running binary is newer than what
    // the registry last recorded — i.e. the user just installed an
    // update.
    if is_newer(effective_version(), &last_seen) {
        Some(effective_version().to_string())
    } else {
        None
    }
}

/// Write the current version into `LastSeenVersion` so the donation
/// prompt won't fire again until the next real update.
pub fn record_donation_shown() {
    if let Some(key) = open_or_create_key() {
        let _ = key.set_value("LastSeenVersion", &effective_version().to_string());
    }
}

pub fn record_donation_skip() {
    if let Some(key) = open_or_create_key() {
        let count = read_dword(&key, "DonationSkipCount").unwrap_or(0);
        let _ = key.set_value("DonationSkipCount", &(count + 1));
    }
}

pub fn dismiss_donation() {
    if let Some(key) = open_or_create_key() {
        let _ = key.set_value("DonationDismissed", &1u32);
    }
}

pub fn sponsor_url() -> &'static str {
    SPONSOR_URL
}

// ── open URL in default browser ──────────────────────────────────

pub fn open_url(url: &str) {
    // CREATE_NO_WINDOW (0x0800_0000) prevents a visible cmd flash.
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", "", url])
        .creation_flags(0x0800_0000)
        .spawn();
}

// ── minimal JSON string extractor ────────────────────────────────

/// Pull the first string value for `key` out of a flat JSON object.
/// Good enough for GitHub API responses where values don't contain
/// escaped quotes.
fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let pos = json.find(&needle)? + needle.len();
    let rest = &json[pos..];
    let start = rest.find('"')? + 1;
    let rest = &rest[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

// ── current version accessor ─────────────────────────────────────

pub fn current_version() -> &'static str {
    effective_version()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_basic() {
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("v0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("10.20.30"), Some((10, 20, 30)));
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("abc"), None);
    }

    #[test]
    fn is_newer_works() {
        assert!(is_newer("0.3.0", "0.2.0"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn extract_json_string_basic() {
        let json = r#"{"tag_name":"v0.3.0","html_url":"https://example.com/release"}"#;
        assert_eq!(
            extract_json_string(json, "tag_name"),
            Some("v0.3.0".to_string())
        );
        assert_eq!(
            extract_json_string(json, "html_url"),
            Some("https://example.com/release".to_string())
        );
        assert_eq!(extract_json_string(json, "missing"), None);
    }

    #[test]
    fn extract_json_string_with_spaces() {
        let json = r#"{ "tag_name" : "v1.0.0" }"#;
        assert_eq!(
            extract_json_string(json, "tag_name"),
            Some("v1.0.0".to_string())
        );
    }
}
