//! Real, non-blocking loading for `<icon>` — a themable SVG glyph
//! (chevron, check, close, ...), distinct from `<img>`:
//!
//! - **SVG only.** Theming a raster image via `currentColor` isn't a
//!   real concept, so any other extension fails immediately with
//!   [`IconLoadError::UnsupportedFormat`], the same "fail fast, not a
//!   confusing decode error" contract [`crate::image::ImageRegistry`]
//!   already has for its own unsupported extensions.
//! - **Themed.** Its `color` tints the icon via `currentColor` (see
//!   [`florui_assets::substitute_current_color`]'s own doc) — resolved
//!   from this node's own `ComputedStyle::color`, the normal CSS
//!   cascade, not anything icon-specific. `crate::desktop`'s own paint
//!   setup reads that resolved color and passes it into
//!   [`IconRegistry::request_raster`]; this module has no style-cascade
//!   access of its own.
//! - **Decorative by default.** No `alt`, always hidden from assistive
//!   technology, never a focus stop, never duplicates a control's own
//!   accessible name — see `crate::accessibility::tree`'s own `icon` arm.
//!
//! Otherwise the exact same two-phase parse-then-rasterize shape
//! [`crate::image::ImageRegistry`] already uses for an SVG `<img>` (see
//! that module's own doc for why rasterizing isn't a single step): kept
//! as a separate, smaller registry rather than folded into it, since the
//! two tags' real constraints (format support, theming, accessibility)
//! differ enough that sharing one struct would mean branching on "is
//! this an icon" throughout it.
//!
//! This same registry also carries a second, unrelated kind of entry: a
//! built-in control decoration (a `<select>`'s own chevron, a checked
//! checkbox's own check mark — see [`IconRegistry::sync_controls`]'s own
//! doc). Neither has a real `<icon>` tag of its own to drive it — the
//! control node's own `FocusPath` is the key instead, and its source is
//! [`IconSource::Embedded`] (bytes baked into this binary via
//! `include_bytes!`, never a filesystem path) rather than an author's
//! `src` attribute. Sharing one [`IconEntry`]/[`IconSource`] shape and
//! one decode/rasterize/cache/staleness-rejection pipeline for both kinds
//! is the whole point (no parallel, duplicated loading path for a
//! built-in glyph) — see [`IconSource`]'s own doc for how the two kinds
//! coexist in one map without a retention pass for one kind ever evicting
//! the other.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use florui_reactive::Executor;
use florui_style::{Arena, FocusPath, NodeId};
use taffy::geometry::Size;

/// `<select>`'s own built-in dropdown chevron — see
/// [`IconRegistry::sync_controls`]'s own doc. Always down-pointing; no
/// open/closed rotation in this delivery, a documented simplification.
const SELECT_CHEVRON_SVG: &[u8] = include_bytes!("../assets/select-chevron.svg");
/// A checked checkbox's own built-in check mark — see
/// [`IconRegistry::sync_controls`]'s own doc.
const CHECKBOX_CHECK_SVG: &[u8] = include_bytes!("../assets/checkbox-check.svg");

/// Where an [`IconEntry`]'s SVG source text comes from. Two entirely
/// separate lifecycles share this one type (and one [`IconRegistry`]
/// map) rather than two parallel structs: [`Self::Path`] entries are
/// registered/retained by [`IconRegistry::sync`] (driven by real `<icon>`
/// tags in the arena), [`Self::Embedded`] entries by
/// [`IconRegistry::sync_controls`] (driven by `<select>`/checked-checkbox
/// nodes) — each sync pass's own retention filter only ever removes its
/// own kind (see each method's own doc), so neither can evict the
/// other's entries out from under it.
#[derive(Clone, PartialEq, Eq)]
enum IconSource {
    /// An author's `<icon src="...">` — resolved as a filesystem path,
    /// same as `<img>`.
    Path(String),
    /// A framework-owned control decoration, baked into this binary at
    /// compile time via `include_bytes!` — never a filesystem path, so a
    /// packaged/shipped binary has no dependency on a dev-checkout
    /// directory layout for it, the same guarantee `florui-assets`
    /// already documents for any other embedded asset.
    Embedded {
        id: &'static str,
        bytes: &'static [u8],
    },
}

impl IconSource {
    fn as_asset_source(&self) -> florui_assets::AssetSource<'_> {
        match self {
            IconSource::Path(src) => florui_assets::AssetSource::Path(Path::new(src)),
            IconSource::Embedded { id, bytes } => {
                florui_assets::AssetSource::Embedded { id, bytes }
            }
        }
    }
}

