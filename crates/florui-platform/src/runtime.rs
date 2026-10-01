//! [`UiRuntime`]: the window-independent half of a running Florui tree.
//!
//! Renders on demand against whatever viewport a host supplies, caches
//! the resulting geometry, and answers hit tests and click dispatch
//! against that cache — a host never has to render again just to know
//! what is under a point. Nothing here knows about `winit`, a window, or
//! any other platform type; a game or mobile host could drive this the
//! same way [`crate::run`] does.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use florui::{Element, Event};
use florui_layout::BoxLayout;
use florui_reactive::executor::{Executor, LocalExecutor};
use florui_reactive::{ComponentScope, DirtyFlag, FocusHost, provide_context};
use florui_style::{
    AnimationTimeline, Arena, ComputedStyle, FocusPath, InteractionState, NodeId, Rule, StyleError,
};
use taffy::prelude::*;

use crate::focus;
use crate::focus_observer::{FocusController, FocusObserverRegistry};
use crate::list_keys::{ListKey, ListKeyRegistry};
use crate::menu_keys;
use crate::position_observer::PositionObserverRegistry;
use crate::scroll::ScrollRegistry;
use crate::size_observer::SizeObserverRegistry;
use crate::text_input::TextInputRegistry;
use crate::viewport::ViewportSize;
use crate::visited_links::VisitedLinks;

/// A validation message and the control it hangs from.
pub(crate) struct ValidationBubble {
    pub field: FocusPath,
    pub message: String,
}

pub struct UiRuntime {
    scope: ComponentScope,
    dirty: DirtyFlag,
    rules: Vec<Rule>,
    root: Box<dyn Fn() -> Element>,
    interaction: InteractionState,
    /// Feeds `:link`/`:visited` into every [`Self::rebuild_interaction`]
    /// call — a fresh, never-shared, always-empty default here (matching
    /// this whole constructor's non-desktop callers: devtools, benches,
    /// most tests). [`crate::desktop::DesktopHost`] replaces it with the
    /// real, process-wide shared one via [`Self::set_visited_links`],
    /// right after construction — the same "provided afterward, not a
    /// constructor argument" shape [`Self::on_needs_update`] already has,
    /// not [`Self::with_rules_and_context`]'s own `extra_context_providers`
    /// shape, since this is runtime-internal state `rebuild_interaction`
    /// consults directly, not something component code reaches via
    /// `use_context`.
    visited_links: VisitedLinks,
    /// [`Self::hovered_path`] resolved against the current arena — same
    /// contract as [`Self::focused_node`].
    hovered: Option<NodeId>,
    /// Persistent identity for the hovered element, same as
    /// [`Self::focused_path`] and for the same reason: a plain [`NodeId`]
    /// doesn't survive a rebuild. `None` means nothing is hovered.
    hovered_path: Option<FocusPath>,
    /// Persistent across renders — see [`FocusPath`]'s own doc for why a
    /// plain [`NodeId`] can't fill this role. `None` means nothing is
    /// focused.
    focused_path: Option<FocusPath>,
    /// [`Self::focused_path`] resolved against the current [`Self::arena`]
    /// — valid only within the arena generation it was resolved in, the
    /// same contract [`Self::hovered`] already has.
    focused_node: Option<NodeId>,
    /// Whether the current focus is keyboard-driven (`:focus-visible`
    /// should match) rather than a mouse click.
    focus_visible: bool,
    /// Where to restore focus as each open modal
    /// [`crate::components::dialog::Dialog`] closes, outermost first — one
    /// entry captured the render a modal opens (`None` if nothing was
    /// focused) and consumed the render it closes. Its length is also how
    /// many modals were open as of the previous render, the authoritative
    /// "did one just open/close" signal. See [`Self::resolve_focus`].
    modal_return_paths: Vec<Option<FocusPath>>,
    /// The same for open menu popovers, see [`Self::resolve_menu_focus`].
    menu_return_paths: Vec<Option<FocusPath>>,
    /// The range input a pointer drag is currently moving, if any — a
    /// [`FocusPath`], not a plain [`NodeId`], because an accepted drag
    /// value re-renders (a fresh [`Self::arena`]) *while the drag is still
    /// open*, unlike a single atomic click; each of
    /// [`Self::continue_range_drag`]/[`Self::end_range_drag`] re-resolves
    /// it against the current arena on every call rather than caching a
    /// resolved id, and simply ends the drag if it no longer resolves (no
    /// invented fallback, matching [`Self::resolve_against`]'s own
    /// precedent).
    range_dragging: Option<FocusPath>,
    arena: Arena,
    styles: HashMap<NodeId, ComputedStyle>,
    layouts: HashMap<NodeId, BoxLayout>,
    /// Every select's own options, by the select's `id` — see
    /// [`crate::select::normalize`]'s own doc for why this exists
    /// alongside `arena` rather than just reading options off it.
    option_summaries: HashMap<String, Vec<crate::select::OptionSummary>>,
    /// The `(select id, option value)` a `<select multiple>`'s last
    /// plain/Ctrl click landed on — the anchor a following Shift-click
    /// range-selects from, real HTML's own multi-select semantics. Plain
    /// strings, not a [`FocusPath`]: unlike `range_dragging`, nothing
    /// here needs re-resolving against a fresh `Arena`.
    multiselect_anchor: Option<(String, String)>,
    /// Text controls the user has typed into, by [`FocusPath`] — see
    /// [`crate::form_control::FormContext::edited`]. Interior-mutable
    /// because [`Self::commit_value`] records into it through `&self`.
    edited: RefCell<HashSet<FocusPath>>,
    /// Controls whose `:user-valid`/`:user-invalid` may match: edited
    /// and then blurred, or their form was submitted.
    user_validated: HashSet<FocusPath>,
    /// The validation message showing under a control, if any — see
    /// [`crate::validation_bubble`]. Interior-mutable because an edit to
    /// its control ([`Self::commit_value`]) dismisses it through `&self`.
    validation_bubble: RefCell<Option<ValidationBubble>>,
    /// The content-box size the user dragged each textarea to, by its `id`;
    /// see [`crate::textarea_resize`].
    resized: RefCell<HashMap<String, (f32, f32)>>,
    /// Carries real `transition`/`@keyframes` state across [`Self::update`]
    /// calls, sampled against a real wall clock captured once at
    /// [`Self::with_rules_and_context`] — see
    /// [`florui_style::AnimationTimeline`]'s own doc.
    animation_timeline: AnimationTimeline,
    animation_epoch: std::time::Instant,
    /// When set, animations read this many seconds instead of the wall clock.
    clock_override: Option<f64>,
    /// The one long-lived font this runtime's own [`Self::update`] lays out
    /// with — loading one builds a whole Parley `FontContext` (and, with
    /// fontique's default `system_fonts: true`, enumerates the system's
    /// installed fonts), too expensive to redo every render. A host that
    /// also paints borrows it back out via [`Self::geometry_and_font_mut`]
    /// so painting shapes and rasterizes the identical glyphs this runtime
    /// already measured, rather than a second, separately-loaded instance.
    font: florui_text::Font,
    /// Reachable by [`florui_reactive::use_resource`] via context, provided
    /// fresh every render the same way any other context value is. Advanced
    /// once per [`Self::update`] so a fetch that's already resolvable (or
    /// was woken by prior progress) commits without a host needing to know
    /// that async work is involved at all. A background completion woken
    /// only through this executor's own waker (real I/O, a timer) reaches
    /// an event-driven host via [`Self::on_needs_update`], the same
    /// callback a [`florui_reactive::Signal::set`] anywhere under the root
    /// already uses.
    executor: Rc<LocalExecutor>,
    /// Reachable by [`crate::use_committed_size`] via context, the same
    /// way `executor` is. Notified after each [`Self::update`]'s own
    /// layout, once real geometry for that render exists.
    size_observers: Rc<SizeObserverRegistry>,
    /// Reachable by [`crate::use_committed_position`] via context, same
    /// as `size_observers` — notified right alongside it.
    position_observers: Rc<PositionObserverRegistry>,
    /// Reachable by [`crate::use_focus_within`] and
    /// [`crate::use_focus_controller`] via context; observers are notified
    /// after layout, requests consumed by [`Self::resolve_focus`].
    focus_observers: Rc<FocusObserverRegistry>,
    focus_controller: Rc<FocusController>,
    /// Lists that take Arrow/Page/Home/End; see [`Self::list_key`].
    list_keys: Rc<ListKeyRegistry>,
    /// Reachable by [`crate::use_scroll_offset`] via context, the same way
    /// `size_observers` is — synced right after it, once this render's own
    /// real layout and content extents exist.
    scroll_registry: Rc<ScrollRegistry>,
    /// Real editing state (caret/selection/undo-redo) for every currently
    /// editable `<input>` — never reachable via `use_context`, unlike
    /// every other registry here: nothing inside a render ever needs it,
    /// only [`crate::desktop::DesktopHost`]'s own keyboard/mouse handlers
    /// and paint (via [`Self::text_input_registry`]). Synced structurally
    /// in [`Self::update`], not via `use_attachment` — see
    /// [`crate::text_input`]'s own module doc for why a bare `<input>` has
    /// no hook call-site to register through.
    text_input_registry: Rc<TextInputRegistry>,
    /// Decoded/loading/failed state for every currently-live `<img>`,
    /// keyed structurally (an `<img>` almost never has an `id`, unlike
    /// `text_input_registry`'s own inputs) — synced structurally in
    /// [`Self::update`], same reason as `text_input_registry`. Never
    /// reachable via `use_context`; only [`Self::fix_image_intrinsic_sizes`]
    /// and paint (via [`Self::image_registry`]) read it.
    image_registry: Rc<crate::image::ImageRegistry>,
    /// Same shape as `image_registry`, for `<icon>` instead — see
    /// `crate::icon`'s own module doc for why a themable icon is a
    /// separate tag/registry rather than an `<img>` variant.
    icon_registry: Rc<crate::icon::IconRegistry>,
    /// Backs `image_registry`'s and `icon_registry`'s own decoding —
    /// shared so a `src` reused by multiple elements (or reloaded after
    /// this same render previously loaded it) decodes once. See
    /// `florui_assets::AssetCache`'s own doc for its retention policy.
    asset_cache: Arc<florui_assets::AssetCache>,
    /// Extra `provide_context` calls a host supplied at construction — run
    /// every [`Self::update`] (including the very first one, inside
    /// [`Self::with_rules`] itself) alongside `executor`/`size_observers`,
    /// without this window-independent runtime having to know what any of
    /// them actually are. [`crate::desktop::DesktopHost`] uses this to
    /// make its own window-specific capabilities (`WindowControls`)
    /// reachable from components from the very first render onward, the
    /// same way `size_observers` already is for a capability this crate
    /// owns directly.
    extra_context_providers: Vec<Box<dyn Fn()>>,
}

/// One keyboard step for a focused range input — see
/// [`UiRuntime::step_range_value`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RangeStep {
    SmallDecrement,
    SmallIncrement,
    LargeDecrement,
    LargeIncrement,
    Min,
    Max,
}

impl UiRuntime {
    /// Parses `css` and renders `root` once against `viewport`, so a
    /// freshly constructed runtime always has real geometry ready.
    pub fn new(
        css: &str,
        root: impl Fn() -> Element + 'static,
        viewport: Size<AvailableSpace>,
    ) -> Result<Self, StyleError> {
        let rules = florui_style::parse_stylesheet(css)?;
        Ok(Self::with_rules(rules, root, viewport))
    }

    /// Same as [`Self::new`], but for a host that already parsed its
    /// stylesheet (typically to fail fast before opening a window) and
    /// doesn't want to parse it again just to build the runtime. No real
    /// OS accessibility signal reaches this path (devtools, benches, and
    /// most tests use it) — `respect_reduced_motion` stays at its
    /// documented default (`true`) and the initial OS-preference read is
    /// `false` (no preference), matching the deterministic behavior every
    /// non-desktop caller already relies on.
    pub(crate) fn with_rules(
        rules: Vec<Rule>,
        root: impl Fn() -> Element + 'static,
        viewport: Size<AvailableSpace>,
    ) -> Self {
        Self::with_rules_and_context(rules, root, viewport, Vec::new(), true, false, false)
    }

