//! Opening a URL in the OS's own default browser — see
//! [`crate::os::windows::open_url`] for the real implementation. Unlike a
//! file dialog, the underlying OS call doesn't block on anything the
//! browser does (it just asks the shell to launch a handler and returns),
//! so this is synchronous — no spawn/callback round trip needed. Apps
//! never call [`open_url`] directly — see [`crate::WindowControls::open_url`]
//! for the public entry point.

/// A real, unexpected failure's message is for diagnostics only, never
/// treated as success — matches [`crate::OpenFileDialogOutcome`]'s own
/// `Failed` convention.
#[derive(Debug, Clone, PartialEq)]
pub enum OpenUrlOutcome {
    Opened,
    /// No equivalent mechanism exists on this platform.
    Unavailable,
    Failed(String),
}

#[cfg(target_os = "windows")]
pub(crate) use crate::os::windows::open_url::open_url;

#[cfg(not(target_os = "windows"))]
pub(crate) use stub::open_url;

#[cfg(not(target_os = "windows"))]
mod stub {
    use winit::window::Window;

    use super::OpenUrlOutcome;

    pub(crate) fn open_url(_window: &Window, _url: &str) -> OpenUrlOutcome {
        OpenUrlOutcome::Unavailable
    }
}
