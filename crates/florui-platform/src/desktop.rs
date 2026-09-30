//! [`run`]: pairs [`UiRuntime`] with a real `winit` window, a `softbuffer`
//! surface, and an event loop, so a caller doesn't have to write its own
//! desktop event loop just to see a component tree running. [`run_windows`]
//! is the same thing generalized to any number of windows at once, each
//! independently titled/styled/iconed — [`run`]/[`run_with_options`]/
//! [`run_with_css_reload`]/[`run_with_css_reload_and_options`] are thin
//! wrappers over it for the common one-window case.
//!
//! HiDPI-aware: layout runs against the window's *logical* size (via
//! [`crate::dpi`]), matching `florui_style`'s CSS-style `width`/`height`;
//! the committed boxes are then scaled back up by the window's own scale
//! factor before painting, so the canvas renders at full physical device
//! resolution rather than blurrily upscaling a logical-resolution one.
//! Cursor positions (physical, from `winit`) are converted the other way
//! before hit-testing against that same logical layout.

use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use florui::Element;
use florui_layout::BoxLayout;
use florui_reactive::provide_context;
use florui_style::{ComputedStyle, FocusPath, NodeId, Rgba, StyleError};
use florui_text::editing::TextEditOp;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use taffy::prelude::*;
use winit::application::ApplicationHandler;
#[cfg(test)]
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::window::{ResizeDirection, Window, WindowId};

use crate::UiRuntime;
use crate::accessibility;
use crate::activation::{ActivationEvent, ActivationEvents, ActivationQueue, SingleInstance};
use crate::appearance;
use crate::appearance::DecorationMode;
use crate::components::popover;
use crate::dpi::{self, ViewportScale};
use crate::drag_drop::{self, DragDropRegistration};
use crate::file_dialog::{OpenFileDialogOutcome, SaveFileDialogOutcome};
use crate::gpu::{self, GpuPresenter};
use crate::list_keys::ListKey;
use crate::menu_keys::{self, MenuKey, MenuMove};
use crate::single_instance::{self, HandoffOutcome, InstanceRole};
use crate::window_controls::{InputMode, ScreenRect, WindowControls, resize_direction_at};
use crate::window_state::{self, WindowPersistence};

#[derive(Debug)]
pub enum RunError {
    EventLoop(winit::error::EventLoopError),
    Stylesheet(StyleError),
    WindowCreation(winit::error::OsError),
    SurfaceCreation(softbuffer::SoftBufferError),
    CssFile(std::io::Error),
    CssWatch(notify::Error),
    /// The named mutex or named pipe itself could not be set up (e.g. the
    /// security descriptor failed to build) — distinct from a normal
    /// hand-off failure, which is reported through [`RunOutcome`] instead
    /// since it isn't fatal to this process.
    SingleInstance(std::io::Error),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::EventLoop(err) => write!(f, "event loop failed: {err}"),
            RunError::Stylesheet(err) => write!(f, "stylesheet failed to parse: {err}"),
            RunError::WindowCreation(err) => write!(f, "window could not be created: {err}"),
            RunError::SurfaceCreation(err) => {
                write!(f, "render surface could not be created: {err}")
            }
            RunError::CssFile(err) => write!(f, "could not read stylesheet file: {err}"),
            RunError::CssWatch(err) => write!(f, "could not watch stylesheet file: {err}"),
            RunError::SingleInstance(err) => {
                write!(f, "single-instance setup failed: {err}")
            }
        }
    }
}

impl std::error::Error for RunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RunError::EventLoop(err) => Some(err),
            RunError::Stylesheet(err) => Some(err),
            RunError::WindowCreation(err) => Some(err),
            RunError::SurfaceCreation(err) => Some(err),
            RunError::CssFile(err) => Some(err),
            RunError::CssWatch(err) => Some(err),
            RunError::SingleInstance(err) => Some(err),
        }
    }
}

/// Every variant now carries the `WindowId` it's about -- with more than
/// one window live, each of these needs to say which one, whereas a
/// single-window host had nothing to disambiguate.
enum UserEvent {
    /// Either a [`florui_reactive::Signal`] changed somewhere under this
    /// window's root, or a [`florui_reactive::use_resource`] fetch became
    /// newly pollable — see [`UiRuntime::on_needs_update`]. Both call for
    /// the same reaction: re-render and repaint.
    Dirty(WindowId),
    /// The watched CSS file (see [`run_with_css_reload`]) changed on disk.
    CssChanged(WindowId),
    /// A [`crate::WindowControls::close`] call from inside the component
    /// tree, routed back through the event loop so it takes the exact
    /// same close path a real `WindowEvent::CloseRequested` (the OS's own
    /// close button, still live even under [`DecorationMode::Custom`] via,
    /// e.g., Alt+F4) — one real shutdown path, not two that could drift
    /// apart.
    RequestClose(WindowId),
    /// An [`ActivationEvent`] arrived from another launch of this same
    /// application — see [`run_single_instance`]. Not per-window, unlike
    /// every other variant here: activation isn't scoped to one window.
    Activation(ActivationEvent),
    /// A [`crate::WindowControls::open_file_dialog`]/`save_file_dialog`
    /// call's background thread finished — routed back through the event
    /// loop so the app's own `on_result` callback runs on the UI thread,
    /// where touching `Signal`s is safe.
    OpenFileDialogResult(WindowId, OpenFileDialogOutcome),
    SaveFileDialogResult(WindowId, SaveFileDialogOutcome),
    /// A real AccessKit event -- the initial tree request, an inbound
    /// `ActionRequest` from the platform AT, or deactivation. Already
    /// carries its own `window_id` (see `accesskit_winit::Event`), unlike
    /// every other variant here.
    Accessibility(accesskit_winit::Event),
}

impl From<accesskit_winit::Event> for UserEvent {
    fn from(event: accesskit_winit::Event) -> Self {
        UserEvent::Accessibility(event)
    }
}

/// What [`run_with_options`]/[`run_with_css_reload_and_options`] ask for
/// about the real window's own chrome — its own struct (not bare
/// parameters) so a later addition doesn't need a new `run_with_*_and_*`
/// function of its own. [`run`]/[`run_with_css_reload`] are thin wrappers
/// over these two with [`WindowOptions::default`] (system decorations, no
/// explicit size, opaque), so every existing caller keeps working
/// unchanged.
///
/// `size`/`min_size` are logical units (see this module's own HiDPI note),
/// `None` meaning "let the platform choose" exactly as today's behavior
/// with no explicit size request. `transparent` is carried here for
/// fidelity with `florui-config`'s own `window.transparent` field, but has
/// no effect on real window creation yet — see [`gpu::transparent_capable_attributes`]'s
/// own doc: a transparent-capable surface is already requested
/// unconditionally for every window, regardless of this option. `icon` is
/// the creation-time half of per-window icons — see
/// [`crate::WindowControls::set_icon`] for updating an already-open
/// window's icon instead. `respect_reduced_motion` (default `true`) is
/// the opt-out for this crate's own automatic transition/`@keyframes`
/// suppression when the real OS prefers reduced motion — `false` restores
/// plain, unsuppressed CSS animation regardless of that OS preference; the
/// `@media (prefers-reduced-motion: ...)` query itself always reflects OS
/// truth either way, see [`florui_style::AnimationTimeline`]'s own doc.
/// `theme` (default [`ThemePreference::System`]) is the same "follow the
/// OS, or force it" shape as `decorations` — see [`crate::theme`]'s own
/// module doc for why, unlike reduced motion, an explicit override here
/// *does* replace what `@media (prefers-color-scheme: ...)` itself
/// reports, rather than leaving a separately-preserved OS truth: `winit`
/// itself doesn't keep one once an override is set.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowOptions {
    pub decorations: DecorationMode,
    pub size: Option<(f64, f64)>,
    pub min_size: Option<(f64, f64)>,
    pub transparent: bool,
    pub icon: Option<florui_icon::RawIcon>,
    pub respect_reduced_motion: bool,
    pub theme: crate::theme::ThemePreference,
    /// `None` (the default) persists nothing -- see
    /// [`crate::WindowPersistence`]'s own doc for the app-side resolution
    /// pattern.
    pub persistence: Option<WindowPersistence>,
}

/// Not `#[derive(Default)]`: every field but `respect_reduced_motion`
/// matches what the derive would have given (unset/off/system-decorated),
/// but that one field's required default (`true`) is not the same as
/// `bool`'s own derived default (`false`) — a derive here would silently
/// invert it for every existing caller of [`WindowOptions::default`]
/// ([`run`], [`run_with_css_reload`]). `theme`'s own derived default
/// ([`ThemePreference::System`]) would have been fine, but it stays here
/// too, alongside the field it now can't be separated from without
/// re-deriving `Default` and reintroducing exactly the risk above.
impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            decorations: DecorationMode::default(),
            size: None,
            min_size: None,
            transparent: false,
            icon: None,
            respect_reduced_motion: true,
            theme: crate::theme::ThemePreference::default(),
            persistence: None,
        }
    }
}

/// Opens a window titled `title` and keeps it live over `root` — called
/// fresh on every render, the way a `#[component]` function normally is.
/// `css` is parsed once; it does not get watched for changes — for a dev
/// loop that reloads edited CSS without losing component state, use
/// [`run_with_css_reload`] instead. System-decorated — for an
/// application-drawn title bar instead, use [`run_with_options`].
///
/// Blocks the calling thread until the window closes.
pub fn run(
    title: &str,
    css: &str,
    canvas_color: Rgba,
    root: impl Fn() -> Element + 'static,
) -> Result<(), RunError> {
    run_with_options(title, css, canvas_color, WindowOptions::default(), root)
}

/// Same as [`run`], but with explicit control over the real window's own
/// chrome — see [`WindowOptions`]'s own doc. Under
/// [`DecorationMode::Custom`], `root`'s own component tree reaches
/// [`crate::use_window_controls`] to actually drive the window it no
/// longer has OS-drawn chrome to drive it for free.
///
/// Blocks the calling thread until the window closes.
pub fn run_with_options(
    title: &str,
    css: &str,
    canvas_color: Rgba,
    options: WindowOptions,
    root: impl Fn() -> Element + 'static,
) -> Result<(), RunError> {
    let spec = WindowSpec::new(title, css, canvas_color, options, root)?;
    run_windows(vec![spec])
}

/// Same as [`run`], but reads `css_path` from disk and watches it for
/// changes instead of taking a fixed string: saving an edit re-parses the
/// stylesheet and repaints through [`UiRuntime::set_rules`], which never
/// touches component state — every `Signal` keeps its value across the
/// reload, unlike a Rust source change, which needs an actual process
/// restart (and does lose it; see `florui dev`'s own reporting of that).
///
/// A reload that fails to parse is reported to stderr and the previous,
/// still-valid stylesheet keeps rendering — only the *initial* read at
/// startup must succeed. System-decorated — for an application-drawn
/// title bar instead, use [`run_with_css_reload_and_options`]. Blocks the
/// calling thread until the window closes.
pub fn run_with_css_reload(
    title: &str,
    css_path: impl AsRef<Path>,
    canvas_color: Rgba,
    root: impl Fn() -> Element + 'static,
) -> Result<(), RunError> {
    run_with_css_reload_and_options(
        title,
        css_path,
        canvas_color,
        WindowOptions::default(),
        root,
    )
}

/// Same as [`run_with_css_reload`], but with explicit control over the
/// real window's own chrome — see [`WindowOptions`]'s own doc and
/// [`run_with_options`]'s own note on [`DecorationMode::Custom`]. Blocks
/// the calling thread until the window closes.
pub fn run_with_css_reload_and_options(
    title: &str,
    css_path: impl AsRef<Path>,
    canvas_color: Rgba,
    options: WindowOptions,
    root: impl Fn() -> Element + 'static,
) -> Result<(), RunError> {
    let spec = WindowSpec::with_css_reload(title, css_path, canvas_color, options, root)?;
    run_windows(vec![spec])
}

/// Opens every window in `initial` on one shared event loop and blocks the
/// calling thread until the last one closes. If any window in `initial`
/// fails to create, the whole batch is treated as fatal (matching
/// [`run_with_options`]'s own single-window failure behavior) — this is
/// only reachable at startup, not once windows are already running.
pub fn run_windows(initial: Vec<WindowSpec>) -> Result<(), RunError> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(RunError::EventLoop)?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut host = DesktopHost {
        windows: HashMap::new(),
        pending: initial,
        proxy: event_loop.create_proxy(),
        fatal_error: None,
        activation_queue: None,
        primary_window_id: None,
        clipboard: crate::clipboard::Clipboard::new(),
        visited_links: crate::visited_links::VisitedLinks::new(),
    };
    event_loop.run_app(&mut host).map_err(RunError::EventLoop)?;
    match host.fatal_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// What [`run_single_instance`] actually did — distinct from [`RunError`],
/// since a failed hand-off is not fatal to this process; it's a normal
/// outcome the caller decides how to react to (e.g. become primary itself
/// instead).
#[derive(Debug)]
pub enum RunOutcome {
    /// This process was (or became) primary and its event loop ran to
    /// completion — the same as a plain [`run_windows`] call returning
    /// `Ok`.
    Ran,
    /// `launch_event` was delivered to, and acknowledged by, the
    /// already-running primary instance. No window was ever created —
    /// the caller should exit.
    HandedOff,
    /// Another instance owns the single-instance mutex, but the hand-off
    /// itself did not complete (the owner was unreachable, died mid-transfer,
    /// or never acknowledged within the configured timeout). The
    /// activation data is handed back rather than silently discarded —
    /// this must never be reported as [`RunOutcome::HandedOff`], and the
    /// caller decides what to do next (e.g. call [`run_windows`] itself,
    /// deliberately becoming a fresh primary).
    HandoffFailed(ActivationEvent),
}

/// Same as [`run_windows`], but first enforces `config`'s opt-in
/// single-instance behavior: exactly one process at a time owns
/// `config.app_identifier`'s OS-backed mutex (see
/// `crate::os::windows::single_instance`'s own doc — Windows-only for
/// now; every other platform always becomes primary, see
/// `crate::single_instance`'s stub). `launch_event` is this process's own
/// activation payload — sent to the existing primary if this process
/// turns out to be secondary, or the first event a fresh primary's own
/// [`crate::use_activation_events`] sees, whichever applies.
///
/// A secondary instance never creates a window or touches an `EventLoop`
/// at all — it performs the hand-off synchronously and returns.
pub fn run_single_instance(
    config: SingleInstance,
    launch_event: ActivationEvent,
    initial: Vec<WindowSpec>,
) -> Result<RunOutcome, RunError> {
    match single_instance::acquire(&config.app_identifier).map_err(RunError::SingleInstance)? {
        InstanceRole::Secondary => {
            match single_instance::handoff(
                &config.app_identifier,
                &launch_event,
                config.handoff_timeout,
            ) {
                HandoffOutcome::Delivered => Ok(RunOutcome::HandedOff),
                HandoffOutcome::Failed => Ok(RunOutcome::HandoffFailed(launch_event)),
            }
        }
        InstanceRole::Primary(mutex_ownership) => {
            let event_loop = EventLoop::<UserEvent>::with_user_event()
                .build()
                .map_err(RunError::EventLoop)?;
            event_loop.set_control_flow(ControlFlow::Wait);

            let activation_queue = Rc::new(RefCell::new(ActivationQueue::default()));
            let proxy = event_loop.create_proxy();
            single_instance::spawn_activation_listener(&config.app_identifier, move |event| {
                let _ = proxy.send_event(UserEvent::Activation(event));
            })
            .map_err(RunError::SingleInstance)?;
            // This process's own launch event goes straight into the same
            // queue the primary window's first render will read — no pipe
            // round trip needed for the process that already owns the
            // mutex.
            activation_queue.borrow_mut().push(launch_event);

            // Keeps the mutex held for as long as this process is primary
            // -- dropped (releasing it) only once `run_app` below returns,
            // i.e. when every window has closed and this process is about
            // to exit.
            let _mutex_ownership = mutex_ownership;

            let mut host = DesktopHost {
                windows: HashMap::new(),
                pending: initial,
                proxy: event_loop.create_proxy(),
                fatal_error: None,
                activation_queue: Some(activation_queue),
                primary_window_id: None,
                clipboard: crate::clipboard::Clipboard::new(),
                visited_links: crate::visited_links::VisitedLinks::new(),
            };
            event_loop.run_app(&mut host).map_err(RunError::EventLoop)?;
            match host.fatal_error {
                Some(error) => Err(error),
                None => Ok(RunOutcome::Ran),
            }
        }
    }
}

/// Everything needed to open one window, not yet created — consumed
/// exactly once by [`DesktopHost::resumed`], which drains a `Vec` of
/// these. Build one with [`WindowSpec::new`] or
/// [`WindowSpec::with_css_reload`].
pub struct WindowSpec {
    title: String,
    canvas_color: Rgba,
    rules: Vec<florui_style::Rule>,
    options: WindowOptions,
    root: Box<dyn Fn() -> Element>,
    /// `Some` => this window watches and live-reloads this file's CSS
    /// (see [`run_with_css_reload`]'s own doc); `None` => static CSS,
    /// parsed once.
    css_path: Option<PathBuf>,
}

impl WindowSpec {
    /// `css` is parsed immediately, so a syntax error is reported before
    /// any window opens rather than after.
    pub fn new(
        title: impl Into<String>,
        css: &str,
        canvas_color: Rgba,
        options: WindowOptions,
        root: impl Fn() -> Element + 'static,
    ) -> Result<Self, RunError> {
        let rules = florui_style::parse_stylesheet(css).map_err(RunError::Stylesheet)?;
        Ok(Self {
            title: title.into(),
            canvas_color,
            rules,
            options,
            root: Box::new(root),
            css_path: None,
        })
    }

    /// Same as [`Self::new`], but reads `css_path` from disk and watches
    /// it for changes once this window is live — see
    /// [`run_with_css_reload`]'s own doc for the reload contract.
    pub fn with_css_reload(
        title: impl Into<String>,
        css_path: impl AsRef<Path>,
        canvas_color: Rgba,
        options: WindowOptions,
        root: impl Fn() -> Element + 'static,
    ) -> Result<Self, RunError> {
        let css_path = css_path.as_ref().to_owned();
        let css = std::fs::read_to_string(&css_path).map_err(RunError::CssFile)?;
        let rules = florui_style::parse_stylesheet(&css).map_err(RunError::Stylesheet)?;
        Ok(Self {
            title: title.into(),
            canvas_color,
            rules,
            options,
            root: Box::new(root),
            css_path: Some(css_path),
        })
    }

