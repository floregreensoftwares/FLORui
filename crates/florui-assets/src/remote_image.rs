//! The pictures a `url(...)` names at an `http://` or `https://` address.
//!
//! Layout and painting ask for an image on the thread that needs it, and a download
//! cannot be waited for there. So asking for one that is not here starts the download in
//! the background and answers "not available" (the layer draws nothing and a list marker
//! is the broken one), and when the picture is decoded, or the download failed, the
//! listener added with [`on_change`] is called so the host lays out and paints again. The
//! next ask finds it.
//!
//! Downloads go through [`crate::net::shared_fetcher`], so they share its queue, its
//! limits and its disk cache with every other picture that names a URL, and decoding goes
//! through the decode slots that every cache shares. The decoded pictures of `url(...)` are
//! kept in one [`AssetCache`] of this module (the process's), which has the memory budget of
//! any cache; the `<img>` of a window use the cache of that window's runtime, with a budget of
//! its own. What is held for the next frame is limited too: pictures that are
//! ready stay while they fit in [`MAX_HELD_BYTES`], least recently asked for going first,
//! and one that went comes back from the disk cache when it is asked for again. A failure
//! is remembered for [`RETRY_AFTER`] and then tried again by the next ask. A picture that
//! stays in use is asked for again after [`RECHECK_AFTER`], in the background and through the
//! HTTP cache, so an application that stays open does not show a picture the server has
//! replaced for ever: the old one is shown until a new one has arrived, and the window
//! then paints again. That needs the cache directory of [`crate::net::NetConfig`]: without
//! one there is nothing to ask the server "has it changed?" with, and every recheck would
//! download the whole picture again to find it had not, so a picture is not asked for again
//! while it stays in use.
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
use crate::{AssetCache, DecodeParams};

/// How long a picture that failed to load is left alone before the next ask tries again.
const RETRY_AFTER: Duration = Duration::from_secs(30);

/// How long a picture that is ready is shown before it is asked for again.
const RECHECK_AFTER: Duration = Duration::from_secs(10 * 60);

/// A picture asked for within this long is in use, and is not let go to make room.
const IN_USE: Duration = Duration::from_secs(10);

/// How many failures are kept before the ones that are due for a retry are let go, and how many
/// addresses have their failure remembered as said.
const MOST_FAILURES: usize = 512;
const MOST_REPORTED: usize = 4096;
const TRIM_EVERY: u32 = 64;

/// How long until the next recheck of a picture whose last recheck downloaded it in full.
const RECHECK_AFTER_UNCACHEABLE: Duration = Duration::from_secs(60 * 60);

/// How long until the next recheck when the last one was answered with a stale copy and the
/// fresh one was being fetched behind it.
const RECHECK_AFTER_STALE: Duration = Duration::from_secs(30);

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
        /// When it was last asked for, to tell the pictures in use from the ones left over.
        used_at: Instant,
        /// A digest of the encoded bytes it was made from, to tell a picture the server
        /// changed from one it only sent again.
        digest: u128,
        /// When it was loaded, or last found to be current.
        checked_at: Instant,
        /// How long after `checked_at` it is asked for again.
        recheck_after: Duration,
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
    /// The addresses whose failure has been said on standard error, so that a picture that
    /// stays broken is said once and not again at every retry.
    reported: std::collections::HashSet<String>,
    /// Failures since the failed slots were last counted.
    failed_since_trim: u32,
    /// What is to be said on standard error, said once the lock is let go: a blocked
    /// terminal must not hold up the threads that paint.
    said: Vec<(String, String)>,
}

impl Store {
    /// Says why `url` cannot be used, once until it has loaded again.
    fn report(&mut self, url: &str, reason: &str) {
        if self.reported.len() >= MOST_REPORTED {
            self.reported.clear();
        }
        if self.reported.insert(url.to_owned()) {
            self.said.push((url.to_owned(), reason.to_owned()));
        }
    }

