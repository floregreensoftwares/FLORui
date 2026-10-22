use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::test_server::{Gauge, Reply, Server};
use super::*;

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

fn config() -> NetConfig {
    NetConfig {
        proxy: ProxyMode::Direct,
        ..NetConfig::default()
    }
}

fn fetcher() -> Fetcher {
    Fetcher::new(config())
}

fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn idle(fetcher: &Fetcher) -> bool {
    let queue = fetcher.inner.lock();
    queue.workers == 0 && queue.jobs.is_empty() && queue.waiting.is_empty()
}

#[test]
fn an_image_is_downloaded_with_its_type() {
    let server = Server::start(|_| Reply::ok("image/png", png()));
    let fetched = fetcher().fetch(&server.url("/a.png")).unwrap();
    assert_eq!(fetched.bytes, png());
    assert_eq!(fetched.content_type.as_deref(), Some("image/png"));
    assert_eq!(fetched.origin, Origin::Network);
}

#[test]
fn the_request_says_what_it_accepts_and_who_asks_and_sends_nothing_else_about_the_user() {
    let (sender, receiver) = mpsc::channel();
    let sender = std::sync::Mutex::new(sender);
    let server = Server::start(move |request| {
        sender
            .lock()
            .unwrap()
            .send(request.headers.clone())
            .unwrap();
        Reply::ok("image/png", png())
    });
    fetcher().fetch(&server.url("/a.png")).unwrap();
    let headers = receiver.recv().unwrap();
    assert!(headers["accept"].starts_with("image/webp,image/png"));
    assert!(headers["user-agent"].starts_with("florui-assets/"));
    for name in ["cookie", "authorization", "referer"] {
        assert!(!headers.contains_key(name), "{name} was sent");
    }
}

#[test]
fn a_status_that_is_not_a_success_is_an_error() {
    let server = Server::start(|request| match request.path.as_str() {
        "/gone" => Reply::status(404),
        _ => Reply::status(503),
    });
    let fetcher = fetcher();
    assert_eq!(
        fetcher.fetch(&server.url("/gone")).unwrap_err(),
        FetchError::Status(404)
    );
    assert_eq!(
        fetcher.fetch(&server.url("/busy")).unwrap_err(),
        FetchError::Status(503)
    );
}

#[test]
fn a_redirect_is_followed_even_when_the_location_is_relative() {
    let server = Server::start(|request| match request.path.as_str() {
        "/old" => Reply::status(302).header("Location", "/dir/new.png"),
        "/dir/new.png" => Reply::ok("image/png", png()),
        _ => Reply::status(404),
    });
    let fetched = fetcher().fetch(&server.url("/old")).unwrap();
    assert_eq!(fetched.bytes, png());
    assert_eq!(server.seen(), ["/old", "/dir/new.png"]);
}

#[test]
fn redirects_past_the_limit_are_an_error_and_stop_at_the_limit() {
    let server = Server::start(|_| Reply::status(301).header("Location", "/again"));
    let fetcher = Fetcher::new(NetConfig {
        max_redirects: 3,
        ..config()
    });
    assert_eq!(
        fetcher.fetch(&server.url("/start")).unwrap_err(),
        FetchError::TooManyRedirects { limit: 3 }
    );
    // The first request and three redirects.
    assert_eq!(server.hits(), 4);
}

#[test]
fn a_redirect_to_another_scheme_is_refused() {
    let server =
        Server::start(|_| Reply::status(302).header("Location", "ftp://example.com/a.png"));
    assert_eq!(
        fetcher().fetch(&server.url("/a")).unwrap_err(),
        FetchError::UnsupportedScheme("ftp".to_owned())
    );
}

#[test]
fn a_redirect_from_https_to_http_is_refused_and_the_others_are_not() {
    let https = Url::parse("https://example.com/a").unwrap();
    let http = Url::parse("http://example.com/a").unwrap();
    assert!(matches!(
        check_redirect(&https, &http),
        Err(FetchError::InsecureRedirect { .. })
    ));
    assert_eq!(check_redirect(&http, &https), Ok(()));
    assert_eq!(check_redirect(&http, &http), Ok(()));
    assert_eq!(check_redirect(&https, &https), Ok(()));
}

#[test]
fn a_declared_length_over_the_limit_is_refused_without_reading_the_body() {
    let server = Server::start(|_| Reply::ok("image/png", vec![0; 64 * 1024]));
    let fetcher = Fetcher::new(NetConfig {
        max_bytes: 1000,
        ..config()
    });
    assert_eq!(
        fetcher.fetch(&server.url("/big.png")).unwrap_err(),
        FetchError::TooLarge { limit: 1000 }
    );
}

