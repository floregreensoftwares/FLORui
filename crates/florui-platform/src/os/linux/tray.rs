//! A tray icon through the StatusNotifierItem protocol (the one GNOME's
//! AppIndicator extension, KDE and most panels host). The item answers
//! `Activate` as a left click and `ContextMenu` as a right click; it exposes
//! no D-Bus menu. GNOME's extension only calls `Activate` (on a double-click)
//! and never `ContextMenu` for an item without a D-Bus menu, so `RightClick`
//! does not arrive there. Other hosts were not checked.
//!
//! D-Bus calls arrive on the connection's own thread. They are queued and the
//! event loop is woken, and the handler runs on the thread that created the
//! icon, like on Windows.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use zbus::blocking::Connection;
use zbus::zvariant::ObjectPath;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    LeftClick,
    RightClick,
}

type Handler = Box<dyn Fn(TrayEvent)>;

thread_local! {
    static HANDLERS: RefCell<HashMap<u64, Handler>> = RefCell::new(HashMap::new());
}

static QUEUE: Mutex<VecDeque<(u64, TrayEvent)>> = Mutex::new(VecDeque::new());
static WAKE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Installed once by the event loop; called from the D-Bus thread when an
/// event is queued.
pub(crate) fn set_waker(wake: impl Fn() + Send + Sync + 'static) {
    let _ = WAKE.set(Box::new(wake));
}

/// Runs the handlers for every queued event, on the calling (UI) thread.
pub(crate) fn deliver_pending() {
    let events: Vec<_> = match QUEUE.lock() {
        Ok(mut queue) => queue.drain(..).collect(),
        Err(_) => return,
    };
    HANDLERS.with(|handlers| {
        let handlers = handlers.borrow();
        for (id, event) in events {
            if let Some(handler) = handlers.get(&id) {
                handler(event);
            }
        }
    });
}

fn push(id: u64, event: TrayEvent) {
    if let Ok(mut queue) = QUEUE.lock() {
        queue.push_back((id, event));
    }
    if let Some(wake) = WAKE.get() {
        wake();
    }
}

type Pixmap = Vec<(i32, i32, Vec<u8>)>;

struct Item {
    id: u64,
    tooltip: String,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    fn activate(&self, _x: i32, _y: i32) {
        push(self.id, TrayEvent::LeftClick);
    }

    fn context_menu(&self, _x: i32, _y: i32) {
        push(self.id, TrayEvent::RightClick);
    }

    fn secondary_activate(&self, _x: i32, _y: i32) {}

    fn scroll(&self, _delta: i32, _orientation: &str) {}

    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }

    #[zbus(property)]
    fn id(&self) -> String {
        format!("florui-{}-{}", std::process::id(), self.id)
    }

    #[zbus(property)]
    fn title(&self) -> String {
        self.tooltip.clone()
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }

    #[zbus(property)]
    fn window_id(&self) -> u32 {
        0
    }

    #[zbus(property)]
    fn icon_name(&self) -> &str {
        "application-x-executable"
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Pixmap {
        Vec::new()
    }

    #[zbus(property)]
    fn tool_tip(&self) -> (String, Pixmap, String, String) {
        (
            String::new(),
            Vec::new(),
            self.tooltip.clone(),
            String::new(),
        )
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn menu(&self) -> ObjectPath<'static> {
        ObjectPath::from_static_str_unchecked("/NO_DBUSMENU")
    }
}

/// A live tray icon; removed when dropped.
pub struct TrayIcon {
    id: u64,
    _connection: Connection,
}

impl TrayIcon {
    /// `None` when there is no session bus, or no panel hosts tray icons.
    pub fn new(tooltip: &str, on_event: impl Fn(TrayEvent) + 'static) -> Option<Self> {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let name = format!("org.kde.StatusNotifierItem-{}-{id}", std::process::id());
        let connection = zbus::blocking::connection::Builder::session()
            .ok()?
            .name(name.as_str())
            .ok()?
            .serve_at(
                "/StatusNotifierItem",
                Item {
                    id,
                    tooltip: tooltip.to_owned(),
                },
            )
            .ok()?
            .build()
            .ok()?;
        if !host_available(&connection) {
            return None;
        }
        register(&connection, &name)?;
        HANDLERS.with(|handlers| handlers.borrow_mut().insert(id, Box::new(on_event)));
        Some(Self {
            id,
            _connection: connection,
        })
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        HANDLERS.with(|handlers| handlers.borrow_mut().remove(&self.id));
    }
}

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";

/// A watcher exists and at least one panel is hosting items.
fn host_available(connection: &Connection) -> bool {
    zbus::blocking::Proxy::new(connection, WATCHER, WATCHER_PATH, WATCHER)
        .ok()
        .and_then(|proxy| {
            proxy
                .get_property::<bool>("IsStatusNotifierHostRegistered")
                .ok()
        })
        .unwrap_or(false)
}

fn register(connection: &Connection, name: &str) -> Option<()> {
    zbus::blocking::Proxy::new(connection, WATCHER, WATCHER_PATH, WATCHER)
        .ok()?
        .call_method("RegisterStatusNotifierItem", &(name,))
        .ok()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_events_reach_the_handler_of_their_icon_only() {
        let seen = std::rc::Rc::new(RefCell::new(Vec::new()));
        let sink = std::rc::Rc::clone(&seen);
        HANDLERS.with(|h| {
            h.borrow_mut()
                .insert(9001, Box::new(move |e| sink.borrow_mut().push(e)))
        });
        push(9001, TrayEvent::LeftClick);
        push(9002, TrayEvent::RightClick);
        push(9001, TrayEvent::RightClick);
        deliver_pending();
        assert_eq!(
            *seen.borrow(),
            vec![TrayEvent::LeftClick, TrayEvent::RightClick]
        );
        deliver_pending();
        assert_eq!(seen.borrow().len(), 2, "events are delivered once");
        HANDLERS.with(|h| h.borrow_mut().remove(&9001));
    }
}
