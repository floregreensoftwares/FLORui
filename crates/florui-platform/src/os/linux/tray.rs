//! A tray icon through the StatusNotifierItem protocol (the one GNOME's
//! AppIndicator extension, KDE and most panels host). The item answers
//! `Activate` as a left click and `ContextMenu` as a right click. A host that
//! only opens a D-Bus menu on a right-click (GNOME's extension never calls
//! `ContextMenu`) is given an empty one, and the moment it asks to show it is
//! delivered as the right click.
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

const MENU_PATH: &str = "/StatusNotifierMenu";

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
        ObjectPath::from_static_str_unchecked(MENU_PATH)
    }
}

/// The `com.canonical.dbusmenu` object that panels such as GNOME's AppIndicator
/// extension open on a right-click. It has no entries of its own: the panel
/// asks whether it should show the menu (`AboutToShow`) just before it does,
/// which is how a right-click reaches the application on a host that never
/// calls `ContextMenu`.
struct Menu {
    id: u64,
}

type Layout = (
    i32,
    HashMap<String, zbus::zvariant::OwnedValue>,
    Vec<zbus::zvariant::OwnedValue>,
);

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    fn get_layout(&self, _parent: i32, _depth: i32, _properties: Vec<String>) -> (u32, Layout) {
        (1, (0, HashMap::new(), Vec::new()))
    }

    fn get_group_properties(
        &self,
        _ids: Vec<i32>,
        _properties: Vec<String>,
    ) -> Vec<(i32, HashMap<String, zbus::zvariant::OwnedValue>)> {
        Vec::new()
    }

    fn get_property(&self, _id: i32, _name: &str) -> zbus::fdo::Result<zbus::zvariant::OwnedValue> {
        Err(zbus::fdo::Error::InvalidArgs("no such property".to_owned()))
    }

    fn event(&self, _id: i32, _event: &str, _data: zbus::zvariant::Value<'_>, _timestamp: u32) {}

    fn about_to_show(&self, id: i32) -> bool {
        if id == 0 {
            push(self.id, TrayEvent::RightClick);
        }
        false
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
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
            .serve_at(MENU_PATH, Menu { id })
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
    fn a_menu_about_to_show_is_a_right_click_and_has_nothing_to_show() {
        let seen = std::rc::Rc::new(RefCell::new(Vec::new()));
        let sink = std::rc::Rc::clone(&seen);
        HANDLERS.with(|h| {
            h.borrow_mut()
                .insert(9003, Box::new(move |e| sink.borrow_mut().push(e)))
        });
        let menu = Menu { id: 9003 };
        assert!(!menu.about_to_show(0), "the layout is never out of date");
        assert!(!menu.about_to_show(5), "an entry is not the menu");
        deliver_pending();
        assert_eq!(*seen.borrow(), vec![TrayEvent::RightClick]);
        let (_, (root, _, children)) = menu.get_layout(0, -1, Vec::new());
        assert_eq!((root, children.len()), (0, 0));
        HANDLERS.with(|h| h.borrow_mut().remove(&9003));
    }

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