    /// Like [`Self::new`], but for already-parsed rules instead of one raw
    /// CSS string — infallible, since parsing already happened. The route
    /// for multiple `StylesheetSource`s (e.g. one or more
    /// `stylesheet_scoped!` declarations plus any plain `stylesheet!`
    /// ones): compile each through `florui_style::compile_sources` first,
    /// which applies each scoped source's own class-selector rewrite
    /// before parsing, something a single concatenated CSS string
    /// couldn't represent (a scope boundary needs to be a property of one
    /// particular source, not the whole merged text). No CSS hot reload —
    /// same tradeoff as [`Self::new`].
    pub fn with_rules(
        title: impl Into<String>,
        rules: Vec<florui_style::Rule>,
        canvas_color: Rgba,
        options: WindowOptions,
        root: impl Fn() -> Element + 'static,
    ) -> Self {
        Self {
            title: title.into(),
            canvas_color,
            rules,
            options,
            root: Box::new(root),
            css_path: None,
        }
    }
}

/// Watches `css_path`'s parent directory (not the file itself, so editors
/// that save via rename/replace are still observed) and wakes the event
/// loop only on a change to `css_path` exactly, tagged with `window_id` so
/// the right [`WindowState`] reloads — mirrors
/// `florui-devtools::preview::watch_fixture`.
fn watch_css_file(
    css_path: &Path,
    proxy: EventLoopProxy<UserEvent>,
    window_id: WindowId,
) -> notify::Result<RecommendedWatcher> {
    let target = css_path
        .canonicalize()
        .unwrap_or_else(|_| css_path.to_owned());
    let parent = css_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let Ok(event) = result else { return };
        let touches_target = event
            .paths
            .iter()
            .any(|p| p.canonicalize().map(|c| c == target).unwrap_or(false));
        if touches_target {
            // The event loop may already be gone; nothing to do if so.
            let _ = proxy.send_event(UserEvent::CssChanged(window_id));
        }
    })?;
    watcher.watch(parent, RecursiveMode::NonRecursive)?;
    Ok(watcher)
}

fn layout_viewport(scale: ViewportScale) -> Size<AvailableSpace> {
    Size {
        width: AvailableSpace::Definite(scale.logical.width),
        height: AvailableSpace::Definite(scale.logical.height),
    }
}

/// The content-box origin (post border/padding) of `node`, in logical
/// pixels — a free function (not a `WindowState` method) so `redraw`'s
/// own already-borrowed `arena`/`styles`/`layouts` can call it directly,
/// without a second, conflicting borrow of `self.runtime`. See
/// [`WindowState::text_input_content_origin`]'s own doc for the full
/// rationale; that method just delegates here.
fn text_input_content_origin(
    arena: &florui_style::Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    layouts: &HashMap<NodeId, BoxLayout>,
    node: NodeId,
) -> (f32, f32) {
    let (x, y) = florui_layout::absolute_position(arena, layouts, node);
    let style = styles.get(&node);
    let border = style.map_or(0.0, |s| s.border.left.width);
    let border_top = style.map_or(0.0, |s| s.border.top.width);
    let padding_left = style.map_or(0.0, |s| s.padding.left);
    let padding_top = style.map_or(0.0, |s| s.padding.top);
    (x + border + padding_left, y + border_top + padding_top)
}

/// Scales every committed box from the logical pixels layout ran against
/// up to physical pixels, so painting can rasterize at full device
/// resolution instead of the canvas's own (unscaled) unit.
fn scale_layouts(layouts: &HashMap<NodeId, BoxLayout>, factor: f32) -> HashMap<NodeId, BoxLayout> {
    layouts
        .iter()
        .map(|(&id, layout)| {
            (
                id,
                BoxLayout {
                    x: layout.x * factor,
                    y: layout.y * factor,
                    width: layout.width * factor,
                    height: layout.height * factor,
                },
            )
        })
        .collect()
}

/// What [`build_text_input_paint`] reads about the frame being painted.
#[derive(Clone, Copy)]
struct TextInputPaintContext<'a> {
    arena: &'a florui_style::Arena,
    styles: &'a HashMap<NodeId, ComputedStyle>,
    layouts: &'a HashMap<NodeId, BoxLayout>,
    registry: &'a crate::text_input::TextInputRegistry,
    focused: Option<NodeId>,
    hovered: Option<NodeId>,
    pointer: (f32, f32),
    /// The `id` of the textarea whose scrollbar thumb is being dragged.
    scroll_dragging: Option<&'a str>,
}

/// Builds one [`florui_paint::TextInputPaint`] entry for every editable
/// `<input>` [`crate::text_input::TextInputRegistry`] currently tracks —
/// the real glyphs and geometry [`crate::desktop`]'s own `redraw` hands
/// to [`florui_paint::paint_to_buffer_with_desktop_extras`]. Only `focused`
/// gets a real caret/selection highlight (`show_caret`/`selection_rects`);
/// every other tracked input still needs its own text painted (it's a
/// real, visible control either way), just without either — matching
/// real browsers, which never show a selection swatch on an unfocused
/// text field. `type="password"` gets a masked substitute run instead of
/// its real glyphs — see [`masked_glyphs`].
fn build_text_input_paint(
    font: &mut florui_text::Font,
    context: TextInputPaintContext<'_>,
    spinner_for: &dyn Fn(NodeId) -> Option<florui_paint::SpinnerHover>,
) -> HashMap<NodeId, florui_paint::TextInputPaint> {
    let TextInputPaintContext {
        arena,
        styles,
        layouts,
        registry,
        focused,
        hovered,
        pointer,
        scroll_dragging,
    } = context;
    let mut result = HashMap::new();
    let editable_inputs = arena.find_all(crate::focus::is_text_control);
    for node in editable_inputs {
        let Some(id) = arena.id_attr(node) else {
            continue;
        };
        let Some((runs, caret_rect, selection_rects, compose_rect)) = registry.paint_data(id, font)
        else {
            continue;
        };
        let is_focused = focused == Some(node);
        let style = styles.get(&node);
        let (runs, caret_rect, selection_rects, compose_rect) =
            if arena.input_type(node) == Some("password") {
                let font_size = style.map_or(16.0, |s| s.font_size);
                let font_weight = style.map_or(400.0, |s| s.font_weight);
                let family = style.map_or(florui_text::FontFamily::SansSerif, |s| {
                    florui_layout::to_text_font_family(s.font_family)
                });
                let real_glyphs: Vec<florui_text::ShapedGlyph> = runs
                    .iter()
                    .flat_map(|run| run.glyphs.iter().copied())
                    .collect();
                let (mask_runs, mask_positions) =
                    masked_glyphs(font, real_glyphs.len(), family, font_size, font_weight);
                let mask_x = |real_x: f32| -> f32 {
                    let index = char_index_at(&real_glyphs, real_x);
                    mask_positions
                        .get(index)
                        .copied()
                        .unwrap_or_else(|| mask_positions.last().copied().unwrap_or(0.0))
                };
                // The rects are `(x0, y0, x1, y1)` corners, so both x's are
                // mapped from real to mask positions; a caret keeps its width.
                let caret_rect = caret_rect.map(|(x0, y0, x1, y1)| {
                    let x = mask_x(x0);
                    (x, y0, x + (x1 - x0), y1)
                });
                let selection_rects = selection_rects
                    .into_iter()
                    .map(|(x0, y0, x1, y1)| (mask_x(x0), y0, mask_x(x1), y1))
                    .collect();
                let compose_rect =
                    compose_rect.map(|(x0, y0, x1, y1)| (mask_x(x0), y0, mask_x(x1), y1));
                (mask_runs, caret_rect, selection_rects, compose_rect)
            } else {
                (runs, caret_rect, selection_rects, compose_rect)
            };
        let placeholder = (runs.is_empty() && crate::form::shows_placeholder(arena, node, true))
            .then(|| arena.attr(node, "placeholder"))
            .flatten()
            .map(|text| {
                let font_size = style.map_or(16.0, |s| s.font_size);
                let font_weight = style.map_or(400.0, |s| s.font_weight);
                let family = style.map_or(florui_text::FontFamily::SansSerif, |s| {
                    florui_layout::to_text_font_family(s.font_family)
                });
                if arena.tag(node) == "textarea" {
                    let width = registry.viewport(id).0.max(1.0);
                    font.shape_wrapped(family, text, font_size, font_weight, width)
                        .runs
                } else {
                    font.shape(family, text, font_size, font_weight).runs
                }
            });
        let (scrollbar, resizer) = if arena.tag(node) == "textarea" {
            let resizer = style.is_none_or(|s| s.resize != florui_style::Resize::None);
            let border = style.map_or((0.0, 0.0, 0.0, 0.0), |s| {
                (
                    s.border.left.width,
                    s.border.top.width,
                    s.border.right.width,
                    s.border.bottom.width,
                )
            });
            let layout = layouts.get(&node);
            let client_height = layout.map_or(0.0, |l| l.height - border.1 - border.3);
            let scrollbar = registry
                .scroll_metrics(id, font)
                .map(|(scroll, max_scroll)| florui_paint::ScrollbarPaint {
                    scroll,
                    max_scroll,
                    viewport: client_height,
                    active: florui_paint::ScrollbarPart::None,
                });
            let scrollbar = scrollbar.map(|mut bar| {
                if scroll_dragging == Some(id) {
                    bar.active = florui_paint::ScrollbarPart::Thumb;
                } else if hovered == Some(node)
                    && let Some(layout) = layout
                {
                    let (x, y) = florui_layout::absolute_position(arena, layouts, node);
                    let geometry = florui_paint::scrollbar_geometry(
                        (layout.width, layout.height),
                        border,
                        resizer,
                        bar,
                    );
                    bar.active =
                        florui_paint::scrollbar_part_at(&geometry, (pointer.0 - x, pointer.1 - y));
                }
                bar
            });
            (scrollbar, resizer)
        } else {
            (None, false)
        };
        result.insert(
            node,
            florui_paint::TextInputPaint {
                scrollbar,
                resizer,
                placeholder,
                runs,
                caret_rect: is_focused.then_some(caret_rect).flatten(),
                selection_rects: if is_focused {
                    selection_rects
                } else {
                    Vec::new()
                },
                compose_rect: is_focused.then_some(compose_rect).flatten(),
                show_caret: is_focused,
                spinner: spinner_for(node),
                scroll: registry.scroll_offset(id),
            },
        );
    }
    result
}

/// Builds one [`florui_paint::ImagePaint`] entry for every `<img>`
/// [`crate::image::ImageRegistry`] currently has a real decode ready for
/// — a still-loading or failed `<img>` simply has no entry (and paints
/// nothing but its own background/border, same as an empty `<div>`).
///
/// Also requests SVG rasterization at each `<img>`'s own real, current
/// content-box size (already-physical/HiDPI-scaled `layouts`, so the
/// raster target matches real on-screen pixels, not logical ones) — a
/// no-op for a PNG entry, or an SVG entry already rasterized at this
/// exact size; see [`crate::image::ImageRegistry::request_raster`]'s own
/// doc for why this has to happen here rather than as soon as an SVG's
/// intrinsic size is known. Runs every redraw, so a later resize/DPI
/// change re-requests at the new size on its own.
#[allow(clippy::too_many_arguments)]
fn build_image_paint(
    arena: &florui_style::Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    layouts: &HashMap<NodeId, florui_layout::BoxLayout>,
    registry: &crate::image::ImageRegistry,
    cache: &Arc<florui_assets::AssetCache>,
    executor: &dyn florui_reactive::Executor,
) -> HashMap<NodeId, florui_paint::ImagePaint> {
    let mut result = HashMap::new();
    for node in arena.find_all(|arena, id| arena.tag(id) == "img") {
        let key = FocusPath::of(arena, node);

        if let Some(src) = arena.src_attr(node)
            && let Some(&layout) = layouts.get(&node)
        {
            let (width, height) = content_box_raster_target(styles.get(&node), layout);
            registry.request_raster(
                &key,
                src,
                florui_assets::RasterFit::Contain { width, height },
                cache,
                executor,
            );
        }

        if let Some(image) = registry.decoded(&key) {
            result.insert(node, image);
        }
    }
    result
}

/// Same as [`build_image_paint`], for `<icon>` instead of `<img>` — see
/// `crate::icon`'s own doc for why they're separate tags/registries. The
/// one real difference in what has to happen here: an icon's own
/// rasterization also needs its resolved `color` (real CSS's
/// `currentColor` source), read the normal way any other style value
/// already is — nothing icon-specific about *that* part.
#[allow(clippy::too_many_arguments)]
fn build_icon_paint(
    arena: &florui_style::Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    layouts: &HashMap<NodeId, florui_layout::BoxLayout>,
    registry: &crate::icon::IconRegistry,
    cache: &Arc<florui_assets::AssetCache>,
    executor: &dyn florui_reactive::Executor,
) -> HashMap<NodeId, florui_paint::ImagePaint> {
    let mut result = HashMap::new();
    for node in arena.find_all(|arena, id| arena.tag(id) == "icon") {
        let key = FocusPath::of(arena, node);
        let style = styles.get(&node);

        if let Some(&layout) = layouts.get(&node) {
            let (width, height) = content_box_raster_target(style, layout);
            let color = style.map_or(florui_style::Rgba::opaque(0, 0, 0), |s| s.color);
            registry.request_raster(
                &key,
                florui_assets::RasterFit::Contain { width, height },
                [color.r, color.g, color.b],
                cache,
                executor,
            );
        }

        if let Some(image) = registry.decoded(&key) {
            result.insert(node, image);
        }
    }
    result
}

/// Builds one [`florui_paint::ImagePaint`] entry for every `<select>`'s
/// own built-in chevron and every checked checkbox's own built-in check
/// mark currently on screen — the same [`crate::icon::IconRegistry`]
/// [`build_icon_paint`] already uses, just driven by
/// [`crate::icon::IconRegistry::sync_controls`]'s own registrations
/// instead of real `<icon>` tags (see that method's own doc). Skips a
/// control whose own resolved `appearance` is
/// [`florui_style::Appearance::None`] entirely -- nothing requested,
/// nothing painted, matching `sync_controls`'s own registration skip.
///
/// Rasterized at a fixed [`florui_paint::CONTROL_ICON_SIZE`] (logical px,
/// square) scaled to this canvas's own physical pixels -- a control
/// decoration has no CSS box of its own to size against the way an
/// `<img>`/`<icon>`'s own content-box does, and no `object-fit` to read
/// either (this is UA chrome, not a replaced element's own content); a
/// fixed size is the same pragmatic simplification `RasterFit::Contain`
/// already is elsewhere, not full size-aware rasterization. *Positioned*
/// within the control's own content box by `florui_paint`'s own
/// control-icon paint branch, which reads that same constant.
fn build_control_icon_paint(
    arena: &florui_style::Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    registry: &crate::icon::IconRegistry,
    cache: &Arc<florui_assets::AssetCache>,
    executor: &dyn florui_reactive::Executor,
    scale_factor: f32,
) -> HashMap<NodeId, florui_paint::ImagePaint> {
    let mut result = HashMap::new();
    let is_control_icon_node = |arena: &florui_style::Arena, id: NodeId| {
        arena.tag(id) == "select"
            || (arena.tag(id) == "input"
                && arena.input_type(id) == Some("checkbox")
                && arena.is_checked(id))
    };
    let size = (florui_paint::CONTROL_ICON_SIZE * scale_factor)
        .round()
        .max(1.0) as u32;
    for node in arena.find_all(is_control_icon_node) {
        let key = FocusPath::of(arena, node);
        let style = styles.get(&node);
        if style.is_some_and(|s| s.appearance == florui_style::Appearance::None) {
            continue;
        }

        let color = style.map_or(florui_style::Rgba::opaque(0, 0, 0), |s| s.color);
        registry.request_raster(
            &key,
            florui_assets::RasterFit::Contain {
                width: size,
                height: size,
            },
            [color.r, color.g, color.b],
            cache,
            executor,
        );

        if let Some(image) = registry.decoded(&key) {
            result.insert(node, image);
        }
    }
    result
}

/// A node's own content-box size, in whatever units `layout` (physical
/// or logical) is already in — the raster target both
/// [`build_image_paint`] and [`build_icon_paint`] request, rounded up to
/// at least one real pixel so a momentarily zero-size box never requests
/// an empty raster.
fn content_box_raster_target(
    style: Option<&ComputedStyle>,
    layout: florui_layout::BoxLayout,
) -> (u32, u32) {
    let (content_width, content_height) = match style {
        Some(s) => (
            (layout.width
                - s.border.left.width
                - s.border.right.width
                - s.padding.left
                - s.padding.right)
                .max(0.0),
            (layout.height
                - s.border.top.width
                - s.border.bottom.width
                - s.padding.top
                - s.padding.bottom)
                .max(0.0),
        ),
        None => (layout.width, layout.height),
    };
    (
        content_width.round().max(1.0) as u32,
        content_height.round().max(1.0) as u32,
    )
}

/// A `type="password"` substitute — verified directly against real
/// Chromium (`getComputedStyle`/`scrollWidth` on injected elements, not
/// guessed): masked dot spacing is uniform, keyed only by character
/// count — a 30-character password of all `I`s and one of all `W`s
/// render at the exact same width, real proportional glyph widths play
/// no part in it. So this shapes a fresh run of that many bullet
/// characters directly (itself real, verified to reproduce the same
/// width Chromium does), rather than reusing the real text's own glyph
/// positions the way an earlier version of this function did.
///
/// Returns the paintable runs (`char_count` bullet glyphs) plus every
/// position `0..=char_count` a caret/selection edge can land on — the
/// last entry is one bullet *past* what's painted, the "just past the
/// last character" position an end-of-text caret needs, without a
/// separate advance-width probe. See [`char_index_at`] for how a real
/// caret/selection pixel position maps to one of these.
fn masked_glyphs(
    font: &mut florui_text::Font,
    char_count: usize,
    family: florui_text::FontFamily,
    font_size: f32,
    font_weight: f32,
) -> (Vec<florui_text::ShapedRun>, Vec<f32>) {
    let probe_text = "\u{2022}".repeat(char_count + 1);
    let mut shaped = font.shape(family, &probe_text, font_size, font_weight);
    let positions: Vec<f32> = shaped
        .runs
        .iter()
        .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.x))
        .collect();
    // The one extra probe bullet above only exists to report the
    // end-of-text position in `positions` -- it was never meant to be
    // painted as part of the real text.
    if let Some(last_run) = shaped.runs.last_mut() {
        last_run.glyphs.pop();
    }
    shaped.runs.retain(|run| !run.glyphs.is_empty());
    (shaped.runs, positions)
}

/// How many of `real_glyphs` (one per real character — a multi-codepoint
/// grapheme cluster is a pre-existing limitation this carries forward,
/// not a new one) sit strictly left of a real caret/selection edge's
/// pixel `x` — that count is exactly that edge's character index into
/// [`masked_glyphs`]'s own `positions`.
fn char_index_at(real_glyphs: &[florui_text::ShapedGlyph], x: f32) -> usize {
    const EPSILON: f32 = 0.5;
    real_glyphs
        .iter()
        .filter(|glyph| glyph.x < x - EPSILON)
        .count()
}

