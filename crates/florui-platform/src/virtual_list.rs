//! [`use_virtual_list`]: mounts only a bounded window of a real, ordered
//! dataset — real block-flow layout, [`KeyedExtents`]/[`ScrollAnchor`]
//! (`florui_reactive::virtualization`) unchanged, [`crate::use_scroll_offset`]/
//! [`crate::use_committed_size`] as the real scroll and real-measurement
//! sources.
//!
//! Fixed and variable height share one code path: [`ItemHeight::Fixed`]
//! seeds a [`KeyedExtents`] that is simply never measured (every key
//! permanently reports its estimate, which already behaves exactly like a
//! real fixed height), so a fixed-height list also skips per-row
//! [`crate::use_committed_size`] registration entirely — nothing to
//! measure.
//!
//! Mounting itself needs no new primitive: a plain loop over the visible
//! window calling [`florui_reactive::use_child_scope_keyed`] already gives
//! correct mount/reorder/unmount semantics. "Item identity is a data key,
//! never a recycled row index" is satisfied by that dispose-then-remount
//! alone — reusing a live `ComponentScope`'s own allocated hook slots
//! across different keys (skipping mount-effect cost during fast
//! scrolling) is a real, separate performance optimization, not attempted
//! here.

use std::ops::Range;
use std::rc::Rc;

use florui::Element;
use florui_reactive::{
    Key, KeyedExtents, Ref, ScrollAnchor, Signal, use_child_scope_keyed, use_memo, use_ref,
    use_signal,
};

use crate::focus_observer::{FocusController, use_focus_controller, use_focus_within};
use crate::scroll::ScrollHandle;
use crate::size_observer::use_committed_size;
use crate::use_scroll_offset;

/// How an item's extent along the scroll axis is determined.
pub enum ItemHeight {
    /// Every item is exactly `height` logical pixels — no per-row
    /// measurement, no correction, ever.
    Fixed(f32),
    /// Items start at `estimate` and are refined in place via
    /// [`crate::use_committed_size`] once each visible row's real layout
    /// commits.
    Variable { estimate: f32 },
}

impl ItemHeight {
    fn default_estimate(&self) -> f32 {
        match self {
            ItemHeight::Fixed(height) => *height,
            ItemHeight::Variable { estimate } => *estimate,
        }
    }

    fn measures(&self) -> bool {
        matches!(self, ItemHeight::Variable { .. })
    }
}

/// Extra items mounted beyond the strictly visible range, so a fast
/// scroll doesn't show a blank frame while a new row's own layout/paint
/// catches up.
#[derive(Clone, Copy)]
pub enum Overscan {
    Items(usize),
    Pixels(f32),
}

impl Default for Overscan {
    fn default() -> Self {
        Overscan::Items(3)
    }
}

/// Cached per-`(item_count, dataset_version, extents_version)` layout:
/// every item's key, in order, plus its cumulative offset along the
/// scroll axis — `cumulative[i]` is the offset item `i` starts at, and
/// `cumulative[item_count]` is the dataset's total extent. Rebuilt only
/// when one of those three deps actually changes (see [`use_virtual_list`]'s
/// `use_memo` call) — not, critically, on every pure scroll-offset render.
struct ListLayout {
    keys: Vec<Key>,
    cumulative: Vec<f32>,
}

impl ListLayout {
    fn total(&self) -> f32 {
        *self.cumulative.last().unwrap_or(&0.0)
    }
}

fn build_layout(
    item_count: usize,
    key_for: &impl Fn(usize) -> Key,
    extents: &KeyedExtents,
) -> ListLayout {
    let mut keys = Vec::with_capacity(item_count);
    let mut cumulative = Vec::with_capacity(item_count + 1);
    let mut offset = 0.0;
    cumulative.push(0.0);
    for i in 0..item_count {
        let key = key_for(i);
        offset += extents.get(&key).value();
        cumulative.push(offset);
        keys.push(key);
    }
    ListLayout { keys, cumulative }
}

/// The visible window's `[start, end)` item indices, extended by
/// `overscan`, given `layout`'s cumulative offsets and the current
/// `(scroll_top, viewport_height)`.
fn visible_range(
    layout: &ListLayout,
    scroll_top: f32,
    viewport_height: f32,
    overscan: &Overscan,
) -> Range<usize> {
    let item_count = layout.keys.len();
    if item_count == 0 {
        return 0..0;
    }
    let (lo, hi) = match overscan {
        Overscan::Pixels(px) => (
            (scroll_top - px).max(0.0),
            scroll_top + viewport_height + px,
        ),
        Overscan::Items(_) => (scroll_top, scroll_top + viewport_height),
    };
    // The item whose own range straddles `lo` is one before the first
    // cumulative entry that already exceeds it.
    let start = layout.cumulative[..item_count]
        .partition_point(|&c| c <= lo)
        .saturating_sub(1);
    // Any item whose own range starts before `hi` needs to be mounted,
    // even if it extends past `hi` — this counts exactly those.
    let end = layout
        .cumulative
        .partition_point(|&c| c < hi)
        .min(item_count);
    match overscan {
        Overscan::Items(n) => (start.saturating_sub(*n))..(end + n).min(item_count),
        Overscan::Pixels(_) => start..end,
    }
}

/// The index of the item at the top of the viewport, `None` for an empty
/// list.
fn first_visible(layout: &ListLayout, scroll_top: f32) -> Option<usize> {
    let item_count = layout.keys.len();
    (item_count > 0).then(|| {
        layout.cumulative[..item_count]
            .partition_point(|&c| c <= scroll_top)
            .saturating_sub(1)
    })
}
/// A mounted virtualized list's live offset and imperative controls —
/// returned by [`use_virtual_list`] alongside the [`Element`] it actually
/// renders.
#[derive(Clone)]
pub struct VirtualListHandle {
    id: String,
    scroll: ScrollHandle,
    extents: Ref<KeyedExtents>,
    layout: Ref<Option<Rc<ListLayout>>>,
    pinned: Ref<Option<Key>>,
    pin_version: Signal<u64>,
    focus: Rc<FocusController>,
}

impl VirtualListHandle {
    /// The list's current scroll offset — reactive, the same as
    /// [`crate::scroll::ScrollHandle::offset`].
    pub fn offset(&self) -> (f32, f32) {
        self.scroll.offset()
    }

