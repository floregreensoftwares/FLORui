//! [`Popover`]: a non-modal, anchor-tracked overlay, built on
//! [`crate::Portal`] like [`crate::components::dialog::Dialog`] — but never touches
//! [`crate::focus::modal_root`], so Tab/Shift+Tab are never trapped.
//!
//! Renders two divs inside `Portal`, not one: the outer is the arena's
//! own overlay root, which always lands at the viewport's origin `(0,0)`
//! regardless of document content (`florui_layout`'s own
//! `an_overlay_root_lays_out_at_the_viewport_origin_regardless_of_
//! document_content` test) — so it stays bare, and the *inner* div, an
//! ordinary `position: absolute` child of it, carries the computed inset
//! (resolved against that `(0,0)` origin, per `position_absolute_lands_
//! at_its_own_explicit_inset_offset`). Putting the inset directly on the
//! overlay root itself would rely on an untested, likely-false claim
//! (Taffy's root layout has no parent to resolve inset against).
//!
//! A freshly-opened popover's content size, and the trigger's own
//! position/size, aren't known until layout reports them back — so the
//! first render after opening is hidden (`opacity: 0`, not `visibility`,
//! which doesn't exist in this codebase) until all three are measured,
//! avoiding a one-frame flash at the wrong spot.
//!
//! `dismissed_by_click` checks the combined surface of every open
//! popover's content and trigger, not each pairwise against its own
//! subtree — pairwise wrongly closes a parent menu on a click inside its
//! own submenu, since a submenu's `Portal` content is a sibling overlay
//! root, not a descendant of the parent's arena node. `dismissed_by_
//! escape` closes only the *last* popover root in document order: a
//! nested submenu's own overlay root is extracted in a later pass than
//! its parent's (`florui_style::Arena::build`'s own doc), so it always
//! lands later in `arena.overlay_roots()` — "last" is the innermost/
//! most-recently-opened one.
//!
//! Not done here: focus-on-open, placements beyond flip/shift (no
//! `autoPlacement`, arrow element, or `size` middleware).

use florui::{Children, Element, Event, Handler, component};
use florui_reactive::use_signal;
use florui_style::{Arena, NodeId};

use crate::components::portal::{Portal, PortalProps};
use crate::position_observer::use_committed_position;
use crate::size_observer::use_committed_size;
use crate::viewport::use_viewport_size;

/// What [`popover_roots`] looks for — reserved, style the inner child
/// instead.
pub const POPOVER_ROOT_CLASS: &str = "florui-popover-root";
/// Unused by any lookup (pairing goes through the `id` suffix below) —
/// kept for app-side selectors, symmetric with [`POPOVER_ROOT_CLASS`].
pub const POPOVER_TRIGGER_CLASS: &str = "florui-popover-trigger";

/// Appended to a `Popover`'s `id` prop for its content root's own `id` —
/// [`trigger_for`] reverses this since `Arena` has no generic attribute
/// getter to pair them any other way.
const POPOVER_ROOT_ID_SUFFIX: &str = "-popover-root";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub side: Side,
    pub align: Align,
}

impl Placement {
    pub fn new(side: Side, align: Align) -> Self {
        Self { side, align }
    }
}

/// Renders `trigger` in place and, while `open`, `children` as an
/// anchor-tracked overlay via [`crate::Portal`].
#[component]
pub fn Popover(
    id: String,
    trigger: Element,
    open: bool,
    placement: Placement,
    ondismiss: Handler,
    children: Children,
) -> Element {
    let trigger_pos = use_signal(|| None::<(f32, f32)>);
    let trigger_size = use_signal(|| None::<(f32, f32)>);
    let content_size = use_signal(|| None::<(f32, f32)>);

    let content_id = format!("{id}-popover-content");
    let root_id = format!("{id}{POPOVER_ROOT_ID_SUFFIX}");

    {
        let trigger_pos = trigger_pos.clone();
        use_committed_position(id.clone(), move |x, y| trigger_pos.set(Some((x, y))));
    }
    {
        let trigger_size = trigger_size.clone();
        use_committed_size(id.clone(), move |w, h| trigger_size.set(Some((w, h))));
    }
    {
        let content_size = content_size.clone();
        use_committed_size(content_id.clone(), move |w, h| {
            content_size.set(Some((w, h)))
        });
    }

    let viewport = use_viewport_size();

    let style = match (trigger_pos.get(), trigger_size.get(), content_size.get()) {
        (Some((tx, ty)), Some((tw, th)), Some(size)) => {
            let (x, y) = resolve_placement(&placement, (tx, ty, tw, th), size, viewport);
            format!("position: absolute; top: {y}px; left: {x}px; opacity: 1;")
        }
        // Not all three measured yet -- stay hidden (see module doc).
        _ => "position: absolute; top: 0px; left: 0px; opacity: 0;".to_string(),
    };

    florui::view! {
        <div id={id.clone()} class={POPOVER_TRIGGER_CLASS}>
            {trigger}
            {open.then(move || florui::view! {
                <Portal>
                    <div
                        id={root_id.clone()}
                        class={POPOVER_ROOT_CLASS}
                        ondismiss={move || ondismiss.call(&Event::new())}
                    >
                        <div id={content_id.clone()} style={style.clone()}>
                            {children}
                        </div>
                    </div>
                </Portal>
            })}
        </div>
    }
}

/// Every open popover's own marker div, in document order.
pub(crate) fn popover_roots(arena: &Arena) -> Vec<NodeId> {
    arena.find_all(|arena, id| {
        arena
            .classes(id)
            .iter()
            .any(|class| class == POPOVER_ROOT_CLASS)
    })
}