/// Collects every [`crate::WINDOW_INPUT_REGION_CLASS`] element's real
/// screen rectangle from this frame's own committed layout and hands
/// them to [`WindowControls::sync_input_regions`] — only called under
/// [`InputMode::Selective`], so an app that never uses it pays nothing.
fn sync_input_regions(
    controls: &WindowControls,
    window: &Window,
    arena: &florui_style::Arena,
    physical_layouts: &HashMap<NodeId, BoxLayout>,
) {
    let Ok(origin) = window.inner_position() else {
        return;
    };
    let regions: Vec<ScreenRect> = physical_layouts
        .iter()
        .filter(|&(&id, _)| {
            arena
                .classes(id)
                .iter()
                .any(|class| class == crate::WINDOW_INPUT_REGION_CLASS)
        })
        .map(|(_, layout)| ScreenRect {
            left: origin.x + layout.x.round() as i32,
            top: origin.y + layout.y.round() as i32,
            right: origin.x + (layout.x + layout.width).round() as i32,
            bottom: origin.y + (layout.y + layout.height).round() as i32,
        })
        .collect();
    controls.sync_input_regions(&regions);
}

/// Whichever presentation path [`DesktopHost::resumed`] actually got —
/// see [`crate::gpu`]'s own doc for the fallback contract between them.
/// `Cpu` is still `softbuffer`, unchanged from before this existed.
enum Presenter {
    Gpu(Box<GpuPresenter>),
    Cpu {
        surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
        _context: softbuffer::Context<Arc<Window>>,
    },
}

/// One real, live window: its actual `winit::window::Window`, real
/// presenter (GPU-preferred, `softbuffer` fallback — see [`Presenter`]),
/// and the [`UiRuntime`] rendering it — every rendering, hit-testing, and
/// dispatch decision for this one window delegates to that runtime. Only
/// ever exists post-creation (built once in [`DesktopHost::resumed`] from
/// a drained [`WindowSpec`]), so nothing here needs `Option` wrapping the
/// way [`WindowSpec`]'s own fields don't need it either.
struct WindowState {
    canvas_color: Rgba,
    runtime: UiRuntime,
    /// The node hit-tested at the last left-button press, if any — a
    /// click only dispatches on release over this same node.
    pressed: Option<NodeId>,
    /// Where the Space key went down, if it is still held: Space activates
    /// on release, and only if focus is still there (as in real HTML).
    space_armed: Option<florui_style::FocusPath>,
    last_cursor: (f64, f64),
    window: Arc<Window>,
    /// Also reachable from the component tree via
    /// [`crate::use_window_controls`] — kept here too so
    /// [`Self::handle_press`] can recognize a press on
    /// [`crate::WINDOW_DRAG_REGION_ID`] and start a real window drag
    /// itself, without a component needing to wire that up by hand.
    controls: Rc<WindowControls>,
    presenter: Presenter,
    /// When a `transition`/`@keyframes` animation still needs sampling —
    /// `None` once nothing is animating. Read and rescheduled only in
    /// [`ApplicationHandler::about_to_wait`], which runs after every loop
    /// iteration regardless of what triggered it (input, a `Signal::set`,
    /// or a previous animation wake), so it always sees whatever the most
    /// recent [`Self::redraw`] left here — no separate cross-thread
    /// signal needed to drive continuous repaints.
    next_animation_wake: Option<std::time::Instant>,
    /// The spinner arrow currently held down, repeating until release.
    spinner_hold: Option<SpinnerHold>,
    scroll_hold: Option<ScrollHold>,
    scroll_drag: Option<ScrollDrag>,
    resize_drag: Option<ResizeDrag>,
    /// Only set for a window built via [`WindowSpec::with_css_reload`] —
    /// [`Self::reload_css`] is a no-op without it.
    css_path: Option<PathBuf>,
    /// Kept alive only to keep watching; dropping it stops delivery.
    _css_watcher: Option<RecommendedWatcher>,
    /// Kept alive only to keep this window's drop target registered;
    /// dropping it revokes it. `None` when registration itself failed
    /// (see `crate::drag_drop::register`'s own doc) — drag-and-drop is
    /// then simply unavailable for this window, not a fatal error.
    _drag_drop: Option<DragDropRegistration>,
    /// What this window asked for — an explicit `Light`/`Dark` override
    /// makes it correctly immune to a live `WindowEvent::ThemeChanged`
    /// (the window is already immune on the `winit` side too once an
    /// override is set; see [`crate::theme`]'s own module doc), checked in
    /// [`Self::handle_theme_changed`].
    theme_preference: crate::theme::ThemePreference,
    /// What this window was created with — a `Custom`-decorated window has
    /// no OS-drawn resize border at all, so [`Self::handle_press`]/
    /// [`Self::handle_cursor_moved`] only hit-test [`resize_direction_at`]
    /// when this is [`DecorationMode::Custom`]; a `System`-decorated window
    /// already gets real OS resize edges for free and must not have this
    /// crate's own margin double up on top of them.
    decorations: DecorationMode,
    /// `Some` when this window opted into bounds persistence — read at
    /// close time (see [`DesktopHost::close_if_confirmed`]) to flush a
    /// final save.
    persistence: Option<WindowPersistence>,
    /// `Some` once this window's bounds changed since the last successful
    /// save; cleared on save. Reset (not just refreshed) on every further
    /// change, so a continuous resize drag keeps pushing the save deadline
    /// out — a true debounce, not a fixed-interval throttle — instead of
    /// saving mid-drag on every qualifying tick.
    pending_geometry_save: Option<std::time::Instant>,
    /// Updated only by a live `WindowEvent::ModifiersChanged` — a real
    /// `KeyEvent` carries no modifier state of its own, so Shift+Tab needs
    /// this to distinguish itself from a plain Tab.
    modifiers: ModifiersState,
    /// The editable `<input>` a left-button press started a text
    /// selection drag on, if any — still down, not yet released.
    /// `CursorMoved` while this is `Some` extends the selection to the
    /// cursor's current position; `handle_release` clears it.
    text_selecting: Option<NodeId>,
    /// The node and instant of the last real left-button press on an
    /// editable `<input>` — a second press on the *same* node within
    /// [`DOUBLE_CLICK_INTERVAL`] selects the word under the cursor
    /// instead of just moving the caret there, the same distinction a
    /// real double-click makes. No existing double-click detection exists
    /// anywhere else in this file to reuse.
    last_text_input_click: Option<(NodeId, std::time::Instant, u8)>,
    /// The instant of the last real left-button press on
    /// [`crate::WINDOW_DRAG_REGION_ID`] — a second press within
    /// [`DOUBLE_CLICK_INTERVAL`] toggles maximize instead of starting
    /// another drag, the same distinction a real title bar makes.
    last_drag_region_click: Option<std::time::Instant>,
    /// Real AccessKit wiring for this window -- see `resumed`'s own doc
    /// for why it must be constructed before the window is first shown.
    accessibility_adapter: accesskit_winit::Adapter,
    /// Builds the real AccessKit tree from this window's own `Arena`
    /// every redraw. Kept per-window (not per-`UiRuntime`) because it
    /// needs post-scroll, DPI-scaled, window-relative bounds that only
    /// exist here, in `redraw` -- see `accessibility::tree`'s own doc.
    accessibility_tree: accessibility::tree::AccessibilityTree,
    /// This render's translation from an AccessKit id back to a real
    /// node -- rebuilt every `redraw`, read by an inbound `ActionRequest`
    /// arriving before the next one.
    accessibility_reverse: HashMap<accesskit::NodeId, NodeId>,
}

/// Not spec-mandated to an exact number — a common real-OS default for
/// "two clicks this close together count as one double-click."
const DOUBLE_CLICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(400);

/// Holding a number field's spinner arrow, or a textarea scrollbar's button
/// or track, acts once on the press, waits this long, then acts every
/// [`HOLD_REPEAT_INTERVAL`] until released (measured in Edge: first repeat
/// ~250ms, then about every 50ms).
const HOLD_REPEAT_DELAY: std::time::Duration = std::time::Duration::from_millis(250);
const HOLD_REPEAT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// A scrollbar thumb being dragged: `grab` is how far below the thumb's top
/// the pointer was when the drag began.
struct ScrollDrag {
    id: String,
    grab: f32,
}

/// A textarea's geometry as the pointer sees it.
struct TextareaBox {
    id: String,
    origin: (f32, f32),
    size: (f32, f32),
    border: (f32, f32, f32, f32),
    chrome: (f32, f32),
    mode: florui_style::Resize,
    bar: Option<florui_paint::ScrollbarPaint>,
}

/// A textarea's resize corner being dragged.
struct ResizeDrag {
    id: String,
    start_pointer: (f32, f32),
    start_border_box: (f32, f32),
    /// Border plus padding, horizontally and vertically, to turn a border
    /// box size back into the content size the element is sized by.
    chrome: (f32, f32),
    mode: florui_style::Resize,
}

/// The cursor over a textarea's resize corner, by the directions it resizes.
fn resize_cursor(mode: florui_style::Resize) -> winit::window::CursorIcon {
    use winit::window::CursorIcon;
    match mode {
        florui_style::Resize::Vertical => CursorIcon::NsResize,
        florui_style::Resize::Horizontal => CursorIcon::EwResize,
        _ => CursorIcon::NwseResize,
    }
}

/// How far one action on `part` scrolls a textarea whose visible height is
/// `viewport`; `None` for the parts that don't scroll by a step.
fn scroll_step(part: florui_paint::ScrollbarPart, viewport: f32) -> Option<f32> {
    use florui_paint::ScrollbarPart as Part;
    match part {
        Part::ButtonUp => Some(-WHEEL_LINE_HEIGHT),
        Part::ButtonDown => Some(WHEEL_LINE_HEIGHT),
        Part::TrackBefore => Some(-viewport * 0.875),
        Part::TrackAfter => Some(viewport * 0.875),
        Part::Thumb | Part::None => None,
    }
}

/// A textarea scrollbar button or track being held down.
struct ScrollHold {
    id: String,
    part: florui_paint::ScrollbarPart,
    next_step: std::time::Instant,
}

/// A number field's spinner arrow being held down.
struct SpinnerHold {
    field: florui_style::FocusPath,
    direction: i32,
    next_step: std::time::Instant,
}

/// Long enough that a drag-resize (many `Resized`/`Moved` events per
/// second) collapses into one save after the user stops; short enough
/// that a crash/kill within a second or two of the last move doesn't lose
/// much. Not spec-mandated to an exact number, only "bounded/debounced."
const WINDOW_STATE_SAVE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(500);

/// Logical pixels one wheel "line" (`MouseScrollDelta::LineDelta`'s own
/// unit) scrolls — real mouse wheels report in lines, not pixels, so this
/// is the conversion factor into the logical pixels a scroll offset is
/// measured in. Not spec-mandated to an exact number, the same as
/// [`WINDOW_STATE_SAVE_DEBOUNCE`]: browsers commonly use a value in this
/// range for the same conversion.
const WHEEL_LINE_HEIGHT: f32 = 40.0;

impl WindowState {
    fn viewport_scale(&self) -> ViewportScale {
        dpi::viewport_scale(self.window.inner_size(), self.window.scale_factor())
    }

    /// Converts a physical-pixel cursor position (as `winit` reports it)
    /// to the logical pixels layout runs against.
    fn to_logical_cursor(&self, x: f64, y: f64) -> (f32, f32) {
        let factor = self.viewport_scale().scale_factor;
        ((x / factor) as f32, (y / factor) as f32)
    }

    /// Only [`DecorationMode::Custom`] ever hit-tests a resize border —
    /// see [`Self::decorations`]'s own doc for why a `System`-decorated
    /// window must not.
    fn resize_direction_at_cursor(&self, x: f32, y: f32) -> Option<ResizeDirection> {
        if self.decorations != DecorationMode::Custom {
            return None;
        }
        let logical = self.viewport_scale().logical;
        resize_direction_at(logical.width, logical.height, x, y)
    }

    /// A real wheel/trackpad event's delta, converted to the logical
    /// pixels a scroll *offset* moves by — a real mouse wheel reports
    /// whole "lines" ([`WHEEL_LINE_HEIGHT`] logical pixels each), a
    /// trackpad reports already-fine-grained physical pixels needing only
    /// the same physical-to-logical scale [`Self::to_logical_cursor`]
    /// already applies to cursor positions.
    ///
    /// Negated on both axes: `winit`'s own `MouseScrollDelta` doc defines a
    /// positive value as "content...should move right and down (revealing
    /// more content left and up)" — the opposite of this offset's own
    /// scroll-position convention (also the DOM's `wheel` event
    /// convention), where a positive value reveals more content
    /// right/down by *increasing* the offset, not moving the content
    /// itself right/down. Scrolling down (revealing lower content) must
    /// increase `offset.1`, not decrease it.
    fn to_logical_scroll_delta(&self, delta: MouseScrollDelta) -> (f32, f32) {
        match delta {
            MouseScrollDelta::LineDelta(x, y) => (-x * WHEEL_LINE_HEIGHT, -y * WHEEL_LINE_HEIGHT),
            MouseScrollDelta::PixelDelta(position) => {
                let factor = self.viewport_scale().scale_factor;
                ((-position.x / factor) as f32, (-position.y / factor) as f32)
            }
        }
    }

    /// The nearest ancestor of `node` (`node` itself included) that is
    /// real CSS's `overflow: scroll`/`auto` on the axis `(dx, dy)` actually
    /// moves along, has more real content than its own viewport on that
    /// axis, and carries an `id` attribute with a live
    /// [`crate::use_scroll_offset`] registration for it — real CSS lets an
    /// `overflow: auto` element with nothing to scroll pass a wheel event
    /// through to a further ancestor, and an ancestor this scroll registry
    /// has never heard of (no matching `use_scroll_offset` call, not just
    /// no `id`) has no offset to move in the first place. `None` when no
    /// ancestor qualifies — the event is simply not a scroll anywhere.
    fn scrollable_ancestor_id(&self, node: NodeId, dx: f32, dy: f32) -> Option<String> {
        let (arena, styles, ..) = self.runtime.geometry();
        let registry = self.runtime.scroll_registry();
        let mut current = Some(node);
        while let Some(id) = current {
            if let Some(style) = styles.get(&id)
                && let Some(attr_id) = arena.id_attr(id)
            {
                let wants_x = dx != 0.0 && style.overflow_scrolls_x;
                let wants_y = dy != 0.0 && style.overflow_scrolls_y;
                let (viewport_w, viewport_h) = registry.viewport_size(attr_id);
                let (content_w, content_h) = registry.content_size(attr_id);
                let scrolls_x = wants_x && content_w > viewport_w;
                let scrolls_y = wants_y && content_h > viewport_h;
                if scrolls_x || scrolls_y {
                    return Some(attr_id.to_string());
                }
            }
            current = arena.parent(id);
        }
        None
    }

    /// The `id` of `node` when it is a `<textarea>`, which scrolls its own
    /// text before any ancestor does.
    fn textarea_id(&self, node: NodeId) -> Option<String> {
        let (arena, ..) = self.runtime.geometry();
        (arena.tag(node) == "textarea")
            .then(|| arena.id_attr(node).map(str::to_owned))
            .flatten()
    }

    /// Real mouse-wheel/trackpad input, hit-tested and routed to whichever
    /// scrollable ancestor actually owns it — see
    /// [`Self::scrollable_ancestor_id`]. Keyboard-driven scrolling (arrow
    /// keys, Page Up/Down, Home/End) is a real, documented gap: no focus
    /// model exists anywhere in this crate yet for a keyboard event to
    /// resolve *which* element it should even move.
    fn handle_mouse_wheel(&mut self, delta: MouseScrollDelta) {
        let (dx, dy) = self.to_logical_scroll_delta(delta);
        let (x, y) = self.to_logical_cursor(self.last_cursor.0, self.last_cursor.1);
        let Some(hit) = self.runtime.hit_test(x, y) else {
            return;
        };
        if let Some(text_id) = self.textarea_id(hit) {
            let registry = self.runtime.text_input_registry();
            let (_, _, _, font) = self.runtime.geometry_and_font_mut();
            if registry.scroll_by(&text_id, dy, font) {
                self.window.request_redraw();
                return;
            }
        }
        let Some(id) = self.scrollable_ancestor_id(hit, dx, dy) else {
            return;
        };
        if self.runtime.scroll_registry().scroll_by(&id, dx, dy) {
            let viewport = layout_viewport(self.viewport_scale());
            self.runtime.update(viewport);
            self.window.request_redraw();
            self.refresh_animation_schedule();
        }
    }

    fn redraw(&mut self) {
        let scale_factor = self.viewport_scale().scale_factor;
        let window = self.window.clone();
        let size = window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };

        let scroll_registry = self.runtime.scroll_registry();
        let text_input_registry = self.runtime.text_input_registry();
        let image_registry = self.runtime.image_registry();
        let icon_registry = self.runtime.icon_registry();
        let asset_cache = self.runtime.asset_cache();
        let executor = self.runtime.executor();
        let focused = self.runtime.focused();
        let hovered = self.runtime.hovered();
        let cursor = self.to_logical_cursor(self.last_cursor.0, self.last_cursor.1);
        let (arena, styles, layouts, font, interaction) =
            self.runtime.geometry_font_and_interaction_mut();
        let scroll_offsets = scroll_registry.offsets_by_node(arena);
        let scrolled_layouts = florui_layout::apply_scroll_offsets(arena, layouts, &scroll_offsets);
        let physical_layouts = scale_layouts(&scrolled_layouts, scale_factor as f32);
        if self.controls.input_mode() == InputMode::Selective {
            sync_input_regions(&self.controls, &window, arena, &physical_layouts);
        }
        let spinner_for = |node: NodeId| {
            if arena.input_type(node) != Some("number")
                || arena.is_disabled(node)
                || arena.attr_flag(node, "readonly")
            {
                return None;
            }
            let is_hovered = hovered == Some(node);
            if !is_hovered && focused != Some(node) {
                return None;
            }
            let layout = layouts.get(&node)?;
            let (x, y) = florui_layout::absolute_position(arena, layouts, node);
            Some(if is_hovered {
                florui_paint::spinner_half_at((x, y, layout.width, layout.height), cursor)
            } else {
                florui_paint::SpinnerHover::None
            })
        };
        let text_inputs = build_text_input_paint(
            font,
            TextInputPaintContext {
                arena,
                styles,
                layouts,
                registry: &text_input_registry,
                focused,
                hovered,
                pointer: cursor,
                scroll_dragging: self.scroll_drag.as_ref().map(|drag| drag.id.as_str()),
            },
            &spinner_for,
        );
        // One combined map: `florui_paint` blits either tag's own decoded
        // pixels identically (see its own "img"/"icon" tag check), so it
        // only needs one `NodeId -> ImagePaint` map, not one per registry.
        let mut images = build_image_paint(
            arena,
            styles,
            &physical_layouts,
            &image_registry,
            &asset_cache,
            &*executor,
        );
        images.extend(build_icon_paint(
            arena,
            styles,
            &physical_layouts,
            &icon_registry,
            &asset_cache,
            &*executor,
        ));
        images.extend(build_control_icon_paint(
            arena,
            styles,
            &icon_registry,
            &asset_cache,
            &*executor,
            scale_factor as f32,
        ));
        if let Some(node) = focused
            && let Some(paint) = text_inputs.get(&node)
            && let Some((x0, y0, x1, y1)) = paint.compose_rect
        {
            // Logical coordinates straight through -- `set_ime_cursor_area`
            // accepts either `Logical*`/`Physical*` and converts using the
            // window's own scale factor internally, so no manual
            // `scale_factor` multiplication belongs here (unlike
            // `physical_layouts`, which painting needs pre-scaled).
            let (origin_x, origin_y) = text_input_content_origin(arena, styles, layouts, node);
            window.set_ime_cursor_area(
                winit::dpi::LogicalPosition::new(origin_x + x0, origin_y + y0),
                winit::dpi::LogicalSize::new((x1 - x0).max(1.0), (y1 - y0).max(1.0)),
            );
        }