#[test]
fn a_body_with_no_length_is_cut_off_at_the_limit_while_it_streams() {
    let server = Server::start(|_| {
        let mut reply = Reply::ok("image/png", vec![0x42; 16 * 1024]);
        reply.endless = true;
        reply
    });
    let fetcher = Fetcher::new(NetConfig {
        max_bytes: 100 * 1024,
        ..config()
    });
    let started = Instant::now();
    assert_eq!(
        fetcher.fetch(&server.url("/endless.png")).unwrap_err(),
        FetchError::TooLarge { limit: 100 * 1024 }
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_server_that_does_not_answer_in_time_is_a_timeout() {
    let server = Server::start(|_| Reply::ok("image/png", png()).delayed(Duration::from_secs(3)));
    let fetcher = Fetcher::new(NetConfig {
        stall_timeout: Duration::from_millis(300),
        ..config()
    });
    assert_eq!(
        fetcher.fetch(&server.url("/slow.png")).unwrap_err(),
        FetchError::Timeout
    );
}

#[test]
fn a_connection_that_is_refused_is_a_transport_error() {
    let closed = {
        let server = Server::start(|_| Reply::status(200));
        server.url("/a.png")
    };
    // Whatever the platform says about it, it is not a success and not a timeout.
    assert!(matches!(
        fetcher().fetch(&closed).unwrap_err(),
        FetchError::Transport(_)
    ));
}

#[test]
fn urls_that_are_not_requested_are_refused_before_anything_is_queued() {
    let fetcher = fetcher();
    let call = |url: &str| fetcher.submit(url, |_| unreachable!()).err().unwrap();
    assert!(matches!(
        call("ftp://example.com/a.png"),
        FetchError::UnsupportedScheme(scheme) if scheme == "ftp"
    ));
    assert!(matches!(call("not a url"), FetchError::InvalidUrl(_)));
    assert!(matches!(
        call("http://user:secret@example.com/a.png"),
        FetchError::InvalidUrl(_)
    ));
    assert!(matches!(call("http://"), FetchError::InvalidUrl(_)));
    assert!(idle(&fetcher));
}

#[test]
fn a_proxy_that_does_not_parse_fails_every_request_instead_of_going_direct() {
    let server = Server::start(|_| Reply::ok("image/png", png()));
    let fetcher = Fetcher::new(NetConfig {
        proxy: ProxyMode::Url("::not a proxy::".to_owned()),
        ..config()
    });
    let error = fetcher.fetch(&server.url("/a.png")).unwrap_err();
    assert!(matches!(&error, FetchError::Transport(reason) if reason.contains("proxy")));
    assert_eq!(server.hits(), 0);
}

#[test]
fn what_the_type_and_the_bytes_say_must_agree() {
    let png = png();
    let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>";
    let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 0, 0];
    let html = b"<!doctype html><title>404</title>";
    let ok = |content_type: Option<&str>, bytes: &[u8]| check_content(content_type, bytes);

    assert_eq!(ok(Some("image/png"), &png), Ok(()));
    assert_eq!(ok(Some("IMAGE/PNG; charset=binary"), &png), Ok(()));
    assert_eq!(ok(Some("image/jpg"), &jpeg), Ok(()));
    assert_eq!(ok(Some("image/svg+xml"), svg), Ok(()));
    // No type, or a generic one: the bytes decide.
    assert_eq!(ok(None, &png), Ok(()));
    assert_eq!(ok(Some("application/octet-stream"), &jpeg), Ok(()));
    assert_eq!(ok(Some("binary/octet-stream"), svg), Ok(()));

    assert_eq!(
        ok(Some("image/png"), &jpeg),
        Err(FetchError::ContentTypeMismatch {
            declared: "image/png".to_owned(),
            found: "JPEG"
        })
    );
    assert_eq!(
        ok(Some("image/svg+xml"), &png),
        Err(FetchError::ContentTypeMismatch {
            declared: "image/svg+xml".to_owned(),
            found: "PNG"
        })
    );
    // An error page that was served as a success.
    assert_eq!(
        ok(Some("text/html; charset=utf-8"), html),
        Err(FetchError::NotAnImage {
            content_type: "text/html".to_owned()
        })
    );
    // A real image under a type that is not one.
    assert_eq!(
        ok(Some("text/plain"), &png),
        Err(FetchError::NotAnImage {
            content_type: "text/plain".to_owned()
        })
    );
    // An image type with no decoder here.
    assert!(matches!(
        ok(Some("image/avif"), &png),
        Err(FetchError::NotAnImage { .. })
    ));
    // An error page served as `200` is no SVG, whatever the type says or leaves out.
    for declared in [None, Some("application/octet-stream")] {
        assert!(matches!(
            ok(declared, html),
            Err(FetchError::NotAnImage { .. })
        ));
    }
    assert!(matches!(
        ok(Some("image/svg+xml"), html),
        Err(FetchError::ContentTypeMismatch { .. })
    ));
    // Nothing says what it is.
    assert!(matches!(
        ok(None, b"hello"),
        Err(FetchError::NotAnImage { .. })
    ));
    assert!(matches!(
        ok(Some("application/octet-stream"), b"hello"),
        Err(FetchError::NotAnImage { .. })
    ));
}

#[test]
fn a_response_declared_as_something_else_is_refused_before_its_body_is_read() {
    // Endless, with no length: reading it would run into the size limit or the timeout.
    let server = Server::start(|_| {
        let mut reply = Reply::ok("text/html", vec![b'x'; 16 * 1024]);
        reply.endless = true;
        reply
    });
    let started = Instant::now();
    assert!(matches!(
        fetcher().fetch(&server.url("/page.png")).unwrap_err(),
        FetchError::NotAnImage { content_type } if content_type == "text/html"
    ));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    // An image type with no decoder here is refused as early.
    let server = Server::start(|_| Reply::ok("image/avif", vec![0; 10]));
    assert!(matches!(
        fetcher().fetch(&server.url("/a.avif")).unwrap_err(),
        FetchError::NotAnImage { .. }
    ));
}

#[test]
fn an_html_page_served_as_an_image_never_reaches_the_caller() {
    let server = Server::start(|_| Reply::ok("text/html", b"<html>nope</html>".to_vec()));
    assert!(matches!(
        fetcher().fetch(&server.url("/a.png")).unwrap_err(),
        FetchError::NotAnImage { .. }
    ));
}

mod proxies {
    use super::test_server::ConnectProxy;
    use super::*;

    #[test]
    fn a_proxy_that_is_given_carries_the_request_to_any_host() {
        let proxy = ConnectProxy::start();
        let origin = Server::start(|_| Reply::ok("image/png", png()));
        let fetcher = Fetcher::new(NetConfig {
            proxy: ProxyMode::Url(proxy.base.clone()),
            ..config()
        });
        let fetched = fetcher.fetch(&origin.url("/cover.png")).unwrap();
        assert_eq!(fetched.bytes, png());
        // The proxy was asked for the host, and the host got the request through it.
        assert_eq!(
            proxy.connects(),
            [origin.base.trim_start_matches("http://")]
        );
        assert_eq!(origin.seen(), ["/cover.png"]);
    }

    #[test]
    fn the_hosts_of_the_bypass_list_are_reached_directly() {
        let proxy = ConnectProxy::start();
        let origin = Server::start(|_| Reply::ok("image/png", png()));
        let fetcher = Fetcher::new(NetConfig {
            proxy: ProxyMode::Url(proxy.base.clone()),
            no_proxy: vec!["127.0.0.0/8".to_owned()],
            ..config()
        });
        assert_eq!(
            fetcher.fetch(&origin.url("/cover.png")).unwrap().bytes,
            png()
        );
        assert_eq!(origin.seen(), ["/cover.png"]);
        assert!(proxy.connects().is_empty());
    }

    #[test]
    fn a_redirect_to_a_host_on_the_bypass_list_leaves_the_proxy() {
        let direct = Server::start(|_| Reply::ok("image/png", png()));
        // The same server by another name, which is the one on the list.
        let target = direct.url("/final.png").replace("127.0.0.1", "localhost");
        let first = Server::start(move |_| Reply::status(302).header("Location", &target));
        let proxy = ConnectProxy::start();
        let fetcher = Fetcher::new(NetConfig {
            proxy: ProxyMode::Url(proxy.base.clone()),
            no_proxy: vec!["localhost".to_owned()],
            ..config()
        });
        let fetched = fetcher.fetch(&first.url("/a.png")).unwrap();
        assert_eq!(fetched.bytes, png());
        // Only the first hop went through the proxy.
        assert_eq!(proxy.connects(), [first.base.trim_start_matches("http://")]);
        assert_eq!(direct.seen(), ["/final.png"]);
    }

    #[test]
    fn a_socks_proxy_is_supported_by_this_build() {
        // Nothing listens there: the request fails, but not because SOCKS was left out.
        let fetcher = Fetcher::new(NetConfig {
            proxy: ProxyMode::Url("socks5://127.0.0.1:1".to_owned()),
            ..config()
        });
        let error = fetcher.fetch("http://example.invalid/a.png").unwrap_err();
        let FetchError::Transport(reason) = error else {
            panic!("a transport error, got {error:?}")
        };
        assert!(!reason.to_lowercase().contains("feature"), "{reason}");
    }

    #[test]
    fn a_proxy_that_cannot_be_used_fails_the_request_and_says_why() {
        let fetcher = Fetcher::new(NetConfig {
            proxy: ProxyMode::Url("::not a proxy::".to_owned()),
            ..config()
        });
        let error = fetcher.fetch("http://127.0.0.1:9/a.png").unwrap_err();
        assert!(matches!(error, FetchError::Transport(reason) if reason.contains("not usable")));
    }
}

mod private_addresses {
    use super::test_server::ConnectProxy;
    use super::*;

    fn blocking() -> NetConfig {
        NetConfig {
            block_private_addresses: true,
            ..config()
        }
    }

    #[test]
    fn a_picture_on_this_machine_is_not_requested_when_private_addresses_are_blocked() {
        let server = Server::start(|_| Reply::ok("image/png", png()));
        let fetcher = Fetcher::new(blocking());
        // By address and by a name that resolves to it.
        for url in [
            server.url("/a.png"),
            server.url("/a.png").replace("127.0.0.1", "localhost"),
        ] {
            assert_eq!(fetcher.fetch(&url).unwrap_err(), FetchError::BlockedAddress);
        }
        assert_eq!(server.hits(), 0, "nothing reached it");
    }

    #[test]
    fn private_addresses_are_requested_by_default() {
        let server = Server::start(|_| Reply::ok("image/png", png()));
        assert!(fetcher().fetch(&server.url("/a.png")).is_ok());
    }

    #[test]
    fn a_proxy_is_trusted_with_where_the_request_goes() {
        let proxy = ConnectProxy::start();
        let server = Server::start(|_| Reply::ok("image/png", png()));
        let fetcher = Fetcher::new(NetConfig {
            proxy: ProxyMode::Url(proxy.base.clone()),
            block_private_addresses: true,
            ..config()
        });
        assert!(fetcher.fetch(&server.url("/a.png")).is_ok());
    }
}

mod compression {
    use std::io::Write as _;

    use super::*;

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    fn brotli(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = brotli::CompressorWriter::new(&mut out, 4096, 5, 20);
            encoder.write_all(bytes).unwrap();
        }
        out
    }

    #[test]
    fn the_request_offers_the_encodings_it_can_decode() {
        let (sender, receiver) = mpsc::channel();
        let sender = std::sync::Mutex::new(sender);
        let server = Server::start(move |request| {
            sender
                .lock()
                .unwrap()
                .send(request.headers.get("accept-encoding").cloned())
                .unwrap();
            Reply::ok("image/png", png())
        });
        fetcher().fetch(&server.url("/a.png")).unwrap();
        let offered = receiver.recv().unwrap().expect("an Accept-Encoding header");
        assert!(
            offered.contains("gzip") && offered.contains("br"),
            "{offered}"
        );
    }

    #[test]
    fn a_gzip_response_is_decoded_before_the_content_check() {
        let server = Server::start(|_| {
            Reply::ok("image/png", gzip(&png())).header("Content-Encoding", "gzip")
        });
        let fetched = fetcher().fetch(&server.url("/a.png")).unwrap();
        assert_eq!(fetched.bytes, png());
    }

    #[test]
    fn a_brotli_response_is_decoded_before_the_content_check() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"/>"#.to_vec();
        let body = brotli(&svg);
        let server = Server::start(move |_| {
            Reply::ok("image/svg+xml", body.clone()).header("Content-Encoding", "br")
        });
        let fetched = fetcher().fetch(&server.url("/a.svg")).unwrap();
        assert_eq!(fetched.bytes, svg);
    }

    #[test]
    fn the_size_limit_counts_what_the_body_decodes_to_not_what_it_weighs() {
        // A few hundred bytes on the wire that grow to 8 MiB: a decompression bomb.
        let body = gzip(&vec![0u8; 8 * 1024 * 1024]);
        assert!(body.len() < 64 * 1024);
        let server = Server::start(move |_| {
            Reply::ok("image/png", body.clone()).header("Content-Encoding", "gzip")
        });
        let fetcher = Fetcher::new(NetConfig {
            max_bytes: 1024 * 1024,
            ..config()
        });
        assert_eq!(
            fetcher.fetch(&server.url("/bomb.png")).unwrap_err(),
            FetchError::TooLarge { limit: 1024 * 1024 }
        );
    }

    #[test]
    fn what_is_stored_on_disk_is_the_decoded_body() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(|_| {
            Reply::ok("image/png", gzip(&png()))
                .header("Content-Encoding", "gzip")
                .header("Cache-Control", "max-age=3600")
        });
        let url = server.url("/a.png");
        let fetcher = Fetcher::new(NetConfig {
            cache_dir: Some(dir.path().to_owned()),
            ..config()
        });
        fetcher.fetch(&url).unwrap();
        let again = Fetcher::new(NetConfig {
            cache_dir: Some(dir.path().to_owned()),
            ..config()
        })
        .fetch(&url)
        .unwrap();
        assert_eq!((again.origin, again.bytes.clone()), (Origin::Cache, png()));
    }
}

