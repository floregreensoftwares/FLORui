//! Real, non-blocking loading for `<img>`, driven structurally — same
//! reason as [`crate::text_input::TextInputRegistry`]'s own doc: `<img>`
//! is a raw primitive tag, not a `#[component]`, so there is no hook
//! call-site of its own to own `use_resource` state through.
//!
//! Unlike [`crate::text_input::TextInputRegistry`] (keyed by an explicit
//! author `id`, since a form control commonly has one anyway), this is
//! keyed by [`florui_style::FocusPath`] — a plain `<img>` almost never
//! has an `id`, and requiring one just to load would be a real
//! regression from how `<img>` works everywhere else.
//!
//! ## Format scope
//!
//! PNG, JPEG, WebP, GIF and SVG, matching `florui-assets`'s own scope — any
//! other extension fails immediately with [`ImageLoadError::UnsupportedFormat`]
//! rather than being silently attempted and failing with a confusing
//! decode error.
//!
//! A raster image (PNG, JPEG, WebP, GIF) decodes in one background step, straight to [`ImageState::Ready`]
//! (`florui_assets::DecodeParams::Native` has no size parameter — see
//! that type's own doc). SVG is real vector content: rasterizing it at
//! an arbitrary fixed size the moment it's parsed would either blur it
//! (too small for its final on-screen size) or waste real memory/CPU
//! (needlessly large) — [CSS-Images-3]'s own reasoning for why a UA
//! doesn't rasterize before layout. So SVG goes through **two**
//! background steps instead: [`ImageRegistry::sync`] parses it just far
//! enough to read its intrinsic size (`ImageEntry::svg_intrinsic_size`,
//! reported by [`ImageRegistry::intrinsic_size`] even while `state` is
//! still [`ImageState::Pending`] — this is exactly what lets layout
//! proceed before any pixel exists at all, the same "loading" case a
//! still-fetching PNG already goes through); once layout has resolved a
//! real content-box size for it, [`ImageRegistry::request_raster`]
//! (called from `crate::desktop`'s own paint setup, the one place both
//! that resolved size and this registry are both in scope) rasterizes at
//! that real size, re-requesting again on a later resize/DPI change
//! rather than staying pinned to whatever size it first rendered at.
//!
//! [CSS-Images-3]: https://www.w3.org/TR/css-images-3/
//!
//! ## Source resolution
//!
//! `src` is a plain filesystem path, resolved by the OS the same way any
//! relative `std::fs` call already is (relative to the process's current
//! working directory) -- no build-time asset embedding for `<img>` sources
//! exists yet (that would follow `florui-icon`'s own `embed`-at-build-time
//! pattern, in a later delivery) -- or, when it starts with `http://` or
//! `https://`, a network source.
//!
//! ### Network sources
//!
//! A network `src` needs the `net` feature; without it the image fails at
//! once with [`florui_assets::FetchError::Unavailable`]. Its loading
//! contract (timeouts, redirects, the size cap while streaming, the media type
//! against the bytes, the HTTP disk cache, the bounded queue) is the one of
//! `florui_assets::net`, and every picture of the process shares that fetcher.
//!
//! - The type is not taken from the extension, which a URL often lacks:
//!   the response's bytes say whether it is a raster image or an SVG.
//! - A picture that is already decoded and alive in the cache is used without
//!   a request.
//! - An `<img>` that unmounts, or whose `src` changes, cancels its request
//!   (the entry holds the request's ticket): one still queued is never sent,
//!   one running stops at its next read.
//! - Decoding goes through the same [`florui_assets::AssetCache`] and decode
//!   slots as a file, so the decoded image is shared between windows and
//!   dropped when the last `<img>` holding it goes.
//! - A failure is the same state as an unreadable file: no pixels, the
//!   reason logged once and available from [`ImageRegistry::error`], and the
//!   size CSS gives the box (a broken image has no intrinsic size).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use florui_reactive::Executor;
use florui_style::{Arena, FocusPath};
use taffy::geometry::Size;

/// Everything that can go wrong loading an `<img>`'s `src`. Wraps
/// [`florui_assets::AssetError`] rather than re-exporting it directly, so
/// this crate can add its own cases (like [`Self::UnsupportedFormat`])
/// without that being a `florui-assets` concern.
#[derive(Debug)]
pub enum ImageLoadError {
    /// `src`'s extension isn't one of a raster image's or `.svg` — see this module's own doc
    /// on format scope.
    UnsupportedFormat {
        extension: Option<String>,
    },
    Asset(florui_assets::AssetError),
}

impl std::fmt::Display for ImageLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageLoadError::UnsupportedFormat { extension } => write!(
                f,
                "unsupported <img> format ({}) -- only .png, .jpg, .jpeg, .webp, .gif and .svg are supported",
                extension.as_deref().unwrap_or("none")
            ),
            ImageLoadError::Asset(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ImageLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ImageLoadError::Asset(err) => Some(err),
            ImageLoadError::UnsupportedFormat { .. } => None,
        }
    }
}

/// What cancels a download when it is dropped: held by the `<img>` it was started for, so
/// an `<img>` that unmounts or changes `src` takes its request out of the queue or stops it.
#[cfg(feature = "net")]
type Ticket = florui_assets::net::FetchTicket;
#[cfg(not(feature = "net"))]
type Ticket = ();

