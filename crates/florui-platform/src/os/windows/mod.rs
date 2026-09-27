pub mod accessibility;
pub mod caption;
pub(crate) mod drag_drop;
pub(crate) mod file_dialog;
pub(crate) mod gpu;
pub(crate) mod input_regions;
pub(crate) mod menu;
pub(crate) mod open_url;
pub mod overlay;
pub(crate) mod single_instance;
pub mod tray;

use std::ffi::c_void;

use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

/// The real platform window handle backing `window`, if any -- `None`
/// (never a panic) when `winit` reports anything other than a real Win32
/// handle. Shared by [`file_dialog`] and [`drag_drop`], both of which
/// need a `windows`-crate `HWND` (not `windows-sys`'s raw type alias,
/// used elsewhere in this module for non-COM calls).
pub(crate) fn raw_hwnd(window: &Window) -> Option<windows::Win32::Foundation::HWND> {
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return None;
    };
    Some(windows::Win32::Foundation::HWND(
        win32.hwnd.get() as *mut c_void
    ))
}
