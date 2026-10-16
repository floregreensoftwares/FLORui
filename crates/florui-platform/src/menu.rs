//! Native right-click context menus — see
//! [`crate::os::windows::menu`] for the real implementation. Apps never
//! call [`show_context_menu`] directly — see
//! [`crate::WindowControls::show_context_menu`] for the public entry
//! point.
//!
//! On Windows this is a real OS-drawn popup (`TrackPopupMenuEx`); on Linux no
//! toolkit-free popup exists, so the menu is a small Florui tree painted in a
//! popup surface of its own (see `crate::menu_popup` and
//! `crate::os::linux::menu`) that follows the light or dark theme. Either
//! way labels, enabled/checked state, and a display-only shortcut hint are the
//! whole surface. There is no way to style it — no CSS, no fonts, no colors.
//! An app that needs a menu of its own design wants a `Popover`.
//!
//! `shortcut` is display text only — this does not register a real OS
//! keyboard accelerator, so choosing that text does not make the shortcut
//! actually work. Preventing duplicate dispatch (the same command firing
//! once from a menu click and again from the app's own keyboard handling)
//! is the app's own responsibility: reuse one [`MenuCommandId`] for both,
//! rather than expecting this module to deduplicate anything.

/// A stable, caller-assigned identity for one menu command — reused across
/// [`crate::WindowControls::show_context_menu`] calls so the app can match
/// a [`ContextMenuOutcome::Selected`] back to the action it means.
///
/// `0` is reserved: `TrackPopupMenuEx` returns `0` both when the menu
/// closes with no selection and, indistinguishably per its own contract,
/// on a real call failure — see [`ContextMenuOutcome::Dismissed`]. Never
/// assign `0` to a real [`MenuEntry::Action`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MenuCommandId(pub u32);

/// One row of a [`crate::WindowControls::show_context_menu`] menu, built
/// fresh from the caller's current snapshot on every call — there is no
/// persistent OS menu object to mutate between calls, so `enabled`/
/// `checked` are read at show time, not tracked afterward.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuEntry {
    Action {
        id: MenuCommandId,
        label: String,
        enabled: bool,
        checked: bool,
        /// Display text only (e.g. `"Ctrl+S"`) — see this module's own
        /// doc for why this does not register a real accelerator.
        shortcut: Option<String>,
    },
    Separator,
}

/// What [`crate::WindowControls::show_context_menu`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMenuOutcome {
    Selected(MenuCommandId),
    /// Escape, a click outside the menu, or (see [`MenuCommandId`]'s own
    /// doc) an indistinguishable real failure.
    Dismissed,
    /// No equivalent mechanism exists on this platform, or no real
    /// platform window handle was available.
    Unavailable,
}

/// Joins `label` and `shortcut` into the `"{label}\t{shortcut}"` pattern a
/// Win32 menu item expects — the tab character is what makes the shortcut
/// text render right-aligned; no custom layout/measuring needed. Pure
/// string building, kept separate from the real menu's HMENU/wide-string
/// plumbing so it's testable without any real OS type.
#[cfg(any(windows, test))]
pub(crate) fn menu_label(label: &str, shortcut: Option<&str>) -> String {
    match shortcut {
        Some(shortcut) => format!("{label}\t{shortcut}"),
        None => label.to_owned(),
    }
}

#[cfg(target_os = "windows")]
pub(crate) use crate::os::windows::menu::show_context_menu;
#[cfg(target_os = "windows")]
pub(crate) use crate::os::windows::menu::{show_system_menu_at, show_system_menu_at_cursor};

#[cfg(target_os = "linux")]
pub(crate) use crate::os::linux::menu::{show_system_menu_at, show_system_menu_at_cursor};

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub(crate) use stub::{show_system_menu_at, show_system_menu_at_cursor};

/// The blocking menu: a Win32 popup menu pumps the application's messages while
/// it is up. Nothing else can, without stopping the event loop and so leaving
/// the window manager's pings unanswered, so there is none elsewhere; see
/// [`show_context_menu_with`].
#[cfg(not(target_os = "windows"))]
pub(crate) fn show_context_menu(
    _window: &winit::window::Window,
    _items: &[MenuEntry],
) -> ContextMenuOutcome {
    ContextMenuOutcome::Unavailable
}

/// Shows the menu and gives what comes of it to `on_outcome`: at once on
/// Windows, once the menu is chosen from or dismissed on Linux, where the event
/// loop keeps running while it is open.
pub(crate) fn show_context_menu_with(
    window: &winit::window::Window,
    items: &[MenuEntry],
    on_outcome: Box<dyn Fn(ContextMenuOutcome)>,
) {
    #[cfg(target_os = "linux")]
    crate::os::linux::menu::show_context_menu_with(window, items, on_outcome);
    #[cfg(not(target_os = "linux"))]
    on_outcome(show_context_menu(window, items));
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
mod stub {
    use winit::window::Window;

    /// No system menu exists as a distinct OS concept on these platforms.
    pub(crate) fn show_system_menu_at_cursor(_window: &Window) {}

    /// See [`show_system_menu_at_cursor`]'s own doc.
    pub(crate) fn show_system_menu_at(_window: &Window, _screen_x: i32, _screen_y: i32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_with_no_shortcut_is_unchanged() {
        assert_eq!(menu_label("Save", None), "Save");
    }

    #[test]
    fn a_label_with_a_shortcut_gets_a_tab_separated_suffix() {
        assert_eq!(menu_label("Save", Some("Ctrl+S")), "Save\tCtrl+S");
    }
}