    /// Scrolls so `key`'s own top edge is at the top of the viewport, if
    /// it's currently part of the dataset — a key no longer present is a
    /// silent no-op, the same "caller decides the fallback" contract
    /// [`ScrollAnchor::resolve`] already has; [`ScrollHandle::scroll_to`]'s
    /// own clamping still protects against any stale offset regardless.
    /// Top-edge only in this first pass — start/center/end alignment is a
    /// small, additive follow-up on this same mechanism, not built here.
    pub fn scroll_to_item(&self, key: impl Into<Key>) {
        let Some(layout) = self.layout.get() else {
            return;
        };
        let anchor = ScrollAnchor::new(key.into(), 0.0);
        if let Some(target_y) = self.extents.with(|e| anchor.resolve(&layout.keys, e)) {
            let (x, _) = self.scroll.offset();
            self.scroll.scroll_to(x, target_y);
        }
    }

    /// Brings `key`'s item into view, mounts it if it was not, and moves
    /// focus to it (its first focusable part when the row itself is not
    /// focusable) — for keyboard movement to an item that is not mounted
    /// yet. A key no longer present does nothing.
    pub fn focus_item(&self, key: impl Into<Key>) {
        let key = key.into();
        let Some(layout) = self.layout.get() else {
            return;
        };
        if !layout.keys.contains(&key) {
            return;
        }
        self.scroll_to_item(key.clone());
        self.focus.request_focus(row_id_of(&self.id, &key));
        self.pinned.set(Some(key));
        self.pin_version.set(self.pin_version.get() + 1);
    }
}

fn spacer(height: f32) -> Element {
    Element::node(
        "div",
        vec![("style".to_string(), format!("height: {height}px;"))],
        Vec::new(),
    )
}

/// Renders only the visible (plus `overscan`) window of an `item_count`-long
/// ordered dataset, keyed by `key_for` and rendered by `render_item` —
/// neither is ever called for an off-screen index. Returns the actual
/// content (two spacers plus the mounted rows) for the caller to place
/// inside their own scrollable element, whose `id` attribute must be this
/// same `id` (see [`crate::use_scroll_offset`]'s identical requirement)
/// and whose CSS must declare a real `overflow-y: scroll`/`auto` and a
/// definite height.
///
/// `dataset_version` is a caller-supplied token (any `PartialEq + Clone`,
/// e.g. a counter bumped whenever the caller's own backing collection is
/// reordered, filtered, or spliced) — the same "caller supplies deps,
/// nothing here infers them" contract [`florui_reactive::use_memo`]
/// already uses throughout this codebase.
pub fn use_virtual_list(
    id: impl Into<String>,
    item_count: usize,
    dataset_version: impl PartialEq + Clone + 'static,
    height: ItemHeight,
    overscan: Overscan,
    key_for: impl Fn(usize) -> Key,
    render_item: impl Fn(usize) -> Element,
) -> (Element, VirtualListHandle) {
    let id = id.into();
    let measures = height.measures();
    let extents = use_ref(|| KeyedExtents::new(height.default_estimate()));
    let extents_version = use_signal(|| 0u64);
    // The focused row stays mounted wherever it scrolls (a bounded policy: one
    // row), so typing, a caret or an IME composition in it is never torn down.
    let pinned = use_ref(|| None::<Key>);
    let pinned_index = use_ref(|| 0usize);
    let pin_version = use_signal(|| 0u64);
    let focus = use_focus_controller();
    let _ = pin_version.get();

    let layout: Rc<ListLayout> = {
        let extents = extents.clone();
        use_memo(
            (item_count, dataset_version, extents_version.get()),
            move |_| Rc::new(extents.with(|e| build_layout(item_count, &key_for, e))),
        )
    };

    let scroll = use_scroll_offset(id.clone(), |_, _| {});
    let (scroll_x, mut scroll_top) = scroll.offset();

    // Keeps whatever item is currently topmost pinned across a structural
    // change (a measurement correcting an estimate, an insertion or
    // removal ahead of the viewport) -- `last_layout` is compared by *Rc
    // pointer*, not content, so it changes if and only if `use_memo`
    // above actually recomputed (item_count/dataset_version/extents_version
    // genuinely changed), never merely because the user scrolled. Without
    // that distinction, resolving the anchor on every render would fight
    // a real, in-progress scroll instead of only correcting real drift.
    let last_layout = use_ref(|| None::<Rc<ListLayout>>);
    let anchor = use_ref(|| None::<ScrollAnchor>);
    // Where the list was scrolled when `anchor` was taken: if it has moved
    // since, the user scrolled (or `scroll_to_item` ran) and the anchor is
    // stale, so it must not drag the list back.
    let anchored_at = use_ref(|| 0.0f32);
    if let (Some(previous), Some(current_anchor)) = (last_layout.get(), anchor.get())
        && (scroll_top - anchored_at.get()).abs() < 0.5
        && !Rc::ptr_eq(&previous, &layout)
        && let Some(target_y) = extents.with(|e| current_anchor.resolve(&layout.keys, e))
        && (target_y - scroll_top).abs() > f32::EPSILON
    {
        scroll.scroll_to(scroll_x, target_y);
        scroll_top = target_y;
    }
    last_layout.set(Some(Rc::clone(&layout)));

    // `ScrollHandle::viewport_size` is a deliberately non-reactive query
    // against last-known geometry (same contract as `use_committed_size`
    // itself) -- reading it here would leave this list permanently empty
    // on its first real render, since nothing would ever schedule the
    // follow-up render that sees the real size `ScrollRegistry::sync`
    // only learns *after* this render's own layout commits. Mirroring it
    // into a real `Signal` via `use_committed_size` is what actually makes
    // "the container's real size just became known" cause a re-render.
    let viewport_height_signal = use_signal(|| 0.0f32);
    {
        let viewport_height_signal = viewport_height_signal.clone();
        use_committed_size(id.clone(), move |_w, h| viewport_height_signal.set(h));
    }
    let viewport_height = viewport_height_signal.get();
    let range = visible_range(&layout, scroll_top, viewport_height, &overscan);

    // Anchored on the first *visible* item, not the first mounted one: rows
    // mounted above the viewport as overscan correct their heights too, and
    // it is what the user sees that must not move.
    anchor.set(first_visible(&layout, scroll_top).map(|first| {
        ScrollAnchor::new(
            layout.keys[first].clone(),
            scroll_top - layout.cumulative[first],
        )
    }));
    anchored_at.set(scroll_top);

    let before = layout.cumulative.get(range.start).copied().unwrap_or(0.0);
    let after = layout.total()
        - layout
            .cumulative
            .get(range.end)
            .copied()
            .unwrap_or_else(|| layout.total());

    // The focused row is found by key, so it follows its item through any
    // reorder; if the item left the dataset, focus falls back to whatever now
    // sits at its index.
    let mut pin = pinned
        .get()
        .and_then(|key| layout.keys.iter().position(|k| *k == key));
    if pin.is_none() && pinned.get().is_some() {
        match item_count
            .checked_sub(1)
            .map(|last| pinned_index.get().min(last))
        {
            Some(fallback) => {
                pinned.set(Some(layout.keys[fallback].clone()));
                focus.request_focus(row_id_of(&id, &layout.keys[fallback]));
                pin = Some(fallback);
            }
            None => pinned.set(None),
        }
    }
    if let Some(index) = pin {
        pinned_index.set(index);
    }
    let pinned_outside = pin.filter(|index| !range.contains(index));

    let mount_row = |i: usize| -> Element {
        let key = layout.keys[i].clone();
        let row_id = row_id_of(&id, &key);
        let element = use_child_scope_keyed(key.clone(), || {
            if measures {
                let extents = extents.clone();
                let extents_version = extents_version.clone();
                let row_key = key.clone();
                use_committed_size(row_id.clone(), move |_w, h| {
                    let changed = extents.with_mut(|e| {
                        let before = e.get(&row_key).value();
                        e.measure(row_key.clone(), h);
                        (before - h).abs() > f32::EPSILON
                    });
                    if changed {
                        extents_version.set(extents_version.get() + 1);
                    }
                });
            }
            {
                let pinned = pinned.clone();
                let pin_version = pin_version.clone();
                let row_key = key.clone();
                use_focus_within(row_id.clone(), move |within| {
                    let changed = pinned.with_mut(|current| {
                        if within && current.as_ref() != Some(&row_key) {
                            *current = Some(row_key.clone());
                            true
                        } else if !within && current.as_ref() == Some(&row_key) {
                            *current = None;
                            true
                        } else {
                            false
                        }
                    });
                    if changed {
                        pin_version.set(pin_version.get() + 1);
                    }
                });
            }
            render_item(i)
        });
        stamp_row(element, row_id, i + 1, item_count)
    };

    let offset = |index: usize| layout.cumulative[index];
    let mut children = Vec::with_capacity(range.len() + 4);
    match pinned_outside {
        Some(p) if p < range.start => {
            children.push(spacer(offset(p)));
            children.push(mount_row(p));
            children.push(spacer((offset(range.start) - offset(p + 1)).max(0.0)));
        }
        _ => children.push(spacer(before)),
    }
    for i in range.clone() {
        children.push(mount_row(i));
    }
    match pinned_outside {
        Some(p) if p >= range.end => {
            children.push(spacer((offset(p) - offset(range.end)).max(0.0)));
            children.push(mount_row(p));
            children.push(spacer((layout.total() - offset(p + 1)).max(0.0)));
        }
        _ => children.push(spacer(after.max(0.0))),
    }

    (
        Element::Fragment(children),
        VirtualListHandle {
            id,
            scroll,
            extents,
            layout: last_layout,
            pinned,
            pin_version,
            focus,
        },
    )
}