    /// Puts a failure in the slot of `url`, and lets go of the failures that can be tried
    /// again anyway when there are many: a feed of thousands of dead addresses must not be
    /// kept for ever.
    fn fail(&mut self, url: &str) {
        let generation = self.generation();
        self.slots.insert(
            url.to_owned(),
            Slot::Failed {
                generation,
                at: Instant::now(),
            },
        );
        // Counted every so many failures and not at every one: it looks at every slot.
        self.failed_since_trim += 1;
        if self.failed_since_trim < TRIM_EVERY {
            return;
        }
        self.failed_since_trim = 0;
        let failures = self
            .slots
            .values()
            .filter(|slot| matches!(slot, Slot::Failed { .. }))
            .count();
        if failures > MOST_FAILURES {
            self.slots.retain(
                |_, slot| !matches!(slot, Slot::Failed { at, .. } if at.elapsed() >= RETRY_AFTER),
            );
        }
    }

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
                    // A picture asked for lately is on a screen, and going would only make
                    // it come back, another to go in its place and the window to paint
                    // forever: more than the budget are held while that many are in use.
                    Slot::Ready { used, used_at, .. }
                        if url != keep && used_at.elapsed() >= IN_USE =>
                    {
                        Some((*used, url.clone()))
                    }
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
/// Everyone who wants to hear of arrivals, by the number their subscription has.
static LISTENERS: Mutex<Vec<(u64, Listener)>> = Mutex::new(Vec::new());
static NEXT_LISTENER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn with_store<T>(work: impl FnOnce(&mut Store) -> T) -> T {
    let (result, said) = {
        let mut guard = STORE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let store = guard.get_or_insert_with(Store::default);
        let result = work(store);
        (result, std::mem::take(&mut store.said))
    };
    for (url, reason) in said {
        report(&url, &reason);
    }
    result
}

/// Whether `url` is one this module loads.
pub fn is_remote(url: &str) -> bool {
    // The fetcher ignores white space around an address, and so does this.
    let url = url
        .trim_start_matches(|c: char| c.is_ascii_whitespace())
        .as_bytes();
    let has = |scheme: &[u8]| {
        url.get(..scheme.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
    };
    has(b"http://") || has(b"https://")
}

/// What keeps a listener added with [`on_change`] listening: dropping it removes the listener.
#[must_use = "the listener is removed when this is dropped"]
pub struct Subscription(u64);

impl Drop for Subscription {
    fn drop(&mut self) {
        LISTENERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(id, _)| *id != self.0);
    }
}

/// Calls `listener` (from a thread of the fetcher) whenever a picture that was asked for
/// has loaded or failed to, so a host can lay out and paint again. Any number of listeners
/// can listen at once, each until its [`Subscription`] is dropped.
pub fn on_change(listener: impl Fn() + Send + Sync + 'static) -> Subscription {
    let id = NEXT_LISTENER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    LISTENERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push((id, Arc::new(listener)));
    Subscription(id)
}

fn notify() {
    // Called outside the lock: a listener may add or drop a subscription.
    let listeners: Vec<Listener> = LISTENERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .map(|(_, listener)| Arc::clone(listener))
        .collect();
    for listener in listeners {
        listener();
    }
}

/// Text that stands for the state `url` is in, for keys of things made from it.
pub(crate) fn identity(url: &str) -> String {
    // Asking for the identity of a picture counts as using it: a picture whose kept drawing
    // is found by its identity, without a load, must not look left alone (and be let go for
    // room) or go for ever without being asked for again. Cheaper than a call of its own,
    // in the one lock this takes.
    let rechecking = recheck_enabled();
    let (generation, request) = with_store(|store| {
        store.clock += 1;
        let clock = store.clock;
        let next = store.next_generation + 1;
        let request = match store.slots.get_mut(url) {
            Some(slot @ Slot::Ready { .. }) => mark_used(slot, clock, rechecking, next),
            _ => None,
        };
        if request.is_some() {
            store.next_generation = next;
        }
        (store.slots.get(url).map(Slot::generation), request)
    });
    if let Some(request) = request {
        begin(url, request);
    }
    match generation {
        Some(generation) => identity_at(url, generation),
        None => format!("{url}@remote-none"),
    }
}

/// What to do once the lock is let go: nothing may be started while it is held, because the
/// fetcher can answer before it returns, and the answer takes the lock.
struct Request {
    generation: u64,
    refresh: bool,
}

/// Starts the request `request` describes, and records what that came to.
fn begin(url: &str, request: Request) {
    let started = start(url, request.generation);
    with_store(|store| attach(store, url, request.generation, request.refresh, started));
}

/// Marks a ready picture as just used, and, if it is due to be asked for again, says so: the
/// request that is then to be made (`next` is the generation it will have).
fn mark_used(slot: &mut Slot, clock: u64, rechecking: bool, next: u64) -> Option<Request> {
    let Slot::Ready {
        used,
        used_at,
        checked_at,
        recheck_after,
        refresh,
        ..
    } = slot
    else {
        return None;
    };
    *used = clock;
    *used_at = Instant::now();
    if rechecking && refresh.is_none() && checked_at.elapsed() >= *recheck_after {
        *refresh = Some((next, None));
        return Some(Request {
            generation: next,
            refresh: true,
        });
    }
    None
}

/// The identity of `url` at `version`, which a picture carries (see `Loaded::version`).
pub(crate) fn identity_at(url: &str, version: u64) -> String {
    format!("{url}@remote{version}")
}

/// The picture, if it is here. Otherwise starts getting it (unless that is under way or
/// failed lately) and answers `None`. A picture that has been shown for a while is answered
/// at once and asked for again in the background.
pub(crate) fn load(url: &str) -> Option<Arc<Loaded>> {
    let rechecking = recheck_enabled();
    let (answer, request) = with_store(|store| {
        store.clock += 1;
        let clock = store.clock;
        let next = store.next_generation + 1;
        let mut request = None;
        let answer = match store.slots.get_mut(url) {
            Some(slot @ Slot::Ready { .. }) => {
                request = mark_used(slot, clock, rechecking, next);
                match slot {
                    Slot::Ready { loaded, .. } => Some(Arc::clone(loaded)),
                    _ => None,
                }
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
    if let Some(request) = request {
        begin(url, request);
    }
    answer
}

/// Whether a picture that stays in use is asked for again: see the module documentation.
#[cfg(feature = "net")]
fn recheck_enabled() -> bool {
    rechecks(crate::net::shared_fetcher().config())
}

#[cfg(not(feature = "net"))]
fn recheck_enabled() -> bool {
    false
}

#[cfg(feature = "net")]
fn rechecks(config: &crate::net::NetConfig) -> bool {
    config.cache_dir.is_some()
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
    let mut said = None;
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
            store.report(url, &reason);
            store.fail(url);
        }
        (
            Some(Slot::Ready {
                refresh: refreshing @ Some(_),
                checked_at,
                recheck_after,
                ..
            }),
            Started::Refused(reason),
        ) if refresh && refreshing.as_ref().is_some_and(|(g, _)| *g == generation) => {
            said = Some(reason);
            *refreshing = None;
            // Not again at once.
            *checked_at = Instant::now();
            *recheck_after = RECHECK_AFTER;
        }
        _ => {}
    }
    if let Some(reason) = said {
        store.report(url, &reason);
    }
}

#[cfg(not(feature = "net"))]
fn start(_url: &str, _generation: u64) -> Started {
    Started::Refused(crate::FetchError::Unavailable.to_string())
}

#[cfg(feature = "net")]
fn start(url: &str, generation: u64) -> Started {
    let owned = url.to_owned();
    // The callback runs on a thread of the fetcher, which is for downloads: decoding is
    // handed to threads of its own, so a burst of large pictures does not hold up the
    // downloads queued behind them.
    match crate::net::shared_fetcher().submit(url, move |outcome| {
        decoder::run(Box::new(move || finish(&owned, generation, outcome)));
    }) {
        Ok(ticket) => Started::Queued(ticket),
        Err(error) => Started::Refused(error.to_string()),
    }
}

fn report(url: &str, reason: &str) {
    // The address is someone else's text: control characters in it must not reach a terminal.
    let shown: String = url
        .chars()
        .take(80)
        .flat_map(char::escape_default)
        .collect();
    eprintln!("florui-assets: the image url({shown}) cannot be used: {reason}");
}

#[cfg(feature = "net")]
fn cache() -> &'static AssetCache {
    static CACHE: OnceLock<AssetCache> = OnceLock::new();
    CACHE.get_or_init(AssetCache::new)
}

/// A few threads that decode what the fetcher has downloaded, as many as there are decode
/// slots (see `AssetCache`), so none of them waits for a slot that others hold.
#[cfg(feature = "net")]
pub(crate) mod decoder {
    use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
    use std::sync::{Arc, Mutex, OnceLock};

    pub(crate) type Job = Box<dyn FnOnce() + Send>;

    pub(crate) fn run(job: Job) {
        // A few jobs wait, no more: each owns the body of a download, and when decoding falls
        // behind, the downloads wait for it (the thread that sends is a fetcher's) instead of
        // piling up what they fetched.
        static QUEUE: OnceLock<SyncSender<Job>> = OnceLock::new();
        let queue = QUEUE.get_or_init(|| {
            let (sender, receiver) = sync_channel::<Job>(16);
            let receiver: Arc<Mutex<Receiver<Job>>> = Arc::new(Mutex::new(receiver));
            let cores = std::thread::available_parallelism().map_or(2, usize::from);
            for _ in 0..(cores / 2).clamp(1, 4) {
                let receiver = Arc::clone(&receiver);
                let _ = std::thread::Builder::new()
                    .name("florui-decode".to_owned())
                    .spawn(move || {
                        loop {
                            let job = receiver
                                .lock()
                                .unwrap_or_else(|poison| poison.into_inner())
                                .recv();
                            let Ok(job) = job else { return };
                            // A job that panics must not end the thread; it records its own
                            // failure (see `finish`).
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                        }
                    });
            }
            sender
        });
        // With no thread to take it (none could be started, so the receiver is gone), the
        // job runs here, on the thread that downloaded it: slower, but it finishes.
        if let Err(std::sync::mpsc::SendError(job)) = queue.send(job) {
            job();
        }
    }
}

/// What a download became.
#[cfg(feature = "net")]
enum Built {
    /// A request for a newer copy that found the one already shown still current. `stale`:
    /// the copy sent was a stale one from the HTTP cache, with a fresh one on its way.
    /// `uncached`: it came from the server in full, the HTTP cache having nothing to confirm
    /// it with (the response is not storable, or has no validator).
    Unchanged {
        stale: bool,
        uncached: bool,
    },
    /// `stale`: it was loaded from a stale copy, with a fresh one being fetched behind it.
    Loaded(Loaded, u64, u128, bool),
    Failed(String),
}

/// The identity of the decode of one download of `url`: the address and the digest of its
/// bytes, so a picture the server replaced is a different entry from the old one (which may
/// still be alive, held by what is painting it) and neither can be handed back for the
/// other, whatever runs at the same time.
pub(crate) fn cache_key(url: &str, digest: u128) -> String {
    format!("{url}\u{0}{digest:032x}")
}

/// The key to decode the bytes of one download of `url` under: see [`cache_key`].
pub(crate) fn download_key(url: &str, bytes: &[u8]) -> String {
    cache_key(url, digest(bytes))
}

/// The digest of the latest download of each address that was decoded, so that a picture
/// still alive in a cache can be found again without downloading it.
static LATEST: Mutex<Option<HashMap<String, u128>>> = Mutex::new(None);

/// [`download_key`], remembering that these bytes are the latest of `url`.
pub(crate) fn register_download(url: &str, bytes: &[u8]) -> String {
    let sum = digest(bytes);
    let mut latest = LATEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let latest = latest.get_or_insert_with(HashMap::new);
    if latest.len() >= MOST_REPORTED {
        latest.clear();
    }
    latest.insert(url.to_owned(), sum);
    cache_key(url, sum)
}

/// The identity of the latest download of `url` that was registered.
pub(crate) fn latest_download(url: &str) -> Option<String> {
    let latest = LATEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    latest.as_ref()?.get(url).map(|sum| cache_key(url, *sum))
}

/// A digest of `bytes` that tells two downloads apart: the start of their SHA-256, which a
/// server cannot make two different pictures share. (Without the `net` feature nothing is
/// downloaded, and a quick hash will do.)
fn digest(bytes: &[u8]) -> u128 {
    #[cfg(feature = "net")]
    {
        use sha2::{Digest as _, Sha256};
        let hash = Sha256::digest(bytes);
        let mut start = [0u8; 16];
        start.copy_from_slice(&hash[..16]);
        u128::from_be_bytes(start)
    }
    #[cfg(not(feature = "net"))]
    {
        use std::hash::Hasher as _;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        hasher.write(bytes);
        u128::from(hasher.finish())
    }
}

/// A download ended: decode it here, on the fetcher's thread and in a decode slot, and
/// record what became of it.
#[cfg(feature = "net")]
fn finish(
    url: &str,
    generation: u64,
    outcome: Result<Arc<crate::net::Fetched>, crate::FetchError>,
) {
    // Whether anyone still waits for this, and the digest of the picture being shown when
    // it is a request for a newer copy of it. A picture that was dropped meanwhile (its
    // window closed, or it was evicted) is not decoded for nothing.
    let (waiting, shown) = with_store(|store| match store.slots.get(url) {
        Some(Slot::Ready {
            refresh: Some((g, _)),
            digest,
            ..
        }) if *g == generation => (true, Some(*digest)),
        Some(Slot::Pending { generation: g, .. }) if *g == generation => (true, None),
        _ => (false, None),
    });
    if !waiting {
        return;
    }
    // A decoder that panics on a hostile file must leave the picture failed, not pending for
    // ever.
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match outcome {
        Ok(fetched) => {
            let origin = fetched.origin;
            let sum = digest(&fetched.bytes);
            if shown == Some(sum) {
                // The same bytes again, from the server or from a copy on disk: nothing to
                // decode and nothing to paint again.
                return Built::Unchanged {
                    stale: fetched.origin == crate::net::Origin::Stale,
                    uncached: fetched.origin == crate::net::Origin::Network,
                };
            }
            let size = fetched.bytes.len() as u64;
            match crate::url_image::build(&fetched.bytes, None, false, |bytes| {
                cache().get_or_decode(&cache_key(url, sum), bytes, DecodeParams::Native)
            }) {
                Ok(loaded) => Built::Loaded(
                    loaded,
                    size,
                    sum,
                    matches!(
                        origin,
                        crate::net::Origin::Stale | crate::net::Origin::StaleOnError
                    ),
                ),
                Err(reason) => Built::Failed(reason),
            }
        }
        Err(error) => Built::Failed(error.to_string()),
    }))
    .unwrap_or_else(|_| Built::Failed("the decoder panicked".to_owned()));
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
        match &built {
            Built::Failed(reason) => store.report(url, reason),
            Built::Loaded(..) => {
                store.reported.remove(url);
            }
            Built::Unchanged { .. } => {}
        }
        match (was, built) {
            (Was::Pending | Was::Refreshed, Built::Loaded(loaded, size, sum, stale)) => {
                install_ready(store, url, loaded, size, sum, stale, new_generation, used);
                true
            }
            (Was::Pending, _) => {
                store.fail(url);
                true
            }
            (Was::Refreshed, built) => {
                // The copy shown stays, and is asked for again after another while: soon
                // when what came was a stale copy with a fresh one being fetched behind it.
                let (stale, uncached) = match built {
                    Built::Unchanged { stale, uncached } => (stale, uncached),
                    _ => (false, false),
                };
                if let Some(Slot::Ready {
                    refresh,
                    checked_at,
                    recheck_after,
                    ..
                }) = store.slots.get_mut(url)
                {
                    *refresh = None;
                    *checked_at = Instant::now();
                    *recheck_after = if stale {
                        RECHECK_AFTER_STALE
                    } else if uncached {
                        // The whole picture came again to find that it had not changed, so
                        // the HTTP cache is not helping with this one: not so often.
                        RECHECK_AFTER_UNCACHEABLE
                    } else {
                        RECHECK_AFTER
                    };
                }
                false
            }
            (Was::Gone, _) => false,
        }
    });
    // Each download of an address is an entry of its own in the cache, and the ones nobody
    // holds any more are only dead weight there: swept every so often.
    static FINISHED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if FINISHED
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        .is_multiple_of(32)
    {
        cache().compact();
    }
    if changed {
        notify();
    }
}

