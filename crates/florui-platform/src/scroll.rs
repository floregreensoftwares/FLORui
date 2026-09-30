//! [`use_scroll_offset`]: real per-id scroll state for an `overflow:
//! scroll`/`auto` element — the [`crate::use_committed_size`] counterpart
//! for a scrollable container.
//!
//! The offset is a plain cell, with a version [`Signal`] bumped to mark the
//! owning scope dirty. An imperative scroll always bumps it; a real wheel
//! tick bumps it only if a render read the offset via
//! [`ScrollHandle::offset`], so a scroll box nothing rendered from just
//! repaints.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use florui_layout::{BoxLayout, ContentExtent};
use florui_reactive::{Cleanup, Signal, use_attachment, use_context, use_signal};
use florui_style::{Arena, ComputedStyle, NodeId};

use crate::scroll_animation::{ScrollAnimation, ScrollKind};

struct ScrollEntry {
    offset: Rc<Cell<(f32, f32)>>,
    animation: Option<ScrollAnimation>,
    /// The element's computed `scroll-behavior: smooth`, as of the last
    /// render; what a plain [`ScrollHandle::scroll_to`] follows.
    smooth_by_css: bool,
    /// Bumped to mark the owning scope dirty; the offset itself is a plain
    /// cell so a wheel tick nobody rendered from can skip a re-render.
    version: Signal<u64>,
    read_in_render: Cell<bool>,
    viewport_size: (f32, f32),
    content_size: (f32, f32),
    last_notified: Option<(f32, f32)>,
    on_scroll: Box<dyn FnMut(f32, f32)>,
}

/// Shared per-[`crate::UiRuntime`] registry of scrollable elements,
/// provided fresh through context every render the same way
/// [`crate::SizeObserverRegistry`] is.
#[derive(Default)]
pub struct ScrollRegistry {
    entries: RefCell<HashMap<String, ScrollEntry>>,
    epoch: Cell<Option<std::time::Instant>>,
    rendering: Cell<bool>,
    /// Ids read during a render before their entry existed (the first
    /// render), so registration can still record the dependency.
    read_before_registration: RefCell<HashSet<String>>,
}

fn bump(version: &Signal<u64>) {
    version.set(version.get().wrapping_add(1));
}

