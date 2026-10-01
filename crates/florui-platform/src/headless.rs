//! A window with no window: a [`UiRuntime`] driven by the same input handling
//! and painted by the same code as a desktop window, for tests. The pointer,
//! keyboard, wheel and IME reach it as they reach a real window, and what it
//! reports back (cursor shape, redraw requests, window commands) is recorded
//! instead of shown.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use florui::Element;
use florui_style::{NodeId, Rgba, StyleError};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, MouseScrollDelta};
use winit::keyboard::{
    Key, KeyCode, ModifiersState, NamedKey, NativeKeyCode, PhysicalKey, SmolStr,
};
use winit::window::{CursorIcon, ResizeDirection};

use crate::appearance::DecorationMode;
use crate::clipboard::MemoryClipboard;
use crate::desktop::layout_viewport;
use crate::dpi::{self, ViewportScale};
use crate::frame::{PaintSources, build_paint_parts};
use crate::input::{Input, InputHost, InputState, KeyInput};
use crate::{OpenUrlOutcome, UiRuntime};

/// How a [`HeadlessWindow`] is set up.
pub struct HeadlessOptions {
    /// Logical size of the window's content.
    pub width: f32,
    pub height: f32,
    /// Device pixels per logical pixel.
    pub scale_factor: f64,
    pub canvas_color: Rgba,
    /// What `prefers-color-scheme: dark` reports.
    pub prefers_dark: bool,
    /// What `prefers-reduced-motion: reduce` reports.
    pub prefers_reduced_motion: bool,
    /// Whether an assistive technology is listening. The accessibility tree is
    /// built for a frame only when it is, the way the desktop host does; a
    /// window nobody listens to builds none.
    pub assistive_technology: bool,
    /// Providers made reachable through context from the first render on,
    /// for a component that reads something its host supplies.
    pub context: Vec<Box<dyn Fn()>>,
}

impl Default for HeadlessOptions {
    fn default() -> Self {
        Self {
            width: 800.0,
            height: 600.0,
            scale_factor: 1.0,
            canvas_color: Rgba::opaque(0x10, 0x10, 0x14),
            prefers_dark: false,
            prefers_reduced_motion: false,
            assistive_technology: true,
            context: Vec::new(),
        }
    }
}

/// A key a test presses, without the window system's event type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestKey {
    Enter,
    Tab,
    Escape,
    Space,
    Backspace,
    Delete,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    /// A printable character; letters also set the physical key, which
    /// shortcuts such as Ctrl+A read.
    Character(char),
}

impl TestKey {
    fn logical(self) -> Key {
        match self {
            TestKey::Enter => Key::Named(NamedKey::Enter),
            TestKey::Tab => Key::Named(NamedKey::Tab),
            TestKey::Escape => Key::Named(NamedKey::Escape),
            TestKey::Space => Key::Named(NamedKey::Space),
            TestKey::Backspace => Key::Named(NamedKey::Backspace),
            TestKey::Delete => Key::Named(NamedKey::Delete),
            TestKey::ArrowUp => Key::Named(NamedKey::ArrowUp),
            TestKey::ArrowDown => Key::Named(NamedKey::ArrowDown),
            TestKey::ArrowLeft => Key::Named(NamedKey::ArrowLeft),
            TestKey::ArrowRight => Key::Named(NamedKey::ArrowRight),
            TestKey::Home => Key::Named(NamedKey::Home),
            TestKey::End => Key::Named(NamedKey::End),
            TestKey::PageUp => Key::Named(NamedKey::PageUp),
            TestKey::PageDown => Key::Named(NamedKey::PageDown),
            TestKey::Character(ch) => Key::Character(SmolStr::new(ch.to_string())),
        }
    }

    fn physical(self) -> PhysicalKey {
        let code = match self {
            TestKey::Character(ch) => match ch.to_ascii_lowercase() {
                'a' => Some(KeyCode::KeyA),
                'c' => Some(KeyCode::KeyC),
                'v' => Some(KeyCode::KeyV),
                'x' => Some(KeyCode::KeyX),
                'y' => Some(KeyCode::KeyY),
                'z' => Some(KeyCode::KeyZ),
                _ => None,
            },
            TestKey::Enter => Some(KeyCode::Enter),
            TestKey::Tab => Some(KeyCode::Tab),
            TestKey::Escape => Some(KeyCode::Escape),
            TestKey::Space => Some(KeyCode::Space),
            TestKey::Backspace => Some(KeyCode::Backspace),
            TestKey::Delete => Some(KeyCode::Delete),
            TestKey::ArrowUp => Some(KeyCode::ArrowUp),
            TestKey::ArrowDown => Some(KeyCode::ArrowDown),
            TestKey::ArrowLeft => Some(KeyCode::ArrowLeft),
            TestKey::ArrowRight => Some(KeyCode::ArrowRight),
            TestKey::Home => Some(KeyCode::Home),
            TestKey::End => Some(KeyCode::End),
            TestKey::PageUp => Some(KeyCode::PageUp),
            TestKey::PageDown => Some(KeyCode::PageDown),
        };
        match code {
            Some(code) => PhysicalKey::Code(code),
            None => PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
        }
    }
}

