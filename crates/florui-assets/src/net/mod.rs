//! Loading the bytes of an `http://` or `https://` image, behind the `net` feature.
//!
//! This is a loading contract of its own, not a third kind of path. What it promises:
//!
//! - **Requests.** `GET` only, with a fixed `Accept` and `User-Agent`, no cookies and no
//!   credentials (a URL with a user name or password is refused). A connection, the wait
//!   for a response and the stall between two reads each have a timeout, and the whole
//!   request has another ([`NetConfig`]). There is no automatic retry: a failure is
//!   reported once, and asking again is the caller's decision.
//! - **Redirects** (301, 302, 303, 307, 308) are followed by hand, up to a limit, only to
//!   `http` and `https`, and never from `https` to `http`.
//! - **Size.** The declared `Content-Length` is checked before the body is read, and the
//!   body is counted while it streams in, so a server that lies about its length or sends
//!   no length is cut off at [`NetConfig::max_bytes`] (by default the
//!   [`crate::limits::MAX_SOURCE_BYTES`] of every other source).
//! - **Content.** A response whose `Content-Type` is not an image type is refused, and one
//!   that names a format the bytes are not is refused too, so an error page served as `200`
//!   never reaches a decoder. A missing or generic (`application/octet-stream`) type is
//!   judged by the bytes alone.
//! - **Cache.** With [`NetConfig::cache_dir`], responses are kept on disk under
//!   `Cache-Control`, `Expires`, `Age`, `ETag` and `Last-Modified` (see the `http_cache`
//!   module), within a byte budget. A stale response is asked again with a conditional
//!   request. The memory a decoded picture takes is budgeted where it is decoded, by
//!   [`crate::AssetCache`]; this module holds no decoded pixels.
//! - **Concurrency.** At most [`NetConfig::max_concurrent`] requests run at once, and at most
//!   [`NetConfig::max_per_host`] to one host; the rest wait in a queue, in order. Asking for
//!   a URL that is already queued or running joins that request instead of starting
//!   another. A [`FetchTicket`] cancels its request when it is dropped: a request nobody
//!   waits for any more leaves the queue, or, if it is running, stops at the next chunk it
//!   reads.
//!
//! ## Choice of client
//!
//! `ureq` 3 with `rustls`: it is blocking, as the rest of this crate is, so no executor or
//! runtime comes with it; it is small next to `reqwest` and `hyper` (no `tokio`); it reads
//! `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` and `NO_PROXY` from the environment
//! ([`ProxyMode::FromEnvironment`], the default); and with the platform verifier the
//! certificates are checked against the system store (the Windows and macOS stores, or the
//! system roots on Linux), so a company's own root certificate works as it does in the
//! browser. The one part that is not Rust is the cryptography of `rustls` (`ring`, which
//! needs a C compiler to build): there is no mature pure Rust provider. Settings that live
//! only in the operating system (the Windows proxy configuration, a PAC file) are not read.

mod http_cache;
#[cfg(test)]
pub(crate) mod test_server;

use std::collections::{HashMap, VecDeque};
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use url::Url;

use crate::FetchError;
use crate::limits::MAX_SOURCE_BYTES;
use crate::url_image::{self, Format};
use http_cache::{DiskCache, Entry, Headers, Policy, unix_now};

const ACCEPT: &str = "image/webp,image/png,image/jpeg,image/gif,image/svg+xml,*/*;q=0.5";

/// Where the proxy settings come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyMode {
    /// `ALL_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY` and `NO_PROXY` (and their lower-case
    /// spellings).
    FromEnvironment,
    /// Connect directly, whatever the environment says.
    Direct,
    /// This proxy (`http://host:port`, `socks5://host:port`...) for every request.
    Url(String),
}

