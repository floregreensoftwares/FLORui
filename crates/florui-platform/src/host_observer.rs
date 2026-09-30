//! Lets a development tool watch windows run by the desktop host without the
//! host knowing what the tool is. The host offers events and finished frames
//! and never hands out anything mutable, so a window behaves the same with or
//! without an observer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use florui_layout::BoxLayout;
use florui_style::{Arena, ComputedStyle, NodeId};
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId};

/// A window's laid-out tree as of a finished frame, read-only.
pub struct ObservedFrame<'a> {
    pub arena: &'a Arena,
    pub styles: &'a HashMap<NodeId, ComputedStyle>,
    /// Physical pixels, scroll offsets applied.
    pub layouts: &'a HashMap<NodeId, BoxLayout>,
    /// Each node's box `(x, y, width, height)` in physical pixels, where it is
    /// drawn: its own transform and every ancestor's are applied.
    pub bounds: &'a HashMap<NodeId, (f32, f32, f32, f32)>,
    pub scale_factor: f64,
    pub focused: Option<NodeId>,
}

/// The frame just painted, before it is presented: premultiplied RGBA, rows
/// top to bottom, `width * height * 4` bytes.
pub struct OverlayCanvas<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a mut [u8],
}

/// What a development tool sees of the desktop host. Every method has a no-op
/// default.
pub trait HostObserver {
    /// A window of the application was created. `window` is for asking for a
    /// repaint; an observer must not resize, move or close it.
    fn window_opened(&mut self, _event_loop: &ActiveEventLoop, _window: &Arc<Window>) {}

    /// A window of the application was closed.
    fn window_closed(&mut self, _window: WindowId) {}

    /// Offered first for every window event. `true` means the window is the
    /// observer's own and the host must not handle the event.
    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window: WindowId,
        _event: &WindowEvent,
    ) -> bool {
        false
    }

    /// A window's frame was laid out and painted.
    fn frame(&mut self, _window: WindowId, _frame: &ObservedFrame<'_>) {}

    /// Draws over the painted frame before it is shown; the application's own
    /// state is not touched.
    fn paint_overlay(&mut self, _window: WindowId, _canvas: OverlayCanvas<'_>) {}

    /// While `true`, pointer movement and button presses on `window` belong to
    /// the observer (an element picker) and do not reach the application.
    fn is_picking(&self, _window: WindowId) -> bool {
        false
    }

    /// The pointer moved over `node` while picking.
    fn pointer_moved(&mut self, _window: WindowId, _node: Option<NodeId>) {}

    /// The button was released over `node` while picking.
    fn picked(&mut self, _window: WindowId, _node: Option<NodeId>) {}
}

pub(crate) type SharedObserver = Rc<RefCell<dyn HostObserver>>;
