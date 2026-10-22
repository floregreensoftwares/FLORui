//! The HTTP cache kept on disk: what a response may be reused for ([`Policy`], from
//! `Cache-Control`, `Expires`, `Age` and the validators) and where it is kept
//! ([`DiskCache`]).
//!
//! It is a private cache for a single user agent that sends the same request headers
//! every time and no credentials, which is why `Vary` is not part of the key: a response
//! that varies on anything but the headers this crate always sends (`Vary: *`) is not
//! stored. Only complete `200` responses are stored. A stored response is used while it is
//! fresh; once stale it is asked again with `If-None-Match` and `If-Modified-Since`, and a
//! `304` renews it without the body travelling again. Two extensions of `Cache-Control`
//! (RFC 5861) let a stale response be used anyway: for `stale-while-revalidate` seconds it
//! is used at once while a fresh copy is fetched in the background, and for
//! `stale-if-error` seconds it is used when asking again fails (a network error or a `5xx`).
//! `no-cache` and `must-revalidate` turn both off.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A fresh response with no `max-age` and no `Expires` but a `Last-Modified` stays fresh
/// for a tenth of the time since it last changed, up to this long (RFC 9111, 4.2.2).
const HEURISTIC_CAP_SECS: u64 = 24 * 60 * 60;

/// How many stores pass between two looks at the size of the directory.
const SCAN_EVERY: u32 = 16;

/// A body with no metadata, or a temporary file, is probably a store in flight until it is
/// this old; after that it was left by one that did not finish.
const LEFTOVER_AFTER: Duration = Duration::from_secs(10 * 60);

/// What the headers of a response allow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Policy {
    /// The response may be written to the cache at all.
    pub store: bool,
    /// How long it can be used without asking the server, from when it was received.
    pub fresh_for: u64,
    /// For how long after that it may be used while a fresh copy is fetched.
    pub stale_while_revalidate: u64,
    /// For how long after that it may be used when fetching a fresh copy fails.
    pub stale_if_error: u64,
    /// Which of the above the headers said, and which were filled in (a lifetime from the
    /// age of the file, a window of none): a `304` that says nothing about one leaves the
    /// stored response's own.
    pub given: Given,
}

/// What a response's headers set, as opposed to what is assumed when they say nothing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Given {
    pub lifetime: bool,
    pub stale_while_revalidate: bool,
    pub stale_if_error: bool,
    /// `no-cache` or `must-revalidate`: no stale window.
    pub revalidate: bool,
}

/// The headers [`Policy::of`] reads.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Headers<'a> {
    pub cache_control: Option<&'a str>,
    pub expires: Option<&'a str>,
    pub date: Option<&'a str>,
    pub last_modified: Option<&'a str>,
    pub etag: Option<&'a str>,
    pub age: Option<&'a str>,
    pub vary: Option<&'a str>,
}

impl Policy {
    /// What a `200` response with `headers`, received at `now` (seconds since the epoch),
    /// allows.
    pub fn of(headers: &Headers<'_>, now: u64) -> Self {
        let never = Self {
            store: false,
            fresh_for: 0,
            stale_while_revalidate: 0,
            stale_if_error: 0,
            given: Given {
                lifetime: true,
                stale_while_revalidate: true,
                stale_if_error: true,
                revalidate: true,
            },
        };
        if headers
            .vary
            .is_some_and(|vary| vary.split(',').any(|v| v.trim() == "*"))
        {
            return never;
        }
        let mut max_age = None;
        let mut no_cache = false;
        let mut must_revalidate = false;
        let (mut while_revalidating, mut if_error) = (0, 0);
        let mut given = Given::default();
        for directive in headers.cache_control.unwrap_or("").split(',') {
            let directive = directive.trim().to_ascii_lowercase();
            let (name, value) = match directive.split_once('=') {
                Some((name, value)) => (name.trim(), Some(value.trim().trim_matches('"'))),
                None => (directive.as_str(), None),
            };
            match name {
                "no-store" => return never,
                "no-cache" => {
                    no_cache = true;
                    given.lifetime = true;
                    given.revalidate = true;
                }
                "must-revalidate" | "proxy-revalidate" => {
                    must_revalidate = true;
                    given.revalidate = true;
                }
                "stale-while-revalidate" => {
                    while_revalidating = value.and_then(|v| v.parse().ok()).unwrap_or(0);
                    given.stale_while_revalidate = true;
                }
                "stale-if-error" => {
                    if_error = value.and_then(|v| v.parse().ok()).unwrap_or(0);
                    given.stale_if_error = true;
                }
                // A malformed value makes the response stale at once, the safe reading.
                "max-age" => {
                    max_age = Some(value.and_then(|v| v.parse::<u64>().ok()).unwrap_or(0));
                    given.lifetime = true;
                }
                _ => {}
            }
        }
        given.lifetime |= headers.expires.is_some();
        let has_validator = headers.etag.is_some() || headers.last_modified.is_some();
        let date = headers.date.and_then(parse_date).unwrap_or(now);
        let mut fresh_for = if no_cache {
            0
        } else if let Some(max_age) = max_age {
            max_age
        } else if let Some(expires) = headers.expires {
            // An `Expires` that does not parse (`0`, `-1`) says it is already stale.
            parse_date(expires).map_or(0, |expires| expires.saturating_sub(date))
        } else if let Some(modified) = headers.last_modified.and_then(parse_date) {
            (date.saturating_sub(modified) / 10).min(HEURISTIC_CAP_SECS)
        } else {
            0
        };
        let age = headers
            .age
            .and_then(|age| age.trim().parse::<u64>().ok())
            .unwrap_or(0);
        fresh_for = fresh_for.saturating_sub(age);
        let (stale_while_revalidate, stale_if_error) = if no_cache || must_revalidate {
            (0, 0)
        } else {
            (while_revalidating, if_error)
        };
        // A stale response nothing can renew is no use kept.
        Self {
            store: fresh_for > 0 || has_validator,
            fresh_for,
            stale_while_revalidate,
            stale_if_error,
            given,
        }
    }
}