/// One node of the accessibility tree, as an assistive technology would see
/// it.
#[derive(Clone, Debug, PartialEq)]
pub struct A11yNode {
    /// The element it describes.
    pub node: NodeId,
    /// The role, as AccessKit names it (`Button`, `CheckBox`, `List`, ...).
    pub role: String,
    pub name: Option<String>,
    pub value: Option<String>,
    /// `(x, y, width, height)` in physical pixels.
    pub bounds: Option<(f32, f32, f32, f32)>,
    pub focused: bool,
    pub disabled: bool,
    pub expanded: Option<bool>,
    /// `"true"`, `"false"` or `"mixed"` for a checkable node.
    pub toggled: Option<String>,
}

/// A painted frame and what describes it.
pub struct HeadlessFrame {
    pub width: u32,
    pub height: u32,
    /// Premultiplied RGBA, rows top to bottom.
    pub rgba: Vec<u8>,
    /// Each node's box `(x, y, width, height)` in physical pixels, where it
    /// is drawn.
    pub bounds: HashMap<NodeId, (f32, f32, f32, f32)>,
    pub accessibility: Vec<A11yNode>,
}

/// What the window around the input handling was asked to do.
struct HeadlessHost {
    scale_factor: f64,
    size: (f32, f32),
    prefers_reduced_motion: bool,
    redraws: Cell<u32>,
    cursor: Cell<CursorIcon>,
    ime_allowed: Cell<bool>,
    animating: bool,
    commands: RefCell<Vec<String>>,
}

impl InputHost for HeadlessHost {
    fn viewport_scale(&self) -> ViewportScale {
        let physical = PhysicalSize::new(
            (self.size.0 as f64 * self.scale_factor).round() as u32,
            (self.size.1 as f64 * self.scale_factor).round() as u32,
        );
        dpi::viewport_scale(physical, self.scale_factor)
    }

    fn request_redraw(&self) {
        self.redraws.set(self.redraws.get() + 1);
    }

    fn decorations(&self) -> DecorationMode {
        DecorationMode::default()
    }

    fn prefers_reduced_motion(&self) -> bool {
        self.prefers_reduced_motion
    }

    fn set_animating(&mut self, animating: bool) {
        self.animating = animating;
    }

    fn set_ime_allowed(&self, allowed: bool) {
        self.ime_allowed.set(allowed);
    }

    fn outer_position(&self) -> Result<PhysicalPosition<i32>, winit::error::NotSupportedError> {
        Ok(PhysicalPosition::new(0, 0))
    }

    fn drag(&self) {
        self.commands.borrow_mut().push("drag".to_string());
    }

    fn start_resize(&self, direction: ResizeDirection) {
        self.commands
            .borrow_mut()
            .push(format!("resize {direction:?}"));
    }

    fn toggle_maximize(&self) {
        self.commands.borrow_mut().push("maximize".to_string());
    }

    fn set_content_cursor(&self, icon: CursorIcon) {
        self.cursor.set(icon);
    }

    fn set_resize_cursor(&self, direction: Option<ResizeDirection>) {
        if let Some(direction) = direction {
            self.commands
                .borrow_mut()
                .push(format!("resize cursor {direction:?}"));
        }
    }

    fn show_system_menu_at_cursor(&self) {
        self.commands.borrow_mut().push("system menu".to_string());
    }

    fn show_system_menu_at(&self, screen_x: i32, screen_y: i32) {
        self.commands
            .borrow_mut()
            .push(format!("system menu at {screen_x},{screen_y}"));
    }

    fn open_url(&self, url: &str) -> OpenUrlOutcome {
        self.commands.borrow_mut().push(format!("open {url}"));
        OpenUrlOutcome::Unavailable
    }

    fn is_picking(&self) -> bool {
        false
    }

    fn pointer_moved(&self, _node: Option<NodeId>) {}

    fn picked(&self, _node: Option<NodeId>) {}
}

/// A [`UiRuntime`] with simulated input and no window.
pub struct HeadlessWindow {
    runtime: UiRuntime,
    input: InputState,
    host: HeadlessHost,
    clipboard: MemoryClipboard,
    canvas_color: Rgba,
    accessibility_tree: crate::accessibility::tree::AccessibilityTree,
    assistive_technology: bool,
    clock: f64,
}

