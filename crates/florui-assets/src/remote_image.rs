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
//! is remembered for [`RETRY_AFTER`] and then tried again by the next ask. A picture that
//! stays in use is asked for again after [`RECHECK_AFTER`], in the background and through the
//! HTTP cache, so an application that stays open does not show a picture the server has
//! replaced for ever: the old one is shown until a new one has arrived, and the window
//! then paints again.
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

/// How long a picture that is ready is shown before it is asked for again.
const RECHECK_AFTER: Duration = Duration::from_secs(10 * 60);

/// What pictures that are ready may hold altogether, in bytes, for the next frame.
const MAX_HELD_BYTES: u64 = 256 * 1024 * 1024;

#[cfg(feature = "net")]
type Ticket = crate::net::FetchTicket;
#[cfg(not(feature = "net"))]
type Ticket = ();

enum Slot {
    /// The download or the decode is under way; dropping this cancels it. `ticket` is
    /// `None` for the moment between the slot being made and the request being queued.
    Pending {
        generation: u64,
        ticket: Option<Ticket>,
    },
    Ready {
        generation: u64,
        loaded: Arc<Loaded>,
        bytes: u64,
        used: u64,
        /// When it was loaded, or last found to be current.
        checked_at: Instant,
        /// A request for a newer copy under way: its generation and, once queued, its
        /// ticket.
        refresh: Option<(u64, Option<Ticket>)>,
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
    // The fetcher ignores white space around an address, and so does this.
    let url = url.trim_start();
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
/// failed lately) and answers `None`. A picture that has been shown for a while is answered
/// at once and asked for again in the background.
pub(crate) fn load(url: &str) -> Option<Arc<Loaded>> {
    /// What to do once the lock is let go: nothing may be started while it is held, because
    /// the fetcher can answer before it returns, and the answer takes the lock.
    struct Request {
        generation: u64,
        refresh: bool,
    }
    let (answer, request) = with_store(|store| {
        store.clock += 1;
        let clock = store.clock;
        let next = store.next_generation + 1;
        let mut request = None;
        let answer = match store.slots.get_mut(url) {
            Some(Slot::Ready {
                loaded,
                used,
                checked_at,
                refresh,
                ..
            }) => {
                *used = clock;
                if refresh.is_none() && checked_at.elapsed() >= RECHECK_AFTER {
                    *refresh = Some((next, None));
                    request = Some(Request {
                        generation: next,
                        refresh: true,
                    });
                }
                Some(Arc::clone(loaded))
            }
            Some(Slot::Pending { .. }) => None,
            Some(Slot::Failed { at, .. }) if at.elapsed() < RETRY_AFTER => None,
            _ => {
                store.slots.insert(
                    url.to_owned(),
                    Slot::Pending {
                        generation: next,
                        ticket: None,
                    },
                );
                request = Some(Request {
                    generation: next,
                    refresh: false,
                });
                None
            }
        };
        if request.is_some() {
            store.next_generation = next;
        }
        (answer, request)
    });
    if let Some(Request {
        generation,
        refresh,
    }) = request
    {
        let started = start(url, generation);
        with_store(|store| attach(store, url, generation, refresh, started));
    }
    answer
}

/// What starting a request came to.
enum Started {
    Queued(Ticket),
    /// It could not be started, and why.
    Refused(String),
}

/// Records what starting the request of `generation` came to, unless that request has
/// ended or been dropped in the meantime (the fetcher can answer before `start` returns),
/// in which case the ticket is let go.
fn attach(store: &mut Store, url: &str, generation: u64, refresh: bool, started: Started) {
    match (store.slots.get_mut(url), started) {
        (
            Some(Slot::Pending {
                generation: g,
                ticket,
            }),
            Started::Queued(queued),
        ) if !refresh && *g == generation => *ticket = Some(queued),
        (
            Some(Slot::Ready {
                refresh: Some((g, ticket)),
                ..
            }),
            Started::Queued(queued),
        ) if refresh && *g == generation => *ticket = Some(queued),
        (Some(slot @ Slot::Pending { .. }), Started::Refused(reason))
            if !refresh && slot.generation() == generation =>
        {
            report(url, &reason);
            let generation = store.generation();
            store.slots.insert(
                url.to_owned(),
                Slot::Failed {
                    generation,
                    at: Instant::now(),
                },
            );
        }
        (
            Some(Slot::Ready {
                refresh: refreshing @ Some(_),
                checked_at,
                ..
            }),
            Started::Refused(reason),
        ) if refresh && refreshing.as_ref().is_some_and(|(g, _)| *g == generation) => {
            report(url, &reason);
            *refreshing = None;
            // Not again at once.
            *checked_at = Instant::now();
        }
        _ => {}
    }
}

#[cfg(not(feature = "net"))]
fn start(_url: &str, _generation: u64) -> Started {
    Started::Refused(crate::FetchError::Unavailable.to_string())
}

#[cfg(feature = "net")]
fn start(url: &str, generation: u64) -> Started {
    let owned = url.to_owned();
    match crate::net::shared_fetcher()
        .submit(url, move |outcome| finish(&owned, generation, outcome))
    {
        Ok(ticket) => Started::Queued(ticket),
        Err(error) => Started::Refused(error.to_string()),
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

/// What a download became.
#[cfg(feature = "net")]
enum Built {
    /// A request for a newer copy that found the one already shown still current.
    Unchanged,
    Loaded(Loaded, u64),
    Failed(String),
}

/// A download ended: decode it here, on the fetcher's thread and in a decode slot, and
/// record what became of it.
#[cfg(feature = "net")]
fn finish(
    url: &str,
    generation: u64,
    outcome: Result<Arc<crate::net::Fetched>, crate::FetchError>,
) {
    let refreshing = with_store(|store| {
        matches!(
            store.slots.get(url),
            Some(Slot::Ready { refresh: Some((g, _)), .. }) if *g == generation
        )
    });
    // A decoder that panics on a hostile file must leave the picture failed, not pending for
    // ever.
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match outcome {
        // The copy kept is the one that was sent again: nothing to decode.
        Ok(fetched) if refreshing && fetched.origin != crate::net::Origin::Network => {
            Built::Unchanged
        }
        Ok(fetched) => {
            let size = fetched.bytes.len() as u64;
            if refreshing {
                // The decode of the old bytes may still be alive, held by the picture
                // being shown, and would be handed back for the new ones.
                cache().invalidate(&AssetId::Url(url.to_owned()));
            }
            match crate::url_image::build(&fetched.bytes, None, |bytes| {
                cache().get_or_decode(AssetId::Url(url.to_owned()), bytes, DecodeParams::Native)
            }) {
                Ok(loaded) => Built::Loaded(loaded, size),
                Err(reason) => Built::Failed(reason),
            }
        }
        Err(error) => Built::Failed(error.to_string()),
    }))
    .unwrap_or_else(|_| Built::Failed("the decoder panicked".to_owned()));
    if let Built::Failed(reason) = &built {
        report(url, reason);
    }
    let changed = with_store(|store| {
        enum Was {
            /// Nobody asks any more (it was dropped from the store, and the download with it).
            Gone,
            Pending,
            Refreshed,
        }
        let was = match store.slots.get(url) {
            Some(Slot::Pending { generation: g, .. }) if *g == generation => Was::Pending,
            Some(Slot::Ready {
                refresh: Some((g, _)),
                ..
            }) if *g == generation => Was::Refreshed,
            _ => Was::Gone,
        };
        if matches!(was, Was::Gone) {
            return false;
        }
        let new_generation = store.generation();
        store.clock += 1;
        let used = store.clock;
        match (was, built) {
            (Was::Pending, Built::Loaded(loaded, size)) => {
                let bytes = retained(&loaded, size);
                store.held += bytes;
                store.slots.insert(
                    url.to_owned(),
                    Slot::Ready {
                        generation: new_generation,
                        loaded: Arc::new(loaded),
                        bytes,
                        used,
                        checked_at: Instant::now(),
                        refresh: None,
                    },
                );
                store.evict(url);
                true
            }
            (Was::Pending, _) => {
                store.slots.insert(
                    url.to_owned(),
                    Slot::Failed {
                        generation: new_generation,
                        at: Instant::now(),
                    },
                );
                true
            }
            (Was::Refreshed, Built::Loaded(loaded, size)) => {
                let bytes = retained(&loaded, size);
                if let Some(Slot::Ready { bytes: old, .. }) = store.slots.remove(url) {
                    store.held -= old;
                }
                store.held += bytes;
                store.slots.insert(
                    url.to_owned(),
                    Slot::Ready {
                        generation: new_generation,
                        loaded: Arc::new(loaded),
                        bytes,
                        used,
                        checked_at: Instant::now(),
                        refresh: None,
                    },
                );
                store.evict(url);
                true
            }
            (Was::Refreshed, _) => {
                // The copy shown stays, and is asked for again after another while.
                if let Some(Slot::Ready {
                    refresh,
                    checked_at,
                    ..
                }) = store.slots.get_mut(url)
                {
                    *refresh = None;
                    *checked_at = Instant::now();
                }
                false
            }
            (Was::Gone, _) => false,
        }
    });
    if changed {
        notify();
    }
}

/// What the picture holds on to once loaded.
#[cfg(feature = "net")]
fn retained(loaded: &Loaded, encoded: u64) -> u64 {
    match &loaded.source {
        crate::url_image::Source::Raster(image) => image.retained_bytes(),
        crate::url_image::Source::Vector(_) => encoded,
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
        assert!(is_remote("  \nhttps://example.com/a.png"));
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
            checked_at: Instant::now(),
            refresh: None,
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

        fn png_of(width: u32, height: u32) -> Vec<u8> {
            let image = image::RgbaImage::from_pixel(width, height, image::Rgba([1, 2, 3, 255]));
            let mut bytes = Vec::new();
            image::DynamicImage::ImageRgba8(image)
                .write_to(
                    &mut std::io::Cursor::new(&mut bytes),
                    image::ImageFormat::Png,
                )
                .unwrap();
            bytes
        }

        /// Makes the picture look as if it had been shown for longer than the recheck time.
        fn age(url: &str) {
            with_store(|store| {
                if let Some(Slot::Ready { checked_at, .. }) = store.slots.get_mut(url) {
                    *checked_at = Instant::now() - RECHECK_AFTER - Duration::from_secs(1);
                }
            });
        }

        #[test]
        fn a_picture_that_stays_in_use_is_asked_for_again_and_replaced_when_the_server_changed_it()
        {
            direct();
            let hits = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&hits);
            let server = Server::start(move |_| {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                let (width, height) = if n == 0 { (3, 2) } else { (5, 5) };
                Reply::ok("image/png", png_of(width, height))
            });
            let url = server.url("/avatar.png");
            assert!(crate::url_image::load(&url).is_none());
            let first = wait_for(|| crate::url_image::load(&url)).expect("it loads");
            assert_eq!(first.intrinsic, (3.0, 2.0));
            let before = crate::url_image::identity(&url);

            age(&url);
            // The old picture is still answered at once, and the new one is on its way.
            let shown = crate::url_image::load(&url).expect("still shown");
            assert_eq!(shown.intrinsic, (3.0, 2.0));
            let replaced = wait_for(|| {
                crate::url_image::load(&url).filter(|loaded| loaded.intrinsic == (5.0, 5.0))
            })
            .expect("the new picture replaces the old one");
            assert_eq!(replaced.intrinsic, (5.0, 5.0));
            assert_ne!(crate::url_image::identity(&url), before);
            // Once, not on every ask.
            for _ in 0..5 {
                crate::url_image::load(&url);
            }
            assert_eq!(server.hits(), 2);
        }

        #[test]
        fn a_recheck_that_fails_keeps_the_picture_and_waits_before_asking_again() {
            direct();
            let hits = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&hits);
            let server = Server::start(move |_| {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    Reply::ok("image/png", png_of(3, 2))
                } else {
                    Reply::status(500)
                }
            });
            let url = server.url("/avatar.png");
            assert!(crate::url_image::load(&url).is_none());
            wait_for(|| crate::url_image::load(&url)).expect("it loads");
            age(&url);
            crate::url_image::load(&url);
            assert!(wait_for(|| (server.hits() == 2).then_some(())).is_some());
            // The failure is recorded: the slot is not refreshing, and not due again.
            assert!(
                wait_for(|| {
                    with_store(|store| {
                        matches!(
                            store.slots.get(&url),
                            Some(Slot::Ready { refresh: None, checked_at, .. })
                                if checked_at.elapsed() < RECHECK_AFTER
                        )
                    })
                    .then_some(())
                })
                .is_some()
            );
            let still = crate::url_image::load(&url).expect("the old picture stays");
            assert_eq!(still.intrinsic, (3.0, 2.0));
            assert_eq!(server.hits(), 2);
        }

        #[test]
        fn an_answer_that_comes_before_the_request_is_recorded_is_not_overwritten() {
            direct();
            // What `load` does when the fetcher answers before `start` returns: the slot is
            // made, the answer arrives, and only then the ticket would be attached.
            let url = "http://example.invalid/early.png";
            with_store(|store| {
                store.slots.insert(
                    url.to_owned(),
                    Slot::Pending {
                        generation: 7,
                        ticket: None,
                    },
                );
            });
            let fetched = Arc::new(crate::net::Fetched {
                bytes: png_of(2, 2),
                content_type: Some("image/png".to_owned()),
                origin: crate::net::Origin::Network,
            });
            finish(url, 7, Ok(fetched));
            assert!(matches!(
                with_store(|store| store
                    .slots
                    .get(url)
                    .map(|s| matches!(s, Slot::Ready { .. }))),
                Some(true)
            ));
            // The late word that it could not be started changes nothing.
            with_store(|store| attach(store, url, 7, false, Started::Refused("late".to_owned())));
            assert!(with_store(|store| matches!(
                store.slots.get(url),
                Some(Slot::Ready { .. })
            )));
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
