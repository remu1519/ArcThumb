//! Native Win32 MessageBox wrappers.
//!
//! We use `MessageBoxW` directly rather than building custom Slint
//! modal windows for three reasons: it gives native look and sounds,
//! it integrates with the Windows focus/keyboard model, and the
//! donation prompt runs before the Slint event loop is up — a moment
//! where Slint's own windows can't be driven.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    IDNO, IDOK, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_OKCANCEL,
    MB_YESNOCANCEL, MessageBoxW,
};
use windows::core::PCWSTR;

pub enum DialogResult {
    Yes,
    No,
    Cancel,
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn hwnd_null() -> HWND {
    HWND(std::ptr::null_mut())
}

pub fn info(title: &str, content: &str) {
    let title_w = to_wide(title);
    let content_w = to_wide(content);
    unsafe {
        MessageBoxW(
            hwnd_null(),
            PCWSTR(content_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

pub fn error(title: &str, content: &str) {
    let title_w = to_wide(title);
    let content_w = to_wide(content);
    unsafe {
        MessageBoxW(
            hwnd_null(),
            PCWSTR(content_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

pub fn confirm_warning(title: &str, content: &str) -> bool {
    let title_w = to_wide(title);
    let content_w = to_wide(content);
    let result = unsafe {
        MessageBoxW(
            hwnd_null(),
            PCWSTR(content_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_OKCANCEL | MB_ICONWARNING,
        )
    };
    result == IDOK
}

pub fn yes_no_cancel(title: &str, content: &str) -> DialogResult {
    let title_w = to_wide(title);
    let content_w = to_wide(content);
    let result = unsafe {
        MessageBoxW(
            hwnd_null(),
            PCWSTR(content_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_YESNOCANCEL | MB_ICONINFORMATION,
        )
    };
    if result == IDYES {
        DialogResult::Yes
    } else if result == IDNO {
        DialogResult::No
    } else {
        DialogResult::Cancel
    }
}
