//! Window class registration, window procedure, and paint logic for
//! the preview handler's child window.

use std::mem::size_of;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, COLOR_WINDOW, CreateCompatibleDC, CreateSolidBrush, DeleteDC, DeleteObject,
    EndPaint, FillRect, GetSysColor, HBITMAP, HBRUSH, HGDIOBJ, PAINTSTRUCT, SRCCOPY,
    STRETCH_HALFTONE, SelectObject, SetStretchBltMode, StretchBlt,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, DefWindowProcW, GWLP_USERDATA, GetClientRect,
    GetWindowLongPtrW, IDC_ARROW, KillTimer, LoadCursorW, RegisterClassExW, SetTimer,
    SetWindowLongPtrW, WM_DESTROY, WM_ERASEBKGND, WM_NCCREATE, WM_PAINT, WM_TIMER, WNDCLASSEXW,
};
use windows::core::{PCWSTR, w};

use crate::bitmap;

use super::ArcThumbPreviewHandler;

/// Owned HBITMAP wrapper that frees the GDI handle on Drop.
pub(crate) struct CachedBitmap {
    pub width: i32,
    pub height: i32,
    pub hbitmap: HBITMAP,
}

impl Drop for CachedBitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.hbitmap.0));
        }
    }
}

// =============================================================================
// Window class registration (one per process)
// =============================================================================

pub(super) fn register_window_class() -> u16 {
    static ATOM: OnceLock<u16> = OnceLock::new();
    *ATOM.get_or_init(|| {
        let hmodule = unsafe { GetModuleHandleW(None).unwrap_or_default() };
        let hinstance = HINSTANCE(hmodule.0);
        let cursor = unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() };
        let wcex = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(preview_wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: Default::default(),
            hCursor: cursor,
            hbrBackground: HBRUSH::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: w!("ArcThumbPreviewWindow"),
            hIconSm: Default::default(),
        };
        unsafe { RegisterClassExW(&wcex) }
    })
}

// =============================================================================
// Window procedure + paint
// =============================================================================

/// Timer ID for the resize debounce timer.
const DEBOUNCE_TIMER_ID: usize = 1;
/// Delay in milliseconds before committing a resize after the last
/// WM_PAINT with a changed size. Short enough to feel responsive,
/// long enough to skip intermediate frames during a drag.
const DEBOUNCE_DELAY_MS: u32 = 80;

unsafe extern "system" fn preview_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match msg {
            WM_NCCREATE => {
                // Stash the user pointer we passed via lpCreateParams.
                let cs = lparam.0 as *const CREATESTRUCTW;
                if !cs.is_null() {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_PAINT => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const ArcThumbPreviewHandler;
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    if !ptr.is_null() {
                        paint(hwnd, &*ptr, false);
                    } else {
                        paint_empty(hwnd);
                    }
                }));
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == DEBOUNCE_TIMER_ID => {
                let _ = KillTimer(hwnd, DEBOUNCE_TIMER_ID);
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const ArcThumbPreviewHandler;
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    if !ptr.is_null() {
                        paint(hwnd, &*ptr, true);
                    }
                }));
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1), // we erase in WM_PAINT
            WM_DESTROY => {
                let _ = KillTimer(hwnd, DEBOUNCE_TIMER_ID);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Build a brush for the system window-background colour. Caller
/// must `DeleteObject` it after use. We can't use the standard
/// `(COLOR_WINDOW + 1)` HBRUSH trick portably across windows-rs
/// 0.58 — `CreateSolidBrush` is more obviously correct.
fn system_window_brush() -> HBRUSH {
    let color = unsafe { GetSysColor(COLOR_WINDOW) };
    unsafe { CreateSolidBrush(COLORREF(color)) }
}

/// Paint with no user state — clear to the system window colour.
fn paint_empty(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    let mut rc = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut rc) };
    let brush = system_window_brush();
    unsafe { FillRect(hdc, &rc, brush) };
    unsafe {
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
    let _ = unsafe { EndPaint(hwnd, &ps) };
}

/// Paint the preview image. When `commit` is true (fired by
/// WM_TIMER), build a pixel-perfect bitmap at the current size.
/// When false (WM_PAINT during drag-resize), either reuse the
/// cache if size matches, or stretch the stale cache and schedule
/// a debounced rebuild.
fn paint(hwnd: HWND, this: &ArcThumbPreviewHandler, commit: bool) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };

    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    let cw = client.right - client.left;
    let ch = client.bottom - client.top;

    // Erase background.
    let brush = system_window_brush();
    unsafe { FillRect(hdc, &client, brush) };
    unsafe {
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }

    let source = this.source.borrow();
    if let Some(img) = source.as_ref() {
        let (dest_w, dest_h, off_x, off_y) = fit_inside(img.width(), img.height(), cw, ch);
        if dest_w > 0 && dest_h > 0 {
            let mut cache = this.cache.borrow_mut();
            let size_matches = cache
                .as_ref()
                .is_some_and(|c| c.width == dest_w && c.height == dest_h);

            if size_matches {
                // Exact match — blit directly, no timer needed.
                blit_cached(hdc, cache.as_ref().unwrap(), off_x, off_y);
            } else if commit {
                // Timer fired — build the pixel-perfect bitmap now.
                let resized = img
                    .resize_exact(
                        dest_w as u32,
                        dest_h as u32,
                        image::imageops::FilterType::Triangle,
                    )
                    .to_rgba8();
                if let Ok(hbmp) = bitmap::from_rgba(&resized) {
                    *cache = Some(CachedBitmap {
                        width: dest_w,
                        height: dest_h,
                        hbitmap: hbmp,
                    });
                    blit_cached(hdc, cache.as_ref().unwrap(), off_x, off_y);
                }
            } else {
                // Mid-drag — stretch the stale cache as a placeholder
                // and schedule a debounced rebuild.
                if let Some(c) = cache.as_ref() {
                    stretch_cached(hdc, c, off_x, off_y, dest_w, dest_h);
                }
                unsafe {
                    let _ = SetTimer(hwnd, DEBOUNCE_TIMER_ID, DEBOUNCE_DELAY_MS, None);
                }
            }
        }
    }

    let _ = unsafe { EndPaint(hwnd, &ps) };
}