fn row_id_of(list_id: &str, key: &Key) -> String {
    format!("{list_id}__row__{}", key.as_str())
}

/// Stamps what the list owns onto a row: its `id` (which the size and focus
/// observers find it by, replacing an author's own), its `listitem` role
/// unless the author gave one, and its place in the whole collection for the
/// accessibility tree. A bare text, fragment or portal result gets a wrapper
/// div to carry them.
fn stamp_row(element: Element, row_id: String, position: usize, size: usize) -> Element {
    let mut element = match element {
        Element::Node(_) => element,
        other => Element::node("div", Vec::new(), vec![other]),
    };
    if let Element::Node(node) = &mut element {
        node.attrs
            .retain(|(name, _)| !matches!(name.as_str(), "id" | "set_size" | "position_in_set"));
        node.attrs.push(("id".to_string(), row_id));
        node.attrs.push(("set_size".to_string(), size.to_string()));
        node.attrs
            .push(("position_in_set".to_string(), position.to_string()));
        if !node.attrs.iter().any(|(name, _)| name == "role") {
            node.attrs
                .push(("role".to_string(), "listitem".to_string()));
        }
    }
    element
}

#[cfg(test)]
mod tests {
    use florui_style::{Arena, NodeId};
    use taffy::prelude::*;

    use super::*;
    use crate::UiRuntime;

    fn viewport() -> Size<AvailableSpace> {
        Size {
            width: AvailableSpace::Definite(200.0),
            height: AvailableSpace::Definite(200.0),
        }
    }

    fn row(i: usize) -> Element {
        Element::node(
            "div",
            vec![("class".to_string(), "row".to_string())],
            vec![Element::text(i.to_string())],
        )
    }

    /// A fixed-height list of `item_count` rows inside a `viewport_height`px
    /// scrollable box, each row `item_height`px tall — the exact CSS
    /// numbers a test's own assertions reason about.
    fn build_runtime(
        item_count: usize,
        item_height: f32,
        viewport_height: f32,
        overscan: Overscan,
    ) -> UiRuntime {
        build_runtime_with_handle(item_count, item_height, viewport_height, overscan).0
    }