mod caching {
    use super::*;

    fn cached_config(dir: &tempfile::TempDir) -> NetConfig {
        NetConfig {
            cache_dir: Some(dir.path().to_owned()),
            ..config()
        }
    }

    #[test]
    fn a_fresh_response_is_not_asked_for_again_not_even_by_a_later_run() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(|_| {
            Reply::ok("image/png", png()).header("Cache-Control", "public, max-age=3600")
        });
        let url = server.url("/cover.png");
        let first = Fetcher::new(cached_config(&dir));
        assert_eq!(first.fetch(&url).unwrap().origin, Origin::Network);
        assert_eq!(first.fetch(&url).unwrap().origin, Origin::Cache);
        // A new fetcher stands for the next launch.
        let second = Fetcher::new(cached_config(&dir));
        let again = second.fetch(&url).unwrap();
        assert_eq!((again.origin, again.bytes.clone()), (Origin::Cache, png()));
        assert_eq!(again.content_type.as_deref(), Some("image/png"));
        assert_eq!(server.hits(), 1);
        assert_eq!(second.stats().cache_hits, 1);
        assert_eq!(second.stats().requests, 0);
    }

    #[test]
    fn a_stale_response_is_confirmed_with_its_etag_and_the_body_does_not_travel_again() {
        let dir = tempfile::tempdir().unwrap();
        let (sender, receiver) = mpsc::channel();
        let sender = std::sync::Mutex::new(sender);
        let server = Server::start(move |request| {
            sender
                .lock()
                .unwrap()
                .send(request.headers.get("if-none-match").cloned())
                .unwrap();
            if request.headers.get("if-none-match").map(String::as_str) == Some("\"v1\"") {
                Reply::status(304)
                    .header("ETag", "\"v1\"")
                    .header("Cache-Control", "max-age=0")
            } else {
                Reply::ok("image/png", png())
                    .header("ETag", "\"v1\"")
                    .header("Cache-Control", "max-age=0")
            }
        });
        let url = server.url("/cover.png");
        let fetcher = Fetcher::new(cached_config(&dir));
        assert_eq!(fetcher.fetch(&url).unwrap().origin, Origin::Network);
        let renewed = fetcher.fetch(&url).unwrap();
        assert_eq!(
            (renewed.origin, renewed.bytes.clone()),
            (Origin::Revalidated, png())
        );
        assert_eq!(receiver.recv().unwrap(), None);
        assert_eq!(receiver.recv().unwrap().as_deref(), Some("\"v1\""));
        assert_eq!(fetcher.stats().revalidated, 1);
    }

    #[test]
    fn a_stale_response_is_confirmed_with_its_modification_date_when_it_has_no_etag() {
        let dir = tempfile::tempdir().unwrap();
        let date = "Sun, 06 Nov 1994 08:49:37 GMT";
        let server = Server::start(move |request| {
            if request.headers.get("if-modified-since").map(String::as_str) == Some(date) {
                Reply::status(304)
            } else {
                Reply::ok("image/png", png())
                    .header("Last-Modified", date)
                    .header("Cache-Control", "no-cache")
            }
        });
        let url = server.url("/cover.png");
        let fetcher = Fetcher::new(cached_config(&dir));
        fetcher.fetch(&url).unwrap();
        assert_eq!(fetcher.fetch(&url).unwrap().origin, Origin::Revalidated);
    }

    #[test]
    fn a_304_that_says_nothing_about_caching_keeps_the_lifetime_of_the_stored_response() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(|request| {
            if request.headers.contains_key("if-none-match") {
                // Only the validator, as many servers and CDNs answer.
                Reply::status(304).header("ETag", "\"v1\"")
            } else {
                Reply::ok("image/png", png())
                    .header("ETag", "\"v1\"")
                    .header("Cache-Control", "max-age=3600, stale-if-error=600")
            }
        });
        let url = server.url("/cover.png");
        let fetcher = Fetcher::new(cached_config(&dir));
        fetcher.fetch(&url).unwrap();
        // Two hours later: stale.
        let disk = fetcher.inner.disk.as_ref().unwrap();
        let (mut entry, body) = disk.lookup(&url, unix_now()).unwrap();
        entry.stored_at -= 7200;
        disk.store(&entry, &body);
        assert_eq!(fetcher.fetch(&url).unwrap().origin, Origin::Revalidated);
        let (renewed, _) = disk.lookup(&url, unix_now()).unwrap();
        assert_eq!(
            (renewed.fresh_for, renewed.stale_if_error),
            (3600, 600),
            "the lifetime is the stored one"
        );
        assert!(renewed.is_fresh(unix_now()));
        // Fresh again: no request.
        assert_eq!(fetcher.fetch(&url).unwrap().origin, Origin::Cache);
    }

    #[test]
    fn a_304_keeps_each_part_of_the_lifetime_it_does_not_set() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(|request| {
            if request.headers.contains_key("if-none-match") {
                // Sets nothing about the lifetime or the windows.
                Reply::status(304)
                    .header("ETag", "\"v1\"")
                    .header("Cache-Control", "public")
            } else {
                Reply::ok("image/png", png())
                    .header("ETag", "\"v1\"")
                    .header(
                        "Cache-Control",
                        "max-age=3600, stale-while-revalidate=86400, stale-if-error=600",
                    )
            }
        });
        let url = server.url("/cover.png");
        let fetcher = Fetcher::new(cached_config(&dir));
        fetcher.fetch(&url).unwrap();
        let disk = fetcher.inner.disk.as_ref().unwrap();
        let (mut entry, body) = disk.lookup(&url, unix_now()).unwrap();
        // Past the lifetime and the window in which it is used while it is asked again.
        entry.stored_at -= 100_000;
        disk.store(&entry, &body);
        assert_eq!(fetcher.fetch(&url).unwrap().origin, Origin::Revalidated);
        let (renewed, _) = disk.lookup(&url, unix_now()).unwrap();
        assert_eq!(
            (
                renewed.fresh_for,
                renewed.stale_while_revalidate,
                renewed.stale_if_error
            ),
            (3600, 86400, 600)
        );
    }

    #[test]
    fn a_304_that_sets_a_new_max_age_replaces_only_that() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(|request| {
            if request.headers.contains_key("if-none-match") {
                Reply::status(304)
                    .header("ETag", "\"v1\"")
                    .header("Cache-Control", "max-age=60")
            } else {
                Reply::ok("image/png", png())
                    .header("ETag", "\"v1\"")
                    .header("Cache-Control", "max-age=3600, stale-if-error=600")
            }
        });
        let url = server.url("/cover.png");
        let fetcher = Fetcher::new(cached_config(&dir));
        fetcher.fetch(&url).unwrap();
        let disk = fetcher.inner.disk.as_ref().unwrap();
        let (mut entry, body) = disk.lookup(&url, unix_now()).unwrap();
        entry.stored_at -= 7200;
        disk.store(&entry, &body);
        fetcher.fetch(&url).unwrap();
        let (renewed, _) = disk.lookup(&url, unix_now()).unwrap();
        assert_eq!((renewed.fresh_for, renewed.stale_if_error), (60, 600));
    }

    #[test]
    fn a_changed_image_replaces_the_stored_one() {
        let dir = tempfile::tempdir().unwrap();
        let version = Arc::new(std::sync::atomic::AtomicUsize::new(1));
        let seen = Arc::clone(&version);
        let server = Server::start(move |_| {
            let n = seen.load(Ordering::SeqCst);
            Reply::ok("image/png", png())
                .header("ETag", &format!("\"v{n}\""))
                .header("Cache-Control", "max-age=0")
        });
        let url = server.url("/cover.png");
        let fetcher = Fetcher::new(cached_config(&dir));
        fetcher.fetch(&url).unwrap();
        version.store(2, Ordering::SeqCst);
        // The server ignores the validator and sends the whole image again.
        assert_eq!(fetcher.fetch(&url).unwrap().origin, Origin::Network);
        let (stored, _) = fetcher
            .inner
            .disk
            .as_ref()
            .unwrap()
            .lookup(&url, unix_now())
            .unwrap();
        assert_eq!(stored.etag.as_deref(), Some("\"v2\""));
    }

    #[test]
    fn no_store_and_unvalidatable_responses_are_asked_for_every_time() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(|request| match request.path.as_str() {
            "/private" => Reply::ok("image/png", png()).header("Cache-Control", "no-store"),
            _ => Reply::ok("image/png", png()),
        });
        let fetcher = Fetcher::new(cached_config(&dir));
        for path in ["/private", "/bare"] {
            assert_eq!(
                fetcher.fetch(&server.url(path)).unwrap().origin,
                Origin::Network
            );
            assert_eq!(
                fetcher.fetch(&server.url(path)).unwrap().origin,
                Origin::Network
            );
        }
        assert_eq!(server.hits(), 4);
        assert!(std::fs::read_dir(dir.path()).map_or(true, |mut files| files.next().is_none()));
    }

    #[test]
    fn a_stale_response_the_server_cannot_confirm_is_an_error_not_the_old_image() {
        let dir = tempfile::tempdir().unwrap();
        let up = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let flag = Arc::clone(&up);
        let server = Server::start(move |_| {
            if flag.load(Ordering::SeqCst) {
                Reply::ok("image/png", png())
                    .header("ETag", "\"a\"")
                    .header("Cache-Control", "max-age=0")
            } else {
                Reply::status(500)
            }
        });
        let url = server.url("/cover.png");
        let fetcher = Fetcher::new(cached_config(&dir));
        fetcher.fetch(&url).unwrap();
        up.store(false, Ordering::SeqCst);
        assert_eq!(fetcher.fetch(&url).unwrap_err(), FetchError::Status(500));
    }

    #[test]
    fn a_response_rejected_by_the_content_check_is_not_stored() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(|_| {
            Reply::ok("text/html", b"<html/>".to_vec()).header("Cache-Control", "max-age=3600")
        });
        let fetcher = Fetcher::new(cached_config(&dir));
        assert!(fetcher.fetch(&server.url("/a.png")).is_err());
        assert!(std::fs::read_dir(dir.path()).map_or(true, |mut files| files.next().is_none()));
    }
}

