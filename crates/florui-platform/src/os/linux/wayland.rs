//! What `winit` does not give a Wayland window: file drag-and-drop with a
//! pointer position, the text clipboard, and per-region input. Each is a
//! protocol object the compositor offers (the data device, the input region of
//! a surface), reached on the connection `winit` already opened, so a window
//! needs no second connection and no extra permission.
//!
//! The objects live on an event queue of their own. The compositor's events
//! for them arrive on the socket `winit` reads, so [`pump`] only has to run
//! them, once per loop iteration; the host wakes the loop through the waker
//! given to [`init`] when data a thread read is ready.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wayland_backend::client::{Backend, ObjectId};
use wayland_client::backend::WaylandError;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, Capability, WlSeat};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_subcompositor::WlSubcompositor;
use wayland_client::protocol::wl_subsurface::WlSubsurface;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, event_created_child,
};
use winit::raw_window_handle::{
    HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
};
use winit::window::{Window, WindowId};

use crate::menu::ContextMenuOutcome;
use crate::menu_popup::{MenuPopup, PopupFrame, PopupKey};

/// How long a read from another client may take before it is given up on.
const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// The text types a clipboard is read and offered as, best first.
const TEXT_TYPES: [&str; 3] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];
const FILE_LIST_TYPE: &str = "text/uri-list";

/// One step of a file drag, in the surface's own (logical) coordinates.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DragStep {
    Enter(Vec<PathBuf>, (f32, f32)),
    Over((f32, f32)),
    Leave,
    Drop(Vec<PathBuf>, (f32, f32)),
}

#[derive(Default)]
struct Mimes(Mutex<Vec<String>>);

struct Drag {
    offer: WlDataOffer,
    serial: u32,
    window: WindowId,
    position: (f32, f32),
    mime: Option<String>,
    files: Arc<Mutex<Option<Vec<PathBuf>>>>,
    entered: bool,
    dropped: bool,
    accepted: bool,
}

struct Own {
    source: WlDataSource,
    text: Arc<str>,
}

#[derive(Default)]
struct State {
    conn: Option<Connection>,
    compositor: Option<WlCompositor>,
    seat: Option<WlSeat>,
    keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
    manager: Option<WlDataDeviceManager>,
    device: Option<WlDataDevice>,
    /// The serial of the latest keyboard or pointer event: what the compositor
    /// wants to see before it lets a client take the clipboard.
    serial: u32,
    windows: HashMap<ObjectId, WindowId>,
    drag: Option<Drag>,
    steps: Vec<(WindowId, DragStep)>,
    selection: Option<WlDataOffer>,
    own: Option<Own>,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
    subcompositor: Option<WlSubcompositor>,
    shm: Option<WlShm>,
    /// The surface the pointer is over and where, while a menu is open.
    pointer_over: Option<ObjectId>,
    pointer_at: (f32, f32),
    popup: Option<PopupInput>,
}

/// What a menu that is open is told by the pointer and keyboard this module
/// listens to.
struct PopupInput {
    child: ObjectId,
    events: VecDeque<PopupEvent>,
}

enum PopupEvent {
    Moved((f32, f32)),
    Left,
    Pressed(Option<(f32, f32)>),
    Key(u32),
}

struct Link {
    conn: Connection,
    queue: EventQueue<State>,
    qh: QueueHandle<State>,
    state: State,
}

thread_local! {
    static LINK: RefCell<Option<Link>> = const { RefCell::new(None) };
}

fn with_link<T>(f: impl FnOnce(&mut Link) -> T) -> Option<T> {
    LINK.with(|slot| slot.borrow_mut().as_mut().map(f))
}

/// Attaches to the compositor `window` is on. Does nothing on X11 and on a
/// second call.
pub(crate) fn init(window: &Window, waker: Arc<dyn Fn() + Send + Sync>) {
    LINK.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = connect(window, waker);
        }
    });
    if let Some(surface) = surface_id(window) {
        let id = window.id();
        with_link(|link| link.state.windows.insert(surface, id));
    }
}