fn parse_date(text: &str) -> Option<u64> {
    httpdate::parse_http_date(text.trim())
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|since| since.as_secs())
}

/// Seconds since the epoch.
pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

/// What is known of a stored response besides its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub url: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_type: Option<String>,
    /// When the response was received, or last renewed by a `304`.
    pub stored_at: u64,
    pub fresh_for: u64,
    pub stale_while_revalidate: u64,
    pub stale_if_error: u64,
    /// The size of the body, to tell a whole file from one cut short.
    pub length: u64,
    /// Which body file is this response's (see [`DiskCache`]); set when it is stored.
    pub body_id: u64,
}

impl Entry {
    fn age(&self, now: u64) -> u64 {
        if self.stored_at > now {
            // The clock was set back since it was stored, so its age is not known. Counting
            // it as new would keep it fresh for as long as the clock is behind: it counts
            // as having just gone stale, which asks the server again (and may still be
            // served within its stale windows if that fails) without throwing it away.
            return self.fresh_for;
        }
        now - self.stored_at
    }

    pub fn is_fresh(&self, now: u64) -> bool {
        self.age(now) < self.fresh_for
    }

    /// Stale, but may be used at once while a fresh copy is fetched.
    pub fn may_serve_while_revalidating(&self, now: u64) -> bool {
        !self.is_fresh(now)
            && self.age(now) < self.fresh_for.saturating_add(self.stale_while_revalidate)
    }

    /// Stale, but may be used if fetching a fresh copy fails.
    pub fn may_serve_on_error(&self, now: u64) -> bool {
        self.age(now) < self.fresh_for.saturating_add(self.stale_if_error)
    }

    fn encode(&self) -> String {
        let mut text = String::from("florui-http-cache 1\n");
        let mut line = |key: &str, value: &str| {
            text.push_str(key);
            text.push_str(": ");
            text.push_str(value);
            text.push('\n');
        };
        line("url", &self.url);
        if let Some(etag) = &self.etag {
            line("etag", etag);
        }
        if let Some(modified) = &self.last_modified {
            line("last-modified", modified);
        }
        if let Some(content_type) = &self.content_type {
            line("content-type", content_type);
        }
        line("stored-at", &self.stored_at.to_string());
        line("fresh-for", &self.fresh_for.to_string());
        line(
            "stale-while-revalidate",
            &self.stale_while_revalidate.to_string(),
        );
        line("stale-if-error", &self.stale_if_error.to_string());
        line("length", &self.length.to_string());
        line("body", &format!("{:016x}", self.body_id));
        text
    }

