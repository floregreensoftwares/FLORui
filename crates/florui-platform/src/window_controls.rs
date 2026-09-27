//! A real window's own drag/minimize/maximize/close, reachable from
//! components the same way [`crate::use_committed_size`] reaches
//! `SizeObserverRegistry` — [`crate::desktop::DesktopHost`] provides one
//! [`WindowControls`] every render, starting with the first. Not tied to
//! [`crate::appearance::DecorationMode::Custom`]: a system-decorated
//! window can still want a draggable toolbar or extra chrome.
//!
//! # Marking a draggable region
//!
//! [`WINDOW_DRAG_REGION_ID`] is a well-known `id`: give one leaf element
//! that id (never one with interactive children — a press only drags
//! when it hits this exact element) and [`crate::desktop::DesktopHost`]
//! calls [`WindowControls::drag`] on press instead of treating it as a
//! click. Reuses the existing `id` attribute; no new markup concept.
//!
//! # Marking an interactive region under [`InputMode::Selective`]
//!
//! [`WINDOW_INPUT_REGION_CLASS`] is the same idea for
//! [`WindowControls::set_input_mode`]'s selective mode, but for
//! potentially many elements at once, so it's a class, not an `id`:
//! [`crate::desktop::DesktopHost`] re-collects every element carrying it
//! after each render and keeps the real window's own input targeting in
//! sync — an app never computes or tracks a rectangle itself.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use florui_reactive::use_context;
use winit::window::{BadIcon, Icon, ResizeDirection, Window};

use crate::appearance::AppearanceReport;
use crate::drag_drop::{DragEvent, DragPayload};
use crate::file_dialog::{
    OpenFileDialogOptions, OpenFileDialogOutcome, SaveFileDialogOptions, SaveFileDialogOutcome,
};
use crate::menu::{ContextMenuOutcome, MenuEntry};
use crate::open_url::OpenUrlOutcome;
use crate::visited_links::VisitedLinks;

/// See this module's own doc, "Marking a draggable region."
pub const WINDOW_DRAG_REGION_ID: &str = "florui-window-drag-region";

/// See this module's own doc, "Marking an interactive region under
/// `InputMode::Selective`."
pub const WINDOW_INPUT_REGION_CLASS: &str = "florui-window-input-region";

/// What the real window does with mouse input — see
/// [`WindowControls::set_input_mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputMode {
    /// Ordinary input targeting — the window receives every click
    /// normally, exactly like before this existed.
    #[default]
    Normal,
    /// The whole window lets every click pass through to whatever is
    /// behind it, on any other application — see
    /// [`crate::overlay`]'s own doc. No-op outside Windows.
    Passthrough,
    /// Only elements carrying [`WINDOW_INPUT_REGION_CLASS`] receive
    /// clicks; everywhere else passes through, on any other
    /// application, the same as [`InputMode::Passthrough`] — see
    /// `crate::os::windows::input_regions`'s own doc for why this needs
    /// a live cursor-tracking hook rather than a plain per-window hit
    /// test. No-op outside Windows.
    Selective,
}

/// A real screen rectangle (physical pixels, not logical/DPI-scaled) —
/// what [`InputMode::Selective`]'s own hit-testing compares the cursor
/// against. Computed by [`crate::desktop::DesktopHost`] from the real
/// committed layout; never constructed by application code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScreenRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl ScreenRect {
    pub(crate) fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

/// How close to a [`crate::appearance::DecorationMode::Custom`] window's
/// own outer edge (logical pixels) counts as its resize border — there is
/// no OS-drawn border to provide one, so this crate's own margin is the
/// only resize affordance such a window has at all.
pub(crate) const RESIZE_MARGIN: f32 = 8.0;