mod staleness {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    fn cached_config(dir: &tempfile::TempDir) -> NetConfig {
        NetConfig {
            cache_dir: Some(dir.path().to_owned()),
            ..config()
        }
    }

    /// A server that answers `200` with the picture first and then whatever `then` says.
    fn first_ok_then(
        control: &'static str,
        then: impl Fn(usize) -> Reply + Send + Sync + 'static,
    ) -> Server {
        let hits = AtomicUsize::new(0);
        Server::start(move |_| {
            let n = hits.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Reply::ok("image/png", png())
                    .header("ETag", "\"v1\"")
                    .header("Cache-Control", control)
            } else {
                then(n)
            }
        })
    }

    #[test]
    fn a_stale_copy_stands_in_when_the_server_fails_within_stale_if_error() {
        let dir = tempfile::tempdir().unwrap();
        let server = first_ok_then("max-age=0, stale-if-error=600", |_| Reply::status(503));
        let url = server.url("/cover.png");
        let fetcher = Fetcher::new(cached_config(&dir));
        assert_eq!(fetcher.fetch(&url).unwrap().origin, Origin::Network);
        let served = fetcher.fetch(&url).unwrap();
        assert_eq!(
            (served.origin, served.bytes.clone()),
            (Origin::StaleOnError, png())
        );
        assert_eq!(server.hits(), 2);
    }

    #[test]
    fn a_stale_copy_stands_in_when_the_network_fails_too() {
        let dir = tempfile::tempdir().unwrap();
        let url = {
            let server = first_ok_then("max-age=0, stale-if-error=600", |_| Reply::status(503));
            let url = server.url("/cover.png");
            Fetcher::new(cached_config(&dir)).fetch(&url).unwrap();
            url
            // The server goes away here.
        };
        let served = Fetcher::new(cached_config(&dir)).fetch(&url).unwrap();
        assert_eq!(served.origin, Origin::StaleOnError);
    }

    #[test]
    fn a_stale_copy_does_not_stand_in_for_a_picture_the_server_says_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let server = first_ok_then("max-age=0, stale-if-error=600", |_| Reply::status(404));
        let url = server.url("/cover.png");
        let fetcher = Fetcher::new(cached_config(&dir));
        fetcher.fetch(&url).unwrap();
        assert_eq!(fetcher.fetch(&url).unwrap_err(), FetchError::Status(404));
    }

    #[test]
    fn a_stale_copy_does_not_stand_in_past_its_window_or_without_one() {
        let dir = tempfile::tempdir().unwrap();
        for (control, age) in [
            ("max-age=0, stale-if-error=60", 3600),
            ("max-age=0, must-revalidate, stale-if-error=600", 0),
            ("max-age=0", 0),
        ] {
            let server = first_ok_then(control, |_| Reply::status(503));
            let url = server.url("/cover.png");
            let fetcher = Fetcher::new(cached_config(&dir));
            fetcher.fetch(&url).unwrap();
            // Makes the copy older, as the passing of time would.
            let disk = fetcher.inner.disk.as_ref().unwrap();
            let (mut entry, body) = disk.lookup(&url, unix_now()).unwrap();
            entry.stored_at -= age;
            disk.store(&entry, &body);
            assert_eq!(
                fetcher.fetch(&url).unwrap_err(),
                FetchError::Status(503),
                "{control} at {age} s"
            );
        }
    }

    #[test]
    fn a_stale_copy_is_used_at_once_and_the_fresh_one_is_fetched_behind_it() {
        let dir = tempfile::tempdir().unwrap();
        let new_png = {
            let image = image::RgbaImage::from_pixel(5, 5, image::Rgba([1, 2, 3, 255]));
            let mut bytes = Vec::new();
            image::DynamicImage::ImageRgba8(image)
                .write_to(
                    &mut std::io::Cursor::new(&mut bytes),
                    image::ImageFormat::Png,
                )
                .unwrap();
            bytes
        };
        let reply_png = new_png.clone();
        let server = first_ok_then("max-age=0, stale-while-revalidate=600", move |_| {
            Reply::ok("image/png", reply_png.clone())
                .header("ETag", "\"v2\"")
                .header("Cache-Control", "max-age=0, stale-while-revalidate=600")
                .delayed(Duration::from_millis(400))
        });
        let url = server.url("/cover.png");
        let fetcher = Fetcher::new(cached_config(&dir));
        assert_eq!(fetcher.fetch(&url).unwrap().origin, Origin::Network);

        let asked = Instant::now();
        let stale = fetcher.fetch(&url).unwrap();
        assert_eq!((stale.origin, stale.bytes.clone()), (Origin::Stale, png()));
        assert!(
            asked.elapsed() < Duration::from_millis(300),
            "it did not wait"
        );

        // While the revalidation is on its way, another ask is not made to wait for it.
        assert!(wait_until(|| server.hits() == 2));
        let asked = Instant::now();
        assert_eq!(fetcher.fetch(&url).unwrap().origin, Origin::Stale);
        assert!(asked.elapsed() < Duration::from_millis(300));

        // The revalidation lands, and the next ask has the new picture.
        let disk = fetcher.inner.disk.as_ref().unwrap();
        assert!(wait_until(|| {
            disk.lookup(&url, unix_now())
                .is_some_and(|(_, body)| body == new_png)
        }));
        assert_eq!(fetcher.fetch(&url).unwrap().bytes, new_png);
        // One revalidation per stale answer that started one, not a loop: the three
        // stale answers started at most two (the second joined the first), and the last
        // one is still running or done.
        assert!(wait_until(|| idle(&fetcher)));
        assert!(server.hits() <= 4, "{} requests", server.hits());
    }

    #[test]
    fn a_failed_revalidation_leaves_the_stale_copy_for_the_next_ask() {
        let dir = tempfile::tempdir().unwrap();
        let server = first_ok_then("max-age=0, stale-while-revalidate=600", |_| {
            Reply::status(500)
        });
        let url = server.url("/cover.png");
        let fetcher = Fetcher::new(cached_config(&dir));
        fetcher.fetch(&url).unwrap();
        assert_eq!(fetcher.fetch(&url).unwrap().origin, Origin::Stale);
        assert!(wait_until(|| server.hits() == 2 && idle(&fetcher)));
        let again = fetcher.fetch(&url).unwrap();
        assert_eq!((again.origin, again.bytes.clone()), (Origin::Stale, png()));
    }
}

