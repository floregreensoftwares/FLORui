//! A shared, deduplicating decode cache. See [`AssetCache`]'s own doc for
//! the retention policy.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use crate::error::AssetError;
use crate::raster::{RasterImage, decode_png};
use crate::source::{AssetId, AssetSource};
use crate::svg::{RasterFit, parse_svg, rasterize_svg, substitute_current_color};

/// Which decode a cache entry holds the result of. A PNG (or any future
/// raster format) decodes once at its native size regardless of how it
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

        if let Some(existing) = self.upgrade(&key) {
            return Ok(existing);
        }

        let decoded = Arc::new(decode(source, params)?);
        self.lock().insert(key, Arc::downgrade(&decoded));
        Ok(decoded)
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
            decode_png(&bytes)
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