    /// Same as [`Self::with_rules`], but for a host (only
    /// [`crate::desktop::DesktopHost`] today) with its own window-specific
    /// capabilities to make reachable from every render's own
    /// `use_context` calls, starting with this constructor's own first
    /// render — see `extra_context_providers`'s own doc for why that
    /// matters and [`crate::use_committed_size`] for the established
    /// pattern a capability provided this way follows.
    ///
    /// `respect_reduced_motion`/`initial_os_prefers_reduced_motion` must be
    /// constructor arguments, not set afterward via
    /// [`Self::set_os_prefers_reduced_motion`] — this constructor already
    /// runs its own first [`Self::update`] below, before a caller gets the
    /// constructed runtime back, exactly the same trap
    /// `extra_context_providers` already had to avoid (see its own doc).
    /// Seeded here, a `@keyframes` animation already running at mount is
    /// correctly suppressed (or not) on frame one; seeded only afterward,
    /// it would render unsuppressed for exactly one frame regardless of
    /// the real OS preference. `initial_prefers_dark_color_scheme` follows
    /// the identical requirement, for the identical reason — a
    /// `@media (prefers-color-scheme: dark)` rule must already resolve
    /// correctly on this constructor's own first render.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_rules_and_context(
        rules: Vec<Rule>,
        root: impl Fn() -> Element + 'static,
        viewport: Size<AvailableSpace>,
        extra_context_providers: Vec<Box<dyn Fn()>>,
        respect_reduced_motion: bool,
        initial_os_prefers_reduced_motion: bool,
        initial_prefers_dark_color_scheme: bool,
    ) -> Self {
        let (scope, dirty) = ComponentScope::new();
        let mut animation_timeline = AnimationTimeline::new();
        animation_timeline.set_auto_suppress_motion(respect_reduced_motion);
        animation_timeline.set_os_prefers_reduced_motion(initial_os_prefers_reduced_motion);
        animation_timeline.set_prefers_dark_color_scheme(initial_prefers_dark_color_scheme);
        let mut runtime = Self {
            scope,
            dirty,
            rules,
            root: Box::new(root),
            interaction: InteractionState::new(),
            visited_links: VisitedLinks::new(),
            hovered: None,
            hovered_path: None,
            focused_path: None,
            focused_node: None,
            focus_visible: false,
            modal_return_paths: Vec::new(),
            menu_return_paths: Vec::new(),
            range_dragging: None,
            arena: Arena::build(&Element::Fragment(Vec::new())),
            styles: HashMap::new(),
            layouts: HashMap::new(),
            option_summaries: HashMap::new(),
            multiselect_anchor: None,
            edited: RefCell::new(HashSet::new()),
            user_validated: HashSet::new(),
            validation_bubble: RefCell::new(None),
            resized: RefCell::new(HashMap::new()),
            animation_timeline,
            animation_epoch: std::time::Instant::now(),
            clock_override: None,
            font: florui_text::Font::load_embedded(),
            executor: Rc::new(LocalExecutor::new()),
            size_observers: Rc::new(SizeObserverRegistry::new()),
            position_observers: Rc::new(PositionObserverRegistry::new()),
            focus_observers: Rc::new(FocusObserverRegistry::new()),
            focus_controller: Rc::new(FocusController::new()),
            list_keys: Rc::new(ListKeyRegistry::new()),
            scroll_registry: Rc::new(ScrollRegistry::new()),
            text_input_registry: Rc::new(TextInputRegistry::new()),
            image_registry: Rc::new(crate::image::ImageRegistry::new()),
            icon_registry: Rc::new(crate::icon::IconRegistry::new()),
            asset_cache: Arc::new(florui_assets::AssetCache::new()),
            extra_context_providers,
        };
        runtime.update(viewport);
        runtime
    }

    /// Makes animations and transitions read `seconds` instead of the wall
    /// clock, so a test steps them deterministically; `None` restores it.
    pub(crate) fn set_manual_clock(&mut self, seconds: Option<f64>) {
        self.clock_override = seconds;
    }

    /// Pushes a freshly-read real OS reduced-motion preference in — a real
    /// desktop host calls this fresh before every [`Self::update`] after
    /// construction (the constructor itself already seeds the initial
    /// value; see [`Self::with_rules_and_context`]'s own doc for why that
    /// distinction matters). Does not itself trigger a render.
    pub(crate) fn set_os_prefers_reduced_motion(&mut self, value: bool) {
        self.animation_timeline.set_os_prefers_reduced_motion(value);
    }

    /// Pushes a freshly-resolved effective color scheme in — a real
    /// desktop host calls this on window construction (via
    /// [`Self::with_rules_and_context`]'s own constructor argument) and
    /// again on every live `WindowEvent::ThemeChanged` while no explicit
    /// `WindowOptions.theme` override is active. Does not itself trigger a
    /// render.
    pub(crate) fn set_prefers_dark_color_scheme(&mut self, value: bool) {
        self.animation_timeline.set_prefers_dark_color_scheme(value);
    }

    /// The flag that marks itself whenever a
    /// [`florui_reactive::Signal::set`] happens anywhere under the root —
    /// register a waker with [`DirtyFlag::on_mark`] to learn about it the
    /// instant it happens rather than polling [`Self::is_dirty`] after
    /// specific events a host already knew to check. Prefer
    /// [`Self::on_needs_update`] for a host loop, which also covers async
    /// resource completions this flag alone does not.
    pub fn dirty_flag(&self) -> DirtyFlag {
        self.dirty.clone()
    }

    /// This runtime's own [`ScrollRegistry`] — the same instance every
    /// render's [`crate::use_scroll_offset`] reaches via context, reachable
    /// here too for a real host's own input handling (e.g. a wheel event)
    /// to drive directly, without going through a component at all.
    pub(crate) fn scroll_registry(&self) -> Rc<ScrollRegistry> {
        Rc::clone(&self.scroll_registry)
    }

    pub(crate) fn text_input_registry(&self) -> Rc<TextInputRegistry> {
        Rc::clone(&self.text_input_registry)
    }

    pub(crate) fn image_registry(&self) -> Rc<crate::image::ImageRegistry> {
        Rc::clone(&self.image_registry)
    }

    pub(crate) fn icon_registry(&self) -> Rc<crate::icon::IconRegistry> {
        Rc::clone(&self.icon_registry)
    }

    /// Backs `image_registry`'s and `icon_registry`'s own decoding —
    /// `crate::desktop`'s own paint setup needs both together to request
    /// an SVG `<img>`'s or `<icon>`'s real rasterization once it knows a
    /// real resolved box size for it (see `crate::image`'s own doc).
    pub(crate) fn asset_cache(&self) -> Arc<florui_assets::AssetCache> {
        Arc::clone(&self.asset_cache)
    }

    pub(crate) fn executor(&self) -> Rc<LocalExecutor> {
        Rc::clone(&self.executor)
    }

    /// Replaces the stylesheet driving every subsequent [`Self::update`].
    /// Only `rules` changes — `scope` (every `Signal`, `use_memo`,
    /// `use_effect`, and the rest of a component's persistent state) is
    /// left completely alone, so a CSS-only reload never resets state the
    /// way a fresh render from scratch would. Does not itself re-render;
    /// call [`Self::update`] afterward to see the new rules take effect.
    pub fn set_rules(&mut self, rules: Vec<Rule>) {
        self.rules = rules;
    }

    /// Registers an additional font (e.g. for a script neither embedded
    /// default covers) with this runtime's own long-lived [`Self::font`]
    /// instance, returning its resolved family name — see
    /// [`florui_text::Font::register`]. Reaching that instance is the
    /// part that didn't exist before this runtime owned it across
    /// renders: registering now genuinely changes what every later
    /// [`Self::update`] shapes and measures with, not just a
    /// freshly-loaded instance nothing else could reach. Same contract as
    /// [`Self::set_rules`]: does not itself re-render — call
    /// [`Self::update`] afterward to see the new font take effect.
    pub fn register_font(&mut self, font_bytes: &[u8]) -> Result<String, florui_text::TextError> {
        self.font.register(font_bytes)
    }

    /// Registers `listener` to run whenever this runtime has something an
    /// event-driven host should react to by calling [`Self::update`]
    /// again: a [`florui_reactive::Signal::set`] anywhere under the root,
    /// or a [`florui_reactive::use_resource`] fetch running on this
    /// runtime's own executor becoming newly pollable. Real progress (a
    /// background thread finishing, an I/O reactor firing) still only
    /// happens on its own — this is only the notification that it did, so
    /// a host that otherwise only wakes for input still learns about it.
    ///
    /// May run on a different thread than whichever owns this runtime, the
    /// same way a real I/O completion can — `listener` itself must not
    /// touch this runtime; only signal that an update is due, the way a
    /// host's own event-loop proxy does. Replaces any previously
    /// registered listener.
    pub fn on_needs_update(&self, listener: impl Fn() + Send + Sync + 'static) {
        let listener = std::sync::Arc::new(listener);
        let for_dirty = std::sync::Arc::clone(&listener);
        self.dirty.on_mark(move || for_dirty());
        self.executor.on_woken(move || listener());
    }

    /// Re-renders the tree against `viewport` and caches the resulting
    /// geometry for [`Self::geometry`]/[`Self::hit_test`] to answer
    /// without rendering again.
    pub fn update(&mut self, viewport: Size<AvailableSpace>) {
        let executor = Rc::clone(&self.executor);
        let size_observers = Rc::clone(&self.size_observers);
        let position_observers = Rc::clone(&self.position_observers);
        let focus_observers = Rc::clone(&self.focus_observers);
        let focus_controller = Rc::clone(&self.focus_controller);
        let list_keys = Rc::clone(&self.list_keys);
        let scroll_registry = Rc::clone(&self.scroll_registry);
        // Resolved once so use_viewport_size sees the same value layout uses.
        let resolved_viewport = media_viewport(viewport);
        scroll_registry.begin_render();
        let tree = self.scope.render(|| {
            provide_context(Rc::clone(&executor) as Rc<dyn Executor>);
            provide_context(Rc::clone(&size_observers));
            provide_context(Rc::clone(&position_observers));
            provide_context(Rc::clone(&focus_observers));
            provide_context(Rc::clone(&focus_controller));
            provide_context(Rc::clone(&focus_controller) as Rc<dyn FocusHost>);
            provide_context(Rc::clone(&list_keys));
            provide_context(Rc::clone(&scroll_registry));
            provide_context(ViewportSize {
                width: resolved_viewport.width,
                height: resolved_viewport.height,
            });
            for provider in &self.extra_context_providers {
                provider();
            }
            (self.root)()
        });
        scroll_registry.end_render();
        // Lets any resource the render just started (or a prior task's
        // waker already requeued) make progress before this frame commits.
        self.executor.run_until_stalled();
        let mut tree = tree;
        self.option_summaries = crate::select::normalize(&mut tree);
        crate::textarea_resize::apply(&mut tree, &self.resized.borrow());
        if let Some(bubble) = self.validation_bubble.borrow().as_ref() {
            let natural = self
                .font
                .measure(
                    florui_text::FontFamily::SansSerif,
                    &bubble.message,
                    crate::validation_bubble::TEXT_FONT_SIZE,
                    400.0,
                )
                .width;
            let wrap_width = self.validation_bubble_field().and_then(|field| {
                let (left, _) = self.drawn_position(field);
                let limit = crate::validation_bubble::max_text_width(left, resolved_viewport.width);
                (natural > limit).then_some(limit)
            });
            tree = Element::Fragment(vec![
                tree,
                crate::validation_bubble::element(&bubble.message, wrap_width),
            ]);
        }
        self.arena = Arena::build(&tree);
        self.drop_validation_bubble_if_orphaned();
        self.image_registry
            .sync(&self.arena, &self.asset_cache, &*self.executor);
        self.icon_registry.sync(&self.arena, &*self.executor);
        self.resolve_hover();
        self.resolve_focus();
        self.animation_timeline.advance_to(
            self.clock_override
                .unwrap_or_else(|| self.animation_epoch.elapsed().as_secs_f64()),
        );
        let florui_layout::LayoutResult {
            styles,
            layouts,
            content_extents,
        } = florui_layout::compute_with_style(
            &mut self.font,
            &self.arena,
            &self.rules,
            &self.interaction,
            resolved_viewport,
            &mut self.animation_timeline,
            viewport,
        )
        .expect("this tree's explicit sizes never produce a layout failure");
        self.styles = styles;
        self.icon_registry
            .sync_controls(&self.arena, &self.styles, &*self.executor);
        let (layouts, content_extents) = self.fix_select_widths(layouts, content_extents, viewport);
        let (layouts, content_extents) =
            self.fix_image_intrinsic_sizes(layouts, content_extents, viewport);
        self.layouts = layouts;
        self.position_open_selects(resolved_viewport.width, resolved_viewport.height);
        self.position_validation_bubble(resolved_viewport.width, resolved_viewport.height);
        // After layout, not before: a committed-size/-position observer
        // must see this render's own real geometry, not the previous one's.
        self.size_observers.notify(&self.arena, &self.layouts);
        self.position_observers
            .notify(&self.arena, &self.layouts, &|node| {
                self.drawn_position(node)
            });
        self.focus_observers.notify(&self.arena, self.focused_node);
        self.scroll_registry
            .sync(&self.arena, &self.styles, &self.layouts, &content_extents);
        self.text_input_registry
            .sync(&self.arena, &self.styles, &self.layouts, &mut self.font);
    }

    /// Widens any *auto-width* select whose widest option's real measured
    /// text needs more room than its own natural width (only ever sized
    /// to whichever single option happens to be its current label, since
    /// the rest aren't in the tree while closed — see `crate::select`'s
    /// own doc). Never touches a select with an explicit author `width`,
    /// matching real HTML: auto-sizing to the widest option only ever
    /// applies absent one. A cheap layout-only re-pass, not a full
    /// `compute_with_style` — `self.styles` is already otherwise correct,
    /// only one node's own resolved `width` and the geometry that flows
    /// from it need to change.
    fn fix_select_widths(
        &mut self,
        layouts: HashMap<NodeId, BoxLayout>,
        content_extents: HashMap<NodeId, florui_layout::ContentExtent>,
        available: Size<AvailableSpace>,
    ) -> (
        HashMap<NodeId, BoxLayout>,
        HashMap<NodeId, florui_layout::ContentExtent>,
    ) {
        let mut any_fixed = false;
        for select in self.arena.find_all(|arena, id| arena.tag(id) == "select") {
            let (Some(select_id), Some(&current)) =
                (self.arena.id_attr(select), layouts.get(&select))
            else {
                continue;
            };
            let Some(options) = self.option_summaries.get(select_id) else {
                continue;
            };
            let Some(style) = self.styles.get(&select) else {
                continue;
            };
            if style.width.is_some() {
                continue;
            }
            let family = match style.font_family {
                florui_style::FontFamily::SansSerif => florui_text::FontFamily::SansSerif,
                florui_style::FontFamily::Monospace => florui_text::FontFamily::Monospace,
            };
            let (font_size, font_weight) = (style.font_size, style.font_weight);
            let content_width = options
                .iter()
                .map(|option| {
                    self.font
                        .measure(family, &option.label, font_size, font_weight)
                        .width
                })
                .fold(0.0_f32, f32::max);
            let needed = content_width
                + style.padding.left
                + style.padding.right
                + style.border.left.width
                + style.border.right.width;
            if needed > current.width
                && let Some(style) = self.styles.get_mut(&select)
            {
                style.width = Some(needed);
                any_fixed = true;
            }
        }
        if !any_fixed {
            return (layouts, content_extents);
        }
        florui_layout::compute_layout_with_content_extents(
            &mut self.font,
            &self.arena,
            &self.styles,
            available,
        )
        .expect("a select-width fix-up never produces a layout failure")
    }

    /// Widens/heightens any `<img>`/`<icon>` whose real decoded intrinsic
    /// size just became known (or changed, on a `src` swap) since the
    /// layout pass this fixes up — `image_registry.sync`/`icon_registry.sync`
    /// (already run earlier this same [`Self::update`]) start a
    /// background load the instant a new/changed element is seen, but
    /// that decode can only ever complete on a *later* frame (see
    /// `crate::image`'s own doc); this is the fix-up that applies it once
    /// it has.
    ///
    /// Same shape as [`Self::fix_select_widths`], but resolves through
    /// [`florui_layout::resolve_replaced_size`] rather than reimplementing
    /// its own sizing rule: this pass's own first-layout `current` box
    /// (computed with no intrinsic size known, i.e. real CSS's own
    /// "still loading" case) already reflects whatever this node's real
    /// available space was, so re-running the same resolver with that as
    /// `available_space` and the now-known intrinsic size as `intrinsic`
    /// reproduces exactly what a single real layout pass with the
    /// intrinsic size known from the start would have produced.
    fn fix_image_intrinsic_sizes(
        &mut self,
        layouts: HashMap<NodeId, BoxLayout>,
        content_extents: HashMap<NodeId, florui_layout::ContentExtent>,
        available: Size<AvailableSpace>,
    ) -> (
        HashMap<NodeId, BoxLayout>,
        HashMap<NodeId, florui_layout::ContentExtent>,
    ) {
        let mut any_fixed = false;
        for node in self
            .arena
            .find_all(|arena, id| matches!(arena.tag(id), "img" | "icon"))
        {
            let Some(&current) = layouts.get(&node) else {
                continue;
            };
            let key = FocusPath::of(&self.arena, node);
            let intrinsic = if self.arena.tag(node) == "img" {
                self.image_registry.intrinsic_size(&key)
            } else {
                self.icon_registry.intrinsic_size(&key)
            };
            let Some(intrinsic) = intrinsic else {
                continue;
            };
            let Some(style) = self.styles.get(&node) else {
                continue;
            };
            let resolved = florui_layout::resolve_replaced_size(
                Some(intrinsic),
                style.aspect_ratio,
                Size {
                    width: style.width,
                    height: style.height,
                },
                Size {
                    width: AvailableSpace::Definite(current.width),
                    height: AvailableSpace::Definite(current.height),
                },
            );
            if ((resolved.width - current.width).abs() > 0.01
                || (resolved.height - current.height).abs() > 0.01)
                && let Some(style) = self.styles.get_mut(&node)
            {
                style.width = Some(resolved.width);
                style.height = Some(resolved.height);
                any_fixed = true;
            }
        }
        if !any_fixed {
            return (layouts, content_extents);
        }
        florui_layout::compute_layout_with_content_extents(
            &mut self.font,
            &self.arena,
            &self.styles,
            available,
        )
        .expect("an image intrinsic-size fix-up never produces a layout failure")
    }

    /// Patches each open select's synthesized content div to its real
    /// placement against the trigger, before anything paints this frame.
    /// Unlike `Popover`'s hook-driven two-render settle, this runs
    /// synchronously right after the one layout pass it reads, so the
    /// placeholder position `select::normalize` gives the div is never
    /// actually painted.
    /// Re-anchors everything placed against another element's drawn
    /// position after a scroll moved it, without a render: open selects, the
    /// validation bubble, and position observers (so a popover follows its
    /// trigger).
    pub(crate) fn reposition_for_scroll(&mut self, viewport: Size<AvailableSpace>) {
        let resolved = media_viewport(viewport);
        self.position_open_selects(resolved.width, resolved.height);
        self.position_validation_bubble(resolved.width, resolved.height);
        self.position_observers
            .notify(&self.arena, &self.layouts, &|node| {
                self.drawn_position(node)
            });
    }

    fn position_open_selects(&mut self, viewport_width: f32, viewport_height: f32) {
        let placement = crate::components::popover::Placement::new(
            crate::components::popover::Side::Bottom,
            crate::components::popover::Align::Start,
        );
        let open_triggers: Vec<NodeId> = self
            .arena
            .find_all(|arena, id| arena.tag(id) == "select" && arena.is_open(id));
        for trigger in open_triggers {
            let Some(select_id) = self.arena.id_attr(trigger) else {
                continue;
            };
            let content_id = format!("{select_id}{}", crate::select::SELECT_CONTENT_ID_SUFFIX);
            let Some(content) = self
                .arena
                .find(|arena, id| arena.id_attr(id) == Some(content_id.as_str()))
            else {
                continue;
            };
            let (Some(&trigger_box), Some(&content_box)) =
                (self.layouts.get(&trigger), self.layouts.get(&content))
            else {
                continue;
            };
            let (tx, ty) = self.drawn_position(trigger);
            let (x, y) = crate::components::popover::resolve_placement(
                &placement,
                (tx, ty, trigger_box.width, trigger_box.height),
                (content_box.width, content_box.height),
                (viewport_width, viewport_height),
            );
            if let Some(layout) = self.layouts.get_mut(&content) {
                layout.x = x;
                layout.y = y;
            }
        }
    }

    /// Whether the most recent [`Self::update`] left any `transition`/
    /// `@keyframes` animation still in progress — a host's cue to keep
    /// scheduling redraws (and calling `update` again) on its own timer
    /// rather than waiting for the next real input/state event.
    pub fn is_animating(&self) -> bool {
        self.animation_timeline.is_animating()
    }

    /// The geometry computed by the most recent [`Self::update`].
    pub fn geometry(
        &self,
    ) -> (
        &Arena,
        &HashMap<NodeId, ComputedStyle>,
        &HashMap<NodeId, BoxLayout>,
    ) {
        (&self.arena, &self.styles, &self.layouts)
    }

    /// Same geometry as [`Self::geometry`], plus this runtime's own
    /// long-lived font — for a host that paints the geometry it just read,
    /// via [`florui_paint::paint_to_buffer`], which needs a `&mut Font` of
    /// its own. Passing this one back in (rather than a separately loaded
    /// instance) keeps painting and layout shaping the identical glyphs.
    pub fn geometry_and_font_mut(
        &mut self,
    ) -> (
        &Arena,
        &HashMap<NodeId, ComputedStyle>,
        &HashMap<NodeId, BoxLayout>,
        &mut florui_text::Font,
    ) {
        (&self.arena, &self.styles, &self.layouts, &mut self.font)
    }

    /// The topmost node under `(x, y)`, against the last computed
    /// geometry and the current scroll offsets, where things are drawn —
    /// does not render again.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<NodeId> {
        let offsets = self.scroll_registry.offsets_by_node(&self.arena);
        if offsets.values().all(|&offset| offset == (0.0, 0.0)) {
            return florui_layout::hit_test(&self.arena, &self.layouts, &self.styles, x, y);
        }
        let scrolled = florui_layout::apply_scroll_offsets(&self.arena, &self.layouts, &offsets);
        florui_layout::hit_test(&self.arena, &scrolled, &self.styles, x, y)
    }

    /// [`florui_layout::absolute_position`] where `node` is drawn: shifted
    /// by every scrolled ancestor's offset, for comparing against a pointer.
    pub(crate) fn drawn_position(&self, node: NodeId) -> (f32, f32) {
        let (mut x, mut y) = florui_layout::absolute_position(&self.arena, &self.layouts, node);
        let mut current = self.arena.parent(node);
        while let Some(ancestor) = current {
            if let Some(id) = self.arena.id_attr(ancestor) {
                let (offset_x, offset_y) = self.scroll_registry.current_offset(id);
                x -= offset_x;
                y -= offset_y;
            }
            current = self.arena.parent(ancestor);
        }
        (x, y)
    }

    /// The currently `:hover`ed node, against the last computed geometry —
    /// `None` if the cursor isn't over anything. A caller dispatching
    /// `mouseenter`/`mouseleave` around [`Self::set_hovered`] needs this
    /// read *before* calling it, since that call overwrites it.
    pub fn hovered(&self) -> Option<NodeId> {
        self.hovered
    }

    /// Updates which node is `:hover`ed. Returns whether that actually
    /// changed anything — `:hover` can affect computed style, so a caller
    /// should follow a `true` result with a fresh [`Self::update`].
    pub fn set_hovered(&mut self, node: Option<NodeId>) -> bool {
        if node == self.hovered {
            return false;
        }
        self.hovered = node;
        self.hovered_path = node.map(|id| FocusPath::of(&self.arena, id));
        self.rebuild_interaction();
        true
    }

    /// The currently focused node, against the last computed geometry —
    /// `None` if nothing is focused.
    pub fn focused(&self) -> Option<NodeId> {
        self.focused_node
    }

    /// Sets (or, for `None`, clears) keyboard focus. Returns whether that
    /// actually changed anything — `:focus`/`:focus-visible` can affect
    /// computed style, so a caller should follow a `true` result with a
    /// fresh [`Self::update`]. `via_keyboard` decides whether
    /// `:focus-visible` matches alongside `:focus` — real Tab traversal
    /// passes `true`; a mouse click setting focus (matching real HTML
    /// `:focus` behavior, not `:focus-visible`) passes `false`.
    pub fn set_focused(&mut self, node: Option<NodeId>, via_keyboard: bool) -> bool {
        let focus_visible = via_keyboard && node.is_some();
        if node == self.focused_node && focus_visible == self.focus_visible {
            return false;
        }
        if node != self.focused_node {
            self.dismiss_validation_bubble();
            if let Some(id) = self.focused_node.and_then(|n| self.arena.id_attr(n)) {
                self.text_input_registry.reset_scroll_on_blur(id);
            }
        }
        if node != self.focused_node
            && let Some(previous) = self.focused_path.take()
            && self.edited.borrow().contains(&previous)
        {
            self.user_validated.insert(previous);
        }
        self.focused_node = node;
        self.focused_path = node.map(|id| FocusPath::of(&self.arena, id));
        self.focus_visible = focus_visible;
        self.rebuild_interaction();
        true
    }

    /// Moves keyboard focus to the next focusable element in document
    /// order, wrapping to the first after the last — always keyboard-
    /// origin, so `:focus-visible` matches. Returns whether focus
    /// actually changed (`false` when there is nothing focusable at all).
    pub fn focus_next(&mut self) -> bool {
        self.step_focus(1)
    }

    /// Same as [`Self::focus_next`], stepping backward and wrapping to
    /// the last element after the first.
    pub fn focus_previous(&mut self) -> bool {
        self.step_focus(-1)
    }

    fn step_focus(&mut self, direction: isize) -> bool {
        let order = focus::tab_stops(
            &self.arena,
            &focus::focus_candidates(&self.arena),
            self.focused_node,
        );
        if order.is_empty() {
            return self.set_focused(None, true);
        }
        let next_index = match self
            .focused_node
            .and_then(|id| order.iter().position(|&candidate| candidate == id))
        {
            Some(index) => {
                let len = order.len() as isize;
                (index as isize + direction).rem_euclid(len) as usize
            }
            None => {
                if direction >= 0 {
                    0
                } else {
                    order.len() - 1
                }
            }
        };
        self.set_focused(Some(order[next_index]), true)
    }

    /// Re-resolves [`Self::hovered_path`] against the fresh arena, same as
    /// [`Self::resolve_focus`] does for focus. Every node is a candidate —
    /// unlike focus, hover isn't limited to a focusable subset.
    fn resolve_hover(&mut self) {
        self.hovered = match &self.hovered_path {
            Some(path) => {
                let candidates = self.arena.find_all(|_, _| true);
                let resolved = path.resolve(&self.arena, &candidates);
                if resolved.is_none() {
                    self.hovered_path = None;
                }
                resolved
            }
            None => None,
        };
    }

    /// Moves keyboard focus one step through the focused radio's group
    /// (wrapping) and returns the newly focused radio, or `None` if focus
    /// isn't on a radio with a sibling to move to.
    pub fn step_radio_group(&mut self, direction: isize) -> Option<NodeId> {
        let next = focus::radio_sibling(&self.arena, self.focused_node?, direction)?;
        self.set_focused(Some(next), true);
        Some(next)
    }

    /// A keyboard step for a focused range input — matches real HTML's
    /// own Left/Down (-step), Right/Up (+step), PageDown (-10×step),
    /// PageUp (+10×step, measured against Chrome for both a default and a
    /// custom `step`), Home (min) and End (max).
    pub(crate) fn step_range_value(&self, node: NodeId, step: RangeStep) -> Option<f32> {
        // Self-gated the same as `dispatch_click`: an accessibility action
        // can target any node id regardless of focus, so the gate can't
        // live only in whatever normally keeps a disabled input from
        // being focused in the first place.
        if self.arena.is_disabled(node) {
            return None;
        }
        let (min, max, unit) = (
            self.arena.range_min(node),
            self.arena.range_max(node),
            self.arena.range_step(node),
        );
        let current = self.arena.range_value(node);
        let requested = match step {
            RangeStep::SmallDecrement => current - unit,
            RangeStep::SmallIncrement => current + unit,
            RangeStep::LargeDecrement => current - 10.0 * unit,
            RangeStep::LargeIncrement => current + 10.0 * unit,
            RangeStep::Min => min,
            RangeStep::Max => max,
        };
        let clamped = requested.clamp(min.min(max), min.max(max));
        if clamped == current {
            return None;
        }
        self.commit_value(node, clamped.to_string());
        Some(clamped)
    }

    /// Every range input still in the arena, focusable or not — a drag
    /// already in progress on one that becomes disabled mid-drag (a real,
    /// if unusual, case) still needs to resolve so [`Self::end_range_drag`]
    /// can end it cleanly rather than silently losing track of it.
    fn range_inputs(&self) -> Vec<NodeId> {
        self.arena.find_all(|arena, id| {
            arena.tag(id) == "input" && focus::is_range_input_type(arena.input_type(id))
        })
    }

    /// The value a pointer at `cursor_x` (absolute, same space as
    /// [`Self::drawn_position`]) requests on `node`'s own
    /// track: real HTML's own "click/drag anywhere jumps directly to that
    /// position" behavior — measured against Chrome, no grab-offset is
    /// preserved regardless of where within the thumb the drag started.
    /// Snapped to `step` from `min` and clamped into `[min, max]`. A
    /// non-positive `step` (malformed markup) skips snapping rather than
    /// dividing by zero.
    fn range_value_at(&self, node: NodeId, cursor_x: f32) -> Option<f32> {
        let layout = self.layouts.get(&node)?;
        if layout.width <= 0.0 {
            return None;
        }
        let (track_x, _) = self.drawn_position(node);
        let (min, max, step) = (
            self.arena.range_min(node),
            self.arena.range_max(node),
            self.arena.range_step(node),
        );
        let fraction = ((cursor_x - track_x) / layout.width).clamp(0.0, 1.0);
        let raw = min + fraction * (max - min);
        let snapped = if step > 0.0 {
            min + ((raw - min) / step).round() * step
        } else {
            raw
        };
        Some(snapped.clamp(min.min(max), min.max(max)))
    }

    /// Whether a pointer drag on a range input is currently open — distinct
    /// from [`Self::continue_range_drag`] returning `None`, which also
    /// covers "a drag is open but this particular move requested nothing
    /// new" (see its own doc); a caller deciding whether to fall through to
    /// ordinary hover/hit-testing needs this, not that.
    pub(crate) fn is_range_dragging(&self) -> bool {
        self.range_dragging.is_some()
    }

    /// Begins tracking a pointer drag on `node`'s own track/thumb and
    /// requests the value at `cursor_x` immediately — matches Chrome's own
    /// "mousedown already jumps the value" behavior, before any move.
    /// Returns the requested value, or `None` if `node` isn't a real,
    /// laid-out range input.
    pub(crate) fn start_range_drag(&mut self, node: NodeId, cursor_x: f32) -> Option<f32> {
        self.range_dragging = Some(FocusPath::of(&self.arena, node));
        self.continue_range_drag(cursor_x)
    }

    /// Continues an already-started drag: re-resolves its own node against
    /// the current arena (see [`Self::range_dragging`]'s own doc), and
    /// requests the value at `cursor_x` only if it differs from the
    /// node's own current value — deduplicated the same way a controlled
    /// value naturally is, since nothing here has any other state to
    /// compare against but what the owner most recently echoed back.
    /// `None` if no drag is open, or it no longer resolves.
    pub(crate) fn continue_range_drag(&mut self, cursor_x: f32) -> Option<f32> {
        let path = self.range_dragging.as_ref()?;
        let node = path.resolve(&self.arena, &self.range_inputs())?;
        // Matches `dispatch_click`: a control that becomes disabled between
        // press and release stops accepting requests, even though the
        // drag itself (started while it was still enabled) only ends on a
        // real release.
        if self.arena.is_disabled(node) {
            return None;
        }
        let requested = self.range_value_at(node, cursor_x)?;
        if requested == self.arena.range_value(node) {
            return None;
        }
        self.commit_value(node, requested.to_string());
        Some(requested)
    }

    /// Ends the current drag, if any, firing its own `commit` event once
    /// (no value payload — the owner already has whichever request it last
    /// accepted; see `components::slider`'s own doc). A no-op if the
    /// dragged node no longer resolves.
    pub(crate) fn end_range_drag(&mut self) {
        let Some(path) = self.range_dragging.take() else {
            return;
        };
        if let Some(node) = path.resolve(&self.arena, &self.range_inputs()) {
            self.dispatch_event(node, "commit");
        }
    }

    /// The option one arrow-key step (wrapping) from the focused, *open*
    /// select's currently selected option — `None` if focus isn't on an
    /// open select, or it has fewer than two options. Unlike
    /// [`Self::step_radio_group`], focus stays on the select itself: an
    /// `<option>` is never independently focusable, only Tab-reachable via
    /// its parent select.
    pub fn step_select_option(&self, direction: isize) -> Option<NodeId> {
        let select = self.focused_node?;
        if self.arena.tag(select) != "select" {
            return None;
        }
        let options = crate::select::options_of(&self.arena, select)?;
        if options.len() < 2 {
            return None;
        }
        // Pivots on `active` (the keyboard highlight), falling back to
        // `selected` — real HTML starts the highlight at the current
        // value when a select first opens, before any arrow key has
        // marked an option `active` itself.
        let index = options
            .iter()
            .position(|&id| self.arena.is_active(id))
            .or_else(|| options.iter().position(|&id| self.arena.is_selected(id)))
            .unwrap_or(0);
        let next = (index as isize + direction).rem_euclid(options.len() as isize) as usize;
        Some(options[next])
    }

    /// The option a focused, *open* select's Enter should commit: its
    /// `active` (highlighted) option, or its `selected` one if none is
    /// active yet (see [`Self::step_select_option`]'s own doc).
    pub fn active_or_selected_option(&self) -> Option<NodeId> {
        let select = self.focused_node?;
        crate::select::active_option(&self.arena, select).or_else(|| {
            crate::select::options_of(&self.arena, select)?
                .into_iter()
                .find(|&id| self.arena.is_selected(id))
        })
    }

    /// Changes a focused, *closed* select's value directly by `direction`
    /// (real HTML: Up/Down on a closed select commits immediately,
    /// without opening it) — `true` if it actually dispatched a click.
    /// Options aren't real `Arena` nodes while closed, so this reads
    /// [`Self::option_summaries`] instead of walking the tree.
    pub fn step_closed_select(&self, direction: isize) -> bool {
        let Some(select) = self.focused_node else {
            return false;
        };
        if self.arena.tag(select) != "select" || self.arena.is_open(select) {
            return false;
        }
        let Some(select_id) = self.arena.id_attr(select) else {
            return false;
        };
        let Some(options) = self.option_summaries.get(select_id) else {
            return false;
        };
        if options.len() < 2 {
            return false;
        }
        let index = options.iter().position(|o| o.selected).unwrap_or(0);
        let next = (index as isize + direction).rem_euclid(options.len() as isize) as usize;
        let Some(handler) = &options[next].onclick else {
            return false;
        };
        florui_reactive::batch(|| handler.call(&Event::new()));
        true
    }

    /// Computes and applies a `<select multiple>` click on `option`,
    /// given which modifier (if any) was held — see
    /// [`crate::select::compute_multiselect`]'s own doc for the exact
    /// semantics. Calls the select's own `onselectionchange` and updates
    /// the Shift-click range anchor. `false` if `option` isn't a real
    /// option inside a real multi-select.
    pub fn commit_multiselect_click(&mut self, option: NodeId, ctrl: bool, shift: bool) -> bool {
        let Some(select) = crate::select::owning_select(&self.arena, option) else {
            return false;
        };
        let Some(select_id) = self.arena.id_attr(select) else {
            return false;
        };
        let select_id = select_id.to_string();
        let clicked_value = self
            .arena
            .value_attr(option)
            .map(str::to_string)
            .unwrap_or_else(|| self.arena.text_content(option).to_string());
        let Some(options) = self.option_summaries.get(&select_id) else {
            return false;
        };
        let anchor_value = self
            .multiselect_anchor
            .as_ref()
            .filter(|(anchor_select, _)| *anchor_select == select_id)
            .map(|(_, value)| value.as_str());
        let new_selection =
            crate::select::compute_multiselect(options, &clicked_value, ctrl, shift, anchor_value);
        if !shift {
            self.multiselect_anchor = Some((select_id, clicked_value));
        }
        if let Some(handler) = self.arena.selection_handler(select, "selection") {
            let handler = handler.clone();
            florui_reactive::batch(|| handler.call(new_selection));
        }
        true
    }

    /// Re-resolves [`Self::focused_path`] against `candidates` — a
    /// [`NodeId`] from the previous arena generation isn't safe to reuse
    /// directly (see [`FocusPath`]'s own doc). Clears focus outright
    /// (and `focused_path`/`focus_visible` with it) if it no longer
    /// resolves against `candidates` — no invented fallback.
    fn resolve_against(&mut self, candidates: &[NodeId]) {
        self.focused_node = match &self.focused_path {
            Some(path) => {
                let resolved = path.resolve(&self.arena, candidates);
                if resolved.is_none() {
                    self.focused_path = None;
                    self.focus_visible = false;
                }
                resolved
            }
            None => None,
        };
    }

    /// Re-resolves focus against this render's freshly rebuilt
    /// [`Self::arena`], and drives the modal [`crate::components::dialog::Dialog`]
    /// open/close transition:
    ///
    /// - **just opened** (more modals than last render): saves wherever
    ///   focus currently is onto [`Self::modal_return_paths`] (`None` if
    ///   nothing was focused), then traps focus onto the innermost
    ///   modal's own first focusable descendant, if any — a real
    ///   `:focus-visible` trap, matching expected modal UX.
    /// - **steady-state open**: resolves the existing focus against the
    ///   innermost modal's content only ([`focus::focusable_within`]),
    ///   never the whole document.
    /// - **just closed** (fewer modals): restores focus to the path saved
    ///   when that modal opened, resolved against what is now innermost
    ///   (the next modal down, or the whole document) — falling back to
    ///   its first focusable node if the original trigger was removed.
    /// - **no modal, no transition**: plain resolution against the
    ///   whole document.
    fn resolve_focus(&mut self) {
        let modals = focus::modal_roots(&self.arena);
        let before = self.modal_return_paths.len();
        let innermost = modals.last().copied();
        let within_innermost = |runtime: &Self| match innermost {
            Some(root) => focus::focusable_within(&runtime.arena, root),
            None => focus::focus_order(&runtime.arena),
        };
        if modals.len() > before {
            for _ in before..modals.len() {
                self.modal_return_paths.push(self.focused_path.clone());
            }
            let first = within_innermost(self).into_iter().next();
            self.focused_path = first.map(|id| FocusPath::of(&self.arena, id));
            self.focused_node = first;
            self.focus_visible = first.is_some();
        } else if modals.len() < before {
            let restore = self
                .modal_return_paths
                .drain(modals.len()..)
                .next()
                .flatten();
            let had_return = restore.is_some();
            self.focused_path = restore;
            let candidates = within_innermost(self);
            self.resolve_against(&candidates);
            // A removed trigger falls back to the start of the document.
            if self.focused_node.is_none() && had_return {
                self.focused_node = candidates.first().copied();
                self.focused_path = self
                    .focused_node
                    .map(|node| FocusPath::of(&self.arena, node));
            }
            if self.focused_node.is_some() {
                self.focus_visible = true;
            }
        } else {
            let candidates = within_innermost(self);
            self.resolve_against(&candidates);
        }
        self.resolve_menu_focus();
        self.resolve_focus_request();
        self.rebuild_interaction();
    }

    /// Serves a component's [`FocusController::request_focus`]: focuses the
    /// element with the requested `id` (or its first focusable descendant)
    /// if it is mounted and reachable, else waits for it a couple of renders.
    fn resolve_focus_request(&mut self) {
        let Some(id) = self.focus_controller.pending_id() else {
            return;
        };
        let candidates = focus::focus_candidates(&self.arena);
        let target = self
            .arena
            .find(|arena, node| arena.id_attr(node) == Some(id.as_str()))
            .and_then(|node| {
                focus::focusable_within(&self.arena, node)
                    .into_iter()
                    .find(|focusable| candidates.contains(focusable))
            });
        match target {
            Some(node) => {
                self.focused_path = Some(FocusPath::of(&self.arena, node));
                self.focused_node = Some(node);
                self.focus_visible = true;
                self.focus_controller.fulfil();
            }
            None => self.focus_controller.wait_a_render(),
        }
    }

    /// Focus on the menu popovers ([`crate::menu_keys`]): a menu that just
    /// opened takes focus on its first item, remembering what held it; one
    /// that just closed gives focus back to that control, unless the user
    /// already moved focus elsewhere (a click outside keeps its target).
    fn resolve_menu_focus(&mut self) {
        let menus = menu_keys::open_menus(&self.arena);
        let before = self.menu_return_paths.len();
        if menus.len() > before {
            for _ in before..menus.len() {
                self.menu_return_paths.push(self.focused_path.clone());
            }
            let first = menus
                .last()
                .and_then(|&(_, menu)| menu_keys::items(&self.arena, menu).first().copied());
            if let Some(first) = first {
                self.focused_path = Some(FocusPath::of(&self.arena, first));
                self.focused_node = Some(first);
                self.focus_visible = true;
            }
        } else if menus.len() < before {
            let restore = self.menu_return_paths.drain(menus.len()..).next().flatten();
            if self.focused_node.is_none()
                && let Some(path) = restore
            {
                let candidates = focus::focus_candidates(&self.arena);
                self.focused_path = Some(path);
                self.resolve_against(&candidates);
                self.focus_visible = self.focused_node.is_some();
            }
        }
    }

    /// Rebuilds `self.interaction` from whatever's currently live
    /// (`hovered`, `focused_node`, `focus_visible`) — the single place
    /// that assembles it, so setting one doesn't silently clobber the
    /// others the way replacing it wholesale would.
    fn rebuild_interaction(&mut self) {
        self.focus_controller.set_focused_id(
            self.focused_node
                .and_then(|node| self.arena.id_attr(node).map(str::to_owned)),
        );
        let mut state = InteractionState::new().with_visited(&self.visited_links.snapshot());
        if let Some(id) = self.hovered {
            state = state.with_hovered(id);
        }
        if let Some(id) = self.focused_node {
            state = state.with_focused(id);
            if self.focus_visible {
                state = state.with_focus_visible(id);
            }
        }
        for node in self.arena.find_all(crate::form::has_form_state) {
            if let Some(form_state) = self.form_state_of(node) {
                state = state.with_form_state(node, form_state);
            }
        }
        self.interaction = state;
    }

    /// [`Self::geometry_and_font_mut`], plus the interaction state — a
    /// separate borrow can't be taken alongside the `&mut Font`.
    pub(crate) fn geometry_font_and_interaction_mut(
        &mut self,
    ) -> (
        &Arena,
        &HashMap<NodeId, ComputedStyle>,
        &HashMap<NodeId, BoxLayout>,
        &mut florui_text::Font,
        &InteractionState,
    ) {
        (
            &self.arena,
            &self.styles,
            &self.layouts,
            &mut self.font,
            &self.interaction,
        )
    }

    /// Swaps in the real, process-wide [`VisitedLinks`] — see
    /// [`Self::visited_links`]'s own doc for why this is a post-
    /// construction setter, not a constructor argument. Rebuilds
    /// [`Self::interaction`] immediately so a caller that already marked
    /// something visited before this runs (unlikely, but not prevented)
    /// isn't left stale until the next unrelated hover/focus change.
    pub(crate) fn set_visited_links(&mut self, links: VisitedLinks) {
        self.visited_links = links;
        self.rebuild_interaction();
    }

    /// Re-reads [`Self::visited_links`] so `:link`/`:visited` matching
    /// reflects a href just marked visited — `WindowControls::open_url`
    /// marks it on the *store* directly (it has no `UiRuntime` to call
    /// back into), so this is the other half. Like [`Self::set_focused`],
    /// this doesn't request a redraw itself — the caller
    /// (`desktop.rs`'s own `activate`) already knows it needs one and
    /// requests it itself, the same pattern every other interaction-state
    /// change here already follows.
    pub(crate) fn refresh_visited_links(&mut self) {
        self.rebuild_interaction();
    }

    /// The node a click on `node` activates. A click reaching a `<label>`
    /// (on the label itself, or on something inside it) activates the
    /// label's control, as in real HTML: the `<input>`/`<button>` its
    /// `for` names, or, with no `for`, its first such descendant. `None`
    /// if that control is disabled (clicking its label does nothing). An
    /// input or button is its own target, and so is anything with no
    /// label around it or no control to reach. Only a label redirects: any
    /// other wrapper never does.
    pub fn activation_target(&self, node: NodeId) -> Option<NodeId> {
        let Some(label) = self.enclosing_label(node) else {
            return Some(node);
        };
        let control = match self.arena.label_for(label) {
            Some(for_id) => self.arena.find(|arena, candidate| {
                focus::is_labelable(arena, candidate) && arena.id_attr(candidate) == Some(for_id)
            }),
            None => self.first_labelable_descendant(label),
        };
        match control {
            Some(control) => focus::is_focusable(&self.arena, control).then_some(control),
            None => Some(node),
        }
    }

    /// The nearest `<label>` at or above `node`, unless an input or button
    /// sits in between: those take their own clicks.
    fn enclosing_label(&self, node: NodeId) -> Option<NodeId> {
        let mut current = Some(node);
        while let Some(id) = current {
            if self.arena.tag(id) == "label" {
                return Some(id);
            }
            if focus::is_labelable(&self.arena, id) {
                return None;
            }
            current = self.arena.parent(id);
        }
        None
    }

    fn first_labelable_descendant(&self, root: NodeId) -> Option<NodeId> {
        let mut stack: Vec<NodeId> = self.arena.children(root).iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            if focus::is_labelable(&self.arena, id) {
                return Some(id);
            }
            stack.extend(self.arena.children(id).iter().rev());
        }
        None
    }

    /// Calls `node`'s `click` handler, if it declared one, against the
    /// last computed geometry — inside [`florui_reactive::batch`], so a
    /// handler that writes more than one `Signal` (or writes the same one
    /// more than once) wakes this runtime's host exactly once for the
    /// whole click, not once per write.
    ///
    /// A disabled button, checkbox or radio's handler never fires, regardless of
    /// caller: real mouse clicks and Enter/Space activation both already
    /// funnel through here (`desktop.rs`'s own `handle_release`/
    /// `handle_keyboard_input`), so gating here is the one place that has
    /// to know about `disabled` at all — and it also catches a control
    /// that becomes disabled between press and release, which
    /// `desktop.rs`'s own press-time filtering alone can't (that only
    /// prevents the separate focus-on-click issue; see `handle_press`'s
    /// own doc). Tag-gated the same as [`crate::focus::is_focusable`].
    ///
    /// Returns whether the handler (if any) called
    /// [`Event::prevent_default`] — a primitive with its own default
    /// action (e.g. `<a href>` following its target) checks this before
    /// running it. No primitive has one yet, so every caller today
    /// ignores the return value; it exists for that future caller.
    pub fn dispatch_click(&self, node: NodeId) -> bool {
        let is_checkable = self.arena.tag(node) == "input"
            && focus::is_checkable_input_type(self.arena.input_type(node));
        let is_gated = matches!(self.arena.tag(node), "button" | "select") || is_checkable;
        if is_gated && self.arena.is_disabled(node) {
            return false;
        }
        let event = Event::new();
        if let Some(handler) = self.arena.handler(node, "click") {
            florui_reactive::trace::with_event("click", node, || {
                florui_reactive::batch(|| handler.call(&event))
            });
        }
        event.default_prevented()
    }

    /// Reports `value` to whichever write-back channel `node`'s own
    /// `value` attribute carries — a [`florui_reactive::Binding`] if it
    /// declared one, else an explicit [`florui::ValueHandler`] if it
    /// declared that instead, else nothing. Shared by every controlled
    /// primitive that writes back through the `value` attribute (a text
    /// input's committed text, a range input's requested number as a
    /// string); the caller still owns redrawing afterward, the same way
    /// [`Self::dispatch_click`]'s caller does.
    pub(crate) fn commit_value(&self, node: NodeId, value: String) {
        if crate::form_control::spec(&self.arena, node).is_some_and(|spec| spec.text_field) {
            self.edited
                .borrow_mut()
                .insert(FocusPath::of(&self.arena, node));
        }
        self.dismiss_validation_bubble_of(node);
        if let Some(binding) = self.arena.value_binding(node, "value") {
            florui_reactive::trace::with_event("value", node, || binding.request_update(value));
        } else if let Some(handler) = self.arena.value_handler(node, "value") {
            florui_reactive::trace::with_event("value", node, || handler.call(value));
        }
    }

    /// Offers a list-navigation key to the nearest virtualized list around
    /// the focused element; `true` when a list consumed it.
    pub(crate) fn list_key(&self, key: ListKey) -> bool {
        self.focused_node
            .is_some_and(|focused| self.list_keys.handle(&self.arena, focused, key))
    }

    /// Same as [`Self::dispatch_click`], generalized to an arbitrary
    /// event name and with no disabled-button gate — a modal
    /// [`crate::components::dialog::Dialog`]'s own root is never itself a
    /// disableable button, so that check has nothing to apply to here.
    /// `node` must be valid against [`Self::arena`] in its current
    /// generation, same as every other accessor here. Returns whether the
    /// handler called [`Event::prevent_default`], same as
    /// [`Self::dispatch_click`].
    pub(crate) fn dispatch_event(&self, node: NodeId, event_name: &str) -> bool {
        let event = Event::new();
        if let Some(handler) = self.arena.handler(node, event_name) {
            florui_reactive::trace::with_event(trace_event_name(event_name), node, || {
                florui_reactive::batch(|| handler.call(&event))
            });
        }
        event.default_prevented()
    }

    /// Whether a [`florui_reactive::Signal::set`] happened since the last
    /// [`Self::clear_dirty`].
    pub fn is_dirty(&self) -> bool {
        self.dirty.get()
    }

    pub fn clear_dirty(&self) {
        self.dirty.clear();
    }
}