mod robustness {
    use super::*;

    #[test]
    fn a_callback_that_panics_does_not_take_the_worker_down() {
        let server = Server::start(|_| Reply::ok("image/png", png()));
        let fetcher = Fetcher::new(NetConfig {
            max_concurrent: 1,
            ..config()
        });
        let _ticket = fetcher
            .submit(&server.url("/a.png"), |_| {
                panic!("the caller's callback panicked")
            })
            .unwrap();
        // With one worker, the next request is served only if it survived.
        assert!(fetcher.fetch(&server.url("/b.png")).is_ok());
        assert!(wait_until(|| idle(&fetcher)));
    }

    #[test]
    fn the_whole_request_has_one_time_budget_across_redirects() {
        // Every hop answers after 300 ms and sends the client on: five hops are 1.5 s, and
        // the budget for all of them is 700 ms.
        let server = Server::start(|request| {
            let n: u32 = request.path.trim_start_matches('/').parse().unwrap_or(0);
            Reply::status(302)
                .header("Location", &format!("/{}", n + 1))
                .delayed(Duration::from_millis(300))
        });
        let fetcher = Fetcher::new(NetConfig {
            total_timeout: Duration::from_millis(700),
            max_redirects: 10,
            ..config()
        });
        let started = Instant::now();
        assert_eq!(
            fetcher.fetch(&server.url("/0")).unwrap_err(),
            FetchError::Timeout
        );
        assert!(
            started.elapsed() < Duration::from_millis(1400),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn the_validators_go_to_the_stored_address_and_not_on_to_where_it_redirects() {
        let dir = tempfile::tempdir().unwrap();
        let (sender, receiver) = mpsc::channel();
        let sender = std::sync::Mutex::new(sender);
        let stale = std::sync::atomic::AtomicBool::new(false);
        let server = Server::start(move |request| {
            sender
                .lock()
                .unwrap()
                .send((
                    request.path.clone(),
                    request.headers.get("if-none-match").cloned(),
                ))
                .unwrap();
            match request.path.as_str() {
                "/old.png" if stale.swap(true, Ordering::SeqCst) => {
                    Reply::status(302).header("Location", "/final.png")
                }
                "/old.png" => Reply::ok("image/png", png())
                    .header("ETag", "\"old\"")
                    .header("Cache-Control", "max-age=0"),
                _ => Reply::ok("image/png", png()).header("ETag", "\"other\""),
            }
        });
        let fetcher = Fetcher::new(NetConfig {
            cache_dir: Some(dir.path().to_owned()),
            ..config()
        });
        let url = server.url("/old.png");
        fetcher.fetch(&url).unwrap();
        fetcher.fetch(&url).unwrap();
        let sent: Vec<_> = receiver.try_iter().collect();
        assert_eq!(
            sent,
            [
                ("/old.png".to_owned(), None),
                ("/old.png".to_owned(), Some("\"old\"".to_owned())),
                ("/final.png".to_owned(), None),
            ]
        );
    }
}

mod redirected_revalidation {
    use super::*;

    #[test]
    fn a_304_from_a_later_hop_does_not_renew_the_stored_copy() {
        let dir = tempfile::tempdir().unwrap();
        let stale = std::sync::atomic::AtomicBool::new(false);
        let server = Server::start(move |request| match request.path.as_str() {
            // First an image with a validator; later the same address redirects, to a
            // server that wrongly answers 304 to a request that was not conditional.
            "/old.png" if stale.swap(true, Ordering::SeqCst) => {
                Reply::status(302).header("Location", "/elsewhere.png")
            }
            "/old.png" => Reply::ok("image/png", png())
                .header("ETag", "\"old\"")
                .header("Cache-Control", "max-age=0"),
            _ => Reply::status(304),
        });
        let fetcher = Fetcher::new(NetConfig {
            cache_dir: Some(dir.path().to_owned()),
            ..config()
        });
        let url = server.url("/old.png");
        fetcher.fetch(&url).unwrap();
        assert!(matches!(
            fetcher.fetch(&url).unwrap_err(),
            FetchError::Transport(reason) if reason.contains("304")
        ));
    }
}

mod unconditional {
    use super::*;

    #[test]
    fn a_304_to_a_request_that_sent_no_validators_does_not_renew_the_stored_copy() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(|request| {
            if request.path == "/cover.png" && !request.headers.contains_key("x-second") {
                Reply::ok("image/png", png()).header("Cache-Control", "max-age=60")
            } else {
                Reply::status(304)
            }
        });
        let url = server.url("/cover.png");
        let fetcher = Fetcher::new(NetConfig {
            cache_dir: Some(dir.path().to_owned()),
            ..config()
        });
        fetcher.fetch(&url).unwrap();
        // Stale, and with nothing to ask the server to confirm it with.
        let disk = fetcher.inner.disk.as_ref().unwrap();
        let (mut entry, body) = disk.lookup(&url, unix_now()).unwrap();
        assert!(entry.etag.is_none() && entry.last_modified.is_none());
        entry.stored_at -= 3600;
        disk.store(&entry, &body);
        // The server now answers 304 to everything, which is not an answer to anything.
        let server_that_always_304s = Server::start(|_| Reply::status(304));
        let moved = server_that_always_304s.url("/cover.png");
        let (mut again, body) = disk.lookup(&url, unix_now()).unwrap();
        again.url = moved.clone();
        disk.store(&again, &body);
        let error = fetcher.fetch(&moved).unwrap_err();
        assert!(matches!(error, FetchError::Transport(reason) if reason.contains("304")));
    }
}

mod separate_lines {
    use super::*;

