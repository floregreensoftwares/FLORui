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
        for directive in headers.cache_control.unwrap_or("").split(',') {
            let directive = directive.trim().to_ascii_lowercase();
            let (name, value) = match directive.split_once('=') {
                Some((name, value)) => (name.trim(), Some(value.trim().trim_matches('"'))),
                None => (directive.as_str(), None),
            };
            match name {
                "no-store" => return never,
                "no-cache" => no_cache = true,
                "must-revalidate" | "proxy-revalidate" => must_revalidate = true,
                "stale-while-revalidate" => {
                    while_revalidating = value.and_then(|v| v.parse().ok()).unwrap_or(0)
                }
                "stale-if-error" => if_error = value.and_then(|v| v.parse().ok()).unwrap_or(0),
                // A malformed value makes the response stale at once, the safe reading.
                "max-age" => max_age = Some(value.and_then(|v| v.parse::<u64>().ok()).unwrap_or(0)),
                _ => {}
            }
        }
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
}

impl Entry {
    fn age(&self, now: u64) -> u64 {
        // A clock set back is not a reason to keep it fresh forever, but it is not a
        // reason to throw it away either.
        now.saturating_sub(self.stored_at)
    }

    pub fn is_fresh(&self, now: u64) -> bool {
        self.age(now) < self.fresh_for
    }

    /// Stale, but may be used at once while a fresh copy is fetched.
    pub fn may_serve_while_revalidating(&self, now: u64) -> bool {
        !self.is_fresh(now) && self.age(now) < self.fresh_for + self.stale_while_revalidate
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
        text
    }

    fn decode(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != "florui-http-cache 1" {
            return None;
        }
        let (mut url, mut etag, mut last_modified, mut content_type) = (None, None, None, None);
        let (mut stored_at, mut fresh_for, mut length) = (None, None, None);
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
        })
    }
}