/// The limits and places of a [`Fetcher`].
#[derive(Debug, Clone)]
pub struct NetConfig {
    /// Establishing the connection, the TLS handshake included. 10 s.
    pub connect_timeout: Duration,
    /// Waiting for the response headers, and between two reads of the body. 15 s.
    pub stall_timeout: Duration,
    /// The whole request, redirects and the body included. 60 s.
    pub total_timeout: Duration,
    /// Redirects followed before giving up. 5.
    pub max_redirects: u32,
    /// The largest body accepted. [`MAX_SOURCE_BYTES`].
    pub max_bytes: u64,
    /// Requests running at once. 8.
    pub max_concurrent: usize,
    /// Requests running at once to one host. 6, the limit browsers use.
    pub max_per_host: usize,
    /// Where responses are kept between runs; `None` keeps nothing. The directory is the
    /// crate's own: it is created if missing and holds only files it wrote.
    pub cache_dir: Option<PathBuf>,
    /// What the cache directory may hold, in bytes. 256 MiB.
    pub cache_max_bytes: u64,
    pub proxy: ProxyMode,
    pub user_agent: String,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            stall_timeout: Duration::from_secs(15),
            total_timeout: Duration::from_secs(60),
            max_redirects: 5,
            max_bytes: MAX_SOURCE_BYTES,
            max_concurrent: 8,
            max_per_host: 6,
            cache_dir: None,
            cache_max_bytes: 256 * 1024 * 1024,
            proxy: ProxyMode::FromEnvironment,
            user_agent: format!("florui-assets/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

/// Where the bytes of a [`Fetched`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Downloaded now.
    Network,
    /// Read from the disk cache without asking the server: it was still fresh.
    Cache,
    /// The disk cache's copy, confirmed by the server with a `304`.
    Revalidated,
}

/// A response that is an image.
#[derive(Debug)]
pub struct Fetched {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
    pub origin: Origin,
}

/// What a [`Fetcher`] has done so far; for diagnostics and tests.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// Requests sent to a server (every hop of a redirect counts).
    pub requests: u64,
    /// Answers read from the disk cache without a request.
    pub cache_hits: u64,
    /// Conditional requests the server answered with `304`.
    pub revalidated: u64,
}

type Outcome = Result<Arc<Fetched>, FetchError>;
type Callback = Box<dyn FnOnce(Outcome) + Send>;

/// One request in the queue or running, and everyone waiting for its answer.
struct Job {
    url: String,
    host: String,
    cancel: AtomicBool,
    waiters: Mutex<Vec<(u64, Callback)>>,
    started: AtomicBool,
}

#[derive(Default)]
struct Queue {
    /// Not started yet, in order.
    waiting: VecDeque<Arc<Job>>,
    /// Waiting or running, by URL.
    jobs: HashMap<String, Arc<Job>>,
    running_by_host: HashMap<String, usize>,
    workers: usize,
    next_waiter: u64,
}

struct Inner {
    config: NetConfig,
    agent: ureq::Agent,
    proxy_error: Option<String>,
    disk: Option<DiskCache>,
    queue: Mutex<Queue>,
    requests: AtomicU64,
    cache_hits: AtomicU64,
    revalidated: AtomicU64,
}

/// Loads images from the network under the limits of a [`NetConfig`]. Cheap to clone;
/// the clones share the queue, the limits and the cache.
#[derive(Clone)]
pub struct Fetcher {
    inner: Arc<Inner>,
}

/// Holds a request: dropping it, or calling [`FetchTicket::cancel`], withdraws the
/// interest of whoever asked. When nobody is left interested, a queued request leaves the
/// queue and a running one stops at its next read. Dropping a ticket whose request has
/// finished does nothing.
pub struct FetchTicket {
    inner: Arc<Inner>,
    job: Arc<Job>,
    waiter: u64,
}

impl FetchTicket {
    pub fn cancel(&self) {
        self.inner.withdraw(&self.job, self.waiter);
    }
}