fn connect(window: &Window, waker: Arc<dyn Fn() + Send + Sync>) -> Option<Link> {
    let RawDisplayHandle::Wayland(display) = window.display_handle().ok()?.as_raw() else {
        return None;
    };
    // SAFETY: the display outlives every window `winit` made on it, and the
    // backend made here does not own or close it.
    let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
    let conn = Connection::from_backend(backend);
    let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
    let qh = queue.handle();
    let mut state = State {
        conn: Some(conn.clone()),
        waker: Some(waker),
        ..State::default()
    };
    state.compositor = globals.bind(&qh, 1..=6, ()).ok();
    state.seat = globals.bind(&qh, 1..=7, ()).ok();
    state.manager = globals.bind(&qh, 1..=3, ()).ok();
    state.subcompositor = globals.bind(&qh, 1..=1, ()).ok();
    state.shm = globals.bind(&qh, 1..=1, ()).ok();
    if let (Some(manager), Some(seat)) = (&state.manager, &state.seat) {
        state.device = Some(manager.get_data_device(seat, &qh, ()));
    }
    queue.roundtrip(&mut state).ok()?;
    spawn_reader(conn.clone(), state.waker.clone()?);
    Some(Link {
        conn,
        queue,
        qh,
        state,
    })
}

fn surface_id(window: &Window) -> Option<ObjectId> {
    let RawWindowHandle::Wayland(handle) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    // SAFETY: the handle is the live `wl_surface` of this window.
    unsafe { ObjectId::from_ptr(WlSurface::interface(), handle.surface.as_ptr().cast()).ok() }
}

fn surface_of(conn: &Connection, window: &Window) -> Option<WlSurface> {
    WlSurface::from_id(conn, surface_id(window)?).ok()
}

/// Runs what the compositor sent since the last call and returns the drag
/// steps that are ready.
pub(crate) fn pump() -> Vec<(WindowId, DragStep)> {
    WAKE_PENDING.store(false, Ordering::SeqCst);
    with_link(|link| {
        let _ = link.queue.dispatch_pending(&mut link.state);
        link.state.advance_drag();
        let _ = link.conn.flush();
        std::mem::take(&mut link.state.steps)
    })
    .unwrap_or_default()
}

/// The app's answer for the drag that entered: whether it takes the payload.
pub(crate) fn answer_drag(accepted: bool) {
    with_link(|link| {
        if let Some(drag) = &mut link.state.drag {
            drag.accepted = accepted;
            let (action, mime) = match (&drag.mime, accepted) {
                (Some(mime), true) => (DndAction::Copy, Some(mime.clone())),
                _ => (DndAction::None, None),
            };
            if drag.offer.version() >= 3 {
                drag.offer.set_actions(action, action);
            }
            drag.offer.accept(drag.serial, mime);
            let _ = link.conn.flush();
        }
    });
}

/// Only these rectangles (the surface's logical pixels) receive input; none is
/// click-through. `None` makes the whole surface receive input again.
pub(crate) fn set_input_region(window: &Window, rects: Option<&[(i32, i32, i32, i32)]>) -> bool {
    with_link(|link| {
        let (Some(compositor), Some(surface)) =
            (&link.state.compositor, surface_of(&link.conn, window))
        else {
            return false;
        };
        match rects {
            Some(rects) => {
                let region: WlRegion = compositor.create_region(&link.qh, ());
                for &(x, y, width, height) in rects {
                    region.add(x, y, width, height);
                }
                surface.set_input_region(Some(&region));
                region.destroy();
            }
            None => surface.set_input_region(None),
        }
        surface.commit();
        link.conn.flush().is_ok()
    })
    .unwrap_or(false)
}