/// `popover_root`'s own paired trigger, if its `id` still resolves to
/// one.
pub(crate) fn trigger_for(arena: &Arena, popover_root: NodeId) -> Option<NodeId> {
    let root_id = arena.id_attr(popover_root)?;
    let trigger_id = root_id.strip_suffix(POPOVER_ROOT_ID_SUFFIX)?;
    arena.find(|arena, id| arena.id_attr(id) == Some(trigger_id))
}

/// Each popover trigger's own control (its first focusable descendant) with
/// whether its popover is open, for the accessibility `expanded` state.
pub(crate) fn trigger_controls(arena: &Arena) -> Vec<(NodeId, bool)> {
    let open: Vec<NodeId> = popover_roots(arena)
        .into_iter()
        .filter_map(|root| trigger_for(arena, root))
        .collect();
    arena
        .find_all(|arena, id| {
            arena
                .classes(id)
                .iter()
                .any(|class| class == POPOVER_TRIGGER_CLASS)
        })
        .into_iter()
        .filter_map(|wrapper| {
            let control = crate::focus::focusable_within(arena, wrapper)
                .into_iter()
                .next()?;
            Some((control, open.contains(&wrapper)))
        })
        .collect()
}

/// Whether `node` is `root` itself or one of its descendants — no such
/// primitive exists on `Arena`, so this walks up via `parent` instead of
/// a downward DFS, since one membership check only needs one path.
pub(crate) fn is_self_or_descendant(arena: &Arena, root: NodeId, node: NodeId) -> bool {
    let mut current = Some(node);
    while let Some(id) = current {
        if id == root {
            return true;
        }
        current = arena.parent(id);
    }
    false
}

/// Which open popover roots a press at `hit` dismisses — see module doc
/// for why this checks the combined surface, not each pairwise.
pub(crate) fn dismissed_by_click(arena: &Arena, hit: Option<NodeId>) -> Vec<NodeId> {
    let roots = popover_roots(arena);
    if roots.is_empty() {
        return Vec::new();
    }
    let Some(hit) = hit else {
        // A press over nothing at all is unambiguously outside everything.
        return roots;
    };
    let inside_any = roots.iter().any(|&root| {
        is_self_or_descendant(arena, root, hit)
            || trigger_for(arena, root)
                .is_some_and(|trigger| is_self_or_descendant(arena, trigger, hit))
    });
    if inside_any { Vec::new() } else { roots }
}

/// The one popover root Escape closes (see module doc for why "last").
pub(crate) fn dismissed_by_escape(arena: &Arena) -> Option<NodeId> {
    popover_roots(arena).into_iter().last()
}

/// The content's final `(x, y)` for `placement`, given `trigger`'s
/// absolute box (`x, y, width, height`), the content's own size, and the
/// window's own size — flips to the opposite side on overflow (whichever
/// leaves more room if neither fits), then clamps both axes into the
/// viewport.
pub(crate) fn resolve_placement(
    placement: &Placement,
    trigger: (f32, f32, f32, f32),
    content_size: (f32, f32),
    viewport: (f32, f32),
) -> (f32, f32) {
    let (tx, ty, tw, th) = trigger;
    let (cw, ch) = content_size;
    let (vw, vh) = viewport;

    let side = match placement.side {
        Side::Top | Side::Bottom => {
            let space_above = ty;
            let space_below = vh - (ty + th);
            choose_side(
                placement.side,
                Side::Top,
                Side::Bottom,
                ch,
                space_above,
                space_below,
            )
        }
        Side::Left | Side::Right => {
            let space_left = tx;
            let space_right = vw - (tx + tw);
            choose_side(
                placement.side,
                Side::Left,
                Side::Right,
                cw,
                space_left,
                space_right,
            )
        }
    };

    let (x, y) = match side {
        Side::Top => (align_cross(placement.align, tx, tw, cw), ty - ch),
        Side::Bottom => (align_cross(placement.align, tx, tw, cw), ty + th),
        Side::Left => (tx - cw, align_cross(placement.align, ty, th, ch)),
        Side::Right => (tx + tw, align_cross(placement.align, ty, th, ch)),
    };

    // Snapped to a whole pixel, like Floating UI/Popper do: a half-pixel
    // `left` re-measures text at a fractional offset, which can shift its
    // shaped width by a pixel and feed back into a different half-pixel
    // `left` forever (confirmed live). Rounding breaks that loop.
    (clamp(x, vw, cw).round(), clamp(y, vh, ch).round())
}

/// `a`/`b` are a Top/Bottom or Left/Right pair. Keeps `preferred` if it
/// fits, flips to the other if only that one fits, else picks whichever
/// leaves more room.
fn choose_side(preferred: Side, a: Side, b: Side, main: f32, space_a: f32, space_b: f32) -> Side {
    let fits_a = space_a >= main;
    let fits_b = space_b >= main;
    if preferred == a {
        if fits_a || (!fits_b) { a } else { b }
    } else if fits_b || !fits_a {
        b
    } else {
        a
    }
}

/// The cross-axis coordinate for `align`, given the trigger's own extent
/// (`origin`, `length`) and the content's own `size` along that axis.
fn align_cross(align: Align, origin: f32, length: f32, size: f32) -> f32 {
    match align {
        Align::Start => origin,
        Align::Center => origin + length / 2.0 - size / 2.0,
        Align::End => origin + length - size,
    }
}