impl Drop for FetchTicket {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl Fetcher {
    pub fn new(config: NetConfig) -> Self {
        let builder = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // Followed by hand, to keep to http(s), refuse a downgrade and count.
            .max_redirects(0)
            .user_agent(config.user_agent.clone())
            .accept(ACCEPT)
            .timeout_connect(Some(config.connect_timeout))
            .timeout_recv_response(Some(config.stall_timeout))
            .timeout_per_read(Some(config.stall_timeout))
            .timeout_global(Some(config.total_timeout))
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            );
        // A proxy that does not parse is a configuration mistake, and going direct instead
        // would send requests where the application meant them not to go: every request
        // fails with the reason.
        let mut proxy_error = None;
        let builder = match &config.proxy {
            ProxyMode::FromEnvironment => builder,
            ProxyMode::Direct => builder.proxy(None),
            ProxyMode::Url(url) => match ureq::Proxy::new(url) {
                Ok(proxy) => builder.proxy(Some(proxy)),
                Err(error) => {
                    proxy_error = Some(format!("the proxy `{url}` is not usable: {error}"));
                    builder.proxy(None)
                }
            },
        };
        let agent_config = builder.build();
        let disk = config
            .cache_dir
            .clone()
            .map(|dir| DiskCache::new(dir, config.cache_max_bytes));
        Self {
            inner: Arc::new(Inner {
                agent: ureq::Agent::new_with_config(agent_config),
                proxy_error,
                disk,
                queue: Mutex::default(),
                requests: AtomicU64::new(0),
                cache_hits: AtomicU64::new(0),
                revalidated: AtomicU64::new(0),
                config,
            }),
        }
    }

    pub fn config(&self) -> &NetConfig {
        &self.inner.config
    }

    pub fn stats(&self) -> Stats {
        Stats {
            requests: self.inner.requests.load(Ordering::Relaxed),
            cache_hits: self.inner.cache_hits.load(Ordering::Relaxed),
            revalidated: self.inner.revalidated.load(Ordering::Relaxed),
        }
    }

    /// Queues a request for `url`, or joins the one already queued or running for it, and
    /// calls `on_done` with the outcome on one of the fetcher's own threads. An `Err` here
    /// means the URL is not one this crate requests, and `on_done` is never called.
    ///
    /// The `#fragment` of the URL is not part of the request or of the identity.
    pub fn submit(
        &self,
        url: &str,
        on_done: impl FnOnce(Outcome) + Send + 'static,
    ) -> Result<FetchTicket, FetchError> {
        let parsed = parse_url(url)?;
        let key = parsed.as_str().to_owned();
        let host = host_key(&parsed);
        let inner = &self.inner;
        let mut queue = inner.lock();
        let waiter = queue.next_waiter;
        queue.next_waiter += 1;
        let job = match queue.jobs.get(&key) {
            Some(job) => {
                // Somebody wants it again after the last one let go of it.
                job.cancel.store(false, Ordering::SeqCst);
                Arc::clone(job)
            }
            None => {
                let job = Arc::new(Job {
                    url: key.clone(),
                    host,
                    cancel: AtomicBool::new(false),
                    waiters: Mutex::default(),
                    started: AtomicBool::new(false),
                });
                queue.jobs.insert(key, Arc::clone(&job));
                queue.waiting.push_back(Arc::clone(&job));
                job
            }
        };
        job.waiters
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push((waiter, Box::new(on_done)));
        if queue.workers < inner.config.max_concurrent.max(1) {
            queue.workers += 1;
            let inner = Arc::clone(inner);
            let spawned = std::thread::Builder::new()
                .name("florui-fetch".to_owned())
                .spawn(move || inner.work());
            if spawned.is_err() {
                queue.workers -= 1;
            }
        }
        Ok(FetchTicket {
            inner: Arc::clone(inner),
            job,
            waiter,
        })
    }

