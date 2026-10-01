//! Mount a component in isolation, drive it with the input a user's hands
//! produce, inspect what it did and compare how it looks, then check that
//! disposing it left nothing behind.
//!
//! The component runs on the same style, layout, text and paint engine as a
//! window, and pointer, keyboard, wheel and IME events go through the code a
//! window's events go through, so a test does not stand in for behavior with
//! private state changes. A [`Harness`] says how to mount (size, scale, theme,
//! context), [`Mounted`] is the running component.
//!
//! ```no_run
//! use florui::prelude::*;
//! use florui_reactive::use_signal;
//! use florui_test::{Harness, by_class};
//!
//! fn counter() -> Element {
//!     let count = use_signal(|| 0);
//!     let clicked = count.clone();
//!     view! {
//!         <div>
//!             <button onclick={move || clicked.set(clicked.get() + 1)}>{"+1"}</button>
//!             <p class="count">{count.get().to_string()}</p>
//!         </div>
//!     }
//! }
//!
//! let mut mounted = Harness::new(counter).mount();
//! let button = mounted.get(florui_test::by_tag("button"));
//! mounted.click(button);
//! let count = mounted.get(by_class("count"));
//! assert_eq!(mounted.text(count), "1");
//! mounted.dispose().assert_clean();
//! ```
//!
//! A test can also assert how much work a change did, and what caused it:
//!
//! ```no_run
//! # use florui::prelude::*;
//! # use florui_test::{Harness, by_tag};
//! # fn counter() -> Element { view! { <button>{"+1"}</button> } }
//! use florui_profile::Counter;
//!
//! let mut mounted = Harness::new(counter).mount();
//! let button = mounted.get(by_tag("button"));
//! let ((), profile) = mounted.profile(|m| m.click(button));
//! assert!(profile.was_dispatched("click", button));
//! assert!(profile.counter(Counter::NodesLaidOut) < 100);
//! ```

mod equivalence;
mod guard;
mod snapshot;

pub use equivalence::{Route, assert_matches_clean};

use florui::Element;
pub use florui_platform::TestKey as Key;
use florui_platform::{
    A11yNode, HeadlessFrame, HeadlessOptions, HeadlessWindow, MemoryClipboard, TestKey,
};
use florui_profile::{Cause, Counter, FrameProfile, Phase};
use florui_reactive::live::{LiveCounts, live_counts};
use florui_style::{Rgba, StyleError};

/// A handle to one element of the mounted component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node(florui_style::NodeId);

/// How to find an element.
#[derive(Clone, Debug)]
pub enum Query {
    Id(String),
    Class(String),
    Tag(String),
    /// The innermost element whose text is exactly this.
    Text(String),
    /// An element with this accessibility role (`Button`, `CheckBox`, `List`,
    /// ...), and this accessible name if one is given.
    Role(String, Option<String>),
}

pub fn by_id(id: &str) -> Query {
    Query::Id(id.to_string())
}

pub fn by_class(class: &str) -> Query {
    Query::Class(class.to_string())
}

pub fn by_tag(tag: &str) -> Query {
    Query::Tag(tag.to_string())
}

pub fn by_text(text: &str) -> Query {
    Query::Text(text.to_string())
}

pub fn by_role(role: &str) -> Query {
    Query::Role(role.to_string(), None)
}

pub fn by_role_named(role: &str, name: &str) -> Query {
    Query::Role(role.to_string(), Some(name.to_string()))
}

/// A box in logical pixels, where the element is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Refuses a viewport whose frame would be too large to paint and copy.
///
/// # Panics
///
/// Panics with the size it was given.
fn check_frame_size(width: f32, height: f32, scale_factor: f64) {
    let pixels = (f64::from(width) * scale_factor) * (f64::from(height) * scale_factor);
    assert!(
        pixels <= guard::MAX_FRAME_PIXELS as f64,
        "a {width} x {height} viewport at a scale of {scale_factor} is a frame of {pixels:.0} 
         pixels; the harness paints at most {} to keep a test from using gigabytes",
        guard::MAX_FRAME_PIXELS
    );
}

/// How a component is mounted.
pub struct Harness {
    root: Box<dyn Fn() -> Element>,
    css: String,
    options: HeadlessOptions,
}

impl Harness {
    pub fn new(root: impl Fn() -> Element + 'static) -> Self {
        Self {
            root: Box::new(root),
            css: String::new(),
            options: HeadlessOptions {
                width: 800.0,
                height: 600.0,
                ..HeadlessOptions::default()
            },
        }
    }

    /// The stylesheet the component is mounted with.
    pub fn css(mut self, css: &str) -> Self {
        self.css = css.to_string();
        self
    }

    /// The viewport, in logical pixels.
    pub fn viewport(mut self, width: f32, height: f32) -> Self {
        self.options.width = width;
        self.options.height = height;
        self
    }