/// The bytes of a downloaded document, shared with the download itself and not copied.
type SharedBytes = Arc<dyn AsRef<[u8]> + Send + Sync>;

/// One `<img>`'s own current load state.
enum ImageState {
    Pending,
    Ready(Arc<florui_assets::RasterImage>),
    Failed(Arc<ImageLoadError>),
}

struct ImageEntry {
    /// The `src` this entry is loading/loaded/failed *for* — compared
    /// against the arena's current `src` each [`ImageRegistry::sync`] to
    /// detect a real change (not the same identity-vs-generation
    /// question `generation` answers; see [`ImageRegistry::sync`]'s own
    /// doc).
    src: String,
    /// Bumped every time a fresh load starts for this entry's key. A
    /// completing background task only commits its result if this still
    /// matches the generation it was spawned with — rejects a stale
    /// completion from a `src` that has since changed again, or from an
    /// entry whose element has since unmounted and been reused by an
    /// unrelated one at the same structural path. Best-effort, the same
    /// as `florui-reactive`'s own `use_resource`: the background OS
    /// thread inside `spawn_blocking` still runs to completion regardless
    /// (real cancellation isn't attempted), only its *result* is
    /// discarded.
    generation: u64,
    state: ImageState,
    /// SVG only: its own intrinsic size, known as soon as parsing
    /// succeeds — well before `state` ever reaches [`ImageState::Ready`],
    /// which for SVG only happens once a real raster target size is
    /// requested (see this module's own doc). `None` for a PNG entry, or
    /// an SVG entry whose parse hasn't completed (or failed) yet.
    svg_intrinsic_size: Option<Size<f32>>,
    /// SVG only: the `RasterFit` a rasterization was last requested or
    /// completed at — compared against a fresh [`ImageRegistry::request_raster`]
    /// call so an unchanged target size (the common case: most redraws
    /// don't resize anything) doesn't keep re-rasterizing identical
    /// output on every single frame.
    svg_rasterized_at: Option<florui_assets::RasterFit>,
    /// A network source only: the request, which the entry's drop cancels.
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    ticket: Option<Ticket>,
    /// A network SVG only: the downloaded document, kept so that rasterizing it at another
    /// size does not download it again (a path is read again, cheaply, from disk).
    svg_bytes: Option<SharedBytes>,
}

/// Real load state for every currently-live `<img>`, keyed by structural
/// identity — see this module's own doc.
#[derive(Default)]
pub struct ImageRegistry {
    entries: Rc<RefCell<HashMap<FocusPath, ImageEntry>>>,
    next_generation: Cell<u64>,
}