    fn build_runtime_with_handle(
        item_count: usize,
        item_height: f32,
        viewport_height: f32,
        overscan: Overscan,
    ) -> (
        UiRuntime,
        std::rc::Rc<std::cell::RefCell<Option<VirtualListHandle>>>,
    ) {
        let handle_slot: std::rc::Rc<std::cell::RefCell<Option<VirtualListHandle>>> =
            std::rc::Rc::new(std::cell::RefCell::new(None));
        let root = {
            let handle_slot = std::rc::Rc::clone(&handle_slot);
            move || {
                let (content, handle) = use_virtual_list(
                    "list",
                    item_count,
                    (),
                    ItemHeight::Fixed(item_height),
                    overscan,
                    Key::from,
                    row,
                );
                *handle_slot.borrow_mut() = Some(handle);
                Element::node(
                    "div",
                    vec![
                        ("id".to_string(), "list".to_string()),
                        ("class".to_string(), "viewport".to_string()),
                    ],
                    vec![content],
                )
            }
        };
        let css = format!(
            ".viewport {{ width: 100px; height: {viewport_height}px; overflow-y: auto; }} \
             .row {{ height: {item_height}px; }}"
        );
        let mut runtime =
            UiRuntime::new(&css, root, viewport()).expect("this test's own CSS always parses");
        // The list's own real viewport height is only known to
        // `use_committed_size` *after* this first render's layout commits
        // -- its `Signal::set` schedules a render that hasn't happened
        // yet, so every test needs this second, real pass before the
        // mounted window reflects real geometry (the same "an effect's
        // own state change needs a follow-up update" pattern this crate's
        // other real-runtime tests already rely on).
        runtime.update(viewport());
        (runtime, handle_slot)
    }

    fn list_node(arena: &Arena) -> NodeId {
        arena
            .find(|a, id| a.id_attr(id) == Some("list"))
            .expect("the virtualized list's own viewport must carry the id it registered with")
    }

    fn mounted_row_texts(runtime: &UiRuntime) -> Vec<String> {
        let (arena, ..) = runtime.geometry();
        arena
            .children(list_node(arena))
            .iter()
            .filter(|&&id| arena.classes(id).iter().any(|c| c == "row"))
            .map(|&id| arena.text_content(id).to_string())
            .collect()
    }

    type Keys = std::rc::Rc<std::cell::RefCell<Vec<usize>>>;

    /// A 20px-row list over a mutable list of keys, each row holding a
    /// button `b<key>` that can take focus, in a 100px viewport.
    fn focus_runtime(
        keys: &Keys,
        version: &std::rc::Rc<std::cell::Cell<u32>>,
    ) -> (
        UiRuntime,
        std::rc::Rc<std::cell::RefCell<Option<VirtualListHandle>>>,
    ) {
        let slot: std::rc::Rc<std::cell::RefCell<Option<VirtualListHandle>>> = Default::default();
        let slot_in = std::rc::Rc::clone(&slot);
        let (keys, version) = (std::rc::Rc::clone(keys), std::rc::Rc::clone(version));
        let root = move || {
            let items = keys.borrow().clone();
            let key_items = items.clone();
            let (content, handle) = use_virtual_list(
                "list",
                items.len(),
                version.get(),
                ItemHeight::Fixed(20.0),
                Overscan::Items(0),
                move |i| Key::from(key_items[i]),
                move |i| {
                    Element::node(
                        "div",
                        vec![("class".to_string(), "row".to_string())],
                        vec![Element::node(
                            "button",
                            vec![("id".to_string(), format!("b{}", items[i]))],
                            vec![Element::text(items[i].to_string())],
                        )],
                    )
                },
            );
            *slot_in.borrow_mut() = Some(handle);
            Element::node(
                "div",
                vec![
                    ("id".to_string(), "list".to_string()),
                    ("class".to_string(), "viewport".to_string()),
                ],
                vec![content],
            )
        };
        let css = ".viewport { width: 100px; height: 100px; overflow-y: auto; } \
                   .row { height: 20px; }";
        let mut runtime = UiRuntime::new(css, root, viewport()).expect("valid CSS");
        runtime.update(viewport());
        (runtime, slot)
    }

    fn button(runtime: &UiRuntime, key: usize) -> Option<NodeId> {
        let (arena, ..) = runtime.geometry();
        arena.find(|a, id| a.id_attr(id) == Some(format!("b{key}").as_str()))
    }

    fn settle(runtime: &mut UiRuntime) {
        for _ in 0..4 {
            runtime.update(viewport());
        }
    }

    #[test]
    fn a_focused_row_stays_mounted_when_it_scrolls_out_of_the_window() {
        let keys: Keys = std::rc::Rc::new(std::cell::RefCell::new((0..100).collect()));
        let (mut runtime, _) = focus_runtime(&keys, &std::rc::Rc::new(std::cell::Cell::new(0)));
        runtime.set_focused(button(&runtime, 2), true);
        settle(&mut runtime);

        assert!(runtime.scroll_registry().scroll_to("list", 0.0, 1200.0));
        settle(&mut runtime);

        assert!(
            button(&runtime, 2).is_some(),
            "the focused row is pinned outside the window"
        );
        assert_eq!(runtime.focused(), button(&runtime, 2), "and keeps focus");
        assert_eq!(
            runtime.scroll_registry().content_size("list").1,
            2000.0,
            "the pinned row sits at its own offset, so the scroll extent is unchanged"
        );
    }

    #[test]
    fn a_row_is_released_once_focus_leaves_it() {
        let keys: Keys = std::rc::Rc::new(std::cell::RefCell::new((0..100).collect()));
        let (mut runtime, _) = focus_runtime(&keys, &std::rc::Rc::new(std::cell::Cell::new(0)));
        runtime.set_focused(button(&runtime, 2), true);
        settle(&mut runtime);
        runtime.scroll_registry().scroll_to("list", 0.0, 1200.0);
        settle(&mut runtime);
        assert!(button(&runtime, 2).is_some());

        runtime.set_focused(None, true);
        settle(&mut runtime);
        assert!(
            button(&runtime, 2).is_none(),
            "an unfocused row outside the window unmounts"
        );
    }

    #[test]
    fn removing_the_focused_item_focuses_the_one_now_at_its_index() {
        let keys: Keys = std::rc::Rc::new(std::cell::RefCell::new((0..10).collect()));
        let version = std::rc::Rc::new(std::cell::Cell::new(0));
        let (mut runtime, _) = focus_runtime(&keys, &version);
        runtime.set_focused(button(&runtime, 2), true);
        settle(&mut runtime);

        keys.borrow_mut().remove(2);
        version.set(1);
        settle(&mut runtime);

        assert_eq!(runtime.focused(), button(&runtime, 3), "the next item");
    }

    #[test]
    fn removing_the_focused_last_item_focuses_the_new_last_one() {
        let keys: Keys = std::rc::Rc::new(std::cell::RefCell::new((0..4).collect()));
        let version = std::rc::Rc::new(std::cell::Cell::new(0));
        let (mut runtime, _) = focus_runtime(&keys, &version);
        runtime.set_focused(button(&runtime, 3), true);
        settle(&mut runtime);

        keys.borrow_mut().pop();
        version.set(1);
        settle(&mut runtime);

        assert_eq!(runtime.focused(), button(&runtime, 2));
    }