/// Whether the clipboard is reachable on this connection.
pub(crate) fn has_clipboard() -> bool {
    with_link(|link| link.state.device.is_some()).unwrap_or(false)
}

/// The clipboard's text, from this process or the client that owns it.
pub(crate) fn clipboard_text() -> Option<String> {
    with_link(|link| {
        let _ = link.queue.dispatch_pending(&mut link.state);
        if let Some(own) = &link.state.own {
            return Some(own.text.to_string());
        }
        let offer = link.state.selection.clone()?;
        let mime = pick_text_type(&mimes_of(&offer))?;
        read_offer(&link.conn, &offer, &mime)
    })
    .flatten()
}

/// Takes the clipboard. `false` when the compositor has handed this client no
/// input event to do it on, or there is no data device.
pub(crate) fn set_clipboard_text(text: &str) -> bool {
    with_link(|link| {
        let _ = link.queue.dispatch_pending(&mut link.state);
        let (Some(manager), Some(device)) = (&link.state.manager, &link.state.device) else {
            return false;
        };
        if link.state.serial == 0 {
            return false;
        }
        let source = manager.create_data_source(&link.qh, ());
        for mime in TEXT_TYPES {
            source.offer(mime.to_owned());
        }
        device.set_selection(Some(&source), link.state.serial);
        if let Some(previous) = link.state.own.replace(Own {
            source,
            text: Arc::from(text),
        }) {
            previous.source.destroy();
        }
        link.conn.flush().is_ok()
    })
    .unwrap_or(false)
}

fn mimes_of(offer: &WlDataOffer) -> Vec<String> {
    offer
        .data::<Mimes>()
        .and_then(|mimes| mimes.0.lock().ok().map(|list| list.clone()))
        .unwrap_or_default()
}

fn pick_text_type(mimes: &[String]) -> Option<String> {
    TEXT_TYPES
        .iter()
        .find(|wanted| mimes.iter().any(|mime| mime == **wanted))
        .map(|wanted| (*wanted).to_owned())
}

/// Asks the owner of `offer` for `mime` and reads what it writes back.
fn read_offer(conn: &Connection, offer: &WlDataOffer, mime: &str) -> Option<String> {
    let reader = request_data(conn, offer, mime)?;
    let mut text = String::new();
    reader.read_to_string(&mut text).ok().map(|_| text)
}

/// The read end of a pipe the owner of `offer` has been asked to fill. A
/// socket pair stands in for a pipe: the owner only writes to it and closes it.
fn request_data(conn: &Connection, offer: &WlDataOffer, mime: &str) -> Option<ReadEnd> {
    let (reader, writer) = UnixStream::pair().ok()?;
    reader.set_read_timeout(Some(READ_TIMEOUT)).ok()?;
    offer.receive(mime.to_owned(), writer.as_fd());
    drop(writer);
    conn.flush().ok()?;
    Some(ReadEnd(reader))
}

struct ReadEnd(UnixStream);

impl ReadEnd {
    fn read_to_string(mut self, into: &mut String) -> std::io::Result<usize> {
        self.0.read_to_string(into)
    }
}

/// The paths a `text/uri-list` names, in order; anything that is not a local
/// `file://` URI is left out.
pub(super) fn parse_uri_list(list: &str) -> Vec<PathBuf> {
    list.lines()
        .map(str::trim)
        .filter_map(crate::activation::file_uri_to_path)
        .map(PathBuf::from)
        .collect()
}

impl State {
    fn wake(&self) {
        if let Some(waker) = &self.waker {
            waker();
        }
    }