impl HeadlessWindow {
    /// Parses `css`, renders `root` once and returns the window.
    pub fn new(
        css: &str,
        root: impl Fn() -> Element + 'static,
        options: HeadlessOptions,
    ) -> Result<Self, StyleError> {
        let rules = florui_style::parse_stylesheet(css)?;
        let host = HeadlessHost {
            scale_factor: options.scale_factor,
            size: (options.width, options.height),
            prefers_reduced_motion: options.prefers_reduced_motion,
            redraws: Cell::new(0),
            cursor: Cell::new(CursorIcon::Default),
            ime_allowed: Cell::new(false),
            animating: false,
            commands: RefCell::new(Vec::new()),
        };
        let viewport = layout_viewport(host.viewport_scale());
        let mut runtime = UiRuntime::with_rules_and_context(
            rules,
            root,
            viewport,
            options.context,
            true,
            options.prefers_reduced_motion,
            options.prefers_dark,
        );
        runtime.set_manual_clock(Some(0.0));
        Ok(Self {
            runtime,
            input: InputState::default(),
            host,
            clipboard: MemoryClipboard::new(),
            canvas_color: options.canvas_color,
            accessibility_tree: crate::accessibility::tree::AccessibilityTree::new(),
            assistive_technology: options.assistive_technology,
            clock: 0.0,
        })
    }

    /// Whether an assistive technology is listening, which is what decides
    /// whether a frame builds the accessibility tree.
    pub fn assistive_technology(&self) -> bool {
        self.assistive_technology
    }

    /// Attaches or detaches an assistive technology. The next frame after
    /// attaching builds the tree from scratch, as the desktop does when a
    /// screen reader asks for its first one; while detached, a frame builds
    /// none and [`HeadlessFrame::accessibility`] is empty.
    pub fn set_assistive_technology(&mut self, listening: bool) {
        self.assistive_technology = listening;
    }

    /// Device pixels per logical pixel.
    pub fn scale_factor(&self) -> f64 {
        self.host.scale_factor
    }

    pub fn runtime(&self) -> &UiRuntime {
        &self.runtime
    }

    pub fn runtime_mut(&mut self) -> &mut UiRuntime {
        &mut self.runtime
    }

    pub fn clipboard(&self) -> &MemoryClipboard {
        &self.clipboard
    }

    /// The cursor shape the last pointer movement asked for.
    pub fn cursor_icon(&self) -> CursorIcon {
        self.host.cursor.get()
    }

    /// Whether the last focus change asked for IME to be enabled.
    pub fn ime_allowed(&self) -> bool {
        self.host.ime_allowed.get()
    }

    /// How many repaints the window was asked for.
    pub fn redraw_requests(&self) -> u32 {
        self.host.redraws.get()
    }

    /// Window commands the content asked for (drag, maximize, resize, the
    /// system menu, opening a link), oldest first.
    pub fn window_commands(&self) -> Vec<String> {
        self.host.commands.borrow().clone()
    }

    fn with_input<R>(&mut self, f: impl FnOnce(&mut Input<'_, HeadlessHost>) -> R) -> R {
        let mut input = Input {
            runtime: &mut self.runtime,
            state: &mut self.input,
            host: &mut self.host,
        };
        f(&mut input)
    }

    /// Re-renders against the current size, as a window does after a change.
    pub fn update(&mut self) {
        self.with_input(|input| input.update_and_request_redraw());
    }

    /// Runs pending async work and re-renders until nothing is left to do.
    pub fn settle(&mut self) {
        for _ in 0..16 {
            self.runtime.executor().run_until_stalled();
            if !self.runtime.is_dirty() {
                return;
            }
            self.update();
        }
    }

    /// Moves the content clock forward by `seconds` and re-renders, which
    /// steps transitions and animations deterministically.
    pub fn advance_clock(&mut self, seconds: f64) {
        self.clock += seconds;
        self.runtime.set_manual_clock(Some(self.clock));
        self.update();
    }

    /// Resizes the window's content to `width` by `height` logical pixels.
    pub fn resize(&mut self, width: f32, height: f32) {
        self.host.size = (width, height);
        self.update();
    }

    /// Which modifier keys are held for the keys that follow.
    pub fn set_modifiers(&mut self, control: bool, shift: bool, alt: bool) {
        let mut state = ModifiersState::empty();
        state.set(ModifiersState::CONTROL, control);
        state.set(ModifiersState::SHIFT, shift);
        state.set(ModifiersState::ALT, alt);
        self.input.modifiers = state;
    }

    /// Moves the pointer to `(x, y)` logical pixels.
    pub fn pointer_move(&mut self, x: f32, y: f32) {
        let factor = self.host.scale_factor;
        self.with_input(|input| input.handle_cursor_moved(x as f64 * factor, y as f64 * factor));
    }

    pub fn pointer_press(&mut self) {
        self.with_input(|input| input.handle_press());
    }

    pub fn pointer_release(&mut self) {
        self.with_input(|input| input.handle_release());
    }

    pub fn pointer_leave(&mut self) {
        self.with_input(|input| input.handle_cursor_left());
    }

    /// Moves to `(x, y)`, presses and releases the primary button.
    pub fn click_at(&mut self, x: f32, y: f32) {
        self.pointer_move(x, y);
        self.pointer_press();
        self.pointer_release();
    }

    /// One wheel notch (`lines`) or trackpad pixels; positive `y` scrolls
    /// the content up, as a wheel turned towards the user does.
    pub fn wheel(&mut self, x: f32, y: f32, lines: bool) {
        let delta = if lines {
            MouseScrollDelta::LineDelta(x, y)
        } else {
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(x as f64, y as f64))
        };
        self.with_input(|input| input.handle_mouse_wheel(delta));
    }