impl ImageRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called once per [`crate::UiRuntime::update`], after
    /// [`Arena::build`] — mirrors [`crate::text_input::TextInputRegistry::sync`]'s
    /// own placement and "create the newly-seen, drop the no-longer-seen"
    /// shape, except keyed structurally (see this module's own doc)
    /// rather than by `id`.
    ///
    /// A newly-seen `<img>`, or one whose `src` changed since the last
    /// call, starts a fresh background load: [`florui_assets::AssetCache::get_or_load`]
    /// runs on a real OS thread via [`florui_reactive::blocking::spawn_blocking`],
    /// and the awaiting task is spawned onto `executor` — the same
    /// `LocalExecutor` `UiRuntime` already drives via `on_needs_update`,
    /// so a completed load wakes the host exactly the way any other
    /// background resource completion already does, with no new wakeup
    /// mechanism needed here.
    pub(crate) fn sync(
        &self,
        arena: &Arena,
        cache: &Arc<florui_assets::AssetCache>,
        executor: &dyn Executor,
    ) {
        let mut entries = self.entries.borrow_mut();
        let mut seen = HashSet::new();

        for node in arena.find_all(|arena, id| arena.tag(id) == "img") {
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
                ImageEntry {
                    src: src.to_string(),
                    generation,
                    state: ImageState::Pending,
                    svg_intrinsic_size: None,
                    svg_rasterized_at: None,
                    ticket: None,
                    svg_bytes: None,
                },
            );

            if florui_assets::url_image::is_remote(src) {
                start_remote(
                    &mut entries,
                    &self.entries,
                    &key,
                    generation,
                    src,
                    cache,
                    executor,
                );
                continue;
            }

            let path = PathBuf::from(src);
            match classify_format(&path) {
                None => {
                    // No file to read at all -- fail immediately, no
                    // background task needed.
                    let extension = path
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .map(str::to_ascii_lowercase);
                    let error = ImageLoadError::UnsupportedFormat { extension };
                    eprintln!("florui-platform: <img src=\"{src}\"> failed to load: {error}");
                    if let Some(entry) = entries.get_mut(&key) {
                        entry.state = ImageState::Failed(Arc::new(error));
                    }
                }
                Some(Format::Raster) => {
                    let cache = Arc::clone(cache);
                    let entries_for_task = Rc::clone(&self.entries);
                    let key_for_task = key.clone();
                    let src_for_task = src.to_string();
                    executor.spawn(Box::pin(async move {
                        let result = florui_reactive::blocking::spawn_blocking(move || {
                            cache.get_or_load(
                                &florui_assets::AssetSource::Path(&path),
                                florui_assets::DecodeParams::Native,
                            )
                        })
                        .await;
                        commit(
                            &entries_for_task,
                            &key_for_task,
                            generation,
                            &src_for_task,
                            || result.map(ImageState::Ready),
                        );
                    }));
                }
                Some(Format::Svg) => {
                    // Only far enough to read the intrinsic size -- see
                    // this module's own doc on why SVG doesn't rasterize
                    // here yet.
                    let entries_for_task = Rc::clone(&self.entries);
                    let key_for_task = key.clone();
                    let src_for_task = src.to_string();
                    executor.spawn(Box::pin(async move {
                        // Returns just the intrinsic size, not the parsed
                        // `VectorImage` itself (which is `Send`, checked
                        // directly rather than assumed): keeping only the
                        // few bytes actually needed here, rather than a
                        // whole parsed tree, means `request_raster`
                        // re-reads and re-parses the same file later
                        // instead of this task holding it in memory for
                        // however long it takes layout to resolve a real
                        // box size -- a deliberate simplicity trade for a
                        // real SVG asset, cheap to parse again next to the
                        // rasterization work that follows it.
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
                                    entry.svg_intrinsic_size = Some(Size {
                                        width: size.width,
                                        height: size.height,
                                    });
                                }
                                Err(err) => {
                                    let error = ImageLoadError::Asset(err);
                                    eprintln!(
                                        "florui-platform: <img src=\"{src_for_task}\"> failed \
                                         to load: {error}"
                                    );
                                    entry.state = ImageState::Failed(Arc::new(error));
                                }
                            }
                        }
                    }));
                }
            }
        }

        entries.retain(|key, _| seen.contains(key));
    }

    /// Rasterizes an SVG `<img>` at `fit` once its intrinsic size is
    /// already known (a no-op before then, or for a PNG entry, or when
    /// `fit` matches whatever this entry was already rasterized/requested
    /// at) — called from `crate::desktop`'s own paint setup, once per
    /// redraw, with the real resolved content-box size for every `<img>`
    /// currently on screen. See this module's own doc for why this is a
    /// separate step from [`Self::sync`] instead of happening as soon as
    /// intrinsic size is known.
    pub(crate) fn request_raster(
        &self,
        key: &FocusPath,
        src: &str,
        fit: florui_assets::RasterFit,
        cache: &Arc<florui_assets::AssetCache>,
        executor: &dyn Executor,
    ) {
        let (generation, downloaded) = {
            let mut entries = self.entries.borrow_mut();
            let Some(entry) = entries.get_mut(key) else {
                return;
            };
            if entry.svg_intrinsic_size.is_none() || entry.svg_rasterized_at == Some(fit) {
                return;
            }
            entry.svg_rasterized_at = Some(fit);
            (entry.generation, entry.svg_bytes.clone())
        };

        let path = PathBuf::from(src);
        let cache = Arc::clone(cache);
        let entries_for_task = Rc::clone(&self.entries);
        let key_for_task = key.clone();
        let src_for_task = src.to_string();
        executor.spawn(Box::pin(async move {
            let result = florui_reactive::blocking::spawn_blocking(move || match downloaded {
                // Under an identity that has the digest of this document in it: another
                // window may hold a rasterization of an older one of the same address.
                Some(bytes) => cache.get_or_decode(
                    florui_assets::url_image::download_id(
                        &path.to_string_lossy(),
                        (*bytes).as_ref(),
                    ),
                    (*bytes).as_ref(),
                    florui_assets::DecodeParams::Rasterized(fit),
                ),
                None => cache.get_or_load(
                    &florui_assets::AssetSource::Path(&path),
                    florui_assets::DecodeParams::Rasterized(fit),
                ),
            })
            .await;
            commit(
                &entries_for_task,
                &key_for_task,
                generation,
                &src_for_task,
                || result.map(ImageState::Ready),
            );
        }));
    }

    /// This `<img>`'s own intrinsic size, once known — `None` while still
    /// pending, on failure, or for a key this registry has never seen.
    /// Read by `crate::UiRuntime`'s own layout fix-up pass (mirroring
    /// `fix_select_widths`). For a PNG entry this only ever becomes
    /// `Some` alongside `state` reaching [`ImageState::Ready`] (decoding
    /// PNG bytes always produces final pixels, nothing partial); for an
    /// SVG entry it becomes `Some` as soon as parsing succeeds, well
    /// before any raster pixels exist — see this module's own doc.
    pub(crate) fn intrinsic_size(&self, key: &FocusPath) -> Option<Size<f32>> {
        let entries = self.entries.borrow();
        let entry = entries.get(key)?;
        match &entry.state {
            ImageState::Ready(image) => Some(Size {
                width: image.width as f32,
                height: image.height as f32,
            }),
            ImageState::Pending => entry.svg_intrinsic_size,
            ImageState::Failed(_) => None,
        }
    }

    /// This `<img>`'s own decoded pixels, once loaded — for painting.
    pub(crate) fn decoded(&self, key: &FocusPath) -> Option<Arc<florui_assets::RasterImage>> {
        match &self.entries.borrow().get(key)?.state {
            ImageState::Ready(image) => Some(Arc::clone(image)),
            ImageState::Pending | ImageState::Failed(_) => None,
        }
    }

    /// This `<img>`'s own load failure, if it has one. `sync` already
    /// logs a failure once, at the moment it happens (see its own body) —
    /// this accessor exists for this module's own tests to assert on
    /// *which* failure occurred, not as a second live diagnostics path;
    /// a real one (devtools, accessibility) can call this once it exists.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn error(&self, key: &FocusPath) -> Option<Arc<ImageLoadError>> {
        match &self.entries.borrow().get(key)?.state {
            ImageState::Failed(err) => Some(Arc::clone(err)),
            ImageState::Pending | ImageState::Ready(_) => None,
        }
    }
}