/// Which edge/corner `(x, y)` (logical, window-relative) falls within
/// [`RESIZE_MARGIN`] of, if any, given the window's own current logical
/// `(width, height)` — `None` anywhere in the interior. A corner takes
/// priority over either single edge it touches.
pub(crate) fn resize_direction_at(
    width: f32,
    height: f32,
    x: f32,
    y: f32,
) -> Option<ResizeDirection> {
    let near_left = x < RESIZE_MARGIN;
    let near_right = x > width - RESIZE_MARGIN;
    let near_top = y < RESIZE_MARGIN;
    let near_bottom = y > height - RESIZE_MARGIN;

    match (near_left, near_right, near_top, near_bottom) {
        (true, _, true, _) => Some(ResizeDirection::NorthWest),
        (_, true, true, _) => Some(ResizeDirection::NorthEast),
        (true, _, _, true) => Some(ResizeDirection::SouthWest),
        (_, true, _, true) => Some(ResizeDirection::SouthEast),
        (true, _, _, _) => Some(ResizeDirection::West),
        (_, true, _, _) => Some(ResizeDirection::East),
        (_, _, true, _) => Some(ResizeDirection::North),
        (_, _, _, true) => Some(ResizeDirection::South),
        _ => None,
    }
}

/// Converts an already-decoded [`florui_icon::RawIcon`] into a real
/// `winit` icon -- shared by [`WindowControls::set_icon`] and
/// [`crate::desktop`]'s own creation-time icon application, so the
/// straight-RGBA-to-`Icon` conversion exists in exactly one place. Errors
/// straight through, unwrapped, matching [`crate::desktop::RunError`]'s
/// own existing convention of exposing raw `winit`/`softbuffer`/`notify`
/// errors rather than wrapping them in a florui-local type.
pub(crate) fn to_winit_icon(icon: &florui_icon::RawIcon) -> Result<Icon, BadIcon> {
    Icon::from_rgba(icon.rgba.clone(), icon.width, icon.height)
}

/// Split out of [`WindowControls`] so it's testable without a real
/// `Window`. `None` (no guard registered) always confirms.
#[derive(Default)]
struct CloseGuard(RefCell<Option<Box<dyn Fn() -> bool>>>);

impl CloseGuard {
    fn set(&self, guard: impl Fn() -> bool + 'static) {
        *self.0.borrow_mut() = Some(Box::new(guard));
    }

    fn clear(&self) {
        *self.0.borrow_mut() = None;
    }

    fn confirm(&self) -> bool {
        match self.0.borrow().as_ref() {
            Some(guard) => guard(),
            None => true,
        }
    }
}

/// The app's own file-dialog callback, deliberately not `Send` (it may
/// close over `Signal`s) -- never leaves the UI thread; only the plain
/// outcome data crosses over from the background thread.
type PendingDialogCallback<Outcome> = RefCell<Option<Box<dyn FnOnce(Outcome)>>>;

type DragAcceptCallback = RefCell<Option<Box<dyn Fn(&DragPayload) -> bool>>>;
type DragEventCallback = RefCell<Option<Box<dyn Fn(DragEvent)>>>;

/// Split out for parallel structure with [`CloseGuard`]. Unlike it,
/// unset (no policy registered) rejects every drop rather than allowing
/// it -- accepting an untrusted external payload must be opt-in.
#[derive(Default)]
struct DragAcceptPolicy(DragAcceptCallback);

impl DragAcceptPolicy {
    fn set(&self, policy: impl Fn(&DragPayload) -> bool + 'static) {
        *self.0.borrow_mut() = Some(Box::new(policy));
    }

    fn clear(&self) {
        *self.0.borrow_mut() = None;
    }

    fn evaluate(&self, payload: &DragPayload) -> bool {
        match self.0.borrow().as_ref() {
            Some(policy) => policy(payload),
            None => false,
        }
    }
}

/// The app's own drag-event callback -- unlike the file-dialog callbacks
/// above, called synchronously and directly from inside the OS's own
/// drag callback (see `crate::os::windows::drag_drop`'s own doc), since
/// that callback already runs on this same UI thread; no background
/// thread or `UserEvent` hand-off is involved.
#[derive(Default)]
struct DragEventHandler(DragEventCallback);

impl DragEventHandler {
    fn set(&self, handler: impl Fn(DragEvent) + 'static) {
        *self.0.borrow_mut() = Some(Box::new(handler));
    }