        let node_bounds: HashMap<NodeId, (f32, f32, f32, f32)> =
            florui_layout::screen_bounds(arena, &physical_layouts, styles, scale_factor as f32);
        let (accessibility_update, accessibility_reverse) =
            self.accessibility_tree
                .build(arena, focused, &node_bounds, interaction);
        self.accessibility_reverse = accessibility_reverse;
        self.accessibility_adapter
            .update_if_active(|| accessibility_update);

        let canvas = florui_paint::paint_to_buffer_with_desktop_extras(
            font,
            size.width,
            size.height,
            self.canvas_color,
            arena,
            styles,
            &physical_layouts,
            scale_factor as f32,
            Some(&text_inputs),
            Some(&images),
        );

        match &mut self.presenter {
            Presenter::Gpu(presenter) => {
                // tiny-skia's own pixel format is RGBA byte order,
                // premultiplied — matches `GpuPresenter`'s own upload
                // texture format exactly, so the painted bytes go straight
                // across with no channel swizzle.
                presenter.resize(width.get(), height.get());
                presenter.present(canvas.data());
            }
            Presenter::Cpu { surface, .. } => {
                if let Err(error) = surface.resize(width, height) {
                    eprintln!("florui-platform: could not resize the render surface: {error}");
                    return;
                }
                let mut buffer = match surface.buffer_mut() {
                    Ok(buffer) => buffer,
                    Err(error) => {
                        eprintln!("florui-platform: render surface buffer unavailable: {error}");
                        return;
                    }
                };
                // `softbuffer`'s own pixel format has no alpha channel at
                // all (see `crate::appearance`'s own doc) — the canvas is
                // always painted fully opaque today regardless, so
                // dropping alpha here is a no-op, not a lossy conversion.
                let pixels: Vec<u32> = canvas
                    .pixels()
                    .iter()
                    .map(|p| u32::from_be_bytes([0, p.red(), p.green(), p.blue()]))
                    .collect();
                buffer.copy_from_slice(&pixels);
                if let Err(error) = buffer.present() {
                    eprintln!("florui-platform: could not present the frame: {error}");
                }
            }
        }
    }

    /// The real handler for `WindowEvent::RedrawRequested`: if the reason
    /// this frame was requested is [`Self::next_animation_wake`] having
    /// come due, advances the animation timeline first so the paint below
    /// reflects the current instant — otherwise this is a plain repaint
    /// (a resize, an exposed region) and [`Self::redraw`] alone is
    /// correct, matching the design's own separation of "update" from
    /// "paint" everywhere else.
    fn redraw_for_frame(&mut self) {
        let deadline_due = self
            .next_animation_wake
            .is_some_and(|deadline| std::time::Instant::now() >= deadline);
        if deadline_due {
            let viewport = layout_viewport(self.viewport_scale());
            self.runtime
                .set_os_prefers_reduced_motion(crate::accessibility::prefers_reduced_motion());
            self.runtime.update(viewport);
            self.refresh_animation_schedule();
        }
        self.redraw();
    }

    /// Re-renders against the current viewport and requests a repaint —
    /// used both after a resize and after a [`UserEvent::Dirty`], so any
    /// `Signal::set` anywhere under the root reaches the screen without
    /// the host having to know which specific interaction caused it.
    fn update_and_request_redraw(&mut self) {
        let viewport = layout_viewport(self.viewport_scale());
        self.runtime.clear_dirty();
        self.runtime
            .set_os_prefers_reduced_motion(crate::accessibility::prefers_reduced_motion());
        self.runtime.update(viewport);
        self.window.request_redraw();
        self.refresh_animation_schedule();
    }

    /// Recomputes [`Self::next_animation_wake`] from
    /// [`UiRuntime::is_animating`] — called after every `runtime.update`
    /// site, so whichever one most recently ran always leaves an accurate
    /// deadline for [`ApplicationHandler::about_to_wait`] to act on. A
    /// stale deadline is intentional between calls: a real transition/
    /// animation samples at whatever instant it actually gets painted at
    /// (`Transition::calculate_value` is time-based, not tied to landing
    /// exactly on a 16ms boundary), so nothing here needs to be exact —
    /// only to keep asking for another frame while it's still true.
    fn refresh_animation_schedule(&mut self) {
        self.next_animation_wake = self
            .runtime
            .is_animating()
            .then(|| std::time::Instant::now() + std::time::Duration::from_millis(16));
    }

    /// Re-derives `InputMode::Selective`'s screen-space regions from the
    /// runtime's already-computed layout — no re-render, just the new
    /// window offset applied to geometry that hasn't otherwise changed.
    fn resync_input_regions(&self) {
        if self.controls.input_mode() != InputMode::Selective {
            return;
        }
        let scale_factor = self.viewport_scale().scale_factor;
        let (arena, _, layouts) = self.runtime.geometry();
        let physical_layouts = scale_layouts(layouts, scale_factor as f32);
        sync_input_regions(&self.controls, &self.window, arena, &physical_layouts);
    }

    /// Marks this window's bounds as changed since the last save, resetting
    /// (not just refreshing) the debounce deadline — see
    /// [`WINDOW_STATE_SAVE_DEBOUNCE`]'s own doc. A no-op window without
    /// persistence enabled.
    fn mark_geometry_dirty(&mut self) {
        if self.persistence.is_some() {
            self.pending_geometry_save = Some(std::time::Instant::now());
        }
    }

    /// Writes this window's current geometry if a change is pending and its
    /// debounce deadline has passed, clearing the pending flag either way
    /// (an unreachable-monitor/disabled edge case never reaches this with
    /// `persistence: None`, since [`Self::mark_geometry_dirty`] never sets
    /// it then). Called from [`ApplicationHandler::about_to_wait`] once per
    /// loop iteration, and unconditionally (regardless of any pending
    /// deadline) from [`DesktopHost::close_if_confirmed`] before this
    /// window's state is dropped.
    fn flush_geometry_save_if_due(&mut self, now: std::time::Instant) {
        let Some(persistence) = &self.persistence else {
            return;
        };
        let Some(changed_at) = self.pending_geometry_save else {
            return;
        };
        if now < changed_at + WINDOW_STATE_SAVE_DEBOUNCE {
            return;
        }
        window_state::capture_and_save(&self.window, persistence);
        self.pending_geometry_save = None;
    }

    /// Updates `:hover` against the runtime's cached geometry — no
    /// rebuild just to know what's under the cursor. While a text-input
    /// drag-select is in progress (see [`Self::handle_text_input_press`]),
    /// also extends that selection to the cursor's current position —
    /// still tracked even once the cursor drags outside the input's own
    /// box, matching real text-selection behavior.
    fn handle_cursor_moved(&mut self, x: f64, y: f64) {
        self.last_cursor = (x, y);
        let (x, y) = self.to_logical_cursor(x, y);
        // A resize edge always wins over content's own `cursor: pointer`
        // -- resolved once here so the check below can skip content
        // cursor logic entirely rather than have both methods race to
        // set the OS cursor on every move.
        let resize_direction = (self.decorations == DecorationMode::Custom)
            .then(|| self.resize_direction_at_cursor(x, y))
            .flatten();
        if self.decorations == DecorationMode::Custom {
            self.controls.set_resize_cursor(resize_direction);
        }
        if let Some(drag) = self.resize_drag.as_ref() {
            self.controls.set_content_cursor(resize_cursor(drag.mode));
        }
        if self.drag_textarea_chrome(x, y) {
            return;
        }
        if let Some(node) = self.text_selecting {
            let Some(id) = ({
                let (arena, ..) = self.runtime.geometry();
                arena.id_attr(node).map(str::to_owned)
            }) else {
                return;
            };
            let (local_x, local_y) = self.text_input_local_point(node, &id, x, y);
            let registry = self.runtime.text_input_registry();
            let (_, _, _, font) = self.runtime.geometry_and_font_mut();
            registry.apply(
                &id,
                TextEditOp::ExtendSelectionToPoint(local_x, local_y),
                font,
            );
            self.update_and_request_redraw();
            return;
        }
        if self.runtime.is_range_dragging() {
            if self.runtime.continue_range_drag(x).is_some() {
                self.update_and_request_redraw();
            }
            return;
        }
        let hit = self
            .runtime
            .hit_test(x, y)
            .filter(|&node| !self.is_disabled(node));
        self.set_hovered_and_redraw(hit);
        if hit.is_some_and(|node| {
            let (arena, ..) = self.runtime.geometry();
            arena.input_type(node) == Some("number") || arena.tag(node) == "textarea"
        }) {
            self.window.request_redraw();
        }
        if resize_direction.is_none() {
            let icon = self.content_cursor(hit, x, y);
            self.controls.set_content_cursor(icon);
        }
    }

    /// The cursor for what's under the pointer: measured in Edge, an I-beam
    /// over an enabled text field's text (not its spinner) and the resize
    /// arrows over a resizable textarea's corner; everything else, scrollbar
    /// included, keeps the arrow.
    fn content_cursor(&mut self, hit: Option<NodeId>, x: f32, y: f32) -> winit::window::CursorIcon {
        use winit::window::CursorIcon;
        let Some(node) = hit else {
            return CursorIcon::Default;
        };
        if let Some(b) = self.textarea_box(node) {
            let rel = (x - b.origin.0, y - b.origin.1);
            if b.mode != florui_style::Resize::None
                && florui_paint::is_over_resizer(b.size, b.border, rel)
            {
                return resize_cursor(b.mode);
            }
            if let Some(bar) = b.bar {
                let geometry = florui_paint::scrollbar_geometry(
                    b.size,
                    b.border,
                    b.mode != florui_style::Resize::None,
                    bar,
                );
                if florui_paint::scrollbar_part_at(&geometry, rel)
                    != florui_paint::ScrollbarPart::None
                {
                    return CursorIcon::Default;
                }
            }
        }
        let (_, styles, ..) = self.runtime.geometry();
        if styles.get(&node).is_some_and(|style| style.cursor_pointer) {
            return CursorIcon::Pointer;
        }
        if self.is_editable_text_input(node) && self.spinner_press_direction(node, x, y).is_none() {
            return CursorIcon::Text;
        }
        CursorIcon::Default
    }

    /// The cursor leaving the window cancels any in-progress press (there
    /// is nowhere left to release onto) and clears `:hover` — otherwise
    /// whichever element was last under the cursor would stay visually
    /// `:hover`ed even after the mouse has left the window entirely.
    fn handle_cursor_left(&mut self) {
        self.pressed = None;
        self.set_hovered_and_redraw(None);
    }

    /// Applies a hover change and, only when it actually changed anything
    /// (`:hover` can affect computed style), dispatches `mouseleave`/
    /// `mouseenter` and re-renders to pick up both. `runtime.hovered()` is
    /// read before `set_hovered` overwrites it, so both `NodeId`s are
    /// valid against the same arena generation.
    fn set_hovered_and_redraw(&mut self, hit: Option<NodeId>) {
        let viewport = layout_viewport(self.viewport_scale());
        let previous = self.runtime.hovered();
        if !self.runtime.set_hovered(hit) {
            return;
        }
        if let Some(node) = previous {
            self.runtime.dispatch_event(node, "mouseleave");
        }
        if let Some(node) = hit {
            self.runtime.dispatch_event(node, "mouseenter");
        }
        self.runtime
            .set_os_prefers_reduced_motion(crate::accessibility::prefers_reduced_motion());
        self.runtime.update(viewport);
        self.window.request_redraw();
        self.refresh_animation_schedule();
    }

    /// Reacts to a real, live OS theme change — only when this window
    /// asked to follow it ([`crate::theme::ThemePreference::System`]); an
    /// explicit override already ignores this on the `winit` side (see
    /// [`crate::theme`]'s own module doc), so this check just keeps
    /// florui's own signal consistent with what the window is actually
    /// doing. Unlike reduced motion, no per-frame polling is needed
    /// anywhere in this file: `WindowEvent::ThemeChanged` tells this host
    /// exactly when the value actually changes.
    fn handle_theme_changed(&mut self, theme: winit::window::Theme) {
        if self.theme_preference != crate::theme::ThemePreference::System {
            return;
        }
        let viewport = layout_viewport(self.viewport_scale());
        self.runtime
            .set_prefers_dark_color_scheme(crate::theme::ColorScheme::from(theme).is_dark());
        self.runtime.update(viewport);
        self.window.request_redraw();
        self.refresh_animation_schedule();
    }

    /// Reacts to a real `WindowEvent::Focused`: records it on
    /// [`WindowControls`] (the live-query value [`WindowControls::is_focused`]
    /// reads back) and re-renders so a component styling itself from that
    /// query — e.g. dimming a custom caption while inactive — picks up the
    /// change immediately rather than waiting for an unrelated redraw.
    fn handle_focus_changed(&mut self, focused: bool) {
        self.controls.set_focused(focused);
        if !focused {
            self.spinner_hold = None;
            self.scroll_hold = None;
            self.runtime.window_focus_lost();
        }
        self.update_and_request_redraw();
    }

    /// A textarea's box, border, padding, resize mode and, when its text
    /// overflows, the scrollbar it is drawn with.
    fn textarea_box(&mut self, node: NodeId) -> Option<TextareaBox> {
        let (id, origin, size, border, chrome, mode, client_height) = {
            let (arena, styles, layouts) = self.runtime.geometry();
            if arena.tag(node) != "textarea" {
                return None;
            }
            let id = arena.id_attr(node)?.to_string();
            let style = styles.get(&node)?;
            let layout = layouts.get(&node)?;
            let border = (
                style.border.left.width,
                style.border.top.width,
                style.border.right.width,
                style.border.bottom.width,
            );
            let chrome = (
                border.0 + border.2 + style.padding.left + style.padding.right,
                border.1 + border.3 + style.padding.top + style.padding.bottom,
            );
            (
                id,
                florui_layout::absolute_position(arena, layouts, node),
                (layout.width, layout.height),
                border,
                chrome,
                style.resize,
                layout.height - border.1 - border.3,
            )
        };
        let registry = self.runtime.text_input_registry();
        let (_, _, _, font) = self.runtime.geometry_and_font_mut();
        let bar = registry
            .scroll_metrics(&id, font)
            .map(|(scroll, max_scroll)| florui_paint::ScrollbarPaint {
                scroll,
                max_scroll,
                viewport: client_height,
                active: florui_paint::ScrollbarPart::None,
            });
        Some(TextareaBox {
            id,
            origin,
            size,
            border,
            chrome,
            mode,
            bar,
        })
    }

    /// A press on a textarea's scrollbar or resize corner; `true` if it
    /// landed on one (and was handled), `false` for the text itself.
    fn press_textarea_chrome(&mut self, node: NodeId, x: f32, y: f32) -> bool {
        let Some(b) = self.textarea_box(node) else {
            return false;
        };
        let rel = (x - b.origin.0, y - b.origin.1);
        let resizable = b.mode != florui_style::Resize::None;
        if resizable && florui_paint::is_over_resizer(b.size, b.border, rel) {
            self.resize_drag = Some(ResizeDrag {
                id: b.id,
                start_pointer: (x, y),
                start_border_box: b.size,
                chrome: b.chrome,
                mode: b.mode,
            });
            return true;
        }
        let Some(bar) = b.bar else {
            return false;
        };
        let geometry = florui_paint::scrollbar_geometry(b.size, b.border, resizable, bar);
        let registry = self.runtime.text_input_registry();
        let part = florui_paint::scrollbar_part_at(&geometry, rel);
        if part == florui_paint::ScrollbarPart::Thumb {
            self.scroll_drag = Some(ScrollDrag {
                id: b.id,
                grab: rel.1 - geometry.thumb.1,
            });
            self.update_and_request_redraw();
            return true;
        }
        let Some(step) = scroll_step(part, bar.viewport) else {
            return false;
        };
        let (_, _, _, font) = self.runtime.geometry_and_font_mut();
        registry.scroll_by(&b.id, step, font);
        self.scroll_hold = Some(ScrollHold {
            id: b.id,
            part,
            next_step: std::time::Instant::now() + HOLD_REPEAT_DELAY,
        });
        self.update_and_request_redraw();
        true
    }

    /// Repeats a held scrollbar button or track once its next step is due,
    /// and returns when the one after that is, or `None` when nothing is
    /// held. Like a browser, a track stops paging once the thumb reaches the
    /// pointer, and a button pauses while the pointer is off it.
    fn repeat_scroll_if_due(&mut self, now: std::time::Instant) -> Option<std::time::Instant> {
        let hold = self.scroll_hold.as_ref()?;
        if now < hold.next_step {
            return Some(hold.next_step);
        }
        let (id, part) = (hold.id.clone(), hold.part);
        let node = {
            let (arena, ..) = self.runtime.geometry();
            arena.find(|a, n| a.id_attr(n) == Some(id.as_str()))
        };
        let Some(b) = node.and_then(|node| self.textarea_box(node)) else {
            self.scroll_hold = None;
            return None;
        };
        if let Some(bar) = b.bar {
            let (x, y) = self.to_logical_cursor(self.last_cursor.0, self.last_cursor.1);
            let rel = (x - b.origin.0, y - b.origin.1);
            let geometry = florui_paint::scrollbar_geometry(
                b.size,
                b.border,
                b.mode != florui_style::Resize::None,
                bar,
            );
            if florui_paint::scrollbar_part_at(&geometry, rel) == part
                && let Some(step) = scroll_step(part, bar.viewport)
            {
                let registry = self.runtime.text_input_registry();
                let (_, _, _, font) = self.runtime.geometry_and_font_mut();
                registry.scroll_by(&id, step, font);
                self.update_and_request_redraw();
            }
        }
        let next = now + HOLD_REPEAT_INTERVAL;
        if let Some(hold) = self.scroll_hold.as_mut() {
            hold.next_step = next;
        }
        Some(next)
    }

    /// Follows the pointer while a scrollbar thumb or resize corner is being
    /// dragged; `true` if one is, so nothing else reacts to the move.
    fn drag_textarea_chrome(&mut self, x: f32, y: f32) -> bool {
        if let Some(drag) = self.scroll_drag.as_ref().map(|d| (d.id.clone(), d.grab)) {
            let node = {
                let (arena, ..) = self.runtime.geometry();
                arena.find(|a, n| a.id_attr(n) == Some(drag.0.as_str()))
            };
            if let Some(b) = node.and_then(|node| self.textarea_box(node))
                && let Some(bar) = b.bar
            {
                let geometry = florui_paint::scrollbar_geometry(
                    b.size,
                    b.border,
                    b.mode != florui_style::Resize::None,
                    bar,
                );
                let travel =
                    (geometry.track_bottom - geometry.track_top - geometry.thumb.3).max(1.0);
                let top = (y - b.origin.1) - drag.1;
                let fraction = ((top - geometry.track_top) / travel).clamp(0.0, 1.0);
                let registry = self.runtime.text_input_registry();
                let (_, _, _, font) = self.runtime.geometry_and_font_mut();
                registry.scroll_to(&b.id, fraction * bar.max_scroll, font);
                self.update_and_request_redraw();
            }
            return true;
        }
        if let Some(drag) = &self.resize_drag {
            let (dx, dy) = (x - drag.start_pointer.0, y - drag.start_pointer.1);
            let horizontal = !matches!(drag.mode, florui_style::Resize::Vertical);
            let vertical = !matches!(drag.mode, florui_style::Resize::Horizontal);
            let min = crate::textarea_resize::MIN_CONTENT_SIZE;
            let width = if horizontal {
                (drag.start_border_box.0 + dx - drag.chrome.0).max(min.0)
            } else {
                drag.start_border_box.0 - drag.chrome.0
            };
            let height = if vertical {
                (drag.start_border_box.1 + dy - drag.chrome.1).max(min.1)
            } else {
                drag.start_border_box.1 - drag.chrome.1
            };
            let id = drag.id.clone();
            self.runtime.set_resized(&id, (width, height));
            self.update_and_request_redraw();
            return true;
        }
        false
    }

    /// Tabbing into a single-line text field selects everything in it, as
    /// Edge does (measured); a textarea keeps its caret, and a click never
    /// selects all.
    fn select_all_on_tab_focus(&mut self, node: Option<NodeId>) {
        let Some(node) = node else {
            return;
        };
        let Some(id) = ({
            let (arena, ..) = self.runtime.geometry();
            (arena.tag(node) == "input")
                .then(|| arena.id_attr(node).map(str::to_owned))
                .flatten()
        }) else {
            return;
        };
        let registry = self.runtime.text_input_registry();
        let (_, _, _, font) = self.runtime.geometry_and_font_mut();
        registry.apply(&id, TextEditOp::SelectAll, font);
    }

    /// A press dismisses the validation bubble, except one that only moves
    /// the caret inside the very field the bubble hangs from (measured in
    /// Edge: clicking empty page area dismisses it).
    fn dismiss_bubble_on_press(&mut self, hit: Option<NodeId>) {
        let inside_own_text_field = hit.is_some_and(|node| {
            self.runtime.validation_bubble_field() == Some(node)
                && self.is_editable_text_input(node)
        });
        if !inside_own_text_field && self.runtime.dismiss_validation_bubble() {
            self.update_and_request_redraw();
        }
    }

    /// Remembers whichever node is under the cursor at press time — the
    /// click itself only fires on release, and only if that release lands
    /// back on this same node (so dragging off a button and releasing
    /// elsewhere cancels it). A press that lands exactly on
    /// [`crate::WINDOW_DRAG_REGION_ID`] is a different gesture entirely —
    /// see that constant's own doc — and starts a real window drag instead
    /// of ever becoming a click candidate, unless it's a second press
    /// within [`DOUBLE_CLICK_INTERVAL`], which toggles maximize instead;
    /// `winit`'s own `drag_window` takes over the mouse for the rest of a
    /// single-click drag gesture, so there is no matching press to
    /// remember here.
    fn handle_press(&mut self) {
        let (x, y) = self.to_logical_cursor(self.last_cursor.0, self.last_cursor.1);
        let hit = self.runtime.hit_test(x, y);
        self.dismiss_bubble_on_press(hit);

        // Checked before any early return below -- must fire regardless
        // of what `hit` turns out to be.
        let dismissed = {
            let (arena, ..) = self.runtime.geometry();
            popover::dismissed_by_click(arena, hit)
        };
        for root in dismissed {
            self.runtime.dispatch_event(root, "dismiss");
        }

        // Checked before the drag region: a `Custom` window's own top few
        // pixels are still a resize border first, exactly like a real
        // title bar's top edge on a system-decorated window.
        if let Some(direction) = self.resize_direction_at_cursor(x, y) {
            self.controls.start_resize(direction);
            return;
        }

        if hit.is_some_and(|node| self.is_drag_region(node)) {
            let now = std::time::Instant::now();
            let is_double_click = self
                .last_drag_region_click
                .is_some_and(|at| now - at < DOUBLE_CLICK_INTERVAL);
            self.last_drag_region_click = Some(now);
            if is_double_click {
                self.controls.toggle_maximize();
            } else {
                self.controls.drag();
            }
            return;
        }
        if let Some(node) = hit
            && !self.is_disabled(node)
            && self.press_textarea_chrome(node, x, y)
        {
            return;
        }
        if let Some(node) = hit
            && !self.is_disabled(node)
            && let Some(direction) = self.spinner_press_direction(node, x, y)
        {
            self.runtime.set_focused(Some(node), false);
            self.step_spinner(node, direction);
            self.spinner_hold = Some(SpinnerHold {
                field: florui_style::FocusPath::of(self.runtime.geometry().0, node),
                direction,
                next_step: std::time::Instant::now() + HOLD_REPEAT_DELAY,
            });
            return;
        }
        if let Some(node) = hit
            && !self.is_disabled(node)
            && self.is_editable_text_input(node)
        {
            self.handle_text_input_press(node, x, y);
            return;
        }
        if let Some(node) = hit
            && !self.is_disabled(node)
            && {
                let (arena, ..) = self.runtime.geometry();
                arena.tag(node) == "input"
                    && crate::focus::is_range_input_type(arena.input_type(node))
            }
        {
            // Matches Chrome: mousedown alone already jumps the value and
            // focuses the control, before any drag movement.
            self.runtime.set_focused(Some(node), false);
            self.runtime.start_range_drag(node, x);
            self.update_and_request_redraw();
            return;
        }
        // A disabled button must not become `pressed`: `handle_release`
        // sets focus purely from `pressed` being `Some`, before it ever
        // calls `dispatch_click` -- excluding it here is what stops a
        // mouse click from focusing a disabled button, which gating
        // `dispatch_click` alone can't (that only stops the click's own
        // handler from firing).
        self.pressed = hit.filter(|&node| !self.is_disabled(node));
    }

    /// A real right-button press on [`crate::WINDOW_DRAG_REGION_ID`] opens
    /// the real OS system menu — the same gesture a native title bar
    /// supports, only meaningful for [`DecorationMode::Custom`] (a
    /// `System`-decorated window's own OS title bar already has this for
    /// free).
    fn handle_right_press(&mut self) {
        if self.decorations != DecorationMode::Custom {
            return;
        }
        let (x, y) = self.to_logical_cursor(self.last_cursor.0, self.last_cursor.1);
        let hit = self.runtime.hit_test(x, y);
        if hit.is_some_and(|node| self.is_drag_region(node)) {
            self.controls.show_system_menu_at_cursor();
        }
    }

    /// `1`/`-1` when a press at `(x, y)` lands on the up/down arrow of the
    /// spinner of the editable number field at `node`, else `None`.
    fn spinner_press_direction(&self, node: NodeId, x: f32, y: f32) -> Option<i32> {
        let (arena, _, layouts) = self.runtime.geometry();
        if arena.tag(node) != "input"
            || arena.input_type(node) != Some("number")
            || arena.attr_flag(node, "readonly")
        {
            return None;
        }
        let layout = layouts.get(&node)?;
        let (bx, by) = florui_layout::absolute_position(arena, layouts, node);
        match florui_paint::spinner_half_at((bx, by, layout.width, layout.height), (x, y)) {
            florui_paint::SpinnerHover::Up => Some(1),
            florui_paint::SpinnerHover::Down => Some(-1),
            florui_paint::SpinnerHover::None => None,
        }
    }

    fn is_editable_text_input(&self, node: NodeId) -> bool {
        let (arena, ..) = self.runtime.geometry();
        crate::focus::is_text_control(arena, node)
    }

    /// A real press on an editable, enabled `<input>`: focuses it (like
    /// real HTML, `:focus-visible` false — a mouse-driven focus, not a
    /// keyboard one) and positions the caret at the press point, or
    /// selects the word under it if this press landed on the same input
    /// within [`DOUBLE_CLICK_INTERVAL`] of the last one. Starts a
    /// same-input drag-select, extended by [`Self::handle_cursor_moved`]
    /// and ended by [`Self::handle_release`].
    fn handle_text_input_press(&mut self, node: NodeId, x: f32, y: f32) {
        let now = std::time::Instant::now();
        let clicks = match self.last_text_input_click {
            Some((last_node, at, count))
                if last_node == node && now - at < DOUBLE_CLICK_INTERVAL =>
            {
                (count + 1).min(3)
            }
            _ => 1,
        };
        self.last_text_input_click = Some((node, now, clicks));

        let previous = self.focused_text_input();
        self.runtime.set_focused(Some(node), false);
        if let Some(previous) = previous
            && previous != node
        {
            self.clear_compose_for(previous);
            self.reset_ime_context();
        }
        self.window.set_ime_allowed(self.allows_ime(node));
        let Some(id) = ({
            let (arena, ..) = self.runtime.geometry();
            arena.id_attr(node).map(str::to_owned)
        }) else {
            self.update_and_request_redraw();
            return;
        };
        let registry = self.runtime.text_input_registry();
        let (local_x, local_y) = self.text_input_local_point(node, &id, x, y);
        let (_, _, _, font) = self.runtime.geometry_and_font_mut();
        let op = match clicks {
            1 => TextEditOp::MoveToPoint(local_x, local_y),
            2 => TextEditOp::SelectWordAtPoint(local_x, local_y),
            _ => TextEditOp::SelectHardLineAtPoint(local_x, local_y),
        };
        // A pure caret/selection move never changes the text itself, so
        // there is nothing to commit back through a `Binding`/
        // `ValueHandler` here -- only the redraw this input's own new
        // caret position needs.
        registry.apply(&id, op, font);
        self.text_selecting = Some(node);
        self.update_and_request_redraw();
    }

    /// The content-box origin (post border/padding) of an editable
    /// `<input>`, in the same logical-pixel space [`Self::to_logical_cursor`]
    /// already converts a real cursor position into — the space every
    /// point-based [`florui_text::editing::TextEditOp`] expects its `x`
    /// in. Mirrors `florui_paint`'s own `content_x`/`content_y`
    /// computation exactly, so a click lands on the same glyph it visibly
    /// painted over.
    fn text_input_content_origin(&self, node: NodeId) -> (f32, f32) {
        let (arena, styles, layouts) = self.runtime.geometry();
        text_input_content_origin(arena, styles, layouts, node)
    }

    /// The pointer position `(x, y)` in the editor's own space: relative to
    /// the content box, shifted by how far the field is scrolled.
    fn text_input_local_point(&mut self, node: NodeId, id: &str, x: f32, y: f32) -> (f32, f32) {
        let (origin_x, origin_y) = self.text_input_content_origin(node);
        let registry = self.runtime.text_input_registry();
        let (scroll_x, scroll_y) = registry.scroll_offset(id);
        let (_, _, _, font) = self.runtime.geometry_and_font_mut();
        // A password's painted bullets are spaced differently from its real
        // text, so a click is mapped back onto the real glyph positions.
        let local_x = registry.unmask_x(id, x - origin_x + scroll_x, font);
        (local_x, y - origin_y + scroll_y)
    }

    fn is_drag_region(&self, node: NodeId) -> bool {
        let (arena, ..) = self.runtime.geometry();
        arena.id_attr(node) == Some(crate::WINDOW_DRAG_REGION_ID)
    }

    /// Tag-gated the same as `florui_platform::focus::is_focusable` and
    /// `UiRuntime::dispatch_click`: `disabled` has no wired behavior
    /// outside `<button>`/editable-or-checkable `<input>`. Used to keep a
    /// disabled control out of `pressed` (see `handle_press`) and out of
    /// `:hover` (see `handle_cursor_moved`) -- real browsers don't
    /// deliver pointer events to a disabled control either.
    fn is_disabled(&self, node: NodeId) -> bool {
        let (arena, ..) = self.runtime.geometry();
        if !arena.is_disabled(node) {
            return false;
        }
        match arena.tag(node) {
            "button" | "select" | "textarea" => true,
            "input" => {
                crate::focus::is_editable_input_type(arena.input_type(node))
                    || crate::focus::is_checkable_input_type(arena.input_type(node))
                    || crate::focus::is_range_input_type(arena.input_type(node))
            }
            _ => false,
        }
    }

    fn should_close(&self) -> bool {
        self.controls.confirm_close()
    }

    /// Re-reads and re-parses the watched CSS file (see
    /// [`WindowSpec::with_css_reload`]), swaps it into the running
    /// [`UiRuntime`] via [`UiRuntime::set_rules`] — never rebuilding the
    /// tree, so every `Signal` keeps its value — and repaints. A failure
    /// (bad syntax, a save-in-progress truncated read) is reported and the
    /// last good stylesheet keeps rendering, the same recovery contract
    /// the native inspector's own fixture preview already established.
    fn reload_css(&mut self) {
        let Some(path) = self.css_path.clone() else {
            return;
        };
        let loaded = std::fs::read_to_string(&path)
            .map_err(RunError::CssFile)
            .and_then(|css| florui_style::parse_stylesheet(&css).map_err(RunError::Stylesheet));
        match loaded {
            Ok(rules) => {
                self.runtime.set_rules(rules);
                println!(
                    "florui-platform: stylesheet reloaded from {}",
                    path.display()
                );
                self.update_and_request_redraw();
            }
            Err(error) => {
                eprintln!(
                    "florui-platform: stylesheet reload failed, keeping last good version: {error}"
                );
            }
        }
    }

    /// One arrow step of the number field at `node`, or just a redraw at its
    /// limit.
    fn step_spinner(&mut self, node: NodeId, direction: i32) {
        match self.runtime.step_number_value(node, direction) {
            Some(next) => self.commit_text_input_value(node, next),
            None => self.update_and_request_redraw(),
        }
    }

    /// Repeats a held spinner arrow once its next step is due, and returns
    /// when the one after that is, or `None` when nothing is held. A held
    /// field that disappeared or was disabled ends the hold.
    fn repeat_spinner_if_due(&mut self, now: std::time::Instant) -> Option<std::time::Instant> {
        let hold = self.spinner_hold.as_ref()?;
        if now < hold.next_step {
            return Some(hold.next_step);
        }
        let (field, direction) = (hold.field.clone(), hold.direction);
        let node = {
            let (arena, ..) = self.runtime.geometry();
            let candidates = arena.find_all(|_, _| true);
            field.resolve(arena, &candidates)
        };
        let Some(node) = node.filter(|&node| !self.is_disabled(node)) else {
            self.spinner_hold = None;
            return None;
        };
        self.step_spinner(node, direction);
        let next = now + HOLD_REPEAT_INTERVAL;
        if let Some(hold) = self.spinner_hold.as_mut() {
            hold.next_step = next;
        }
        Some(next)
    }

    fn handle_release(&mut self) {
        self.text_selecting = None;
        self.scroll_drag = None;
        self.resize_drag = None;
        self.spinner_hold = None;
        self.scroll_hold = None;
        if self.runtime.is_range_dragging() {
            self.runtime.end_range_drag();
            self.update_and_request_redraw();
            return;
        }
        let (x, y) = self.to_logical_cursor(self.last_cursor.0, self.last_cursor.1);
        let pressed = self.pressed.take();
        let released_over = self.runtime.hit_test(x, y);
        if let (Some(pressed), Some(released_over)) = (pressed, released_over)
            && pressed == released_over
        {
            // Picking an option also closes its select -- unlike a plain
            // popover, where a click inside content never dismisses.
            // Skips the focus-to-target step below too: an `<option>` is
            // never itself focusable (see `focus::is_focusable`), and
            // focus should stay on the select, not fall off the document
            // when the option it landed on disappears from the tree next
            // render.
            let is_option = {
                let (arena, ..) = self.runtime.geometry();
                arena.tag(pressed) == "option"
            };
            if is_option {
                let in_multiple = {
                    let (arena, ..) = self.runtime.geometry();
                    crate::select::owning_select(arena, pressed)
                        .is_some_and(|select| arena.is_multiple(select))
                };
                if in_multiple {
                    // A multi-select has no dismiss concept at all -- it's
                    // always visible (see `crate::select`'s own doc), so
                    // this only ever computes and reports the new
                    // selection, never closes anything.
                    let (ctrl, shift) = (self.modifiers.control_key(), self.modifiers.shift_key());
                    self.runtime.commit_multiselect_click(pressed, ctrl, shift);
                } else {
                    self.runtime.dispatch_click(pressed);
                    self.dismiss_select_root_of(pressed);
                }
                self.update_and_request_redraw();
                return;
            }
            // Matches real HTML: a click sets keyboard focus to its
            // target too, just not :focus-visible (via_keyboard: false).
            let Some(target) = self.runtime.activation_target(pressed) else {
                return;
            };
            let previous = self.focused_text_input();
            let focus_changed = self.runtime.set_focused(Some(target), false);
            self.activate(target);
            if focus_changed {
                // `pressed` is never itself an editable text input here
                // (`handle_press` routes those through
                // `handle_text_input_press` instead) -- so focus is
                // always leaving text-input territory when it lands here.
                if let Some(previous) = previous {
                    self.clear_compose_for(previous);
                }
                self.window.set_ime_allowed(false);
                self.update_and_request_redraw();
            }
        }
    }

    /// Dispatches `node`'s click, then -- unless the handler called
    /// `Event::prevent_default()` -- runs its default action, if it has
    /// one. Only `<a href="http(s)://...">` has one today; the branch is
    /// a no-op for every other tag, so every activation path (mouse,
    /// keyboard, an accessibility action) can route through here
    /// uniformly rather than each needing its own opt-in.
    fn activate(&mut self, node: NodeId) {
        self.activate_with(node, false);
    }

    /// [`Self::activate`], with whether the activation came from the
    /// keyboard (a form's first invalid control takes `:focus-visible`
    /// only then).
    fn activate_with(&mut self, node: NodeId, via_keyboard: bool) {
        if self.runtime.dispatch_click(node) {
            return;
        }
        if self.runtime.activate_form_button(node, via_keyboard) {
            self.update_and_request_redraw();
            return;
        }
        // Collected as an owned `String` before the open-URL call below,
        // rather than kept as a `&str` borrowed from `self.runtime`'s own
        // arena: `refresh_visited_links` needs `&mut self.runtime` right
        // after, which a live borrow from `geometry()` would still be
        // blocking otherwise.
        let href = {
            let (arena, ..) = self.runtime.geometry();
            (arena.tag(node) == "a")
                .then(|| arena.href(node))
                .flatten()
                .filter(|href| crate::href::is_openable(href))
                .map(str::to_owned)
        };
        let Some(href) = href else {
            return;
        };
        if self.controls.open_url(&href) == crate::OpenUrlOutcome::Opened {
            // The link just became `:visited` -- refresh the styling that
            // depends on it and redraw, the same two-step every other
            // interaction-state change here already follows (see
            // `set_focused`'s own callers).
            self.runtime.refresh_visited_links();
            self.update_and_request_redraw();
        }
    }

    /// The real handler for `WindowEvent::KeyboardInput`. Only a fresh
    /// key-down does anything: a held key's own repeat must not re-fire
    /// activation, and a key-up carries no action of its own here. Tab/
    /// Shift+Tab move focus; Enter/Space activate whatever is currently
    /// focused through the same [`UiRuntime::dispatch_click`] a real
    /// mouse click already uses — no second event name invented.
    ///
    /// A focused editable `<input>` intercepts every key but Tab (which
    /// must still move focus away, matching real HTML) — see
    /// [`Self::handle_text_input_key`]. Real HTML's own Enter/Space
    /// activation behavior for a text input (submitting a form, none of
    /// which exists here) does not apply, so those two fall to the
    /// text-input handler too rather than the click-dispatch branch
    /// below. A focused range input's own step keys get the same
    /// repeat-surviving carve-out, measured against Chrome.
    fn handle_keyboard_input(&mut self, event: KeyEvent, clipboard: &crate::clipboard::Clipboard) {
        if event.state != ElementState::Pressed {
            if matches!(event.logical_key, Key::Named(NamedKey::Space)) {
                self.release_space();
            }
            return;
        }
        // A held key's own OS auto-repeat must reach text editing (real
        // held-Backspace/arrow-key repeat, matching every other real text
        // input) but must not re-fire Tab/Enter/Space/Escape's own
        // activation -- gated below, only for that branch, not up here
        // where it would also suppress text-input repeat.
        let is_tab_or_escape = matches!(
            event.logical_key,
            Key::Named(NamedKey::Tab) | Key::Named(NamedKey::Escape)
        );
        if !is_tab_or_escape && let Some(node) = self.focused_text_input() {
            self.handle_text_input_key(node, &event, clipboard);
            return;
        }
        // A focused range input's own step keys must also survive a held
        // key's OS auto-repeat -- measured against Chrome, which keeps
        // stepping for as long as the key stays down, not just once per
        // press. Checked here, before the repeat gate below, the same way
        // text-input editing already is.
        if let Key::Named(
            key @ (NamedKey::ArrowUp
            | NamedKey::ArrowLeft
            | NamedKey::ArrowDown
            | NamedKey::ArrowRight
            | NamedKey::PageUp
            | NamedKey::PageDown
            | NamedKey::Home
            | NamedKey::End),
        ) = event.logical_key
        {
            let range_focused = self.runtime.focused().filter(|&node| {
                let (arena, ..) = self.runtime.geometry();
                crate::focus::is_range_input_type(arena.input_type(node))
            });
            if let Some(node) = range_focused {
                let step = match key {
                    // Matches Chrome: Right/Up increments, Left/Down decrements.
                    NamedKey::ArrowUp | NamedKey::ArrowRight => {
                        crate::runtime::RangeStep::SmallIncrement
                    }
                    NamedKey::ArrowLeft | NamedKey::ArrowDown => {
                        crate::runtime::RangeStep::SmallDecrement
                    }
                    NamedKey::PageUp => crate::runtime::RangeStep::LargeIncrement,
                    NamedKey::PageDown => crate::runtime::RangeStep::LargeDecrement,
                    NamedKey::Home => crate::runtime::RangeStep::Min,
                    NamedKey::End => crate::runtime::RangeStep::Max,
                    _ => unreachable!(),
                };
                if self.runtime.step_range_value(node, step).is_some() {
                    self.update_and_request_redraw();
                }
                return;
            }
        }
        // Menu navigation repeats with a held key, like a native menu.
        if let Key::Named(key) = event.logical_key {
            let menu_key = match key {
                NamedKey::ArrowDown => Some(MenuKey::Next),
                NamedKey::ArrowUp => Some(MenuKey::Previous),
                NamedKey::Home => Some(MenuKey::First),
                NamedKey::End => Some(MenuKey::Last),
                NamedKey::ArrowRight => Some(MenuKey::Open),
                NamedKey::ArrowLeft => Some(MenuKey::Close),
                _ => None,
            };
            if let Some(menu_key) = menu_key
                && self.handle_menu_key(menu_key)
            {
                return;
            }
        }
        // A virtualized list moves between rows, repeating with a held key. A
        // select or a radio keeps the arrows for itself.
        if let Key::Named(key) = event.logical_key {
            let list_key = match key {
                NamedKey::ArrowDown => Some(ListKey::Next),
                NamedKey::ArrowUp => Some(ListKey::Previous),
                NamedKey::PageDown => Some(ListKey::PageDown),
                NamedKey::PageUp => Some(ListKey::PageUp),
                NamedKey::Home => Some(ListKey::First),
                NamedKey::End => Some(ListKey::Last),
                _ => None,
            };
            let owns_keys = self.runtime.focused().is_some_and(|node| {
                let (arena, ..) = self.runtime.geometry();
                arena.tag(node) == "select" || crate::focus::is_radio(arena, node)
            });
            if let Some(list_key) = list_key
                && !owns_keys
                && self.runtime.list_key(list_key)
            {
                self.update_and_request_redraw();
                return;
            }
        }
        if event.repeat {
            return;
        }
        match event.logical_key {
            Key::Named(NamedKey::Tab) => {
                self.close_menus_for_tab();
                let previous = self.focused_text_input();
                let moved = if self.modifiers.shift_key() {
                    self.runtime.focus_previous()
                } else {
                    self.runtime.focus_next()
                };
                if moved {
                    let now_focused = self.focused_text_input();
                    if let Some(previous) = previous
                        && Some(previous) != now_focused
                    {
                        self.clear_compose_for(previous);
                        // Real reset, not just this crate's own buffer --
                        // see `Self::reset_ime_context`'s own doc. Needed
                        // even when moving to a *different* text input,
                        // since `set_ime_allowed(true)` right after is a
                        // real no-op if it was already `true`.
                        self.reset_ime_context();
                    }
                    self.window
                        .set_ime_allowed(now_focused.is_some_and(|node| self.allows_ime(node)));
                    self.select_all_on_tab_focus(now_focused);
                    self.update_and_request_redraw();
                }
            }
            // Real Alt+Space, before the plain Space arm below claims it as
            // a click instead -- only meaningful for `DecorationMode::Custom`,
            // same as the real right-click gesture in `handle_right_press`.
            Key::Named(NamedKey::Space)
                if self.modifiers.alt_key() && self.decorations == DecorationMode::Custom =>
            {
                if let Ok(origin) = self.window.outer_position() {
                    let scale = self.viewport_scale().scale_factor;
                    let offset_x = (8.0 * scale).round() as i32;
                    let offset_y = (30.0 * scale).round() as i32;
                    self.controls
                        .show_system_menu_at(origin.x + offset_x, origin.y + offset_y);
                }
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(focused) = self.runtime.focused() {
                    let is_open_select = {
                        let (arena, ..) = self.runtime.geometry();
                        arena.tag(focused) == "select" && arena.is_open(focused)
                    };
                    if is_open_select {
                        self.commit_active_select_option();
                    } else {
                        let (arena, ..) = self.runtime.geometry();
                        if crate::focus::activates_on_enter(arena, focused) {
                            self.activate_with(focused, true);
                        }
                    }
                }
            }
            // Fires on release, see `release_space`. Never arms on a
            // focused `<a>` -- real anchors ignore Space entirely.
            Key::Named(NamedKey::Space) => {
                if let Some(focused) = self.runtime.focused() {
                    let (arena, ..) = self.runtime.geometry();
                    if crate::focus::activates_on_space(arena, focused) {
                        self.space_armed = Some(florui_style::FocusPath::of(arena, focused));
                    }
                }
            }
            // Selection follows focus within a radio group, so moving
            // focus also activates the new radio. A focused range input
            // reacts to the same four keys its own, different way (a
            // step, not a focus move) -- already handled above, before
            // the repeat gate, so this arm only ever sees a radio group
            // (or nothing focused) by the time it runs.
            Key::Named(
                key @ (NamedKey::ArrowUp
                | NamedKey::ArrowLeft
                | NamedKey::ArrowDown
                | NamedKey::ArrowRight),
            ) => {
                let direction = if matches!(key, NamedKey::ArrowUp | NamedKey::ArrowLeft) {
                    -1
                } else {
                    1
                };
                if let Some(radio) = self.runtime.step_radio_group(direction) {
                    self.activate(radio);
                    self.update_and_request_redraw();
                } else if matches!(key, NamedKey::ArrowUp | NamedKey::ArrowDown) {
                    self.step_select(direction);
                }
            }
            // Only a modal `Dialog` wires this up at all -- resolved
            // structurally (its own reserved marker class), not from
            // whatever currently has focus, so it works even if nothing
            // inside the dialog does.
            Key::Named(NamedKey::Escape) => {
                let modal_root = {
                    let (arena, ..) = self.runtime.geometry();
                    crate::focus::modal_root(arena)
                };
                let popover_root = {
                    let (arena, ..) = self.runtime.geometry();
                    popover::dismissed_by_escape(arena)
                };
                // Both can be open at once; Escape closes only the later
                // (innermost) one.
                match (modal_root, popover_root) {
                    (Some(modal), Some(popover)) if popover > modal => {
                        self.runtime.dispatch_event(popover, "dismiss");
                    }
                    (Some(modal), _) => {
                        self.runtime.dispatch_event(modal, "close");
                    }
                    (None, Some(popover)) => {
                        self.runtime.dispatch_event(popover, "dismiss");
                    }
                    (None, None) => {}
                }
            }
            _ => {}
        }
    }

    /// A menu key with focus inside an open menu (see [`crate::menu_keys`]);
    /// `true` if it was consumed. Inside a menu the arrows never fall through
    /// to radio groups or selects.
    fn handle_menu_key(&mut self, key: MenuKey) -> bool {
        let Some(focused) = self.runtime.focused() else {
            return false;
        };
        let (in_menu, action) = {
            let (arena, ..) = self.runtime.geometry();
            (
                menu_keys::focus_in_menu(arena, focused),
                menu_keys::menu_move(arena, focused, key),
            )
        };
        match action {
            Some(MenuMove::Focus(node)) => {
                self.runtime.set_focused(Some(node), true);
                self.update_and_request_redraw();
            }
            Some(MenuMove::Activate(node)) => self.activate_with(node, true),
            Some(MenuMove::Dismiss(root)) => {
                self.runtime.dispatch_event(root, "dismiss");
            }
            None => {}
        }
        in_menu
    }

    /// Tab leaves a menu: every open menu closes, focus returns to the
    /// control that opened the outermost one, and Tab continues from there.
    fn close_menus_for_tab(&mut self) {
        let Some(focused) = self.runtime.focused() else {
            return;
        };
        let roots = {
            let (arena, ..) = self.runtime.geometry();
            if menu_keys::focus_in_menu(arena, focused) {
                menu_keys::menu_roots(arena)
            } else {
                Vec::new()
            }
        };
        if roots.is_empty() {
            return;
        }
        for root in roots.into_iter().rev() {
            self.runtime.dispatch_event(root, "dismiss");
        }
        self.update_and_request_redraw();
    }

    /// Dismisses whichever popover-shaped overlay root contains `node` —
    /// used for a click on an `<option>`, which is always inside a
    /// select's own synthesized root (see `crate::select`).
    fn dismiss_select_root_of(&mut self, node: NodeId) {
        let root = {
            let (arena, ..) = self.runtime.geometry();
            popover::popover_roots(arena)
                .into_iter()
                .find(|&root| popover::is_self_or_descendant(arena, root, node))
        };
        if let Some(root) = root {
            self.runtime.dispatch_event(root, "dismiss");
        }
    }

    /// Matches real `<select>`: an open one's Up/Down only moves the
    /// keyboard highlight (`"activate"`, not a commit — see
    /// [`crate::UiRuntime::step_select_option`]); a closed one's Up/Down
    /// commits the adjacent value immediately, without opening
    /// ([`crate::UiRuntime::step_closed_select`]).
    fn step_select(&mut self, direction: isize) {
        if let Some(option) = self.runtime.step_select_option(direction) {
            self.runtime.dispatch_event(option, "activate");
            self.update_and_request_redraw();
            return;
        }
        if self.runtime.step_closed_select(direction) {
            self.update_and_request_redraw();
        }
    }

    /// Commits whichever option is active (or, absent one, selected) in
    /// the focused open select, and dismisses it — Enter's own behavior
    /// while a select is open. Falls back to the select's own `onclick`
    /// (normally a plain open/close toggle) if no option resolves at all.
    fn commit_active_select_option(&mut self) {
        let Some(option) = self.runtime.active_or_selected_option() else {
            if let Some(focused) = self.runtime.focused() {
                self.runtime.dispatch_click(focused);
                self.update_and_request_redraw();
            }
            return;
        };
        self.runtime.dispatch_click(option);
        self.dismiss_select_root_of(option);
        self.update_and_request_redraw();
    }

    /// Space activates on key release, like a native checkbox or button:
    /// the click fires only if focus is still on the node it went down on.
    fn release_space(&mut self) {
        let Some(armed) = self.space_armed.take() else {
            return;
        };
        let Some(focused) = self.runtime.focused() else {
            return;
        };
        let (arena, ..) = self.runtime.geometry();
        if florui_style::FocusPath::of(arena, focused) == armed {
            self.activate_with(focused, true);
        }
    }

    /// The currently keyboard-focused node, if it's an editable `<input>`
    /// — a focused checkbox or radio (also focusable, but not
    /// editable text) correctly falls through to `None` here, same as
    /// a focused `<button>`.
    fn focused_text_input(&self) -> Option<NodeId> {
        let node = self.runtime.focused()?;
        let (arena, ..) = self.runtime.geometry();
        crate::focus::is_text_control(arena, node).then_some(node)
    }

    /// Whether IME composition should ever be allowed for `node` --
    /// never for `type="password"`. The OS's own candidate window shows
    /// composing text in the clear regardless of this crate's own
    /// masking (a real, unavoidable leak in the platform's IME
    /// architecture, not something an app can suppress) -- disabling IME
    /// there entirely, so a password field only ever takes direct
    /// keystrokes, is the same real-world choice many existing
    /// applications already make for this exact reason.
    fn allows_ime(&self, node: NodeId) -> bool {
        let (arena, ..) = self.runtime.geometry();
        arena.input_type(node) != Some("password")
    }

    /// Defensive: clears any live IME composition on `node`. winit never
    /// signals `Ime::Disabled` when focus moves directly between two
    /// text inputs, or from one to a button (both keep this window's
    /// `set_ime_allowed` unchanged or update it independently) -- nothing
    /// else tells the app "the input that just lost focus needs its
    /// preedit cleared." A real no-op if `node` wasn't composing, or has
    /// no `id` attribute (see [`crate::text_input::TextInputRegistry::clear_compose`]).
    fn clear_compose_for(&mut self, node: NodeId) {
        let Some(id) = ({
            let (arena, ..) = self.runtime.geometry();
            arena.id_attr(node).map(str::to_owned)
        }) else {
            return;
        };
        let registry = self.runtime.text_input_registry();
        let (_, _, _, font) = self.runtime.geometry_and_font_mut();
        registry.clear_compose(&id, font);
    }

    /// Forces the real OS-level IME session to end, for whichever input
    /// it was still composing against. Real Windows IME composition is
    /// scoped to the whole window (one input context per `hwnd`), not to
    /// any one `<input>` node -- so moving focus directly from a
    /// composing text input to another one leaves the OS's own
    /// candidate/preedit state alive and still targeting *this* window,
    /// misdirected into whichever input is now focused once the next
    /// `WM_IME_COMPOSITION` arrives. `set_ime_allowed(true)` again would
    /// be a real no-op here (IME was already allowed) -- toggling off
    /// then on re-associates the window's input context (see winit's own
    /// `ImeContext::set_ime_allowed`, which calls `ImmAssociateContextEx`
    /// either way), the actual mechanism that discards a stale
    /// composition. [`Self::clear_compose_for`] alone only clears this
    /// crate's own buffer state; it can't reach into the OS's IME session
    /// at all.
    fn reset_ime_context(&mut self) {
        self.window.set_ime_allowed(false);
    }

    /// Maps one real key-down on a focused editable `<input>` to a
    /// [`florui_text::editing::TextEditOp`] (or an undo/redo/clipboard
    /// action), applies it through [`UiRuntime::text_input_registry`],
    /// and commits an accepted text change back through whichever of the
    /// node's own `Binding`/`ValueHandler` it carries (exactly one of
    /// those two is ever present, never both). Silently does nothing for
    /// a key that isn't mapped to a text-editing action (arrows/Home/End/Backspace/Delete/typed
    /// characters, their Ctrl/Shift variants, Ctrl+A, Ctrl+Z/Shift+Z/Y,
    /// Ctrl+C/X/V) or for a node missing its own `id` attribute (see
    /// `crate::text_input`'s own module doc).
    fn handle_text_input_key(
        &mut self,
        node: NodeId,
        event: &KeyEvent,
        clipboard: &crate::clipboard::Clipboard,
    ) {
        let Some(id) = ({
            let (arena, ..) = self.runtime.geometry();
            arena.id_attr(node).map(str::to_owned)
        }) else {
            return;
        };
        let registry = self.runtime.text_input_registry();
        let ctrl = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();

        let multiline = self.runtime.geometry().0.tag(node) == "textarea";
        if matches!(event.logical_key, Key::Named(NamedKey::Enter)) {
            if multiline {
                self.commit_text_input_op(
                    &registry,
                    &id,
                    node,
                    TextEditOp::InsertOrReplace("\n".to_string()),
                );
            } else if !event.repeat {
                self.submit_implicitly(node);
            }
            return;
        }
        if multiline
            && !ctrl
            && let Key::Named(key @ (NamedKey::PageUp | NamedKey::PageDown)) = event.logical_key
        {
            let direction = if matches!(key, NamedKey::PageUp) {
                -1
            } else {
                1
            };
            let (_, _, _, font) = self.runtime.geometry_and_font_mut();
            match registry.page(&id, direction, shift, font) {
                Some(new_text) => self.commit_text_input_value(node, new_text),
                None => self.update_and_request_redraw(),
            }
            return;
        }

        // Clipboard/undo shortcuts key off the *physical* key -- the
        // logical one can vary with layout/locale even while held with
        // Ctrl, where a mnemonic like "the C key" is what every real app
        // actually means.
        if ctrl {
            let physical = match event.physical_key {
                PhysicalKey::Code(code) => Some(code),
                PhysicalKey::Unidentified(_) => None,
            };
            match physical {
                Some(KeyCode::KeyA) => {
                    self.commit_text_input_op(&registry, &id, node, TextEditOp::SelectAll);
                    return;
                }
                Some(KeyCode::KeyC) => {
                    if let Some(selected) = registry.selected_text(&id) {
                        clipboard.set_text(selected);
                    }
                    return;
                }
                Some(KeyCode::KeyX) => {
                    if let Some(selected) = registry.selected_text(&id) {
                        clipboard.set_text(selected);
                        self.commit_text_input_op(
                            &registry,
                            &id,
                            node,
                            TextEditOp::InsertOrReplace(String::new()),
                        );
                    }
                    return;
                }
                Some(KeyCode::KeyV) => {
                    if let Some(pasted) = clipboard.get_text() {
                        let pasted = self.runtime.clean_typed(node, &pasted);
                        self.commit_text_input_op(
                            &registry,
                            &id,
                            node,
                            TextEditOp::InsertOrReplace(pasted),
                        );
                    }
                    return;
                }
                Some(KeyCode::KeyZ) if shift => {
                    self.commit_text_input_undo_redo(&registry, &id, node, false);
                    return;
                }
                Some(KeyCode::KeyZ) => {
                    self.commit_text_input_undo_redo(&registry, &id, node, true);
                    return;
                }
                Some(KeyCode::KeyY) => {
                    self.commit_text_input_undo_redo(&registry, &id, node, false);
                    return;
                }
                _ => {}
            }
        }

        let op = match &event.logical_key {
            Key::Named(NamedKey::ArrowLeft) => Some(match (ctrl, shift) {
                (true, true) => TextEditOp::SelectWordLeft,
                (true, false) => TextEditOp::MoveWordLeft,
                (false, true) => TextEditOp::SelectLeft,
                (false, false) => TextEditOp::MoveLeft,
            }),
            Key::Named(NamedKey::ArrowRight) => Some(match (ctrl, shift) {
                (true, true) => TextEditOp::SelectWordRight,
                (true, false) => TextEditOp::MoveWordRight,
                (false, true) => TextEditOp::SelectRight,
                (false, false) => TextEditOp::MoveRight,
            }),
            Key::Named(NamedKey::Home) => Some(match (ctrl, shift) {
                (true, true) => TextEditOp::SelectTextStart,
                (true, false) => TextEditOp::MoveTextStart,
                (false, true) => TextEditOp::SelectLineStart,
                (false, false) => TextEditOp::MoveLineStart,
            }),
            Key::Named(NamedKey::End) => Some(match (ctrl, shift) {
                (true, true) => TextEditOp::SelectTextEnd,
                (true, false) => TextEditOp::MoveTextEnd,
                (false, true) => TextEditOp::SelectLineEnd,
                (false, false) => TextEditOp::MoveLineEnd,
            }),
            Key::Named(NamedKey::Backspace) => Some(if ctrl {
                TextEditOp::BackdeleteWord
            } else {
                TextEditOp::Backdelete
            }),
            Key::Named(NamedKey::Delete) => Some(if ctrl {
                TextEditOp::DeleteWord
            } else {
                TextEditOp::Delete
            }),
            // A real character the user typed, cleaned for this field's own
            // kind (see `UiRuntime::clean_typed`); Ctrl-held combinations
            // other than the shortcuts already handled above carry no
            // text-insertion meaning here.
            Key::Character(text) if !ctrl => {
                let text = self.runtime.clean_typed(node, text);
                (!text.is_empty()).then_some(TextEditOp::InsertOrReplace(text))
            }
            // Unlike every other printable character, winit reports the
            // spacebar as a *named* key, never `Key::Character(" ")` --
            // a deliberate deviation from the UI Events spec (see
            // `winit::keyboard::NamedKey::Space`'s own doc). Without this
            // arm it fell through to the `_ => None` below and typing a
            // space did nothing.
            Key::Named(NamedKey::Space) if !ctrl => {
                Some(TextEditOp::InsertOrReplace(" ".to_string()))
            }
            // A textarea moves its caret between lines; Shift extends.
            Key::Named(key @ (NamedKey::ArrowUp | NamedKey::ArrowDown)) if multiline => {
                Some(match (matches!(key, NamedKey::ArrowUp), shift) {
                    (true, false) => TextEditOp::MoveUp,
                    (true, true) => TextEditOp::SelectUp,
                    (false, false) => TextEditOp::MoveDown,
                    (false, true) => TextEditOp::SelectDown,
                })
            }
            // A number field steps with Up/Down instead of moving a caret.
            Key::Named(key @ (NamedKey::ArrowUp | NamedKey::ArrowDown)) if !ctrl => {
                let direction = if matches!(key, NamedKey::ArrowUp) {
                    1
                } else {
                    -1
                };
                if let Some(next) = self.runtime.step_number_value(node, direction) {
                    self.commit_text_input_value(node, next);
                }
                None
            }
            _ => None,
        };
        if let Some(op) = op {
            self.commit_text_input_op(&registry, &id, node, op);
        }
    }

    /// Enter in a text field: click its form's default submit button, or
    /// submit directly when it is the form's only text field.
    fn submit_implicitly(&mut self, field: NodeId) {
        match self.runtime.implicit_submission(field) {
            Some(crate::runtime::ImplicitSubmit::Click(button)) => {
                self.activate_with(button, true);
            }
            Some(crate::runtime::ImplicitSubmit::Submit(form)) => {
                self.runtime.submit_form(form, None, true);
                self.update_and_request_redraw();
            }
            None => {}
        }
    }

    /// The real handler for `WindowEvent::Ime`. `Preedit`/`Commit` only
    /// ever arrive for the focused text input (winit only allows IME
    /// events at all once [`Self::handle_text_input_press`]/the `Tab`
    /// branch/`Self::handle_release` have called `set_ime_allowed` for
    /// it) — a missing focused input or `id` attribute is a real no-op,
    /// not an error. `Commit` reuses the exact same
    /// [`Self::commit_text_input_op`] a typed character already goes
    /// through: once committed, IME-composed text is exactly as real as
    /// anything typed directly.
    fn handle_ime_event(&mut self, ime: Ime) {
        let Some(node) = self.focused_text_input() else {
            return;
        };
        // Defense in depth: `type="password"` never asks the OS to
        // enable IME in the first place (see `Self::allows_ime`'s own
        // doc), but a stray event delivered anyway must still not reach
        // a password field's own buffer.
        if !self.allows_ime(node) {
            return;
        }
        let Some(id) = ({
            let (arena, ..) = self.runtime.geometry();
            arena.id_attr(node).map(str::to_owned)
        }) else {
            return;
        };
        let registry = self.runtime.text_input_registry();
        match ime {
            Ime::Preedit(text, cursor) => {
                let (_, _, _, font) = self.runtime.geometry_and_font_mut();
                registry.set_compose(&id, &text, cursor, font);
                self.update_and_request_redraw();
            }
            Ime::Commit(text) => {
                let text = self.runtime.clean_typed(node, &text);
                self.commit_text_input_op(&registry, &id, node, TextEditOp::InsertOrReplace(text));
            }
            // `Enabled` needs no action (this window already allowed IME
            // before the OS would ever send `Preedit`/`Commit`).
            // `Disabled` defensively clears a composition winit itself
            // didn't already clear via an empty `Preedit` first.
            Ime::Enabled | Ime::Disabled => {
                let (_, _, _, font) = self.runtime.geometry_and_font_mut();
                registry.clear_compose(&id, font);
                self.update_and_request_redraw();
            }
        }
    }

    /// Applies `op`, and if it actually changed the text, commits the
    /// result through the node's own `Binding`/`ValueHandler` and
    /// re-renders — the one real write-back path every editing/undo/redo
    /// key shares.
    /// Applies `op` and always redraws -- a pure movement/selection op
    /// (`MoveLeft`, `SelectAll`, ...) changes nothing a `Binding`/
    /// `ValueHandler` needs to hear about, but it still moves the caret or
    /// selection highlight, which is only ever visible once this window's
    /// own next frame actually paints it.
    fn commit_text_input_op(
        &mut self,
        registry: &crate::text_input::TextInputRegistry,
        id: &str,
        node: NodeId,
        op: TextEditOp,
    ) {
        let (_, _, _, font) = self.runtime.geometry_and_font_mut();
        match registry.apply(id, op, font) {
            Some(new_text) => self.commit_text_input_value(node, new_text),
            None => self.update_and_request_redraw(),
        }
    }

    fn commit_text_input_undo_redo(
        &mut self,
        registry: &crate::text_input::TextInputRegistry,
        id: &str,
        node: NodeId,
        is_undo: bool,
    ) {
        let (_, _, _, font) = self.runtime.geometry_and_font_mut();
        let result = if is_undo {
            registry.undo(id, font)
        } else {
            registry.redo(id, font)
        };
        match result {
            Some(new_text) => self.commit_text_input_value(node, new_text),
            None => self.update_and_request_redraw(),
        }
    }

    /// Reports `new_text` to whichever write-back channel `node`'s own
    /// `value` attribute actually carries, then re-renders — the owner's
    /// own next value (accepted, rejected, or something else entirely) is
    /// what the following [`crate::text_input::TextInputRegistry::sync`]
    /// reconciles against, not this value directly; see that module's own
    /// doc.
    fn commit_text_input_value(&mut self, node: NodeId, new_text: String) {
        self.runtime.commit_value(node, new_text);
        self.update_and_request_redraw();
    }
}