impl ScrollRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a render pass: an offset read from here until
    /// [`Self::end_render`] is one the rendered output depends on.
    pub(crate) fn begin_render(&self) {
        self.rendering.set(true);
        self.read_before_registration.borrow_mut().clear();
        for entry in self.entries.borrow().values() {
            entry.read_in_render.set(false);
        }
    }

    pub(crate) fn end_render(&self) {
        self.rendering.set(false);
    }

    fn set(
        &self,
        id: String,
        offset: Rc<Cell<(f32, f32)>>,
        version: Signal<u64>,
        on_scroll: Box<dyn FnMut(f32, f32)>,
    ) {
        let pending_read = self.read_before_registration.borrow_mut().remove(&id)
            || self
                .entries
                .borrow()
                .get(&id)
                .is_some_and(|entry| entry.read_in_render.get());
        self.entries.borrow_mut().insert(
            id,
            ScrollEntry {
                offset,
                animation: None,
                smooth_by_css: false,
                version,
                read_in_render: Cell::new(pending_read),
                viewport_size: (0.0, 0.0),
                content_size: (0.0, 0.0),
                last_notified: None,
                on_scroll,
            },
        );
    }

    fn remove(&self, id: &str) {
        self.entries.borrow_mut().remove(id);
    }

    /// Refreshes every registered id's viewport/content size from this
    /// render's own real geometry, and re-clamps its persisted offset —
    /// content that shrank (e.g. a future virtualized list removing items)
    /// can't leave the offset pointing past its new end. Called once per
    /// [`crate::UiRuntime::update`], right after `size_observers.notify` —
    /// see that call's own doc for why after, not before.
    pub(crate) fn sync(
        &self,
        arena: &Arena,
        styles: &HashMap<NodeId, ComputedStyle>,
        layouts: &HashMap<NodeId, BoxLayout>,
        content_extents: &HashMap<NodeId, ContentExtent>,
    ) {
        let ids: Vec<String> = self.entries.borrow().keys().cloned().collect();
        for id in ids {
            let Some(node) = arena.find(|a, candidate| a.id_attr(candidate) == Some(id.as_str()))
            else {
                continue;
            };
            let Some(layout) = layouts.get(&node) else {
                continue;
            };
            let viewport_size = (layout.width, layout.height);
            let content_size = content_extents
                .get(&node)
                .map_or(viewport_size, |extent| (extent.width, extent.height));

            // Removed and reinserted around the callback, rather than held
            // borrowed across it, so `on_scroll` touching this same
            // registry can't panic on a re-entrant borrow — the same
            // discipline `SizeObserverRegistry::notify` already follows.
            let Some(mut entry) = self.entries.borrow_mut().remove(&id) else {
                continue;
            };
            entry.viewport_size = viewport_size;
            entry.content_size = content_size;
            entry.smooth_by_css = styles
                .get(&node)
                .is_some_and(|style| style.scroll_behavior_smooth);
            let current = entry.offset.get();
            let clamped = clamp_offset(current, viewport_size, content_size);
            if clamped != current {
                entry.offset.set(clamped);
                entry.animation = None;
                bump(&entry.version);
            }
            if entry.animation.is_some_and(|animation| {
                animation.target() != clamp_offset(animation.target(), viewport_size, content_size)
            }) {
                entry.animation = None;
            }
            if entry.last_notified != Some(clamped) {
                entry.last_notified = Some(clamped);
                (entry.on_scroll)(clamped.0, clamped.1);
            }
            self.entries.borrow_mut().insert(id, entry);
        }
    }

    /// Moves `id`'s offset by `(dx, dy)` from its current value, clamped to
    /// its last-known content/viewport size. Returns whether the offset
    /// actually changed — the real wheel handler only needs to repaint
    /// when it did.
    pub(crate) fn scroll_by(&self, id: &str, dx: f32, dy: f32) -> bool {
        let current = self.current_offset(id);
        self.scroll_to(id, current.0 + dx, current.1 + dy)
    }

    /// Sets `id`'s offset to `(x, y)`, clamped the same way, and always
    /// marks the owning scope dirty so an imperative scroll reaches the
    /// screen. Returns whether the offset actually changed.
    pub(crate) fn scroll_to(&self, id: &str, x: f32, y: f32) -> bool {
        let Some((offset, version, viewport_size, content_size)) =
            self.entries.borrow_mut().get_mut(id).map(|entry| {
                entry.animation = None;
                (
                    Rc::clone(&entry.offset),
                    entry.version.clone(),
                    entry.viewport_size,
                    entry.content_size,
                )
            })
        else {
            return false;
        };
        let clamped = clamp_offset((x, y), viewport_size, content_size);
        if clamped == offset.get() {
            return false;
        }
        offset.set(clamped);
        bump(&version);
        true
    }

    /// The real wheel's [`Self::scroll_by`]: the scope is only marked dirty
    /// when a render read this offset, so a plain scroll box repaints
    /// without re-rendering or re-laying-out. `on_scroll` still runs
    /// immediately. Returns whether the offset actually changed.
    pub(crate) fn wheel_scroll_by(&self, id: &str, dx: f32, dy: f32) -> bool {
        if let Some(entry) = self.entries.borrow_mut().get_mut(id) {
            entry.animation = None;
        }
        let current = self.current_offset(id);
        self.move_quietly(id, (current.0 + dx, current.1 + dy))
    }

    /// Moves `id` to `target` (clamped) without dirtying the scope unless a
    /// render read the offset; `on_scroll` runs immediately. Returns whether
    /// the offset changed.
    fn move_quietly(&self, id: &str, target: (f32, f32)) -> bool {
        let Some((offset, version, read_in_render, viewport_size, content_size)) =
            self.entries.borrow().get(id).map(|entry| {
                (
                    Rc::clone(&entry.offset),
                    entry.version.clone(),
                    entry.read_in_render.get(),
                    entry.viewport_size,
                    entry.content_size,
                )
            })
        else {
            return false;
        };
        let clamped = clamp_offset(target, viewport_size, content_size);
        if clamped == offset.get() {
            return false;
        }
        offset.set(clamped);
        // Removed and reinserted around the callback for the same
        // re-entrancy reason as in `sync`.
        let removed = self.entries.borrow_mut().remove(id);
        if let Some(mut entry) = removed {
            if entry.last_notified != Some(clamped) {
                entry.last_notified = Some(clamped);
                (entry.on_scroll)(clamped.0, clamped.1);
            }
            self.entries.borrow_mut().insert(id.to_string(), entry);
        }
        if read_in_render {
            bump(&version);
        }
        true
    }

    /// Seconds on the clock smooth-scroll animations run against.
    pub(crate) fn now(&self) -> f64 {
        let epoch = self.epoch.get().unwrap_or_else(|| {
            let now = std::time::Instant::now();
            self.epoch.set(Some(now));
            now
        });
        epoch.elapsed().as_secs_f64()
    }

    /// Smooth-scrolls `id` by `(dx, dy)` from where its current animation is
    /// headed (or from its offset when idle), so a burst of wheel notches
    /// accumulates. Returns whether there is now somewhere new to go.
    pub(crate) fn animate_by(
        &self,
        id: &str,
        dx: f32,
        dy: f32,
        now: f64,
        kind: ScrollKind,
    ) -> bool {
        let base = self.animation_target(id);
        self.animate_to(id, base.0 + dx, base.1 + dy, now, kind)
    }

    /// Smooth-scrolls `id` to `(x, y)`, clamped. Returns whether the target
    /// differs from where it is already headed.
    pub(crate) fn animate_to(&self, id: &str, x: f32, y: f32, now: f64, kind: ScrollKind) -> bool {
        let mut entries = self.entries.borrow_mut();
        let Some(entry) = entries.get_mut(id) else {
            return false;
        };
        let current = entry.offset.get();
        let headed = entry
            .animation
            .map_or(current, |animation| animation.target());
        let target = clamp_offset((x, y), entry.viewport_size, entry.content_size);
        if target == headed {
            return false;
        }
        match entry.animation.as_mut() {
            Some(animation) => animation.retarget(target, now),
            None => entry.animation = Some(ScrollAnimation::new(current, target, now, kind)),
        }
        true
    }

    /// A programmatic smooth scroll. Marks the scope dirty once so the host
    /// wakes and starts stepping it; the frames themselves do not re-render.
    fn scroll_smoothly_to(&self, id: &str, x: f32, y: f32) {
        if self.animate_to(id, x, y, self.now(), ScrollKind::Programmatic)
            && let Some(version) = self.entries.borrow().get(id).map(|e| e.version.clone())
        {
            bump(&version);
        }
    }

    fn smooth_by_css(&self, id: &str) -> bool {
        self.entries
            .borrow()
            .get(id)
            .is_some_and(|entry| entry.smooth_by_css)
    }

    fn animation_target(&self, id: &str) -> (f32, f32) {
        self.entries.borrow().get(id).map_or((0.0, 0.0), |entry| {
            entry
                .animation
                .map_or_else(|| entry.offset.get(), |animation| animation.target())
        })
    }

    pub(crate) fn is_animating(&self) -> bool {
        self.entries
            .borrow()
            .values()
            .any(|entry| entry.animation.is_some())
    }

    /// Moves every animating entry to where its animation is at `now`.
    /// Returns whether any is still running.
    pub(crate) fn advance_animations(&self, now: f64) -> bool {
        let animating: Vec<(String, (f32, f32), bool)> = self
            .entries
            .borrow()
            .iter()
            .filter_map(|(id, entry)| {
                let animation = entry.animation?;
                Some((id.clone(), animation.offset_at(now), animation.is_done(now)))
            })
            .collect();
        for (id, position, done) in animating {
            if done && let Some(entry) = self.entries.borrow_mut().get_mut(&id) {
                entry.animation = None;
            }
            self.move_quietly(&id, position);
        }
        self.is_animating()
    }

    pub(crate) fn current_offset(&self, id: &str) -> (f32, f32) {
        self.entries
            .borrow()
            .get(id)
            .map_or((0.0, 0.0), |entry| entry.offset.get())
    }

    /// What [`ScrollHandle::offset`] reads: during a render it also records
    /// that the output depends on this offset.
    fn offset(&self, id: &str) -> (f32, f32) {
        let entries = self.entries.borrow();
        let Some(entry) = entries.get(id) else {
            if self.rendering.get() {
                self.read_before_registration
                    .borrow_mut()
                    .insert(id.to_string());
            }
            return (0.0, 0.0);
        };
        if self.rendering.get() {
            entry.read_in_render.set(true);
        }
        entry.offset.get()
    }

    pub(crate) fn viewport_size(&self, id: &str) -> (f32, f32) {
        self.entries
            .borrow()
            .get(id)
            .map_or((0.0, 0.0), |entry| entry.viewport_size)
    }

    pub(crate) fn content_size(&self, id: &str) -> (f32, f32) {
        self.entries
            .borrow()
            .get(id)
            .map_or((0.0, 0.0), |entry| entry.content_size)
    }

    /// Every registered id's current offset, keyed by its *current*
    /// [`NodeId`] rather than its stable string id — for
    /// [`florui_layout::apply_scroll_offsets`], which walks the arena by
    /// [`NodeId`] and has no reason to know about string ids at all. An id
    /// that no longer resolves to any node this render (removed from the
    /// tree, not yet mounted) is simply absent, the same as never having
    /// scrolled.
    pub(crate) fn offsets_by_node(&self, arena: &Arena) -> HashMap<NodeId, (f32, f32)> {
        self.entries
            .borrow()
            .iter()
            .filter_map(|(id, entry)| {
                arena
                    .find(|a, candidate| a.id_attr(candidate) == Some(id.as_str()))
                    .map(|node| (node, entry.offset.get()))
            })
            .collect()
    }
}