    #[test]
    fn a_header_sent_as_several_lines_is_one_list() {
        let dir = tempfile::tempdir().unwrap();
        for (first, second, name) in [
            ("max-age=600", "no-store", "Cache-Control"),
            ("max-age=600", "private, no-store", "Cache-Control"),
        ] {
            let server = Server::start(move |_| {
                Reply::ok("image/png", png())
                    .header(name, first)
                    .header(name, second)
            });
            let url = server.url("/cover.png");
            let fetcher = Fetcher::new(NetConfig {
                cache_dir: Some(dir.path().to_owned()),
                ..config()
            });
            assert_eq!(fetcher.fetch(&url).unwrap().origin, Origin::Network);
            assert_eq!(
                fetcher.fetch(&url).unwrap().origin,
                Origin::Network,
                "{first} and {second} are both in force"
            );
        }
        let server = Server::start(|_| {
            Reply::ok("image/png", png())
                .header("Cache-Control", "max-age=600")
                .header("Vary", "Accept-Encoding")
                .header("Vary", "*")
        });
        let url = server.url("/varies.png");
        let fetcher = Fetcher::new(NetConfig {
            cache_dir: Some(dir.path().to_owned()),
            ..config()
        });
        fetcher.fetch(&url).unwrap();
        assert_eq!(fetcher.fetch(&url).unwrap().origin, Origin::Network);
    }
}

mod dropping {
    use super::*;