    fn clear(&self) {
        *self.0.borrow_mut() = None;
    }

    fn dispatch(&self, event: DragEvent) {
        if let Some(handler) = self.0.borrow().as_ref() {
            handler(event);
        }
    }
}

/// A real, live handle to the window a [`crate::desktop::DesktopHost`] is
/// running — every method here acts on the actual window immediately, not
/// a request the host might defer or ignore.
pub struct WindowControls {
    window: Arc<Window>,
    request_close: Box<dyn Fn()>,
    close_guard: CloseGuard,
    input_mode: Cell<InputMode>,
    /// Last value reported by a real `WindowEvent::Focused` — a freshly
    /// created window is assumed focused until told otherwise, since
    /// `winit` gives no synchronous way to ask a window's current focus
    /// state at construction time.
    focused: Cell<bool>,
    /// What [`crate::appearance::probe_appearance`] found for this real
    /// window at creation time — fixed for the window's whole lifetime,
    /// unlike `focused`/`input_mode` above, since decorations/transparency
    /// aren't changed after creation.
    appearance_report: AppearanceReport,
    /// `Arc`, not `Box` like `request_close` -- a *clone* of this needs to
    /// move into the background thread [`Self::open_file_dialog`] spawns,
    /// while the original stays here for the next call.
    notify_open_dialog_result: Arc<dyn Fn(OpenFileDialogOutcome) + Send + Sync>,
    notify_save_dialog_result: Arc<dyn Fn(SaveFileDialogOutcome) + Send + Sync>,
    /// `Some` between [`Self::open_file_dialog`]/`save_file_dialog` being
    /// called and its result arriving.
    pending_open_dialog: PendingDialogCallback<OpenFileDialogOutcome>,
    pending_save_dialog: PendingDialogCallback<SaveFileDialogOutcome>,
    drag_accept_policy: DragAcceptPolicy,
    drag_event_handler: DragEventHandler,
    /// Shared across every window this process opens — see
    /// [`VisitedLinks`]'s own doc. [`Self::open_url`] is the only method
    /// that writes to it.
    visited_links: VisitedLinks,
}

