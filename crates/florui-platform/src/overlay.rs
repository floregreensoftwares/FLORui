//! Whole-window and selective click-through — see
//! [`crate::os::windows::overlay`] and `crate::os::windows::input_regions`
//! for Windows, and `crate::os::linux::overlay` for Linux (selective mode
//! on X11 only). A no-op on other systems.

use crate::window_controls::{InputMode, ScreenRect};

#[cfg(target_os = "windows")]
pub(crate) fn set_input_mode(window: &winit::window::Window, mode: InputMode) -> bool {
    crate::os::windows::overlay::set_input_mode(window, mode)
}

#[cfg(target_os = "linux")]
pub(crate) fn set_input_mode(window: &winit::window::Window, mode: InputMode) -> bool {
    crate::os::linux::overlay::set_input_mode(window, mode)
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub(crate) fn set_input_mode(_window: &winit::window::Window, _mode: InputMode) -> bool {
    false
}

#[cfg(target_os = "windows")]
pub(crate) fn sync_input_regions(window: &winit::window::Window, regions: &[ScreenRect]) {
    crate::os::windows::overlay::sync_input_regions(window, regions);
}

#[cfg(target_os = "linux")]
pub(crate) fn sync_input_regions(window: &winit::window::Window, regions: &[ScreenRect]) {
    crate::os::linux::overlay::sync_input_regions(window, regions);
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub(crate) fn sync_input_regions(_window: &winit::window::Window, _regions: &[ScreenRect]) {}
