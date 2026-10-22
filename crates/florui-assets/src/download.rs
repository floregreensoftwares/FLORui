//! The picture an `<img>` names at an `http://` or `https://` address: started in the
//! background, delivered decoded to a callback, and cancelled by dropping its ticket.
//!
//! It is what a window's `<img>` needs and what `florui-platform` would otherwise have to
//! build from the pieces of the `net` feature, and it exists with or without that feature: a
//! build without it answers every address with `FetchError::Unavailable` at once, so a
//! consumer needs no conditional build of its own, and cannot end up with `<img>` and
//! `url(...)` disagreeing about whether the network is there when another crate in the build
//! turns the feature on.

use std::sync::Arc;

use crate::{AssetCache, AssetError, DecodeParams, RasterImage, Size2D};
#[cfg(not(feature = "net"))]
use crate::{FetchError, url_image};

/// The bytes of a downloaded document, shared with the download and not copied.
pub type SharedBytes = Arc<dyn AsRef<[u8]> + Send + Sync>;

/// What a download became.
pub enum Downloaded {
    /// A raster image, decoded.
    Raster(Arc<RasterImage>),
    /// An SVG, parsed far enough to know its size; it is rasterized later, when the size it
    /// is drawn at is known, from `document`, decoded under `key` (see
    /// [`AssetCache::get_or_decode`]): the key has a digest of the document in it, so that
    /// another version of the same address is never handed back for this one.
    Svg {
        intrinsic: Size2D,
        key: String,
        document: SharedBytes,
    },
}

/// Cancels the download when it is dropped: a download still queued is never sent, one running
/// stops at its next read, and the callback is not called.
pub struct DownloadTicket {
    #[cfg(feature = "net")]
    _ticket: crate::net::FetchTicket,
    /// Set when the ticket is dropped, for a download that has arrived and waits to be
    /// decoded, which the ticket of the fetch can no longer take back.
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for DownloadTicket {
    fn drop(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The raster image of the latest download of `url` that is still alive in `cache`, if there
/// is one: a picture that is held is shared as it is, as a memory cache does, without asking
/// the server.
pub fn live_image(url: &str, cache: &AssetCache) -> Option<Arc<RasterImage>> {
    let key = crate::url_image::latest_download(url)?;
    cache.cached_url(&key, DecodeParams::Native)
}

/// Starts downloading the picture at `url` and decoding it in `cache`. `on_done` is called
/// once, from a thread of the decoders, unless the ticket is dropped first.
///
/// `Err` when it cannot be started at all (the address is not one that is requested, or this
/// build has no network loading); `on_done` is not called then.
#[cfg(feature = "net")]
pub fn start(
    url: &str,
    cache: &Arc<AssetCache>,
    on_done: impl FnOnce(Result<Downloaded, AssetError>) + Send + 'static,
) -> Result<DownloadTicket, AssetError> {
    let address = crate::url_image::remote_address(url);
    let (cache, owned) = (Arc::clone(cache), address.clone());
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let for_job = Arc::clone(&cancelled);
    let ticket = crate::net::shared_fetcher()
        .submit(&address, move |outcome| {
            // Decoding is for threads of its own, not the fetcher's, which are for downloads.
            let weight = outcome.as_ref().map_or(0, |fetched| fetched.bytes.len());
            crate::remote_image::decoder::run(
                Box::new(move || {
                    // Dropped while it waited to be decoded: nobody wants it any more.
                    if for_job.load(std::sync::atomic::Ordering::SeqCst) {
                        return;
                    }
                    let failed = |error| AssetError::Fetch {
                        url: owned.clone(),
                        error,
                    };
                    let result = match outcome {
                        Err(error) => Err(failed(error)),
                        Ok(fetched) => {
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                decode(&owned, &cache, fetched)
                            }))
                            .unwrap_or_else(|_| {
                                Err(failed(crate::FetchError::Transport(
                                    "the decoder panicked".to_owned(),
                                )))
                            })
                        }
                    };
                    on_done(result);
                }),
                weight,
            );
        })
        .map_err(|error| AssetError::Fetch {
            url: address.clone(),
            error,
        })?;
    Ok(DownloadTicket {
        _ticket: ticket,
        cancelled,
    })
}

/// Without the `net` feature every address fails at once, saying why.
#[cfg(not(feature = "net"))]
pub fn start(
    url: &str,
    _cache: &Arc<AssetCache>,
    _on_done: impl FnOnce(Result<Downloaded, AssetError>) + Send + 'static,
) -> Result<DownloadTicket, AssetError> {
    Err(AssetError::Fetch {
        url: url_image::remote_address(url),
        error: FetchError::Unavailable,
    })
}