/// Everything that can go wrong loading an `<icon>`'s `src`.
#[derive(Debug)]
pub enum IconLoadError {
    /// `src`'s extension isn't `.svg` — see this module's own doc on
    /// format scope.
    UnsupportedFormat {
        extension: Option<String>,
    },
    Asset(florui_assets::AssetError),
}

impl std::fmt::Display for IconLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IconLoadError::UnsupportedFormat { extension } => write!(
                f,
                "unsupported <icon> format ({}) -- only .svg is supported",
                extension.as_deref().unwrap_or("none")
            ),
            IconLoadError::Asset(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for IconLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IconLoadError::Asset(err) => Some(err),
            IconLoadError::UnsupportedFormat { .. } => None,
        }
    }
}

enum IconState {
    Pending,
    Ready(Arc<florui_assets::RasterImage>),
    Failed(Arc<IconLoadError>),
}

struct IconEntry {
    source: IconSource,
    generation: u64,
    state: IconState,
    /// Known as soon as parsing succeeds, well before `state` ever
    /// reaches [`IconState::Ready`] — see this module's own doc.
    intrinsic_size: Option<Size<f32>>,
    /// The `(RasterFit, color)` this entry was last rasterized/requested
    /// at, so an unchanged target (the common case: most redraws neither
    /// resize nor recolor anything) doesn't keep re-rasterizing identical
    /// output every single frame.
    rasterized_at: Option<(florui_assets::RasterFit, [u8; 3])>,
}

/// Real load state for every currently-live `<icon>` *and* every live
/// built-in control decoration, keyed by structural identity in both
/// cases — an `<icon>` almost never has an author `id` (the same reason
/// [`crate::image::ImageRegistry`] is keyed this way rather than by one),
/// and a control decoration is keyed by the *control's own* `FocusPath`
/// (it has no `<icon>` tag of its own at all — see this module's own
/// doc).
#[derive(Default)]
pub struct IconRegistry {
    entries: Rc<RefCell<HashMap<FocusPath, IconEntry>>>,
    next_generation: Cell<u64>,
}