    fn decode(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != "florui-http-cache 1" {
            return None;
        }
        let (mut url, mut etag, mut last_modified, mut content_type) = (None, None, None, None);
        let (mut stored_at, mut fresh_for, mut length) = (None, None, None);
        let mut body_id = None;
        let (mut while_revalidating, mut if_error) = (0, 0);
        for line in lines {
            let (key, value) = line.split_once(": ")?;
            match key {
                "url" => url = Some(value.to_owned()),
                "etag" => etag = Some(value.to_owned()),
                "last-modified" => last_modified = Some(value.to_owned()),
                "content-type" => content_type = Some(value.to_owned()),
                "stored-at" => stored_at = value.parse().ok(),
                "fresh-for" => fresh_for = value.parse().ok(),
                "stale-while-revalidate" => while_revalidating = value.parse().ok()?,
                "stale-if-error" => if_error = value.parse().ok()?,
                "length" => length = value.parse().ok(),
                "body" => body_id = u64::from_str_radix(value, 16).ok(),
                _ => {}
            }
        }
        Some(Self {
            url: url?,
            etag,
            last_modified,
            content_type,
            stored_at: stored_at?,
            fresh_for: fresh_for?,
            stale_while_revalidate: while_revalidating,
            stale_if_error: if_error,
            length: length?,
            body_id: body_id?,
        })
    }
}

/// A directory of stored responses, two files each (`<hash>.meta` and `<hash>.body`),
/// kept under a byte budget by dropping the least recently used.
///
/// Files are written to a temporary name and renamed, the body before the metadata, and each
/// response's body has a name of its own (`<hash>.<id>.body`, the id kept in the metadata):
/// a response stored over an older one never replaces the body that the older metadata
/// names, so another process, or a crash, cannot pair new bytes with old validators. The
/// metadata is what makes an entry exist; an entry whose body is missing or of another
/// length counts as absent.
pub(crate) struct DiskCache {
    dir: PathBuf,
    max_bytes: u64,
    stores: AtomicU32,
    /// What was written since the directory was last looked at.
    written: std::sync::atomic::AtomicU64,
}