    /// Calls back into the fetcher when it is dropped, as a task woken by the drop of a
    /// callback might.
    struct UsesTheFetcherWhenDropped(Fetcher, mpsc::Sender<()>);

    impl Drop for UsesTheFetcherWhenDropped {
        fn drop(&mut self) {
            let ticket = self.0.submit("http://127.0.0.1:1/never.png", |_| {});
            drop(ticket);
            let _ = self.1.send(());
        }
    }

    #[test]
    fn letting_go_of_a_request_drops_its_callback_outside_the_locks() {
        let server =
            Server::start(|_| Reply::ok("image/png", png()).delayed(Duration::from_millis(300)));
        let fetcher = fetcher();
        let (done, finished) = mpsc::channel();
        let guard = UsesTheFetcherWhenDropped(fetcher.clone(), done);
        let ticket = fetcher
            .submit(&server.url("/a.png"), move |_| drop(guard))
            .unwrap();
        // Dropping the ticket drops the callback, and with it the guard, which uses the
        // fetcher: with the queue locked that would wait for itself.
        let letting_go = std::thread::spawn(move || drop(ticket));
        assert!(
            finished.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the callback was dropped while the queue was locked"
        );
        letting_go.join().unwrap();
    }
}

mod scheduling {
    use super::*;

    fn slow(gauge: &Arc<Gauge>) -> Server {
        Server::start_counting(Arc::clone(gauge), |_| {
            Reply::ok("image/png", png()).delayed(Duration::from_millis(150))
        })
    }

    #[test]
    fn requests_to_one_host_run_at_most_per_host_at_once_and_all_finish() {
        let gauge = Arc::new(Gauge::default());
        let server = slow(&gauge);
        let fetcher = Fetcher::new(NetConfig {
            max_concurrent: 8,
            max_per_host: 2,
            ..config()
        });
        let (sender, receiver) = mpsc::channel();
        let tickets: Vec<_> = (0..8)
            .map(|n| {
                let sender = sender.clone();
                fetcher
                    .submit(&server.url(&format!("/{n}.png")), move |outcome| {
                        sender.send(outcome.is_ok()).unwrap();
                    })
                    .unwrap()
            })
            .collect();
        for _ in 0..8 {
            assert!(receiver.recv_timeout(Duration::from_secs(10)).unwrap());
        }
        drop(tickets);
        assert_eq!(gauge.peak.load(Ordering::SeqCst), 2);
        assert!(wait_until(|| idle(&fetcher)));
    }

    #[test]
    fn requests_to_several_hosts_run_at_most_max_concurrent_at_once() {
        let gauge = Arc::new(Gauge::default());
        let servers: Vec<_> = (0..3).map(|_| slow(&gauge)).collect();
        let fetcher = Fetcher::new(NetConfig {
            max_concurrent: 3,
            max_per_host: 2,
            ..config()
        });
        let (sender, receiver) = mpsc::channel();
        let mut tickets = Vec::new();
        for server in &servers {
            for n in 0..4 {
                let sender = sender.clone();
                tickets.push(
                    fetcher
                        .submit(&server.url(&format!("/{n}.png")), move |outcome| {
                            sender.send(outcome.is_ok()).unwrap();
                        })
                        .unwrap(),
                );
            }
        }
        for _ in 0..12 {
            assert!(receiver.recv_timeout(Duration::from_secs(20)).unwrap());
        }
        assert_eq!(gauge.peak.load(Ordering::SeqCst), 3);
        assert!(wait_until(|| idle(&fetcher)));
    }