/// BitBlt a cached bitmap at its native size.
fn blit_cached(hdc: windows::Win32::Graphics::Gdi::HDC, c: &CachedBitmap, x: i32, y: i32) {
    unsafe {
        let mem_dc = CreateCompatibleDC(hdc);
        let old = SelectObject(mem_dc, HGDIOBJ(c.hbitmap.0));
        let _ = BitBlt(hdc, x, y, c.width, c.height, mem_dc, 0, 0, SRCCOPY);
        SelectObject(mem_dc, old);
        let _ = DeleteDC(mem_dc);
    }
}

/// StretchBlt a cached bitmap to a different size (fast placeholder
/// during drag-resize).
fn stretch_cached(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    c: &CachedBitmap,
    x: i32,
    y: i32,
    dest_w: i32,
    dest_h: i32,
) {
    unsafe {
        let mem_dc = CreateCompatibleDC(hdc);
        let old = SelectObject(mem_dc, HGDIOBJ(c.hbitmap.0));
        let _ = SetStretchBltMode(hdc, STRETCH_HALFTONE);
        let _ = StretchBlt(
            hdc, x, y, dest_w, dest_h, mem_dc, 0, 0, c.width, c.height, SRCCOPY,
        );
        SelectObject(mem_dc, old);
        let _ = DeleteDC(mem_dc);
    }
}

/// Aspect-fit `(src_w, src_h)` inside a `(box_w, box_h)` rectangle,
/// returning `(dest_w, dest_h, x_offset, y_offset)` for centering.
/// Pure function — easy to unit test.
pub(super) fn fit_inside(src_w: u32, src_h: u32, box_w: i32, box_h: i32) -> (i32, i32, i32, i32) {
    if src_w == 0 || src_h == 0 || box_w <= 0 || box_h <= 0 {
        return (0, 0, 0, 0);
    }
    let scale_x = box_w as f64 / src_w as f64;
    let scale_y = box_h as f64 / src_h as f64;
    let scale = scale_x.min(scale_y);
    let dest_w = (src_w as f64 * scale).round() as i32;
    let dest_h = (src_h as f64 * scale).round() as i32;
    let off_x = (box_w - dest_w) / 2;
    let off_y = (box_h - dest_h) / 2;
    (dest_w, dest_h, off_x, off_y)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_inside_square_in_square() {
        // 100×100 inside 200×200 → scaled to 200×200, no offset.
        assert_eq!(fit_inside(100, 100, 200, 200), (200, 200, 0, 0));
    }

    #[test]
    fn fit_inside_landscape_in_square() {
        // 100×50 → fills width, top/bottom letterboxed.
        assert_eq!(fit_inside(100, 50, 200, 200), (200, 100, 0, 50));
    }

    #[test]
    fn fit_inside_portrait_in_square() {
        // 50×100 → fills height, left/right pillarboxed.
        assert_eq!(fit_inside(50, 100, 200, 200), (100, 200, 50, 0));
    }

    #[test]
    fn fit_inside_smaller_source_still_scales_up() {
        // 40×20 inside 200×200 → scale=5x → 200×100, offset y=50.
        assert_eq!(fit_inside(40, 20, 200, 200), (200, 100, 0, 50));
    }

    #[test]
    fn fit_inside_zero_source() {
        assert_eq!(fit_inside(0, 100, 200, 200), (0, 0, 0, 0));
        assert_eq!(fit_inside(100, 0, 200, 200), (0, 0, 0, 0));
    }

    #[test]
    fn fit_inside_zero_box() {
        assert_eq!(fit_inside(100, 100, 0, 200), (0, 0, 0, 0));
        assert_eq!(fit_inside(100, 100, 200, 0), (0, 0, 0, 0));
        assert_eq!(fit_inside(100, 100, -1, 200), (0, 0, 0, 0));
    }

    #[test]
    fn fit_inside_non_square_box() {
        // 100×100 inside 400×200 → constrained by height, → 200×200,
        // centered horizontally.
        assert_eq!(fit_inside(100, 100, 400, 200), (200, 200, 100, 0));
    }

    #[test]
    fn fit_inside_centers_when_aspect_matches() {
        // 100×50 inside 200×100 → exact fit.
        assert_eq!(fit_inside(100, 50, 200, 100), (200, 100, 0, 0));
    }
}