impl WindowControls {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        window: Arc<Window>,
        request_close: impl Fn() + 'static,
        notify_open_dialog_result: impl Fn(OpenFileDialogOutcome) + Send + Sync + 'static,
        notify_save_dialog_result: impl Fn(SaveFileDialogOutcome) + Send + Sync + 'static,
        appearance_report: AppearanceReport,
        visited_links: VisitedLinks,
    ) -> Self {
        Self {
            window,
            request_close: Box::new(request_close),
            close_guard: CloseGuard::default(),
            input_mode: Cell::new(InputMode::Normal),
            focused: Cell::new(true),
            appearance_report,
            notify_open_dialog_result: Arc::new(notify_open_dialog_result),
            notify_save_dialog_result: Arc::new(notify_save_dialog_result),
            pending_open_dialog: RefCell::new(None),
            pending_save_dialog: RefCell::new(None),
            drag_accept_policy: DragAcceptPolicy::default(),
            drag_event_handler: DragEventHandler::default(),
            visited_links,
        }
    }

    /// Shows a real native "Open File" dialog, asynchronously: this
    /// method returns immediately, and `on_result` runs later, on this
    /// same UI thread, once the dialog closes -- safe to touch `Signal`s
    /// or any other component state directly from `on_result`. If a
    /// dialog is already pending for this window, `on_result` is called
    /// immediately with [`OpenFileDialogOutcome::Failed`] rather than
    /// silently replacing or queuing behind the first one.
    pub fn open_file_dialog(
        &self,
        options: OpenFileDialogOptions,
        on_result: impl FnOnce(OpenFileDialogOutcome) + 'static,
    ) {
        if self.pending_open_dialog.borrow().is_some() {
            on_result(OpenFileDialogOutcome::Failed(
                "another open-file dialog is already showing for this window".to_owned(),
            ));
            return;
        }
        *self.pending_open_dialog.borrow_mut() = Some(Box::new(on_result));
        let notify = Arc::clone(&self.notify_open_dialog_result);
        crate::file_dialog::spawn_open_dialog(&self.window, options, move |outcome| {
            notify(outcome);
        });
    }

    /// See [`Self::open_file_dialog`]'s own doc -- same contract, for a
    /// native "Save File" dialog instead.
    pub fn save_file_dialog(
        &self,
        options: SaveFileDialogOptions,
        on_result: impl FnOnce(SaveFileDialogOutcome) + 'static,
    ) {
        if self.pending_save_dialog.borrow().is_some() {
            on_result(SaveFileDialogOutcome::Failed(
                "another save-file dialog is already showing for this window".to_owned(),
            ));
            return;
        }
        *self.pending_save_dialog.borrow_mut() = Some(Box::new(on_result));
        let notify = Arc::clone(&self.notify_save_dialog_result);
        crate::file_dialog::spawn_save_dialog(&self.window, options, move |outcome| {
            notify(outcome);
        });
    }

    /// Called by [`crate::desktop::DesktopHost`] once `outcome` arrives
    /// via its own `UserEvent` -- takes and invokes whichever callback
    /// [`Self::open_file_dialog`] stored, on this same UI thread.
    pub(crate) fn deliver_open_dialog_result(&self, outcome: OpenFileDialogOutcome) {
        if let Some(callback) = self.pending_open_dialog.borrow_mut().take() {
            callback(outcome);
        }
    }

    /// See [`Self::deliver_open_dialog_result`]'s own doc -- same
    /// contract, for [`Self::save_file_dialog`] instead.
    pub(crate) fn deliver_save_dialog_result(&self, outcome: SaveFileDialogOutcome) {
        if let Some(callback) = self.pending_save_dialog.borrow_mut().take() {
            callback(outcome);
        }
    }

    /// Opens `url` in the OS's own default browser. Unlike
    /// [`Self::open_file_dialog`], this returns its outcome immediately --
    /// the underlying OS call doesn't block on anything the browser does,
    /// so there is no result to deliver later. `<a href="http(s)://...">`
    /// calls this itself as its own default action; an app (or
    /// `florui_routing::Link`) can call it directly too, for the same
    /// reason [`Self::open_file_dialog`] is public rather than
    /// `<input type="file">`-only.
    ///
    /// Marks `url` visited (see [`VisitedLinks`]) only on
    /// [`OpenUrlOutcome::Opened`] -- a cancelled `event.prevent_default()`
    /// never reaches this method at all (the caller checks that first),
    /// and [`OpenUrlOutcome::Failed`]/[`OpenUrlOutcome::Unavailable`]
    /// mean the browser never actually opened, so nothing was visited.
    /// This is the one place that rule lives, not duplicated at every
    /// caller.
    pub fn open_url(&self, url: &str) -> OpenUrlOutcome {
        let outcome = crate::open_url::open_url(&self.window, url);
        if outcome == OpenUrlOutcome::Opened {
            self.visited_links.mark_visited(url);
        }
        outcome
    }

    /// Starts an OS-native move-drag from the current mouse position. A
    /// `winit`-level failure is silently ignored — nothing useful to do
    /// differently either way.
    pub fn drag(&self) {
        let _ = self.window.drag_window();
    }

    /// Starts an OS-native resize-drag in `direction` from the current
    /// mouse position — same shape as [`Self::drag`]. A `winit`-level
    /// failure is silently ignored.
    pub(crate) fn start_resize(&self, direction: ResizeDirection) {
        let _ = self.window.drag_resize_window(direction);
    }

    /// Applies the real OS resize cursor for `direction`, or the default
    /// arrow when `None` — called on every cursor move over a
    /// [`crate::appearance::DecorationMode::Custom`] window so the border
    /// looks resizable even though nothing is actually being dragged yet.
    pub(crate) fn set_resize_cursor(&self, direction: Option<ResizeDirection>) {
        match direction {
            Some(direction) => self
                .window
                .set_cursor(winit::window::CursorIcon::from(direction)),
            None => self.window.set_cursor(winit::window::CursorIcon::Default),
        }
    }

    /// Applies the real OS pointer/default cursor for whatever's under
    /// the mouse right now (`cursor: pointer`, e.g. a focusable `<a>`) --
    /// called on every cursor move that isn't already claimed by
    /// [`Self::set_resize_cursor`] (a resize edge always wins). Unlike
    /// that method, this isn't gated to
    /// [`crate::appearance::DecorationMode::Custom`]: a system-decorated
    /// window has no cursor logic of its own to conflict with.
    pub(crate) fn set_content_cursor(&self, pointer: bool) {
        self.window.set_cursor(if pointer {
            winit::window::CursorIcon::Pointer
        } else {
            winit::window::CursorIcon::Default
        });
    }

    /// Shows a real native context menu at the current cursor position,
    /// blocking this thread until it closes -- the same synchronous shape
    /// as [`Self::drag`], not the async open/save-dialog pattern: a
    /// context menu is a short OS-pumped modal gesture, not something that
    /// can be left open indefinitely. See [`crate::menu`]'s own doc for
    /// what `items` can express and its documented limits (display-only
    /// shortcuts, no CSS styling).
    pub fn show_context_menu(&self, items: &[MenuEntry]) -> ContextMenuOutcome {
        crate::menu::show_context_menu(&self.window, items)
    }

    /// Shows the real OS system menu (Restore/Move/Size/Minimize/Maximize/
    /// Close) at the current cursor position -- called by
    /// [`crate::desktop::DesktopHost`] on a real right-click of
    /// [`WINDOW_DRAG_REGION_ID`]. No-op outside Windows -- see
    /// [`crate::menu`]'s own stub.
    pub(crate) fn show_system_menu_at_cursor(&self) {
        crate::menu::show_system_menu_at_cursor(&self.window);
    }

    /// See [`Self::show_system_menu_at_cursor`]'s own doc -- positioned at
    /// an explicit screen point instead, for a keyboard-triggered Alt+Space.
    pub(crate) fn show_system_menu_at(&self, screen_x: i32, screen_y: i32) {
        crate::menu::show_system_menu_at(&self.window, screen_x, screen_y);
    }

    /// Live query, not a cached guess — picks up snap/double-click too.
    pub fn is_maximized(&self) -> bool {
        self.window.is_maximized()
    }

    pub fn minimize(&self) {
        self.window.set_minimized(true);
    }

    pub fn toggle_maximize(&self) {
        let maximized = self.window.is_maximized();
        self.window.set_maximized(!maximized);
    }

    /// Whether this window currently has OS input focus — reflects the
    /// last real `WindowEvent::Focused`, not a live OS query (`winit`
    /// exposes no synchronous way to ask), so a component reading this
    /// during the very first render before any such event has arrived
    /// sees the assumed-focused default from [`Self::new`].
    pub fn is_focused(&self) -> bool {
        self.focused.get()
    }

    /// Called by [`crate::desktop::DesktopHost`] on a real
    /// `WindowEvent::Focused`.
    pub(crate) fn set_focused(&self, focused: bool) {
        self.focused.set(focused);
    }

    /// What this window actually got vs. what [`crate::run_with_options`]
    /// requested, per capability — see [`AppearanceReport`]'s own doc.
    /// Probed once, against this real window, at creation time.
    pub fn appearance_report(&self) -> &AppearanceReport {
        &self.appearance_report
    }

    pub fn set_always_on_top(&self, enabled: bool) {
        self.window.set_window_level(if enabled {
            winit::window::WindowLevel::AlwaysOnTop
        } else {
            winit::window::WindowLevel::Normal
        });
    }

    /// Borderless fullscreen on the window's current monitor.
    pub fn set_fullscreen(&self, enabled: bool) {
        self.window
            .set_fullscreen(enabled.then_some(winit::window::Fullscreen::Borderless(None)));
    }

    pub fn is_fullscreen(&self) -> bool {
        self.window.fullscreen().is_some()
    }

    /// Switches how the real window targets mouse input — see
    /// [`InputMode`]'s own doc for what each mode does. `false` (no-op,
    /// though this mode is still recorded and reported by
    /// [`Self::input_mode`]) outside Windows.
    pub fn set_input_mode(&self, mode: InputMode) -> bool {
        self.input_mode.set(mode);
        crate::overlay::set_input_mode(&self.window, mode)
    }

    /// The mode last requested via [`Self::set_input_mode`] —
    /// [`InputMode::Normal`] if never called.
    pub fn input_mode(&self) -> InputMode {
        self.input_mode.get()
    }

    /// Feeds [`InputMode::Selective`]'s live hit-testing the current
    /// screen rectangles of every [`WINDOW_INPUT_REGION_CLASS`] element —
    /// called by [`crate::desktop::DesktopHost`] after every render; a
    /// no-op whenever [`Self::input_mode`] isn't
    /// [`InputMode::Selective`].
    pub(crate) fn sync_input_regions(&self, regions: &[ScreenRect]) {
        if self.input_mode.get() == InputMode::Selective {
            crate::overlay::sync_input_regions(&self.window, regions);
        }
    }

    /// Sets this already-open window's icon at runtime -- the "update"
    /// half of florui-config's own "window creation/update APIs" for
    /// per-window icon overrides (creation-time icon application lives in
    /// [`crate::desktop`], via the same [`to_winit_icon`]). No-op on
    /// macOS ("macOS doesn't have window icons", per `winit`'s own
    /// `Window::set_window_icon` doc) -- not gated here, since a silent
    /// platform no-op is exactly what the underlying `winit` call itself
    /// already does.
    pub fn set_icon(&self, icon: &florui_icon::RawIcon) -> Result<(), BadIcon> {
        let winit_icon = to_winit_icon(icon)?;
        self.window.set_window_icon(Some(winit_icon));
        Ok(())
    }

    /// Same shutdown path as the OS's own close button, including
    /// whatever [`Self::set_close_guard`] currently allows or vetoes.
    pub fn close(&self) {
        (self.request_close)();
    }

    /// `guard` decides every future close request — the real OS close
    /// button as much as [`Self::close`]. Returning `false` leaves the
    /// window open; showing a "you have unsaved changes" dialog is the
    /// caller's own job. Replaces any previous guard; pair with
    /// [`florui_reactive::use_effect`] and [`Self::clear_close_guard`] as
    /// its cleanup so it tracks live reactive state, not a stale snapshot.
    pub fn set_close_guard(&self, guard: impl Fn() -> bool + 'static) {
        self.close_guard.set(guard);
    }

    /// Removes the guard; future closes are allowed again.
    pub fn clear_close_guard(&self) {
        self.close_guard.clear();
    }

    pub(crate) fn confirm_close(&self) -> bool {
        self.close_guard.confirm()
    }

    /// `policy` decides whether a hovering external drag payload is
    /// accepted -- called synchronously and directly from inside the OS's
    /// own drag callback (see `crate::os::windows::drag_drop`'s own doc),
    /// so it must not block. Unset (the default) rejects every drop,
    /// since accepting an untrusted external payload must be opt-in --
    /// the opposite default from [`Self::set_close_guard`]. Replaces any
    /// previous policy.
    pub fn set_drag_accept_policy(&self, policy: impl Fn(&DragPayload) -> bool + 'static) {
        self.drag_accept_policy.set(policy);
    }

    /// Removes the policy; every drop is rejected again until a new one
    /// is set.
    pub fn clear_drag_accept_policy(&self) {
        self.drag_accept_policy.clear();
    }

    /// `handler` receives every [`DragEvent`] for this window for as long
    /// as it stays registered -- see [`DragEvent`]'s own doc for exactly
    /// which variants fire for a payload
    /// [`Self::set_drag_accept_policy`] rejected. Replaces any previous
    /// handler.
    pub fn on_drag_event(&self, handler: impl Fn(DragEvent) + 'static) {
        self.drag_event_handler.set(handler);
    }

    /// Removes the handler; future drag events are silently dropped.
    pub fn clear_drag_event_handler(&self) {
        self.drag_event_handler.clear();
    }

    pub(crate) fn evaluate_drag_accept(&self, payload: &DragPayload) -> bool {
        self.drag_accept_policy.evaluate(payload)
    }

    pub(crate) fn dispatch_drag_event(&self, event: DragEvent) {
        self.drag_event_handler.dispatch(event);
    }
}