/// Owns every currently-open window and whichever [`WindowSpec`]s haven't
/// been created yet. `windows` is the single source of truth for "is a
/// window still open" — closing one removes its entry, and the process
/// exits once the map is empty (the simplest correct "last window closes
/// the app" default; a fully customizable close/focus/last-window
/// contract across many windows is separate, larger work this does not
/// attempt).
struct DesktopHost {
    windows: HashMap<WindowId, WindowState>,
    pending: Vec<WindowSpec>,
    proxy: EventLoopProxy<UserEvent>,
    fatal_error: Option<RunError>,
    /// `Some` only under [`run_single_instance`] — shared with the
    /// pipe-listener thread's `UserEvent::Activation` sends and with the
    /// primary window's own [`ActivationEvents`] context (see `resumed`).
    activation_queue: Option<Rc<RefCell<ActivationQueue>>>,
    /// The first window `resumed` ever creates — the sole window
    /// [`ActivationEvents`] context is provided to and the sole target
    /// `UserEvent::Activation` redraws. Multi-window fan-out is
    /// deliberately out of scope: a queue shared by every window would
    /// make `take_pending` a race over which window's render drains it
    /// first.
    primary_window_id: Option<WindowId>,
    /// One real OS clipboard for this whole process — see
    /// `crate::clipboard`'s own module doc for why this isn't per-window
    /// or reachable via `use_context`.
    clipboard: crate::clipboard::Clipboard,
    /// One visited-link record for this whole process, same reasoning as
    /// `clipboard` above — cloned (cheap: an `Rc` handle) into every
    /// window's own [`WindowControls`]/[`UiRuntime`] as it's created.
    visited_links: crate::visited_links::VisitedLinks,
}