fn clamp_offset(
    offset: (f32, f32),
    viewport_size: (f32, f32),
    content_size: (f32, f32),
) -> (f32, f32) {
    let max_x = (content_size.0 - viewport_size.0).max(0.0);
    let max_y = (content_size.1 - viewport_size.1).max(0.0);
    (offset.0.clamp(0.0, max_x), offset.1.clamp(0.0, max_y))
}

/// A real, generic scrollable element's live offset and geometry —
/// returned by [`use_scroll_offset`]. Cheap to clone; every clone reads
/// and drives the same underlying registry entry.
#[derive(Clone)]
pub struct ScrollHandle {
    registry: Rc<ScrollRegistry>,
    id: String,
}

impl ScrollHandle {
    /// The current scroll offset. Reactive: reading this inside a
    /// component re-renders it when the offset changes, the same as
    /// reading any other [`Signal`] — this is backed by one.
    pub fn offset(&self) -> (f32, f32) {
        self.registry.offset(&self.id)
    }

    /// The scrollable element's own padding-box size, as of the most
    /// recent render — a plain query against last-known geometry, the
    /// same "answer against last computed frame" contract
    /// [`crate::use_committed_size`] already has; not itself reactive.
    pub fn viewport_size(&self) -> (f32, f32) {
        self.registry.viewport_size(&self.id)
    }

