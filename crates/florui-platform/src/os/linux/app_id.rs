//! The name a window carries so the desktop can match it to the desktop
//! entry `florui register` wrote (`StartupWMClass` on X11, the entry's name
//! on Wayland): that is what gives the window the application's icon in the
//! dock and the task switcher.

use winit::platform::wayland::WindowAttributesExtWayland;
use winit::platform::x11::WindowAttributesExtX11;
use winit::window::WindowAttributes;

pub(crate) fn with_app_id(attributes: WindowAttributes, identifier: &str) -> WindowAttributes {
    let attributes = WindowAttributesExtWayland::with_name(attributes, identifier, identifier);
    WindowAttributesExtX11::with_name(attributes, identifier, identifier)
}