/// A trace names an event with a `'static` string, so the dispatched names
/// the runtime knows are listed and anything else is `"other"`.
fn trace_event_name(name: &str) -> &'static str {
    const KNOWN: [&str; 4] = ["mouseenter", "mouseleave", "dismiss", "click"];
    KNOWN
        .iter()
        .find(|known| **known == name)
        .copied()
        .unwrap_or("other")
}

/// The viewport `@media`'s own size features resolve against, from
/// whatever the host actually gave [`UiRuntime::update`] — a definite
/// axis is the real one; `MinContent`/`MaxContent` (a host measuring its
/// own intrinsic size, not rendering into a fixed viewport) falls back to
/// [`florui_style::Viewport::default`]'s own placeholder on that axis,
/// since there is no real viewport size to report.
fn media_viewport(viewport: Size<AvailableSpace>) -> florui_style::Viewport {
    let default = florui_style::Viewport::default();
    florui_style::Viewport {
        width: match viewport.width {
            AvailableSpace::Definite(width) => width,
            AvailableSpace::MinContent | AvailableSpace::MaxContent => default.width,
        },
        height: match viewport.height {
            AvailableSpace::Definite(height) => height,
            AvailableSpace::MinContent | AvailableSpace::MaxContent => default.height,
        },
    }
}

