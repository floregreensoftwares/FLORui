//! The pictures a `url(...)` names at an `http://` or `https://` address.
//!
//! Layout and painting ask for an image on the thread that needs it, and a download
//! cannot be waited for there. So asking for one that is not here starts the download in
//! the background and answers "not available" (the layer draws nothing and a list marker
//! is the broken one), and when the picture is decoded, or the download failed, the
//! listener set with [`on_change`] is called so the host lays out and paints again. The
//! next ask finds it.
//!
//! Downloads go through [`crate::net::shared_fetcher`], so they share its queue, its
//! limits and its disk cache with every other picture that names a URL, and decoding goes
//! through one [`AssetCache`], so the decode slots and the memory budget are the ones of
//! every other image. What is held for the next frame is limited too: pictures that are
//! ready stay while they fit in [`MAX_HELD_BYTES`], least recently asked for going first,
//! and one that went comes back from the disk cache when it is asked for again. A failure
//! is remembered for [`RETRY_AFTER`] and then tried again by the next ask.
//!
//! Without the `net` feature an address like this fails at once, with the reason.

// Without the `net` feature every address fails at once, so most of what follows is idle.
#![cfg_attr(not(feature = "net"), allow(dead_code))]

use std::collections::HashMap;
#[cfg(feature = "net")]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::url_image::Loaded;
#[cfg(feature = "net")]
use crate::{AssetCache, AssetId, DecodeParams};

/// How long a picture that failed to load is left alone before the next ask tries again.
const RETRY_AFTER: Duration = Duration::from_secs(30);

/// What pictures that are ready may hold altogether, in bytes, for the next frame.
const MAX_HELD_BYTES: u64 = 256 * 1024 * 1024;

#[cfg(feature = "net")]
type Ticket = crate::net::FetchTicket;
#[cfg(not(feature = "net"))]
type Ticket = ();

enum Slot {
    /// The download or the decode is under way; dropping this cancels it.
    Pending {
        generation: u64,
        _ticket: Ticket,
    },
    Ready {
        generation: u64,
        loaded: Arc<Loaded>,
        bytes: u64,
        used: u64,
    },
    Failed {
        generation: u64,
        at: Instant,
    },
}

impl Slot {
    fn generation(&self) -> u64 {
        match self {
            Slot::Pending { generation, .. }
            | Slot::Ready { generation, .. }
            | Slot::Failed { generation, .. } => *generation,
        }
    }
}

#[derive(Default)]
struct Store {
    slots: HashMap<String, Slot>,
    clock: u64,
    next_generation: u64,
    held: u64,
}

impl Store {
    fn generation(&mut self) -> u64 {
        self.next_generation += 1;
        self.next_generation
    }

    /// Lets the least recently used ready pictures go until what is held fits, except
    /// `keep`.
    fn evict(&mut self, keep: &str) {
        while self.held > MAX_HELD_BYTES {
            let oldest = self
                .slots
                .iter()
                .filter_map(|(url, slot)| match slot {
                    Slot::Ready { used, .. } if url != keep => Some((*used, url.clone())),
                    _ => None,
                })
                .min();
            let Some((_, url)) = oldest else { return };
            if let Some(Slot::Ready { bytes, .. }) = self.slots.remove(&url) {
                self.held -= bytes;
            }
        }
    }
}

static STORE: Mutex<Option<Store>> = Mutex::new(None);
type Listener = Arc<dyn Fn() + Send + Sync>;
static LISTENER: Mutex<Option<Listener>> = Mutex::new(None);

fn with_store<T>(work: impl FnOnce(&mut Store) -> T) -> T {
    let mut guard = STORE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    work(guard.get_or_insert_with(Store::default))
}

/// Whether `url` is one this module loads.
pub fn is_remote(url: &str) -> bool {
    let start = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    start.starts_with("http://") || start.starts_with("https://")
}