    /// [`Self::submit`] and wait for the answer on this thread.
    pub fn fetch(&self, url: &str) -> Outcome {
        let (sender, receiver) = std::sync::mpsc::channel();
        let _ticket = self.submit(url, move |outcome| {
            let _ = sender.send(outcome);
        })?;
        receiver.recv().unwrap_or(Err(FetchError::Cancelled))
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Takes `waiter` off `job`; the last one to go cancels it.
    fn withdraw(&self, job: &Arc<Job>, waiter: u64) {
        let mut queue = self.lock();
        let left = {
            let mut waiters = job
                .waiters
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let before = waiters.len();
            waiters.retain(|(id, _)| *id != waiter);
            if waiters.len() == before {
                // Already answered.
                return;
            }
            waiters.len()
        };
        if left > 0 {
            return;
        }
        job.cancel.store(true, Ordering::SeqCst);
        if !job.started.load(Ordering::SeqCst) {
            queue.waiting.retain(|queued| !Arc::ptr_eq(queued, job));
            queue.jobs.remove(&job.url);
        }
    }

    /// A worker thread: runs queued requests that the limits allow until there are none,
    /// then ends.
    fn work(self: Arc<Self>) {
        loop {
            let job = {
                let mut queue = self.lock();
                let limit = self.config.max_per_host.max(1);
                let next = queue.waiting.iter().position(|job| {
                    queue.running_by_host.get(&job.host).copied().unwrap_or(0) < limit
                });
                let Some(next) = next else {
                    queue.workers -= 1;
                    return;
                };
                let job = queue.waiting.remove(next).expect("a position in the queue");
                *queue.running_by_host.entry(job.host.clone()).or_insert(0) += 1;
                job.started.store(true, Ordering::SeqCst);
                job
            };
            let outcome = self.run(&job).map(Arc::new);
            let waiters = {
                let mut queue = self.lock();
                if let Some(count) = queue.running_by_host.get_mut(&job.host) {
                    *count -= 1;
                    if *count == 0 {
                        queue.running_by_host.remove(&job.host);
                    }
                }
                let mut waiters = job
                    .waiters
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if matches!(outcome, Err(FetchError::Cancelled)) && !waiters.is_empty() {
                    // Everyone let go and somebody asked again while it was stopping: it
                    // starts over for them.
                    job.cancel.store(false, Ordering::SeqCst);
                    job.started.store(false, Ordering::SeqCst);
                    drop(waiters);
                    queue.waiting.push_back(job);
                    continue;
                }
                queue.jobs.remove(&job.url);
                std::mem::take(&mut *waiters)
            };
            for (_, callback) in waiters {
                callback(outcome.clone());
            }
        }
    }

    fn run(&self, job: &Job) -> Result<Fetched, FetchError> {
        let cancelled = || job.cancel.load(Ordering::SeqCst);
        if cancelled() {
            return Err(FetchError::Cancelled);
        }
        let now = unix_now();
        let stored = self
            .disk
            .as_ref()
            .and_then(|disk| disk.lookup(&job.url, now));
        if let Some((entry, body)) = &stored
            && entry.is_fresh(now)
        {
            self.cache_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(Fetched {
                bytes: body.clone(),
                content_type: entry.content_type.clone(),
                origin: Origin::Cache,
            });
        }
        if let Some(reason) = &self.proxy_error {
            return Err(FetchError::Transport(reason.clone()));
        }
        let validators = stored.as_ref().map(|(entry, _)| entry);
        let mut response = self.get(job, validators)?;
        let status = response.status().as_u16();
        let received_at = unix_now();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let (etag, last_modified, content_type) = (
            header("etag"),
            header("last-modified"),
            header("content-type"),
        );
        let cache_control = header("cache-control");
        let (expires, date, age, vary) = (
            header("expires"),
            header("date"),
            header("age"),
            header("vary"),
        );
        let policy_of = |etag: Option<&str>, last_modified: Option<&str>| {
            Policy::of(
                &Headers {
                    cache_control: cache_control.as_deref(),
                    expires: expires.as_deref(),
                    date: date.as_deref(),
                    last_modified,
                    etag,
                    age: age.as_deref(),
                    vary: vary.as_deref(),
                },
                received_at,
            )
        };
        if status == 304 {
            let Some((mut entry, body)) = stored else {
                return Err(FetchError::Transport(
                    "the server answered 304 to a request that was not conditional".to_owned(),
                ));
            };
            self.revalidated.fetch_add(1, Ordering::Relaxed);
            if let Some(etag) = etag {
                entry.etag = Some(etag);
            }
            if let Some(modified) = last_modified {
                entry.last_modified = Some(modified);
            }
            let policy = policy_of(entry.etag.as_deref(), entry.last_modified.as_deref());
            entry.stored_at = received_at;
            entry.fresh_for = policy.fresh_for;
            if let Some(disk) = &self.disk {
                if policy.store {
                    disk.renew(&entry);
                } else {
                    disk.remove(&entry.url);
                }
            }
            return Ok(Fetched {
                bytes: body,
                content_type: entry.content_type,
                origin: Origin::Revalidated,
            });
        }
        if status != 200 {
            return Err(FetchError::Status(status));
        }
        let bytes = self.read_body(response.body_mut(), job)?;
        check_content(content_type.as_deref(), &bytes)?;
        if let Some(disk) = &self.disk {
            let policy = policy_of(etag.as_deref(), last_modified.as_deref());
            if policy.store {
                disk.store(
                    &Entry {
                        url: job.url.clone(),
                        etag,
                        last_modified,
                        content_type: content_type.clone(),
                        stored_at: received_at,
                        fresh_for: policy.fresh_for,
                        length: bytes.len() as u64,
                    },
                    &bytes,
                );
            } else {
                disk.remove(&job.url);
            }
        }
        Ok(Fetched {
            bytes,
            content_type,
            origin: Origin::Network,
        })
    }

    /// Sends the request and follows redirects by hand; the response it returns is not a
    /// redirect.
    fn get(
        &self,
        job: &Job,
        validators: Option<&Entry>,
    ) -> Result<ureq::http::Response<ureq::Body>, FetchError> {
        let mut url = parse_url(&job.url)?;
        for _ in 0..=self.config.max_redirects {
            if job.cancel.load(Ordering::SeqCst) {
                return Err(FetchError::Cancelled);
            }
            let mut request = self.agent.get(url.as_str());
            if let Some(entry) = validators {
                if let Some(etag) = &entry.etag {
                    request = request.header("If-None-Match", etag);
                }
                if let Some(modified) = &entry.last_modified {
                    request = request.header("If-Modified-Since", modified);
                }
            }
            self.requests.fetch_add(1, Ordering::Relaxed);
            let response = request.call().map_err(map_error)?;
            if !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                return Ok(response);
            }
            let location = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    FetchError::Transport("a redirect with no Location header".to_owned())
                })?;
            let next = url
                .join(location)
                .map_err(|error| FetchError::InvalidUrl(format!("the redirect: {error}")))?;
            let next = parse_url(next.as_str())?;
            check_redirect(&url, &next)?;
            url = next;
        }
        Err(FetchError::TooManyRedirects {
            limit: self.config.max_redirects,
        })
    }

    /// The body, counted as it arrives.
    fn read_body(&self, body: &mut ureq::Body, job: &Job) -> Result<Vec<u8>, FetchError> {
        let limit = self.config.max_bytes;
        if let Some(declared) = body.content_length()
            && declared > limit
        {
            return Err(FetchError::TooLarge { limit });
        }
        let declared = body.content_length().unwrap_or(0).min(8 * 1024 * 1024);
        let mut bytes = Vec::with_capacity(declared as usize);
        let mut reader = body.as_reader();
        let mut chunk = vec![0u8; 32 * 1024];
        loop {
            if job.cancel.load(Ordering::SeqCst) {
                return Err(FetchError::Cancelled);
            }
            let read = reader.read(&mut chunk).map_err(map_io_error)?;
            if read == 0 {
                return Ok(bytes);
            }
            if bytes.len() as u64 + read as u64 > limit {
                return Err(FetchError::TooLarge { limit });
            }
            bytes.extend_from_slice(&chunk[..read]);
        }
    }
}