impl IconRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called once per [`crate::UiRuntime::update`], after
    /// [`Arena::build`] — mirrors
    /// [`crate::image::ImageRegistry::sync`]'s own placement and shape.
    /// Retains only [`IconSource::Path`] entries not seen this pass —
    /// see [`IconSource`]'s own doc for why a [`IconSource::Embedded`]
    /// entry is never touched here.
    pub(crate) fn sync(&self, arena: &Arena, executor: &dyn Executor) {
        let mut entries = self.entries.borrow_mut();
        let mut seen = HashSet::new();

        for node in arena.find_all(|arena, id| arena.tag(id) == "icon") {
            let Some(src) = arena.src_attr(node) else {
                continue;
            };
            let key = FocusPath::of(arena, node);
            seen.insert(key.clone());
            let source = IconSource::Path(src.to_string());
            let already_loading_this_source = entries.get(&key).is_some_and(|e| e.source == source);
            if !already_loading_this_source {
                Self::start_loading(
                    &self.entries,
                    &self.next_generation,
                    &mut entries,
                    key,
                    source,
                    executor,
                );
            }
        }

        entries.retain(|key, entry| {
            !matches!(entry.source, IconSource::Path(_)) || seen.contains(key)
        });
    }

    /// Registers/retains a built-in control decoration for every
    /// currently-live `<select>` and checked checkbox — the same
    /// registration/retention split [`Self::sync`] already has for real
    /// `<icon>` tags, called from the same place
    /// (`crate::UiRuntime::update`, right alongside it), just driven by a
    /// different arena query and [`IconSource::Embedded`] instead of an
    /// author's `src`. An `appearance: none` (real CSS's `--florui-appearance:
    /// none;`, see [`florui_style::Appearance`]'s own doc) control is
    /// skipped entirely -- no entry, nothing decoded, nothing painted.
    /// Retains only [`IconSource::Embedded`] entries not seen this pass —
    /// mirrors [`Self::sync`]'s own retention split so neither sync pass
    /// ever evicts the other kind's entries.
    pub(crate) fn sync_controls(
        &self,
        arena: &Arena,
        styles: &HashMap<NodeId, florui_style::ComputedStyle>,
        executor: &dyn Executor,
    ) {
        let mut entries = self.entries.borrow_mut();
        let mut seen = HashSet::new();

        for node in arena.find_all(|arena, id| arena.tag(id) == "select") {
            if styles
                .get(&node)
                .is_some_and(|s| s.appearance == florui_style::Appearance::None)
            {
                continue;
            }
            let key = FocusPath::of(arena, node);
            seen.insert(key.clone());
            let source = IconSource::Embedded {
                id: "select-chevron",
                bytes: SELECT_CHEVRON_SVG,
            };
            if !entries.get(&key).is_some_and(|e| e.source == source) {
                Self::start_loading(
                    &self.entries,
                    &self.next_generation,
                    &mut entries,
                    key,
                    source,
                    executor,
                );
            }
        }

        for node in arena.find_all(|arena, id| {
            arena.tag(id) == "input"
                && arena.input_type(id) == Some("checkbox")
                && arena.is_checked(id)
        }) {
            if styles
                .get(&node)
                .is_some_and(|s| s.appearance == florui_style::Appearance::None)
            {
                continue;
            }
            let key = FocusPath::of(arena, node);
            seen.insert(key.clone());
            let source = IconSource::Embedded {
                id: "checkbox-check",
                bytes: CHECKBOX_CHECK_SVG,
            };
            if !entries.get(&key).is_some_and(|e| e.source == source) {
                Self::start_loading(
                    &self.entries,
                    &self.next_generation,
                    &mut entries,
                    key,
                    source,
                    executor,
                );
            }
        }

        entries.retain(|key, entry| {
            !matches!(entry.source, IconSource::Embedded { .. }) || seen.contains(key)
        });
    }

    /// Shared tail of both sync passes above: inserts a fresh `Pending`
    /// entry, fails immediately for a [`IconSource::Path`] whose
    /// extension isn't `.svg` (see this module's own doc on format scope
    /// -- a [`IconSource::Embedded`] source has no filename to check at
    /// all, and is always a real, framework-authored `.svg`), else spawns
    /// a background task that parses far enough to read the intrinsic
    /// size -- see this module's own doc, and `crate::image`'s own doc
    /// (the identical reasoning for an SVG `<img>`) for why.
    fn start_loading(
        registry_entries: &Rc<RefCell<HashMap<FocusPath, IconEntry>>>,
        next_generation: &Cell<u64>,
        entries: &mut HashMap<FocusPath, IconEntry>,
        key: FocusPath,
        source: IconSource,
        executor: &dyn Executor,
    ) {
        let generation = next_generation.get();
        next_generation.set(generation + 1);
        entries.insert(
            key.clone(),
            IconEntry {
                source: source.clone(),
                generation,
                state: IconState::Pending,
                intrinsic_size: None,
                rasterized_at: None,
            },
        );

        if let IconSource::Path(src) = &source {
            let extension = Path::new(src)
                .extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_ascii_lowercase);
            if extension.as_deref() != Some("svg") {
                let error = IconLoadError::UnsupportedFormat { extension };
                eprintln!("florui-platform: <icon src=\"{src}\"> failed to load: {error}");
                if let Some(entry) = entries.get_mut(&key) {
                    entry.state = IconState::Failed(Arc::new(error));
                }
                return;
            }
        }

        let entries_for_task = Rc::clone(registry_entries);
        let key_for_task = key.clone();
        let source_for_task = source.clone();
        executor.spawn(Box::pin(async move {
            let text_source = source_for_task.clone();
            let result = florui_reactive::blocking::spawn_blocking(move || {
                let text = text_source.as_asset_source().read_text()?;
                let image = florui_assets::parse_svg(&text)?;
                Ok::<_, florui_assets::AssetError>(image.intrinsic_size)
            })
            .await;
            let mut entries = entries_for_task.borrow_mut();
            if let Some(entry) = entries.get_mut(&key_for_task)
                && entry.generation == generation
            {
                match result {
                    Ok(size) => {
                        entry.intrinsic_size = Some(Size {
                            width: size.width,
                            height: size.height,
                        });
                    }
                    Err(err) => {
                        let error = IconLoadError::Asset(err);
                        eprintln!("florui-platform: icon failed to load: {error}");
                        entry.state = IconState::Failed(Arc::new(error));
                    }
                }
            }
        }));
    }

    /// Rasterizes an icon at `fit`/`color` once its intrinsic size is
    /// already known (a no-op before then, or when `(fit, color)` matches
    /// whatever this entry was already rasterized/requested at) — called
    /// from `crate::desktop`'s own paint setup, once per redraw, with the
    /// real resolved content-box size and `color` for every real `<icon>`
    /// and every built-in control decoration currently on screen. Reads
    /// `key`'s own already-registered source (from [`Self::sync`] or
    /// [`Self::sync_controls`]) rather than taking one as a parameter —
    /// there is nothing left to say once an entry already exists. Mirrors
    /// [`crate::image::ImageRegistry::request_raster`]'s own doc.
    pub(crate) fn request_raster(
        &self,
        key: &FocusPath,
        fit: florui_assets::RasterFit,
        color: [u8; 3],
        cache: &Arc<florui_assets::AssetCache>,
        executor: &dyn Executor,
    ) {
        let (source, generation) = {
            let mut entries = self.entries.borrow_mut();
            let Some(entry) = entries.get_mut(key) else {
                return;
            };
            if entry.intrinsic_size.is_none() || entry.rasterized_at == Some((fit, color)) {
                return;
            }
            entry.rasterized_at = Some((fit, color));
            (entry.source.clone(), entry.generation)
        };

        let cache = Arc::clone(cache);
        let entries_for_task = Rc::clone(&self.entries);
        let key_for_task = key.clone();
        executor.spawn(Box::pin(async move {
            let result = florui_reactive::blocking::spawn_blocking(move || {
                cache.get_or_load(
                    &source.as_asset_source(),
                    florui_assets::DecodeParams::RasterizedThemed(fit, color),
                )
            })
            .await;
            let mut entries = entries_for_task.borrow_mut();
            if let Some(entry) = entries.get_mut(&key_for_task)
                && entry.generation == generation
            {
                entry.state = match result {
                    Ok(image) => IconState::Ready(image),
                    Err(err) => {
                        let error = IconLoadError::Asset(err);
                        eprintln!("florui-platform: icon failed to load: {error}");
                        IconState::Failed(Arc::new(error))
                    }
                };
            }
        }));
    }

    /// This `<icon>`'s own intrinsic size, once known — see
    /// [`crate::image::ImageRegistry::intrinsic_size`]'s own doc for why
    /// this can be `Some` well before `state` reaches
    /// [`IconState::Ready`].
    pub(crate) fn intrinsic_size(&self, key: &FocusPath) -> Option<Size<f32>> {
        let entries = self.entries.borrow();
        let entry = entries.get(key)?;
        match &entry.state {
            IconState::Ready(image) => Some(Size {
                width: image.width as f32,
                height: image.height as f32,
            }),
            IconState::Pending => entry.intrinsic_size,
            IconState::Failed(_) => None,
        }
    }

    /// This `<icon>`'s own decoded, already-themed pixels, once
    /// rasterized — for painting.
    pub(crate) fn decoded(&self, key: &FocusPath) -> Option<Arc<florui_assets::RasterImage>> {
        match &self.entries.borrow().get(key)?.state {
            IconState::Ready(image) => Some(Arc::clone(image)),
            IconState::Pending | IconState::Failed(_) => None,
        }
    }

    /// This `<icon>`'s own load failure, if it has one. See
    /// [`crate::image::ImageRegistry::error`]'s own doc: a real
    /// diagnostics consumer, once one exists, calls this; today only this
    /// module's own tests do.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn error(&self, key: &FocusPath) -> Option<Arc<IconLoadError>> {
        match &self.entries.borrow().get(key)?.state {
            IconState::Failed(err) => Some(Arc::clone(err)),
            IconState::Pending | IconState::Ready(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use florui::prelude::*;
    use florui_reactive::executor::LocalExecutor;
    use florui_style::Arena;

    const SVG_TEXT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
  <path d="M0 0 H24 V24 H0 Z" fill="currentColor"/>
</svg>"##;

    fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn an_icon_reports_its_intrinsic_size_before_any_rasterization() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chevron.svg");
        std::fs::write(&path, SVG_TEXT).unwrap();

        let tree: Element = view! { <icon src={path.display().to_string()} /> };
        let arena = Arena::build(&tree);
        let registry = IconRegistry::new();
        let executor = LocalExecutor::new();

        registry.sync(&arena, &executor);
        let key = FocusPath::of(&arena, arena.roots()[0]);
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.intrinsic_size(&key).is_some()
        }));
        assert_eq!(
            registry.intrinsic_size(&key),
            Some(Size {
                width: 24.0,
                height: 24.0
            })
        );
        assert!(registry.decoded(&key).is_none());
    }

    #[test]
    fn request_raster_produces_a_really_themed_icon() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chevron.svg");
        std::fs::write(&path, SVG_TEXT).unwrap();
        let src = path.display().to_string();

        let tree: Element = view! { <icon src={src.clone()} /> };
        let arena = Arena::build(&tree);
        let registry = IconRegistry::new();
        let cache = Arc::new(florui_assets::AssetCache::new());
        let executor = LocalExecutor::new();

        registry.sync(&arena, &executor);
        let key = FocusPath::of(&arena, arena.roots()[0]);
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.intrinsic_size(&key).is_some()
        }));

        registry.request_raster(
            &key,
            florui_assets::RasterFit::Contain {
                width: 16,
                height: 16,
            },
            [0x00, 0x99, 0x00],
            &cache,
            &executor,
        );
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.decoded(&key).is_some()
        }));
        let raster = registry.decoded(&key).unwrap();
        assert_eq!((raster.width, raster.height), (16, 16));
        let offset = ((8 * 16 + 8) * 4) as usize;
        assert_eq!(raster.rgba[offset], 0, "red channel");
        assert_eq!(raster.rgba[offset + 1], 0x99, "green channel");
        assert_eq!(raster.rgba[offset + 2], 0, "blue channel");
    }

    #[test]
    fn request_raster_treats_a_recolor_as_a_real_new_raster() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chevron.svg");
        std::fs::write(&path, SVG_TEXT).unwrap();
        let src = path.display().to_string();

        let tree: Element = view! { <icon src={src.clone()} /> };
        let arena = Arena::build(&tree);
        let registry = IconRegistry::new();
        let cache = Arc::new(florui_assets::AssetCache::new());
        let executor = LocalExecutor::new();

        registry.sync(&arena, &executor);
        let key = FocusPath::of(&arena, arena.roots()[0]);
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.intrinsic_size(&key).is_some()
        }));

        let fit = florui_assets::RasterFit::Contain {
            width: 16,
            height: 16,
        };
        registry.request_raster(&key, fit, [0xff, 0x00, 0x00], &cache, &executor);
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry
                .decoded(&key)
                .is_some_and(|image| image.rgba[0] == 0xff)
        }));

        // Same size, a different color -- a real new rasterize, not a
        // dedup-skip like an unchanged request would be.
        registry.request_raster(&key, fit, [0x00, 0x00, 0xff], &cache, &executor);
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry
                .decoded(&key)
                .is_some_and(|image| image.rgba[2] == 0xff)
        }));
    }

    #[test]
    fn a_non_svg_extension_fails_immediately_without_a_background_task() {
        let tree: Element = view! { <icon src="chevron.png" /> };
        let arena = Arena::build(&tree);
        let registry = IconRegistry::new();
        let executor = LocalExecutor::new();

        registry.sync(&arena, &executor);
        let key = FocusPath::of(&arena, arena.roots()[0]);
        assert!(matches!(
            registry.error(&key).as_deref(),
            Some(IconLoadError::UnsupportedFormat { .. })
        ));
    }

    #[test]
    fn a_missing_file_reports_a_load_error_not_a_panic() {
        let tree: Element = view! { <icon src="does/not/exist.svg" /> };
        let arena = Arena::build(&tree);
        let registry = IconRegistry::new();
        let executor = LocalExecutor::new();

        registry.sync(&arena, &executor);
        let key = FocusPath::of(&arena, arena.roots()[0]);
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.error(&key).is_some()
        }));
        assert!(matches!(
            *registry.error(&key).unwrap(),
            IconLoadError::Asset(florui_assets::AssetError::Io { .. })
        ));
    }

    #[test]
    fn an_icon_no_longer_present_is_dropped_from_the_registry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chevron.svg");
        std::fs::write(&path, SVG_TEXT).unwrap();

        let tree_with_icon: Element =
            view! { <div><icon src={path.display().to_string()} /></div> };
        let arena = Arena::build(&tree_with_icon);
        let registry = IconRegistry::new();
        let executor = LocalExecutor::new();

        registry.sync(&arena, &executor);
        let icon_node = arena.children(arena.roots()[0])[0];
        let key = FocusPath::of(&arena, icon_node);
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.intrinsic_size(&key).is_some()
        }));

        let tree_without_icon: Element = view! { <div /> };
        let arena = Arena::build(&tree_without_icon);
        registry.sync(&arena, &executor);
        assert!(registry.intrinsic_size(&key).is_none());
        assert_eq!(registry.entries.borrow().len(), 0);
    }

    fn styles_for(
        arena: &Arena,
        css: &str,
    ) -> HashMap<florui_style::NodeId, florui_style::ComputedStyle> {
        let rules = florui_style::parse_stylesheet(css).unwrap();
        florui_style::compute(
            arena,
            &rules,
            &florui_style::InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        )
    }

    #[test]
    fn sync_controls_registers_a_select_chevron() {
        let tree: Element = view! { <select /> };
        let arena = Arena::build(&tree);
        let styles = styles_for(&arena, "");
        let registry = IconRegistry::new();
        let executor = LocalExecutor::new();

        registry.sync_controls(&arena, &styles, &executor);
        let key = FocusPath::of(&arena, arena.roots()[0]);
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.intrinsic_size(&key).is_some()
        }));
    }

    #[test]
    fn sync_controls_registers_a_checked_checkboxs_check() {
        let tree: Element = view! { <input type="checkbox" checked={true} /> };
        let arena = Arena::build(&tree);
        let styles = styles_for(&arena, "");
        let registry = IconRegistry::new();
        let executor = LocalExecutor::new();

        registry.sync_controls(&arena, &styles, &executor);
        let key = FocusPath::of(&arena, arena.roots()[0]);
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.intrinsic_size(&key).is_some()
        }));
    }

    #[test]
    fn sync_controls_never_registers_an_unchecked_checkbox() {
        let tree: Element = view! { <input type="checkbox" checked={false} /> };
        let arena = Arena::build(&tree);
        let styles = styles_for(&arena, "");
        let registry = IconRegistry::new();
        let executor = LocalExecutor::new();

        registry.sync_controls(&arena, &styles, &executor);
        assert_eq!(registry.entries.borrow().len(), 0);
    }

    #[test]
    fn sync_controls_skips_a_control_with_florui_appearance_none() {
        let tree: Element = view! { <select class="hidden" /> };
        let arena = Arena::build(&tree);
        let styles = styles_for(&arena, ".hidden { --florui-appearance: none; }");
        let registry = IconRegistry::new();
        let executor = LocalExecutor::new();

        registry.sync_controls(&arena, &styles, &executor);
        assert_eq!(registry.entries.borrow().len(), 0);
    }

    #[test]
    fn sync_and_sync_controls_never_evict_each_others_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chevron.svg");
        std::fs::write(&path, SVG_TEXT).unwrap();

        let tree: Element = view! {
            <div>
                <icon src={path.display().to_string()} />
                <select />
            </div>
        };
        let arena = Arena::build(&tree);
        let styles = styles_for(&arena, "");
        let registry = IconRegistry::new();
        let executor = LocalExecutor::new();

        let icon_node = arena.children(arena.roots()[0])[0];
        let select_node = arena.children(arena.roots()[0])[1];
        let icon_key = FocusPath::of(&arena, icon_node);
        let select_key = FocusPath::of(&arena, select_node);

        // Order matters here: each sync pass's own retention filter must
        // leave the other kind of entry alone regardless of which ran
        // most recently.
        registry.sync(&arena, &executor);
        registry.sync_controls(&arena, &styles, &executor);
        registry.sync(&arena, &executor);
        registry.sync_controls(&arena, &styles, &executor);

        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.intrinsic_size(&icon_key).is_some()
                && registry.intrinsic_size(&select_key).is_some()
        }));
        assert_eq!(registry.entries.borrow().len(), 2);
    }

    #[test]
    fn sync_controls_drops_a_select_no_longer_present() {
        let tree_with_select: Element = view! { <div><select /></div> };
        let arena = Arena::build(&tree_with_select);
        let styles = styles_for(&arena, "");
        let registry = IconRegistry::new();
        let executor = LocalExecutor::new();

        registry.sync_controls(&arena, &styles, &executor);
        let select_node = arena.children(arena.roots()[0])[0];
        let key = FocusPath::of(&arena, select_node);
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.intrinsic_size(&key).is_some()
        }));

        let tree_without_select: Element = view! { <div /> };
        let arena = Arena::build(&tree_without_select);
        let styles = styles_for(&arena, "");
        registry.sync_controls(&arena, &styles, &executor);
        assert!(registry.intrinsic_size(&key).is_none());
        assert_eq!(registry.entries.borrow().len(), 0);
    }
}
