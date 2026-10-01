//! Pointer, keyboard and text-input handling that does not depend on a
//! window. [`Input`] borrows a [`UiRuntime`], the [`InputState`] of one window
//! and an [`InputHost`], the few things it needs from the window around it
//! (redraws, cursor shape, IME, window drag and resize). The desktop host
//! implements the trait with a real window; a test harness implements it with
//! nothing, so both drive the same code.

use florui_style::NodeId;
use florui_text::editing::TextEditOp;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Ime, MouseScrollDelta};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::window::{CursorIcon, ResizeDirection};

use crate::UiRuntime;
use crate::appearance::DecorationMode;
use crate::components::popover;
use crate::desktop::{layout_viewport, text_input_content_origin};
use crate::dpi::ViewportScale;
use crate::list_keys::ListKey;
use crate::menu_keys::{self, MenuKey, MenuMove};
use crate::scroll_animation::ScrollKind;
use crate::window_controls::resize_direction_at;

/// A key press or release, free of the window system's event type so a test
/// can build one.
#[derive(Clone, Debug)]
pub(crate) struct KeyInput {
    pub(crate) logical_key: Key,
    pub(crate) physical_key: PhysicalKey,
    pub(crate) state: ElementState,
    pub(crate) repeat: bool,
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
pub(crate) struct ScrollDrag {
    pub(crate) id: String,
    pub(crate) grab: f32,
}

/// A textarea's geometry as the pointer sees it.
pub(crate) struct TextareaBox {
    id: String,
    origin: (f32, f32),
    size: (f32, f32),
    border: (f32, f32, f32, f32),
    chrome: (f32, f32),
    mode: florui_style::Resize,
    bar: Option<florui_paint::ScrollbarPaint>,
}

/// A textarea's resize corner being dragged.
pub(crate) struct ResizeDrag {
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
pub(crate) struct ScrollHold {
    id: String,
    part: florui_paint::ScrollbarPart,
    next_step: std::time::Instant,
}

/// A number field's spinner arrow being held down.
pub(crate) struct SpinnerHold {
    field: florui_style::FocusPath,
    direction: i32,
    next_step: std::time::Instant,
}

/// Logical pixels one wheel "line" (`MouseScrollDelta::LineDelta`'s own
/// unit) scrolls — real mouse wheels report in lines, not pixels, so this
/// is the conversion factor into the logical pixels a scroll offset is
/// measured in. Not spec-mandated to an exact number, the same as
/// [`WINDOW_STATE_SAVE_DEBOUNCE`]: browsers commonly use a value in this
/// range for the same conversion.
const WHEEL_LINE_HEIGHT: f32 = 40.0;

/// Logical pixels one wheel notch scrolls, measured in Edge at the default
/// of three lines per notch. The OS lines-per-notch setting is not read.
const WHEEL_NOTCH_PIXELS: f32 = 100.0;

/// Pointer, keyboard and hold-to-repeat state of one window.
#[derive(Default)]
pub(crate) struct InputState {
    /// The node hit-tested at the last left-button press, if any — a
    /// click only dispatches on release over this same node.
    pub(crate) pressed: Option<NodeId>,
    /// Where the Space key went down, if it is still held: Space activates
    /// on release, and only if focus is still there (as in real HTML).
    pub(crate) space_armed: Option<florui_style::FocusPath>,
    pub(crate) last_cursor: (f64, f64),
    /// Updated only by a live `WindowEvent::ModifiersChanged` — a real
    /// `KeyEvent` carries no modifier state of its own, so Shift+Tab needs
    /// this to distinguish itself from a plain Tab.
    pub(crate) modifiers: ModifiersState,
    /// The editable `<input>` a left-button press started a text
    /// selection drag on, if any — still down, not yet released.
    /// `CursorMoved` while this is `Some` extends the selection to the
    /// cursor's current position; `handle_release` clears it.
    pub(crate) text_selecting: Option<NodeId>,
    /// The node and instant of the last real left-button press on an
    /// editable `<input>` — a second press on the *same* node within
    /// [`DOUBLE_CLICK_INTERVAL`] selects the word under the cursor
    /// instead of just moving the caret there, the same distinction a
    /// real double-click makes. No existing double-click detection exists
    /// anywhere else in this file to reuse.
    pub(crate) last_text_input_click: Option<(NodeId, std::time::Instant, u8)>,
    /// The instant of the last real left-button press on
    /// [`crate::WINDOW_DRAG_REGION_ID`] — a second press within
    /// [`DOUBLE_CLICK_INTERVAL`] toggles maximize instead of starting
    /// another drag, the same distinction a real title bar makes.
    pub(crate) last_drag_region_click: Option<std::time::Instant>,
    /// The spinner arrow currently held down, repeating until release.
    pub(crate) spinner_hold: Option<SpinnerHold>,
    pub(crate) scroll_hold: Option<ScrollHold>,
    pub(crate) scroll_drag: Option<ScrollDrag>,
    pub(crate) resize_drag: Option<ResizeDrag>,
    /// When the running smooth scrolls next step; `None` when idle.
    pub(crate) scroll_frame_due: Option<std::time::Instant>,
}

/// What [`Input`] needs from the window around it.
pub(crate) trait InputHost {
    fn viewport_scale(&self) -> ViewportScale;
    fn request_redraw(&self);
    fn decorations(&self) -> DecorationMode;
    fn prefers_reduced_motion(&self) -> bool;
    /// Called after every update with whether something is still animating,
    /// so the host can schedule the next frame.
    fn set_animating(&mut self, animating: bool);
    fn set_ime_allowed(&self, allowed: bool);
    fn outer_position(&self) -> Result<PhysicalPosition<i32>, winit::error::NotSupportedError>;
    fn drag(&self);
    fn start_resize(&self, direction: ResizeDirection);
    fn toggle_maximize(&self);
    fn set_content_cursor(&self, icon: CursorIcon);
    fn set_resize_cursor(&self, direction: Option<ResizeDirection>);
    fn show_system_menu_at_cursor(&self);
    fn show_system_menu_at(&self, screen_x: i32, screen_y: i32);
    fn open_url(&self, url: &str) -> crate::OpenUrlOutcome;
    /// A development tool's element picker owns the pointer.
    fn is_picking(&self) -> bool;
    fn pointer_moved(&self, node: Option<NodeId>);
    fn picked(&self, node: Option<NodeId>);
}

/// The input handling of one window: its runtime and state, borrowed, and the
/// host around it.
pub(crate) struct Input<'a, H: InputHost> {
    pub(crate) runtime: &'a mut UiRuntime,
    pub(crate) state: &'a mut InputState,
    pub(crate) host: &'a mut H,
}

impl<H: InputHost> Input<'_, H> {
    fn viewport_scale(&self) -> ViewportScale {
        self.host.viewport_scale()
    }