    fn drag_enter(
        &mut self,
        serial: u32,
        surface: &WlSurface,
        position: (f64, f64),
        offer: Option<WlDataOffer>,
    ) {
        let Some(offer) = offer else {
            return;
        };
        let Some(&window) = self.windows.get(&surface.id()) else {
            offer.destroy();
            return;
        };
        let mime = mimes_of(&offer)
            .into_iter()
            .find(|mime| mime == FILE_LIST_TYPE);
        let files = Arc::new(Mutex::new(None));
        // Some sources hand their data over only to a target that has said it
        // takes the type, so take it provisionally; the app's own answer
        // replaces this one once it has seen the files.
        if let Some(mime) = &mime {
            if offer.version() >= 3 {
                offer.set_actions(DndAction::Copy, DndAction::Copy);
            }
            offer.accept(serial, Some(mime.clone()));
        }
        if let (Some(mime), Some(conn)) = (&mime, &self.conn)
            && let Some(reader) = request_data(conn, &offer, mime)
        {
            let files = Arc::clone(&files);
            let waker = self.waker.clone();
            std::thread::spawn(move || {
                let mut list = String::new();
                let _ = reader.read_to_string(&mut list);
                if let Ok(mut slot) = files.lock() {
                    *slot = Some(parse_uri_list(&list));
                }
                if let Some(waker) = waker {
                    waker();
                }
            });
        }
        if mime.is_none() {
            offer.accept(serial, None);
        }
        self.drag = Some(Drag {
            offer,
            serial,
            window,
            position: (position.0 as f32, position.1 as f32),
            mime,
            files,
            entered: false,
            dropped: false,
            accepted: false,
        });
    }

    fn drag_motion(&mut self, position: (f64, f64)) {
        let Some(drag) = &mut self.drag else {
            return;
        };
        drag.position = (position.0 as f32, position.1 as f32);
        if drag.entered {
            self.steps
                .push((drag.window, DragStep::Over(drag.position)));
        }
    }

    fn drag_leave(&mut self) {
        // A `leave` that follows a `drop` ends the session, not the payload:
        // the drop is delivered once its file list has been read.
        if self.drag.as_ref().is_some_and(|drag| drag.dropped) {
            return;
        }
        if let Some(drag) = self.drag.take() {
            if drag.entered {
                self.steps.push((drag.window, DragStep::Leave));
            }
            drag.offer.destroy();
        }
    }

    fn drag_drop(&mut self) {
        if let Some(drag) = &mut self.drag {
            drag.dropped = true;
        }
    }