    /// Device pixels per logical pixel.
    pub fn dpr(mut self, scale_factor: f64) -> Self {
        self.options.scale_factor = scale_factor;
        self
    }

    /// What `prefers-color-scheme: dark` reports.
    pub fn dark(mut self, dark: bool) -> Self {
        self.options.prefers_dark = dark;
        self
    }

    /// What `prefers-reduced-motion: reduce` reports.
    pub fn reduced_motion(mut self, reduce: bool) -> Self {
        self.options.prefers_reduced_motion = reduce;
        self
    }

    pub fn canvas_color(mut self, color: Rgba) -> Self {
        self.options.canvas_color = color;
        self
    }

    /// Makes something reachable through `use_context` from the first render
    /// on, the way a host would provide it.
    pub fn context(mut self, provide: impl Fn() + 'static) -> Self {
        self.options.context.push(Box::new(provide));
        self
    }

    /// Mounts the component.
    ///
    /// # Panics
    ///
    /// Panics if the stylesheet does not parse; use [`Self::try_mount`] to
    /// handle that.
    pub fn mount(self) -> Mounted {
        self.try_mount().expect("the stylesheet parses")
    }

    pub fn try_mount(self) -> Result<Mounted, StyleError> {
        let Harness { root, css, options } = self;
        check_frame_size(options.width, options.height, options.scale_factor);
        let before = live_counts();
        let mut window = HeadlessWindow::new(&css, root, options)?;
        window.settle();
        Ok(Mounted {
            window: Some(window),
            before,
            disposed: false,
            _watchdog: guard::Watchdog::abort_after(guard::timeout()),
        })
    }
}

/// What was still alive after a component was disposed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TeardownReport {
    /// Live state that was not there before mounting.
    pub left_behind: LiveCounts,
}

impl TeardownReport {
    pub fn is_clean(&self) -> bool {
        self.left_behind.is_empty()
    }

    /// # Panics
    ///
    /// Panics naming what was left behind.
    pub fn assert_clean(&self) {
        assert!(
            self.is_clean(),
            "the component left state alive after it was disposed: {:?}. A signal or ref held \
             by a callback that captures it, a cleanup that never ran, or a scope kept by a \
             cycle are the usual causes.",
            self.left_behind
        );
    }
}

/// What a stretch of work did, as [`Mounted::profile`] measured it. Times vary
/// from run to run, so assert on counts, counters and causes; use times to
/// compare, not to pin.
#[derive(Debug, Clone)]
pub struct Profile {
    frames: Vec<FrameProfile>,
}

impl Profile {
    /// The frames the work produced, with every span's total.
    pub fn frames(&self) -> &[FrameProfile] {
        &self.frames
    }

    /// What `counter` counted, summed over the frames.
    pub fn counter(&self, counter: Counter) -> u64 {
        self.frames.iter().map(|f| f.counter(counter)).sum()
    }

    /// How many times `phase` ran.
    pub fn calls(&self, phase: Phase) -> u32 {
        self.frames
            .iter()
            .map(|f| f.phase(phase).map_or(0, |p| p.calls))
            .sum()
    }

    /// The time `phase` took, summed.
    pub fn time(&self, phase: Phase) -> std::time::Duration {
        self.frames.iter().map(|f| f.total(phase)).sum()
    }

    /// What caused the work, in the order it happened: a click, a signal
    /// write, a resource.
    pub fn causes(&self) -> Vec<&Cause> {
        self.frames.iter().flat_map(|f| f.causes.iter()).collect()
    }

    /// Whether a signal was written while the handler of `name` on `node` ran:
    /// the write is attributed to that event.
    pub fn written_during(&self, name: &str, node: Node) -> bool {
        self.causes()
            .iter()
            .any(|c| c.kind == "signal-write" && c.event == Some((name, node.0)))
    }

    /// Whether an event named `name` (`click`, ...) was dispatched on `node`.
    pub fn was_dispatched(&self, name: &str, node: Node) -> bool {
        self.causes()
            .iter()
            .any(|c| c.kind == "event" && c.event == Some((name, node.0)))
    }
}

/// A component running under test.
pub struct Mounted {
    window: Option<HeadlessWindow>,
    before: LiveCounts,
    disposed: bool,
    /// Ends the process if the component stays mounted past the time budget.
    _watchdog: guard::Watchdog,
}

impl Mounted {
    fn window(&self) -> &HeadlessWindow {
        self.window.as_ref().expect("the component is mounted")
    }

    fn window_mut(&mut self) -> &mut HeadlessWindow {
        self.window.as_mut().expect("the component is mounted")
    }

    // Queries