/// Shifts `value` into `[0, viewport_extent - content_extent]` —
/// `.max(0.0)` keeps the bound from going negative when content is
/// bigger than the viewport.
fn clamp(value: f32, viewport_extent: f32, content_extent: f32) -> f32 {
    let max = (viewport_extent - content_extent).max(0.0);
    value.clamp(0.0, max)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use florui::prelude::*;
    use taffy::{AvailableSpace, Size};

    use super::*;
    use crate::UiRuntime;

    fn arena_with(view: Element) -> Arena {
        Arena::build(&view)
    }

    fn viewport() -> Size<AvailableSpace> {
        Size {
            width: AvailableSpace::Definite(400.0),
            height: AvailableSpace::Definite(400.0),
        }
    }

    /// Mirrors the real `popover` example's own structure (a `.page` flex
    /// column, a `.menu` with `.menu-item-text` rows, a paragraph after
    /// it) closely enough for a bug that only shows up in that real
    /// composition to show up here too.
    fn menu_runtime(open: Rc<Cell<bool>>) -> UiRuntime {
        UiRuntime::new(
            ".page { display: flex; flex-direction: column; padding-top: 24px; \
                     padding-right: 24px; padding-bottom: 24px; padding-left: 24px; } \
             .action { padding-top: 10px; padding-right: 20px; padding-bottom: 10px; \
                       padding-left: 20px; } \
             .menu { display: flex; flex-direction: column; width: 180px; \
                     background-color: #26262c; border-width: 2px; border-style: solid; \
                     border-color: #4c4c56; } \
             .menu-item-text { margin-top: 0px; margin-bottom: 0px; padding-top: 8px; \
                                padding-right: 14px; padding-bottom: 8px; padding-left: 14px; } \
             .elsewhere { margin-top: 24px; padding-top: 40px; padding-bottom: 40px; }",
            move || {
                view! {
                    <div class="page">
                        <p>{"Instructions..."}</p>
                        <Popover
                            id={"menu".to_string()}
                            open={open.get()}
                            placement={Placement::new(Side::Bottom, Align::Start)}
                            ondismiss={Handler::new(|| {})}
                            trigger={view! {
                                <button id="trigger" class="action">{"Open menu"}</button>
                            }}
                        >
                            <div class="menu">
                                <p id="item-one" class="menu-item-text">{"Item one"}</p>
                                <p id="item-two" class="menu-item-text">{"Item two"}</p>
                            </div>
                        </Popover>
                        <p id="elsewhere" class="elsewhere">{"Click here to dismiss"}</p>
                    </div>
                }
            },
            viewport(),
        )
        .unwrap()
    }

    /// Same as [`menu_runtime`] but with a nested submenu `Popover` as
    /// the menu's own last row, matching the real example's own
    /// structure exactly (including `.menu-item`, the trigger class a
    /// flex column's default cross-axis stretch could widen unexpectedly).
    fn nested_menu_runtime(open: Rc<Cell<bool>>, submenu_open: Rc<Cell<bool>>) -> UiRuntime {
        UiRuntime::new(
            ".page { display: flex; flex-direction: column; padding-top: 24px; \
                     padding-right: 24px; padding-bottom: 24px; padding-left: 24px; } \
             .action { padding-top: 10px; padding-right: 20px; padding-bottom: 10px; \
                       padding-left: 20px; } \
             .menu { display: flex; flex-direction: column; width: 180px; \
                     padding-top: 6px; padding-bottom: 6px; background-color: #26262c; \
                     border-width: 2px; border-style: solid; border-color: #4c4c56; } \
             .menu-item-text { margin-top: 0px; margin-bottom: 0px; padding-top: 8px; \
                                padding-right: 14px; padding-bottom: 8px; padding-left: 14px; } \
             .menu-item { padding-top: 8px; padding-right: 14px; \
                          padding-bottom: 8px; padding-left: 14px; border-width: 0px; } \
             .elsewhere { margin-top: 24px; padding-top: 40px; padding-bottom: 40px; }",
            move || {
                view! {
                    <div class="page">
                        <p>{"Instructions..."}</p>
                        <Popover
                            id={"menu".to_string()}
                            open={open.get()}
                            placement={Placement::new(Side::Bottom, Align::Start)}
                            ondismiss={Handler::new(|| {})}
                            trigger={view! {
                                <button id="trigger" class="action">{"Open menu"}</button>
                            }}
                        >
                            <div id="parent-menu" class="menu">
                                <p id="item-one" class="menu-item-text">{"Item one"}</p>
                                <p id="item-two" class="menu-item-text">{"Item two"}</p>
                                <Popover
                                    id={"submenu".to_string()}
                                    open={submenu_open.get()}
                                    placement={Placement::new(Side::Right, Align::Start)}
                                    ondismiss={Handler::new(|| {})}
                                    trigger={view! {
                                        <button id="submenu-trigger" class="menu-item">
                                            {"Submenu"}
                                        </button>
                                    }}
                                >
                                    <div id="submenu-menu" class="menu">
                                        <p id="submenu-item" class="menu-item-text">
                                            {"Submenu item"}
                                        </p>
                                    </div>
                                </Popover>
                            </div>
                        </Popover>
                        <p id="elsewhere" class="elsewhere">{"Click here to dismiss"}</p>
                    </div>
                }
            },
            viewport(),
        )
        .unwrap()
    }

    #[test]
    fn a_submenu_trigger_row_stretches_to_match_its_sibling_rows_own_width() {
        // A submenu's own trigger wrapper is a plain block div, so it
        // naturally stretches to the menu's own flex-column width, same
        // as its sibling `<p>` rows -- this is what makes a `Side::Right`
        // submenu land flush against the panel's own edge instead of
        // overlapping it: anchoring against a *narrower*, shrink-to-fit
        // wrapper landed the submenu partway across the parent panel
        // instead of beside it (a real regression once tried, reverted).
        let open = Rc::new(Cell::new(false));
        let submenu_open = Rc::new(Cell::new(false));
        let mut runtime = nested_menu_runtime(Rc::clone(&open), Rc::clone(&submenu_open));
        open.set(true);
        runtime.update(viewport());
        runtime.update(viewport());

        let (arena, _, layouts) = runtime.geometry();
        let wrapper = arena
            .find(|a, n| a.id_attr(n) == Some("submenu"))
            .expect("the submenu's own trigger wrapper must be in the tree");
        let item_one = arena
            .find(|a, n| a.id_attr(n) == Some("item-one"))
            .expect("a sibling row must be in the tree");
        assert!(
            (layouts[&wrapper].width - layouts[&item_one].width).abs() < 1.0,
            "the trigger wrapper ({}) must match its sibling rows' own width ({}), not shrink \
             to just its own button",
            layouts[&wrapper].width,
            layouts[&item_one].width
        );
    }

    #[test]
    fn a_submenu_triggers_button_matches_its_sibling_rows_own_height() {
        // A bare `<button>` is `display: inline-block` by default. Its own
        // box used to come from raw text metrics alone (see
        // `florui_layout::measure_inline_block_intrinsic_size`'s own
        // fix) -- confirmed as a real bug by two buttons with *different*
        // declared padding coming out at the identical height, proving
        // padding wasn't contributing to it at all. This guards the fix.
        let open = Rc::new(Cell::new(false));
        let submenu_open = Rc::new(Cell::new(false));
        let mut runtime = nested_menu_runtime(Rc::clone(&open), Rc::clone(&submenu_open));
        open.set(true);
        runtime.update(viewport());
        runtime.update(viewport());

        let (arena, _, layouts) = runtime.geometry();
        let button = arena
            .find(|a, n| a.id_attr(n) == Some("submenu-trigger"))
            .unwrap();
        let item_one = arena.find(|a, n| a.id_attr(n) == Some("item-one")).unwrap();
        assert!(
            (layouts[&button].height - layouts[&item_one].height).abs() < 2.0,
            "the submenu trigger button ({}) must be as tall as its sibling rows ({}), \
             including its own padding",
            layouts[&button].height,
            layouts[&item_one].height
        );
    }

    #[test]
    fn a_submenu_lands_beside_its_parent_panel_not_overlapping_it() {
        let open = Rc::new(Cell::new(false));
        let submenu_open = Rc::new(Cell::new(false));
        let mut runtime = nested_menu_runtime(Rc::clone(&open), Rc::clone(&submenu_open));
        open.set(true);
        runtime.update(viewport());
        runtime.update(viewport());
        submenu_open.set(true);
        runtime.update(viewport());
        runtime.update(viewport());

        let (arena, _, layouts) = runtime.geometry();
        let menu = arena
            .find(|a, n| a.id_attr(n) == Some("parent-menu"))
            .expect("the parent menu's own div must be in the tree");
        let submenu_content = arena
            .find(|a, n| a.id_attr(n) == Some("submenu-popover-content"))
            .expect("the submenu's own content div must be in the tree");

        let menu_abs = florui_layout::absolute_position(arena, layouts, menu);
        let menu_right_edge = menu_abs.0 + layouts[&menu].width;
        let submenu_abs = florui_layout::absolute_position(arena, layouts, submenu_content);

        // The trigger row stretches to the menu's own *content* width,
        // landing flush with its inner border edge -- a few pixels short
        // of `menu`'s own border-inclusive outer width. Real "overlapping
        // most of the panel" (the regression this guards) is off by
        // dozens of pixels, not a border's worth.
        assert!(
            submenu_abs.0 >= menu_right_edge - 4.0,
            "the submenu ({}) must land at or beside the parent panel's own right edge ({}), \
             not overlapping it",
            submenu_abs.0,
            menu_right_edge
        );
    }

    #[test]
    fn a_menus_own_bottom_padding_survives_a_nested_submenu_as_its_last_row() {
        let open = Rc::new(Cell::new(false));
        let submenu_open = Rc::new(Cell::new(false));
        let mut runtime = nested_menu_runtime(Rc::clone(&open), Rc::clone(&submenu_open));
        open.set(true);
        runtime.update(viewport());
        runtime.update(viewport());
        submenu_open.set(true);
        runtime.update(viewport());
        runtime.update(viewport());

        let (arena, _, layouts) = runtime.geometry();
        let find = |id: &str| {
            arena
                .find(|a, n| a.id_attr(n) == Some(id))
                .unwrap_or_else(|| panic!("no node with id {id}"))
        };
        let menu = find("parent-menu");
        let submenu_trigger_row = find("submenu"); // the wrapper div, the real flex child
        let item_one = find("item-one");

        let menu_abs = florui_layout::absolute_position(arena, layouts, menu);
        let menu_box = layouts[&menu];
        let item_one_abs = florui_layout::absolute_position(arena, layouts, item_one);
        let row_abs = florui_layout::absolute_position(arena, layouts, submenu_trigger_row);
        let row_box = layouts[&submenu_trigger_row];

        assert_eq!(
            (item_one_abs.1 - menu_abs.1),
            (menu_abs.1 + menu_box.height) - (row_abs.1 + row_box.height),
            "top padding (menu top to item one) must equal bottom padding (last row's bottom \
             edge to menu bottom)"
        );
    }

    #[test]
    fn dismissed_by_escape_picks_the_submenu_not_the_parent_menu() {
        let open = Rc::new(Cell::new(false));
        let submenu_open = Rc::new(Cell::new(false));
        let mut runtime = nested_menu_runtime(Rc::clone(&open), Rc::clone(&submenu_open));
        open.set(true);
        runtime.update(viewport());
        runtime.update(viewport());
        submenu_open.set(true);
        runtime.update(viewport());
        runtime.update(viewport());

        let (arena, ..) = runtime.geometry();
        let closes = dismissed_by_escape(arena).expect("a popover root must be open");
        let trigger = trigger_for(arena, closes).expect("the closed root must have a trigger");
        assert_eq!(
            arena.id_attr(trigger),
            Some("submenu"),
            "Escape must close the innermost (submenu) popover, not the outer menu"
        );
    }

    #[test]
    fn a_submenus_overlay_root_paints_after_its_parent_menus_own() {
        let open = Rc::new(Cell::new(false));
        let submenu_open = Rc::new(Cell::new(false));
        let mut runtime = nested_menu_runtime(Rc::clone(&open), Rc::clone(&submenu_open));
        open.set(true);
        runtime.update(viewport());
        runtime.update(viewport());
        submenu_open.set(true);
        runtime.update(viewport());
        runtime.update(viewport());

        let (arena, ..) = runtime.geometry();
        let roots = popover_roots(arena);
        let parent_root = roots
            .iter()
            .find(|&&root| {
                trigger_for(arena, root).is_some_and(|t| arena.id_attr(t) == Some("menu"))
            })
            .expect("the parent menu's own overlay root must exist");
        let submenu_root = roots
            .iter()
            .find(|&&root| {
                trigger_for(arena, root).is_some_and(|t| arena.id_attr(t) == Some("submenu"))
            })
            .expect("the submenu's own overlay root must exist");
        let all_roots = arena.roots();
        let parent_index = all_roots.iter().position(|r| r == parent_root).unwrap();
        let submenu_index = all_roots.iter().position(|r| r == submenu_root).unwrap();
        assert!(
            submenu_index > parent_index,
            "the submenu's overlay root ({submenu_index}) must land after its parent menu's own \
             ({parent_index}) so it paints/hit-tests on top, not behind it"
        );
    }

    #[test]
    fn an_open_popovers_content_lands_directly_below_its_trigger_with_no_gap() {
        let open = Rc::new(Cell::new(false));
        let mut runtime = menu_runtime(Rc::clone(&open));
        open.set(true);
        // The first update mounts the content div with its size still
        // unknown (hidden); the second sees the size use_committed_size
        // reported after that first layout, and computes the real spot.
        runtime.update(viewport());
        runtime.update(viewport());

        let (arena, styles, layouts) = runtime.geometry();
        let find = |id: &str| {
            arena
                .find(|a, n| a.id_attr(n) == Some(id))
                .unwrap_or_else(|| panic!("no node with id {id}"))
        };
        let trigger = find("trigger");
        let item_one = find("item-one");
        let item_two = find("item-two");
        let menu = arena
            .find(|a, n| a.classes(n).iter().any(|c| c == "menu"))
            .expect("the menu div must be in the tree");

        let trigger_abs = florui_layout::absolute_position(arena, layouts, trigger);
        let menu_abs = florui_layout::absolute_position(arena, layouts, menu);
        let item_one_abs = florui_layout::absolute_position(arena, layouts, item_one);
        let item_two_abs = florui_layout::absolute_position(arena, layouts, item_two);

        assert_eq!(
            styles.get(&menu).map(|s| s.opacity),
            Some(1.0),
            "the content must be visible by the second update, not still hidden"
        );
        assert!(
            (menu_abs.1 - (trigger_abs.1 + layouts[&trigger].height)).abs() < 2.0,
            "the menu must land right below the trigger's own bottom edge"
        );
        assert_eq!(
            item_two_abs.1,
            item_one_abs.1 + layouts[&item_one].height,
            "item two must sit directly beneath item one, with no gap between them"
        );
    }

    #[test]
    fn a_popover_opened_via_real_hover_dispatch_settles_visible_and_positioned() {
        // Every other test in this module drives `open` via the external
        // `Cell` an `onclick` uses. This drives it through the real
        // `set_hovered`/`dispatch_event` path instead, confirming the
        // same two-update settle contract holds there too.
        let mut runtime = UiRuntime::new(
            ".trigger { padding-top: 8px; padding-right: 8px; padding-bottom: 8px; \
                        padding-left: 8px; } \
             .tooltip { padding-top: 4px; padding-right: 8px; padding-bottom: 4px; \
                        padding-left: 8px; }",
            || {
                let open = use_signal(|| false);
                let enter = open.clone();
                let leave_dismiss = open.clone();
                let leave_mouse = open.clone();
                view! {
                    <Popover
                        id={"tip".to_string()}
                        open={open.get()}
                        placement={Placement::new(Side::Bottom, Align::Center)}
                        ondismiss={Handler::new(move || leave_dismiss.set(false))}
                        trigger={view! {
                            <button
                                id="trigger"
                                class="trigger"
                                onmouseenter={move || enter.set(true)}
                                onmouseleave={move || leave_mouse.set(false)}
                            >
                                {"_"}
                            </button>
                        }}
                    >
                        <div id="tooltip-body" class="tooltip">{"Minimize"}</div>
                    </Popover>
                }
            },
            viewport(),
        )
        .unwrap();

        let trigger = {
            let (arena, ..) = runtime.geometry();
            arena
                .find(|a, n| a.id_attr(n) == Some("trigger"))
                .expect("the trigger button must be in the tree")
        };

        assert!(
            runtime.set_hovered(Some(trigger)),
            "hovering the trigger for the first time must register as a real change"
        );
        runtime.dispatch_event(trigger, "mouseenter");
        // Same two-update settle contract as every click-driven test above.
        runtime.update(viewport());
        runtime.update(viewport());

        let (arena, styles, layouts) = runtime.geometry();
        let tooltip_body = arena
            .find(|a, n| a.id_attr(n) == Some("tooltip-body"))
            .expect("the tooltip content must be in the tree once open");
        assert_eq!(
            styles.get(&tooltip_body).map(|s| s.opacity),
            Some(1.0),
            "the tooltip must be visible by the second update after a real mouseenter \
             dispatch, not stuck hidden"
        );
        assert!(
            layouts[&tooltip_body].width > 0.0 && layouts[&tooltip_body].height > 0.0,
            "the tooltip must have a real, nonzero measured size: {:?}",
            layouts[&tooltip_body]
        );

        // Real mouseleave must close it again, the same as a real cursor
        // leaving the trigger.
        assert!(runtime.set_hovered(None));
        runtime.dispatch_event(trigger, "mouseleave");
        runtime.update(viewport());

        let (arena, ..) = runtime.geometry();
        assert!(
            arena
                .find(|a, n| a.id_attr(n) == Some("tooltip-body"))
                .is_none(),
            "the tooltip content must be gone from the tree once closed again"
        );
    }

    /// Builds one hover-triggered tooltip popover, like
    /// `custom_titlebar.rs`'s own `caption_button` -- a plain function
    /// called several times in a fixed order, each constructing its own
    /// `Popover` from the same source line.
    fn hover_tooltip_button(id: &str, label: &str) -> Element {
        let open = use_signal(|| false);
        let enter = open.clone();
        let leave_dismiss = open.clone();
        let leave_mouse = open.clone();
        let trigger_id = format!("{id}-trigger");
        let body_id = format!("{id}-body");
        view! {
            <Popover
                id={id.to_string()}
                open={open.get()}
                placement={Placement::new(Side::Bottom, Align::Center)}
                ondismiss={Handler::new(move || leave_dismiss.set(false))}
                trigger={view! {
                    <button
                        id={trigger_id}
                        class="trigger"
                        onmouseenter={move || enter.set(true)}
                        onmouseleave={move || leave_mouse.set(false)}
                    >
                        {"_"}
                    </button>
                }}
            >
                <div id={body_id} class="tooltip">{label.to_string()}</div>
            </Popover>
        }
    }

    fn three_sibling_tooltips_runtime() -> UiRuntime {
        UiRuntime::new(
            ".row { display: flex; flex-direction: row; } \
             .trigger { width: 44px; height: 40px; } \
             .tooltip { padding-top: 4px; padding-right: 8px; padding-bottom: 4px; \
                        padding-left: 8px; }",
            || {
                view! {
                    <div class="row">
                        {hover_tooltip_button("minimize", "Minimize")}
                        {hover_tooltip_button("toggle-maximize", "Maximize")}
                        {hover_tooltip_button("close", "Close")}
                    </div>
                }
            },
            viewport(),
        )
        .unwrap()
    }

    #[test]
    fn each_sibling_tooltip_from_a_shared_helper_opens_independently() {
        // Three `Popover`s built from the same source line (a shared
        // helper called three times), as siblings. Hovering each must
        // show its own tooltip and label, not a stale sibling's.
        let mut runtime = three_sibling_tooltips_runtime();

        for (trigger_id, body_id, label) in [
            ("minimize-trigger", "minimize-body", "Minimize"),
            (
                "toggle-maximize-trigger",
                "toggle-maximize-body",
                "Maximize",
            ),
            ("close-trigger", "close-body", "Close"),
        ] {
            let trigger = {
                let (arena, ..) = runtime.geometry();
                arena
                    .find(|a, n| a.id_attr(n) == Some(trigger_id))
                    .unwrap_or_else(|| panic!("no trigger with id {trigger_id}"))
            };
            runtime.set_hovered(Some(trigger));
            runtime.dispatch_event(trigger, "mouseenter");
            runtime.update(viewport());
            runtime.update(viewport());

            let (arena, styles, layouts) = runtime.geometry();
            let body = arena
                .find(|a, n| a.id_attr(n) == Some(body_id))
                .unwrap_or_else(|| panic!("{body_id}'s own tooltip content must be in the tree"));
            assert_eq!(
                styles.get(&body).map(|s| s.opacity),
                Some(1.0),
                "{body_id} must be visible after its own hover settles"
            );
            assert!(
                layouts[&body].width > 0.0 && layouts[&body].height > 0.0,
                "{body_id} must have a real measured size: {:?}",
                layouts[&body]
            );
            assert_eq!(
                arena.text_content(body),
                label,
                "{body_id} must show its own label, not a sibling's"
            );

            runtime.set_hovered(None);
            runtime.dispatch_event(trigger, "mouseleave");
            runtime.update(viewport());
        }
    }

    /// Mirrors `DesktopHost::set_hovered_and_redraw`: reads `hovered()`
    /// before `set_hovered` overwrites it, with no manual by-`id`
    /// re-lookup — unlike the sibling test above, this exercises
    /// `UiRuntime`'s own hover-identity bookkeeping across a rebuild.
    fn hover_via_runtime_state(runtime: &mut UiRuntime, hit: Option<NodeId>) {
        let previous = runtime.hovered();
        if !runtime.set_hovered(hit) {
            return;
        }
        if let Some(node) = previous {
            runtime.dispatch_event(node, "mouseleave");
        }
        if let Some(node) = hit {
            runtime.dispatch_event(node, "mouseenter");
        }
        runtime.update(viewport());
    }

    #[test]
    fn moving_hover_between_siblings_via_runtime_hovered_shows_only_the_current_ones_tooltip() {
        // Each sibling's tooltip content only exists while its `Popover`
        // is open, so opening/closing one reshapes the arena every
        // iteration -- exactly the condition `resolve_hover` exists to
        // survive.
        let mut runtime = three_sibling_tooltips_runtime();

        for (trigger_id, body_id, label) in [
            ("minimize-trigger", "minimize-body", "Minimize"),
            (
                "toggle-maximize-trigger",
                "toggle-maximize-body",
                "Maximize",
            ),
            ("close-trigger", "close-body", "Close"),
        ] {
            let trigger = {
                let (arena, ..) = runtime.geometry();
                arena
                    .find(|a, n| a.id_attr(n) == Some(trigger_id))
                    .unwrap_or_else(|| panic!("no trigger with id {trigger_id}"))
            };
            hover_via_runtime_state(&mut runtime, Some(trigger));
            // Second update to let the newly opened tooltip's position/
            // size measurement settle, same contract as every other test
            // in this module.
            runtime.update(viewport());

            let (arena, styles, layouts) = runtime.geometry();
            let body = arena
                .find(|a, n| a.id_attr(n) == Some(body_id))
                .unwrap_or_else(|| panic!("{body_id}'s own tooltip content must be in the tree"));
            assert_eq!(
                styles.get(&body).map(|s| s.opacity),
                Some(1.0),
                "{body_id} must be visible after its own hover settles"
            );
            assert!(
                layouts[&body].width > 0.0 && layouts[&body].height > 0.0,
                "{body_id} must have a real measured size: {:?}",
                layouts[&body]
            );
            assert_eq!(
                arena.text_content(body),
                label,
                "{body_id} must show its own label, not a sibling's"
            );

            for (other_body_id, _) in [
                ("minimize-body", "Minimize"),
                ("toggle-maximize-body", "Maximize"),
                ("close-body", "Close"),
            ] {
                if other_body_id != body_id {
                    assert!(
                        arena
                            .find(|a, n| a.id_attr(n) == Some(other_body_id))
                            .is_none(),
                        "{other_body_id}'s tooltip must not be showing while {body_id} is hovered"
                    );
                }
            }
        }

        hover_via_runtime_state(&mut runtime, None);
        let (arena, ..) = runtime.geometry();
        for body_id in ["minimize-body", "toggle-maximize-body", "close-body"] {
            assert!(
                arena.find(|a, n| a.id_attr(n) == Some(body_id)).is_none(),
                "{body_id}'s tooltip must be gone once the cursor leaves every trigger"
            );
        }
    }

    #[test]
    fn popover_roots_finds_every_open_popovers_own_marker_div() {
        let tree: Element = view! {
            <div>
                <div id="a-popover-root" class={POPOVER_ROOT_CLASS} />
                <div id="b-popover-root" class={POPOVER_ROOT_CLASS} />
                <div id="unrelated" />
            </div>
        };
        let arena = arena_with(tree);
        assert_eq!(popover_roots(&arena).len(), 2);
    }

    #[test]
    fn trigger_for_resolves_the_paired_trigger_by_id_suffix() {
        let tree: Element = view! {
            <div>
                <button id="menu">{"Open"}</button>
                <div id="menu-popover-root" class={POPOVER_ROOT_CLASS} />
            </div>
        };
        let arena = arena_with(tree);
        let root = popover_roots(&arena)[0];
        let trigger = trigger_for(&arena, root).expect("trigger must resolve");
        assert_eq!(arena.tag(trigger), "button");
    }

    #[test]
    fn trigger_for_is_none_when_no_matching_trigger_id_exists() {
        let tree: Element = view! {
            <div id="menu-popover-root" class={POPOVER_ROOT_CLASS} />
        };
        let arena = arena_with(tree);
        let root = popover_roots(&arena)[0];
        assert_eq!(trigger_for(&arena, root), None);
    }

    #[test]
    fn dismissed_by_click_is_empty_when_the_hit_is_inside_the_popovers_own_content() {
        let tree: Element = view! {
            <div>
                <button id="menu">{"Open"}</button>
                <div id="menu-popover-root" class={POPOVER_ROOT_CLASS}>
                    <button id="item">{"Item"}</button>
                </div>
            </div>
        };
        let arena = arena_with(tree);
        let item = arena.find(|a, id| a.id_attr(id) == Some("item")).unwrap();
        assert_eq!(dismissed_by_click(&arena, Some(item)), Vec::<NodeId>::new());
    }

    #[test]
    fn dismissed_by_click_is_empty_when_the_hit_is_on_the_popovers_own_trigger() {
        let tree: Element = view! {
            <div>
                <button id="menu">{"Open"}</button>
                <div id="menu-popover-root" class={POPOVER_ROOT_CLASS} />
            </div>
        };
        let arena = arena_with(tree);
        let trigger = arena.find(|a, id| a.id_attr(id) == Some("menu")).unwrap();
        assert_eq!(
            dismissed_by_click(&arena, Some(trigger)),
            Vec::<NodeId>::new()
        );
    }

    #[test]
    fn dismissed_by_click_is_empty_when_the_hit_is_inside_a_nested_submenus_own_content() {
        // A submenu's Portal content is a sibling overlay root, never a
        // descendant of the parent's own arena node -- this is the
        // regression test for the pairwise-check bug this module's own
        // doc calls out.
        let tree: Element = view! {
            <div>
                <button id="parent">{"Parent"}</button>
                <div id="parent-popover-root" class={POPOVER_ROOT_CLASS}>
                    <button id="child">{"Child"}</button>
                </div>
                <div id="child-popover-root" class={POPOVER_ROOT_CLASS}>
                    <button id="submenu-item">{"Submenu item"}</button>
                </div>
            </div>
        };
        let arena = arena_with(tree);
        let submenu_item = arena
            .find(|a, id| a.id_attr(id) == Some("submenu-item"))
            .unwrap();
        assert_eq!(
            dismissed_by_click(&arena, Some(submenu_item)),
            Vec::<NodeId>::new(),
            "a click inside a nested submenu's own content must not dismiss the parent menu"
        );
    }

    #[test]
    fn dismissed_by_click_dismisses_every_open_popover_on_a_genuinely_outside_hit() {
        let tree: Element = view! {
            <div>
                <button id="menu">{"Open"}</button>
                <div id="menu-popover-root" class={POPOVER_ROOT_CLASS} />
                <div id="elsewhere" />
            </div>
        };
        let arena = arena_with(tree);
        let elsewhere = arena
            .find(|a, id| a.id_attr(id) == Some("elsewhere"))
            .unwrap();
        let root = popover_roots(&arena)[0];
        assert_eq!(dismissed_by_click(&arena, Some(elsewhere)), vec![root]);
    }

    #[test]
    fn dismissed_by_click_dismisses_everything_when_the_hit_is_none() {
        let tree: Element = view! {
            <div id="menu-popover-root" class={POPOVER_ROOT_CLASS} />
        };
        let arena = arena_with(tree);
        let root = popover_roots(&arena)[0];
        assert_eq!(dismissed_by_click(&arena, None), vec![root]);
    }

    #[test]
    fn dismissed_by_click_is_empty_without_any_open_popover() {
        let tree: Element = view! { <div id="elsewhere" /> };
        let arena = arena_with(tree);
        let elsewhere = arena
            .find(|a, id| a.id_attr(id) == Some("elsewhere"))
            .unwrap();
        assert_eq!(
            dismissed_by_click(&arena, Some(elsewhere)),
            Vec::<NodeId>::new()
        );
    }

    #[test]
    fn dismissed_by_escape_picks_the_last_in_document_order() {
        let tree: Element = view! {
            <div>
                <div id="a-popover-root" class={POPOVER_ROOT_CLASS} />
                <div id="b-popover-root" class={POPOVER_ROOT_CLASS} />
            </div>
        };
        let arena = arena_with(tree);
        let last = popover_roots(&arena)[1];
        assert_eq!(dismissed_by_escape(&arena), Some(last));
    }

    #[test]
    fn dismissed_by_escape_is_none_without_any_open_popover() {
        let tree: Element = view! { <div /> };
        let arena = arena_with(tree);
        assert_eq!(dismissed_by_escape(&arena), None);
    }

    #[test]
    fn resolve_placement_keeps_the_preferred_side_when_it_fits() {
        let placement = Placement::new(Side::Bottom, Align::Start);
        let (x, y) = resolve_placement(
            &placement,
            (10.0, 10.0, 50.0, 20.0),
            (100.0, 40.0),
            (500.0, 500.0),
        );
        assert_eq!((x, y), (10.0, 30.0));
    }

    #[test]
    fn resolve_placement_flips_to_the_opposite_side_when_the_preferred_side_overflows_the_viewport()
    {
        let placement = Placement::new(Side::Bottom, Align::Start);
        // Trigger near the bottom edge of a 200px-tall viewport, content
        // needs 100px -- Bottom has no room, Top does.
        let (_, y) = resolve_placement(
            &placement,
            (10.0, 170.0, 50.0, 20.0),
            (60.0, 100.0),
            (500.0, 200.0),
        );
        assert_eq!(
            y, 70.0,
            "must flip above the trigger (170 - 100) when below overflows"
        );
    }

    #[test]
    fn resolve_placement_shifts_along_the_cross_axis_to_stay_inside_the_viewport() {
        let placement = Placement::new(Side::Bottom, Align::Start);
        // Trigger near the right edge -- content-start-aligned would run
        // off the right edge of a 200px-wide viewport.
        let (x, _) = resolve_placement(
            &placement,
            (180.0, 10.0, 20.0, 20.0),
            (100.0, 30.0),
            (200.0, 500.0),
        );
        assert_eq!(
            x, 100.0,
            "must shift left to stay within [0, viewport_width - content_width]"
        );
    }

    #[test]
    fn resolve_placement_center_aligns_along_the_cross_axis() {
        let placement = Placement::new(Side::Bottom, Align::Center);
        let (x, _) = resolve_placement(
            &placement,
            (100.0, 10.0, 40.0, 20.0),
            (20.0, 10.0),
            (500.0, 500.0),
        );
        // trigger center = 100 + 40/2 = 120; content half-width = 10 -> x = 110
        assert_eq!(x, 110.0);
    }

    #[test]
    fn resolve_placement_rounds_a_half_pixel_result_to_a_whole_pixel() {
        // Real values from a live caption-button tooltip: an odd-width
        // content box (67px) centered under a 44px trigger lands on a
        // half pixel (660 + 44/2 - 67/2 = 648.5) -- left unrounded, this
        // fed back into an infinite re-layout loop, because a half-pixel
        // `left` reshapes the text it positions, which can shift that
        // text's own measured width by a pixel, changing `left` again,
        // forever.
        let placement = Placement::new(Side::Bottom, Align::Center);
        let (x, _) = resolve_placement(
            &placement,
            (660.0, 0.0, 44.0, 40.0),
            (67.0, 24.0),
            (800.0, 600.0),
        );
        assert_eq!(
            x, 649.0,
            "648.5 must round to a whole pixel, not stay fractional"
        );
    }

    #[test]
    fn resolve_placement_picks_whichever_side_has_more_room_when_neither_fits() {
        let placement = Placement::new(Side::Bottom, Align::Start);
        // 50px-tall viewport, content needs 200px -- neither Top (10px
        // of room above the trigger) nor Bottom (30px below) truly
        // fits, so the side with more room (Bottom) wins, then clamps.
        let (_, y) = resolve_placement(
            &placement,
            (0.0, 10.0, 10.0, 10.0),
            (10.0, 200.0),
            (500.0, 50.0),
        );
        assert_eq!(
            y, 0.0,
            "clamped into the viewport after picking the side with more room"
        );
    }
}