    /// The element's real scrollable content extent, as of the most
    /// recent render — same non-reactive contract as
    /// [`Self::viewport_size`].
    pub fn content_size(&self) -> (f32, f32) {
        self.registry.content_size(&self.id)
    }

    /// Scrolls to `(x, y)`, clamped to the element's last-known
    /// content/viewport size, the way `element.scrollTo(x, y)` does: smooth
    /// when the element's CSS says `scroll-behavior: smooth`, immediate
    /// otherwise.
    pub fn scroll_to(&self, x: f32, y: f32) {
        if self.registry.smooth_by_css(&self.id) {
            self.scroll_to_smooth(x, y);
        } else {
            self.scroll_to_instant(x, y);
        }
    }

    /// Scrolls by `(dx, dy)` from the current offset (or from where a running
    /// smooth scroll is headed), clamped the same way and following
    /// `scroll-behavior` like [`Self::scroll_to`].
    pub fn scroll_by(&self, dx: f32, dy: f32) {
        if self.registry.smooth_by_css(&self.id) {
            self.scroll_by_smooth(dx, dy);
        } else {
            self.registry.scroll_by(&self.id, dx, dy);
        }
    }

    /// Like [`Self::scroll_to`] with `behavior: "instant"`: jumps whatever
    /// the CSS says, and cancels a running smooth scroll.
    pub fn scroll_to_instant(&self, x: f32, y: f32) {
        self.registry.scroll_to(&self.id, x, y);
    }