    /// Turns a drag whose file list has been read into its `Enter`, and a drop
    /// into its `Drop`, once the list is there.
    fn advance_drag(&mut self) {
        let Some(drag) = &mut self.drag else {
            return;
        };
        // `None`: still being read. A payload that is no file list, or names no
        // local file, is never delivered, and is let go when it is dropped.
        let read = drag.files.lock().ok().and_then(|slot| slot.clone());
        let files = match (&drag.mime, read) {
            (Some(_), Some(files)) if !files.is_empty() => files,
            (Some(_), None) => return,
            _ => {
                if drag.dropped
                    && let Some(drag) = self.drag.take()
                {
                    drag.offer.destroy();
                }
                return;
            }
        };
        if !drag.entered {
            drag.entered = true;
            self.steps
                .push((drag.window, DragStep::Enter(files.clone(), drag.position)));
            if drag.dropped {
                // The app answers the enter before the drop is delivered.
                if let Some(waker) = &self.waker {
                    waker();
                }
                return;
            }
        }
        if drag.dropped {
            if drag.accepted {
                self.steps
                    .push((drag.window, DragStep::Drop(files, drag.position)));
                if drag.offer.version() >= 3 {
                    drag.offer.finish();
                }
            } else {
                self.steps.push((drag.window, DragStep::Leave));
            }
            if let Some(drag) = self.drag.take() {
                drag.offer.destroy();
            }
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlCompositor, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlCompositor,
        _: <WlCompositor as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlRegion, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlRegion,
        _: <WlRegion as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataDeviceManager,
        _: <WlDataDeviceManager as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(Capability::Keyboard) && state.keyboard.is_none() {
                state.keyboard = Some(seat.get_keyboard(qh, ()));
            }
            if capabilities.contains(Capability::Pointer) && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(qh, ()));
            }
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { serial, .. }
            | wl_keyboard::Event::Modifiers { serial, .. } => {
                state.serial = serial;
            }
            wl_keyboard::Event::Key {
                serial,
                key,
                state: WEnum::Value(wl_keyboard::KeyState::Pressed),
                ..
            } => {
                state.serial = serial;
                if let Some(popup) = &mut state.popup {
                    popup.events.push_back(PopupEvent::Key(key));
                }
            }
            wl_keyboard::Event::Key { serial, .. } => state.serial = serial,
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                serial,
                surface,
                surface_x,
                surface_y,
            } => {
                state.serial = serial;
                state.pointer_over = Some(surface.id());
                state.pointer_at = (surface_x as f32, surface_y as f32);
                state.popup_pointer(true);
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_at = (surface_x as f32, surface_y as f32);
                state.popup_pointer(true);
            }
            wl_pointer::Event::Leave { .. } => {
                state.pointer_over = None;
                if let Some(popup) = &mut state.popup {
                    popup.events.push_back(PopupEvent::Left);
                }
            }
            wl_pointer::Event::Button {
                serial,
                state: button,
                ..
            } => {
                state.serial = serial;
                if button == WEnum::Value(wl_pointer::ButtonState::Pressed) {
                    state.popup_pointer(false);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::Enter {
                serial,
                surface,
                x,
                y,
                id,
            } => state.drag_enter(serial, &surface, (x, y), id),
            wl_data_device::Event::Motion { x, y, .. } => state.drag_motion((x, y)),
            wl_data_device::Event::Leave => state.drag_leave(),
            wl_data_device::Event::Drop => state.drag_drop(),
            wl_data_device::Event::Selection { id } => {
                if let Some(old) = std::mem::replace(&mut state.selection, id) {
                    old.destroy();
                }
            }
            _ => {}
        }
        state.wake();
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, Mimes::default()),
    ]);
}

impl Dispatch<WlDataOffer, Mimes> for State {
    fn event(
        _: &mut Self,
        _: &WlDataOffer,
        event: wl_data_offer::Event,
        mimes: &Mimes,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event
            && let Ok(mut list) = mimes.0.lock()
        {
            list.push(mime_type);
        }
    }
}

impl Dispatch<WlDataSource, ()> for State {
    fn event(
        state: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { fd, .. } => {
                let Some(own) = state.own.as_ref().filter(|own| own.source == *source) else {
                    return;
                };
                let text = Arc::clone(&own.text);
                std::thread::spawn(move || {
                    let _ = std::fs::File::from(fd).write_all(text.as_bytes());
                });
            }
            wl_data_source::Event::Cancelled => {
                if state.own.as_ref().is_some_and(|own| own.source == *source) {
                    state.own = None;
                }
                source.destroy();
            }
            _ => {}
        }
    }
}