    #[test]
    fn focus_item_mounts_an_unmounted_item_and_focuses_it() {
        let keys: Keys = std::rc::Rc::new(std::cell::RefCell::new((0..100).collect()));
        let (mut runtime, handle) =
            focus_runtime(&keys, &std::rc::Rc::new(std::cell::Cell::new(0)));
        assert!(button(&runtime, 70).is_none(), "far outside the window");

        handle.borrow().as_ref().unwrap().focus_item(70);
        settle(&mut runtime);

        assert!(button(&runtime, 70).is_some());
        assert_eq!(runtime.focused(), button(&runtime, 70));
    }

    #[test]
    fn focus_item_on_a_key_that_is_not_there_does_nothing() {
        let keys: Keys = std::rc::Rc::new(std::cell::RefCell::new((0..10).collect()));
        let (mut runtime, handle) =
            focus_runtime(&keys, &std::rc::Rc::new(std::cell::Cell::new(0)));
        handle.borrow().as_ref().unwrap().focus_item(99);
        settle(&mut runtime);
        assert_eq!(runtime.focused(), None);
    }
    #[test]
    fn rows_carry_their_place_in_the_whole_collection() {
        let runtime = build_runtime(100, 20.0, 100.0, Overscan::Items(0));
        let (arena, ..) = runtime.geometry();
        let rows: Vec<_> = arena
            .children(list_node(arena))
            .iter()
            .copied()
            .filter(|&id| arena.classes(id).iter().any(|c| c == "row"))
            .collect();
        assert_eq!(
            arena.role(rows[0]),
            Some(florui_style::AccessibleRole::ListItem)
        );
        assert_eq!(arena.collection_position(rows[0]), Some((1, 100)));
        assert_eq!(arena.collection_position(rows[4]), Some((5, 100)));
    }
    #[test]
    fn only_the_visible_window_is_mounted() {
        // 100 rows of 20px inside a 100px-tall viewport -- exactly 5 fit.
        let runtime = build_runtime(100, 20.0, 100.0, Overscan::Items(0));
        assert_eq!(
            mounted_row_texts(&runtime),
            vec!["0", "1", "2", "3", "4"],
            "only the strictly visible rows should ever be mounted, not the other 95"
        );
    }

    #[test]
    fn overscan_extends_the_mounted_window_on_both_sides() {
        // Scroll to the middle of a long list so overscan has room to
        // extend on both sides, not just clamp against an edge.
        let item_height = 20.0;
        let viewport_height = 100.0;
        let mut runtime = build_runtime(100, item_height, viewport_height, Overscan::Items(2));
        let target_offset = 40.0 * item_height; // scroll so item 40 is the first visible row
        assert!(
            runtime
                .scroll_registry()
                .scroll_to("list", 0.0, target_offset),
            "scrolling to a real, different offset must report a change"
        );
        runtime.update(viewport());

        // Visible window without overscan would be items 40..45; overscan
        // of 2 extends it to 38..47.
        let expected: Vec<String> = (38..47).map(|i| i.to_string()).collect();
        assert_eq!(mounted_row_texts(&runtime), expected);
    }

    #[test]
    fn scrolling_changes_which_rows_are_mounted() {
        let item_height = 20.0;
        let mut runtime = build_runtime(100, item_height, 100.0, Overscan::Items(0));
        assert_eq!(mounted_row_texts(&runtime)[0], "0");

        assert!(
            runtime
                .scroll_registry()
                .scroll_to("list", 0.0, 10.0 * item_height)
        );
        runtime.update(viewport());

        assert_eq!(
            mounted_row_texts(&runtime),
            vec!["10", "11", "12", "13", "14"],
            "scrolling must dispose the old window's rows and mount the new one"
        );
    }

    #[test]
    fn scroll_to_item_moves_to_the_targets_real_position() {
        let item_height = 20.0;
        let (mut runtime, handle_slot) =
            build_runtime_with_handle(100, item_height, 100.0, Overscan::Items(0));
        let handle = handle_slot.borrow().clone().unwrap();

        handle.scroll_to_item(Key::from(42usize));
        runtime.update(viewport());

        assert_eq!(
            handle.offset(),
            (0.0, 42.0 * item_height),
            "scroll_to_item must resolve the target key's own real cumulative offset"
        );
        assert_eq!(mounted_row_texts(&runtime)[0], "42");
    }

    #[test]
    fn scroll_to_item_on_a_key_no_longer_present_is_a_silent_no_op() {
        let item_height = 20.0;
        let (mut runtime, handle_slot) =
            build_runtime_with_handle(100, item_height, 100.0, Overscan::Items(0));
        let handle = handle_slot.borrow().clone().unwrap();

        handle.scroll_to_item(Key::from("not-a-real-key"));
        runtime.update(viewport());

        assert_eq!(
            handle.offset(),
            (0.0, 0.0),
            "a key that was never in the dataset must not move the offset at all"
        );
    }

    #[test]
    fn an_empty_dataset_mounts_no_rows_and_does_not_panic() {
        let runtime = build_runtime(0, 20.0, 100.0, Overscan::Items(3));
        assert!(mounted_row_texts(&runtime).is_empty());
    }