    /// Re-renders against the current viewport and requests a repaint.
    pub(crate) fn update_and_request_redraw(&mut self) {
        let viewport = layout_viewport(self.host.viewport_scale());
        self.runtime.clear_dirty();
        self.runtime
            .set_os_prefers_reduced_motion(self.host.prefers_reduced_motion());
        self.runtime.update(viewport);
        self.host.request_redraw();
        self.refresh_animation_schedule();
    }

    fn refresh_animation_schedule(&mut self) {
        let animating = self.runtime.is_animating();
        self.host.set_animating(animating);
    }

    /// Converts a physical-pixel cursor position (as `winit` reports it)
    /// to the logical pixels layout runs against.
    pub(crate) fn to_logical_cursor(&self, x: f64, y: f64) -> (f32, f32) {
        let factor = self.viewport_scale().scale_factor;
        ((x / factor) as f32, (y / factor) as f32)
    }

    /// Only [`DecorationMode::Custom`] ever hit-tests a resize border —
    /// see [`Self::decorations`]'s own doc for why a `System`-decorated
    /// window must not.
    pub(crate) fn resize_direction_at_cursor(&self, x: f32, y: f32) -> Option<ResizeDirection> {
        if self.host.decorations() != DecorationMode::Custom {
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
    ///
    /// The flag is whether the delta is a wheel notch, which scrolls
    /// smoothly; a trackpad's pixel deltas already carry the OS's own
    /// momentum and apply as they arrive.
    pub(crate) fn to_logical_scroll_delta(&self, delta: MouseScrollDelta) -> ((f32, f32), bool) {
        match delta {
            MouseScrollDelta::LineDelta(x, y) => {
                ((-x * WHEEL_NOTCH_PIXELS, -y * WHEEL_NOTCH_PIXELS), true)
            }
            MouseScrollDelta::PixelDelta(position) => {
                let factor = self.viewport_scale().scale_factor;
                (
                    ((-position.x / factor) as f32, (-position.y / factor) as f32),
                    false,
                )
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
    pub(crate) fn scrollable_ancestor_id(&self, node: NodeId, dx: f32, dy: f32) -> Option<String> {
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
    pub(crate) fn textarea_id(&self, node: NodeId) -> Option<String> {
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
    pub(crate) fn handle_mouse_wheel(&mut self, delta: MouseScrollDelta) {
        let ((dx, dy), animated) = self.to_logical_scroll_delta(delta);
        let (x, y) = self.to_logical_cursor(self.state.last_cursor.0, self.state.last_cursor.1);
        let Some(hit) = self.runtime.hit_test(x, y) else {
            return;
        };
        if let Some(text_id) = self.textarea_id(hit) {
            let registry = self.runtime.text_input_registry();
            let (_, _, _, font) = self.runtime.geometry_and_font_mut();
            if registry.scroll_by(&text_id, dy, font) {
                self.host.request_redraw();
                return;
            }
        }
        let Some(id) = self.scrollable_ancestor_id(hit, dx, dy) else {
            return;
        };
        // No `update`: `redraw` already applies the registry's offsets, and a
        // render that read the offset (or an `on_scroll` that set state)
        // marks the scope dirty on its own.
        let registry = self.runtime.scroll_registry();
        if animated {
            // `advance_scroll_animations` moves it frame by frame.
            if registry.animate_by(&id, dx, dy, registry.now(), ScrollKind::Wheel) {
                self.host.request_redraw();
            }
        } else if registry.wheel_scroll_by(&id, dx, dy) {
            self.after_scroll_moved();
        }
    }

    /// Everything that depends on where scrolled content is drawn, for a
    /// scroll that just moved without a render.
    pub(crate) fn after_scroll_moved(&mut self) {
        self.runtime
            .reposition_for_scroll(layout_viewport(self.viewport_scale()));
        self.host.request_redraw();
        self.refresh_hover_at_cursor();
    }

    /// Steps running smooth scrolls to the current instant. Returns when to
    /// come back, or `None` once nothing is animating.
    pub(crate) fn advance_scroll_animations(
        &mut self,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        let registry = self.runtime.scroll_registry();
        if !registry.is_animating() {
            self.state.scroll_frame_due = None;
            return None;
        }
        if let Some(due) = self.state.scroll_frame_due
            && now < due
        {
            return Some(due);
        }
        let running = registry.advance_animations(registry.now());
        self.after_scroll_moved();
        self.state.scroll_frame_due = running.then(|| now + std::time::Duration::from_millis(16));
        self.state.scroll_frame_due
    }

    /// Re-resolves `:hover` and the content cursor for a pointer that hasn't
    /// moved but has different content under it, as after a wheel tick.
    pub(crate) fn refresh_hover_at_cursor(&mut self) {
        let (x, y) = self.to_logical_cursor(self.state.last_cursor.0, self.state.last_cursor.1);
        let hit = self
            .runtime
            .hit_test(x, y)
            .filter(|&node| !self.is_disabled(node));
        self.set_hovered_and_redraw(hit);
        let over_resize_edge = self.host.decorations() == DecorationMode::Custom
            && self.resize_direction_at_cursor(x, y).is_some();
        if !over_resize_edge {
            let icon = self.content_cursor(hit, x, y);
            self.host.set_content_cursor(icon);
        }
    }

    /// Updates `:hover` against the runtime's cached geometry — no
    /// rebuild just to know what's under the cursor. While a text-input
    /// drag-select is in progress (see [`Self::handle_text_input_press`]),
    /// also extends that selection to the cursor's current position —
    /// still tracked even once the cursor drags outside the input's own
    /// box, matching real text-selection behavior.
    pub(crate) fn handle_cursor_moved(&mut self, x: f64, y: f64) {
        self.state.last_cursor = (x, y);
        let (x, y) = self.to_logical_cursor(x, y);
        if self.host.is_picking() {
            let hit = self.runtime.hit_test(x, y);
            self.host.pointer_moved(hit);
            return;
        }
        // A resize edge always wins over content's own `cursor: pointer`
        // -- resolved once here so the check below can skip content
        // cursor logic entirely rather than have both methods race to
        // set the OS cursor on every move.
        let resize_direction = (self.host.decorations() == DecorationMode::Custom)
            .then(|| self.resize_direction_at_cursor(x, y))
            .flatten();
        if self.host.decorations() == DecorationMode::Custom {
            self.host.set_resize_cursor(resize_direction);
        }
        if let Some(drag) = self.state.resize_drag.as_ref() {
            self.host.set_content_cursor(resize_cursor(drag.mode));
        }
        if self.drag_textarea_chrome(x, y) {
            return;
        }
        if let Some(node) = self.state.text_selecting {
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
            self.host.request_redraw();
        }
        if resize_direction.is_none() {
            let icon = self.content_cursor(hit, x, y);
            self.host.set_content_cursor(icon);
        }
    }

    /// The cursor for what's under the pointer: measured in Edge, an I-beam
    /// over an enabled text field's text (not its spinner) and the resize
    /// arrows over a resizable textarea's corner; everything else, scrollbar
    /// included, keeps the arrow.
    pub(crate) fn content_cursor(
        &mut self,
        hit: Option<NodeId>,
        x: f32,
        y: f32,
    ) -> winit::window::CursorIcon {
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
    pub(crate) fn handle_cursor_left(&mut self) {
        self.state.pressed = None;
        self.set_hovered_and_redraw(None);
    }

    /// Applies a hover change and, only when it actually changed anything
    /// (`:hover` can affect computed style), dispatches `mouseleave`/
    /// `mouseenter` and re-renders to pick up both. `runtime.hovered()` is
    /// read before `set_hovered` overwrites it, so both `NodeId`s are
    /// valid against the same arena generation.
    pub(crate) fn set_hovered_and_redraw(&mut self, hit: Option<NodeId>) {
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
            .set_os_prefers_reduced_motion(self.host.prefers_reduced_motion());
        self.runtime.update(viewport);
        self.host.request_redraw();
        self.refresh_animation_schedule();
    }

    /// A textarea's box, border, padding, resize mode and, when its text
    /// overflows, the scrollbar it is drawn with.
    pub(crate) fn textarea_box(&mut self, node: NodeId) -> Option<TextareaBox> {
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
                self.runtime.drawn_position(node),
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
    pub(crate) fn press_textarea_chrome(&mut self, node: NodeId, x: f32, y: f32) -> bool {
        let Some(b) = self.textarea_box(node) else {
            return false;
        };
        let rel = (x - b.origin.0, y - b.origin.1);
        let resizable = b.mode != florui_style::Resize::None;
        if resizable && florui_paint::is_over_resizer(b.size, b.border, rel) {
            self.state.resize_drag = Some(ResizeDrag {
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
            self.state.scroll_drag = Some(ScrollDrag {
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
        self.state.scroll_hold = Some(ScrollHold {
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
    pub(crate) fn repeat_scroll_if_due(
        &mut self,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        let hold = self.state.scroll_hold.as_ref()?;
        if now < hold.next_step {
            return Some(hold.next_step);
        }
        let (id, part) = (hold.id.clone(), hold.part);
        let node = {
            let (arena, ..) = self.runtime.geometry();
            arena.find(|a, n| a.id_attr(n) == Some(id.as_str()))
        };
        let Some(b) = node.and_then(|node| self.textarea_box(node)) else {
            self.state.scroll_hold = None;
            return None;
        };
        if let Some(bar) = b.bar {
            let (x, y) = self.to_logical_cursor(self.state.last_cursor.0, self.state.last_cursor.1);
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
        if let Some(hold) = self.state.scroll_hold.as_mut() {
            hold.next_step = next;
        }
        Some(next)
    }

    /// Follows the pointer while a scrollbar thumb or resize corner is being
    /// dragged; `true` if one is, so nothing else reacts to the move.
    pub(crate) fn drag_textarea_chrome(&mut self, x: f32, y: f32) -> bool {
        if let Some(drag) = self
            .state
            .scroll_drag
            .as_ref()
            .map(|d| (d.id.clone(), d.grab))
        {
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
        if let Some(drag) = &self.state.resize_drag {
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
    pub(crate) fn select_all_on_tab_focus(&mut self, node: Option<NodeId>) {
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
    pub(crate) fn dismiss_bubble_on_press(&mut self, hit: Option<NodeId>) {
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
    pub(crate) fn handle_press(&mut self) {
        if self.host.is_picking() {
            return;
        }
        let (x, y) = self.to_logical_cursor(self.state.last_cursor.0, self.state.last_cursor.1);
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
            self.host.start_resize(direction);
            return;
        }

        if hit.is_some_and(|node| self.is_drag_region(node)) {
            let now = std::time::Instant::now();
            let is_double_click = self
                .state
                .last_drag_region_click
                .is_some_and(|at| now - at < DOUBLE_CLICK_INTERVAL);
            self.state.last_drag_region_click = Some(now);
            if is_double_click {
                self.host.toggle_maximize();
            } else {
                self.host.drag();
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
            self.state.spinner_hold = Some(SpinnerHold {
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
        self.state.pressed = hit.filter(|&node| !self.is_disabled(node));
    }

    /// A real right-button press on [`crate::WINDOW_DRAG_REGION_ID`] opens
    /// the real OS system menu — the same gesture a native title bar
    /// supports, only meaningful for [`DecorationMode::Custom`] (a
    /// `System`-decorated window's own OS title bar already has this for
    /// free).
    pub(crate) fn handle_right_press(&mut self) {
        if self.host.decorations() != DecorationMode::Custom {
            return;
        }
        let (x, y) = self.to_logical_cursor(self.state.last_cursor.0, self.state.last_cursor.1);
        let hit = self.runtime.hit_test(x, y);
        if hit.is_some_and(|node| self.is_drag_region(node)) {
            self.host.show_system_menu_at_cursor();
        }
    }

    /// `1`/`-1` when a press at `(x, y)` lands on the up/down arrow of the
    /// spinner of the editable number field at `node`, else `None`.
    pub(crate) fn spinner_press_direction(&self, node: NodeId, x: f32, y: f32) -> Option<i32> {
        let (arena, _, layouts) = self.runtime.geometry();
        if arena.tag(node) != "input"
            || arena.input_type(node) != Some("number")
            || arena.attr_flag(node, "readonly")
        {
            return None;
        }
        let layout = layouts.get(&node)?;
        let (bx, by) = self.runtime.drawn_position(node);
        match florui_paint::spinner_half_at((bx, by, layout.width, layout.height), (x, y)) {
            florui_paint::SpinnerHover::Up => Some(1),
            florui_paint::SpinnerHover::Down => Some(-1),
            florui_paint::SpinnerHover::None => None,
        }
    }

    pub(crate) fn is_editable_text_input(&self, node: NodeId) -> bool {
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
    pub(crate) fn handle_text_input_press(&mut self, node: NodeId, x: f32, y: f32) {
        let now = std::time::Instant::now();
        let clicks = match self.state.last_text_input_click {
            Some((last_node, at, count))
                if last_node == node && now - at < DOUBLE_CLICK_INTERVAL =>
            {
                (count + 1).min(3)
            }
            _ => 1,
        };
        self.state.last_text_input_click = Some((node, now, clicks));

        let previous = self.focused_text_input();
        self.runtime.set_focused(Some(node), false);
        if let Some(previous) = previous
            && previous != node
        {
            self.clear_compose_for(previous);
            self.reset_ime_context();
        }
        self.host.set_ime_allowed(self.allows_ime(node));
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
        self.state.text_selecting = Some(node);
        self.update_and_request_redraw();
    }

    /// The content-box origin (post border/padding) of an editable
    /// `<input>`, in the same logical-pixel space [`Self::to_logical_cursor`]
    /// already converts a real cursor position into — the space every
    /// point-based [`florui_text::editing::TextEditOp`] expects its `x`
    /// in. Mirrors `florui_paint`'s own `content_x`/`content_y`
    /// computation exactly, so a click lands on the same glyph it visibly
    /// painted over.
    pub(crate) fn text_input_content_origin(&self, node: NodeId) -> (f32, f32) {
        let (_, styles, _) = self.runtime.geometry();
        text_input_content_origin(self.runtime.drawn_position(node), styles, node)
    }

    /// The pointer position `(x, y)` in the editor's own space: relative to
    /// the content box, shifted by how far the field is scrolled.
    pub(crate) fn text_input_local_point(
        &mut self,
        node: NodeId,
        id: &str,
        x: f32,
        y: f32,
    ) -> (f32, f32) {
        let (origin_x, origin_y) = self.text_input_content_origin(node);
        let registry = self.runtime.text_input_registry();
        let (scroll_x, scroll_y) = registry.scroll_offset(id);
        let (_, _, _, font) = self.runtime.geometry_and_font_mut();
        // A password's painted bullets are spaced differently from its real
        // text, so a click is mapped back onto the real glyph positions.
        let local_x = registry.unmask_x(id, x - origin_x + scroll_x, font);
        (local_x, y - origin_y + scroll_y)
    }

    pub(crate) fn is_drag_region(&self, node: NodeId) -> bool {
        let (arena, ..) = self.runtime.geometry();
        arena.id_attr(node) == Some(crate::WINDOW_DRAG_REGION_ID)
    }

    /// Tag-gated the same as `florui_platform::focus::is_focusable` and
    /// `UiRuntime::dispatch_click`: `disabled` has no wired behavior
    /// outside `<button>`/editable-or-checkable `<input>`. Used to keep a
    /// disabled control out of `pressed` (see `handle_press`) and out of
    /// `:hover` (see `handle_cursor_moved`) -- real browsers don't
    /// deliver pointer events to a disabled control either.
    pub(crate) fn is_disabled(&self, node: NodeId) -> bool {
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

    /// One arrow step of the number field at `node`, or just a redraw at its
    /// limit.
    pub(crate) fn step_spinner(&mut self, node: NodeId, direction: i32) {
        match self.runtime.step_number_value(node, direction) {
            Some(next) => self.commit_text_input_value(node, next),
            None => self.update_and_request_redraw(),
        }
    }

    /// Repeats a held spinner arrow once its next step is due, and returns
    /// when the one after that is, or `None` when nothing is held. A held
    /// field that disappeared or was disabled ends the hold.
    pub(crate) fn repeat_spinner_if_due(
        &mut self,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        let hold = self.state.spinner_hold.as_ref()?;
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
            self.state.spinner_hold = None;
            return None;
        };
        self.step_spinner(node, direction);
        let next = now + HOLD_REPEAT_INTERVAL;
        if let Some(hold) = self.state.spinner_hold.as_mut() {
            hold.next_step = next;
        }
        Some(next)
    }

    pub(crate) fn handle_release(&mut self) {
        if self.host.is_picking() {
            let (x, y) = self.to_logical_cursor(self.state.last_cursor.0, self.state.last_cursor.1);
            let hit = self.runtime.hit_test(x, y);
            self.host.picked(hit);
            return;
        }
        self.state.text_selecting = None;
        self.state.scroll_drag = None;
        self.state.resize_drag = None;
        self.state.spinner_hold = None;
        self.state.scroll_hold = None;
        if self.runtime.is_range_dragging() {
            self.runtime.end_range_drag();
            self.update_and_request_redraw();
            return;
        }
        let (x, y) = self.to_logical_cursor(self.state.last_cursor.0, self.state.last_cursor.1);
        let pressed = self.state.pressed.take();
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
                    let (ctrl, shift) = (
                        self.state.modifiers.control_key(),
                        self.state.modifiers.shift_key(),
                    );
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
                self.host.set_ime_allowed(false);
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
    pub(crate) fn activate(&mut self, node: NodeId) {
        self.activate_with(node, false);
    }

    /// [`Self::activate`], with whether the activation came from the
    /// keyboard (a form's first invalid control takes `:focus-visible`
    /// only then).
    pub(crate) fn activate_with(&mut self, node: NodeId, via_keyboard: bool) {
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
        if self.host.open_url(&href) == crate::OpenUrlOutcome::Opened {
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
    pub(crate) fn handle_keyboard_input(
        &mut self,
        event: KeyInput,
        clipboard: &dyn crate::clipboard::ClipboardAccess,
    ) {
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
                let moved = if self.state.modifiers.shift_key() {
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
                    self.host
                        .set_ime_allowed(now_focused.is_some_and(|node| self.allows_ime(node)));
                    self.select_all_on_tab_focus(now_focused);
                    self.update_and_request_redraw();
                }
            }
            // Real Alt+Space, before the plain Space arm below claims it as
            // a click instead -- only meaningful for `DecorationMode::Custom`,
            // same as the real right-click gesture in `handle_right_press`.
            Key::Named(NamedKey::Space)
                if self.state.modifiers.alt_key()
                    && self.host.decorations() == DecorationMode::Custom =>
            {
                if let Ok(origin) = self.host.outer_position() {
                    let scale = self.viewport_scale().scale_factor;
                    let offset_x = (8.0 * scale).round() as i32;
                    let offset_y = (30.0 * scale).round() as i32;
                    self.host
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
                        self.state.space_armed = Some(florui_style::FocusPath::of(arena, focused));
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
    pub(crate) fn handle_menu_key(&mut self, key: MenuKey) -> bool {
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
    pub(crate) fn close_menus_for_tab(&mut self) {
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
    pub(crate) fn dismiss_select_root_of(&mut self, node: NodeId) {
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
    pub(crate) fn step_select(&mut self, direction: isize) {
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
    pub(crate) fn commit_active_select_option(&mut self) {
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
    pub(crate) fn release_space(&mut self) {
        let Some(armed) = self.state.space_armed.take() else {
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
    pub(crate) fn focused_text_input(&self) -> Option<NodeId> {
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
    pub(crate) fn allows_ime(&self, node: NodeId) -> bool {
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
    pub(crate) fn clear_compose_for(&mut self, node: NodeId) {
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
    pub(crate) fn reset_ime_context(&mut self) {
        self.host.set_ime_allowed(false);
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
    pub(crate) fn handle_text_input_key(
        &mut self,
        node: NodeId,
        event: &KeyInput,
        clipboard: &dyn crate::clipboard::ClipboardAccess,
    ) {
        let Some(id) = ({
            let (arena, ..) = self.runtime.geometry();
            arena.id_attr(node).map(str::to_owned)
        }) else {
            return;
        };
        let registry = self.runtime.text_input_registry();
        let ctrl = self.state.modifiers.control_key();
        let shift = self.state.modifiers.shift_key();

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
    pub(crate) fn submit_implicitly(&mut self, field: NodeId) {
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
    pub(crate) fn handle_ime_event(&mut self, ime: Ime) {
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
    pub(crate) fn commit_text_input_op(
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

    pub(crate) fn commit_text_input_undo_redo(
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
    pub(crate) fn commit_text_input_value(&mut self, node: NodeId, new_text: String) {
        self.runtime.commit_value(node, new_text);
        self.update_and_request_redraw();
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
}