    #[test]
    fn a_host_at_its_limit_does_not_hold_up_another_one() {
        let gauge = Arc::new(Gauge::default());
        let busy = slow(&gauge);
        let other = Server::start(|_| Reply::ok("image/png", png()));
        let fetcher = Fetcher::new(NetConfig {
            max_concurrent: 2,
            max_per_host: 1,
            ..config()
        });
        let _first = fetcher.submit(&busy.url("/1.png"), |_| {}).unwrap();
        let _second = fetcher.submit(&busy.url("/2.png"), |_| {}).unwrap();
        // Two workers exist and the second request of the busy host is queued; the other
        // host is served by the second worker without waiting for it.
        let started = Instant::now();
        assert!(fetcher.fetch(&other.url("/x.png")).is_ok());
        assert!(started.elapsed() < Duration::from_millis(140));
    }

    #[test]
    fn asking_for_a_url_that_is_in_flight_joins_it() {
        let server =
            Server::start(|_| Reply::ok("image/png", png()).delayed(Duration::from_millis(200)));
        let fetcher = fetcher();
        let (sender, receiver) = mpsc::channel();
        let _tickets: Vec<_> = (0..5)
            .map(|n| {
                let sender = sender.clone();
                // Only the fragment differs.
                fetcher
                    .submit(&server.url(&format!("/a.png#{n}")), move |outcome| {
                        sender.send(outcome.map(|f| f.bytes.len())).unwrap();
                    })
                    .unwrap()
            })
            .collect();
        for _ in 0..5 {
            assert_eq!(
                receiver.recv_timeout(Duration::from_secs(10)).unwrap(),
                Ok(png().len())
            );
        }
        assert_eq!(server.hits(), 1);
    }

    #[test]
    fn a_request_nobody_waits_for_leaves_the_queue_and_is_never_sent() {
        let gauge = Arc::new(Gauge::default());
        let server = slow(&gauge);
        let fetcher = Fetcher::new(NetConfig {
            max_concurrent: 1,
            ..config()
        });
        let (sender, receiver) = mpsc::channel();
        let first = {
            let sender = sender.clone();
            fetcher
                .submit(&server.url("/first.png"), move |outcome| {
                    sender.send(("first", outcome.is_ok())).unwrap();
                })
                .unwrap()
        };
        let dropped = fetcher
            .submit(&server.url("/dropped.png"), |_| panic!("it was cancelled"))
            .unwrap();
        let kept = {
            let sender = sender.clone();
            fetcher
                .submit(&server.url("/kept.png"), move |outcome| {
                    sender.send(("kept", outcome.is_ok())).unwrap();
                })
                .unwrap()
        };
        drop(dropped);
        let mut done = vec![
            receiver.recv_timeout(Duration::from_secs(10)).unwrap(),
            receiver.recv_timeout(Duration::from_secs(10)).unwrap(),
        ];
        done.sort();
        assert_eq!(done, [("first", true), ("kept", true)]);
        assert_eq!(server.seen(), ["/first.png", "/kept.png"]);
        drop((first, kept));
        assert!(wait_until(|| idle(&fetcher)));
    }

    #[test]
    fn one_waiter_letting_go_does_not_cancel_a_request_that_others_wait_for() {
        let server =
            Server::start(|_| Reply::ok("image/png", png()).delayed(Duration::from_millis(200)));
        let fetcher = fetcher();
        let (sender, receiver) = mpsc::channel();
        let stays = fetcher
            .submit(&server.url("/a.png"), move |outcome| {
                sender.send(outcome.is_ok()).unwrap();
            })
            .unwrap();
        let leaves = fetcher
            .submit(&server.url("/a.png"), |_| panic!("it let go"))
            .unwrap();
        drop(leaves);
        assert!(receiver.recv_timeout(Duration::from_secs(10)).unwrap());
        drop(stays);
    }

    #[test]
    fn a_running_request_nobody_waits_for_stops_reading() {
        let server = Server::start(|_| {
            let mut reply = Reply::ok("image/png", vec![0x42; 16 * 1024]);
            reply.endless = true;
            reply
        });
        let fetcher = Fetcher::new(NetConfig {
            max_bytes: u64::MAX,
            ..config()
        });
        let ticket = fetcher
            .submit(&server.url("/endless.png"), |_| panic!("it was cancelled"))
            .unwrap();
        assert!(wait_until(
            || server.gauge.active.load(Ordering::SeqCst) == 1
        ));
        drop(ticket);
        // The worker leaves, and with it the connection, which ends the server's side.
        assert!(wait_until(|| idle(&fetcher)));
        assert!(wait_until(
            || server.gauge.active.load(Ordering::SeqCst) == 0
        ));
    }

    #[test]
    fn asking_again_just_after_letting_go_gets_an_answer_not_a_cancellation() {
        let server =
            Server::start(|_| Reply::ok("image/png", png()).delayed(Duration::from_millis(300)));
        let fetcher = fetcher();
        let ticket = fetcher.submit(&server.url("/a.png"), |_| {}).unwrap();
        // Running by now.
        assert!(wait_until(
            || server.gauge.active.load(Ordering::SeqCst) == 1
        ));
        drop(ticket);
        let again = fetcher.fetch(&server.url("/a.png")).unwrap();
        assert_eq!(again.bytes, png());
        assert_eq!(server.hits(), 1);
    }

    #[test]
    fn a_failure_reaches_every_waiter() {
        let server = Server::start(|_| Reply::status(500).delayed(Duration::from_millis(100)));
        let fetcher = fetcher();
        let (sender, receiver) = mpsc::channel();
        let _tickets: Vec<_> = (0..3)
            .map(|_| {
                let sender = sender.clone();
                fetcher
                    .submit(&server.url("/a.png"), move |outcome| {
                        sender.send(outcome.unwrap_err()).unwrap();
                    })
                    .unwrap()
            })
            .collect();
        for _ in 0..3 {
            assert_eq!(
                receiver.recv_timeout(Duration::from_secs(10)).unwrap(),
                FetchError::Status(500)
            );
        }
        assert_eq!(server.hits(), 1);
    }
}

/// Reaches a real HTTPS host through the platform's certificate store and the proxy
/// settings of the environment. Needs a network, so it only runs when asked for:
/// `cargo test -p florui-assets --features net -- --ignored real_https`.
#[test]
#[ignore = "needs network access"]
fn real_https_is_reached_through_the_system_store_and_the_environment_proxy() {
    let fetcher = Fetcher::new(NetConfig::default());
    let fetched = fetcher
        .fetch("https://www.rust-lang.org/static/images/rust-logo-blk.svg")
        .expect("an SVG over TLS");
    assert!(fetched.bytes.starts_with(b"<svg") || fetched.bytes.starts_with(b"<?xml"));
}