mod forms;
#[cfg(feature = "desktop")]
pub(crate) use forms::ImplicitSubmit;
#[cfg(test)]
mod forms_tests;

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use florui_style::Rgba;

    use florui::prelude::*;
    use florui_reactive::testing::manual_future;
    use florui_reactive::{Resource, use_context, use_resource};

    use super::*;
    use crate::{Dialog, DialogProps, Portal, PortalProps, use_committed_size};

    fn viewport() -> Size<AvailableSpace> {
        Size {
            width: AvailableSpace::Definite(100.0),
            height: AvailableSpace::Definite(100.0),
        }
    }

    fn status_text(id: NodeId, runtime: &UiRuntime) -> String {
        let (arena, ..) = runtime.geometry();
        arena.text_content(id).to_string()
    }

    fn find_status(runtime: &UiRuntime) -> NodeId {
        let (arena, ..) = runtime.geometry();
        arena
            .find(|arena, id| arena.id_attr(id) == Some("status"))
            .expect("root always renders a #status node")
    }

    /// Proves `use_resource` works through a real [`UiRuntime`], not just
    /// the raw `florui-reactive` hook in isolation: the `Executor` it
    /// needs comes from context [`Self::update`] provides, and its
    /// eventual `Ready` state reaches a real rendered tree.
    #[test]
    fn a_resource_resolves_through_a_real_update_cycle() {
        let (future, resolver) = manual_future::<Result<i32, &'static str>>();
        let future = Rc::new(RefCell::new(Some(future)));

        let root = move || {
            let future = Rc::clone(&future);
            let resource = use_resource("key", move |_| {
                future
                    .borrow_mut()
                    .take()
                    .expect("the fetch only runs once for an unchanged key")
            });
            let text = match resource.get() {
                Resource::Idle => "idle".to_string(),
                Resource::Pending { .. } => "pending".to_string(),
                Resource::Ready(value) => format!("ready:{value}"),
                Resource::Failed { error, .. } => format!("failed:{error}"),
            };
            view! { <div id="status">{text}</div> }
        };

        let mut runtime = UiRuntime::with_rules(Vec::new(), root, viewport());
        // The effect that starts the fetch runs after the first render
        // commits, the same as any other mount effect — its `Signal::set`
        // to `Pending` only shows up once something re-renders afterward.
        assert!(runtime.is_dirty());
        runtime.clear_dirty();
        runtime.update(viewport());
        let status = find_status(&runtime);
        assert_eq!(status_text(status, &runtime), "pending");

        resolver.resolve(Ok(42));
        // This render's own snapshot still reads the pre-completion state:
        // `update` advances the executor (committing `Ready`) only after
        // building this render's tree, the same ordering that makes the
        // initial `Pending` above take one extra render to show up too.
        runtime.update(viewport());
        assert!(runtime.is_dirty());
        runtime.clear_dirty();
        runtime.update(viewport());
        let status = find_status(&runtime);
        assert_eq!(status_text(status, &runtime), "ready:42");
    }

    /// The concrete, automatable half of "CSS reload preserves state": a
    /// real winit file watcher and event loop are only exercisable
    /// manually, but the actual mechanism — swapping `rules` without
    /// touching `scope` — needs no window at all to prove.
    #[test]
    fn set_rules_changes_style_without_resetting_component_state() {
        let root = || {
            let count = use_signal(|| 0);
            let clicked = count.clone();
            view! {
                <button id="status" onclick={move || clicked.set(clicked.get() + 1)}>
                    {count.get().to_string()}
                </button>
            }
        };
        let mut runtime = UiRuntime::with_rules(Vec::new(), root, viewport());
        let status = find_status(&runtime);
        runtime.dispatch_click(status);
        runtime.update(viewport());
        assert_eq!(status_text(find_status(&runtime), &runtime), "1");

        let new_rules = florui_style::parse_stylesheet("#status { color: #ff0000; }")
            .expect("a trivial rule always parses");
        runtime.set_rules(new_rules);
        runtime.update(viewport());

        assert_eq!(
            status_text(find_status(&runtime), &runtime),
            "1",
            "swapping in a real stylesheet must not reset the click count set before it"
        );
        let (_, styles, _) = runtime.geometry();
        let status = find_status(&runtime);
        assert_eq!(
            styles[&status].color,
            Rgba::opaque(0xff, 0x00, 0x00),
            "the new rule must actually take effect, not just fail to reset state"
        );
    }

    /// Proves `register_font` reaches the runtime's own long-lived `Font`
    /// instance — the one every real `Self::update` shapes and measures
    /// with — not a separate, freshly-loaded one nothing else could ever
    /// see. Before the font became a persistent field on `UiRuntime`
    /// itself, there was no instance for a caller to register an extra
    /// font onto in the first place: each `compute_layout` call built and
    /// discarded its own.
    #[test]
    fn register_font_reaches_the_same_persistent_font_instance_across_updates() {
        let mut runtime = UiRuntime::with_rules(Vec::new(), || view! { <div /> }, viewport());

        let family_name = runtime
            .register_font(florui_text::EMBEDDED_MONOSPACE_FONT)
            .expect("a real embedded font file must register successfully");
        assert!(!family_name.is_empty());

        // Does not itself re-render (matches set_rules's own contract) --
        // an explicit update afterward must still work normally, proving
        // registering didn't leave the runtime's own font in a broken or
        // replaced state.
        runtime.update(viewport());

        let (.., font) = runtime.geometry_and_font_mut();
        let metrics = font.measure(florui_text::FontFamily::SansSerif, "x", 16.0, 400.0);
        assert!(
            metrics.width > 0.0,
            "the same font instance register_font touched must still measure real text \
             correctly afterward"
        );
    }

    /// `DesktopHost` needs a window-specific capability (`WindowControls`)
    /// reachable from `use_context` starting with this constructor's own
    /// first render, not only from the second render onward — a component
    /// that unconditionally calls `use_window_controls()` at mount would
    /// otherwise silently see `None` on its very first frame. Registering
    /// a provider only *after* construction (a plain setter, rather than
    /// a constructor argument) would miss exactly that first render, since
    /// the constructor already ran its own first `update` before a setter
    /// call could ever run.
    #[test]
    fn extra_context_providers_apply_starting_from_the_very_first_render() {
        let seen = Rc::new(RefCell::new(None));
        let seen_in_root = Rc::clone(&seen);
        let root = move || {
            *seen_in_root.borrow_mut() = use_context::<i32>();
            view! { <div /> }
        };
        let providers: Vec<Box<dyn Fn()>> =
            vec![Box::new(|| florui_reactive::provide_context(42_i32))];

        let _runtime = UiRuntime::with_rules_and_context(
            Vec::new(),
            root,
            viewport(),
            providers,
            true,
            false,
            false,
        );

        assert_eq!(
            *seen.borrow(),
            Some(42),
            "a provider passed to the constructor must run during the constructor's own first render"
        );
    }

    /// Same shape as the `extra_context_providers` test above, for the
    /// identical reason: `respect_reduced_motion`/
    /// `initial_os_prefers_reduced_motion` must be constructor arguments,
    /// not set afterward, because the constructor already runs its own
    /// first `update` before a caller could ever call
    /// `set_os_prefers_reduced_motion`. Uses a `@keyframes` animation, not
    /// a transition: a transition needs a *previous* render to change away
    /// from, which the constructor's own first render never has — a
    /// `@keyframes` animation is active from the very first render an
    /// element with `animation-name` appears in, exactly the case this
    /// guards.
    #[test]
    fn reduced_motion_suppression_applies_starting_from_the_very_first_render() {
        let css = "
            .box {
                opacity: 1;
                animation-name: dim;
                animation-duration: 10s;
                animation-fill-mode: forwards;
            }
            @keyframes dim {
                from { opacity: 0.3; }
                to { opacity: 0.3; }
            }
        ";
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let root = || view! { <div class="box" /> };

        let runtime = UiRuntime::with_rules_and_context(
            rules,
            root,
            viewport(),
            Vec::new(),
            true,
            true,
            false,
        );

        let (arena, styles, _) = runtime.geometry();
        let node = arena.roots()[0];
        assert_eq!(
            styles[&node].opacity, 1.0,
            "suppression seeded at construction must already apply to the constructor's own \
             first render (plain opacity: 1), not splice in the animation's 0.3 value"
        );
    }

    /// Same shape again, for `initial_prefers_dark_color_scheme`: a
    /// `@media (prefers-color-scheme: dark)` rule must already resolve
    /// correctly on the constructor's own first render, not just on
    /// updates after it.
    #[test]
    fn color_scheme_applies_starting_from_the_very_first_render() {
        let css = "
            .box { background-color: #ffffff; }
            @media (prefers-color-scheme: dark) {
                .box { background-color: #000000; }
            }
        ";
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let root = || view! { <div class="box" /> };

        let runtime = UiRuntime::with_rules_and_context(
            rules,
            root,
            viewport(),
            Vec::new(),
            true,
            false,
            true,
        );

        let (arena, styles, _) = runtime.geometry();
        let node = arena.roots()[0];
        assert_eq!(
            styles[&node].background_color,
            florui_style::Rgba::opaque(0, 0, 0),
            "the dark-scheme value seeded at construction must already apply to the \
             constructor's own first render"
        );
    }

    #[test]
    fn dispatch_click_calls_the_nodes_own_click_handler() {
        let clicked = Rc::new(Cell::new(false));
        let clicked_in_handler = Rc::clone(&clicked);
        let runtime = UiRuntime::with_rules(
            Vec::new(),
            move || {
                let clicked = Rc::clone(&clicked_in_handler);
                view! { <button onclick={move || clicked.set(true)} /> }
            },
            Size::MAX_CONTENT,
        );
        let button = runtime.geometry().0.roots()[0];

        runtime.dispatch_click(button);

        assert!(clicked.get());
    }

    #[test]
    fn dispatch_click_on_a_node_with_no_handler_does_nothing() {
        let runtime = UiRuntime::with_rules(Vec::new(), || view! { <div /> }, Size::MAX_CONTENT);
        let node = runtime.geometry().0.roots()[0];

        // Must not panic — the whole point of the test.
        runtime.dispatch_click(node);
    }

    #[test]
    fn dispatch_click_on_a_disabled_button_does_not_call_its_handler() {
        let clicked = Rc::new(Cell::new(false));
        let clicked_in_handler = Rc::clone(&clicked);
        let runtime = UiRuntime::with_rules(
            Vec::new(),
            move || {
                let clicked = Rc::clone(&clicked_in_handler);
                view! { <button disabled="true" onclick={move || clicked.set(true)} /> }
            },
            Size::MAX_CONTENT,
        );
        let button = runtime.geometry().0.roots()[0];

        runtime.dispatch_click(button);

        assert!(
            !clicked.get(),
            "a disabled button's click handler must not fire"
        );
    }

    #[test]
    fn dispatch_click_ignores_disabled_on_a_non_button_element() {
        // v1 scope: disabled has no wired behavior outside <button> --
        // see focus::is_focusable's own doc.
        let clicked = Rc::new(Cell::new(false));
        let clicked_in_handler = Rc::clone(&clicked);
        let runtime = UiRuntime::with_rules(
            Vec::new(),
            move || {
                let clicked = Rc::clone(&clicked_in_handler);
                view! { <div disabled="true" onclick={move || clicked.set(true)} /> }
            },
            Size::MAX_CONTENT,
        );
        let div = runtime.geometry().0.roots()[0];

        runtime.dispatch_click(div);

        assert!(clicked.get());
    }

    #[test]
    fn a_handler_writing_two_signals_wakes_the_host_exactly_once() {
        let wakes = Rc::new(Cell::new(0));
        let wakes_in_waker = Rc::clone(&wakes);

        let runtime = UiRuntime::with_rules(
            Vec::new(),
            || {
                let a = use_signal(|| 0);
                let b = use_signal(|| 0);
                view! {
                    <button onclick={move || {
                        a.set(1);
                        b.set(2);
                    }} />
                }
            },
            Size::MAX_CONTENT,
        );
        runtime
            .dirty_flag()
            .on_mark(move || wakes_in_waker.set(wakes_in_waker.get() + 1));
        let button = runtime.geometry().0.roots()[0];

        runtime.dispatch_click(button);

        assert_eq!(
            wakes.get(),
            1,
            "one click writing two signals must wake the host once, not twice"
        );
    }

    /// Proves the bridge an event-driven host needs actually exists: a
    /// fetch resolved by a real OS thread (not this test calling anything
    /// on the resource or the runtime) still reaches the rendered tree,
    /// with [`UiRuntime::on_needs_update`] as the only thing telling this
    /// test when to call [`UiRuntime::update`] again — no click, no
    /// resize, no polling loop. `on_needs_update` is registered right
    /// after construction, before the mount-effect catch-up update below —
    /// the same order [`crate::run`]'s real desktop host uses — so a fetch
    /// that resolves unusually fast still has a listener in place; the
    /// artificial delay is extra margin, not what makes this correct.
    #[test]
    fn a_background_completion_notifies_on_needs_update_without_a_polling_loop() {
        let root = || {
            let resource = use_resource("key", |_| {
                florui_reactive::blocking::spawn_blocking(|| {
                    std::thread::sleep(std::time::Duration::from_millis(30));
                    Ok::<i32, &'static str>(42)
                })
            });
            let text = match resource.get() {
                Resource::Idle => "idle".to_string(),
                Resource::Pending { .. } => "pending".to_string(),
                Resource::Ready(value) => format!("ready:{value}"),
                Resource::Failed { error, .. } => format!("failed:{error}"),
            };
            view! { <div id="status">{text}</div> }
        };

        let mut runtime = UiRuntime::with_rules(Vec::new(), root, viewport());
        let (needs_update_tx, needs_update_rx) = std::sync::mpsc::channel();
        runtime.on_needs_update(move || {
            let _ = needs_update_tx.send(());
        });
        if runtime.is_dirty() {
            runtime.clear_dirty();
            runtime.update(viewport());
        }
        assert_eq!(status_text(find_status(&runtime), &runtime), "pending");

        needs_update_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the background thread's completion must reach on_needs_update on its own");
        // As above: this render's own snapshot still reads the
        // pre-completion state, since `update` only commits `Ready` (via
        // run_until_stalled) after building it.
        runtime.update(viewport());
        assert!(runtime.is_dirty());
        runtime.clear_dirty();
        runtime.update(viewport());
        assert_eq!(status_text(find_status(&runtime), &runtime), "ready:42");
    }

    /// End-to-end through a real [`UiRuntime`]: an `<img>` with no CSS
    /// size at all starts at `0x0` (a real intrinsic size not decoded
    /// yet), then, once the real background PNG decode completes and
    /// wakes the host on its own (same `on_needs_update` path the
    /// `use_resource` tests above exercise, but via `image_registry`'s
    /// own direct `executor.spawn` rather than a hook), the very next
    /// `update` resizes it to its own real 17x9 intrinsic size — proving
    /// `fix_image_intrinsic_sizes` actually applies a completed load, not
    /// just that `ImageRegistry` reports one in isolation.
    #[test]
    fn an_img_resizes_to_its_real_intrinsic_size_once_a_background_decode_completes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        let image = image::RgbaImage::from_fn(17, 9, |_, _| image::Rgba([1, 2, 3, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        std::fs::write(&path, bytes).unwrap();
        let src = path.display().to_string();

        let root = move || {
            view! { <img id="pic" src={src.clone()} /> }
        };

        // `on_needs_update` (exercised in isolation by the two tests
        // above, for the shared `spawn_blocking`/executor primitive
        // itself) isn't used here: a tiny local PNG decode can complete
        // faster than this test can even register a listener after
        // `with_rules`'s own internal first `update` already started it,
        // an inherent race for anything this fast rather than a real bug
        // -- polling `update` directly still exercises the real
        // background thread and the real fix-up pass, just without
        // depending on that ordering.
        let mut runtime = UiRuntime::with_rules(Vec::new(), root, viewport());

        let find_img = |runtime: &UiRuntime| -> NodeId {
            let (arena, ..) = runtime.geometry();
            arena
                .find(|arena, id| arena.id_attr(id) == Some("pic"))
                .expect("root always renders the #pic img")
        };
        let img = find_img(&runtime);
        let (_, _, layouts) = runtime.geometry();
        assert_eq!(
            (layouts[&img].width, layouts[&img].height),
            (100.0, 0.0),
            "no intrinsic size decoded yet -- fills the 100px viewport width \
             (real CSS's own 'still loading' case), zero height (no ratio either)"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut resized = false;
        while std::time::Instant::now() < deadline {
            runtime.clear_dirty();
            runtime.update(viewport());
            let img = find_img(&runtime);
            let (_, _, layouts) = runtime.geometry();
            if (layouts[&img].width, layouts[&img].height) == (17.0, 9.0) {
                resized = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            resized,
            "the <img> should resize to its real 17x9 intrinsic size once the real \
             background PNG decode completes"
        );
    }

    /// the loading contract's own acceptance requirement: "an externally
    /// completed future reveals content without a manual update, click,
    /// or resize." Same shape as the resource-only version above, with a
    /// `loading_boundary` deciding between a fallback and the real
    /// content instead of a component branching on `Resource` itself.
    #[test]
    fn a_loading_boundary_reveals_content_on_a_real_background_completion() {
        let root = || {
            let resource = use_resource("key", |_| {
                florui_reactive::blocking::spawn_blocking(|| {
                    std::thread::sleep(std::time::Duration::from_millis(30));
                    Ok::<i32, &'static str>(42)
                })
            });
            florui_reactive::loading_boundary(
                &[&resource],
                |_refreshing| view! { <div id="status">{"content"}</div> },
                || view! { <div id="status">{"fallback"}</div> },
            )
        };

        let mut runtime = UiRuntime::with_rules(Vec::new(), root, viewport());
        let (needs_update_tx, needs_update_rx) = std::sync::mpsc::channel();
        runtime.on_needs_update(move || {
            let _ = needs_update_tx.send(());
        });
        if runtime.is_dirty() {
            runtime.clear_dirty();
            runtime.update(viewport());
        }
        assert_eq!(status_text(find_status(&runtime), &runtime), "fallback");

        needs_update_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the background thread's completion must reach on_needs_update on its own");
        // As above: this render's own snapshot still reads the
        // pre-completion state.
        runtime.update(viewport());
        assert!(runtime.is_dirty());
        runtime.clear_dirty();
        runtime.update(viewport());
        assert_eq!(status_text(find_status(&runtime), &runtime), "content");
    }

    #[test]
    fn use_committed_size_notifies_after_layout_and_coalesces_unchanged_sizes() {
        let sizes = Rc::new(RefCell::new(Vec::<(f32, f32)>::new()));
        let sizes_for_root = Rc::clone(&sizes);

        let root = move || {
            let sizes = Rc::clone(&sizes_for_root);
            let long = use_signal(|| false);
            let toggle = long.clone();
            let text = if long.get() {
                "a much longer run of text than before"
            } else {
                "short"
            };
            use_committed_size("box", move |w, h| sizes.borrow_mut().push((w, h)));
            view! {
                <div>
                    <div id="box">{text}</div>
                    <button onclick={move || toggle.set(true)} />
                </div>
            }
        };

        let mut runtime = UiRuntime::with_rules(Vec::new(), root, Size::MAX_CONTENT);
        assert_eq!(
            sizes.borrow().len(),
            1,
            "the very first layout already has a committed size to report"
        );
        let (short_width, _) = sizes.borrow()[0];

        let button = runtime
            .geometry()
            .0
            .find(|a, id| a.tag(id) == "button")
            .unwrap();
        runtime.dispatch_click(button);
        runtime.update(Size::MAX_CONTENT);

        assert_eq!(
            sizes.borrow().len(),
            2,
            "the text grew wider, so the observer must fire again"
        );
        let (long_width, _) = sizes.borrow()[1];
        assert!(
            long_width > short_width,
            "the longer text must measure wider than the short one"
        );

        // Nothing changed this time — must not renotify.
        runtime.update(Size::MAX_CONTENT);
        assert_eq!(
            sizes.borrow().len(),
            2,
            "an unchanged committed size must not renotify"
        );
    }

    #[test]
    fn use_committed_size_stops_firing_once_its_scope_unmounts() {
        let sizes = Rc::new(RefCell::new(0));
        let sizes_for_root = Rc::clone(&sizes);
        let show = Rc::new(Cell::new(true));
        let show_for_root = Rc::clone(&show);

        let root = move || {
            let sizes = Rc::clone(&sizes_for_root);
            if show_for_root.get() {
                use_child_scope_keyed("observed", move || {
                    use_committed_size("box", move |_, _| *sizes.borrow_mut() += 1);
                    view! { <div id="box">{"content"}</div> }
                })
            } else {
                view! { <div /> }
            }
        };

        let mut runtime = UiRuntime::with_rules(Vec::new(), root, viewport());
        assert_eq!(*sizes.borrow(), 1);

        show.set(false);
        runtime.update(viewport());
        assert_eq!(
            *sizes.borrow(),
            1,
            "unmounting the observing scope must dispose its attachment, not fire it again"
        );
    }

    #[test]
    fn commit_value_writes_through_a_binding_when_the_node_declared_one() {
        let requests: Rc<RefCell<Vec<String>>> = Rc::default();
        let recorded = requests.clone();
        let binding = Binding::new("old".to_string(), move |value| {
            recorded.borrow_mut().push(value)
        });
        let runtime = UiRuntime::with_rules(
            Vec::new(),
            move || view! { <input type="text" id="a" value={binding.clone()} /> },
            viewport(),
        );
        runtime.commit_value(node_id(&runtime, "a"), "new".to_string());
        assert_eq!(*requests.borrow(), vec!["new".to_string()]);
    }

    #[test]
    fn commit_value_calls_the_explicit_handler_when_there_is_no_binding() {
        let received: Rc<RefCell<Vec<String>>> = Rc::default();
        let recorded = received.clone();
        let runtime = UiRuntime::with_rules(
            Vec::new(),
            move || {
                let recorded = recorded.clone();
                view! {
                    <input
                        type="text"
                        id="a"
                        value={"old".to_string()}
                        oninput={move |value: String| recorded.borrow_mut().push(value)}
                    />
                }
            },
            viewport(),
        );
        runtime.commit_value(node_id(&runtime, "a"), "new".to_string());
        assert_eq!(*received.borrow(), vec!["new".to_string()]);
    }

    fn range_runtime(min: f32, max: f32, step: f32, value: f32) -> UiRuntime {
        UiRuntime::with_rules(
            Vec::new(),
            move || {
                view! {
                    <input
                        id="r"
                        type="range"
                        min={min.to_string()}
                        max={max.to_string()}
                        step={step.to_string()}
                        value={value.to_string()}
                        oninput={|_: String| {}}
                    />
                }
            },
            viewport(),
        )
    }

    /// A 100px-wide track at the layout root's own origin (`x = 0`), so a
    /// `cursor_x` of e.g. `25.0` is directly `25%` of the track — chosen to
    /// make the geometry math in each assertion read as the percentage
    /// itself.
    fn range_drag_runtime(
        min: f32,
        max: f32,
        step: f32,
        value: f32,
        requests: Rc<RefCell<Vec<String>>>,
    ) -> UiRuntime {
        UiRuntime::new(
            ".track { width: 100px; height: 20px; border-width: 0px; padding-top: 0px; padding-right: 0px; padding-bottom: 0px; padding-left: 0px; }",
            move || {
                let requests = requests.clone();
                view! {
                    <input
                        id="r"
                        class="track"
                        type="range"
                        min={min.to_string()}
                        max={max.to_string()}
                        step={step.to_string()}
                        value={value.to_string()}
                        oninput={move |v: String| requests.borrow_mut().push(v)}
                    />
                }
            },
            viewport(),
        )
        .unwrap()
    }

    /// `step_range_value` reads and writes through the owner's own value
    /// (via [`UiRuntime::commit_value`], same as any controlled
    /// component), so a runtime whose `oninput` never accepts the request
    /// never actually moves — each case here checks one interaction
    /// against its own starting state, matching how `Switch`'s own tests
    /// each check one interaction rather than chaining several against a
    /// runtime that has nothing accepting them.
    #[test]
    fn step_range_value_moves_by_one_step_in_either_direction() {
        let runtime = range_runtime(0.0, 10.0, 1.0, 5.0);
        let node = node_id(&runtime, "r");
        assert_eq!(
            runtime.step_range_value(node, RangeStep::SmallIncrement),
            Some(6.0)
        );
        assert_eq!(
            runtime.step_range_value(node, RangeStep::SmallDecrement),
            Some(4.0)
        );
    }

    #[test]
    fn step_range_value_clamps_at_the_bounds_and_is_a_no_op_past_them() {
        let at_max = range_runtime(0.0, 10.0, 1.0, 10.0);
        let node = node_id(&at_max, "r");
        assert_eq!(
            at_max.step_range_value(node, RangeStep::SmallIncrement),
            None,
            "already at max"
        );

        let at_min = range_runtime(0.0, 10.0, 1.0, 0.0);
        let node = node_id(&at_min, "r");
        assert_eq!(
            at_min.step_range_value(node, RangeStep::SmallDecrement),
            None,
            "already at min"
        );
    }

    #[test]
    fn step_range_value_home_and_end_jump_to_min_and_max() {
        let runtime = range_runtime(0.0, 10.0, 1.0, 5.0);
        let node = node_id(&runtime, "r");
        assert_eq!(runtime.step_range_value(node, RangeStep::Min), Some(0.0));
        assert_eq!(runtime.step_range_value(node, RangeStep::Max), Some(10.0));
    }

    #[test]
    fn start_range_drag_jumps_directly_to_the_cursors_own_position() {
        // Matches Chrome: mousedown alone (no move yet) already jumps the
        // value to wherever the cursor landed, on the 0-100 range a 100px
        // track makes `cursor_x` read as a percentage directly.
        let requests = Rc::default();
        let mut runtime = range_drag_runtime(0.0, 100.0, 1.0, 30.0, Rc::clone(&requests));
        let node = node_id(&runtime, "r");
        assert_eq!(runtime.start_range_drag(node, 70.0), Some(70.0));
        assert_eq!(*requests.borrow(), vec!["70".to_string()]);
    }

    #[test]
    fn continue_range_drag_snaps_to_step_and_dedupes_an_unchanged_value() {
        // A real accepting owner, re-rendering between steps like a real
        // controlled app does -- only then does "the same requested value
        // twice" become observable at all (a static arena's own value
        // never moves, so it can't demonstrate a dedupe against it).
        let requests: Rc<RefCell<Vec<f32>>> = Rc::default();
        let recorded = requests.clone();
        let value = Rc::new(Cell::new(0.0_f32));
        let stored = value.clone();
        let mut runtime = UiRuntime::new(
            ".track { width: 100px; height: 20px; border-width: 0px; padding-top: 0px; padding-right: 0px; padding-bottom: 0px; padding-left: 0px; }",
            move || {
                let current = value.get();
                let value = value.clone();
                let recorded = recorded.clone();
                view! {
                    <input
                        id="r"
                        class="track"
                        type="range"
                        min="0"
                        max="100"
                        step="10"
                        value={current.to_string()}
                        oninput={move |v: String| {
                            let parsed: f32 = v.parse().unwrap();
                            recorded.borrow_mut().push(parsed);
                            value.set(parsed);
                        }}
                    />
                }
            },
            viewport(),
        )
        .unwrap();
        let node = node_id(&runtime, "r");

        // step=10: cursor at 24% snaps to the nearest multiple of 10 (20).
        assert_eq!(runtime.start_range_drag(node, 24.0), Some(20.0));
        runtime.update(viewport());
        assert_eq!(*requests.borrow(), vec![20.0]);

        // 21% also snaps to 20 -- now that the owner's own value really is
        // 20, this must not request it again.
        assert_eq!(runtime.continue_range_drag(21.0), None);
        assert_eq!(*requests.borrow(), vec![20.0]);

        assert_eq!(runtime.continue_range_drag(85.0), Some(90.0));
        assert_eq!(*requests.borrow(), vec![20.0, 90.0]);
        assert_eq!(
            stored.get(),
            90.0,
            "the binding's own storage updates synchronously"
        );
    }

    #[test]
    fn continue_range_drag_clamps_past_either_end_of_the_track() {
        let requests = Rc::default();
        let mut runtime = range_drag_runtime(0.0, 100.0, 1.0, 50.0, Rc::clone(&requests));
        let node = node_id(&runtime, "r");
        runtime.start_range_drag(node, -40.0);
        assert_eq!(*requests.borrow(), vec!["0".to_string()], "clamped to min");
        assert_eq!(
            runtime.continue_range_drag(500.0),
            Some(100.0),
            "clamped to max"
        );
    }

    #[test]
    fn continue_and_end_range_drag_without_a_drag_open_are_a_no_op() {
        let requests = Rc::default();
        let mut runtime = range_drag_runtime(0.0, 100.0, 1.0, 50.0, requests);
        assert_eq!(runtime.continue_range_drag(70.0), None);
        runtime.end_range_drag(); // must not panic with nothing to end
    }

    #[test]
    fn end_range_drag_fires_commit_exactly_once_and_clears_the_drag() {
        let commits = Rc::new(Cell::new(0));
        let recorded = commits.clone();
        let requests = Rc::default();
        let mut runtime = UiRuntime::new(
            ".track { width: 100px; height: 20px; border-width: 0px; padding-top: 0px; padding-right: 0px; padding-bottom: 0px; padding-left: 0px; }",
            move || {
                let recorded = recorded.clone();
                let requests: Rc<RefCell<Vec<String>>> = Rc::clone(&requests);
                view! {
                    <input
                        id="r"
                        class="track"
                        type="range"
                        min="0"
                        max="100"
                        value={"30".to_string()}
                        oninput={move |v: String| requests.borrow_mut().push(v)}
                        oncommit={move || recorded.set(recorded.get() + 1)}
                    />
                }
            },
            viewport(),
        )
        .unwrap();
        let node = node_id(&runtime, "r");
        runtime.start_range_drag(node, 60.0);
        runtime.continue_range_drag(80.0);
        assert_eq!(commits.get(), 0, "commit only fires once the drag ends");
        runtime.end_range_drag();
        assert_eq!(commits.get(), 1);
        // A second end (nothing open) must not fire it again.
        runtime.end_range_drag();
        assert_eq!(commits.get(), 1);
    }

    #[test]
    fn step_range_value_large_step_is_ten_times_the_small_step() {
        // Matches both cases measured against Chrome: default step 1 on a
        // 0-100 range moves 10 on Page; a custom step 10 moves 100.
        let default_step = range_runtime(0.0, 100.0, 1.0, 30.0);
        let node = node_id(&default_step, "r");
        assert_eq!(
            default_step.step_range_value(node, RangeStep::LargeIncrement),
            Some(40.0)
        );

        let custom_step = range_runtime(0.0, 100.0, 10.0, 0.0);
        let node = node_id(&custom_step, "r");
        assert_eq!(
            custom_step.step_range_value(node, RangeStep::LargeIncrement),
            Some(100.0)
        );
    }

    #[test]
    fn step_range_value_requests_the_new_value_through_the_value_attribute() {
        let requests: Rc<RefCell<Vec<String>>> = Rc::default();
        let recorded = requests.clone();
        let runtime = UiRuntime::with_rules(
            Vec::new(),
            move || {
                let recorded = recorded.clone();
                view! {
                    <input
                        id="r"
                        type="range"
                        min="0"
                        max="10"
                        value={"5".to_string()}
                        oninput={move |value: String| recorded.borrow_mut().push(value)}
                    />
                }
            },
            viewport(),
        );
        runtime.step_range_value(node_id(&runtime, "r"), RangeStep::SmallIncrement);
        assert_eq!(*requests.borrow(), vec!["6".to_string()]);
    }

    fn three_buttons_runtime() -> UiRuntime {
        UiRuntime::with_rules(
            Vec::new(),
            || {
                view! {
                    <div>
                        <button id="a">{"A"}</button>
                        <button id="b">{"B"}</button>
                        <button id="c">{"C"}</button>
                    </div>
                }
            },
            viewport(),
        )
    }

    fn node_id(runtime: &UiRuntime, id_attr: &str) -> NodeId {
        let (arena, ..) = runtime.geometry();
        arena
            .find(|arena, node| arena.id_attr(node) == Some(id_attr))
            .unwrap()
    }

    fn label_runtime(disabled: bool, clicks: std::rc::Rc<std::cell::Cell<u32>>) -> UiRuntime {
        UiRuntime::with_rules(
            Vec::new(),
            move || {
                let clicks = clicks.clone();
                view! {
                    <div id="wrapper">
                        <input
                            id="box"
                            type="checkbox"
                            disabled={disabled}
                            onclick={move || clicks.set(clicks.get() + 1)}
                        />
                        <label id="named" for="box">{"Box"}</label>
                        <label id="dangling" for="missing">{"Nothing"}</label>
                        <label id="wrapping"><span id="inner">{"x"}</span></label>
                    </div>
                }
            },
            viewport(),
        )
    }

    #[test]
    fn a_label_for_activates_its_control() {
        let clicks = std::rc::Rc::new(std::cell::Cell::new(0));
        let runtime = label_runtime(false, clicks.clone());
        let (label, control) = (node_id(&runtime, "named"), node_id(&runtime, "box"));
        assert_eq!(runtime.activation_target(label), Some(control));
        runtime.dispatch_click(runtime.activation_target(label).unwrap());
        assert_eq!(
            clicks.get(),
            1,
            "the label click reached the input's handler"
        );
    }

    #[test]
    fn a_label_for_a_disabled_control_activates_nothing() {
        let runtime = label_runtime(true, std::rc::Rc::default());
        assert_eq!(runtime.activation_target(node_id(&runtime, "named")), None);
    }

    #[test]
    fn a_label_without_a_matching_control_and_a_plain_wrapper_activate_themselves() {
        let runtime = label_runtime(false, std::rc::Rc::default());
        for id in ["dangling", "wrapping", "wrapper", "inner"] {
            let node = node_id(&runtime, id);
            assert_eq!(runtime.activation_target(node), Some(node), "{id}");
        }
    }

    fn nested_label_runtime() -> UiRuntime {
        UiRuntime::with_rules(
            Vec::new(),
            || {
                view! {
                    <div id="plain-wrapper">
                        <label id="wrap">
                            <input id="wrapped" type="checkbox" />
                            <span id="wrap-text">{"Wrapped"}</span>
                        </label>
                        <label id="mismatch" for="nothing">
                            <input id="ignored" type="checkbox" />
                        </label>
                        <label id="with-button"><button id="inside">{"go"}</button></label>
                        <label id="off-target"><input id="disabled-one" type="checkbox" disabled="true" /></label>
                    </div>
                }
            },
            viewport(),
        )
    }

    #[test]
    fn a_label_wrapping_a_control_activates_it_from_the_label_and_from_inside_it() {
        let runtime = nested_label_runtime();
        let control = node_id(&runtime, "wrapped");
        for id in ["wrap", "wrap-text"] {
            let node = node_id(&runtime, id);
            assert_eq!(runtime.activation_target(node), Some(control), "{id}");
        }
    }

    #[test]
    fn a_label_with_a_for_never_falls_back_to_a_descendant_control() {
        let runtime = nested_label_runtime();
        let label = node_id(&runtime, "mismatch");
        assert_eq!(runtime.activation_target(label), Some(label));
    }

    #[test]
    fn a_control_inside_a_label_is_its_own_target_and_a_disabled_one_activates_nothing() {
        let runtime = nested_label_runtime();
        let button = node_id(&runtime, "inside");
        assert_eq!(runtime.activation_target(button), Some(button));
        assert_eq!(
            runtime.activation_target(node_id(&runtime, "off-target")),
            None
        );
        let plain = node_id(&runtime, "plain-wrapper");
        assert_eq!(runtime.activation_target(plain), Some(plain));
    }

    fn radio_group_runtime() -> UiRuntime {
        UiRuntime::with_rules(
            Vec::new(),
            || {
                view! {
                    <div>
                        <button id="before">{"B"}</button>
                        <input id="r1" type="radio" name="g" />
                        <input id="r2" type="radio" name="g" checked="true" />
                        <input id="r3" type="radio" name="g" />
                        <button id="after">{"A"}</button>
                    </div>
                }
            },
            viewport(),
        )
    }

    /// `selected_id` picks which option starts selected — separate
    /// runtimes rather than one mutated in place, since this crate's
    /// controlled contract never writes `selected` back itself (see
    /// `select_runtime`'s own callers).
    fn select_runtime_selecting(open: bool, selected_id: &'static str) -> UiRuntime {
        UiRuntime::with_rules(
            Vec::new(),
            move || {
                view! {
                    <select id="size" open={open}>
                        <option id="s" value="s" selected={selected_id == "s"}>{"Small"}</option>
                        <option id="m" value="m" selected={selected_id == "m"}>{"Medium"}</option>
                        <option id="l" value="l" selected={selected_id == "l"}>{"Large"}</option>
                    </select>
                }
            },
            viewport(),
        )
    }

    #[test]
    fn step_select_option_moves_from_the_selected_option() {
        let mut runtime = select_runtime_selecting(true, "m");
        runtime.set_focused(Some(node_id(&runtime, "size")), true);
        let (s, l) = (node_id(&runtime, "s"), node_id(&runtime, "l"));
        assert_eq!(runtime.step_select_option(1), Some(l));
        assert_eq!(runtime.step_select_option(-1), Some(s));
    }

    #[test]
    fn step_select_option_wraps_at_each_end() {
        let mut runtime = select_runtime_selecting(true, "l");
        runtime.set_focused(Some(node_id(&runtime, "size")), true);
        assert_eq!(
            runtime.step_select_option(1),
            Some(node_id(&runtime, "s")),
            "wraps past the last option"
        );

        let mut runtime = select_runtime_selecting(true, "s");
        runtime.set_focused(Some(node_id(&runtime, "size")), true);
        assert_eq!(
            runtime.step_select_option(-1),
            Some(node_id(&runtime, "l")),
            "wraps past the first option"
        );
    }

    #[test]
    fn step_select_option_is_none_for_a_closed_select() {
        let mut runtime = select_runtime_selecting(false, "m");
        runtime.set_focused(Some(node_id(&runtime, "size")), true);
        assert_eq!(runtime.step_select_option(1), None);
    }

    #[test]
    fn a_closed_selects_auto_width_matches_its_widest_option_regardless_of_which_is_selected() {
        let width_of = |runtime: &UiRuntime| {
            let (arena, _, layouts) = runtime.geometry();
            let select = arena.find(|a, id| a.tag(id) == "select").unwrap();
            layouts[&select].width
        };
        // "Small" is narrower than "Medium" -- selecting it must not
        // shrink the box below what "Medium" (the widest label) needs.
        let narrow_selected = select_runtime_selecting(false, "s");
        let wide_selected = select_runtime_selecting(false, "m");
        assert_eq!(width_of(&narrow_selected), width_of(&wide_selected));
    }

    #[test]
    fn an_explicit_author_width_is_never_overridden_by_the_widest_option() {
        let runtime = UiRuntime::with_rules(
            florui_style::parse_stylesheet("select { width: 10px; }").unwrap(),
            || {
                view! {
                    <select id="size">
                        <option value="s">{"Small"}</option>
                        <option value="xl">{"Extra Large Option"}</option>
                    </select>
                }
            },
            viewport(),
        );
        let (arena, styles, layouts) = runtime.geometry();
        let select = arena.roots()[0];
        let style = &styles[&select];
        // The author only overrides `width`, so the UA default stylesheet's
        // own `padding-left` (see `default_stylesheet.rs`) still applies on
        // top of it.
        let expected = 10.0
            + style.padding.left
            + style.padding.right
            + style.border.left.width
            + style.border.right.width;
        assert_eq!(
            layouts[&select].width, expected,
            "author width wins even though \"Extra Large Option\" needs more room"
        );
    }

    #[test]
    fn dispatch_click_on_a_disabled_select_does_not_fire() {
        let clicks = std::rc::Rc::new(std::cell::Cell::new(0));
        let for_click = clicks.clone();
        let runtime = UiRuntime::with_rules(
            Vec::new(),
            move || {
                let for_click = for_click.clone();
                view! {
                    <select id="size" disabled="true" onclick={move || for_click.set(for_click.get() + 1)} />
                }
            },
            viewport(),
        );
        runtime.dispatch_click(node_id(&runtime, "size"));
        assert_eq!(clicks.get(), 0);
    }

    #[test]
    fn tab_passes_through_a_radio_group_once_at_its_checked_member() {
        let mut runtime = radio_group_runtime();
        let (before, r2, after) = (
            node_id(&runtime, "before"),
            node_id(&runtime, "r2"),
            node_id(&runtime, "after"),
        );
        assert!(runtime.focus_next());
        assert_eq!(runtime.focused(), Some(before));
        assert!(runtime.focus_next());
        assert_eq!(runtime.focused(), Some(r2));
        assert!(runtime.focus_next());
        assert_eq!(runtime.focused(), Some(after));
        assert!(runtime.focus_previous());
        assert_eq!(runtime.focused(), Some(r2));
    }

    #[test]
    fn step_radio_group_moves_focus_within_the_group_and_wraps() {
        let mut runtime = radio_group_runtime();
        let (r1, r2, r3) = (
            node_id(&runtime, "r1"),
            node_id(&runtime, "r2"),
            node_id(&runtime, "r3"),
        );
        runtime.set_focused(Some(r2), true);
        assert_eq!(runtime.step_radio_group(1), Some(r3));
        assert_eq!(runtime.focused(), Some(r3));
        assert_eq!(runtime.step_radio_group(1), Some(r1));
        assert_eq!(runtime.step_radio_group(-1), Some(r3));
    }

    #[test]
    fn step_radio_group_is_a_no_op_off_a_radio() {
        let mut runtime = radio_group_runtime();
        let before = node_id(&runtime, "before");
        runtime.set_focused(Some(before), true);
        assert_eq!(runtime.step_radio_group(1), None);
        assert_eq!(runtime.focused(), Some(before));
    }

    #[test]
    fn focus_next_moves_forward_through_document_order_and_wraps() {
        let mut runtime = three_buttons_runtime();
        let (a, b, c) = (
            node_id(&runtime, "a"),
            node_id(&runtime, "b"),
            node_id(&runtime, "c"),
        );

        assert!(runtime.focus_next());
        assert_eq!(runtime.focused(), Some(a));
        assert!(runtime.focus_next());
        assert_eq!(runtime.focused(), Some(b));
        assert!(runtime.focus_next());
        assert_eq!(runtime.focused(), Some(c));
        assert!(
            runtime.focus_next(),
            "tabbing past the last element must wrap back to the first"
        );
        assert_eq!(runtime.focused(), Some(a));
    }

    #[test]
    fn focus_previous_moves_backward_and_wraps() {
        let mut runtime = three_buttons_runtime();
        let (a, c) = (node_id(&runtime, "a"), node_id(&runtime, "c"));

        assert!(
            runtime.focus_previous(),
            "shift-tabbing with nothing focused must land on the last element"
        );
        assert_eq!(runtime.focused(), Some(c));
        assert!(runtime.focus_previous());
        assert_eq!(runtime.focused(), Some(node_id(&runtime, "b")));
        assert!(runtime.focus_previous());
        assert_eq!(runtime.focused(), Some(a));
        assert!(
            runtime.focus_previous(),
            "shift-tabbing past the first element must wrap to the last"
        );
        assert_eq!(runtime.focused(), Some(c));
    }

    #[test]
    fn focus_applies_only_to_the_focused_button() {
        let mut runtime = three_buttons_runtime();
        runtime.set_rules(
            florui_style::parse_stylesheet(
                "button { background-color: #111111; } button:focus { background-color: #222222; }",
            )
            .unwrap(),
        );
        let a = node_id(&runtime, "a");
        let b = node_id(&runtime, "b");
        runtime.set_focused(Some(a), true);
        runtime.update(viewport());

        let (_, styles, _) = runtime.geometry();
        assert_eq!(styles[&a].background_color, Rgba::opaque(0x22, 0x22, 0x22));
        assert_eq!(styles[&b].background_color, Rgba::opaque(0x11, 0x11, 0x11));
    }

    #[test]
    fn focus_visible_applies_after_keyboard_focus_but_not_a_pointer_click() {
        let mut runtime = three_buttons_runtime();
        runtime.set_rules(
            florui_style::parse_stylesheet(
                "button { background-color: #111111; } button:focus-visible { background-color: #333333; }",
            )
            .unwrap(),
        );
        let a = node_id(&runtime, "a");

        runtime.set_focused(Some(a), false);
        runtime.update(viewport());
        let (_, styles, _) = runtime.geometry();
        assert_eq!(
            styles[&a].background_color,
            Rgba::opaque(0x11, 0x11, 0x11),
            ":focus-visible must not match a pointer-origin focus"
        );

        runtime.set_focused(Some(a), true);
        runtime.update(viewport());
        let (_, styles, _) = runtime.geometry();
        assert_eq!(styles[&a].background_color, Rgba::opaque(0x33, 0x33, 0x33));
    }

    #[test]
    fn removing_the_focused_element_clears_focus_without_panicking() {
        let show = Rc::new(Cell::new(true));
        let show_for_root = Rc::clone(&show);
        let mut runtime = UiRuntime::with_rules(
            Vec::new(),
            move || {
                if show_for_root.get() {
                    view! { <button id="target">{"Go"}</button> }
                } else {
                    view! { <div /> }
                }
            },
            viewport(),
        );
        let target = node_id(&runtime, "target");
        runtime.set_focused(Some(target), true);
        assert_eq!(runtime.focused(), Some(target));

        show.set(false);
        runtime.update(viewport());

        assert_eq!(
            runtime.focused(),
            None,
            "focus must clear outright once its element is gone"
        );
    }

    #[test]
    fn focus_next_skips_a_disabled_button() {
        let mut runtime = UiRuntime::with_rules(
            Vec::new(),
            || {
                view! {
                    <div>
                        <button id="a">{"A"}</button>
                        <button id="b" disabled="true">{"B"}</button>
                        <button id="c">{"C"}</button>
                    </div>
                }
            },
            viewport(),
        );
        let (a, c) = (node_id(&runtime, "a"), node_id(&runtime, "c"));

        assert!(runtime.focus_next());
        assert_eq!(runtime.focused(), Some(a));
        assert!(runtime.focus_next());
        assert_eq!(
            runtime.focused(),
            Some(c),
            "tab must skip the disabled button in between"
        );
    }

    #[test]
    fn a_button_disabled_at_runtime_loses_focus() {
        let disabled = Rc::new(Cell::new(false));
        let disabled_for_root = Rc::clone(&disabled);
        let mut runtime = UiRuntime::with_rules(
            Vec::new(),
            move || {
                let disabled = disabled_for_root.get();
                view! { <button id="target" disabled={disabled}>{"Go"}</button> }
            },
            viewport(),
        );
        let target = node_id(&runtime, "target");
        runtime.set_focused(Some(target), true);
        assert_eq!(runtime.focused(), Some(target));

        disabled.set(true);
        runtime.update(viewport());

        assert_eq!(
            runtime.focused(),
            None,
            "a button must lose focus the instant it becomes disabled -- resolve_focus's \
             existing no-longer-resolves clearing already covers this"
        );
    }

    #[test]
    fn a_portal_appears_as_an_overlay_root_only_while_rendered() {
        let show = Rc::new(Cell::new(false));
        let show_for_root = Rc::clone(&show);
        let mut runtime = UiRuntime::with_rules(
            Vec::new(),
            move || {
                let overlay = if show_for_root.get() {
                    view! { <Portal><div id="overlay" /></Portal> }
                } else {
                    view! { <div /> }
                };
                view! {
                    <div>
                        <div id="doc" />
                        {overlay}
                    </div>
                }
            },
            viewport(),
        );

        assert!(
            runtime.geometry().0.overlay_roots().is_empty(),
            "nothing rendered a Portal yet"
        );

        show.set(true);
        runtime.update(viewport());
        let (arena, _, layouts) = runtime.geometry();
        assert_eq!(arena.overlay_roots().len(), 1);
        let overlay_root = arena.overlay_roots()[0];
        assert_eq!(arena.id_attr(overlay_root), Some("overlay"));
        assert!(layouts.contains_key(&overlay_root));

        show.set(false);
        runtime.update(viewport());
        assert!(
            runtime.geometry().0.overlay_roots().is_empty(),
            "a fresh render with no Portal in the tree must leave no stale overlay root"
        );

        show.set(true);
        runtime.update(viewport());
        assert_eq!(
            runtime.geometry().0.overlay_roots().len(),
            1,
            "a Portal rendered again after being hidden must reappear"
        );
    }

    #[test]
    fn dispatch_click_on_portal_content_does_not_reach_a_backdrop_beneath_it() {
        let backdrop_clicked = Rc::new(Cell::new(false));
        let backdrop_clicked_in_handler = Rc::clone(&backdrop_clicked);
        let content_clicked = Rc::new(Cell::new(false));
        let content_clicked_in_handler = Rc::clone(&content_clicked);

        let runtime = UiRuntime::with_rules(
            Vec::new(),
            move || {
                let backdrop_clicked = Rc::clone(&backdrop_clicked_in_handler);
                let content_clicked = Rc::clone(&content_clicked_in_handler);
                view! {
                    <Portal>
                        <div id="backdrop" onclick={move || backdrop_clicked.set(true)}>
                            <div id="content" onclick={move || content_clicked.set(true)} />
                        </div>
                    </Portal>
                }
            },
            viewport(),
        );

        let content = node_id(&runtime, "content");
        runtime.dispatch_click(content);

        assert!(content_clicked.get());
        assert!(
            !backdrop_clicked.get(),
            "dispatch_click targets exactly the hit-tested node -- no bubbling to an ancestor, \
             which is what makes a plain backdrop onclick safe to use for dismissal"
        );
    }

    #[test]
    fn dispatch_event_calls_the_nodes_own_handler_for_that_event_name() {
        let closed = Rc::new(Cell::new(false));
        let closed_in_handler = Rc::clone(&closed);
        let runtime = UiRuntime::with_rules(
            Vec::new(),
            move || {
                let closed = Rc::clone(&closed_in_handler);
                view! { <div id="target" onclose={move || closed.set(true)} /> }
            },
            viewport(),
        );
        let target = node_id(&runtime, "target");

        runtime.dispatch_event(target, "close");

        assert!(closed.get());
    }

    fn dialog_runtime(open: Rc<Cell<bool>>, remove_trigger: Rc<Cell<bool>>) -> UiRuntime {
        UiRuntime::with_rules(
            Vec::new(),
            move || {
                let dialog = if open.get() {
                    view! {
                        <Dialog label={"Test dialog".to_string()} onclose={Handler::new(|| {})}>
                            <button id="first-inside">{"First inside"}</button>
                            <button id="second-inside">{"Second inside"}</button>
                        </Dialog>
                    }
                } else {
                    view! { <div /> }
                };
                let trigger = if remove_trigger.get() {
                    view! { <div /> }
                } else {
                    view! { <button id="trigger">{"Trigger"}</button> }
                };
                view! {
                    <div>
                        {trigger}
                        {dialog}
                    </div>
                }
            },
            viewport(),
        )
    }

    #[test]
    fn an_open_dialog_produces_exactly_one_overlay_root() {
        let open = Rc::new(Cell::new(true));
        let mut runtime = dialog_runtime(open, Rc::new(Cell::new(false)));
        runtime.update(viewport());

        let (arena, ..) = runtime.geometry();
        assert_eq!(arena.overlay_roots().len(), 1);
        let root = arena.overlay_roots()[0];
        assert!(
            arena
                .classes(root)
                .iter()
                .any(|class| class == crate::components::dialog::MODAL_ROOT_CLASS),
            "Dialog's own single-level Portal usage must be unaffected by nested-portal extraction"
        );
    }

    #[test]
    fn opening_a_modal_dialog_saves_prior_focus_and_traps_focus_inside_it() {
        let open = Rc::new(Cell::new(false));
        let mut runtime = dialog_runtime(Rc::clone(&open), Rc::new(Cell::new(false)));

        let trigger = node_id(&runtime, "trigger");
        runtime.set_focused(Some(trigger), true);
        assert_eq!(runtime.focused(), Some(trigger));

        open.set(true);
        runtime.update(viewport());

        let first_inside = node_id(&runtime, "first-inside");
        assert_eq!(
            runtime.focused(),
            Some(first_inside),
            "focus must move to the modal's own first focusable descendant on open"
        );
    }

    #[test]
    fn tab_does_not_escape_a_modal_dialogs_own_content() {
        let open = Rc::new(Cell::new(true));
        let mut runtime = dialog_runtime(Rc::clone(&open), Rc::new(Cell::new(false)));
        runtime.update(viewport());

        let first_inside = node_id(&runtime, "first-inside");
        let second_inside = node_id(&runtime, "second-inside");
        assert_eq!(runtime.focused(), Some(first_inside));

        assert!(runtime.focus_next());
        assert_eq!(runtime.focused(), Some(second_inside));

        // Wraps back to the first inside element -- never escapes to
        // "trigger", which sits outside the modal's own content.
        assert!(runtime.focus_next());
        assert_eq!(runtime.focused(), Some(first_inside));
    }

    #[test]
    fn closing_a_modal_dialog_restores_focus_to_the_original_trigger() {
        let open = Rc::new(Cell::new(false));
        let mut runtime = dialog_runtime(Rc::clone(&open), Rc::new(Cell::new(false)));

        let trigger = node_id(&runtime, "trigger");
        runtime.set_focused(Some(trigger), true);

        open.set(true);
        runtime.update(viewport());
        assert_ne!(
            runtime.focused(),
            Some(trigger),
            "focus moved into the modal"
        );

        open.set(false);
        runtime.update(viewport());
        assert_eq!(
            runtime.focused(),
            Some(trigger),
            "closing the modal must restore focus to its original trigger"
        );
    }

    #[test]
    fn closing_a_modal_dialog_whose_trigger_was_removed_clears_focus() {
        let open = Rc::new(Cell::new(false));
        let remove_trigger = Rc::new(Cell::new(false));
        let mut runtime = dialog_runtime(Rc::clone(&open), Rc::clone(&remove_trigger));

        let trigger = node_id(&runtime, "trigger");
        runtime.set_focused(Some(trigger), true);

        open.set(true);
        runtime.update(viewport());

        remove_trigger.set(true);
        open.set(false);
        runtime.update(viewport());

        assert_eq!(
            runtime.focused(),
            None,
            "nothing focusable is left to fall back to"
        );
    }

    #[test]
    fn closing_a_modal_dialog_whose_trigger_was_removed_focuses_the_first_focusable() {
        let open = Rc::new(Cell::new(false));
        let remove_trigger = Rc::new(Cell::new(false));
        let (open_in, remove_in) = (Rc::clone(&open), Rc::clone(&remove_trigger));
        let mut runtime = UiRuntime::with_rules(
            Vec::new(),
            move || {
                let dialog = if open_in.get() {
                    view! {
                        <Dialog label={"Test dialog".to_string()} onclose={Handler::new(|| {})}>
                            <button id="inside">{"Inside"}</button>
                        </Dialog>
                    }
                } else {
                    view! { <div /> }
                };
                let trigger = if remove_in.get() {
                    view! { <div /> }
                } else {
                    view! { <button id="trigger">{"Trigger"}</button> }
                };
                view! {
                    <div>
                        <button id="first">{"First"}</button>
                        {trigger}
                        {dialog}
                    </div>
                }
            },
            viewport(),
        );
        let trigger = node_id(&runtime, "trigger");
        runtime.set_focused(Some(trigger), true);
        open.set(true);
        runtime.update(viewport());
        remove_trigger.set(true);
        open.set(false);
        runtime.update(viewport());

        assert_eq!(runtime.focused(), Some(node_id(&runtime, "first")));
    }

    fn nested_dialog_runtime(outer: Rc<Cell<bool>>, inner: Rc<Cell<bool>>) -> UiRuntime {
        UiRuntime::with_rules(
            Vec::new(),
            move || {
                let inner_dialog = if inner.get() {
                    view! {
                        <Dialog label={"Inner".to_string()} onclose={Handler::new(|| {})}>
                            <button id="inner-button">{"Inner"}</button>
                        </Dialog>
                    }
                } else {
                    view! { <div /> }
                };
                let outer_dialog = if outer.get() {
                    view! {
                        <Dialog label={"Outer".to_string()} onclose={Handler::new(|| {})}>
                            <button id="outer-button">{"Outer"}</button>
                            {inner_dialog}
                        </Dialog>
                    }
                } else {
                    view! { <div /> }
                };
                view! {
                    <div>
                        <button id="page-button">{"Page"}</button>
                        {outer_dialog}
                    </div>
                }
            },
            viewport(),
        )
    }

    #[test]
    fn a_dialog_opened_inside_a_dialog_owns_focus_and_hands_it_back_in_order() {
        let (outer, inner) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
        let mut runtime = nested_dialog_runtime(Rc::clone(&outer), Rc::clone(&inner));
        runtime.set_focused(Some(node_id(&runtime, "page-button")), true);

        outer.set(true);
        runtime.update(viewport());
        assert_eq!(runtime.focused(), Some(node_id(&runtime, "outer-button")));

        inner.set(true);
        runtime.update(viewport());
        assert_eq!(
            runtime.focused(),
            Some(node_id(&runtime, "inner-button")),
            "the innermost dialog takes focus"
        );
        runtime.focus_next();
        assert_eq!(
            runtime.focused(),
            Some(node_id(&runtime, "inner-button")),
            "Tab stays inside the innermost dialog"
        );

        inner.set(false);
        runtime.update(viewport());
        assert_eq!(
            runtime.focused(),
            Some(node_id(&runtime, "outer-button")),
            "closing the inner dialog returns to the outer one"
        );

        outer.set(false);
        runtime.update(viewport());
        assert_eq!(
            runtime.focused(),
            Some(node_id(&runtime, "page-button")),
            "closing the outer dialog returns to the page"
        );
    }
    #[test]
    fn closing_a_modal_opened_with_nothing_focused_leaves_nothing_focused() {
        let open = Rc::new(Cell::new(false));
        let mut runtime = dialog_runtime(Rc::clone(&open), Rc::new(Cell::new(false)));
        open.set(true);
        runtime.update(viewport());
        open.set(false);
        runtime.update(viewport());
        assert_eq!(runtime.focused(), None);
    }

    fn rows_runtime() -> UiRuntime {
        let root = || {
            crate::use_scroll_offset("box", |_, _| {});
            view! {
                <div id="box" class="box">
                    <div id="a" class="row" />
                    <div id="b" class="row" />
                    <div id="c" class="row" />
                </div>
            }
        };
        let rules = florui_style::parse_stylesheet(
            ".box { width: 50px; height: 50px; overflow-y: auto; } .row { height: 40px; }",
        )
        .unwrap();
        UiRuntime::with_rules(rules, root, viewport())
    }

    fn hit_id(runtime: &UiRuntime, x: f32, y: f32) -> Option<String> {
        let node = runtime.hit_test(x, y)?;
        let (arena, ..) = runtime.geometry();
        arena.id_attr(node).map(str::to_owned)
    }

    #[test]
    fn hit_testing_follows_the_scrolled_content_not_its_unscrolled_layout() {
        let runtime = rows_runtime();
        assert_eq!(hit_id(&runtime, 5.0, 5.0).as_deref(), Some("a"));

        // 45px of scroll puts row "b" (unscrolled y 40..80) under y = 5.
        runtime.scroll_registry().scroll_to("box", 0.0, 45.0);

        assert_eq!(hit_id(&runtime, 5.0, 5.0).as_deref(), Some("b"));
        // "b" now spans y -5..35 and "c" 35..75.
        assert_eq!(hit_id(&runtime, 5.0, 34.0).as_deref(), Some("b"));
        assert_eq!(hit_id(&runtime, 5.0, 36.0).as_deref(), Some("c"));
    }

    #[test]
    fn scrolled_content_is_still_clipped_to_its_container_when_hit_testing() {
        let runtime = rows_runtime();
        runtime.scroll_registry().scroll_to("box", 0.0, 45.0);

        assert_eq!(
            hit_id(&runtime, 5.0, 60.0),
            None,
            "below the 50px box nothing is hit, even though scrolled content reaches there"
        );
    }

    #[test]
    fn drawn_position_subtracts_every_scrolled_ancestors_offset() {
        let runtime = rows_runtime();
        let row_b = {
            let (arena, ..) = runtime.geometry();
            arena.find(|a, id| a.id_attr(id) == Some("b")).unwrap()
        };
        assert_eq!(runtime.drawn_position(row_b), (0.0, 40.0));

        runtime.scroll_registry().scroll_to("box", 0.0, 45.0);

        assert_eq!(runtime.drawn_position(row_b), (0.0, -5.0));
        let (arena, _, layouts) = runtime.geometry();
        assert_eq!(
            florui_layout::absolute_position(arena, layouts, row_b),
            (0.0, 40.0),
            "the layout itself stays unscrolled"
        );
    }

    fn overlay_scroll_runtime(
        positions: Rc<RefCell<Vec<(f32, f32)>>>,
        select_open: bool,
    ) -> UiRuntime {
        let root = move || {
            crate::use_scroll_offset("box", |_, _| {});
            let log = Rc::clone(&positions);
            crate::use_committed_position("anchor", move |x, y| log.borrow_mut().push((x, y)));
            view! {
                <div id="box" class="box">
                    <div class="spacer" />
                    <div id="anchor" class="anchor" />
                    <select id="sel" class="sel" open={select_open}>
                        <option id="o1" value="o1">{"One"}</option>
                    </select>
                    <div class="spacer" />
                    <div class="spacer" />
                </div>
            }
        };
        let rules = florui_style::parse_stylesheet(
            ".box { width: 200px; height: 100px; overflow-y: auto; } \
             .spacer { height: 80px; } .anchor { height: 20px; } .sel { width: 100px; height: 20px; }",
        )
        .unwrap();
        let viewport = Size {
            width: AvailableSpace::Definite(400.0),
            height: AvailableSpace::Definite(400.0),
        };
        UiRuntime::with_rules(rules, root, viewport)
    }

    const OVERLAY_VIEWPORT: Size<AvailableSpace> = Size {
        width: AvailableSpace::Definite(400.0),
        height: AvailableSpace::Definite(400.0),
    };

    #[test]
    fn a_position_observer_follows_its_element_when_a_scroll_moves_it() {
        let positions = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = overlay_scroll_runtime(Rc::clone(&positions), false);
        assert_eq!(positions.borrow().last(), Some(&(0.0, 80.0)));

        runtime.scroll_registry().wheel_scroll_by("box", 0.0, 30.0);
        runtime.reposition_for_scroll(OVERLAY_VIEWPORT);

        assert_eq!(
            positions.borrow().last(),
            Some(&(0.0, 50.0)),
            "reports where the anchor is drawn, not its unscrolled layout position"
        );
    }

    #[test]
    fn an_open_selects_dropdown_follows_its_trigger_when_a_scroll_moves_it() {
        let positions = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = overlay_scroll_runtime(positions, true);
        let content_top = |runtime: &UiRuntime| {
            let content = node_id(runtime, "sel-select-content");
            let (_, _, layouts) = runtime.geometry();
            layouts[&content].y
        };
        let trigger = node_id(&runtime, "sel");
        let gap_below_trigger =
            |runtime: &UiRuntime| content_top(runtime) - runtime.drawn_position(trigger).1;
        let (top_before, gap_before) = (content_top(&runtime), gap_below_trigger(&runtime));

        runtime.scroll_registry().wheel_scroll_by("box", 0.0, 30.0);
        runtime.reposition_for_scroll(OVERLAY_VIEWPORT);

        assert_eq!(
            content_top(&runtime),
            top_before - 30.0,
            "the dropdown moves with the 30px scroll"
        );
        assert_eq!(
            gap_below_trigger(&runtime),
            gap_before,
            "and stays the same distance under the trigger where it is drawn"
        );
    }

    #[test]
    fn a_range_drag_maps_the_pointer_onto_the_track_where_a_horizontal_scroll_drew_it() {
        let requests: Rc<RefCell<Vec<String>>> = Rc::default();
        let log = Rc::clone(&requests);
        let rules = florui_style::parse_stylesheet(
            ".box { width: 100px; height: 20px; overflow-x: auto; } \
             .track { display: block; width: 100px; height: 20px; margin-left: 150px; \
             border-width: 0px; padding-top: 0px; padding-right: 0px; padding-bottom: 0px; \
             padding-left: 0px; }",
        )
        .unwrap();
        let mut runtime = UiRuntime::with_rules(
            rules,
            move || {
                crate::use_scroll_offset("box", |_, _| {});
                let log = Rc::clone(&log);
                view! {
                    <div id="box" class="box">
                        <input id="r" class="track" type="range" min="0" max="100" step="1"
                            value={"0".to_string()}
                            oninput={move |v: String| log.borrow_mut().push(v)} />
                    </div>
                }
            },
            viewport(),
        );
        let node = node_id(&runtime, "r");

        // Scrolled 100px right, the track is drawn at x 50..150.
        runtime.scroll_registry().scroll_to("box", 100.0, 0.0);
        runtime.start_range_drag(node, 100.0);

        assert_eq!(*requests.borrow(), vec!["50".to_string()]);
    }
}
