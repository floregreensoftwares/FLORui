//! A shared, deduplicating decode cache. See [`AssetCache`]'s own doc for
//! the retention policy.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

use crate::error::AssetError;
use crate::raster::{RasterImage, decode_raster};
use crate::source::{AssetId, AssetSource};
use crate::svg::{RasterFit, parse_svg_for, rasterize_svg, substitute_current_color};

/// Which decode a cache entry holds the result of. A raster image (PNG, JPEG,
/// WebP, GIF) decodes once at its native size regardless of how it
/// will be displayed, so it needs no size parameter; an SVG is decoded
/// per requested [`RasterFit`], since a different target size is
/// genuinely a different pixel result worth caching separately.
/// [`Self::RasterizedThemed`] additionally substitutes `currentColor`
/// (see [`substitute_current_color`]'s own doc) before rasterizing, so a
/// themed icon's own resolved color is part of its cache identity too —
/// two components showing the same icon file in two different colors are
/// genuinely two different results, not one cached result overwriting
/// the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecodeParams {
    Native,
    Rasterized(RasterFit),
    RasterizedThemed(RasterFit, [u8; 3]),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    id: AssetId,
    params: DecodeParams,
}

/// A shared, in-process cache of decoded assets, deduplicating repeated
/// requests for the same (source, decode parameters) pair across every
/// consumer.
///
/// **Retention policy**: a weak map. An entry is kept alive exactly as
/// long as at least one `Arc<RasterImage>` handle previously returned by
/// [`AssetCache::get_or_load`] is still held somewhere; once every holder
/// drops its handle, the entry is gone and the next request re-decodes
/// from scratch. This crate never times out or bounds-by-count on its
/// own — a consumer that wants a warm cache simply keeps holding the
/// handle (e.g. for as long as a mounted `<img>` element referencing that
/// source stays alive). A dropped-but-not-yet-requested-again entry
/// leaves only a dead `Weak` pointer in the map (a few words), not the
/// pixel data itself; call [`AssetCache::compact`] to reclaim even that
/// immediately rather than waiting for the next lookup on that same key.
pub struct AssetCache {
    entries: Mutex<HashMap<CacheKey, Weak<RasterImage>>>,
    /// What the images that are alive may retain altogether.
    memory_limit: u64,
    /// The decodes under way, so that asking for what another thread is already
    /// decoding waits for that one instead of decoding it again.
    decoding: Mutex<HashMap<CacheKey, Arc<Decoding>>>,
}

impl Default for AssetCache {
    fn default() -> Self {
        Self::with_memory_limit(crate::limits::MAX_RETAINED_BYTES)
    }
}

/// Clears a decode under way when its thread leaves, however it leaves: the ones
/// waiting for it are let go, and a panic in a decoder does not leave the key claimed
/// for good.
struct FinishDecode<'a> {
    cache: &'a AssetCache,
    key: &'a CacheKey,
    decoding: &'a Decoding,
}

impl Drop for FinishDecode<'_> {
    fn drop(&mut self) {
        self.cache
            .decoding
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(self.key);
        *self
            .decoding
            .done
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = true;
        self.decoding.finished.notify_all();
    }
}

/// A decode under way: finished once `done` is set.
#[derive(Default)]
struct Decoding {
    done: Mutex<bool>,
    finished: Condvar,
}

/// How many decodes run at once, whatever number of threads ask: a screen of
/// pictures loading together would otherwise decode on as many threads as there
/// are pictures, and the interface would wait for all of them.
fn decode_slots() -> &'static (Mutex<usize>, Condvar) {
    static SLOTS: OnceLock<(Mutex<usize>, Condvar)> = OnceLock::new();
    SLOTS.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(2, usize::from);
        (Mutex::new((cores / 2).clamp(1, 4)), Condvar::new())
    })
}

/// A place among the decodes that run, given back when it is dropped.
struct DecodeSlot;