/// The URL as it is requested: `http` or `https`, with a host, no credentials and no
/// fragment.
fn parse_url(text: &str) -> Result<Url, FetchError> {
    let mut url =
        Url::parse(text.trim()).map_err(|error| FetchError::InvalidUrl(error.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(FetchError::UnsupportedScheme(url.scheme().to_owned()));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(FetchError::InvalidUrl("it has no host".to_owned()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(FetchError::InvalidUrl(
            "it carries a user name or password".to_owned(),
        ));
    }
    url.set_fragment(None);
    Ok(url)
}

/// What the per-host limit counts: scheme, host and port.
fn host_key(url: &Url) -> String {
    format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str().unwrap_or_default(),
        url.port_or_known_default().unwrap_or(0)
    )
}

/// A redirect may go from `http` to either scheme and from `https` to `https` only.
fn check_redirect(from: &Url, to: &Url) -> Result<(), FetchError> {
    if from.scheme() == "https" && to.scheme() == "http" {
        return Err(FetchError::InsecureRedirect { to: to.to_string() });
    }
    Ok(())
}

fn map_error(error: ureq::Error) -> FetchError {
    match error {
        ureq::Error::Timeout(_) => FetchError::Timeout,
        ureq::Error::Io(error) => map_io_error(error),
        ureq::Error::HostNotFound => FetchError::Transport("the host was not found".to_owned()),
        ureq::Error::BadUri(reason) => FetchError::InvalidUrl(reason),
        other => FetchError::Transport(other.to_string()),
    }
}

fn map_io_error(error: std::io::Error) -> FetchError {
    if let Some(inner) = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<ureq::Error>())
    {
        if matches!(inner, ureq::Error::Timeout(_)) {
            return FetchError::Timeout;
        }
        return FetchError::Transport(inner.to_string());
    }
    if matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) {
        return FetchError::Timeout;
    }
    FetchError::Transport(error.to_string())
}