    /// Like [`Self::scroll_to`] with `behavior: "smooth"`, whatever the CSS
    /// says.
    pub fn scroll_to_smooth(&self, x: f32, y: f32) {
        self.registry.scroll_smoothly_to(&self.id, x, y);
    }

    /// Like [`Self::scroll_by`] with `behavior: "smooth"`; repeated calls
    /// accumulate from where the running animation is headed.
    pub fn scroll_by_smooth(&self, dx: f32, dy: f32) {
        let headed = self.registry.animation_target(&self.id);
        self.registry
            .scroll_smoothly_to(&self.id, headed.0 + dx, headed.1 + dy);
    }
}

/// Subscribes to real scroll state for the element whose `id` attribute is
/// `id`: `on_scroll(x, y)` runs whenever this element's offset actually
/// changes (a real wheel event, an imperative [`ScrollHandle::scroll_to`],
/// or a clamp forced by shrinking content), including once for whatever
/// offset the very first sync produces. Requires a [`ScrollRegistry`] in
/// context, which [`crate::UiRuntime`] provides every render — calling
/// this outside one is a programming error, not a recoverable condition.
///
/// # Panics
///
/// Panics if no [`ScrollRegistry`] is in context.
pub fn use_scroll_offset(
    id: impl Into<String>,
    on_scroll: impl FnMut(f32, f32) + 'static,
) -> ScrollHandle {
    let id = id.into();
    let registry = use_context::<Rc<ScrollRegistry>>().expect(
        "use_scroll_offset needs a ScrollRegistry in context — only a UiRuntime-hosted render \
         provides one",
    );
    let offset = use_signal(|| Rc::new(Cell::new((0.0f32, 0.0f32)))).get();
    let version = use_signal(|| 0u64);
    let setup_id = id.clone();
    use_attachment(Rc::clone(&registry), id.clone(), move |registry| {
        registry.set(
            setup_id.clone(),
            Rc::clone(&offset),
            version.clone(),
            Box::new(on_scroll),
        );
        let registry = Rc::clone(registry);
        let cleanup_id = setup_id;
        Some(Box::new(move || registry.remove(&cleanup_id)) as Cleanup)
    });
    ScrollHandle { registry, id }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use florui::prelude::*;
    use taffy::prelude::*;

    use super::*;
    use crate::UiRuntime;

    #[test]
    fn clamp_offset_bounds_to_zero_and_to_the_real_scrollable_range() {
        assert_eq!(
            clamp_offset((-5.0, -5.0), (50.0, 50.0), (200.0, 200.0)),
            (0.0, 0.0)
        );
        assert_eq!(
            clamp_offset((1000.0, 1000.0), (50.0, 50.0), (200.0, 200.0)),
            (150.0, 150.0)
        );
        assert_eq!(
            clamp_offset((10.0, 10.0), (50.0, 50.0), (30.0, 30.0)),
            (0.0, 0.0),
            "content smaller than the viewport has nothing to scroll, regardless of a stale non-zero offset"
        );
    }

    fn viewport() -> Size<AvailableSpace> {
        Size {
            width: AvailableSpace::Definite(200.0),
            height: AvailableSpace::Definite(200.0),
        }
    }

    #[test]
    fn use_scroll_offset_reports_real_geometry_and_clamps_scroll_to_the_real_content_extent() {
        let handle_slot: Rc<RefCell<Option<ScrollHandle>>> = Rc::new(RefCell::new(None));
        let on_scroll_log: Rc<RefCell<Vec<(f32, f32)>>> = Rc::new(RefCell::new(Vec::new()));
        let root = {
            let handle_slot = Rc::clone(&handle_slot);
            let on_scroll_log = Rc::clone(&on_scroll_log);
            move || {
                let log = Rc::clone(&on_scroll_log);
                let handle = use_scroll_offset("box", move |x, y| log.borrow_mut().push((x, y)));
                *handle_slot.borrow_mut() = Some(handle);
                view! {
                    <div id="box" class="box">
                        <div class="content" />
                    </div>
                }
            }
        };
        let rules = florui_style::parse_stylesheet(
            ".box { width: 50px; height: 50px; } .content { width: 10px; height: 200px; }",
        )
        .unwrap();
        let mut runtime = UiRuntime::with_rules(rules, root, viewport());

        // `UiRuntime::with_rules` already ran one real `update` during
        // construction — the very first sync, right after that first real
        // layout, already reports real geometry and fires `on_scroll` once
        // for the initial `(0, 0)` offset, the same "including once for
        // whatever ... the very first ... produces" contract
        // `use_committed_size` already has.
        let handle = handle_slot.borrow().clone().unwrap();
        assert_eq!(handle.viewport_size(), (50.0, 50.0));
        assert_eq!(handle.content_size(), (10.0, 200.0));
        assert_eq!(on_scroll_log.borrow().as_slice(), &[(0.0, 0.0)]);

        handle.scroll_to(0.0, 1000.0);
        runtime.update(viewport());
        assert_eq!(
            handle.offset(),
            (0.0, 150.0),
            "scrolling past the real content extent must clamp to the real maximum \
             (content height 200 minus viewport height 50), not the requested value"
        );
        assert_eq!(
            on_scroll_log.borrow().as_slice(),
            &[(0.0, 0.0), (0.0, 150.0)],
            "on_scroll must fire again for the clamped value actually reached"
        );

        handle.scroll_to(0.0, 150.0);
        runtime.update(viewport());
        assert_eq!(
            on_scroll_log.borrow().len(),
            2,
            "on_scroll must not fire again when the offset does not actually change"
        );
    }

    fn scroll_box_runtime(
        read_offset_in_render: bool,
        log: Rc<RefCell<Vec<(f32, f32)>>>,
    ) -> UiRuntime {
        let root = move || {
            let log = Rc::clone(&log);
            let handle = use_scroll_offset("box", move |x, y| log.borrow_mut().push((x, y)));
            if read_offset_in_render {
                let _ = handle.offset();
            }
            view! {
                <div id="box" class="box">
                    <div class="content" />
                </div>
            }
        };
        let rules = florui_style::parse_stylesheet(
            ".box { width: 50px; height: 50px; } .content { width: 10px; height: 200px; }",
        )
        .unwrap();
        let runtime = UiRuntime::with_rules(rules, root, viewport());
        runtime.clear_dirty();
        runtime
    }

    #[test]
    fn a_wheel_tick_nothing_rendered_from_moves_the_offset_without_dirtying_the_scope() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let runtime = scroll_box_runtime(false, Rc::clone(&log));
        let registry = runtime.scroll_registry();

        assert!(registry.wheel_scroll_by("box", 0.0, 30.0));

        assert!(
            !runtime.is_dirty(),
            "no render read the offset, so nothing needs re-rendering"
        );
        assert_eq!(registry.current_offset("box"), (0.0, 30.0));
        assert_eq!(log.borrow().last(), Some(&(0.0, 30.0)));
    }

    #[test]
    fn a_wheel_tick_dirties_the_scope_when_a_render_read_the_offset() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let runtime = scroll_box_runtime(true, log);
        let registry = runtime.scroll_registry();

        assert!(registry.wheel_scroll_by("box", 0.0, 30.0));

        assert!(
            runtime.is_dirty(),
            "a component rendered from this offset must be re-rendered"
        );
    }

    #[test]
    fn an_imperative_scroll_always_dirties_the_scope() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let runtime = scroll_box_runtime(false, log);
        let registry = runtime.scroll_registry();

        assert!(registry.scroll_to("box", 0.0, 30.0));

        assert!(
            runtime.is_dirty(),
            "nothing else would repaint an imperative scroll"
        );
    }

    #[test]
    fn a_wheel_tick_that_clamps_to_no_change_reports_nothing() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let runtime = scroll_box_runtime(false, log);
        let registry = runtime.scroll_registry();

        assert!(registry.wheel_scroll_by("box", 0.0, 1000.0));
        assert_eq!(registry.current_offset("box"), (0.0, 150.0));
        assert!(!registry.wheel_scroll_by("box", 0.0, 10.0));
    }

    #[test]
    fn a_smooth_scroll_moves_gradually_to_its_target_without_dirtying_the_scope() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let runtime = scroll_box_runtime(false, Rc::clone(&log));
        let registry = runtime.scroll_registry();

        assert!(registry.animate_by("box", 0.0, 100.0, 0.0, ScrollKind::Wheel));
        assert!(registry.is_animating());
        assert_eq!(registry.current_offset("box"), (0.0, 0.0));

        let mut last = 0.0;
        for step in 1..=5 {
            registry.advance_animations(f64::from(step) * 0.02);
            let y = registry.current_offset("box").1;
            assert!(y >= last, "never moves backwards");
            last = y;
        }
        assert!(last > 0.0 && last < 100.0, "partway at 0.1s: {last}");
        assert!(
            !runtime.is_dirty(),
            "frames do not re-render a plain scroll box"
        );

        assert!(!registry.advance_animations(1.0));
        assert_eq!(registry.current_offset("box"), (0.0, 100.0));
        assert!(!registry.is_animating());
        assert!(log.borrow().len() > 3, "on_scroll follows every frame");
        assert_eq!(log.borrow().last(), Some(&(0.0, 100.0)));
    }

    #[test]
    fn repeated_notches_accumulate_from_where_the_animation_is_headed() {
        let runtime = scroll_box_runtime(false, Rc::new(RefCell::new(Vec::new())));
        let registry = runtime.scroll_registry();

        registry.animate_by("box", 0.0, 100.0, 0.0, ScrollKind::Wheel);
        registry.advance_animations(0.03);
        registry.animate_by("box", 0.0, 100.0, 0.03, ScrollKind::Wheel);
        registry.advance_animations(5.0);

        assert_eq!(
            registry.current_offset("box"),
            (0.0, 150.0),
            "two notches add up, clamped to the real maximum (200 - 50)"
        );
    }

    #[test]
    fn an_instant_scroll_cancels_a_running_smooth_one() {
        let runtime = scroll_box_runtime(false, Rc::new(RefCell::new(Vec::new())));
        let registry = runtime.scroll_registry();
        registry.animate_by("box", 0.0, 100.0, 0.0, ScrollKind::Wheel);

        registry.scroll_to("box", 0.0, 20.0);

        assert!(!registry.is_animating());
        registry.advance_animations(5.0);
        assert_eq!(registry.current_offset("box"), (0.0, 20.0));
    }

    #[test]
    fn a_smooth_scroll_dirties_the_scope_each_frame_when_a_render_read_the_offset() {
        let runtime = scroll_box_runtime(true, Rc::new(RefCell::new(Vec::new())));
        let registry = runtime.scroll_registry();
        registry.animate_by("box", 0.0, 100.0, 0.0, ScrollKind::Wheel);
        assert!(
            !runtime.is_dirty(),
            "starting a wheel animation moves nothing yet"
        );

        registry.advance_animations(0.05);

        assert!(
            runtime.is_dirty(),
            "the rendered output depends on the offset"
        );
    }

    /// An outer and an inner scroll box, each with a handle, under `css`.
    fn nested_boxes(css: &str) -> (UiRuntime, ScrollHandle, ScrollHandle) {
        let slots: Rc<RefCell<Vec<ScrollHandle>>> = Rc::default();
        let captured = Rc::clone(&slots);
        let root = move || {
            let outer = use_scroll_offset("outer", |_, _| {});
            let inner = use_scroll_offset("inner", |_, _| {});
            *captured.borrow_mut() = vec![outer, inner];
            view! {
                <div id="outer" class="outer">
                    <div id="inner" class="inner">
                        <div class="content" />
                    </div>
                    <div class="content" />
                </div>
            }
        };
        let css = format!(
            ".outer {{ width: 60px; height: 60px; overflow-y: auto; }} \
             .inner {{ width: 50px; height: 50px; overflow-y: auto; }} \
             .content {{ width: 10px; height: 200px; }} {css}"
        );
        let runtime = UiRuntime::with_rules(
            florui_style::parse_stylesheet(&css).unwrap(),
            root,
            viewport(),
        );
        let handles = slots.borrow().clone();
        (runtime, handles[0].clone(), handles[1].clone())
    }

    #[test]
    fn css_scroll_behavior_smooth_makes_a_plain_scroll_to_animate() {
        let (runtime, outer, _) = nested_boxes(".outer { scroll-behavior: smooth; }");
        let registry = runtime.scroll_registry();

        outer.scroll_to(0.0, 100.0);

        assert!(registry.is_animating());
        assert_eq!(registry.current_offset("outer"), (0.0, 0.0), "not a jump");
        registry.advance_animations(registry.now() + 10.0);
        assert_eq!(registry.current_offset("outer"), (0.0, 100.0));
    }

    #[test]
    fn without_the_css_a_plain_scroll_to_still_jumps() {
        let (runtime, outer, _) = nested_boxes("");
        outer.scroll_to(0.0, 100.0);
        assert!(!runtime.scroll_registry().is_animating());
        assert_eq!(
            runtime.scroll_registry().current_offset("outer"),
            (0.0, 100.0)
        );
    }

    #[test]
    fn an_explicit_instant_scroll_ignores_the_css() {
        let (runtime, outer, _) = nested_boxes(".outer { scroll-behavior: smooth; }");
        outer.scroll_to_instant(0.0, 100.0);
        assert!(!runtime.scroll_registry().is_animating());
        assert_eq!(
            runtime.scroll_registry().current_offset("outer"),
            (0.0, 100.0)
        );
    }

    #[test]
    fn a_nested_scroller_under_a_smooth_one_stays_instant_because_the_property_does_not_inherit() {
        let (runtime, _, inner) = nested_boxes(".outer { scroll-behavior: smooth; }");
        inner.scroll_to(0.0, 100.0);
        assert!(!runtime.scroll_registry().is_animating());
        assert_eq!(
            runtime.scroll_registry().current_offset("inner"),
            (0.0, 100.0)
        );
    }

    #[test]
    fn a_programmatic_smooth_scroll_wakes_the_host_once_and_then_animates() {
        let runtime = scroll_box_runtime(false, Rc::new(RefCell::new(Vec::new())));
        let registry = runtime.scroll_registry();

        registry.scroll_smoothly_to("box", 0.0, 120.0);

        assert!(runtime.is_dirty(), "the host is woken to start stepping it");
        assert!(registry.is_animating());
        registry.advance_animations(registry.now() + 10.0);
        assert_eq!(registry.current_offset("box"), (0.0, 120.0));
    }
}
