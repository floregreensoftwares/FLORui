//! A shared, deduplicating decode cache. See [`AssetCache`]'s own doc for
//! the retention policy.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

use crate::error::AssetError;
use crate::raster::{RasterImage, decode_raster};
use crate::source::{AssetId, AssetSource};
use crate::svg::{RasterFit, parse_svg, rasterize_svg, substitute_current_color};

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
#[derive(Default)]
pub struct AssetCache {
    entries: Mutex<HashMap<CacheKey, Weak<RasterImage>>>,
    /// The decodes under way, so that asking for what another thread is already
    /// decoding waits for that one instead of decoding it again.
    decoding: Mutex<HashMap<CacheKey, Arc<Decoding>>>,
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
        let key = CacheKey { id, params };

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
            let result = {
                let _slot = DecodeSlot::take();
                decode(source, params)
            };
            let decoded = result.map(Arc::new);
            if let Ok(image) = &decoded {
                self.lock().insert(key.clone(), Arc::downgrade(image));
            }
            self.decoding
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&key);
            *decoding
                .done
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = true;
            decoding.finished.notify_all();
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

fn decode(source: &AssetSource<'_>, params: DecodeParams) -> Result<RasterImage, AssetError> {
    match params {
        DecodeParams::Native => {
            let bytes = source.read_bytes()?;
            let image = decode_raster(&bytes)?;
            // What painting reads is made here, on the thread that decodes.
            image.prepare();
            Ok(image)
        }
        DecodeParams::Rasterized(fit) => {
            let text = source.read_text()?;
            let vector = parse_svg(&text)?;
            rasterize_svg(&vector, fit)
        }
        DecodeParams::RasterizedThemed(fit, color) => {
            let text = source.read_text()?;
            let themed = substitute_current_color(&text, color);
            let vector = parse_svg(&themed)?;
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
}