macro_rules! ignore_events {
    ($($proxy:ty),* $(,)?) => {$(
        impl Dispatch<$proxy, ()> for State {
            fn event(
                _: &mut Self,
                _: &$proxy,
                _: <$proxy as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    )*};
}

ignore_events!(WlSubcompositor, WlSubsurface, WlShm, WlShmPool, WlSurface);

impl Dispatch<WlBuffer, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlBuffer,
        _: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl State {
    /// Tells an open menu what the pointer did: `moved`, or a press. A press
    /// or a move over the menu's own surface is at the pointer's position in
    /// it; anywhere else a move is the pointer leaving, and a press dismisses.
    fn popup_pointer(&mut self, moved: bool) {
        let Some(popup) = &mut self.popup else {
            return;
        };
        let over_menu = self.pointer_over.as_ref() == Some(&popup.child);
        popup.events.push_back(match (moved, over_menu) {
            (true, true) => PopupEvent::Moved(self.pointer_at),
            (true, false) => PopupEvent::Left,
            (false, true) => PopupEvent::Pressed(Some(self.pointer_at)),
            (false, false) => PopupEvent::Pressed(None),
        });
    }
}

/// evdev key codes, which is what a `wl_keyboard` reports.
fn key_for(code: u32) -> Option<PopupKey> {
    match code {
        103 => Some(PopupKey::Up),
        108 => Some(PopupKey::Down),
        102 => Some(PopupKey::Home),
        107 => Some(PopupKey::End),
        28 | 96 | 57 => Some(PopupKey::Activate),
        1 => Some(PopupKey::Dismiss),
        _ => None,
    }
}

/// A buffer holding `frame`, in shared memory the compositor reads.
fn shm_buffer(shm: &WlShm, qh: &QueueHandle<State>, frame: &PopupFrame) -> Option<WlBuffer> {
    // SAFETY: a plain system call; the result is checked and owned below.
    let raw = unsafe { libc::memfd_create(c"florui-menu".as_ptr(), libc::MFD_CLOEXEC) };
    if raw < 0 {
        return None;
    }
    // SAFETY: `raw` is a file descriptor this call just made and nothing else owns.
    let mut file = std::fs::File::from(unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) });
    file.write_all(&frame.bgra).ok()?;
    let pool = shm.create_pool(file.as_fd(), i32::try_from(frame.bgra.len()).ok()?, qh, ());
    let buffer = pool.create_buffer(
        0,
        i32::try_from(frame.width).ok()?,
        i32::try_from(frame.height).ok()?,
        i32::try_from(frame.width * 4).ok()?,
        wl_shm::Format::Argb8888,
        qh,
        (),
    );
    pool.destroy();
    Some(buffer)
}

/// A menu that is open on a window: the subsurface it is drawn in, a
/// subsurface of the window's own surface so that it stays inside what the
/// compositor draws for the window. It listens to the seat's pointer and
/// keyboard directly, and is driven by [`advance_menu`] from the loop.
pub(crate) struct MenuSession {
    parent: WlSurface,
    child: WlSurface,
    subsurface: WlSubsurface,
    buffer: Option<WlBuffer>,
}

/// Opens `popup` over `window` at `at` (the window's logical pixels). `None`
/// when the compositor lacks what a subsurface needs.
pub(crate) fn open_menu(
    window: &Window,
    popup: &mut MenuPopup,
    at: (f32, f32),
) -> Option<MenuSession> {
    with_link(|link| {
        let compositor = link.state.compositor.clone()?;
        let subcompositor = link.state.subcompositor.clone()?;
        let parent = surface_of(&link.conn, window)?;
        // Where it goes: at the pointer, moved back inside the window.
        let inside = window.inner_size().to_logical::<f32>(window.scale_factor());
        let (width, height) = popup.size();
        let x = at.0.min(inside.width - width).max(0.0);
        let y = if at.1 + height > inside.height {
            (at.1 - height).max(0.0)
        } else {
            at.1
        };
        let child = compositor.create_surface(&link.qh, ());
        let subsurface = subcompositor.get_subsurface(&child, &parent, &link.qh, ());
        subsurface.set_position(x as i32, y as i32);
        subsurface.set_desync();
        child.set_buffer_scale(popup.scale() as i32);
        link.state.popup = Some(PopupInput {
            child: child.id(),
            events: VecDeque::new(),
        });
        let mut session = MenuSession {
            parent,
            child,
            subsurface,
            buffer: None,
        };
        session.present(link, &popup.frame());
        Some(session)
    })
    .flatten()
}

impl MenuSession {
    fn present(&mut self, link: &Link, frame: &PopupFrame) {
        let Some(shm) = &link.state.shm else {
            return;
        };
        if let Some(next) = shm_buffer(shm, &link.qh, frame) {
            self.child.attach(Some(&next), 0, 0);
            self.child
                .damage_buffer(0, 0, frame.width as i32, frame.height as i32);
            self.child.commit();
            // The position is applied with the parent's next commit.
            self.parent.commit();
            if let Some(old) = self.buffer.replace(next) {
                old.destroy();
            }
            let _ = link.conn.flush();
        }
    }
}