/// Judges a response by its media type and its bytes together; see the module doc.
fn check_content(content_type: Option<&str>, bytes: &[u8]) -> Result<(), FetchError> {
    let declared = content_type
        .and_then(|value| value.split(';').next())
        .map(|media| media.trim().to_ascii_lowercase())
        .filter(|media| !media.is_empty());
    let found = url_image::sniff(bytes);
    let found_name = match found {
        Some(Format::Raster) => crate::raster::Format::sniff(bytes).map(|format| match format {
            crate::raster::Format::Png => "PNG",
            crate::raster::Format::Jpeg => "JPEG",
            crate::raster::Format::Webp => "WebP",
            crate::raster::Format::Gif => "GIF",
        }),
        Some(Format::Svg) => Some("SVG"),
        None => None,
    };
    let not_an_image = |media: &str| FetchError::NotAnImage {
        content_type: media.to_owned(),
    };
    let Some(declared) = declared else {
        return found.map(|_| ()).ok_or_else(|| not_an_image("unknown"));
    };
    if matches!(
        declared.as_str(),
        "application/octet-stream" | "binary/octet-stream" | "application/x-unknown"
    ) {
        return found.map(|_| ()).ok_or_else(|| not_an_image(&declared));
    }
    if !declared.starts_with("image/") {
        return Err(not_an_image(&declared));
    }
    let expected = if declared.contains("svg") {
        Some(Format::Svg)
    } else {
        crate::raster::Format::of_media_type(&declared).map(|_| Format::Raster)
    };
    let Some(expected) = expected else {
        // An image type with no decoder here (AVIF, BMP, ICO...).
        return Err(not_an_image(&declared));
    };
    // The same family is not enough for a raster image: `image/png` over JPEG bytes says
    // one thing and shows another.
    let same = found == Some(expected)
        && (expected == Format::Svg
            || crate::raster::Format::of_media_type(&declared)
                == crate::raster::Format::sniff(bytes));
    if same {
        Ok(())
    } else {
        Err(FetchError::ContentTypeMismatch {
            declared,
            found: found_name.unwrap_or("not an image"),
        })
    }
}

static PENDING_CONFIG: Mutex<Option<NetConfig>> = Mutex::new(None);
static SHARED: OnceLock<Fetcher> = OnceLock::new();

/// Sets the configuration of [`shared_fetcher`], which the images that name a URL without
/// anything to hold a fetcher (a `background-image`, an `<img>`) load through. Takes
/// effect only before the first use; afterwards nothing changes and this returns `false`.
pub fn configure(config: NetConfig) -> bool {
    if SHARED.get().is_some() {
        return false;
    }
    *PENDING_CONFIG
        .lock()
        .unwrap_or_else(|poison| poison.into_inner()) = Some(config);
    true
}

/// The process-wide fetcher: one queue and one set of limits for every picture that names
/// a URL, whichever window or element asked.
pub fn shared_fetcher() -> &'static Fetcher {
    SHARED.get_or_init(|| {
        let config = PENDING_CONFIG
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
            .unwrap_or_default();
        Fetcher::new(config)
    })
}

#[cfg(test)]
mod tests;
