//! A system tray icon — see [`crate::os::windows::tray`] and
//! `crate::os::linux::tray` for the real implementations.
//! `TrayIcon::new` returns `None` elsewhere, and where nothing hosts tray
//! icons (a GNOME session without the AppIndicator extension).

#[cfg(target_os = "windows")]
pub use crate::os::windows::tray::{TrayEvent, TrayIcon};

#[cfg(target_os = "linux")]
pub use crate::os::linux::tray::{TrayEvent, TrayIcon};

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub use stub::{TrayEvent, TrayIcon};

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
mod stub {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum TrayEvent {
        LeftClick,
        RightClick,
    }

    pub struct TrayIcon(());

    impl TrayIcon {
        pub fn new(_tooltip: &str, _on_event: impl Fn(TrayEvent) + 'static) -> Option<Self> {
            None
        }
    }
}