/// FNV-1a over the URL: stable across runs and Rust versions, which `DefaultHasher` does
/// not promise. A collision is caught by the URL kept in the metadata.
fn stem(url: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in url.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

impl DiskCache {
    pub fn new(dir: PathBuf, max_bytes: u64) -> Self {
        Self {
            dir,
            max_bytes,
            // So that the first store looks at what an earlier run left.
            stores: AtomicU32::new(SCAN_EVERY - 1),
            written: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn meta_path(&self, url: &str) -> PathBuf {
        self.dir.join(format!("{}.meta", stem(url)))
    }

    fn body_path(&self, entry: &Entry) -> PathBuf {
        self.dir
            .join(format!("{}.{:016x}.body", stem(&entry.url), entry.body_id))
    }

    /// The stored response for `url`, if its metadata is there and its body is there and
    /// whole (judged by its size, without reading it). A hit counts as a use for the
    /// eviction order.
    pub fn lookup_entry(&self, url: &str, now: u64) -> Option<Entry> {
        let meta_path = self.meta_path(url);
        let entry = Entry::decode(&fs::read_to_string(&meta_path).ok()?)?;
        if entry.url != url || fs::metadata(self.body_path(&entry)).ok()?.len() != entry.length {
            return None;
        }
        touch(&meta_path, now);
        Some(entry)
    }

    /// The body of `entry`, read when it is needed: a stale entry that the server answers
    /// with a new body never needs it. `None` if it is gone or not whole by now.
    pub fn read_body(&self, entry: &Entry) -> Option<Vec<u8>> {
        let body = fs::read(self.body_path(entry)).ok()?;
        (body.len() as u64 == entry.length).then_some(body)
    }

    /// [`Self::lookup_entry`] and [`Self::read_body`] together.
    #[cfg(test)]
    pub fn lookup(&self, url: &str, now: u64) -> Option<(Entry, Vec<u8>)> {
        let entry = self.lookup_entry(url, now)?;
        let body = self.read_body(&entry)?;
        Some((entry, body))
    }

    /// Writes `entry` and `body`, then drops the oldest entries if the directory is over
    /// budget. A failure is only a miss later, so it is not reported.
    pub fn store(&self, entry: &Entry, body: &[u8]) {
        if body.len() as u64 > self.max_bytes / 4 {
            // Too big to keep, and the copy kept before is out of date now.
            self.remove(&entry.url);
            return;
        }
        if fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let url = entry.url.as_str();
        // The response that this one replaces, whose body goes once the new metadata is in.
        let replaced = self.read_meta(url);
        let entry = Entry {
            body_id: fresh_id(),
            ..entry.clone()
        };
        if write_atomically(&self.body_path(&entry), body).is_err()
            || write_atomically(&self.meta_path(url), entry.encode().as_bytes()).is_err()
        {
            let _ = fs::remove_file(self.body_path(&entry));
            return;
        }
        if let Some(replaced) = replaced {
            let _ = fs::remove_file(self.body_path(&replaced));
        }
        // Often enough that the directory cannot run far over its budget: every few stores,
        // and whenever a quarter of the budget has been written since the last look.
        let written =
            self.written.fetch_add(body.len() as u64, Ordering::Relaxed) + body.len() as u64;
        if self.stores.fetch_add(1, Ordering::Relaxed) + 1 >= SCAN_EVERY
            || written >= self.max_bytes / 4
        {
            self.stores.store(0, Ordering::Relaxed);
            self.written.store(0, Ordering::Relaxed);
            self.evict();
        }
    }

    /// Writes the metadata of an entry renewed by a `304`; the body stays. Only if the
    /// entry on disk is still the one that was renewed (`before`): another request for the
    /// same address may have stored a new response since, whose body this must not be put
    /// over.
    pub fn renew(&self, before: &Entry, entry: &Entry) {
        let same = fs::read_to_string(self.meta_path(&entry.url))
            .ok()
            .and_then(|text| Entry::decode(&text))
            .is_some_and(|now| {
                (now.body_id, &now.etag, &now.last_modified, now.length)
                    == (
                        before.body_id,
                        &before.etag,
                        &before.last_modified,
                        before.length,
                    )
            });
        if same {
            let _ = write_atomically(&self.meta_path(&entry.url), entry.encode().as_bytes());
        }
    }

    pub fn remove(&self, url: &str) {
        let entry = self.read_meta(url);
        let _ = fs::remove_file(self.meta_path(url));
        if let Some(entry) = entry {
            let _ = fs::remove_file(self.body_path(&entry));
        }
    }

    /// The metadata of `url`, whether or not its body is there.
    fn read_meta(&self, url: &str) -> Option<Entry> {
        Entry::decode(&fs::read_to_string(self.meta_path(url)).ok()?)
            .filter(|entry| entry.url == url)
    }

    /// Drops the files left by stores that did not finish, then the least recently used
    /// entries until the directory is at 90% of its budget.
    ///
    /// Works from the directory listing alone (names, sizes, times), without opening the
    /// metadata files: the name of a body is its response's hash and id, so the bodies of an
    /// entry are the ones that start with the hash of its metadata's name. The id is read
    /// from the metadata only to tell which of several bodies of one hash is the live one.
    fn evict(&self) {
        let Ok(listing) = fs::read_dir(&self.dir) else {
            return;
        };
        let now = SystemTime::now();
        let old = |modified: SystemTime| {
            now.duration_since(modified)
                .is_ok_and(|age| age >= LEFTOVER_AFTER)
        };
        struct Group {
            meta: Option<(PathBuf, u64, SystemTime)>,
            bodies: Vec<(PathBuf, u64, SystemTime)>,
        }
        let mut groups: std::collections::HashMap<String, Group> = std::collections::HashMap::new();
        for item in listing.flatten() {
            let path = item.path();
            let Ok(meta) = item.metadata() else { continue };
            let modified = meta.modified().unwrap_or(UNIX_EPOCH);
            let name = item.file_name().to_string_lossy().into_owned();
            // `<hash>.meta`, `<hash>.<id>.body`, `<hash>.tmp<pid>-<n>`.
            let hash = name.split('.').next().unwrap_or("").to_owned();
            let group = groups.entry(hash).or_insert(Group {
                meta: None,
                bodies: Vec::new(),
            });
            match path.extension().and_then(|e| e.to_str()) {
                Some("meta") => group.meta = Some((path, meta.len(), modified)),
                Some("body") => group.bodies.push((path, meta.len(), modified)),
                Some(extension) if extension.starts_with("tmp") && old(modified) => {
                    let _ = fs::remove_file(&path);
                }
                _ => {}
            }
        }
        // Entries by age with their size and files.
        let mut entries: Vec<(SystemTime, u64, Vec<PathBuf>)> = Vec::new();
        let mut total = 0u64;
        for group in groups.into_values() {
            let Some((meta_path, meta_len, meta_modified)) = group.meta else {
                // Bodies that no metadata goes with: left by a store that did not finish, or
                // by an older version of the cache. They go once too old to be one in flight.
                for (path, _, modified) in group.bodies {
                    if old(modified) {
                        let _ = fs::remove_file(path);
                    }
                }
                continue;
            };
            let mut bodies = group.bodies;
            // With one body it is the live one; with several, the metadata says which, and
            // the others are left over from stores that were replaced or did not finish.
            if bodies.len() > 1
                && let Some(entry) = fs::read_to_string(&meta_path)
                    .ok()
                    .and_then(|text| Entry::decode(&text))
            {
                let live = self.body_path(&entry);
                bodies.retain(|(path, _, modified)| {
                    let keep = *path == live || !old(*modified);
                    if !keep {
                        let _ = fs::remove_file(path);
                    }
                    keep
                });
            }
            let size = meta_len + bodies.iter().map(|(_, len, _)| len).sum::<u64>();
            total += size;
            let mut files = vec![meta_path];
            files.extend(bodies.into_iter().map(|(path, ..)| path));
            entries.push((meta_modified, size, files));
        }
        if total <= self.max_bytes {
            return;
        }
        entries.sort_by_key(|(modified, ..)| *modified);
        let goal = self.max_bytes / 10 * 9;
        for (_, size, files) in entries {
            if total <= goal {
                break;
            }
            // The metadata first: it is what makes the entry exist.
            for file in files {
                let _ = fs::remove_file(file);
            }
            total -= size;
        }
    }
}

/// A number no other response stored here shares, in this process or another: the time, the
/// process and a count, mixed.
fn fresh_id() -> u64 {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos() as u64);
    let mixed = nanos
        ^ (u64::from(std::process::id()) << 32)
        ^ u64::from(COUNTER.fetch_add(1, Ordering::Relaxed)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    mixed.wrapping_mul(0xff51_afd7_ed55_8ccd)
}

fn touch(path: &Path, now: u64) {
    if let Ok(file) = fs::File::options().write(true).open(path) {
        let _ = file.set_modified(UNIX_EPOCH + Duration::from_secs(now));
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // The process id and a counter keep two writers of the same entry from sharing a
    // temporary file.
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let temporary = path.with_extension(format!(
        "tmp{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.flush()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn policy(headers: Headers<'_>) -> Policy {
        Policy::of(&headers, NOW)
    }

    #[test]
    fn max_age_gives_the_freshness_and_age_is_taken_off() {
        let headers = Headers {
            cache_control: Some("public, max-age=600"),
            age: Some("100"),
            ..Headers::default()
        };
        assert_eq!(
            (policy(headers).store, policy(headers).fresh_for),
            (true, 500)
        );
    }

    #[test]
    fn no_store_and_vary_star_are_never_stored() {
        for headers in [
            Headers {
                cache_control: Some("max-age=60, no-store"),
                ..Headers::default()
            },
            Headers {
                cache_control: Some("max-age=60"),
                vary: Some("Accept-Encoding, *"),
                ..Headers::default()
            },
        ] {
            assert!(!policy(headers).store);
        }
    }

    #[test]
    fn no_cache_is_stored_only_when_it_can_be_validated() {
        let with_etag = Headers {
            cache_control: Some("no-cache, max-age=600"),
            etag: Some("\"a\""),
            ..Headers::default()
        };
        assert_eq!(
            (policy(with_etag).store, policy(with_etag).fresh_for),
            (true, 0)
        );
        let without = Headers {
            cache_control: Some("no-cache"),
            ..Headers::default()
        };
        assert!(!policy(without).store);
    }

    #[test]
    fn expires_counts_from_the_date_of_the_response_and_a_bad_one_is_stale() {
        let headers = Headers {
            expires: Some("Sun, 06 Nov 1994 09:49:37 GMT"),
            date: Some("Sun, 06 Nov 1994 08:49:37 GMT"),
            etag: Some("\"a\""),
            ..Headers::default()
        };
        assert_eq!(policy(headers).fresh_for, 3600);
        let headers = Headers {
            expires: Some("0"),
            etag: Some("\"a\""),
            ..Headers::default()
        };
        assert_eq!(policy(headers).fresh_for, 0);
    }

    #[test]
    fn max_age_wins_over_expires() {
        let headers = Headers {
            cache_control: Some("max-age=10"),
            expires: Some("Sun, 06 Nov 1994 09:49:37 GMT"),
            date: Some("Sun, 06 Nov 1994 08:49:37 GMT"),
            ..Headers::default()
        };
        assert_eq!(policy(headers).fresh_for, 10);
    }

    #[test]
    fn a_response_with_nothing_to_go_on_uses_a_tenth_of_its_age_up_to_a_day() {
        let headers = Headers {
            date: Some("Sun, 06 Nov 1994 08:49:37 GMT"),
            last_modified: Some("Sun, 06 Nov 1994 07:49:37 GMT"),
            ..Headers::default()
        };
        assert_eq!(policy(headers).fresh_for, 360);
        let headers = Headers {
            date: Some("Sun, 06 Nov 1994 08:49:37 GMT"),
            last_modified: Some("Sat, 06 Nov 1993 08:49:37 GMT"),
            ..Headers::default()
        };
        assert_eq!(policy(headers).fresh_for, HEURISTIC_CAP_SECS);
    }

    #[test]
    fn a_response_that_is_stale_and_cannot_be_validated_is_not_stored() {
        assert!(!policy(Headers::default()).store);
    }

    #[test]
    fn a_malformed_max_age_is_stale() {
        let headers = Headers {
            cache_control: Some("max-age=soon"),
            etag: Some("\"a\""),
            ..Headers::default()
        };
        assert_eq!(policy(headers).fresh_for, 0);
    }

    fn entry(url: &str, length: u64) -> Entry {
        Entry {
            url: url.to_owned(),
            etag: Some("\"abc\"".to_owned()),
            last_modified: Some("Sun, 06 Nov 1994 08:49:37 GMT".to_owned()),
            content_type: Some("image/png".to_owned()),
            stored_at: NOW,
            fresh_for: 60,
            stale_while_revalidate: 30,
            stale_if_error: 600,
            length,
            body_id: 0,
        }
    }

    #[test]
    fn an_entry_survives_being_written_and_read() {
        let entry = entry("https://example.com/a.png?x=1", 3);
        assert_eq!(Entry::decode(&entry.encode()), Some(entry));
    }

    #[test]
    fn the_stale_windows_follow_the_freshness() {
        let entry = entry("https://example.com/a.png", 1);
        // Fresh for 60 s, usable while revalidating until 90 s, usable on error until 660 s.
        assert!(!entry.may_serve_while_revalidating(NOW + 59));
        assert!(entry.may_serve_while_revalidating(NOW + 60));
        assert!(entry.may_serve_while_revalidating(NOW + 89));
        assert!(!entry.may_serve_while_revalidating(NOW + 90));
        assert!(entry.may_serve_on_error(NOW + 659));
        assert!(!entry.may_serve_on_error(NOW + 660));
    }

    #[test]
    fn the_stale_windows_are_read_from_cache_control() {
        let headers = Headers {
            cache_control: Some("max-age=60, stale-while-revalidate=30, stale-if-error=600"),
            ..Headers::default()
        };
        let policy = policy(headers);
        assert_eq!(
            (policy.stale_while_revalidate, policy.stale_if_error),
            (30, 600)
        );
    }

    #[test]
    fn no_cache_and_must_revalidate_turn_the_stale_windows_off() {
        for directive in ["no-cache", "must-revalidate"] {
            let control =
                format!("max-age=60, {directive}, stale-while-revalidate=30, stale-if-error=600");
            let headers = Headers {
                cache_control: Some(&control),
                etag: Some("\"a\""),
                ..Headers::default()
            };
            let policy = policy(headers);
            assert_eq!(
                (policy.stale_while_revalidate, policy.stale_if_error),
                (0, 0)
            );
        }
    }

    #[test]
    fn an_entry_written_before_the_stale_windows_existed_reads_with_none() {
        let old = "florui-http-cache 1\nurl: https://example.com/a.png\nstored-at: 1\nfresh-for: 2\nlength: 3\nbody: 00000000000000ab\n";
        let entry = Entry::decode(old).unwrap();
        assert_eq!((entry.stale_while_revalidate, entry.stale_if_error), (0, 0));
        // One from before bodies had names of their own names no body: it is absent.
        let older = "florui-http-cache 1\nurl: https://example.com/a.png\nstored-at: 1\nfresh-for: 2\nlength: 3\n";
        assert_eq!(Entry::decode(older), None);
    }

    #[test]
    fn freshness_ends_at_the_boundary() {
        let entry = entry("https://example.com/a.png", 1);
        assert!(entry.is_fresh(NOW + 59));
        assert!(!entry.is_fresh(NOW + 60));
        // A clock set back is not a reason to keep it fresh for as long as it stays back: it
        // counts as just gone stale, within its windows.
        assert!(!entry.is_fresh(NOW - 1000));
        assert!(entry.may_serve_while_revalidating(NOW - 1000));
        assert!(entry.may_serve_on_error(NOW - 1000));
    }

    #[test]
    fn the_disk_cache_returns_what_was_stored_and_only_for_that_url() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_owned(), 1 << 20);
        let stored = entry("https://example.com/a.png", 4);
        cache.store(&stored, b"abcd");
        let (found, body) = cache.lookup("https://example.com/a.png", NOW).unwrap();
        assert_eq!(
            (
                Entry {
                    body_id: 0,
                    ..found
                },
                body
            ),
            (stored, b"abcd".to_vec())
        );
        assert!(cache.lookup("https://example.com/b.png", NOW).is_none());
    }

    #[test]
    fn a_body_that_is_cut_short_or_missing_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_owned(), 1 << 20);
        let url = "https://example.com/a.png";
        cache.store(&entry(url, 4), b"abcd");
        let stored = cache.read_meta(url).unwrap();
        fs::write(cache.body_path(&stored), b"ab").unwrap();
        assert!(cache.lookup(url, NOW).is_none());
        fs::remove_file(cache.body_path(&stored)).unwrap();
        assert!(cache.lookup(url, NOW).is_none());
    }

    #[test]
    fn a_metadata_file_that_names_another_url_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_owned(), 1 << 20);
        cache.store(&entry("https://example.com/a.png", 1), b"a");
        // As if two URLs had the same hash.
        fs::copy(
            cache.meta_path("https://example.com/a.png"),
            cache.meta_path("https://example.com/other.png"),
        )
        .unwrap();
        assert!(cache.lookup("https://example.com/other.png", NOW).is_none());
    }

    #[test]
    fn a_corrupt_metadata_file_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_owned(), 1 << 20);
        let url = "https://example.com/a.png";
        cache.store(&entry(url, 1), b"a");
        fs::write(cache.meta_path(url), "garbage").unwrap();
        assert!(cache.lookup(url, NOW).is_none());
    }

    #[test]
    fn the_least_recently_used_entries_go_when_the_directory_is_over_budget() {
        let dir = tempfile::tempdir().unwrap();
        // Written under a large budget, so that nothing is dropped while it is filled.
        let filling = DiskCache::new(dir.path().to_owned(), 1 << 20);
        let body = vec![7u8; 1000];
        let urls: Vec<String> = (0..8)
            .map(|n| format!("https://example.com/{n}.png"))
            .collect();
        for (n, url) in urls.iter().enumerate() {
            filling.store(&entry(url, 1000), &body);
            // Older entries are older on disk too.
            touch(&filling.meta_path(url), NOW + n as u64);
        }
        // Room for about four 1000-byte bodies; no single body over a quarter of it.
        let cache = DiskCache::new(dir.path().to_owned(), 5000);
        cache.evict();
        let kept: Vec<bool> = urls
            .iter()
            .map(|url| cache.lookup(url, NOW + 100).is_some())
            .collect();
        // The newest are there and the oldest are not.
        assert!(kept[7] && kept[6]);
        assert!(!kept[0] && !kept[1]);
        let total: u64 = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|item| item.metadata().unwrap().len())
            .sum();
        assert!(total <= 5000, "{total} bytes left");
    }

    #[test]
    fn a_quarter_of_the_budget_written_asks_for_a_look_before_the_sixteenth_store() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_owned(), 8000);
        let body = vec![1u8; 1000];
        // Each is a quarter of the budget at most; ten of them are well over the budget, and
        // far fewer stores than the usual interval.
        for n in 0..10 {
            let url = format!("https://example.com/{n}.png");
            cache.store(&entry(&url, 1000), &body);
            touch(&cache.meta_path(&url), NOW + n as u64);
        }
        let total: u64 = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|item| item.metadata().unwrap().len())
            .sum();
        assert!(
            total <= 8000 + 2 * 1300,
            "{total} bytes, far over the budget"
        );
    }

    #[test]
    fn a_body_over_a_quarter_of_the_budget_is_not_stored() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_owned(), 1000);
        cache.store(&entry("https://example.com/a.png", 300), &[0u8; 300]);
        assert!(cache.lookup("https://example.com/a.png", NOW).is_none());
    }

    #[test]
    fn files_left_by_stores_that_did_not_finish_are_cleaned_once_they_are_old() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_owned(), 1 << 20);
        cache.store(&entry("https://example.com/kept.png", 1), b"a");
        let long_ago = SystemTime::now() - Duration::from_secs(3600);
        let leave = |name: &str, age: Option<SystemTime>| {
            let path = dir.path().join(name);
            fs::write(&path, b"leftover").unwrap();
            if let Some(age) = age {
                fs::File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(age)
                    .unwrap();
            }
            path
        };
        let old_body = leave("0000000000000001.body", Some(long_ago));
        let old_temporary = leave("0000000000000002.tmp123-4", Some(long_ago));
        let young_body = leave("0000000000000003.body", None);
        let young_temporary = leave("0000000000000004.tmp123-5", None);
        cache.evict();
        assert!(!old_body.exists() && !old_temporary.exists());
        assert!(
            young_body.exists() && young_temporary.exists(),
            "may be in flight"
        );
        assert!(cache.lookup("https://example.com/kept.png", NOW).is_some());
    }

    #[test]
    fn a_body_too_big_to_keep_takes_the_older_copy_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_owned(), 1000);
        let url = "https://example.com/a.png";
        cache.store(&entry(url, 10), &[0u8; 10]);
        assert!(cache.lookup(url, NOW).is_some());
        cache.store(&entry(url, 600), &[0u8; 600]);
        assert!(
            cache.lookup(url, NOW).is_none(),
            "the old copy is not served as if current"
        );
    }

    #[test]
    fn windows_that_are_absurdly_long_do_not_overflow() {
        let mut entry = entry("https://example.com/a.png", 1);
        entry.fresh_for = u64::MAX - 1;
        entry.stale_while_revalidate = u64::MAX;
        entry.stale_if_error = u64::MAX;
        assert!(entry.is_fresh(NOW + 1));
        assert!(entry.may_serve_on_error(NOW + 1));
        entry.fresh_for = 10;
        assert!(entry.may_serve_while_revalidating(NOW + 11));
    }

    #[test]
    fn a_renewal_does_not_go_over_a_response_stored_since() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_owned(), 1 << 20);
        let url = "https://example.com/a.png";
        cache.store(&entry(url, 3), b"old");
        let (before, _) = cache.lookup(url, NOW).unwrap();
        // Another request stored a new response while the 304 was on its way.
        let mut newer = entry(url, 5);
        newer.etag = Some("\"newer\"".to_owned());
        cache.store(&newer, b"newer");
        let mut renewed = before.clone();
        renewed.stored_at += 10;
        cache.renew(&before, &renewed);
        let (found, body) = cache.lookup(url, NOW).unwrap();
        assert_eq!(
            (found.etag.as_deref(), body),
            (Some("\"newer\""), b"newer".to_vec())
        );
        // And when nothing came between, it renews.
        let mut again = found.clone();
        again.stored_at += 20;
        cache.renew(&found, &again);
        assert_eq!(cache.lookup(url, NOW).unwrap().0.stored_at, again.stored_at);
    }

    #[test]
    fn a_response_of_the_same_size_never_takes_the_body_of_the_one_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_owned(), 1 << 20);
        let url = "https://example.com/a.png";
        let mut first = entry(url, 4);
        first.etag = Some("\"a\"".to_owned());
        cache.store(&first, b"aaaa");
        let (kept, _) = cache.lookup(url, NOW).unwrap();
        // A second store that dies after writing its body, before the metadata: the body is
        // there under a name of its own, and the metadata still names the first.
        let stray = Entry {
            body_id: kept.body_id.wrapping_add(1),
            ..kept.clone()
        };
        fs::write(cache.body_path(&stray), b"bbbb").unwrap();
        let (found, body) = cache.lookup(url, NOW).unwrap();
        assert_eq!(
            (found.etag.as_deref(), body),
            (Some("\"a\""), b"aaaa".to_vec()),
            "the validators still go with their own bytes"
        );
        // A store that finishes replaces both, and the old body goes.
        let mut second = entry(url, 4);
        second.etag = Some("\"b\"".to_owned());
        cache.store(&second, b"bbbb");
        let (found, body) = cache.lookup(url, NOW).unwrap();
        assert_eq!(
            (found.etag.as_deref(), body),
            (Some("\"b\""), b"bbbb".to_vec())
        );
        assert!(!cache.body_path(&kept).exists());
    }

    #[test]
    fn an_old_stray_body_beside_the_live_one_goes_and_the_live_one_stays() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_owned(), 1 << 20);
        let url = "https://example.com/a.png";
        cache.store(&entry(url, 4), b"live");
        let (live, _) = cache.lookup(url, NOW).unwrap();
        let stray = Entry {
            body_id: live.body_id.wrapping_add(7),
            ..live.clone()
        };
        fs::write(cache.body_path(&stray), b"stray").unwrap();
        let long_ago = SystemTime::now() - Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(cache.body_path(&stray))
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        cache.evict();
        assert!(!cache.body_path(&stray).exists());
        assert_eq!(cache.lookup(url, NOW).unwrap().1, b"live");
    }
}