#[cfg(feature = "net")]
fn decode(
    url: &str,
    cache: &AssetCache,
    fetched: Arc<crate::net::Fetched>,
) -> Result<Downloaded, AssetError> {
    // Under a key with the digest of these bytes in it: a decode of an older version, still
    // alive in another window, is never handed back for them.
    let key = crate::url_image::register_download(url, &fetched.bytes);
    if crate::Format::sniff(&fetched.bytes).is_some() {
        return cache
            .get_or_decode(&key, &fetched.bytes, DecodeParams::Native)
            .map(Downloaded::Raster);
    }
    let text =
        std::str::from_utf8(&fetched.bytes).map_err(|_| AssetError::InvalidUtf8 { path: None })?;
    // Someone else's document: it may not name files of ours.
    let image = crate::parse_svg_untrusted(text)?;
    Ok(Downloaded::Svg {
        intrinsic: image.intrinsic_size,
        key,
        document: fetched,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "net"))]
    #[test]
    fn without_the_net_feature_an_address_is_refused_at_once() {
        let cache = Arc::new(AssetCache::new());
        let refused = start("https://example.com/a.png", &cache, |_| {
            unreachable!("not started")
        });
        assert!(matches!(
            refused,
            Err(AssetError::Fetch {
                error: FetchError::Unavailable,
                ..
            })
        ));
    }

    #[test]
    fn nothing_is_alive_before_anything_was_downloaded() {
        let cache = AssetCache::new();
        assert!(live_image("https://example.com/never.png", &cache).is_none());
    }

    #[cfg(feature = "net")]
    mod downloads {
        use std::sync::mpsc;
        use std::time::Duration;

        use super::*;
        use crate::net::test_server::{Reply, Server};
        use crate::net::{NetConfig, ProxyMode};

        fn png() -> Vec<u8> {
            let image = image::RgbaImage::from_pixel(3, 2, image::Rgba([1, 2, 3, 255]));
            let mut bytes = Vec::new();
            image::DynamicImage::ImageRgba8(image)
                .write_to(
                    &mut std::io::Cursor::new(&mut bytes),
                    image::ImageFormat::Png,
                )
                .unwrap();
            bytes
        }

        fn direct() {
            crate::net::configure(NetConfig {
                proxy: ProxyMode::Direct,
                cache_dir: Some(std::env::temp_dir().join("florui-assets-remote-image-tests")),
                ..NetConfig::default()
            });
        }

        fn wait_for(receiver: &mpsc::Receiver<Result<Downloaded, AssetError>>) -> Downloaded {
            receiver
                .recv_timeout(Duration::from_secs(10))
                .expect("it is delivered")
                .unwrap_or_else(|error| panic!("it loads: {error}"))
        }

        #[test]
        fn a_raster_image_is_delivered_decoded_and_then_found_alive_without_a_request() {
            direct();
            let server = Server::start(|_| Reply::ok("image/png", png()));
            let url = server.url("/a.png");
            let cache = Arc::new(AssetCache::new());
            let (sender, receiver) = mpsc::channel();
            let _ticket = start(&url, &cache, move |result| {
                let _ = sender.send(result);
            })
            .unwrap();
            let Downloaded::Raster(image) = wait_for(&receiver) else {
                panic!("a raster image")
            };
            assert_eq!((image.width, image.height), (3, 2));
            let alive = live_image(&url, &cache).expect("alive while held");
            assert!(Arc::ptr_eq(&image, &alive));
            assert_eq!(server.hits(), 1);
            drop((image, alive));
            assert!(
                live_image(&url, &cache).is_none(),
                "gone with its last holder"
            );
        }

        #[test]
        fn an_svg_is_delivered_with_its_size_its_document_and_a_key_that_has_its_digest() {
            direct();
            let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect width="40" height="20" fill="#36f"/></svg>"##;
            let server = Server::start(move |_| Reply::ok("image/svg+xml", svg.to_vec()));
            let url = server.url("/logo.svg");
            let cache = Arc::new(AssetCache::new());
            let (sender, receiver) = mpsc::channel();
            let _ticket = start(&url, &cache, move |result| {
                let _ = sender.send(result);
            })
            .unwrap();
            let Downloaded::Svg {
                intrinsic,
                key,
                document,
            } = wait_for(&receiver)
            else {
                panic!("an SVG")
            };
            assert_eq!((intrinsic.width, intrinsic.height), (40.0, 20.0));
            assert_eq!((*document).as_ref(), svg.as_slice());
            assert_eq!(key, crate::url_image::download_key(&url, svg));
        }

        #[test]
        fn a_failure_reaches_the_callback_with_the_reason() {
            direct();
            let server = Server::start(|_| Reply::status(404));
            let url = server.url("/gone.png");
            let cache = Arc::new(AssetCache::new());
            let (sender, receiver) = mpsc::channel();
            let _ticket = start(&url, &cache, move |result| {
                let _ = sender.send(result);
            })
            .unwrap();
            let result = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
            assert!(matches!(
                result,
                Err(AssetError::Fetch {
                    error: crate::FetchError::Status(404),
                    ..
                })
            ));
        }

        #[test]
        fn an_address_that_is_not_requested_is_refused_at_once() {
            direct();
            let cache = Arc::new(AssetCache::new());
            let refused = start("ftp://example.com/a.png", &cache, |_| unreachable!());
            assert!(matches!(
                refused,
                Err(AssetError::Fetch {
                    error: crate::FetchError::UnsupportedScheme(_),
                    ..
                })
            ));
        }

        #[test]
        fn dropping_the_ticket_cancels_and_the_callback_is_not_called() {
            direct();
            let server = Server::start(|_| {
                Reply::ok("image/png", png()).delayed(Duration::from_millis(400))
            });
            let url = server.url("/slow.png");
            let cache = Arc::new(AssetCache::new());
            let (sender, receiver) = mpsc::channel::<()>();
            let ticket = start(&url, &cache, move |_| {
                let _ = sender.send(());
            })
            .unwrap();
            drop(ticket);
            assert!(
                receiver.recv_timeout(Duration::from_millis(800)).is_err(),
                "cancelled"
            );
        }
    }
}