/// Puts a loaded picture in the slot of `url`, in place of whatever was there (a request for
/// it, or the older picture it replaces), and lets go of pictures to make room.
#[cfg(feature = "net")]
#[allow(clippy::too_many_arguments)]
fn install_ready(
    store: &mut Store,
    url: &str,
    mut loaded: Loaded,
    encoded: u64,
    digest: u128,
    stale: bool,
    generation: u64,
    used: u64,
) {
    let bytes = retained(&loaded, encoded);
    if let Some(Slot::Ready { bytes: old, .. }) = store.slots.remove(url) {
        store.held -= old;
    }
    store.held += bytes;
    loaded.version = generation;
    store.slots.insert(
        url.to_owned(),
        Slot::Ready {
            generation,
            loaded: Arc::new(loaded),
            bytes,
            used,
            used_at: Instant::now(),
            digest,
            checked_at: Instant::now(),
            // Loaded from a stale copy, with a fresh one being fetched behind it: soon again.
            recheck_after: if stale {
                RECHECK_AFTER_STALE
            } else {
                RECHECK_AFTER
            },
            refresh: None,
        },
    );
    store.evict(url);
}

/// What the picture holds on to once loaded.
#[cfg(feature = "net")]
fn retained(loaded: &Loaded, encoded: u64) -> u64 {
    match &loaded.source {
        crate::url_image::Source::Raster(image) => image.retained_bytes(),
        // The parsed tree and what is drawn from it are many times the document.
        crate::url_image::Source::Vector(_) => encoded.saturating_mul(8),
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
                version: 0,
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
            // Left alone long enough to be let go.
            used_at: Instant::now() - IN_USE - Duration::from_secs(1),
            digest: 0,
            checked_at: Instant::now(),
            recheck_after: RECHECK_AFTER,
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
        // A picture in use is not let go for room, even over the budget.
        if let Some(Slot::Ready { used_at, .. }) = store.slots.get_mut("b") {
            *used_at = Instant::now();
        }
        store.held = MAX_HELD_BYTES * 2;
        store.evict("c");
        assert!(
            store.slots.contains_key("b"),
            "asked for lately, so on a screen"
        );
        if let Some(Slot::Ready { used_at, .. }) = store.slots.get_mut("b") {
            *used_at = Instant::now() - IN_USE - Duration::from_secs(1);
        }
        store.held = MAX_HELD_BYTES;
        // Over budget with only the kept picture left: it stays.
        store.held = MAX_HELD_BYTES * 2;
        store.evict("c");
        assert!(store.slots.contains_key("c") && !store.slots.contains_key("b"));
    }

    #[test]
    fn every_listener_hears_and_a_dropped_subscription_stops_hearing() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (first, second) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (a, b) = (Arc::clone(&first), Arc::clone(&second));
        let one = on_change(move || {
            a.fetch_add(1, Ordering::SeqCst);
        });
        let _two = on_change(move || {
            b.fetch_add(1, Ordering::SeqCst);
        });
        notify();
        assert!(first.load(Ordering::SeqCst) >= 1 && second.load(Ordering::SeqCst) >= 1);
        drop(one);
        let before = (first.load(Ordering::SeqCst), second.load(Ordering::SeqCst));
        notify();
        assert_eq!(
            first.load(Ordering::SeqCst),
            before.0,
            "dropped, it hears no more"
        );
        assert!(
            second.load(Ordering::SeqCst) > before.1,
            "the other still does"
        );
    }

    #[test]
    fn a_failure_is_said_once_and_the_failures_kept_are_limited() {
        let mut store = Store::default();
        // Said once however many times it fails.
        store.report("http://example.invalid/a.png", "down");
        assert!(store.reported.contains("http://example.invalid/a.png"));
        let said = store.reported.len();
        store.report("http://example.invalid/a.png", "down again");
        assert_eq!(store.reported.len(), said);
        // Thousands of dead addresses: the ones that can be retried anyway are let go.
        for n in 0..(MOST_FAILURES + 2 * TRIM_EVERY as usize) {
            store.fail(&format!("http://example.invalid/{n}.png"));
            // Old enough to be due for a retry.
            if let Some(Slot::Failed { at, .. }) = store
                .slots
                .get_mut(&format!("http://example.invalid/{n}.png"))
            {
                *at = Instant::now() - RETRY_AFTER - Duration::from_secs(1);
            }
        }
        assert!(
            store.slots.len() <= MOST_FAILURES + 1,
            "{} kept",
            store.slots.len()
        );
    }

    #[cfg(feature = "net")]
    #[test]
    fn two_downloads_of_one_address_with_different_bytes_do_not_share_a_decode() {
        assert_ne!(
            cache_key("http://example.invalid/a.png", 1),
            cache_key("http://example.invalid/a.png", 2)
        );
        assert_eq!(
            cache_key("http://example.invalid/a.png", 1),
            cache_key("http://example.invalid/a.png", 1)
        );
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
            // A cache directory is what makes a recheck possible; nothing the tests serve is
            // allowed to be stored in it (no validators, no lifetime), so every fetch still
            // goes to the server.
            crate::net::configure(NetConfig {
                proxy: ProxyMode::Direct,
                cache_dir: Some(std::env::temp_dir().join("florui-assets-remote-image-tests")),
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
            let _subscription = crate::url_image::on_remote_change(move || {
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
                if let Some(Slot::Ready { recheck_after, .. }) = store.slots.get_mut(url) {
                    *recheck_after = Duration::ZERO;
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
            // What is made from a picture is keyed by that picture, not by whatever the
            // address is at when the key is read: the old one keeps its own key.
            assert_eq!(crate::url_image::identity_of(&url, &first), before);
            assert_eq!(
                crate::url_image::identity_of(&url, &replaced),
                crate::url_image::identity(&url)
            );
            // Once, not on every ask.
            for _ in 0..5 {
                crate::url_image::load(&url);
            }
            assert_eq!(server.hits(), 2);
        }

        #[test]
        fn a_recheck_answered_with_a_stale_copy_is_repeated_soon_not_after_the_whole_wait() {
            direct();
            let url = "http://example.invalid/stale.png";
            let bytes = png_of(2, 2);
            let sum = digest(&bytes);
            let loaded = crate::url_image::build(&bytes, None, false, |b| {
                cache().get_or_decode(&cache_key(url, sum), b, DecodeParams::Native)
            })
            .unwrap();
            with_store(|store| {
                store.slots.insert(
                    url.to_owned(),
                    Slot::Ready {
                        generation: 1,
                        loaded: Arc::new(loaded),
                        bytes: 1,
                        used: 1,
                        used_at: Instant::now(),
                        digest: sum,
                        checked_at: Instant::now(),
                        recheck_after: Duration::ZERO,
                        refresh: Some((9, None)),
                    },
                );
            });
            let sent_again = |origin| {
                Arc::new(crate::net::Fetched {
                    bytes: bytes.clone(),
                    content_type: Some("image/png".to_owned()),
                    origin,
                })
            };
            finish(url, 9, Ok(sent_again(crate::net::Origin::Stale)));
            let wait = |url: &str| {
                with_store(|store| match store.slots.get(url) {
                    Some(Slot::Ready {
                        recheck_after,
                        refresh: None,
                        ..
                    }) => Some(*recheck_after),
                    _ => None,
                })
            };
            assert_eq!(wait(url), Some(RECHECK_AFTER_STALE));
            // The same bytes from a copy that is current wait the whole time.
            with_store(|store| {
                if let Some(Slot::Ready { refresh, .. }) = store.slots.get_mut(url) {
                    *refresh = Some((10, None));
                }
            });
            finish(url, 10, Ok(sent_again(crate::net::Origin::Cache)));
            assert_eq!(wait(url), Some(RECHECK_AFTER));
        }

        #[test]
        fn a_picture_first_loaded_from_a_stale_copy_is_checked_again_soon() {
            direct();
            let url = "http://example.invalid/first-stale.png";
            let bytes = png_of(2, 2);
            with_store(|store| {
                store.slots.insert(
                    url.to_owned(),
                    Slot::Pending {
                        generation: 3,
                        ticket: None,
                    },
                );
            });
            finish(
                url,
                3,
                Ok(Arc::new(crate::net::Fetched {
                    bytes,
                    content_type: Some("image/png".to_owned()),
                    origin: crate::net::Origin::Stale,
                })),
            );
            let wait = with_store(|store| match store.slots.get(url) {
                Some(Slot::Ready { recheck_after, .. }) => Some(*recheck_after),
                _ => None,
            });
            assert_eq!(wait, Some(RECHECK_AFTER_STALE));
        }

        #[test]
        fn a_picture_nobody_waits_for_any_more_is_not_decoded() {
            direct();
            let url = "http://example.invalid/dropped.png";
            // No slot for it: it was evicted or its window closed while it downloaded.
            finish(
                url,
                1,
                Ok(Arc::new(crate::net::Fetched {
                    bytes: b"not even an image".to_vec(),
                    content_type: Some("image/png".to_owned()),
                    origin: crate::net::Origin::Network,
                })),
            );
            assert!(with_store(|store| !store.slots.contains_key(url)));
        }

        #[test]
        fn the_decoder_threads_survive_a_job_that_panics() {
            let (sender, receiver) = std::sync::mpsc::channel();
            decoder::run(Box::new(|| panic!("a decoder gave up")));
            for n in 0..4 {
                let sender = sender.clone();
                decoder::run(Box::new(move || sender.send(n).unwrap()));
            }
            let mut got: Vec<_> = (0..4)
                .map(|_| receiver.recv_timeout(Duration::from_secs(10)).unwrap())
                .collect();
            got.sort();
            assert_eq!(got, [0, 1, 2, 3]);
        }

        #[test]
        fn a_picture_is_asked_for_again_only_when_there_is_a_cache_to_ask_with() {
            assert!(!rechecks(&NetConfig::default()));
            assert!(rechecks(&NetConfig {
                cache_dir: Some(std::path::PathBuf::from("cache")),
                ..NetConfig::default()
            }));
        }

        #[test]
        fn a_recheck_that_downloaded_the_whole_picture_to_find_it_unchanged_waits_longer() {
            direct();
            let url = "http://example.invalid/uncacheable.png";
            let bytes = png_of(2, 2);
            let sum = digest(&bytes);
            let loaded = crate::url_image::build(&bytes, None, false, |b| {
                cache().get_or_decode(&cache_key(url, sum), b, DecodeParams::Native)
            })
            .unwrap();
            with_store(|store| {
                store.slots.insert(
                    url.to_owned(),
                    Slot::Ready {
                        generation: 1,
                        loaded: Arc::new(loaded),
                        bytes: 1,
                        used: 1,
                        used_at: Instant::now(),
                        digest: sum,
                        checked_at: Instant::now(),
                        recheck_after: Duration::ZERO,
                        refresh: Some((5, None)),
                    },
                );
            });
            finish(
                url,
                5,
                Ok(Arc::new(crate::net::Fetched {
                    bytes,
                    content_type: Some("image/png".to_owned()),
                    origin: crate::net::Origin::Network,
                })),
            );
            let wait = with_store(|store| match store.slots.get(url) {
                Some(Slot::Ready { recheck_after, .. }) => Some(*recheck_after),
                _ => None,
            });
            assert_eq!(wait, Some(RECHECK_AFTER_UNCACHEABLE));
        }

        #[test]
        fn one_picture_written_two_ways_is_one_picture() {
            use crate::url_image::remote_address;
            let same = [
                "https://example.com/a.png",
                "  HTTPS://Example.COM:443/a.png#frag ",
                "https://example.com/./b/../a.png",
            ];
            for url in same {
                assert_eq!(remote_address(url), "https://example.com/a.png", "{url}");
            }
            // What is different stays different.
            assert_ne!(
                remote_address("https://example.com/a.png?v=2"),
                remote_address("https://example.com/a.png")
            );
            assert_ne!(
                remote_address("https://example.com:8443/a.png"),
                remote_address("https://example.com/a.png")
            );
        }

        #[test]
        fn asking_for_the_identity_of_a_remote_picture_marks_it_in_use_and_of_anything_else_does_nothing()
         {
            direct();
            let url = "http://example.invalid/touched.png";
            let bytes = png_of(2, 2);
            let sum = digest(&bytes);
            let loaded = crate::url_image::build(&bytes, None, false, |b| {
                cache().get_or_decode(&cache_key(url, sum), b, DecodeParams::Native)
            })
            .unwrap();
            with_store(|store| {
                store.slots.insert(
                    url.to_owned(),
                    Slot::Ready {
                        generation: 1,
                        loaded: Arc::new(loaded),
                        bytes: 1,
                        used: 1,
                        used_at: Instant::now() - IN_USE - Duration::from_secs(5),
                        digest: sum,
                        checked_at: Instant::now(),
                        recheck_after: RECHECK_AFTER,
                        refresh: None,
                    },
                );
            });
            crate::url_image::identity(url);
            let in_use = with_store(|store| match store.slots.get(url) {
                Some(Slot::Ready { used_at, .. }) => used_at.elapsed() < IN_USE,
                _ => false,
            });
            assert!(in_use, "asked for lately");
            let before = with_store(|store| store.slots.len());
            crate::url_image::identity("assets/a.png");
            crate::url_image::identity("data:image/png;base64,AAAA");
            assert_eq!(with_store(|store| store.slots.len()), before);
        }

        #[test]
        fn a_recheck_that_gets_the_same_bytes_again_changes_nothing() {
            direct();
            let server = Server::start(|_| Reply::ok("image/png", png_of(3, 2)));
            let url = server.url("/same.png");
            assert!(crate::url_image::load(&url).is_none());
            let shown = wait_for(|| crate::url_image::load(&url)).expect("it loads");
            let identity = crate::url_image::identity(&url);
            age(&url);
            crate::url_image::load(&url);
            assert!(wait_for(|| (server.hits() == 2).then_some(())).is_some());
            // The recheck ended: the slot no longer waits for it.
            assert!(
                wait_for(|| {
                    with_store(|store| {
                        matches!(
                            store.slots.get(&url),
                            Some(Slot::Ready { refresh: None, .. })
                        )
                    })
                    .then_some(())
                })
                .is_some()
            );
            let after = crate::url_image::load(&url).unwrap();
            assert!(Arc::ptr_eq(&shown, &after), "not decoded again");
            assert_eq!(
                crate::url_image::identity(&url),
                identity,
                "nothing to paint again"
            );
        }

        #[test]
        fn the_same_picture_written_with_white_space_around_it_is_one_picture() {
            direct();
            let server = Server::start(|_| Reply::ok("image/png", png_of(2, 2)));
            let url = server.url("/spaced.png");
            let padded = format!(" \n{url}\t ");
            assert!(crate::url_image::load(&padded).is_none());
            let loaded = wait_for(|| crate::url_image::load(&padded)).expect("it loads");
            let plain = crate::url_image::load(&url).expect("the same picture");
            assert!(Arc::ptr_eq(&loaded, &plain));
            assert_eq!(server.hits(), 1);
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
                            Some(Slot::Ready { refresh: None, recheck_after, .. })
                                if *recheck_after == RECHECK_AFTER
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

    #[test]
    fn the_latest_registered_download_of_an_address_is_the_one_found_again() {
        let url = "http://example.invalid/latest-test.png";
        assert_eq!(latest_download(url), None);
        let first = register_download(url, b"first");
        assert_eq!(latest_download(url), Some(first.clone()));
        let second = register_download(url, b"second");
        assert_ne!(first, second, "different bytes, different identity");
        assert_eq!(latest_download(url), Some(second));
        // The same bytes again are the same identity.
        assert_eq!(register_download(url, b"first"), first);
    }
}