/// Calls `listener` (from a thread of the fetcher) whenever a picture that was asked for
/// has loaded or failed to, so the host can lay out and paint again. One listener serves
/// the whole process; setting another replaces it.
pub fn on_change(listener: impl Fn() + Send + Sync + 'static) {
    *LISTENER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(listener));
}

fn notify() {
    let listener = LISTENER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(listener) = listener {
        listener();
    }
}

/// Text that stands for the state `url` is in, for keys of things made from it.
pub(crate) fn identity(url: &str) -> String {
    let generation = with_store(|store| store.slots.get(url).map(Slot::generation));
    match generation {
        Some(generation) => format!("{url}@remote{generation}"),
        None => format!("{url}@remote-none"),
    }
}

/// The picture, if it is here. Otherwise starts getting it (unless that is under way or
/// failed lately) and answers `None`.
pub(crate) fn load(url: &str) -> Option<Arc<Loaded>> {
    with_store(|store| {
        store.clock += 1;
        let clock = store.clock;
        match store.slots.get_mut(url) {
            Some(Slot::Ready { loaded, used, .. }) => {
                *used = clock;
                return Some(Arc::clone(loaded));
            }
            Some(Slot::Pending { .. }) => return None,
            Some(Slot::Failed { at, .. }) if at.elapsed() < RETRY_AFTER => return None,
            _ => {}
        }
        let generation = store.generation();
        let slot = start(url, generation);
        store.slots.insert(url.to_owned(), slot);
        None
    })
}

#[cfg(not(feature = "net"))]
fn start(url: &str, generation: u64) -> Slot {
    report(url, &crate::FetchError::Unavailable.to_string());
    Slot::Failed {
        generation,
        at: Instant::now(),
    }
}

#[cfg(feature = "net")]
fn start(url: &str, generation: u64) -> Slot {
    let owned = url.to_owned();
    match crate::net::shared_fetcher()
        .submit(url, move |outcome| finish(&owned, generation, outcome))
    {
        Ok(ticket) => Slot::Pending {
            generation,
            _ticket: ticket,
        },
        Err(error) => {
            report(url, &error.to_string());
            Slot::Failed {
                generation,
                at: Instant::now(),
            }
        }
    }
}

fn report(url: &str, reason: &str) {
    let shown: String = url.chars().take(80).collect();
    eprintln!("florui-assets: the image url({shown}) cannot be used: {reason}");
}

#[cfg(feature = "net")]
fn cache() -> &'static AssetCache {
    static CACHE: OnceLock<AssetCache> = OnceLock::new();
    CACHE.get_or_init(AssetCache::new)
}