impl Drop for WindowControls {
    /// [`InputMode::Selective`]'s hook and [`InputMode::Passthrough`]'s
    /// `WS_EX_TRANSPARENT` both outlive this struct otherwise — nothing
    /// else ever un-sets them once the window itself is gone.
    fn drop(&mut self) {
        if self.input_mode.get() != InputMode::Normal {
            crate::overlay::set_input_mode(&self.window, InputMode::Normal);
        }
    }
}

/// Reads the [`WindowControls`] [`crate::desktop::DesktopHost`] provides
/// every render. `None` outside a real desktop host.
pub fn use_window_controls() -> Option<Rc<WindowControls>> {
    use_context::<Rc<WindowControls>>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_interior_is_never_a_resize_direction() {
        assert_eq!(resize_direction_at(400.0, 300.0, 200.0, 150.0), None);
    }

    #[test]
    fn each_edges_own_middle_resizes_along_that_single_axis() {
        assert_eq!(
            resize_direction_at(400.0, 300.0, 1.0, 150.0),
            Some(ResizeDirection::West)
        );
        assert_eq!(
            resize_direction_at(400.0, 300.0, 399.0, 150.0),
            Some(ResizeDirection::East)
        );
        assert_eq!(
            resize_direction_at(400.0, 300.0, 200.0, 1.0),
            Some(ResizeDirection::North)
        );
        assert_eq!(
            resize_direction_at(400.0, 300.0, 200.0, 299.0),
            Some(ResizeDirection::South)
        );
    }

    #[test]
    fn a_corner_takes_priority_over_either_edge_it_touches() {
        assert_eq!(
            resize_direction_at(400.0, 300.0, 1.0, 1.0),
            Some(ResizeDirection::NorthWest)
        );
        assert_eq!(
            resize_direction_at(400.0, 300.0, 399.0, 1.0),
            Some(ResizeDirection::NorthEast)
        );
        assert_eq!(
            resize_direction_at(400.0, 300.0, 1.0, 299.0),
            Some(ResizeDirection::SouthWest)
        );
        assert_eq!(
            resize_direction_at(400.0, 300.0, 399.0, 299.0),
            Some(ResizeDirection::SouthEast)
        );
    }

    #[test]
    fn exactly_at_the_margin_boundary_is_already_interior() {
        // RESIZE_MARGIN is 8.0 -- a point exactly on it is not "closer
        // than" the margin, matching ScreenRect's own half-open convention.
        assert_eq!(
            resize_direction_at(400.0, 300.0, RESIZE_MARGIN, 150.0),
            None
        );
    }

    fn raw_icon(rgba: Vec<u8>, width: u32, height: u32) -> florui_icon::RawIcon {
        florui_icon::RawIcon {
            rgba,
            width,
            height,
        }
    }

    #[test]
    fn to_winit_icon_converts_a_well_formed_raw_icon() {
        let icon = raw_icon(vec![0; (2 * 2 * 4) as usize], 2, 2);
        assert!(to_winit_icon(&icon).is_ok());
    }

    #[test]
    fn to_winit_icon_reports_a_byte_count_not_divisible_by_4() {
        let icon = raw_icon(vec![0; 5], 1, 1);
        let err = to_winit_icon(&icon).unwrap_err();
        assert!(matches!(err, BadIcon::ByteCountNotDivisibleBy4 { .. }));
    }

    #[test]
    fn to_winit_icon_reports_a_dimension_pixel_count_mismatch() {
        // 4 pixels' worth of bytes, but dimensions claim only 1 pixel.
        let icon = raw_icon(vec![0; 4 * 4], 1, 1);
        let err = to_winit_icon(&icon).unwrap_err();
        assert!(matches!(err, BadIcon::DimensionsVsPixelCount { .. }));
    }

    #[test]
    fn confirms_by_default_with_no_guard_registered() {
        let guard = CloseGuard::default();
        assert!(guard.confirm(), "no guard set must never block a close");
    }

    #[test]
    fn a_registered_guard_can_veto_a_close() {
        let guard = CloseGuard::default();
        guard.set(|| false);
        assert!(!guard.confirm());
    }

    #[test]
    fn a_registered_guard_can_allow_a_close() {
        let guard = CloseGuard::default();
        guard.set(|| true);
        assert!(guard.confirm());
    }

    #[test]
    fn confirm_reads_the_guards_current_state_not_a_snapshot() {
        let allowed = Rc::new(RefCell::new(false));
        let guard = CloseGuard::default();
        let allowed_in_guard = Rc::clone(&allowed);
        guard.set(move || *allowed_in_guard.borrow());

        assert!(
            !guard.confirm(),
            "the guard's own real state starts out vetoing the close"
        );
        *allowed.borrow_mut() = true;
        assert!(
            guard.confirm(),
            "confirm() must re-read the guard's current state, not cache the first result"
        );
    }

    #[test]
    fn clearing_a_guard_restores_the_default_allow_behavior() {
        let guard = CloseGuard::default();
        guard.set(|| false);
        assert!(!guard.confirm());

        guard.clear();
        assert!(
            guard.confirm(),
            "clearing the guard must go back to the no-guard default, not stay vetoed"
        );
    }

    #[test]
    fn setting_a_new_guard_replaces_the_previous_one() {
        let guard = CloseGuard::default();
        guard.set(|| false);
        guard.set(|| true);
        assert!(
            guard.confirm(),
            "the second set() must fully replace the first, not combine with it"
        );
    }

    fn files_payload() -> DragPayload {
        DragPayload::Files(vec!["dropped.txt".into()])
    }

    #[test]
    fn drag_accept_policy_rejects_by_default() {
        let policy = DragAcceptPolicy::default();
        assert!(
            !policy.evaluate(&files_payload()),
            "no policy registered must reject, unlike CloseGuard's default-allow"
        );
    }

    #[test]
    fn drag_accept_policy_can_accept() {
        let policy = DragAcceptPolicy::default();
        policy.set(|_| true);
        assert!(policy.evaluate(&files_payload()));
    }

    #[test]
    fn drag_accept_policy_clear_restores_the_default_reject() {
        let policy = DragAcceptPolicy::default();
        policy.set(|_| true);
        policy.clear();
        assert!(!policy.evaluate(&files_payload()));
    }

    #[test]
    fn drag_accept_policy_replaces_not_combines() {
        let policy = DragAcceptPolicy::default();
        policy.set(|_| true);
        policy.set(|_| false);
        assert!(
            !policy.evaluate(&files_payload()),
            "the second set() must fully replace the first"
        );
    }

    #[test]
    fn drag_event_handler_is_a_silent_no_op_by_default() {
        let handler = DragEventHandler::default();
        handler.dispatch(DragEvent::Leave);
    }

    #[test]
    fn drag_event_handler_dispatches_to_the_registered_handler() {
        let seen = Rc::new(RefCell::new(None));
        let handler = DragEventHandler::default();
        let seen_in_handler = Rc::clone(&seen);
        handler.set(move |event| *seen_in_handler.borrow_mut() = Some(event));

        handler.dispatch(DragEvent::Leave);
        assert_eq!(*seen.borrow(), Some(DragEvent::Leave));
    }

    #[test]
    fn drag_event_handler_replaces_not_combines() {
        let calls = Rc::new(RefCell::new(0));
        let handler = DragEventHandler::default();

        let first_calls = Rc::clone(&calls);
        handler.set(move |_| *first_calls.borrow_mut() += 1);
        let second_calls = Rc::clone(&calls);
        handler.set(move |_| *second_calls.borrow_mut() += 10);

        handler.dispatch(DragEvent::Leave);
        assert_eq!(
            *calls.borrow(),
            10,
            "the second set() must fully replace the first, not combine with it"
        );
    }
}
