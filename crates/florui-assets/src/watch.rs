//! Development-time invalidation: watch a `Path`-sourced asset's parent
//! directory and evict its cache entries when the file changes, the same
//! non-recursive parent-directory watch pattern `florui-platform`'s CSS
//! hot reload and `florui-devtools`'s preview host already use (watching
//! the parent, not the file itself, survives an editor's atomic
//! save-via-rename). Behind the `watch` feature — see `Cargo.toml`.
//!
//! This module only invalidates this crate's own cache; it does not wake
//! a host event loop or trigger a repaint. Wiring an invalidation into a
//! running application's own update/wakeup mechanism is that
//! application's integration to make (see the crate-level doc).

use std::path::Path;
use std::sync::Arc;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::cache::AssetCache;
use crate::source::AssetSource;

/// Errors from setting up a development-time watch. Decoding/reading
/// errors are reported through [`crate::AssetError`] as usual; this is
/// only about the watcher itself failing to start.
#[derive(Debug)]
pub enum WatchError {
    Notify(notify::Error),
}

impl std::fmt::Display for WatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WatchError::Notify(err) => write!(f, "could not watch asset directory: {err}"),
        }
    }
}

impl std::error::Error for WatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WatchError::Notify(err) => Some(err),
        }
    }
}

/// Watches `path`'s parent directory and, on a change to `path` itself,
/// invalidates every cache entry for it in `cache` and calls
/// `on_invalidated`. Returns a live [`RecommendedWatcher`] the caller must
/// keep alive for as long as the watch should stay active — dropping it
/// stops the watch, the same lifetime contract `notify::Watcher`
/// implementations always have.
pub fn watch_path(
    cache: Arc<AssetCache>,
    path: &Path,
    on_invalidated: impl Fn() + Send + 'static,
) -> Result<RecommendedWatcher, WatchError> {
    let watched_path = path.to_owned();
    let parent = watched_path
        .parent()
        .map(Path::to_owned)
        .unwrap_or_else(|| Path::new(".").to_owned());

    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let Ok(event) = result else { return };
        if !event.paths.iter().any(|changed| changed == &watched_path) {
            return;
        }
        if let Ok(id) = AssetSource::Path(&watched_path).identity() {
            cache.invalidate(&id);
        }
        on_invalidated();
    })
    .map_err(WatchError::Notify)?;
    watcher
        .watch(&parent, RecursiveMode::NonRecursive)
        .map_err(WatchError::Notify)?;
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::DecodeParams;
    use crate::svg::RasterFit;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    const RED_SQUARE_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
  <rect width="32" height="32" fill="#ff0000"/>
</svg>"##;

    const GREEN_SQUARE_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
  <rect width="32" height="32" fill="#00ff00"/>
</svg>"##;

    fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_file_change_invalidates_its_cache_entry_and_notifies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.svg");
        std::fs::write(&path, RED_SQUARE_SVG).unwrap();

        let cache = Arc::new(AssetCache::new());
        let params = DecodeParams::Rasterized(RasterFit::Contain {
            width: 8,
            height: 8,
        });
        let first = cache
            .get_or_load(&AssetSource::Path(&path), params)
            .unwrap();
        drop(first);

        let notified = Arc::new(AtomicBool::new(false));
        let notified_writer = Arc::clone(&notified);
        let _watcher = watch_path(Arc::clone(&cache), &path, move || {
            notified_writer.store(true, Ordering::SeqCst);
        })
        .unwrap();

        // Real OS-level file replace (matches how an editor actually
        // saves), not an in-place write to the same inode.
        let replacement = dir.path().join("icon.svg.tmp");
        std::fs::write(&replacement, GREEN_SQUARE_SVG).unwrap();
        std::fs::rename(&replacement, &path).unwrap();

        assert!(
            wait_until(|| notified.load(Ordering::SeqCst)),
            "watcher callback should fire on a real file replace"
        );

        let after = cache
            .get_or_load(&AssetSource::Path(&path), params)
            .unwrap();
        // Green, not red -- proves the cache actually re-read the new
        // file rather than serving a stale decode after invalidation.
        let pixel_offset = ((4 * 8 + 4) * 4) as usize;
        assert_eq!(after.rgba[pixel_offset], 0, "red channel");
        assert_eq!(after.rgba[pixel_offset + 1], 255, "green channel");
    }
}