impl DesktopHost {
    /// Stops the event loop after logging `error`, and keeps it so
    /// [`run_windows`] can return it once `run_app` unwinds — an
    /// `ApplicationHandler` method has no return value of its own to
    /// report failure through.
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: RunError) {
        eprintln!("florui-platform: {error}");
        self.fatal_error = Some(error);
        event_loop.exit();
    }

    /// Shared by the OS's own `CloseRequested` and
    /// [`UserEvent::RequestClose`] — one real close path for `id`, both
    /// askable to veto via that window's own
    /// [`WindowState::should_close`]. Removes just that window's entry;
    /// only exits the whole process once none are left.
    fn close_if_confirmed(&mut self, event_loop: &ActiveEventLoop, id: WindowId) {
        let Some(state) = self.windows.get(&id) else {
            return;
        };
        if !state.should_close() {
            return;
        }
        if let Some(persistence) = &state.persistence {
            window_state::capture_and_save(&state.window, persistence);
        }
        self.windows.remove(&id);
        if self.windows.is_empty() {
            event_loop.exit();
        }
    }
}

impl ApplicationHandler<UserEvent> for DesktopHost {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // `mem::take` (not `self.pending.drain(..)`) so the loop owns its
        // own `Vec` independently of `self` -- `self.fail` below needs a
        // fresh `&mut self`, which a live borrow from `drain` would
        // conflict with. A `spec` not yet reached when the function
        // returns early (a failure partway through the batch) is dropped
        // along with the rest of this now-local `Vec`'s `IntoIter`,
        // matching "the whole batch fails together."
        for spec in std::mem::take(&mut self.pending) {
            // Always requested — harmless for whichever presenter actually
            // ends up used (see `gpu::transparent_capable_attributes`'s own
            // doc), and `crate::gpu::GpuPresenter::try_new` needs the window
            // to have already been created with these attributes to have any
            // chance at real `TransparentSurface` compositing.
            let mut attrs = Window::default_attributes()
                .with_title(spec.title.clone())
                .with_decorations(matches!(spec.options.decorations, DecorationMode::System))
                .with_theme(crate::theme::requested_winit_theme(spec.options.theme));
            attrs = drag_drop::disable_builtin_drag_and_drop(attrs);
            if let Some((width, height)) = spec.options.size {
                attrs = attrs.with_inner_size(winit::dpi::LogicalSize::new(width, height));
            }
            if let Some((width, height)) = spec.options.min_size {
                attrs = attrs.with_min_inner_size(winit::dpi::LogicalSize::new(width, height));
            }
            // A malformed icon doesn't take the window down with it --
            // logged and skipped, matching the CSS-reload-failure
            // precedent ("report it, keep what already works") rather
            // than window/surface-creation's own fatal-batch precedent.
            if let Some(icon) = &spec.options.icon {
                match crate::window_controls::to_winit_icon(icon) {
                    Ok(winit_icon) => attrs = attrs.with_window_icon(Some(winit_icon)),
                    Err(error) => {
                        eprintln!("florui-platform: window icon could not be applied: {error}");
                    }
                }
            }
            // Restores persisted bounds, revalidated against currently
            // connected monitors -- a saved position that no longer
            // overlaps any of them is a full miss, not partially honored,
            // so a title/drag region is never left unreachable off-screen.
            // A saved size with no saved position (never recorded, e.g. no
            // `Moved` event ever fired) still applies -- losing position
            // alone is a smaller compromise than losing everything.
            let mut should_restore_maximized = false;
            if let Some(persistence) = &spec.options.persistence {
                let saved =
                    window_state::window_state_path(&persistence.app_identifier, &persistence.key)
                        .as_deref()
                        .and_then(window_state::load_or_default);
                if let Some(saved) = saved {
                    let monitors: Vec<window_state::MonitorRect> = event_loop
                        .available_monitors()
                        .map(|monitor| window_state::MonitorRect {
                            position: (monitor.position().x, monitor.position().y),
                            physical_size: (monitor.size().width, monitor.size().height),
                            scale_factor: monitor.scale_factor(),
                        })
                        .collect();
                    let restorable_position = saved.position.filter(|&position| {
                        window_state::overlapping_monitor(
                            saved.logical_size,
                            position,
                            saved.monitor.scale_factor,
                            &monitors,
                        )
                        .is_some()
                    });
                    if saved.position.is_none() || restorable_position.is_some() {
                        let clamped =
                            window_state::clamp_to_min(saved.logical_size, spec.options.min_size);
                        attrs = attrs
                            .with_inner_size(winit::dpi::LogicalSize::new(clamped.0, clamped.1));
                        if let Some(position) = restorable_position {
                            attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(
                                position.0, position.1,
                            ));
                        }
                        should_restore_maximized = saved.maximized;
                    }
                }
            }
            // Invisible until the real AccessKit adapter below exists --
            // `accesskit_winit::Adapter::with_event_loop_proxy` panics if
            // constructed after the window has already been shown once.
            let attrs = gpu::transparent_capable_attributes(attrs).with_visible(false);
            let window = match event_loop.create_window(attrs) {
                Ok(window) => Arc::new(window),
                Err(error) => return self.fail(event_loop, RunError::WindowCreation(error)),
            };
            let window_id = window.id();
            let accessibility_adapter = accesskit_winit::Adapter::with_event_loop_proxy(
                event_loop,
                &window,
                self.proxy.clone(),
            );
            window.set_visible(true);
            // The very first window this host ever creates, across its
            // whole lifetime -- `pending` is only ever drained once (see
            // this function's own doc), so this is unambiguous.
            let is_primary_window = self.primary_window_id.is_none();
            if is_primary_window {
                self.primary_window_id = Some(window_id);
            }
            if should_restore_maximized {
                window.set_maximized(true);
            }