    /// A dataset addressed by *stable ids*, not index -- unlike `row`'s
    /// plain `Key::from(i)` (fine for the tests above, where nothing ever
    /// reorders), this is what a real insertion/removal test needs:
    /// `key_for`/`render_item` must key and label by the item's own
    /// identity, so a later render with a different `item_count`/ordering
    /// can still recognize "the same item" by its stable id even though
    /// its own index shifted underneath it.
    fn build_runtime_with_stable_ids(
        ids: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
        version: std::rc::Rc<std::cell::Cell<u32>>,
        item_height: f32,
        viewport_height: f32,
    ) -> UiRuntime {
        let root = {
            let ids = std::rc::Rc::clone(&ids);
            let version = std::rc::Rc::clone(&version);
            move || {
                let snapshot = std::rc::Rc::new(ids.borrow().clone());
                let item_count = snapshot.len();
                let key_for = {
                    let snapshot = std::rc::Rc::clone(&snapshot);
                    move |i: usize| Key::from(snapshot[i].clone())
                };
                let render_item = {
                    let snapshot = std::rc::Rc::clone(&snapshot);
                    move |i: usize| {
                        Element::node(
                            "div",
                            vec![("class".to_string(), "row".to_string())],
                            vec![Element::text(snapshot[i].clone())],
                        )
                    }
                };
                let (content, _handle) = use_virtual_list(
                    "list",
                    item_count,
                    version.get(),
                    ItemHeight::Fixed(item_height),
                    Overscan::Items(0),
                    key_for,
                    render_item,
                );
                Element::node(
                    "div",
                    vec![
                        ("id".to_string(), "list".to_string()),
                        ("class".to_string(), "viewport".to_string()),
                    ],
                    vec![content],
                )
            }
        };
        let css = format!(
            ".viewport {{ width: 100px; height: {viewport_height}px; overflow-y: auto; }} \
             .row {{ height: {item_height}px; }}"
        );
        let mut runtime =
            UiRuntime::new(&css, root, viewport()).expect("this test's own CSS always parses");
        runtime.update(viewport());
        runtime
    }

    #[test]
    fn insertion_before_the_viewport_keeps_the_anchored_item_in_the_same_screen_position() {
        let item_height = 20.0;
        let ids = std::rc::Rc::new(std::cell::RefCell::new(
            (0..100).map(|i| format!("item-{i}")).collect::<Vec<_>>(),
        ));
        let version = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let mut runtime = build_runtime_with_stable_ids(
            std::rc::Rc::clone(&ids),
            std::rc::Rc::clone(&version),
            item_height,
            100.0,
        );

        // Scroll so "item-20" is the first mounted row.
        assert!(
            runtime
                .scroll_registry()
                .scroll_to("list", 0.0, 20.0 * item_height)
        );
        runtime.update(viewport());
        assert_eq!(mounted_row_texts(&runtime)[0], "item-20");

        // Insert 10 new items ahead of everything -- "item-20" is now at
        // index 30, but it's still the *same* item by its own stable id.
        {
            let mut ids = ids.borrow_mut();
            for i in (0..10).rev() {
                ids.insert(0, format!("new-{i}"));
            }
        }
        version.set(version.get() + 1);
        runtime.update(viewport());

        assert_eq!(
            mounted_row_texts(&runtime)[0],
            "item-20",
            "the anchored item must still be the first mounted row after an insertion ahead of it"
        );
        assert!(
            !runtime
                .scroll_registry()
                .scroll_to("list", 0.0, 30.0 * item_height),
            "the scroll offset must already have been corrected to item-20's new position -- \
             asking to scroll there again should be a no-op, not a real change"
        );
    }

    #[test]
    fn a_deleted_anchor_does_not_panic_and_leaves_the_offset_clamped() {
        let item_height = 20.0;
        let ids = std::rc::Rc::new(std::cell::RefCell::new(
            (0..100).map(|i| format!("item-{i}")).collect::<Vec<_>>(),
        ));
        let version = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let mut runtime = build_runtime_with_stable_ids(
            std::rc::Rc::clone(&ids),
            std::rc::Rc::clone(&version),
            item_height,
            100.0,
        );

        assert!(
            runtime
                .scroll_registry()
                .scroll_to("list", 0.0, 20.0 * item_height)
        );
        runtime.update(viewport());
        assert_eq!(mounted_row_texts(&runtime)[0], "item-20");

        // Delete exactly the anchored item -- `ScrollAnchor::resolve` has
        // nothing left to resolve against.
        {
            let mut ids = ids.borrow_mut();
            ids.remove(20);
        }
        version.set(version.get() + 1);
        runtime.update(viewport());

        // No fallback is chosen for a deleted anchor -- the offset is left
        // exactly as it was, and `ScrollRegistry::sync`'s own clamp is
        // still what protects it from pointing past the (now one item
        // shorter) real content.
        assert_eq!(mounted_row_texts(&runtime)[0], "item-21");
    }

    #[test]
    fn an_estimate_correcting_to_a_real_measurement_updates_the_total_scrollable_extent() {
        let estimate = 20.0;
        let real_first_row_height = 100.0;
        let handle_slot: std::rc::Rc<std::cell::RefCell<Option<VirtualListHandle>>> =
            std::rc::Rc::new(std::cell::RefCell::new(None));
        let root = {
            let handle_slot = std::rc::Rc::clone(&handle_slot);
            move || {
                let (content, handle) = use_virtual_list(
                    "list",
                    100,
                    (),
                    ItemHeight::Variable { estimate },
                    Overscan::Items(0),
                    Key::from,
                    |i| {
                        let class = if i == 0 { "tall-row" } else { "row" };
                        Element::node(
                            "div",
                            vec![("class".to_string(), class.to_string())],
                            vec![Element::text(i.to_string())],
                        )
                    },
                );
                *handle_slot.borrow_mut() = Some(handle);
                Element::node(
                    "div",
                    vec![
                        ("id".to_string(), "list".to_string()),
                        ("class".to_string(), "viewport".to_string()),
                    ],
                    vec![content],
                )
            }
        };
        let css = format!(
            ".viewport {{ width: 100px; height: 100px; overflow-y: auto; }} \
             .row {{ height: {estimate}px; }} \
             .tall-row {{ height: {real_first_row_height}px; }}"
        );
        let mut runtime =
            UiRuntime::new(&css, root, viewport()).expect("this test's own CSS always parses");
        // Settles across several real passes: the first mounts nothing
        // (the viewport's own real height isn't known yet), the second
        // mounts row 0 at its estimate for the first time (only *then*
        // does it get a real `use_committed_size` registration to
        // measure), and only the third actually renders against row 0's
        // corrected extent.
        for _ in 0..4 {
            runtime.update(viewport());
        }

        // Uncorrected total would be 100 * 20 = 2000 (max scroll 1900);
        // the real total is 99 * 20 + 100 = 2080 (max scroll 1980) --
        // scrolling far past either and reading back the *clamped* offset
        // distinguishes whether the correction actually reached the
        // scrollable extent, not just this list's own internal bookkeeping.
        runtime
            .scroll_registry()
            .scroll_to("list", 0.0, 1_000_000.0);
        runtime.update(viewport());
        let handle = handle_slot.borrow().clone().unwrap();
        assert_eq!(
            handle.offset(),
            (0.0, 1980.0),
            "the corrected (not estimated) total extent must be what scrolling clamps against"
        );
    }