/// Without the `net` feature a network source fails at once, saying why.
#[cfg(not(feature = "net"))]
fn start_remote(
    entries: &mut HashMap<FocusPath, ImageEntry>,
    _shared: &Rc<RefCell<HashMap<FocusPath, ImageEntry>>>,
    key: &FocusPath,
    _generation: u64,
    src: &str,
    _cache: &Arc<florui_assets::AssetCache>,
    _executor: &dyn Executor,
) {
    let error = ImageLoadError::Asset(florui_assets::AssetError::Fetch {
        url: src.to_owned(),
        error: florui_assets::FetchError::Unavailable,
    });
    eprintln!("florui-platform: <img src=\"{src}\"> failed to load: {error}");
    if let Some(entry) = entries.get_mut(key) {
        entry.state = ImageState::Failed(Arc::new(error));
    }
}

/// What a downloaded picture became.
#[cfg(feature = "net")]
enum Downloaded {
    Raster(Arc<florui_assets::RasterImage>),
    /// Parsed far enough to know its size; rasterized later, when layout knows the size
    /// it is drawn at.
    Svg {
        intrinsic: florui_assets::Size2D,
        bytes: SharedBytes,
    },
}

/// Starts downloading a network `src`: the request goes to the shared fetcher, whose
/// queue and limits are the process's own, and its ticket is kept in the entry so that
/// the entry going away (the `<img>` unmounted, or `src` changed) cancels it. A picture
/// that is already decoded and alive in `cache` is used without any request.
///
/// The download finishes on a thread of the fetcher and wakes the task below through
/// [`delivery`]; decoding then runs in a decode slot like any other image.
#[cfg(feature = "net")]
fn start_remote(
    entries: &mut HashMap<FocusPath, ImageEntry>,
    shared: &Rc<RefCell<HashMap<FocusPath, ImageEntry>>>,
    key: &FocusPath,
    generation: u64,
    src: &str,
    cache: &Arc<florui_assets::AssetCache>,
    executor: &dyn Executor,
) {
    use florui_assets::{AssetError, AssetId, DecodeParams};

    // The address as it is requested, so that the same picture written twice is one.
    let src = florui_assets::url_image::remote_address(src);
    let Some(entry) = entries.get_mut(key) else {
        return;
    };
    let id = AssetId::Url(src.to_owned());
    if let Some(image) = cache.cached(&id, DecodeParams::Native) {
        entry.state = ImageState::Ready(image);
        return;
    }
    let (sender, receiver) = crate::delivery::delivery();
    let ticket = florui_assets::net::shared_fetcher().submit(src, move |outcome| {
        sender.send(outcome);
    });
    let ticket = match ticket {
        Ok(ticket) => ticket,
        Err(error) => {
            let error = ImageLoadError::Asset(AssetError::Fetch {
                url: src.to_owned(),
                error,
            });
            eprintln!("florui-platform: <img src=\"{src}\"> failed to load: {error}");
            entry.state = ImageState::Failed(Arc::new(error));
            return;
        }
    };
    entry.ticket = Some(ticket);

    let cache = Arc::clone(cache);
    let entries_for_task = Rc::clone(shared);
    let key_for_task = key.clone();
    let src_for_task = src.to_owned();
    executor.spawn(Box::pin(async move {
        // `None`: the request was cancelled, and with it this entry's interest.
        let Some(outcome) = receiver.await else {
            return;
        };
        let url = src_for_task.clone();
        let result = match outcome {
            Err(error) => Err(AssetError::Fetch { url, error }),
            Ok(fetched) => {
                florui_reactive::blocking::spawn_blocking(move || {
                    if florui_assets::Format::sniff(&fetched.bytes).is_some() {
                        return cache
                            .get_or_decode(id, &fetched.bytes, DecodeParams::Native)
                            .map(Downloaded::Raster);
                    }
                    let text = std::str::from_utf8(&fetched.bytes)
                        .map_err(|_| AssetError::InvalidUtf8 { path: None })?;
                    // Someone else's document: it may not name files of ours.
                    let image = florui_assets::parse_svg_untrusted(text)?;
                    Ok(Downloaded::Svg {
                        intrinsic: image.intrinsic_size,
                        bytes: fetched,
                    })
                })
                .await
            }
        };
        let mut entries = entries_for_task.borrow_mut();
        let Some(entry) = entries.get_mut(&key_for_task) else {
            return;
        };
        if entry.generation != generation {
            return;
        }
        entry.ticket = None;
        match result {
            Ok(Downloaded::Raster(image)) => entry.state = ImageState::Ready(image),
            Ok(Downloaded::Svg { intrinsic, bytes }) => {
                entry.svg_intrinsic_size = Some(Size {
                    width: intrinsic.width,
                    height: intrinsic.height,
                });
                entry.svg_bytes = Some(bytes);
            }
            Err(error) => {
                let error = ImageLoadError::Asset(error);
                eprintln!("florui-platform: <img src=\"{src_for_task}\"> failed to load: {error}");
                entry.state = ImageState::Failed(Arc::new(error));
            }
        }
    }));
}