/// A download ended: decode it here, on the fetcher's thread and in a decode slot, and
/// record what became of it.
#[cfg(feature = "net")]
fn finish(
    url: &str,
    generation: u64,
    outcome: Result<Arc<crate::net::Fetched>, crate::FetchError>,
) {
    let built = match outcome {
        Ok(fetched) => {
            let size = fetched.bytes.len() as u64;
            crate::url_image::build(fetched.bytes.clone(), None, |bytes| {
                cache().get_or_decode(AssetId::Url(url.to_owned()), bytes, DecodeParams::Native)
            })
            .map(|loaded| (loaded, size))
        }
        Err(error) => Err(error.to_string()),
    };
    if let Err(reason) = &built {
        report(url, reason);
    }
    let changed = with_store(|store| {
        // Nobody asks any more (it was dropped from the store, and the download with it).
        let waiting = matches!(
            store.slots.get(url),
            Some(Slot::Pending { generation: g, .. }) if *g == generation
        );
        if !waiting {
            return false;
        }
        let generation = store.generation();
        store.clock += 1;
        let slot = match built {
            Ok((loaded, size)) => {
                let bytes = match &loaded.source {
                    crate::url_image::Source::Raster(image) => image.retained_bytes(),
                    crate::url_image::Source::Vector(_) => size,
                };
                store.held += bytes;
                Slot::Ready {
                    generation,
                    loaded: Arc::new(loaded),
                    bytes,
                    used: store.clock,
                }
            }
            Err(_) => Slot::Failed {
                generation,
                at: Instant::now(),
            },
        };
        store.slots.insert(url.to_owned(), slot);
        store.evict(url);
        true
    });
    if changed {
        notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_and_https_addresses_are_remote() {
        assert!(is_remote("http://example.com/a.png"));
        assert!(is_remote("https://example.com/a.png"));
        assert!(is_remote("HTTPS://example.com/a.png"));
        assert!(!is_remote("data:image/png;base64,AAAA"));
        assert!(!is_remote("assets/a.png"));
        assert!(!is_remote("ftp://example.com/a.png"));
        assert!(!is_remote("https:/"));
        // A multi-byte character at the cut is not a panic.
        assert!(!is_remote("httpé://x"));
    }

    #[test]
    fn eviction_drops_the_least_recently_used_ready_picture_and_never_the_one_kept() {
        let ready = |used, bytes| Slot::Ready {
            generation: 1,
            loaded: Arc::new(Loaded {
                source: crate::url_image::Source::Vector(Box::new(
                    crate::parse_svg(
                        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"/>",
                    )
                    .unwrap(),
                )),
                intrinsic: (1.0, 1.0),
                natural: crate::url_image::Natural::default(),
            }),
            bytes,
            used,
        };
        let mut store = Store::default();
        for (url, used) in [("a", 1), ("b", 2), ("c", 3)] {
            store
                .slots
                .insert(url.to_owned(), ready(used, MAX_HELD_BYTES / 2));
            store.held += MAX_HELD_BYTES / 2;
        }
        // 1.5 budgets held: "a" goes, and then "b" would be next but it fits.
        store.evict("c");
        assert!(!store.slots.contains_key("a"));
        assert!(store.slots.contains_key("b") && store.slots.contains_key("c"));
        assert_eq!(store.held, MAX_HELD_BYTES);
        // Over budget with only the kept picture left: it stays.
        store.held = MAX_HELD_BYTES * 2;
        store.evict("c");
        assert!(store.slots.contains_key("c") && !store.slots.contains_key("b"));
    }

    #[cfg(feature = "net")]
    mod downloads {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use super::*;
        use crate::net::test_server::{Reply, Server};
        use crate::net::{NetConfig, ProxyMode};

        fn png() -> Vec<u8> {
            let image = image::RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255]));
            let mut bytes = Vec::new();
            image::DynamicImage::ImageRgba8(image)
                .write_to(
                    &mut std::io::Cursor::new(&mut bytes),
                    image::ImageFormat::Png,
                )
                .unwrap();
            bytes
        }

        /// The shared fetcher goes direct in these tests, whatever the environment says.
        fn direct() {
            crate::net::configure(NetConfig {
                proxy: ProxyMode::Direct,
                ..NetConfig::default()
            });
        }

        fn wait_for<T>(mut ask: impl FnMut() -> Option<T>) -> Option<T> {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if let Some(found) = ask() {
                    return Some(found);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            None
        }

        #[test]
        fn asking_never_waits_and_the_picture_is_there_once_it_has_loaded() {
            direct();
            let server = Server::start(|_| {
                Reply::ok("image/png", png()).delayed(Duration::from_millis(300))
            });
            let url = server.url("/cover.png");
            let asked = Instant::now();
            assert!(crate::url_image::load(&url).is_none());
            assert!(asked.elapsed() < Duration::from_millis(250));
            let before = crate::url_image::identity(&url);
            let loaded = wait_for(|| crate::url_image::load(&url)).expect("the picture loads");
            assert_eq!(loaded.intrinsic, (3.0, 2.0));
            assert!(matches!(loaded.source, crate::url_image::Source::Raster(_)));
            // What is made from the picture is made again once it is here.
            assert_ne!(crate::url_image::identity(&url), before);
            // Asked for again, it is the same picture and nothing is downloaded again.
            let again = crate::url_image::load(&url).unwrap();
            assert!(Arc::ptr_eq(&loaded, &again));
            assert_eq!(server.hits(), 1);
        }

        #[test]
        fn the_listener_hears_of_a_picture_that_loaded_and_of_one_that_failed() {
            direct();
            let heard = Arc::new(AtomicUsize::new(0));
            let count = Arc::clone(&heard);
            crate::url_image::on_remote_change(move || {
                count.fetch_add(1, Ordering::SeqCst);
            });
            let server = Server::start(|request| match request.path.as_str() {
                "/ok.png" => Reply::ok("image/png", png()),
                _ => Reply::status(404),
            });
            let (ok, gone) = (server.url("/ok.png"), server.url("/gone.png"));
            assert!(crate::url_image::load(&ok).is_none());
            assert!(crate::url_image::load(&gone).is_none());
            assert!(wait_for(|| crate::url_image::load(&ok)).is_some());
            // Every picture asked for here ended in one call, whatever other tests do.
            assert!(wait_for(|| (heard.load(Ordering::SeqCst) >= 2).then_some(())).is_some());
        }

        #[test]
        fn a_failure_is_remembered_and_not_asked_again_at_once() {
            direct();
            let server = Server::start(|_| Reply::status(500));
            let url = server.url("/broken.png");
            assert!(crate::url_image::load(&url).is_none());
            assert!(wait_for(|| (server.hits() == 1).then_some(())).is_some());
            // Failed by now: more asks do not send more requests.
            assert!(
                wait_for(|| {
                    with_store(|store| matches!(store.slots.get(&url), Some(Slot::Failed { .. })))
                        .then_some(())
                })
                .is_some()
            );
            let failed = crate::url_image::identity(&url);
            for _ in 0..5 {
                assert!(crate::url_image::load(&url).is_none());
            }
            assert_eq!(server.hits(), 1);
            assert_eq!(crate::url_image::identity(&url), failed);
        }

        #[test]
        fn what_is_not_an_image_never_becomes_one() {
            direct();
            let server = Server::start(|_| Reply::ok("text/html", b"<html>nope</html>".to_vec()));
            let url = server.url("/page.png");
            assert!(crate::url_image::load(&url).is_none());
            assert!(
                wait_for(|| {
                    with_store(|store| matches!(store.slots.get(&url), Some(Slot::Failed { .. })))
                        .then_some(())
                })
                .is_some()
            );
        }

        #[test]
        fn an_svg_over_the_network_loads_as_a_vector_with_its_own_size() {
            direct();
            let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect width="40" height="20" fill="#36f"/></svg>"##;
            let server = Server::start(move |_| Reply::ok("image/svg+xml", svg.to_vec()));
            let url = server.url("/logo.svg");
            assert!(crate::url_image::load(&url).is_none());
            let loaded = wait_for(|| crate::url_image::load(&url)).expect("the SVG loads");
            assert!(matches!(loaded.source, crate::url_image::Source::Vector(_)));
            assert_eq!(loaded.natural.width, Some(40.0));
            assert_eq!(loaded.natural.ratio, Some(2.0));
        }

        #[test]
        fn a_credential_in_the_address_fails_without_a_request() {
            direct();
            let url = "http://user:secret@127.0.0.1:9/a.png";
            assert!(crate::url_image::load(url).is_none());
            assert!(with_store(|store| matches!(
                store.slots.get(url),
                Some(Slot::Failed { .. })
            )));
        }
    }

    #[cfg(not(feature = "net"))]
    #[test]
    fn without_the_net_feature_an_address_fails_at_once() {
        let url = "https://example.com/no-net.png";
        assert!(crate::url_image::load(url).is_none());
        assert!(with_store(|store| matches!(
            store.slots.get(url),
            Some(Slot::Failed { .. })
        )));
    }
}