    /// A variable-height list whose real rows are `real_height` tall against
    /// a 20px estimate, `overscan` rows mounted past the viewport either way.
    fn variable_runtime(
        real_height: f32,
        overscan: usize,
    ) -> (
        UiRuntime,
        std::rc::Rc<std::cell::RefCell<Option<VirtualListHandle>>>,
    ) {
        let slot: std::rc::Rc<std::cell::RefCell<Option<VirtualListHandle>>> = Default::default();
        let slot_in = std::rc::Rc::clone(&slot);
        let root = move || {
            let (content, handle) = use_virtual_list(
                "list",
                200,
                (),
                ItemHeight::Variable { estimate: 20.0 },
                Overscan::Items(overscan),
                Key::from,
                row,
            );
            *slot_in.borrow_mut() = Some(handle);
            Element::node(
                "div",
                vec![
                    ("id".to_string(), "list".to_string()),
                    ("class".to_string(), "viewport".to_string()),
                ],
                vec![content],
            )
        };
        let css = format!(
            ".viewport {{ width: 100px; height: 100px; overflow-y: auto; }} \
             .row {{ height: {real_height}px; }}"
        );
        let mut runtime = UiRuntime::new(&css, root, viewport()).expect("valid CSS");
        runtime.update(viewport());
        (runtime, slot)
    }

    #[test]
    fn rows_above_the_viewport_correcting_their_height_do_not_move_what_is_visible() {
        // Rows 48 and 49 mount as overscan above item 50 at their 20px
        // estimate and measure 40px: the visible top item must stay 50, so the
        // offset follows its new place: 48 estimated rows plus the 2 measured.
        let (mut runtime, handle) = variable_runtime(40.0, 2);
        runtime.scroll_registry().scroll_to("list", 0.0, 1000.0);
        settle(&mut runtime);
        let offset = handle.borrow().as_ref().unwrap().offset().1;
        assert_eq!(
            offset,
            48.0 * 20.0 + 2.0 * 40.0,
            "item 50 still starts at the top of the viewport"
        );
    }

    #[test]
    fn a_font_change_resizing_every_mounted_row_keeps_the_topmost_item_in_place() {
        let (mut runtime, handle) = variable_runtime(20.0, 0);
        runtime.scroll_registry().scroll_to("list", 0.0, 1000.0);
        settle(&mut runtime);
        assert_eq!(handle.borrow().as_ref().unwrap().offset().1, 1000.0);

        // A larger font: every row is now 30px. The mounted rows re-measure;
        // the top item (50) must not slide, so the offset stays at its start.
        let rules = florui_style::parse_stylesheet(
            ".viewport { width: 100px; height: 100px; overflow-y: auto; } \
             .row { height: 30px; }",
        )
        .unwrap();
        runtime.set_rules(rules);
        settle(&mut runtime);
        assert_eq!(handle.borrow().as_ref().unwrap().offset().1, 1000.0);
        assert_eq!(
            mounted_row_texts(&runtime).first().map(String::as_str),
            Some("50")
        );
    }
    #[test]
    fn rapid_scrolling_always_mounts_exactly_the_current_window_plus_overscan() {
        let item_height = 20.0;
        let mut runtime = build_runtime(1000, item_height, 100.0, Overscan::Items(2));
        // Five strictly visible rows, extended by 2 on each side once
        // there's room -- jump the scroll position around (not a smooth
        // sweep) the way a real fast fling would, and check every step
        // lands on exactly window+overscan, never a stale or partial set.
        for &start in &[0usize, 500, 50, 990, 200] {
            let target = (start as f32) * item_height;
            runtime.scroll_registry().scroll_to("list", 0.0, target);
            runtime.update(viewport());
            let expected: Vec<String> = (start.saturating_sub(2)..(start + 5 + 2).min(1000))
                .map(|i| i.to_string())
                .collect();
            assert_eq!(
                mounted_row_texts(&runtime),
                expected,
                "at scroll start {start}"
            );
        }
    }

    #[test]
    fn reordering_the_same_visible_keys_mounts_and_unmounts_nothing() {
        let log: std::rc::Rc<std::cell::RefCell<Vec<String>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let order: std::rc::Rc<std::cell::RefCell<Vec<usize>>> =
            std::rc::Rc::new(std::cell::RefCell::new(vec![0, 1, 2, 3, 4]));
        let root = {
            let log = std::rc::Rc::clone(&log);
            let order = std::rc::Rc::clone(&order);
            move || {
                let order = order.borrow().clone();
                let log = std::rc::Rc::clone(&log);
                let order_for_render = order.clone();
                let (content, _handle) = use_virtual_list(
                    "list",
                    5,
                    order.clone(),
                    ItemHeight::Fixed(20.0),
                    Overscan::Items(0),
                    move |i| Key::from(order[i]),
                    move |i| {
                        let real_key = order_for_render[i];
                        let log = std::rc::Rc::clone(&log);
                        florui_reactive::use_effect((), move || {
                            log.borrow_mut().push(format!("mount {real_key}"));
                            let log = std::rc::Rc::clone(&log);
                            Some(Box::new(move || {
                                log.borrow_mut().push(format!("unmount {real_key}"))
                            }) as florui_reactive::Cleanup)
                        });
                        row(real_key)
                    },
                );
                Element::node(
                    "div",
                    vec![
                        ("id".to_string(), "list".to_string()),
                        ("class".to_string(), "viewport".to_string()),
                    ],
                    vec![content],
                )
            }
        };
        let css = ".viewport { width: 100px; height: 100px; overflow-y: auto; } \
                    .row { height: 20px; }"
            .to_string();
        let mut runtime =
            UiRuntime::new(&css, root, viewport()).expect("this test's own CSS always parses");
        runtime.update(viewport());
        assert_eq!(
            log.borrow().as_slice(),
            &["mount 0", "mount 1", "mount 2", "mount 3", "mount 4"]
        );

        // Swaps the *middle* two items only -- the window's own first key
        // (index 0's own item, "0") stays exactly where it was. Moving it
        // instead would be a real identity change at the anchor position
        // itself, which this list's own anchor-preservation logic (see
        // the measurement-correction/insertion tests) *correctly* follows
        // by adjusting the scroll offset -- intentional, tested behavior
        // elsewhere, not something this test is about.
        log.borrow_mut().clear();
        *order.borrow_mut() = vec![0, 3, 2, 1, 4];
        runtime.update(viewport());
        assert!(
            log.borrow().is_empty(),
            "reordering keys within the same window, without moving the anchor, must not \
             mount or unmount any of them, got {:?}",
            log.borrow()
        );
    }