            // GPU-preferred, `softbuffer` fallback — see `crate::gpu`'s own
            // doc for the two-tier (three-way, counting this CPU path)
            // capability contract this implements.
            let presenter = match GpuPresenter::try_new(window.clone()) {
                Some(gpu_presenter) => Presenter::Gpu(Box::new(gpu_presenter)),
                None => {
                    let context = match softbuffer::Context::new(window.clone()) {
                        Ok(context) => context,
                        Err(error) => {
                            return self.fail(event_loop, RunError::SurfaceCreation(error));
                        }
                    };
                    let surface = match softbuffer::Surface::new(&context, window.clone()) {
                        Ok(surface) => surface,
                        Err(error) => {
                            return self.fail(event_loop, RunError::SurfaceCreation(error));
                        }
                    };
                    Presenter::Cpu {
                        surface,
                        _context: context,
                    }
                }
            };

            let viewport = layout_viewport(dpi::viewport_scale(
                window.inner_size(),
                window.scale_factor(),
            ));

            // A real capability probe against this actual production
            // window, not just the standalone `appearance_probe` example —
            // see `appearance::probe_appearance`'s own doc for why it takes
            // a plain `&Window` and is safe to call here too.
            let appearance_report = appearance::probe_appearance(
                &window,
                appearance::AppearanceRequest {
                    decorations: spec.options.decorations,
                    transparent: spec.options.transparent,
                },
            );

