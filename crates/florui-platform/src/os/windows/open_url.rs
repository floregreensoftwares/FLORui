//! Opens a URL in the OS's own default browser via `ShellExecuteW` — see
//! `crate::open_url`'s own doc for why this is synchronous, unlike the
//! file dialogs next to it.

use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::HSTRING;
use winit::window::Window;

use crate::open_url::OpenUrlOutcome;
use crate::os::windows::raw_hwnd;

pub(crate) fn open_url(window: &Window, url: &str) -> OpenUrlOutcome {
    let hwnd = raw_hwnd(window);
    let operation = HSTRING::from("open");
    let file = HSTRING::from(url);
    // SAFETY: asks the shell to launch a handler for `file`; `operation`
    // and `file` are live `HSTRING`s for the whole call, and
    // `ShellExecuteW` doesn't retain either pointer past it.
    let result = unsafe { ShellExecuteW(hwnd, &operation, &file, None, None, SW_SHOWNORMAL) };
    // `ShellExecute`'s own decades-old contract: a return value > 32
    // means success -- the type is a repurposed `HINSTANCE`, never a real
    // handle, so anything <= 32 is an error code instead.
    let code = result.0 as isize;
    if code > 32 {
        OpenUrlOutcome::Opened
    } else {
        OpenUrlOutcome::Failed(format!("ShellExecuteW returned {code}"))
    }
}