    /// Presses and releases `key`.
    pub fn press_key(&mut self, key: TestKey) {
        self.key(key, true);
        self.key(key, false);
    }

    /// A key going down (`pressed`) or coming up.
    pub fn key(&mut self, key: TestKey, pressed: bool) {
        let input = KeyInput {
            logical_key: key.logical(),
            physical_key: key.physical(),
            state: if pressed {
                ElementState::Pressed
            } else {
                ElementState::Released
            },
            repeat: false,
        };
        let clipboard = &self.clipboard;
        let mut handler = Input {
            runtime: &mut self.runtime,
            state: &mut self.input,
            host: &mut self.host,
        };
        handler.handle_keyboard_input(input, clipboard);
    }

    /// Types `text` one character at a time.
    pub fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            self.press_key(TestKey::Character(ch));
        }
    }

    /// Commits `text` as an input method would.
    pub fn ime_commit(&mut self, text: &str) {
        self.with_input(|input| input.handle_ime_event(Ime::Commit(text.to_string())));
    }

    /// Paints a frame and describes it, through the code a window paints with.
    pub fn frame(&mut self) -> HeadlessFrame {
        let _frame_end = crate::frame::FrameEnd;
        let scale_factor = self.host.scale_factor;
        let physical = (
            (self.host.size.0 as f64 * scale_factor).round() as u32,
            (self.host.size.1 as f64 * scale_factor).round() as u32,
        );
        let scroll_registry = self.runtime.scroll_registry();
        let text_input_registry = self.runtime.text_input_registry();
        let image_registry = self.runtime.image_registry();
        let icon_registry = self.runtime.icon_registry();
        let asset_cache = self.runtime.asset_cache();
        let executor = self.runtime.executor();
        let focused = self.runtime.focused();
        let hovered = self.runtime.hovered();
        let cursor = (
            (self.input.last_cursor.0 / scale_factor) as f32,
            (self.input.last_cursor.1 / scale_factor) as f32,
        );
        let scroll_dragging = self.input.scroll_drag.as_ref().map(|drag| drag.id.clone());
        let (arena, styles, layouts, font, interaction) =
            self.runtime.geometry_font_and_interaction_mut();
        let parts = build_paint_parts(PaintSources {
            arena,
            styles,
            layouts,
            font,
            scroll_registry: &scroll_registry,
            text_input_registry: &text_input_registry,
            image_registry: &image_registry,
            icon_registry: &icon_registry,
            asset_cache: &asset_cache,
            executor: &*executor,
            focused,
            hovered,
            cursor,
            scroll_dragging: scroll_dragging.as_deref(),
            scale_factor,
        });
        let bounds = florui_layout::screen_bounds(
            arena,
            &parts.physical_layouts,
            styles,
            scale_factor as f32,
        );
        let accessibility_span = florui_profile::span(florui_profile::Phase::Accessibility);
        // Only while something listens, as in the desktop host.
        let accessibility: Vec<A11yNode> = if !self.assistive_technology {
            Vec::new()
        } else {
            let (update, reverse) =
                self.accessibility_tree
                    .build(arena, focused, &bounds, interaction);
            update
                .nodes
                .iter()
                .filter_map(|(id, node)| {
                    let target = *reverse.get(id)?;
                    let rect = node.bounds();
                    Some(A11yNode {
                        node: target,
                        role: format!("{:?}", node.role()),
                        name: node.label().map(str::to_owned),
                        value: node.value().map(str::to_owned),
                        bounds: rect.map(|r| {
                            (
                                r.x0 as f32,
                                r.y0 as f32,
                                (r.x1 - r.x0) as f32,
                                (r.y1 - r.y0) as f32,
                            )
                        }),
                        focused: focused == Some(target),
                        disabled: node.is_disabled(),
                        expanded: node.is_expanded(),
                        toggled: node
                            .toggled()
                            .map(|toggled| format!("{toggled:?}").to_lowercase()),
                    })
                })
                .collect()
        };
        drop(accessibility_span);
        let raster_span = florui_profile::span(florui_profile::Phase::Raster);
        let canvas = florui_paint::paint_to_buffer_with_desktop_extras(
            font,
            physical.0,
            physical.1,
            self.canvas_color,
            arena,
            styles,
            &parts.physical_layouts,
            scale_factor as f32,
            Some(&parts.text_inputs),
            Some(&parts.images),
        );
        drop(raster_span);
        HeadlessFrame {
            width: physical.0,
            height: physical.1,
            rgba: canvas.data().to_vec(),
            bounds,
            accessibility,
        }
    }
}

#[cfg(test)]
mod tests {
    use florui::prelude::*;
    use florui_reactive::{Binding, use_signal};

    use super::*;
    use crate::ClipboardAccess;

    const CSS: &str = ".page { display: flex; flex-direction: column; } \
        .go { width: 80px; height: 30px; } \
        .box { width: 100px; height: 50px; overflow-y: auto; } \
        .tall { height: 400px; }";