    #[test]
    fn a_resize_recomputes_the_mounted_window() {
        let item_height = 20.0;
        let mut runtime = build_runtime(100, item_height, 100.0, Overscan::Items(0));
        assert_eq!(mounted_row_texts(&runtime).len(), 5);

        let taller_rules = florui_style::parse_stylesheet(
            ".viewport { width: 100px; height: 240px; overflow-y: auto; } .row { height: 20px; }",
        )
        .expect("this test's own CSS always parses");
        runtime.set_rules(taller_rules);
        // Same one-render lag as everywhere else here: the first pass
        // after the rule change still renders against the *old* committed
        // height (`use_committed_size`'s own callback only fires, and
        // schedules a render, once *this* pass's real layout commits);
        // only the second pass actually sees it.
        runtime.update(viewport());
        runtime.update(viewport());

        assert_eq!(
            mounted_row_texts(&runtime).len(),
            12,
            "a taller real viewport must mount more rows, not keep the old window"
        );
    }

    #[test]
    fn unmounting_the_whole_list_disposes_every_currently_mounted_row() {
        let log: std::rc::Rc<std::cell::RefCell<Vec<String>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let show_list = std::rc::Rc::new(std::cell::Cell::new(true));
        let root = {
            let log = std::rc::Rc::clone(&log);
            let show_list = std::rc::Rc::clone(&show_list);
            move || {
                if !show_list.get() {
                    return Element::node("div", Vec::new(), Vec::new());
                }
                let log = std::rc::Rc::clone(&log);
                let (content, _handle) = use_virtual_list(
                    "list",
                    5,
                    (),
                    ItemHeight::Fixed(20.0),
                    Overscan::Items(0),
                    Key::from,
                    move |i| {
                        let log = std::rc::Rc::clone(&log);
                        florui_reactive::use_effect((), move || {
                            let log = std::rc::Rc::clone(&log);
                            Some(
                                Box::new(move || log.borrow_mut().push(format!("unmount {i}")))
                                    as florui_reactive::Cleanup,
                            )
                        });
                        row(i)
                    },
                );
                Element::node(
                    "div",
                    vec![
                        ("id".to_string(), "list".to_string()),
                        ("class".to_string(), "viewport".to_string()),
                    ],
                    vec![content],
                )
            }
        };
        let css = ".viewport { width: 100px; height: 100px; overflow-y: auto; } \
                    .row { height: 20px; }"
            .to_string();
        let mut runtime =
            UiRuntime::new(&css, root, viewport()).expect("this test's own CSS always parses");
        runtime.update(viewport());
        assert_eq!(mounted_row_texts(&runtime).len(), 5);

        show_list.set(false);
        runtime.update(viewport());

        let mut unmounted = log.borrow().clone();
        unmounted.sort();
        assert_eq!(
            unmounted,
            vec![
                "unmount 0",
                "unmount 1",
                "unmount 2",
                "unmount 3",
                "unmount 4"
            ],
            "every row mounted at the moment the whole list unmounts must dispose"
        );
    }

    #[test]
    fn ten_thousand_items_still_mount_only_the_window_plus_overscan() {
        let runtime = build_runtime(10_000, 20.0, 100.0, Overscan::Items(3));
        assert_eq!(
            mounted_row_texts(&runtime).len(),
            8,
            "5 strictly visible + 3 overscan, regardless of the dataset being 10,000 long"
        );
    }

    #[test]
    fn fixed_mode_never_measures_and_ignores_a_rows_real_rendered_size() {
        // Row 0 renders 100px tall for real, but Fixed(20.0) declares
        // every item 20px regardless -- if Fixed mode ever measured, the
        // total (and therefore the scroll clamp) would reflect the real
        // 100px; it must not.
        let handle_slot: std::rc::Rc<std::cell::RefCell<Option<VirtualListHandle>>> =
            std::rc::Rc::new(std::cell::RefCell::new(None));
        let root = {
            let handle_slot = std::rc::Rc::clone(&handle_slot);
            move || {
                let (content, handle) = use_virtual_list(
                    "list",
                    100,
                    (),
                    ItemHeight::Fixed(20.0),
                    Overscan::Items(0),
                    Key::from,
                    |i| {
                        let class = if i == 0 { "tall-row" } else { "row" };
                        Element::node(
                            "div",
                            vec![("class".to_string(), class.to_string())],
                            vec![Element::text(i.to_string())],
                        )
                    },
                );
                *handle_slot.borrow_mut() = Some(handle);
                Element::node(
                    "div",
                    vec![
                        ("id".to_string(), "list".to_string()),
                        ("class".to_string(), "viewport".to_string()),
                    ],
                    vec![content],
                )
            }
        };
        let css = ".viewport { width: 100px; height: 100px; overflow-y: auto; } \
                    .row { height: 20px; } .tall-row { height: 100px; }"
            .to_string();
        let mut runtime =
            UiRuntime::new(&css, root, viewport()).expect("this test's own CSS always parses");
        for _ in 0..4 {
            runtime.update(viewport());
        }

        runtime
            .scroll_registry()
            .scroll_to("list", 0.0, 1_000_000.0);
        runtime.update(viewport());
        let handle = handle_slot.borrow().clone().unwrap();
        let (x, y) = handle.offset();
        assert_eq!(x, 0.0);
        // Real Taffy layout rounds to whole pixels, so 100 rows declared
        // at exactly 20px each doesn't always land on exactly 2000px of
        // real content -- a few pixels of slack either way is that
        // rounding, not evidence either way about measurement. What this
        // test actually checks is the *order of magnitude*: if Fixed mode
        // ever measured the real 100px row, the clamp would land near
        // 1980 (2080 total - 100 viewport), not near 1900.
        assert!(
            (y - 1900.0).abs() < 10.0,
            "Fixed mode must clamp near the declared 20px-per-item total (~1900), \
             not near a real rendered size it never measured (~1980), got {y}"
        );
    }
}
