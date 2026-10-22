//! A tiny HTTP/1.1 server on the loopback interface for the tests that load pictures
//! from "the network".

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Answers each request with `reply(path)`: a status, a content type, a body and
/// how long to wait first. Records the paths asked for.
pub(crate) struct Server {
    pub(crate) base: String,
    seen: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl Server {
    pub(crate) fn start(
        reply: impl Fn(&str) -> (u16, &'static str, Vec<u8>, Duration) + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let reply = Arc::new(reply);
        let (seen_for_thread, stop_for_thread) = (Arc::clone(&seen), Arc::clone(&stop));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop_for_thread.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(mut stream) = stream else { continue };
                let (reply, seen) = (Arc::clone(&reply), Arc::clone(&seen_for_thread));
                std::thread::spawn(move || {
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        match stream.read(&mut byte) {
                            Ok(1) => head.push(byte[0]),
                            _ => return,
                        }
                    }
                    let text = String::from_utf8_lossy(&head).into_owned();
                    let path = text.split(' ').nth(1).unwrap_or("/").to_owned();
                    seen.lock().unwrap().push(path.clone());
                    let (status, content_type, body, delay) = reply(&path);
                    std::thread::sleep(delay);
                    let head = format!(
                        "HTTP/1.1 {status} X\r\nConnection: close\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&body);
                });
            }
        });
        Self { base, seen, stop }
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    pub(crate) fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(self.base.trim_start_matches("http://"));
    }
}

/// The configuration every test of this crate gives the process-wide fetcher, which the
/// first call fixes for all of them: direct connections, and one request at a time so a
/// queued request can be told from a running one.
pub(crate) fn configure_fetcher() {
    florui_assets::net::configure(florui_assets::net::NetConfig {
        max_concurrent: 1,
        proxy: florui_assets::net::ProxyMode::Direct,
        ..florui_assets::net::NetConfig::default()
    });
}