    fn window(root: impl Fn() -> Element + 'static) -> HeadlessWindow {
        HeadlessWindow::new(
            CSS,
            root,
            HeadlessOptions {
                width: 300.0,
                height: 300.0,
                ..HeadlessOptions::default()
            },
        )
        .expect("the stylesheet parses")
    }

    fn first(window: &HeadlessWindow, tag: &str) -> NodeId {
        let (arena, ..) = window.runtime().geometry();
        arena
            .find(|a, id| a.tag(id) == tag)
            .unwrap_or_else(|| panic!("no <{tag}>"))
    }

    fn center(window: &mut HeadlessWindow, node: NodeId) -> (f32, f32) {
        let (x, y, w, h) = window.frame().bounds[&node];
        (x + w / 2.0, y + h / 2.0)
    }

    fn text_of(window: &HeadlessWindow, class: &str) -> String {
        let (arena, ..) = window.runtime().geometry();
        let node = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == class))
            .unwrap_or_else(|| panic!("no .{class}"));
        arena.text_content(node).to_string()
    }

    fn counter() -> Element {
        let count = use_signal(|| 0);
        let clicked = count.clone();
        view! {
            <div class="page">
                <button class="go" onclick={move || clicked.set(clicked.get() + 1)}>{"Go"}</button>
                <p class="count">{count.get().to_string()}</p>
            </div>
        }
    }

    #[test]
    fn a_click_reaches_the_button_under_it() {
        let mut window = window(counter);
        let button = first(&window, "button");
        let (x, y) = center(&mut window, button);
        window.click_at(x, y);
        window.settle();
        assert_eq!(text_of(&window, "count"), "1");
        window.click_at(290.0, 290.0);
        window.settle();
        assert_eq!(
            text_of(&window, "count"),
            "1",
            "a click elsewhere does nothing"
        );
    }

    fn click_the_button_and_paint() -> HeadlessFrame {
        let mut window = window(counter);
        let button = first(&window, "button");
        let (x, y) = center(&mut window, button);
        window.click_at(x, y);
        window.settle();
        window.frame()
    }

    #[test]
    fn profiling_does_not_change_what_a_frame_shows() {
        let plain = click_the_button_and_paint();
        florui_profile::start(true);
        let profiled = click_the_button_and_paint();
        florui_profile::stop();

        assert_eq!(plain.rgba, profiled.rgba, "the pixels are the same");
        assert_eq!(plain.bounds, profiled.bounds, "the geometry is the same");
        assert_eq!(plain.accessibility, profiled.accessibility);
    }

    #[test]
    fn a_frame_is_profiled_by_phase_with_its_causes() {
        use florui_profile::Phase;

        let mut window = window(counter);
        let button = first(&window, "button");
        let (x, y) = center(&mut window, button);
        florui_profile::start(true);
        window.click_at(x, y);
        window.settle();
        window.frame();
        let frames = florui_profile::frames();
        florui_profile::stop();

        let frame = frames.last().expect("the frame was profiled");
        for phase in [
            Phase::Update,
            Phase::Render,
            Phase::ArenaBuild,
            Phase::Cascade,
            Phase::Layout,
            Phase::Observers,
            Phase::PaintParts,
            Phase::Accessibility,
            Phase::Raster,
        ] {
            assert!(frame.phase(phase).is_some(), "{phase:?} did not run");
        }
        let update = frame.total(Phase::Update);
        let inside: std::time::Duration = [
            Phase::Render,
            Phase::Async,
            Phase::ArenaBuild,
            Phase::Sync,
            Phase::Cascade,
            Phase::Layout,
            Phase::PostLayout,
            Phase::Observers,
        ]
        .iter()
        .map(|p| frame.total(*p))
        .sum();
        // The pointer moving onto the button restyles without an update; that
        // cascade is inside its own Restyle span, not inside Update.
        let restyle = frame.total(Phase::Restyle);
        assert!(
            inside <= update + restyle,
            "the stages of an update fit inside it, and a restyle's cascade inside the restyle"
        );
        assert!(frame.wall() >= update + frame.total(Phase::Raster));

        assert!(
            frame
                .causes
                .iter()
                .any(|c| c.kind == "event" && c.event == Some(("click", button))),
            "the click that started it is named: {:?}",
            frame.causes
        );
        assert!(
            frame
                .causes
                .iter()
                .any(|c| c.kind == "signal-write" && c.event == Some(("click", button))),
            "the write its handler made is listed with the click: {:?}",
            frame.causes
        );
    }

    #[test]
    fn a_frame_counts_what_each_stage_touched_and_how_the_text_cache_answered() {
        use florui_profile::Counter;

        let mut window = window(counter);
        let button = first(&window, "button");
        let (x, y) = center(&mut window, button);
        window.update();
        window.frame();

        florui_profile::start(false);
        window.update();
        window.frame();
        window.click_at(x, y);
        window.settle();
        window.frame();
        let frames = florui_profile::frames();
        florui_profile::stop();

        let unchanged = &frames[0];
        let nodes = unchanged.counter(Counter::NodesLaidOut);
        assert!(nodes > 0);
        assert_eq!(unchanged.counter(Counter::NodesStyled), nodes);
        assert!(unchanged.counter(Counter::NodesPainted) > 0);
        assert!(
            unchanged.counter(Counter::TextMemoHits) > 0,
            "text that did not change is answered from the memo"
        );
        assert_eq!(unchanged.counter(Counter::TextMemoMisses), 0);

        let changed = &frames[1];
        assert!(
            changed.counter(Counter::TextMemoMisses) > 0,
            "the count text changed, so it had to be measured again"
        );
    }

    #[test]
    fn a_hover_rule_applies_to_an_element_while_a_descendant_is_under_the_pointer() {
        const HOVER_CSS: &str = ".row { width: 200px; padding: 12px; background-color: #ffffff; }             .row:hover { background-color: #0000ff; }             .label { display: block; width: 100px; height: 20px; background-color: #ff0000; }             .label:hover { background-color: #00ff00; }";
        let mut window = HeadlessWindow::new(
            HOVER_CSS,
            || {
                Element::node(
                    "div",
                    vec![("class".into(), "row".into())],
                    vec![Element::node(
                        "span",
                        vec![("class".into(), "label".into())],
                        vec![],
                    )],
                )
            },
            HeadlessOptions {
                width: 300.0,
                height: 300.0,
                ..HeadlessOptions::default()
            },
        )
        .expect("the stylesheet parses");
        let class = |window: &HeadlessWindow, name: &str| {
            let (arena, ..) = window.runtime().geometry();
            arena
                .find(|a, id| a.classes(id).iter().any(|c| c == name))
                .unwrap_or_else(|| panic!("no .{name}"))
        };
        let (row, label) = (class(&window, "row"), class(&window, "label"));
        let pixel_at = |frame: &HeadlessFrame, node: NodeId, dx: f32, dy: f32| {
            let (x, y, ..) = frame.bounds[&node];
            let at = ((y + dy) as u32 * frame.width + (x + dx) as u32) as usize * 4;
            [frame.rgba[at], frame.rgba[at + 1], frame.rgba[at + 2]]
        };

        let idle = window.frame();
        assert_eq!(
            pixel_at(&idle, row, 2.0, 2.0),
            [255, 255, 255],
            "nothing hovered"
        );

        // Over the label, which is inside the row's padding.
        let (x, y, w, h) = idle.bounds[&label];
        window.pointer_move(x + w / 2.0, y + h / 2.0);
        window.settle();
        let over_label = window.frame();
        assert_eq!(
            pixel_at(&over_label, row, 2.0, 2.0),
            [0, 0, 255],
            "the row is :hover while its label is under the pointer"
        );
        assert_eq!(
            pixel_at(&over_label, label, 2.0, 2.0),
            [0, 255, 0],
            "the label's own :hover rule applies as well"
        );

        // Over the row's own padding the label is not hovered.
        let (rx, ry, ..) = over_label.bounds[&row];
        window.pointer_move(rx + 3.0, ry + 3.0);
        window.settle();
        let over_row = window.frame();
        assert_eq!(pixel_at(&over_row, row, 2.0, 2.0), [0, 0, 255]);
        assert_eq!(
            pixel_at(&over_row, label, 2.0, 2.0),
            [255, 0, 0],
            "a child is not :hover because its parent is"
        );
    }

    /// What a window paints after the pointer moves over three rows and back, with
    /// the hover repaint path on or off, and how many nodes were laid out while
    /// it moved.
    fn paint_after_hovering(css: &str, repaint: bool, advance: f64) -> (Vec<u8>, u64) {
        use florui_profile::Counter;

        let mut window = HeadlessWindow::new(
            css,
            || {
                let rows = (0..4)
                    .map(|i| {
                        Element::node(
                            "div",
                            vec![("class".into(), "row".into())],
                            vec![Element::text(format!("Row {i}"))],
                        )
                    })
                    .collect();
                Element::node("div", vec![("class".into(), "list".into())], rows)
            },
            HeadlessOptions {
                width: 300.0,
                height: 300.0,
                ..HeadlessOptions::default()
            },
        )
        .expect("the stylesheet parses");
        window.runtime_mut().set_hover_repaint(repaint);
        let frame = window.frame();
        let rows: Vec<NodeId> = {
            let (arena, ..) = window.runtime().geometry();
            arena.find_all(|a, id| a.classes(id).iter().any(|c| c == "row"))
        };
        let centers: Vec<(f32, f32)> = rows
            .iter()
            .map(|row| {
                let (x, y, w, h) = frame.bounds[row];
                (x + w / 2.0, y + h / 2.0)
            })
            .collect();

        florui_profile::start(false);
        for &i in &[1usize, 2, 3, 0, 2] {
            window.pointer_move(centers[i].0, centers[i].1);
            window.settle();
        }
        // The frame that ends the moves carries what they laid out; advancing the
        // clock afterwards is a full update of its own and is not counted.
        window.frame();
        let laid_out = florui_profile::recent_frames(1)
            .last()
            .map_or(0, |f| f.counter(Counter::NodesLaidOut));
        florui_profile::stop();
        if advance > 0.0 {
            window.advance_clock(advance);
        }
        (window.frame().rgba, laid_out)
    }

    #[test]
    fn a_hover_that_only_repaints_paints_what_a_full_update_would() {
        const BASE: &str = ".list { display: flex; flex-direction: column; width: 200px; }             .row { height: 30px; margin: 4px; padding: 2px; background-color: #ffffff; } ";
        // (name, stylesheet, seconds to advance, whether the repaint path applies)
        let cases: [(&str, String, f64, bool); 6] = [
            (
                "a color",
                format!("{BASE}.row:hover {{ background-color: #0000ff; }}"),
                0.0,
                true,
            ),
            (
                "a color and a shadow",
                format!(
                    "{BASE}.row:hover {{ background-color: #ff8800; box-shadow: 0px 2px 6px rgba(0, 0, 0, 0.4); }}"
                ),
                0.0,
                true,
            ),
            (
                "a color under a transition",
                format!(
                    "{BASE}.row {{ transition: background-color 1s; }} .row:hover {{ background-color: #0000ff; }}"
                ),
                0.3,
                true,
            ),
            (
                "a width, which moves layout",
                format!("{BASE}.row:hover {{ width: 120px; }}"),
                0.0,
                false,
            ),
            (
                "a font size, which re-measures text",
                format!("{BASE}.row:hover {{ font-size: 24px; }}"),
                0.0,
                false,
            ),
            (
                "a color under a container query",
                format!(
                    "{BASE}.list {{ container-type: inline-size; }} .row:hover {{ background-color: #0000ff; }} @container (min-width: 100px) {{ .row {{ padding: 6px; }} }}"
                ),
                0.0,
                false,
            ),
        ];
        for (name, css, advance, repaints) in cases {
            let (with_path, laid_out_with) = paint_after_hovering(&css, true, advance);
            let (full, laid_out_full) = paint_after_hovering(&css, false, advance);
            assert!(
                with_path == full,
                "{name}: the frame after hovering differs between the repaint path and a full update"
            );
            assert!(laid_out_full > 0, "{name}: a full update lays the tree out");
            if repaints {
                assert_eq!(
                    laid_out_with, 0,
                    "{name}: a paint-only hover must not lay anything out"
                );
            } else {
                assert!(laid_out_with > 0, "{name}: this change needs a layout");
            }
        }
    }

    #[test]
    fn a_click_is_the_origin_of_the_write_its_handler_makes() {
        use florui_reactive::trace::{self, Origin, TraceKind};

        let mut window = window(counter);
        let button = first(&window, "button");
        let (x, y) = center(&mut window, button);
        let mark = trace::next_sequence();
        window.click_at(x, y);
        window.settle();

        let traces = trace::since(mark);
        let click = Origin::Event {
            name: "click",
            target: button,
        };
        assert!(
            traces
                .iter()
                .any(|t| t.kind == TraceKind::EventDispatched && t.origin == click),
            "{traces:?}"
        );
        assert!(
            traces
                .iter()
                .any(|t| t.kind == TraceKind::SignalWrite && t.origin == click),
            "the counter's write happens inside the click handler: {traces:?}"
        );
    }

    #[test]
    fn tab_focuses_the_button_and_enter_and_space_activate_it() {
        let mut window = window(counter);
        window.press_key(TestKey::Tab);
        window.settle();
        assert_eq!(window.runtime().focused(), Some(first(&window, "button")));
        window.press_key(TestKey::Enter);
        window.settle();
        assert_eq!(text_of(&window, "count"), "1");
        window.press_key(TestKey::Space);
        window.settle();
        assert_eq!(text_of(&window, "count"), "2");
    }

    #[test]
    fn typing_in_a_text_field_reaches_its_handler_and_paste_uses_the_clipboard() {
        let mut window = window(|| {
            let text = use_signal(String::new);
            let binding = Binding::new(text.get(), {
                let text = text.clone();
                move |value: String| text.set(value)
            });
            view! {
                <div class="page">
                    <input id="name" type="text" value={binding} />
                    <p class="typed">{text.get()}</p>
                </div>
            }
        });
        let input = first(&window, "input");
        let (x, y) = center(&mut window, input);
        window.click_at(x, y);
        window.type_text("abc");
        window.settle();
        assert_eq!(text_of(&window, "typed"), "abc");

        window.clipboard().set_text("XY".to_string());
        window.set_modifiers(true, false, false);
        window.press_key(TestKey::Character('v'));
        window.set_modifiers(false, false, false);
        window.settle();
        assert_eq!(text_of(&window, "typed"), "abcXY");
    }

    #[test]
    fn a_trackpad_scroll_moves_the_scroll_box_under_the_pointer() {
        let mut window = window(|| {
            let scroll = florui_platform_scroll();
            let offset = scroll.offset().1;
            view! {
                <div class="page">
                    <div id="box" class="box"><div class="tall">{"tall"}</div></div>
                    <p class="offset">{format!("{offset:.0}")}</p>
                </div>
            }
        });
        window.settle();
        let scroll_box = first(&window, "div");
        let _ = scroll_box;
        window.pointer_move(50.0, 20.0);
        window.wheel(0.0, -30.0, false);
        window.settle();
        assert_eq!(text_of(&window, "offset"), "30");
    }

    fn florui_platform_scroll() -> crate::ScrollHandle {
        crate::use_scroll_offset("box", |_, _| {})
    }

    #[test]
    fn the_frame_carries_geometry_and_the_accessibility_tree() {
        let mut window = window(counter);
        let frame = window.frame();
        assert_eq!((frame.width, frame.height), (300, 300));
        assert_eq!(frame.rgba.len(), 300 * 300 * 4);
        let button = frame
            .accessibility
            .iter()
            .find(|node| node.role == "Button")
            .expect("the button is in the tree");
        assert_eq!(button.name.as_deref(), Some("Go"));
        assert_eq!(button.bounds.map(|b| (b.2, b.3)), Some((82.0, 32.0)));
    }

    #[test]
    fn a_device_pixel_ratio_scales_the_frame_and_bounds_but_not_layout() {
        let mut window = HeadlessWindow::new(
            CSS,
            counter,
            HeadlessOptions {
                width: 300.0,
                height: 300.0,
                scale_factor: 2.0,
                ..HeadlessOptions::default()
            },
        )
        .expect("parses");
        let frame = window.frame();
        assert_eq!((frame.width, frame.height), (600, 600));
        let button = first(&window, "button");
        assert_eq!(
            frame.bounds[&button].2, 164.0,
            "physical width of an 82px button (80 plus the 1px border each side)"
        );
        // A click given in logical pixels still lands on it.
        let (x, y, w, h) = frame.bounds[&button];
        window.click_at((x + w / 2.0) / 2.0, (y + h / 2.0) / 2.0);
        window.settle();
        assert_eq!(text_of(&window, "count"), "1");
    }

    fn window_listened_to(listening: bool) -> HeadlessWindow {
        HeadlessWindow::new(
            CSS,
            counter,
            HeadlessOptions {
                width: 300.0,
                height: 300.0,
                assistive_technology: listening,
                ..HeadlessOptions::default()
            },
        )
        .expect("the stylesheet parses")
    }

    /// Clicks the button and presses Tab, which changes both the count the
    /// tree names and the focus it reports.
    fn interact(window: &mut HeadlessWindow) {
        let button = first(window, "button");
        let (x, y) = center(window, button);
        window.click_at(x, y);
        window.settle();
        window.press_key(TestKey::Tab);
        window.settle();
    }

    #[test]
    fn a_window_nobody_listens_to_builds_no_tree() {
        let mut window = window_listened_to(false);
        assert!(!window.assistive_technology());
        for _ in 0..3 {
            window.update();
            assert!(window.frame().accessibility.is_empty());
        }
        assert_eq!(window.accessibility_tree.builds(), 0);

        window.set_assistive_technology(true);
        let tree = window.frame().accessibility;
        assert_eq!(window.accessibility_tree.builds(), 1);
        assert!(
            tree.iter().any(|node| node.role == "Button"),
            "the first tree after attaching describes the page: {tree:?}"
        );
    }

    #[test]
    fn the_first_tree_after_attaching_equals_an_always_on_windows() {
        let mut always = window_listened_to(true);
        let mut late = window_listened_to(false);
        // Both go through the same interaction; only one is being listened to.
        interact(&mut always);
        interact(&mut late);
        always.frame();
        assert!(late.frame().accessibility.is_empty());

        late.set_assistive_technology(true);
        let attached = late.frame().accessibility;
        let expected = always.frame().accessibility;
        assert!(!expected.is_empty());
        assert_eq!(attached, expected, "the tree handed over on attaching");

        // And it keeps agreeing as the page changes afterwards.
        interact(&mut always);
        interact(&mut late);
        assert_eq!(late.frame().accessibility, always.frame().accessibility);
    }

    #[test]
    fn detaching_stops_the_builds_and_attaching_again_builds_the_current_tree() {
        let mut window = window_listened_to(true);
        window.frame();
        window.frame();
        assert_eq!(window.accessibility_tree.builds(), 2);

        window.set_assistive_technology(false);
        interact(&mut window);
        assert!(window.frame().accessibility.is_empty());
        assert_eq!(
            window.accessibility_tree.builds(),
            2,
            "nothing built while detached"
        );

        window.set_assistive_technology(true);
        let after = window.frame().accessibility;
        assert_eq!(window.accessibility_tree.builds(), 3);
        assert!(
            after.iter().any(|node| node.value.as_deref() == Some("1")),
            "the tree reflects the click made while detached: {after:?}"
        );
        assert!(
            after
                .iter()
                .any(|node| node.role == "Button" && node.focused),
            "and the focus the Tab key moved while detached: {after:?}"
        );
    }
}