impl Drop for MenuSession {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            buffer.destroy();
        }
        self.subsurface.destroy();
        self.child.destroy();
        self.parent.commit();
        with_link(|link| {
            link.state.popup = None;
            let _ = link.conn.flush();
        });
    }
}

/// Applies what the pointer and keyboard did since the last call to `popup`.
/// `Some` once the menu is chosen from or dismissed.
pub(crate) fn advance_menu(
    session: &mut MenuSession,
    popup: &mut MenuPopup,
) -> Option<ContextMenuOutcome> {
    with_link(|link| {
        let events: Vec<PopupEvent> = link.state.popup.as_mut()?.events.drain(..).collect();
        let mut changed = false;
        for event in events {
            let chosen = match event {
                PopupEvent::Moved((x, y)) => {
                    popup.pointer_moved(x, y);
                    changed = true;
                    None
                }
                PopupEvent::Left => {
                    popup.pointer_left();
                    changed = true;
                    None
                }
                PopupEvent::Pressed(pressed) => popup.pointer_pressed(pressed),
                PopupEvent::Key(code) => key_for(code).and_then(|key| {
                    changed = true;
                    popup.key_pressed(key)
                }),
            };
            if chosen.is_some() {
                return chosen;
            }
        }
        if changed {
            session.present(link, &popup.frame());
        }
        None
    })
    .flatten()
}

/// Whether the loop has been woken for the compositor and not yet run.
static WAKE_PENDING: AtomicBool = AtomicBool::new(false);

fn wake(waker: &Arc<dyn Fn() + Send + Sync>) {
    if !WAKE_PENDING.swap(true, Ordering::SeqCst) {
        waker();
    }
}

/// `winit` only reads the socket when something wakes its own loop, so the
/// compositor's events for this module's objects would wait for the next
/// unrelated one. This thread reads the socket as soon as there is something,
/// the way `wl_display_prepare_read` lets several readers share it, and wakes
/// the loop to run what it read.
fn spawn_reader(conn: Connection, waker: Arc<dyn Fn() + Send + Sync>) {
    std::thread::spawn(move || {
        loop {
            let Some(guard) = conn.prepare_read() else {
                // Events for this module are waiting for the loop to run them.
                wake(&waker);
                std::thread::sleep(Duration::from_millis(2));
                continue;
            };
            let mut poll = libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `poll` is one valid `pollfd` that lives for the call.
            let ready = unsafe { libc::poll(&mut poll, 1, -1) };
            if ready < 0 {
                drop(guard);
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            match guard.read() {
                Ok(_) => wake(&waker),
                Err(WaylandError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uri_list_gives_its_local_files_in_order() {
        let list = "# file:///tmp/commented-out.garden\r\nfile:///home/me/My%20Plan.garden\r\n\r\nhttp://example.com/x\r\nfile://localhost/tmp/b.garden  \r\n";
        assert_eq!(
            parse_uri_list(list),
            vec![
                PathBuf::from("/home/me/My Plan.garden"),
                PathBuf::from("/tmp/b.garden")
            ]
        );
    }

    #[test]
    fn a_list_with_no_local_file_is_empty() {
        assert!(parse_uri_list("http://example.com/\n").is_empty());
        assert!(parse_uri_list("").is_empty());
    }

    #[test]
    fn plain_text_is_preferred_as_utf8() {
        let offered = vec![
            "text/plain".to_owned(),
            "UTF8_STRING".to_owned(),
            "text/html".to_owned(),
        ];
        assert_eq!(pick_text_type(&offered), Some("UTF8_STRING".to_owned()));
        assert_eq!(pick_text_type(&["image/png".to_owned()]), None);
    }
}
