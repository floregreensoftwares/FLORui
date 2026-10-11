//! Renders `Card` through the real style+layout+paint pipeline in a live
//! window: `notify` hot-reloads the CSS on save, and real mouse movement
//! drives `:hover`.
//!
//! `cargo run --example live -p florui-example-app`

use florui_style::StyleMap;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::Path;
use std::rc::Rc;

use florui::Element;
use florui_example_app::components::card::{Card, CardProps};
use florui_layout::BoxLayout;
use florui_style::{Arena, InteractionState, NodeId, Rgba};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use taffy::prelude::*;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

const CARD_CSS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/components/card.css");
const BUTTON_CSS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/components/button.css");
const CANVAS_COLOR: Rgba = Rgba::opaque(0x10, 0x10, 0x14);

enum UserEvent {
    StylesheetsChanged,
}

fn main() {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("winit event loop should build on a real desktop session");
    event_loop.set_control_flow(ControlFlow::Wait);

    let proxy = event_loop.create_proxy();
    let _watcher =
        watch_stylesheets(proxy).expect("should be able to watch the components directory");

    let mut app = App::new();
    event_loop
        .run_app(&mut app)
        .expect("event loop should not fail on a real desktop session");
}

/// Watches the directory, not the files directly, so rename/replace saves
/// are still observed.
fn watch_stylesheets(proxy: EventLoopProxy<UserEvent>) -> notify::Result<RecommendedWatcher> {
    let dir = Path::new(CARD_CSS_PATH)
        .parent()
        .expect("card.css always has a parent directory")
        .to_owned();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        if result.is_ok() {
            let _ = proxy.send_event(UserEvent::StylesheetsChanged);
        }
    })?;
    watcher.watch(&dir, RecursiveMode::NonRecursive)?;
    Ok(watcher)
}

fn load_rules() -> Vec<florui_style::Rule> {
    try_load_rules().expect("card.css and button.css should be valid at startup")
}

/// Fallible counterpart of [`load_rules`], for hot-reload: a save in
/// progress shouldn't crash the window.
fn try_load_rules() -> Result<Vec<florui_style::Rule>, Box<dyn std::error::Error>> {
    let card_css = std::fs::read_to_string(CARD_CSS_PATH)?;
    let button_css = std::fs::read_to_string(BUTTON_CSS_PATH)?;
    let css = format!("{card_css}\n{button_css}");
    Ok(florui_style::parse_stylesheet(&css)?)
}

struct App {
    // Kept alive so `arena`'s text content stays valid.
    _tree: Element,
    arena: Arena,
    rules: Vec<florui_style::Rule>,
    button: Option<NodeId>,
    interaction: InteractionState,
    // Long-lived, not reloaded per frame — see `florui_layout::compute_layout`'s
    // own doc for why. Shared between this app's own layout and paint calls
    // so both shape the identical glyphs.
    font: florui_text::Font,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    _context: Option<softbuffer::Context<Rc<Window>>>,
}

impl App {
    fn new() -> Self {
        let tree = florui_reactive::render_once(|| {
            Card(CardProps {
                title: "Florui".to_string(),
            })
        });
        let arena = Arena::build(&tree);
        let button = arena.find(|a, id| a.tag(id) == "button");
        Self {
            _tree: tree,
            arena,
            rules: load_rules(),
            button,
            interaction: InteractionState::new(),
            font: florui_text::Font::load_embedded(),
            window: None,
            surface: None,
            _context: None,
        }
    }

    fn compute(&mut self) -> (StyleMap, HashMap<NodeId, BoxLayout>) {
        let styles = florui_style::compute(
            &self.arena,
            &self.rules,
            &self.interaction,
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let layouts =
            florui_layout::compute_layout(&mut self.font, &self.arena, &styles, Size::MAX_CONTENT)
                .expect("this tree's explicit sizes never produce a layout failure");
        (styles, layouts)
    }

    fn redraw(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let size = window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };

        let (styles, layouts) = self.compute();
        let canvas = florui_paint::paint_to_buffer(
            &mut self.font,
            size.width,
            size.height,
            CANVAS_COLOR,
            &self.arena,
            &styles,
            &layouts,
            1.0,
        );

        let Some(surface) = &mut self.surface else {
            return;
        };
        if surface.resize(width, height).is_err() {
            return;
        }
        let Ok(mut buffer) = surface.buffer_mut() else {
            return;
        };
        // Canvas is always opaque, so premultiplied-by-255 is a no-op.
        let pixels: Vec<u32> = canvas
            .pixels()
            .iter()
            .map(|p| u32::from_be_bytes([0, p.red(), p.green(), p.blue()]))
            .collect();
        buffer.copy_from_slice(&pixels);
        let _ = buffer.present();
    }

    /// Updates `:hover` from the cursor position; redraws only if it changed.
    fn handle_cursor_moved(&mut self, x: f64, y: f64) {
        let Some(button) = self.button else { return };
        let (_, layouts) = self.compute();
        let Some(&layout) = layouts.get(&button) else {
            return;
        };
        let (bx, by) = florui_layout::absolute_position(&self.arena, &layouts, button);
        let inside = x >= bx as f64
            && x < (bx + layout.width) as f64
            && y >= by as f64
            && y < (by + layout.height) as f64;

        let was_hovered = self.interaction.is_hovered(button);
        if inside != was_hovered {
            self.interaction = if inside {
                InteractionState::new().with_hovered(button)
            } else {
                InteractionState::new()
            };
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title("Florui live preview");
        let window = Rc::new(
            event_loop
                .create_window(attrs)
                .expect("window should be creatable on a real desktop session"),
        );
        let context = softbuffer::Context::new(window.clone())
            .expect("softbuffer context should be creatable");
        let surface = softbuffer::Surface::new(&context, window.clone())
            .expect("softbuffer surface should be creatable");

        self.window = Some(window);
        self._context = Some(context);
        self.surface = Some(surface);
        self.redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.window.as_ref().map(|w| w.id()) != Some(window_id) {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_) => self.redraw(),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position.x, position.y);
            }
            _ => {}
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::StylesheetsChanged => match try_load_rules() {
                Ok(rules) => {
                    self.rules = rules;
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
                Err(error) => {
                    eprintln!("stylesheet reload failed, keeping last good version: {error}");
                }
            },
        }
    }
}