impl DecodeSlot {
    fn take() -> Self {
        let (free, freed) = decode_slots();
        let mut free = free.lock().unwrap_or_else(|poison| poison.into_inner());
        while *free == 0 {
            free = freed
                .wait(free)
                .unwrap_or_else(|poison| poison.into_inner());
        }
        *free -= 1;
        Self
    }
}

impl Drop for DecodeSlot {
    fn drop(&mut self) {
        let (free, freed) = decode_slots();
        *free.lock().unwrap_or_else(|poison| poison.into_inner()) += 1;
        freed.notify_one();
    }
}

impl AssetCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// A cache whose live images may retain `limit` bytes altogether (see
    /// [`crate::limits::MAX_RETAINED_BYTES`], the default).
    pub fn with_memory_limit(limit: u64) -> Self {
        Self {
            entries: Mutex::default(),
            memory_limit: limit,
            decoding: Mutex::default(),
        }
    }

    /// What the images this cache has handed out and that are still held retain.
    fn held_bytes(&self) -> u64 {
        self.lock()
            .values()
            .filter_map(Weak::upgrade)
            .map(|image| image.retained_bytes())
            .sum()
    }

    /// Returns a cached decode for `(source, params)` if any handle to it
    /// is still alive, otherwise resolves, reads, decodes, and caches a
    /// fresh one.
    ///
    /// Synchronous and blocking: this crate intentionally owns no
    /// executor or thread pool of its own (see the crate-level doc) — a
    /// caller running on a UI thread is responsible for dispatching this
    /// call onto a background thread and delivering the result back.
    pub fn get_or_load(
        &self,
        source: &AssetSource<'_>,
        params: DecodeParams,
    ) -> Result<Arc<RasterImage>, AssetError> {
        let id = source.identity()?;
        self.get_or_decode_with(id, params, || source.read_bytes())
    }

    /// The decode of `(id, params)` if a handle to it is alive, without loading or
    /// decoding anything. For a source whose bytes are costly to come by (a download):
    /// ask here first, and only fetch on `None`.
    pub fn cached(&self, id: &AssetId, params: DecodeParams) -> Option<Arc<RasterImage>> {
        let key = CacheKey {
            id: id.clone(),
            params,
        };
        let found = self.upgrade(&key);
        if found.is_some() {
            florui_profile::count(florui_profile::Counter::AssetCacheHits, 1);
        }
        found
    }

    /// Like [`Self::get_or_load`] for bytes that are already in memory under `id`, such
    /// as a download's: a cached decode is returned, a decode under way is waited for,
    /// and otherwise `bytes` are decoded in one of the decode slots and cached. The
    /// image is shared and dropped like any other.
    pub fn get_or_decode(
        &self,
        id: AssetId,
        bytes: &[u8],
        params: DecodeParams,
    ) -> Result<Arc<RasterImage>, AssetError> {
        self.get_or_decode_with(id, params, || Ok(Cow::Borrowed(bytes)))
    }

    fn get_or_decode_with<'b>(
        &self,
        id: AssetId,
        params: DecodeParams,
        read: impl FnOnce() -> Result<Cow<'b, [u8]>, AssetError>,
    ) -> Result<Arc<RasterImage>, AssetError> {
        let key = CacheKey { id, params };
        let mut read = Some(read);

        loop {
            if let Some(existing) = self.upgrade(&key) {
                florui_profile::count(florui_profile::Counter::AssetCacheHits, 1);
                return Ok(existing);
            }
            // Either this thread decodes it, or it waits for the one that does
            // and looks again: if that failed, nothing is cached and it tries.
            let (decoding, leads) = {
                let mut under_way = self
                    .decoding
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                match under_way.get(&key) {
                    Some(decoding) => (Arc::clone(decoding), false),
                    None => {
                        let decoding = Arc::new(Decoding::default());
                        under_way.insert(key.clone(), Arc::clone(&decoding));
                        (decoding, true)
                    }
                }
            };
            if !leads {
                let mut done = decoding
                    .done
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                while !*done {
                    done = decoding
                        .finished
                        .wait(done)
                        .unwrap_or_else(|poison| poison.into_inner());
                }
                continue;
            }
            florui_profile::count(florui_profile::Counter::AssetCacheMisses, 1);
            // Runs when this leaves, a panic included, after the image is cached.
            let _finish = FinishDecode {
                cache: self,
                key: &key,
                decoding: &decoding,
            };
            let result = {
                let _slot = DecodeSlot::take();
                let read = read.take().expect("only the thread that leads decodes");
                // A download is someone else's document; a file or embedded asset is the
                // application's own.
                decode(read, params, !matches!(key.id, AssetId::Url(_)))
            };
            let decoded = result.and_then(|image| {
                let (needed, held) = (image.retained_bytes(), self.held_bytes());
                if held + needed > self.memory_limit {
                    return Err(AssetError::MemoryBudget {
                        needed,
                        held,
                        limit: self.memory_limit,
                    });
                }
                // What painting reads is made here, on the thread that decodes, once
                // the image is let in.
                if params == DecodeParams::Native {
                    image.prepare();
                }
                Ok(Arc::new(image))
            });
            if let Ok(image) = &decoded {
                self.lock().insert(key.clone(), Arc::downgrade(image));
            }
            return decoded;
        }
    }

    fn upgrade(&self, key: &CacheKey) -> Option<Arc<RasterImage>> {
        self.lock().get(key).and_then(Weak::upgrade)
    }

    /// Drops every dead (no remaining holders) entry's map slot. Never
    /// required for correctness — a dead entry is just a `Weak` pointer —
    /// but a consumer doing a large one-time asset-set teardown can call
    /// this to reclaim that bookkeeping memory immediately.
    pub fn compact(&self) {
        self.lock().retain(|_, weak| weak.strong_count() > 0);
    }

    /// Removes every cached entry for `id`, regardless of decode
    /// parameters — used when the underlying source changed on disk (see
    /// `watch.rs`) and every size/fit variant of it is now stale.
    /// Existing holders keep their already-decoded `Arc` unchanged
    /// (nothing mutates out from under a live reader); the next
    /// `get_or_load` for any param variant of `id` decodes fresh.
    #[cfg_attr(not(feature = "watch"), allow(dead_code))]
    pub(crate) fn invalidate(&self, id: &AssetId) {
        self.lock().retain(|key, _| &key.id != id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<CacheKey, Weak<RasterImage>>> {
        self.entries
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

fn decode<'b>(
    read: impl FnOnce() -> Result<Cow<'b, [u8]>, AssetError>,
    params: DecodeParams,
    trusted: bool,
) -> Result<RasterImage, AssetError> {
    let parse = |text: &str| parse_svg_for(trusted, text);
    let text = |bytes: Cow<'_, [u8]>| {
        String::from_utf8(bytes.into_owned()).map_err(|_| AssetError::InvalidUtf8 { path: None })
    };
    match params {
        DecodeParams::Native => {
            let bytes = read()?;
            decode_raster(&bytes)
        }
        DecodeParams::Rasterized(fit) => {
            let text = text(read()?)?;
            let vector = parse(&text)?;
            rasterize_svg(&vector, fit)
        }
        DecodeParams::RasterizedThemed(fit, color) => {
            let text = text(read()?)?;
            let themed = substitute_current_color(&text, color);
            let vector = parse(&themed)?;
            rasterize_svg(&vector, fit)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const BLUE_CIRCLE_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
  <circle cx="16" cy="16" r="14" fill="#3366ff"/>
</svg>"##;

    fn write_svg(dir: &tempfile::TempDir, name: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, BLUE_CIRCLE_SVG).unwrap();
        path
    }

    fn encoded(format: image::ImageFormat) -> Vec<u8> {
        let image =
            image::RgbImage::from_fn(48, 32, |x, y| image::Rgb([x as u8 * 4, y as u8 * 4, 90]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut std::io::Cursor::new(&mut bytes), format)
            .unwrap();
        bytes
    }

    #[test]
    fn every_raster_format_loads_through_the_cache_by_its_extension_or_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let cache = AssetCache::new();
        for (name, format) in [
            ("a.png", image::ImageFormat::Png),
            ("a.jpg", image::ImageFormat::Jpeg),
            ("a.webp", image::ImageFormat::WebP),
            ("a.gif", image::ImageFormat::Gif),
            // The name says nothing: the first bytes do.
            ("no-extension", image::ImageFormat::Jpeg),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, encoded(format)).unwrap();
            let image = cache
                .get_or_load(&AssetSource::Path(&path), DecodeParams::Native)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!((image.width, image.height), (48, 32), "{name}");
        }
    }

    #[test]
    fn threads_asking_for_the_same_image_at_once_share_one_decode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cover.jpg");
        std::fs::write(&path, encoded(image::ImageFormat::Jpeg)).unwrap();
        let cache = Arc::new(AssetCache::new());
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (cache, path) = (Arc::clone(&cache), path.clone());
                std::thread::spawn(move || {
                    cache
                        .get_or_load(&AssetSource::Path(&path), DecodeParams::Native)
                        .unwrap()
                })
            })
            .collect();
        let images: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        for image in &images {
            assert!(Arc::ptr_eq(&images[0], image), "one decode for all of them");
        }
    }

    #[test]
    fn an_image_that_would_pass_the_memory_limit_is_refused_until_others_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (dir.path().join("a.jpg"), dir.path().join("b.jpg"));
        std::fs::write(&first, encoded(image::ImageFormat::Jpeg)).unwrap();
        std::fs::write(&second, encoded(image::ImageFormat::Jpeg)).unwrap();
        // 48 x 32 opaque pixels retain 6144 bytes: room for one and a half.
        let cache = AssetCache::with_memory_limit(9000);
        let kept = cache
            .get_or_load(&AssetSource::Path(&first), DecodeParams::Native)
            .unwrap();
        let refused = cache
            .get_or_load(&AssetSource::Path(&second), DecodeParams::Native)
            .unwrap_err();
        assert!(
            matches!(refused, AssetError::MemoryBudget { .. }),
            "{refused}"
        );
        drop(kept);
        assert!(
            cache
                .get_or_load(&AssetSource::Path(&second), DecodeParams::Native)
                .is_ok(),
            "room again once the first is dropped"
        );
    }

    #[test]
    fn a_panic_in_a_decode_does_not_leave_the_key_claimed_or_the_waiters_behind() {
        let cache = AssetCache::new();
        let key = CacheKey {
            id: AssetId::Embedded("guard-test"),
            params: DecodeParams::Native,
        };
        let decoding = Arc::new(Decoding::default());
        cache
            .decoding
            .lock()
            .unwrap()
            .insert(key.clone(), Arc::clone(&decoding));
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _finish = FinishDecode {
                cache: &cache,
                key: &key,
                decoding: &decoding,
            };
            panic!("a decoder gave up");
        }));
        assert!(panicked.is_err());
        assert!(
            cache.decoding.lock().unwrap().is_empty(),
            "the key is free again"
        );
        assert!(
            *decoding.done.lock().unwrap(),
            "the ones waiting are let go"
        );
    }

    #[test]
    fn a_decode_that_fails_is_not_kept_and_does_not_hold_the_next_one_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.jpg");
        std::fs::write(&path, [0xFF, 0xD8, 0xFF, 0x00, 0x01]).unwrap();
        let cache = AssetCache::new();
        assert!(
            cache
                .get_or_load(&AssetSource::Path(&path), DecodeParams::Native)
                .is_err()
        );
        std::fs::write(&path, encoded(image::ImageFormat::Jpeg)).unwrap();
        assert!(
            cache
                .get_or_load(&AssetSource::Path(&path), DecodeParams::Native)
                .is_ok()
        );
    }

    #[test]
    fn get_or_load_dedupes_concurrently_held_requests() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_svg(&dir, "icon.svg");
        let cache = AssetCache::new();
        let params = DecodeParams::Rasterized(RasterFit::Contain {
            width: 32,
            height: 32,
        });

        let first = cache
            .get_or_load(&AssetSource::Path(&path), params)
            .unwrap();
        let second = cache
            .get_or_load(&AssetSource::Path(&path), params)
            .unwrap();
        assert!(
            Arc::ptr_eq(&first, &second),
            "both requests should share one decode while both handles are alive"
        );
    }

    #[test]
    fn get_or_load_redecodes_once_every_handle_has_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_svg(&dir, "icon.svg");
        let cache = AssetCache::new();
        let params = DecodeParams::Rasterized(RasterFit::Contain {
            width: 32,
            height: 32,
        });

        let first = cache
            .get_or_load(&AssetSource::Path(&path), params)
            .unwrap();
        let first_ptr = Arc::as_ptr(&first);
        drop(first);

        let second = cache
            .get_or_load(&AssetSource::Path(&path), params)
            .unwrap();
        assert_ne!(
            Arc::as_ptr(&second),
            first_ptr,
            "no live handle should mean a fresh allocation, not a stale reuse"
        );
    }

    #[test]
    fn get_or_load_treats_different_raster_fits_as_different_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_svg(&dir, "icon.svg");
        let cache = AssetCache::new();

        let small = cache
            .get_or_load(
                &AssetSource::Path(&path),
                DecodeParams::Rasterized(RasterFit::Contain {
                    width: 16,
                    height: 16,
                }),
            )
            .unwrap();
        let large = cache
            .get_or_load(
                &AssetSource::Path(&path),
                DecodeParams::Rasterized(RasterFit::Contain {
                    width: 64,
                    height: 64,
                }),
            )
            .unwrap();
        assert_eq!((small.width, small.height), (16, 16));
        assert_eq!((large.width, large.height), (64, 64));
    }

    #[test]
    fn invalidate_forces_every_param_variant_of_an_id_to_redecode() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_svg(&dir, "icon.svg");
        let cache = AssetCache::new();
        let params = DecodeParams::Rasterized(RasterFit::Contain {
            width: 32,
            height: 32,
        });

        let first = cache
            .get_or_load(&AssetSource::Path(&path), params)
            .unwrap();
        let id = AssetSource::Path(&path).identity().unwrap();
        cache.invalidate(&id);
        // The already-returned handle is untouched.
        assert_eq!(first.width, 32);

        let second = cache
            .get_or_load(&AssetSource::Path(&path), params)
            .unwrap();
        assert!(
            !Arc::ptr_eq(&first, &second),
            "invalidation should force a fresh decode even though `first` is still alive"
        );
    }

    #[test]
    fn compact_removes_only_dead_entries() {
        let dir = tempfile::tempdir().unwrap();
        let alive_path = write_svg(&dir, "alive.svg");
        let dead_path = write_svg(&dir, "dead.svg");
        let cache = AssetCache::new();
        let params = DecodeParams::Rasterized(RasterFit::Contain {
            width: 32,
            height: 32,
        });

        let alive_handle = cache
            .get_or_load(&AssetSource::Path(&alive_path), params)
            .unwrap();
        drop(
            cache
                .get_or_load(&AssetSource::Path(&dead_path), params)
                .unwrap(),
        );

        cache.compact();
        assert_eq!(
            cache.lock().len(),
            1,
            "only the still-held entry survives compact"
        );
        drop(alive_handle);
    }

    #[test]
    fn get_or_load_treats_different_theme_colors_as_different_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.svg");
        std::fs::write(
            &path,
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
  <circle cx="16" cy="16" r="14" fill="currentColor"/>
</svg>"##,
        )
        .unwrap();
        let cache = AssetCache::new();
        let fit = RasterFit::Contain {
            width: 32,
            height: 32,
        };

        let red = cache
            .get_or_load(
                &AssetSource::Path(&path),
                DecodeParams::RasterizedThemed(fit, [0xff, 0x00, 0x00]),
            )
            .unwrap();
        let green = cache
            .get_or_load(
                &AssetSource::Path(&path),
                DecodeParams::RasterizedThemed(fit, [0x00, 0xff, 0x00]),
            )
            .unwrap();
        assert!(
            !Arc::ptr_eq(&red, &green),
            "two different theme colors of the same file must not share a cache entry"
        );
        let offset = ((16 * 32 + 16) * 4) as usize;
        assert_eq!(red.rgba[offset], 255, "red channel of the red-themed icon");
        assert_eq!(
            green.rgba[offset + 1],
            255,
            "green channel of the green-themed icon"
        );
    }

    #[test]
    fn a_png_source_uses_native_decode_params() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.png");
        let image = image::RgbaImage::from_fn(3, 3, |_, _| image::Rgba([1, 2, 3, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        std::fs::write(&path, bytes).unwrap();

        let cache = AssetCache::new();
        let decoded = cache
            .get_or_load(&AssetSource::Path(&path), DecodeParams::Native)
            .unwrap();
        assert_eq!((decoded.width, decoded.height), (3, 3));
    }

    #[test]
    fn bytes_decoded_under_a_url_are_shared_while_held_and_dropped_after() {
        let cache = AssetCache::new();
        let id = AssetId::Url("https://example.com/cover.jpg".to_string());
        assert!(cache.cached(&id, DecodeParams::Native).is_none());
        let bytes = encoded(image::ImageFormat::Jpeg);
        let first = cache
            .get_or_decode(id.clone(), &bytes, DecodeParams::Native)
            .unwrap();
        assert_eq!((first.width, first.height), (48, 32));
        let second = cache
            .get_or_decode(id.clone(), &bytes, DecodeParams::Native)
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(Arc::ptr_eq(
            &first,
            &cache.cached(&id, DecodeParams::Native).unwrap()
        ));
        drop((first, second));
        assert!(cache.cached(&id, DecodeParams::Native).is_none());
    }

    #[test]
    fn bytes_that_are_not_an_image_fail_under_a_url_and_nothing_is_cached() {
        let cache = AssetCache::new();
        let id = AssetId::Url("https://example.com/not-an-image".to_string());
        let error = cache
            .get_or_decode(id.clone(), b"nope", DecodeParams::Native)
            .unwrap_err();
        assert!(matches!(error, AssetError::Raster(_)));
        assert!(cache.cached(&id, DecodeParams::Native).is_none());
    }

    #[test]
    fn an_svg_from_a_url_rasterizes_per_requested_size() {
        use crate::svg::RasterFit;
        let cache = AssetCache::new();
        let id = AssetId::Url("https://example.com/icon.svg".to_string());
        let fit = RasterFit::Stretch {
            width: 20,
            height: 20,
        };
        let image = cache
            .get_or_decode(
                id.clone(),
                BLUE_CIRCLE_SVG.as_bytes(),
                DecodeParams::Rasterized(fit),
            )
            .unwrap();
        assert_eq!((image.width, image.height), (20, 20));
        assert!(cache.cached(&id, DecodeParams::Native).is_none());
    }

    #[test]
    fn a_downloaded_svg_may_not_name_a_file_but_the_applications_own_may() {
        use crate::svg::RasterFit;
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("inner.svg");
        std::fs::write(&inner, BLUE_CIRCLE_SVG).unwrap();
        let outer = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="32" height="32"><image xlink:href="{}" width="32" height="32"/></svg>"##,
            inner.display()
        );
        let fit = RasterFit::Stretch {
            width: 32,
            height: 32,
        };
        let drawn = |image: &RasterImage| image.rgba.chunks(4).any(|pixel| pixel[3] != 0);
        let cache = AssetCache::new();
        let params = DecodeParams::Rasterized(fit);
        let trusted = cache
            .get_or_decode(AssetId::Embedded("own"), outer.as_bytes(), params)
            .unwrap();
        assert!(
            drawn(&trusted),
            "the application's own document reads the file"
        );
        let downloaded = cache
            .get_or_decode(
                AssetId::Url("https://example.com/a.svg".to_owned()),
                outer.as_bytes(),
                params,
            )
            .unwrap();
        assert!(!drawn(&downloaded), "a download does not");
    }
}