    /// Every element matching `query`, in document order.
    pub fn all(&mut self, query: Query) -> Vec<Node> {
        let roles = matches!(query, Query::Role(..))
            .then(|| self.window_mut().frame().accessibility)
            .unwrap_or_default();
        let (arena, ..) = self.window().runtime().geometry();
        match &query {
            Query::Id(id) => arena
                .find_all(|a, node| a.id_attr(node) == Some(id.as_str()))
                .into_iter()
                .map(Node)
                .collect(),
            Query::Class(class) => arena
                .find_all(|a, node| a.classes(node).iter().any(|c| c == class))
                .into_iter()
                .map(Node)
                .collect(),
            Query::Tag(tag) => arena
                .find_all(|a, node| a.tag(node) == tag)
                .into_iter()
                .map(Node)
                .collect(),
            Query::Text(text) => {
                let matching = arena.find_all(|a, node| a.text_content(node) == text.as_str());
                // The innermost one: a parent with one child of the same text
                // matches too, and is not what the test means.
                matching
                    .iter()
                    .copied()
                    .filter(|&node| {
                        !arena
                            .children(node)
                            .iter()
                            .any(|child| matching.contains(child))
                    })
                    .map(Node)
                    .collect()
            }
            Query::Role(role, name) => roles
                .iter()
                .filter(|node| node.role == *role)
                .filter(|node| name.as_ref().is_none_or(|n| node.name.as_ref() == Some(n)))
                .map(|node| Node(node.node))
                .collect(),
        }
    }

    pub fn query(&mut self, query: Query) -> Option<Node> {
        self.all(query).into_iter().next()
    }

    /// The first element matching `query`.
    ///
    /// # Panics
    ///
    /// Panics when nothing matches, saying what was asked for.
    pub fn get(&mut self, query: Query) -> Node {
        let described = format!("{query:?}");
        self.query(query)
            .unwrap_or_else(|| panic!("no element matches {described}"))
    }

    // Reading

    /// The text of the element and everything inside it.
    pub fn text(&self, node: Node) -> String {
        let (arena, ..) = self.window().runtime().geometry();
        arena.text_content(node.0).to_string()
    }

    /// Where the element is drawn, in logical pixels.
    pub fn bounds(&mut self, node: Node) -> Option<Rect> {
        let scale = self.window().scale_factor() as f32;
        let frame = self.window_mut().frame();
        frame
            .bounds
            .get(&node.0)
            .map(|&(x, y, width, height)| Rect {
                x: x / scale,
                y: y / scale,
                width: width / scale,
                height: height / scale,
            })
    }

    /// The accessibility entry of the element, if it has one.
    pub fn accessibility(&mut self, node: Node) -> Option<A11yNode> {
        self.window_mut()
            .frame()
            .accessibility
            .into_iter()
            .find(|entry| entry.node == node.0)
    }

    /// Every accessibility node, as an assistive technology would see them.
    pub fn accessibility_tree(&mut self) -> Vec<A11yNode> {
        self.window_mut().frame().accessibility
    }

    /// The element that has keyboard focus.
    pub fn focused(&self) -> Option<Node> {
        self.window().runtime().focused().map(Node)
    }

    pub fn is_focused(&self, node: Node) -> bool {
        self.focused() == Some(node)
    }

    pub fn is_hovered(&self, node: Node) -> bool {
        self.window().runtime().hovered() == Some(node.0)
    }

    /// The text on the clipboard the component sees.
    pub fn clipboard(&self) -> &MemoryClipboard {
        self.window().clipboard()
    }

    /// The painted frame: pixels, where each element is drawn, accessibility.
    pub fn frame(&mut self) -> HeadlessFrame {
        self.window_mut().frame()
    }

    /// The tree of the mounted component, for a check that needs to name
    /// elements.
    pub(crate) fn arena(&self) -> &florui_style::Arena {
        self.window().runtime().geometry().0
    }

    // Input

    /// Moves to the middle of the element and presses and releases the
    /// primary button.
    ///
    /// # Panics
    ///
    /// Panics if the element is not drawn.
    pub fn click(&mut self, node: Node) {
        let (x, y) = self.center(node);
        self.window_mut().click_at(x, y);
        self.window_mut().settle();
    }

    pub fn click_at(&mut self, x: f32, y: f32) {
        self.window_mut().click_at(x, y);
        self.window_mut().settle();
    }

    /// Moves the pointer onto the element.
    pub fn hover(&mut self, node: Node) {
        let (x, y) = self.center(node);
        self.window_mut().pointer_move(x, y);
        self.window_mut().settle();
    }

    /// Moves the pointer to `(x, y)` in logical pixels, without pressing.
    pub fn move_pointer(&mut self, x: f32, y: f32) {
        self.window_mut().pointer_move(x, y);
        self.window_mut().settle();
    }

    /// Turns the wheel over wherever the pointer is: `dy` pixels down moves the
    /// content up. Unlike [`Self::scroll`] it does not move the pointer first.
    pub fn wheel(&mut self, dx: f32, dy: f32) {
        self.window_mut().wheel(-dx, -dy, false);
        self.window_mut().settle();
    }