enum Format {
    /// A PNG, JPEG, WebP or GIF.
    Raster,
    Svg,
}

/// `None` for anything but a raster image's extension or `.svg` (case-insensitive).
/// See this module's own doc on format scope.
fn classify_format(path: &Path) -> Option<Format> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("svg") => Some(Format::Svg),
        Some(extension) => florui_assets::Format::of_extension(extension).map(|_| Format::Raster),
        None => None,
    }
}

/// Shared tail of a completing background load/rasterize task: commits
/// `result` into `key`'s own entry, but only if it's still there and its
/// generation still matches `generation` — rejects a stale completion the
/// same way both call sites (PNG decode, SVG rasterize) need to. Logs a
/// failure once, here, the one place either path's error ever finishes.
fn commit(
    entries: &Rc<RefCell<HashMap<FocusPath, ImageEntry>>>,
    key: &FocusPath,
    generation: u64,
    src: &str,
    result: impl FnOnce() -> Result<ImageState, florui_assets::AssetError>,
) {
    let mut entries = entries.borrow_mut();
    if let Some(entry) = entries.get_mut(key)
        && entry.generation == generation
    {
        entry.state = match result() {
            Ok(state) => state,
            Err(err) => {
                let error = ImageLoadError::Asset(err);
                eprintln!("florui-platform: <img src=\"{src}\"> failed to load: {error}");
                ImageState::Failed(Arc::new(error))
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use florui::prelude::*;
    use florui_reactive::executor::LocalExecutor;
    use florui_style::Arena;

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([x as u8, y as u8, 128, 255])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

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

    // A 2:1 real SVG, matching this crate's own layout tests' own fixture
    // shape -- real intrinsic size (100x50), a real fill to prove
    // rasterization actually ran (not just an empty canvas).
    const SVG_TEXT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 50">
  <rect width="100" height="50" fill="#3366ff"/>
</svg>"##;

    #[test]
    fn a_png_source_loads_and_reports_its_real_intrinsic_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        std::fs::write(&path, png_bytes(17, 9)).unwrap();

        let tree: Element = view! { <img src={path.display().to_string()} /> };
        let arena = Arena::build(&tree);
        let registry = ImageRegistry::new();
        let cache = Arc::new(florui_assets::AssetCache::new());
        let executor = LocalExecutor::new();

        registry.sync(&arena, &cache, &executor);
        let key = FocusPath::of(&arena, arena.roots()[0]);
        assert!(registry.intrinsic_size(&key).is_none(), "not ready yet");

        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.intrinsic_size(&key).is_some()
        }));
        assert_eq!(
            registry.intrinsic_size(&key),
            Some(Size {
                width: 17.0,
                height: 9.0
            })
        );
    }

    #[test]
    fn a_jpeg_source_loads_and_reports_its_real_intrinsic_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cover.jpg");
        let image = image::RgbImage::from_fn(30, 20, |x, y| image::Rgb([x as u8, y as u8, 90]));
        image::DynamicImage::ImageRgb8(image)
            .save_with_format(&path, image::ImageFormat::Jpeg)
            .unwrap();
        let tree: Element = view! { <img src={path.display().to_string()} /> };
        let arena = Arena::build(&tree);
        let registry = ImageRegistry::new();
        let cache = Arc::new(florui_assets::AssetCache::new());
        let executor = LocalExecutor::new();
        registry.sync(&arena, &cache, &executor);
        let key = FocusPath::of(&arena, arena.roots()[0]);
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.intrinsic_size(&key).is_some()
        }));
        assert_eq!(
            registry.intrinsic_size(&key),
            Some(Size {
                width: 30.0,
                height: 20.0
            })
        );
    }

    #[test]
    fn a_missing_file_reports_a_load_error_not_a_panic() {
        let tree: Element = view! { <img src="does/not/exist.png" /> };
        let arena = Arena::build(&tree);
        let registry = ImageRegistry::new();
        let cache = Arc::new(florui_assets::AssetCache::new());
        let executor = LocalExecutor::new();

        registry.sync(&arena, &cache, &executor);
        let key = FocusPath::of(&arena, arena.roots()[0]);

        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.error(&key).is_some()
        }));
        assert!(matches!(
            *registry.error(&key).unwrap(),
            ImageLoadError::Asset(florui_assets::AssetError::Io { .. })
        ));
    }

    #[test]
    fn an_unsupported_extension_fails_immediately_without_a_background_task() {
        let tree: Element = view! { <img src="photo.bmp" /> };
        let arena = Arena::build(&tree);
        let registry = ImageRegistry::new();
        let cache = Arc::new(florui_assets::AssetCache::new());
        let executor = LocalExecutor::new();

        registry.sync(&arena, &cache, &executor);
        let key = FocusPath::of(&arena, arena.roots()[0]);
        assert!(matches!(
            registry.error(&key).as_deref(),
            Some(ImageLoadError::UnsupportedFormat { .. })
        ));
    }

    #[test]
    fn an_svg_source_reports_its_intrinsic_size_before_any_rasterization() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.svg");
        std::fs::write(&path, SVG_TEXT).unwrap();

        let tree: Element = view! { <img src={path.display().to_string()} /> };
        let arena = Arena::build(&tree);
        let registry = ImageRegistry::new();
        let cache = Arc::new(florui_assets::AssetCache::new());
        let executor = LocalExecutor::new();

        registry.sync(&arena, &cache, &executor);
        let key = FocusPath::of(&arena, arena.roots()[0]);

        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.intrinsic_size(&key).is_some()
        }));
        assert_eq!(
            registry.intrinsic_size(&key),
            Some(Size {
                width: 100.0,
                height: 50.0
            })
        );
        // Real intrinsic size known, but nothing rasterized yet -- no
        // pixels to paint until something actually requests a size.
        assert!(registry.decoded(&key).is_none());
    }

    #[test]
    fn request_raster_produces_real_pixels_at_the_requested_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.svg");
        std::fs::write(&path, SVG_TEXT).unwrap();
        let src = path.display().to_string();

        let tree: Element = view! { <img src={src.clone()} /> };
        let arena = Arena::build(&tree);
        let registry = ImageRegistry::new();
        let cache = Arc::new(florui_assets::AssetCache::new());
        let executor = LocalExecutor::new();

        registry.sync(&arena, &cache, &executor);
        let key = FocusPath::of(&arena, arena.roots()[0]);
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.intrinsic_size(&key).is_some()
        }));

        registry.request_raster(
            &key,
            &src,
            florui_assets::RasterFit::Contain {
                width: 64,
                height: 32,
            },
            &cache,
            &executor,
        );
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.decoded(&key).is_some()
        }));
        let raster = registry.decoded(&key).unwrap();
        assert_eq!((raster.width, raster.height), (64, 32));
    }

    #[test]
    fn request_raster_before_intrinsic_size_is_known_is_a_harmless_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.svg");
        std::fs::write(&path, SVG_TEXT).unwrap();
        let src = path.display().to_string();

        let tree: Element = view! { <img src={src.clone()} /> };
        let arena = Arena::build(&tree);
        let registry = ImageRegistry::new();
        let cache = Arc::new(florui_assets::AssetCache::new());
        let executor = LocalExecutor::new();

        registry.sync(&arena, &cache, &executor);
        let key = FocusPath::of(&arena, arena.roots()[0]);
        // Immediately, before the background parse has had any chance to
        // run at all.
        registry.request_raster(
            &key,
            &src,
            florui_assets::RasterFit::Contain {
                width: 64,
                height: 32,
            },
            &cache,
            &executor,
        );
        assert!(registry.decoded(&key).is_none());

        // The real parse (and only then a real raster, once requested
        // again) still completes normally afterward.
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.intrinsic_size(&key).is_some()
        }));
    }

    #[test]
    fn request_raster_at_an_unchanged_size_does_not_produce_a_new_arc() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.svg");
        std::fs::write(&path, SVG_TEXT).unwrap();
        let src = path.display().to_string();

        let tree: Element = view! { <img src={src.clone()} /> };
        let arena = Arena::build(&tree);
        let registry = ImageRegistry::new();
        let cache = Arc::new(florui_assets::AssetCache::new());
        let executor = LocalExecutor::new();

        registry.sync(&arena, &cache, &executor);
        let key = FocusPath::of(&arena, arena.roots()[0]);
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.intrinsic_size(&key).is_some()
        }));

        let fit = florui_assets::RasterFit::Contain {
            width: 64,
            height: 32,
        };
        registry.request_raster(&key, &src, fit, &cache, &executor);
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.decoded(&key).is_some()
        }));
        let first = registry.decoded(&key).unwrap();

        // Same size again -- every real redraw calls this, most of which
        // never resize anything.
        registry.request_raster(&key, &src, fit, &cache, &executor);
        executor.run_until_stalled();
        let second = registry.decoded(&key).unwrap();
        assert!(
            Arc::ptr_eq(&first, &second),
            "an unchanged raster target should not re-rasterize"
        );
    }

    #[test]
    fn request_raster_at_a_new_size_produces_a_real_new_raster() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.svg");
        std::fs::write(&path, SVG_TEXT).unwrap();
        let src = path.display().to_string();

        let tree: Element = view! { <img src={src.clone()} /> };
        let arena = Arena::build(&tree);
        let registry = ImageRegistry::new();
        let cache = Arc::new(florui_assets::AssetCache::new());
        let executor = LocalExecutor::new();

        registry.sync(&arena, &cache, &executor);
        let key = FocusPath::of(&arena, arena.roots()[0]);
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.intrinsic_size(&key).is_some()
        }));

        registry.request_raster(
            &key,
            &src,
            florui_assets::RasterFit::Contain {
                width: 64,
                height: 32,
            },
            &cache,
            &executor,
        );
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.decoded(&key).map(|image| image.width) == Some(64)
        }));

        // A resize/DPI change -- a real new target size.
        registry.request_raster(
            &key,
            &src,
            florui_assets::RasterFit::Contain {
                width: 200,
                height: 100,
            },
            &cache,
            &executor,
        );
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.decoded(&key).map(|image| image.width) == Some(200)
        }));
    }

    #[test]
    fn changing_src_starts_a_fresh_load_and_rejects_the_stale_one() {
        let dir = tempfile::tempdir().unwrap();
        let red_path = dir.path().join("red.png");
        let blue_path = dir.path().join("blue.png");
        std::fs::write(&red_path, png_bytes(10, 10)).unwrap();
        std::fs::write(&blue_path, png_bytes(20, 20)).unwrap();

        let registry = ImageRegistry::new();
        let cache = Arc::new(florui_assets::AssetCache::new());
        let executor = LocalExecutor::new();

        let first_tree: Element = view! { <img src={red_path.display().to_string()} /> };
        let arena = Arena::build(&first_tree);
        registry.sync(&arena, &cache, &executor);
        let key = FocusPath::of(&arena, arena.roots()[0]);
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.intrinsic_size(&key).is_some()
        }));
        assert_eq!(registry.intrinsic_size(&key).unwrap().width, 10.0);

        // Same structural position, a different `src` -- a real "src
        // swap", not a remount.
        let second_tree: Element = view! { <img src={blue_path.display().to_string()} /> };
        let arena = Arena::build(&second_tree);
        registry.sync(&arena, &cache, &executor);
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.intrinsic_size(&key).map(|size| size.width) == Some(20.0)
        }));
    }

    #[test]
    fn an_img_no_longer_present_is_dropped_from_the_registry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        std::fs::write(&path, png_bytes(5, 5)).unwrap();

        let registry = ImageRegistry::new();
        let cache = Arc::new(florui_assets::AssetCache::new());
        let executor = LocalExecutor::new();

        let with_img: Element = view! { <div><img src={path.display().to_string()} /></div> };
        let arena = Arena::build(&with_img);
        registry.sync(&arena, &cache, &executor);
        let img_node = arena.children(arena.roots()[0])[0];
        let key = FocusPath::of(&arena, img_node);
        assert!(wait_until(|| {
            executor.run_until_stalled();
            registry.intrinsic_size(&key).is_some()
        }));

        let without_img: Element = view! { <div /> };
        let arena = Arena::build(&without_img);
        registry.sync(&arena, &cache, &executor);
        assert!(registry.intrinsic_size(&key).is_none());
        assert_eq!(registry.entries.borrow().len(), 0);
    }

    #[cfg(feature = "net")]
    mod network {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        use super::*;
        use crate::test_http::{Server, configure_fetcher as configure};

        const NO_DELAY: Duration = Duration::ZERO;

        fn load_one(
            tree: &Element,
        ) -> (
            Arena,
            ImageRegistry,
            Arc<florui_assets::AssetCache>,
            LocalExecutor,
        ) {
            configure();
            let arena = Arena::build(tree);
            let registry = ImageRegistry::new();
            let cache = Arc::new(florui_assets::AssetCache::new());
            let executor = LocalExecutor::new();
            registry.sync(&arena, &cache, &executor);
            (arena, registry, cache, executor)
        }

        #[test]
        fn a_png_over_http_loads_and_reports_its_real_intrinsic_size() {
            let server = Server::start(|_| (200, "image/png", png_bytes(17, 9), NO_DELAY));
            // No extension to go by: the bytes say what it is.
            let src = server.url("/avatar?id=3");
            let tree: Element = view! { <img src={src} /> };
            let (arena, registry, _cache, executor) = load_one(&tree);
            let key = FocusPath::of(&arena, arena.roots()[0]);
            assert!(registry.intrinsic_size(&key).is_none(), "not ready yet");
            assert!(wait_until(|| {
                executor.run_until_stalled();
                registry.decoded(&key).is_some()
            }));
            assert_eq!(
                registry.intrinsic_size(&key),
                Some(Size {
                    width: 17.0,
                    height: 9.0
                })
            );
            assert_eq!(server.seen(), ["/avatar?id=3"]);
        }

        #[test]
        fn a_picture_already_decoded_and_alive_is_not_downloaded_again() {
            let server = Server::start(|_| (200, "image/png", png_bytes(4, 4), NO_DELAY));
            let tree: Element = view! { <img src={server.url("/a.png")} /> };
            let (arena, registry, cache, executor) = load_one(&tree);
            let key = FocusPath::of(&arena, arena.roots()[0]);
            assert!(wait_until(|| {
                executor.run_until_stalled();
                registry.decoded(&key).is_some()
            }));
            let held = registry.decoded(&key).unwrap();
            // Another registry, as another window has: the decode is shared.
            let other = ImageRegistry::new();
            other.sync(&arena, &cache, &executor);
            let again = other.decoded(&key).expect("ready at once");
            assert!(Arc::ptr_eq(&held, &again));
            assert_eq!(server.seen().len(), 1);
        }

        #[test]
        fn an_svg_over_http_reports_its_size_and_rasterizes_at_the_requested_fit() {
            let svg = SVG_TEXT.as_bytes().to_vec();
            let server = Server::start(move |_| (200, "image/svg+xml", svg.clone(), NO_DELAY));
            let src = server.url("/logo.svg");
            let tree: Element = view! { <img src={src.clone()} /> };
            let (arena, registry, cache, executor) = load_one(&tree);
            let key = FocusPath::of(&arena, arena.roots()[0]);
            assert!(wait_until(|| {
                executor.run_until_stalled();
                registry.intrinsic_size(&key).is_some()
            }));
            assert_eq!(
                registry.intrinsic_size(&key),
                Some(Size {
                    width: 100.0,
                    height: 50.0
                })
            );
            let fit = florui_assets::RasterFit::Stretch {
                width: 40,
                height: 20,
            };
            registry.request_raster(&key, &src, fit, &cache, &executor);
            assert!(wait_until(|| {
                executor.run_until_stalled();
                registry.decoded(&key).is_some()
            }));
            let raster = registry.decoded(&key).unwrap();
            assert_eq!((raster.width, raster.height), (40, 20));
            // Rasterizing used the document that was already here.
            assert_eq!(server.seen().len(), 1);
        }

        #[test]
        fn a_status_that_is_not_a_success_fails_the_image_with_the_reason() {
            let server = Server::start(|_| (404, "text/plain", b"gone".to_vec(), NO_DELAY));
            let tree: Element = view! { <img src={server.url("/missing.png")} /> };
            let (arena, registry, _cache, executor) = load_one(&tree);
            let key = FocusPath::of(&arena, arena.roots()[0]);
            assert!(wait_until(|| {
                executor.run_until_stalled();
                registry.error(&key).is_some()
            }));
            assert!(matches!(
                &*registry.error(&key).unwrap(),
                ImageLoadError::Asset(florui_assets::AssetError::Fetch {
                    error: florui_assets::FetchError::Status(404),
                    ..
                })
            ));
            assert!(registry.decoded(&key).is_none());
            assert!(registry.intrinsic_size(&key).is_none());
        }

        #[test]
        fn a_page_served_as_an_image_fails_before_any_decoder() {
            let server = Server::start(|_| (200, "text/html", b"<html></html>".to_vec(), NO_DELAY));
            let tree: Element = view! { <img src={server.url("/a.png")} /> };
            let (arena, registry, _cache, executor) = load_one(&tree);
            let key = FocusPath::of(&arena, arena.roots()[0]);
            assert!(wait_until(|| {
                executor.run_until_stalled();
                registry.error(&key).is_some()
            }));
            assert!(matches!(
                &*registry.error(&key).unwrap(),
                ImageLoadError::Asset(florui_assets::AssetError::Fetch {
                    error: florui_assets::FetchError::NotAnImage { .. },
                    ..
                })
            ));
        }

        #[test]
        fn an_image_that_unmounts_takes_its_queued_request_with_it() {
            let slow = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&slow);
            let server = Server::start(move |path| {
                counter.fetch_add(1, Ordering::SeqCst);
                let delay = if path == "/first.png" {
                    Duration::from_millis(400)
                } else {
                    NO_DELAY
                };
                (200, "image/png", png_bytes(2, 2), delay)
            });
            configure();
            let both: Element = view! {
                <div>
                    <img src={server.url("/first.png")} />
                    <img src={server.url("/dropped.png")} />
                </div>
            };
            let arena = Arena::build(&both);
            let registry = ImageRegistry::new();
            let cache = Arc::new(florui_assets::AssetCache::new());
            let executor = LocalExecutor::new();
            registry.sync(&arena, &cache, &executor);
            // The first is running; the second is queued behind it (one request at a
            // time). The next render has only the first.
            assert!(wait_until(|| !server.seen().is_empty()));
            let only_first: Element = view! {
                <div>
                    <img src={server.url("/first.png")} />
                </div>
            };
            let arena = Arena::build(&only_first);
            registry.sync(&arena, &cache, &executor);
            let key = FocusPath::of(&arena, arena.find_all(|a, id| a.tag(id) == "img")[0]);
            assert!(wait_until(|| {
                executor.run_until_stalled();
                registry.decoded(&key).is_some()
            }));
            std::thread::sleep(Duration::from_millis(200));
            assert_eq!(server.seen(), ["/first.png"]);
        }

        #[test]
        fn a_changed_src_cancels_the_request_for_the_old_one() {
            let server = Server::start(|path| {
                let delay = if path == "/first.png" {
                    Duration::from_millis(400)
                } else {
                    NO_DELAY
                };
                (200, "image/png", png_bytes(2, 2), delay)
            });
            configure();
            let before: Element = view! { <img src={server.url("/first.png")} /> };
            let arena = Arena::build(&before);
            let registry = ImageRegistry::new();
            let cache = Arc::new(florui_assets::AssetCache::new());
            let executor = LocalExecutor::new();
            registry.sync(&arena, &cache, &executor);
            assert!(wait_until(|| !server.seen().is_empty()));
            let after: Element = view! { <img src={server.url("/second.png")} /> };
            let arena = Arena::build(&after);
            registry.sync(&arena, &cache, &executor);
            let key = FocusPath::of(&arena, arena.roots()[0]);
            assert!(wait_until(|| {
                executor.run_until_stalled();
                registry.decoded(&key).is_some()
            }));
            let image = registry.decoded(&key).unwrap();
            assert_eq!((image.width, image.height), (2, 2));
            assert_eq!(server.seen(), ["/first.png", "/second.png"]);
        }
    }

    #[cfg(not(feature = "net"))]
    #[test]
    fn without_the_net_feature_a_network_src_fails_with_the_reason() {
        let tree: Element = view! { <img src="https://example.com/cover.jpg" /> };
        let arena = Arena::build(&tree);
        let registry = ImageRegistry::new();
        let cache = Arc::new(florui_assets::AssetCache::new());
        let executor = LocalExecutor::new();
        registry.sync(&arena, &cache, &executor);
        let key = FocusPath::of(&arena, arena.roots()[0]);
        assert!(matches!(
            &*registry.error(&key).expect("failed at once"),
            ImageLoadError::Asset(florui_assets::AssetError::Fetch {
                error: florui_assets::FetchError::Unavailable,
                ..
            })
        ));
    }
}
