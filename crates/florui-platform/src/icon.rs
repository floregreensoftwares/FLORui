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

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use florui_reactive::Executor;
use florui_style::{Arena, FocusPath};
use taffy::geometry::Size;

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
    src: String,
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

/// Real load state for every currently-live `<icon>`, keyed by structural
/// identity (an `<icon>` almost never has an author `id`, the same reason
/// [`crate::image::ImageRegistry`] is keyed this way rather than by one).
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
    pub(crate) fn sync(&self, arena: &Arena, executor: &dyn Executor) {
        let mut entries = self.entries.borrow_mut();
        let mut seen = HashSet::new();

        for node in arena.find_all(|arena, id| arena.tag(id) == "icon") {
            let Some(src) = arena.src_attr(node) else {
                continue;
            };
            let key = FocusPath::of(arena, node);
            seen.insert(key.clone());

            let already_loading_this_src = entries.get(&key).is_some_and(|entry| entry.src == src);
            if already_loading_this_src {
                continue;
            }

            let generation = self.next_generation.get();
            self.next_generation.set(generation + 1);
            entries.insert(
                key.clone(),
                IconEntry {
                    src: src.to_string(),
                    generation,
                    state: IconState::Pending,
                    intrinsic_size: None,
                    rasterized_at: None,
                },
            );

            let path = PathBuf::from(src);
            let extension = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_ascii_lowercase);
            if extension.as_deref() != Some("svg") {
                let error = IconLoadError::UnsupportedFormat { extension };
                eprintln!("florui-platform: <icon src=\"{src}\"> failed to load: {error}");
                if let Some(entry) = entries.get_mut(&key) {
                    entry.state = IconState::Failed(Arc::new(error));
                }
                continue;
            }

            // Only far enough to read the intrinsic size -- see this
            // module's own doc, and `crate::image`'s own doc (the
            // identical reasoning for an SVG `<img>`) for why.
            let entries_for_task = Rc::clone(&self.entries);
            let key_for_task = key.clone();
            let src_for_task = src.to_string();
            executor.spawn(Box::pin(async move {
                let result = florui_reactive::blocking::spawn_blocking(move || {
                    let source = florui_assets::AssetSource::Path(&path);
                    let text = source.read_text()?;
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
                            eprintln!(
                                "florui-platform: <icon src=\"{src_for_task}\"> failed to \
                                 load: {error}"
                            );
                            entry.state = IconState::Failed(Arc::new(error));
                        }
                    }
                }
            }));
        }

        entries.retain(|key, _| seen.contains(key));
    }

    /// Rasterizes an `<icon>` at `fit`/`color` once its intrinsic size is
    /// already known (a no-op before then, or when `(fit, color)` matches
    /// whatever this entry was already rasterized/requested at) — called
    /// from `crate::desktop`'s own paint setup, once per redraw, with the
    /// real resolved content-box size and `color` for every `<icon>`
    /// currently on screen. Mirrors
    /// [`crate::image::ImageRegistry::request_raster`]'s own doc.
    pub(crate) fn request_raster(
        &self,
        key: &FocusPath,
        src: &str,
        fit: florui_assets::RasterFit,
        color: [u8; 3],
        cache: &Arc<florui_assets::AssetCache>,
        executor: &dyn Executor,
    ) {
        let generation = {
            let mut entries = self.entries.borrow_mut();
            let Some(entry) = entries.get_mut(key) else {
                return;
            };
            if entry.intrinsic_size.is_none() || entry.rasterized_at == Some((fit, color)) {
                return;
            }
            entry.rasterized_at = Some((fit, color));
            entry.generation
        };

        let path = PathBuf::from(src);
        let cache = Arc::clone(cache);
        let entries_for_task = Rc::clone(&self.entries);
        let key_for_task = key.clone();
        let src_for_task = src.to_string();
        executor.spawn(Box::pin(async move {
            let result = florui_reactive::blocking::spawn_blocking(move || {
                cache.get_or_load(
                    &florui_assets::AssetSource::Path(&path),
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
                        eprintln!(
                            "florui-platform: <icon src=\"{src_for_task}\"> failed to load: \
                             {error}"
                        );
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
            &src,
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
        registry.request_raster(&key, &src, fit, [0xff, 0x00, 0x00], &cache, &executor);
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry
                .decoded(&key)
                .is_some_and(|image| image.rgba[0] == 0xff)
        }));

        // Same size, a different color -- a real new rasterize, not a
        // dedup-skip like an unchanged request would be.
        registry.request_raster(&key, &src, fit, [0x00, 0x00, 0xff], &cache, &executor);
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
}