            // Reachable from the component tree via `crate::use_window_controls`
            // from this runtime's very first render onward — see
            // `UiRuntime::with_rules_and_context`'s own doc for why that needs
            // to be a constructor argument rather than registered afterward.
            let controls_window = window.clone();
            let close_proxy = self.proxy.clone();
            let open_dialog_proxy = self.proxy.clone();
            let save_dialog_proxy = self.proxy.clone();
            let controls = Rc::new(WindowControls::new(
                controls_window,
                move || {
                    let _ = close_proxy.send_event(UserEvent::RequestClose(window_id));
                },
                move |outcome| {
                    let _ = open_dialog_proxy
                        .send_event(UserEvent::OpenFileDialogResult(window_id, outcome));
                },
                move |outcome| {
                    let _ = save_dialog_proxy
                        .send_event(UserEvent::SaveFileDialogResult(window_id, outcome));
                },
                appearance_report,
                self.visited_links.clone(),
            ));
            let drag_drop_registration = drag_drop::register(&window, Rc::clone(&controls));
            let mut context_providers: Vec<Box<dyn Fn()>> = {
                let controls = Rc::clone(&controls);
                vec![Box::new(move || {
                    provide_context(Rc::clone(&controls));
                })]
            };
            // Only the primary window ever gets this context -- see
            // `DesktopHost::primary_window_id`'s own doc for why.
            if is_primary_window && let Some(queue) = &self.activation_queue {
                let events = ActivationEvents(Rc::clone(queue));
                context_providers.push(Box::new(move || {
                    provide_context(events.clone());
                }));
            }

            let respect_reduced_motion = spec.options.respect_reduced_motion;
            let theme_preference = spec.options.theme;
            let initial_color_scheme =
                crate::theme::effective_color_scheme(theme_preference, &window);
            let mut runtime = UiRuntime::with_rules_and_context(
                spec.rules,
                spec.root,
                viewport,
                context_providers,
                respect_reduced_motion,
                crate::accessibility::prefers_reduced_motion(),
                initial_color_scheme.is_dark(),
            );
            runtime.set_visited_links(self.visited_links.clone());
            let proxy = self.proxy.clone();
            runtime.on_needs_update(move || {
                let _ = proxy.send_event(UserEvent::Dirty(window_id));
            });
            // The listener above can only be registered after the runtime (and
            // the first render its constructor already ran) exists — so an
            // initial mount effect that itself calls `Signal::set` marks the
            // flag with nothing listening yet, and that mark would otherwise
            // be lost: nothing else re-checks it before the first paint.
            if runtime.is_dirty() {
                runtime.clear_dirty();
                runtime.update(viewport);
            }

            let css_watcher = match &spec.css_path {
                Some(path) => match watch_css_file(path, self.proxy.clone(), window_id) {
                    Ok(watcher) => Some(watcher),
                    Err(error) => return self.fail(event_loop, RunError::CssWatch(error)),
                },
                None => None,
            };

            let mut state = WindowState {
                canvas_color: spec.canvas_color,
                runtime,
                pressed: None,
                space_armed: None,
                last_cursor: (0.0, 0.0),
                window,
                controls,
                presenter,
                next_animation_wake: None,
                spinner_hold: None,
                scroll_hold: None,
                scroll_drag: None,
                resize_drag: None,
                css_path: spec.css_path,
                _css_watcher: css_watcher,
                _drag_drop: drag_drop_registration,
                theme_preference,
                decorations: spec.options.decorations,
                persistence: spec.options.persistence.clone(),
                pending_geometry_save: None,
                modifiers: ModifiersState::empty(),
                text_selecting: None,
                last_text_input_click: None,
                last_drag_region_click: None,
                accessibility_adapter,
                accessibility_tree: accessibility::tree::AccessibilityTree::new(),
                accessibility_reverse: HashMap::new(),
            };
            state.redraw();
            // A `@keyframes` animation already running on mount (no `:hover`
            // or other interaction needed to start it) needs a deadline set
            // here — every other call site only does this after an `update()`
            // this constructor's own first render already ran.
            state.refresh_animation_schedule();
            self.windows.insert(window_id, state);
        }
    }

    /// Runs after every loop iteration, whatever triggered it (input, a
    /// `UserEvent`, a previous animation wake) — the one place this host
    /// decides the control flow for the *next* iteration. Redraws are
    /// requested per window independently (each may be animating on its
    /// own schedule); the next `WaitUntil` deadline is the *minimum* across
    /// every window still waiting on one, so no window's animation lags
    /// behind another's.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = std::time::Instant::now();
        let mut next_wake: Option<std::time::Instant> = None;
        for state in self.windows.values_mut() {
            match state.next_animation_wake {
                Some(deadline) if now >= deadline => state.window.request_redraw(),
                Some(deadline) => {
                    next_wake = Some(next_wake.map_or(deadline, |current| current.min(deadline)));
                }
                None => {}
            }
            for deadline in [
                state.repeat_spinner_if_due(now),
                state.repeat_scroll_if_due(now),
            ]
            .into_iter()
            .flatten()
            {
                next_wake = Some(next_wake.map_or(deadline, |current| current.min(deadline)));
            }
            state.flush_geometry_save_if_due(now);
            if let Some(changed_at) = state.pending_geometry_save {
                let save_deadline = changed_at + WINDOW_STATE_SAVE_DEBOUNCE;
                next_wake =
                    Some(next_wake.map_or(save_deadline, |current| current.min(save_deadline)));
            }
        }
        match next_wake {
            Some(deadline) => event_loop.set_control_flow(ControlFlow::WaitUntil(deadline)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        // Handled before looking up `windows` mutably -- closing calls
        // `self.close_if_confirmed`, a `&mut self` method, which a live
        // `&mut WindowState` borrow (below) would conflict with.
        if matches!(event, WindowEvent::CloseRequested) {
            self.close_if_confirmed(event_loop, window_id);
            return;
        }
        let Some(state) = self.windows.get_mut(&window_id) else {
            return;
        };
        state
            .accessibility_adapter
            .process_event(&state.window, &event);
        match event {
            WindowEvent::Resized(_) => {
                state.update_and_request_redraw();
                state.mark_geometry_dirty();
            }
            // Fires on its own — not bundled into `Resized` — when the
            // window moves to a display with a different scale factor, or
            // the OS scale setting changes live; `viewport_scale` re-reads
            // `window.scale_factor()` fresh every call, so re-rendering is
            // all this needs.
            WindowEvent::ScaleFactorChanged { .. } => state.update_and_request_redraw(),
            WindowEvent::ThemeChanged(theme) => state.handle_theme_changed(theme),
            WindowEvent::Focused(focused) => state.handle_focus_changed(focused),
            // A pure move (dragging the window, snapping it) changes
            // nothing about its content, only where `InputMode::Selective`'s
            // own screen-space regions sit -- resyncing them here, from
            // the already-computed layout, avoids paying for a full
            // re-render on every step of a drag.
            WindowEvent::Moved(_) => {
                state.resync_input_regions();
                state.mark_geometry_dirty();
            }
            WindowEvent::RedrawRequested => state.redraw_for_frame(),
            WindowEvent::CursorMoved { position, .. } => {
                state.handle_cursor_moved(position.x, position.y);
            }
            WindowEvent::CursorLeft { .. } => state.handle_cursor_left(),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => state.handle_press(),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => state.handle_release(),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => state.handle_right_press(),
            WindowEvent::MouseWheel { delta, .. } => state.handle_mouse_wheel(delta),
            WindowEvent::ModifiersChanged(modifiers) => state.modifiers = modifiers.state(),
            // `is_synthetic: true` is winit re-synthesizing "this key was
            // already held" on focus gain/loss — must not trigger
            // activation, only a genuine key-down the user just pressed.
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => state.handle_keyboard_input(event, &self.clipboard),
            WindowEvent::Ime(ime) => state.handle_ime_event(ime),
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Dirty(id) => {
                if let Some(state) = self.windows.get_mut(&id) {
                    state.update_and_request_redraw();
                }
            }
            UserEvent::CssChanged(id) => {
                if let Some(state) = self.windows.get_mut(&id) {
                    state.reload_css();
                }
            }
            UserEvent::RequestClose(id) => self.close_if_confirmed(event_loop, id),
            UserEvent::Activation(activation_event) => {
                let Some(queue) = &self.activation_queue else {
                    return;
                };
                queue.borrow_mut().push(activation_event);
                // If the primary window hasn't mounted yet, there's
                // nothing to redraw -- its own first `update()` (already
                // run inside `UiRuntime::with_rules_and_context`) will see
                // this queue's contents, since the same `Rc` is what its
                // context provider reads.
                if let Some(id) = self.primary_window_id
                    && let Some(state) = self.windows.get_mut(&id)
                {
                    state.update_and_request_redraw();
                }
            }
            UserEvent::OpenFileDialogResult(id, outcome) => {
                if let Some(state) = self.windows.get_mut(&id) {
                    state.controls.deliver_open_dialog_result(outcome);
                    state.update_and_request_redraw();
                }
            }
            UserEvent::SaveFileDialogResult(id, outcome) => {
                if let Some(state) = self.windows.get_mut(&id) {
                    state.controls.deliver_save_dialog_result(outcome);
                    state.update_and_request_redraw();
                }
            }
            UserEvent::Accessibility(event) => {
                let Some(state) = self.windows.get_mut(&event.window_id) else {
                    return;
                };
                match event.window_event {
                    // `with_event_loop_proxy`'s own doc: this constructor
                    // always returns `None` from `request_initial_tree`, so
                    // the real first tree is whatever the next `redraw`'s
                    // own `update_if_active` call sends -- the adapter is
                    // already active by the time that runs.
                    accesskit_winit::WindowEvent::InitialTreeRequested => {
                        state.update_and_request_redraw();
                    }
                    accesskit_winit::WindowEvent::ActionRequested(request) => {
                        let Some(node) = state
                            .accessibility_reverse
                            .get(&request.target_node)
                            .copied()
                        else {
                            return;
                        };
                        match request.action {
                            // Matches Tab's own semantics (`via_keyboard: true`).
                            accesskit::Action::Focus => {
                                if state.runtime.set_focused(Some(node), true) {
                                    state.update_and_request_redraw();
                                }
                            }
                            // Matches `handle_release`'s own click semantics
                            // exactly: focus first (`via_keyboard: false`),
                            // then dispatch -- a redraw here only covers the
                            // focus-indicator change, since the click
                            // handler's own `Signal` writes (if any) already
                            // redraw via `UserEvent::Dirty`.
                            accesskit::Action::Click => {
                                // An `<option>` is never focusable (see
                                // `handle_release`'s own matching case) --
                                // AT-driven activation must skip
                                // `set_focused` for the same reason.
                                let is_option = {
                                    let (arena, ..) = state.runtime.geometry();
                                    arena.tag(node) == "option"
                                };
                                if is_option {
                                    let in_multiple = {
                                        let (arena, ..) = state.runtime.geometry();
                                        crate::select::owning_select(arena, node)
                                            .is_some_and(|select| arena.is_multiple(select))
                                    };
                                    if in_multiple {
                                        // No modifier state travels with an
                                        // AT `Click` action -- a real
                                        // Ctrl/Shift-equivalent needs
                                        // SelectionItemPattern's own
                                        // Add/Remove actions, not attempted
                                        // here; this replaces the whole
                                        // selection, same as a plain click.
                                        state.runtime.commit_multiselect_click(node, false, false);
                                    } else {
                                        state.runtime.dispatch_click(node);
                                        state.dismiss_select_root_of(node);
                                    }
                                    state.update_and_request_redraw();
                                } else {
                                    let focus_changed =
                                        state.runtime.set_focused(Some(node), false);
                                    state.activate(node);
                                    if focus_changed {
                                        state.update_and_request_redraw();
                                    }
                                }
                            }
                            // A select's own `onclick` is an open/closed
                            // toggle (the same contract every example
                            // wires it with) -- gating each action to the
                            // direction it actually means keeps AT-driven
                            // Expand from silently closing an already-open
                            // select, and vice versa.
                            action @ (accesskit::Action::Expand | accesskit::Action::Collapse) => {
                                let should_dispatch = {
                                    let (arena, ..) = state.runtime.geometry();
                                    arena.is_open(node) == (action == accesskit::Action::Collapse)
                                };
                                if should_dispatch {
                                    state.runtime.dispatch_click(node);
                                    state.update_and_request_redraw();
                                }
                            }
                            // `ReplaceSelectedText` is `TextEditOp::InsertOrReplace`
                            // directly. `SetValue` means the whole value,
                            // not just the selection -- select all first
                            // so the same insert replaces everything.
                            action @ (accesskit::Action::SetValue
                            | accesskit::Action::ReplaceSelectedText) => {
                                let Some(accesskit::ActionData::Value(value)) = request.data else {
                                    return;
                                };
                                let Some(id) = ({
                                    let (arena, ..) = state.runtime.geometry();
                                    arena.id_attr(node).map(str::to_owned)
                                }) else {
                                    return;
                                };
                                let value = state.runtime.clean_typed(node, &value);
                                let registry = state.runtime.text_input_registry();
                                if action == accesskit::Action::SetValue {
                                    state.commit_text_input_op(
                                        &registry,
                                        &id,
                                        node,
                                        TextEditOp::SelectAll,
                                    );
                                }
                                state.commit_text_input_op(
                                    &registry,
                                    &id,
                                    node,
                                    TextEditOp::InsertOrReplace(value),
                                );
                            }
                            // A screen reader's own increment/decrement
                            // gesture on a slider steps it the same way an
                            // arrow key already does.
                            action @ (accesskit::Action::Increment
                            | accesskit::Action::Decrement) => {
                                let increment = action == accesskit::Action::Increment;
                                if let Some(next) = state
                                    .runtime
                                    .step_number_value(node, if increment { 1 } else { -1 })
                                {
                                    state.commit_text_input_value(node, next);
                                    return;
                                }
                                let step = if increment {
                                    crate::runtime::RangeStep::SmallIncrement
                                } else {
                                    crate::runtime::RangeStep::SmallDecrement
                                };
                                if state.runtime.step_range_value(node, step).is_some() {
                                    state.update_and_request_redraw();
                                }
                            }
                            _ => {}
                        }
                    }
                    accesskit_winit::WindowEvent::AccessibilityDeactivated => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_resize_corner_cursor_follows_the_directions_it_resizes() {
        use florui_style::Resize;
        use winit::window::CursorIcon;
        assert_eq!(resize_cursor(Resize::Both), CursorIcon::NwseResize);
        assert_eq!(resize_cursor(Resize::Vertical), CursorIcon::NsResize);
        assert_eq!(resize_cursor(Resize::Horizontal), CursorIcon::EwResize);
    }
    #[test]
    fn scrollbar_buttons_step_a_line_and_the_track_a_page() {
        use florui_paint::ScrollbarPart as Part;
        assert_eq!(
            scroll_step(Part::ButtonDown, 100.0),
            Some(WHEEL_LINE_HEIGHT)
        );
        assert_eq!(scroll_step(Part::ButtonUp, 100.0), Some(-WHEEL_LINE_HEIGHT));
        assert_eq!(scroll_step(Part::TrackAfter, 120.0), Some(105.0));
        assert_eq!(scroll_step(Part::TrackBefore, 120.0), Some(-105.0));
        assert_eq!(scroll_step(Part::Thumb, 120.0), None);
    }

    #[test]
    fn window_options_default_respects_reduced_motion() {
        assert!(
            WindowOptions::default().respect_reduced_motion,
            "the manual Default impl must not silently invert this field's required default, \
             the way adding it to a derive would have"
        );
    }

    #[test]
    fn layout_viewport_uses_the_logical_size_not_the_physical_one() {
        let scale = dpi::viewport_scale(PhysicalSize::new(1600, 1200), 2.0);
        let viewport = layout_viewport(scale);
        assert_eq!(viewport.width, AvailableSpace::Definite(800.0));
        assert_eq!(viewport.height, AvailableSpace::Definite(600.0));
    }

    #[test]
    fn masked_glyphs_spaces_dots_uniformly_regardless_of_character_width() {
        let mut font = florui_text::Font::load_embedded();
        let (narrow_runs, narrow_positions) = masked_glyphs(
            &mut font,
            13,
            florui_text::FontFamily::SansSerif,
            20.0,
            400.0,
        );
        let (wide_runs, wide_positions) = masked_glyphs(
            &mut font,
            13,
            florui_text::FontFamily::SansSerif,
            20.0,
            400.0,
        );
        // Same character *count* always produces the same positions --
        // real Chromium does this regardless of which real characters
        // were typed (verified: an all-"I" and an all-"W" password of
        // the same length render at the exact same width).
        assert_eq!(narrow_positions, wide_positions);
        assert_eq!(narrow_runs.len(), wide_runs.len());
        let painted: usize = narrow_runs.iter().map(|run| run.glyphs.len()).sum();
        assert_eq!(painted, 13, "the extra probe dot must not be painted");
        assert_eq!(
            narrow_positions.len(),
            14,
            "13 real positions plus one past-the-end"
        );
    }

    #[test]
    fn masked_glyphs_for_an_empty_password_paints_nothing() {
        let mut font = florui_text::Font::load_embedded();
        let (runs, positions) = masked_glyphs(
            &mut font,
            0,
            florui_text::FontFamily::SansSerif,
            20.0,
            400.0,
        );
        assert!(runs.iter().all(|run| run.glyphs.is_empty()));
        assert_eq!(positions.len(), 1, "just the caret-at-start position");
    }

    #[test]
    fn char_index_at_counts_real_glyphs_strictly_left_of_x() {
        let glyphs = [
            florui_text::ShapedGlyph {
                id: 0,
                x: 0.0,
                y: 0.0,
            },
            florui_text::ShapedGlyph {
                id: 0,
                x: 10.0,
                y: 0.0,
            },
            florui_text::ShapedGlyph {
                id: 0,
                x: 20.0,
                y: 0.0,
            },
        ];
        assert_eq!(char_index_at(&glyphs, 0.0), 0);
        assert_eq!(char_index_at(&glyphs, 10.0), 1);
        assert_eq!(char_index_at(&glyphs, 25.0), 3);
    }

    #[test]
    fn scale_layouts_at_1x_is_the_identity() {
        let mut layouts = HashMap::new();
        layouts.insert(
            0,
            BoxLayout {
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 50.0,
            },
        );
        let scaled = scale_layouts(&layouts, 1.0);
        assert_eq!(scaled[&0], layouts[&0]);
    }

    #[test]
    fn scale_layouts_at_2x_doubles_every_field() {
        let mut layouts = HashMap::new();
        layouts.insert(
            0,
            BoxLayout {
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 50.0,
            },
        );
        let scaled = scale_layouts(&layouts, 2.0);
        assert_eq!(
            scaled[&0],
            BoxLayout {
                x: 20.0,
                y: 40.0,
                width: 200.0,
                height: 100.0,
            }
        );
    }

    #[test]
    fn scale_layouts_preserves_every_node_id_and_only_those() {
        let mut layouts = HashMap::new();
        layouts.insert(
            1,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
        );
        layouts.insert(
            2,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
        );
        let scaled = scale_layouts(&layouts, 1.5);
        let mut ids: Vec<NodeId> = scaled.keys().copied().collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2]);
    }
}