/// A directory of stored responses, two files each (`<hash>.meta` and `<hash>.body`),
/// kept under a byte budget by dropping the least recently used.
///
/// Files are written to a temporary name and renamed, the body before the metadata, so
/// that another process, or a crash, never leaves a metadata file that names a body that
/// is not whole: an entry whose body is missing or of another length counts as absent.
pub(crate) struct DiskCache {
    dir: PathBuf,
    max_bytes: u64,
    stores: AtomicU32,
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
        }
    }

    fn meta_path(&self, url: &str) -> PathBuf {
        self.dir.join(format!("{}.meta", stem(url)))
    }

    fn body_path(&self, url: &str) -> PathBuf {
        self.dir.join(format!("{}.body", stem(url)))
    }

    /// The stored response for `url` and its body, if both are there and whole. A hit
    /// counts as a use for the eviction order.
    pub fn lookup(&self, url: &str, now: u64) -> Option<(Entry, Vec<u8>)> {
        let meta_path = self.meta_path(url);
        let entry = Entry::decode(&fs::read_to_string(&meta_path).ok()?)?;
        if entry.url != url {
            return None;
        }
        let body = fs::read(self.body_path(url)).ok()?;
        if body.len() as u64 != entry.length {
            return None;
        }
        touch(&meta_path, now);
        Some((entry, body))
    }

    /// Writes `entry` and `body`, then drops the oldest entries if the directory is over
    /// budget. A failure is only a miss later, so it is not reported.
    pub fn store(&self, entry: &Entry, body: &[u8]) {
        if body.len() as u64 > self.max_bytes / 4 || fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let url = entry.url.as_str();
        if write_atomically(&self.body_path(url), body).is_err()
            || write_atomically(&self.meta_path(url), entry.encode().as_bytes()).is_err()
        {
            self.remove(url);
            return;
        }
        if self.stores.fetch_add(1, Ordering::Relaxed) + 1 >= SCAN_EVERY {
            self.stores.store(0, Ordering::Relaxed);
            self.evict();
        }
    }

    /// Writes the metadata of an entry renewed by a `304`; the body stays.
    pub fn renew(&self, entry: &Entry) {
        let _ = write_atomically(&self.meta_path(&entry.url), entry.encode().as_bytes());
    }

    pub fn remove(&self, url: &str) {
        let _ = fs::remove_file(self.meta_path(url));
        let _ = fs::remove_file(self.body_path(url));
    }

    /// Drops the least recently used entries until the directory is at 90% of its budget,
    /// and anything in it that is not part of a whole entry.
    fn evict(&self) {
        let Ok(listing) = fs::read_dir(&self.dir) else {
            return;
        };
        let mut entries: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
        let mut total = 0u64;
        for item in listing.flatten() {
            let path = item.path();
            if path.extension().and_then(|e| e.to_str()) != Some("meta") {
                continue;
            }
            let Ok(meta) = item.metadata() else { continue };
            let body = path.with_extension("body");
            let body_len = fs::metadata(&body).map_or(0, |m| m.len());
            let size = meta.len() + body_len;
            total += size;
            entries.push((meta.modified().unwrap_or(UNIX_EPOCH), size, path));
        }
        if total <= self.max_bytes {
            return;
        }
        entries.sort_by_key(|(modified, ..)| *modified);
        let goal = self.max_bytes / 10 * 9;
        for (_, size, path) in entries {
            if total <= goal {
                break;
            }
            let _ = fs::remove_file(path.with_extension("body"));
            let _ = fs::remove_file(&path);
            total -= size;
        }
    }
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
        let old = "florui-http-cache 1\nurl: https://example.com/a.png\nstored-at: 1\nfresh-for: 2\nlength: 3\n";
        let entry = Entry::decode(old).unwrap();
        assert_eq!((entry.stale_while_revalidate, entry.stale_if_error), (0, 0));
    }

    #[test]
    fn freshness_ends_at_the_boundary() {
        let entry = entry("https://example.com/a.png", 1);
        assert!(entry.is_fresh(NOW + 59));
        assert!(!entry.is_fresh(NOW + 60));
        // A clock set back is not a reason to keep it fresh forever.
        assert!(entry.is_fresh(NOW - 1000));
    }

    #[test]
    fn the_disk_cache_returns_what_was_stored_and_only_for_that_url() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_owned(), 1 << 20);
        let stored = entry("https://example.com/a.png", 4);
        cache.store(&stored, b"abcd");
        let (found, body) = cache.lookup("https://example.com/a.png", NOW).unwrap();
        assert_eq!((found, body), (stored, b"abcd".to_vec()));
        assert!(cache.lookup("https://example.com/b.png", NOW).is_none());
    }

    #[test]
    fn a_body_that_is_cut_short_or_missing_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_owned(), 1 << 20);
        let url = "https://example.com/a.png";
        cache.store(&entry(url, 4), b"abcd");
        fs::write(cache.body_path(url), b"ab").unwrap();
        assert!(cache.lookup(url, NOW).is_none());
        fs::remove_file(cache.body_path(url)).unwrap();
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
        fs::copy(
            cache.body_path("https://example.com/a.png"),
            cache.body_path("https://example.com/other.png"),
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
        // Room for about four 1000-byte bodies; no single body over a quarter of it.
        let cache = DiskCache::new(dir.path().to_owned(), 5000);
        let body = vec![7u8; 1000];
        let urls: Vec<String> = (0..8)
            .map(|n| format!("https://example.com/{n}.png"))
            .collect();
        for (n, url) in urls.iter().enumerate() {
            cache.store(&entry(url, 1000), &body);
            // Older entries are older on disk too.
            touch(&cache.meta_path(url), NOW + n as u64);
        }
        cache.stores.store(SCAN_EVERY, Ordering::Relaxed);
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
    fn a_body_over_a_quarter_of_the_budget_is_not_stored() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_owned(), 1000);
        cache.store(&entry("https://example.com/a.png", 300), &[0u8; 300]);
        assert!(cache.lookup("https://example.com/a.png", NOW).is_none());
    }
}