    /// Replaces the stylesheet and re-renders, as a reload of the file does.
    ///
    /// # Panics
    ///
    /// Panics if `css` does not parse.
    pub fn reload_css(&mut self, css: &str) {
        let rules = florui_style::parse_stylesheet(css).expect("the stylesheet parses");
        self.window_mut().runtime_mut().set_rules(rules);
        self.window_mut().update();
        self.window_mut().settle();
    }

    /// Presses and releases a key.
    pub fn press(&mut self, key: TestKey) {
        self.window_mut().press_key(key);
        self.window_mut().settle();
    }

    /// Types `text` one character at a time.
    pub fn type_text(&mut self, text: &str) {
        self.window_mut().type_text(text);
        self.window_mut().settle();
    }

    /// Holds the modifier keys for what follows.
    pub fn modifiers(&mut self, control: bool, shift: bool, alt: bool) {
        self.window_mut().set_modifiers(control, shift, alt);
    }

    /// Commits `text` as an input method would.
    pub fn ime_commit(&mut self, text: &str) {
        self.window_mut().ime_commit(text);
        self.window_mut().settle();
    }

    /// Scrolls by `(dx, dy)` logical pixels with the pointer over the element.
    pub fn scroll(&mut self, node: Node, dx: f32, dy: f32) {
        let (x, y) = self.center(node);
        self.window_mut().pointer_move(x, y);
        self.window_mut().wheel(-dx, -dy, false);
        self.window_mut().settle();
    }

    /// Resizes the viewport.
    pub fn resize(&mut self, width: f32, height: f32) {
        let scale = self.window().scale_factor();
        check_frame_size(width, height, scale);
        self.window_mut().resize(width, height);
        self.window_mut().settle();
    }

    /// Moves the clock `seconds` forward, stepping transitions and animations.
    pub fn advance(&mut self, seconds: f64) {
        self.window_mut().advance_clock(seconds);
        self.window_mut().settle();
    }

    /// Runs pending async work and re-renders until nothing is left.
    pub fn settle(&mut self) {
        self.window_mut().settle();
    }

    fn center(&mut self, node: Node) -> (f32, f32) {
        let rect = self
            .bounds(node)
            .expect("the element is drawn, so it has a box to click");
        (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
    }

    /// Runs `work` and then paints a frame, and reports what that cost: what
    /// caused it, how many elements each stage touched, how the text and image
    /// caches answered, and how long each stage took. A test can assert that a
    /// change did little work (or that a cache answered) as well as that it
    /// looked right.
    ///
    /// Everything between the start of `work` and the end of the frame counts,
    /// so the update the work triggered and the painting that follows are both
    /// in it. Calls do not nest.
    pub fn profile<R>(&mut self, work: impl FnOnce(&mut Mounted) -> R) -> (R, Profile) {
        florui_profile::start(false);
        let result = work(self);
        self.window_mut().frame();
        florui_profile::stop();
        let frames = florui_profile::recent_frames(1);
        (result, Profile { frames })
    }

    // Assertions

    /// Asserts the element is drawn at `expected`, to within `tolerance`.
    ///
    /// # Panics
    ///
    /// Panics showing the box it has instead.
    pub fn assert_bounds(&mut self, node: Node, expected: Rect, tolerance: f32) {
        let actual = self
            .bounds(node)
            .unwrap_or_else(|| panic!("the element is not drawn, so it has no bounds"));
        let close = |a: f32, b: f32| (a - b).abs() <= tolerance;
        assert!(
            close(actual.x, expected.x)
                && close(actual.y, expected.y)
                && close(actual.width, expected.width)
                && close(actual.height, expected.height),
            "bounds differ: expected {expected:?}, drawn at {actual:?} (tolerance {tolerance})"
        );
    }

    /// Compares the painted frame with the baseline image `name`; see
    /// [`snapshot`]'s module for where baselines live and how to update them.
    pub fn assert_snapshot(&mut self, name: &str) {
        let frame = self.frame();
        snapshot::assert_matches(name, &frame);
    }

    // Teardown

    /// Drops the component and reports what it left alive.
    pub fn dispose(mut self) -> TeardownReport {
        self.teardown()
    }

    fn teardown(&mut self) -> TeardownReport {
        self.disposed = true;
        drop(self.window.take());
        TeardownReport {
            left_behind: live_counts().since(self.before),
        }
    }
}

impl Drop for Mounted {
    /// A component dropped without [`Mounted::dispose`] is still checked, so
    /// a leak fails the test even when the test forgot to ask.
    fn drop(&mut self) {
        if self.disposed {
            return;
        }
        let report = self.teardown();
        if !std::thread::panicking() {
            report.assert_clean();
        }
    }
}
