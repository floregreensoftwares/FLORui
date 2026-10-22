//! A small HTTP/1.1 server on the loopback interface, for the tests of this module: it
//! answers whatever a closure says, can be slow or endless, and counts what reaches it
//! and how much of it runs at once.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) struct Request {
    pub path: String,
    /// Lower-case names.
    pub headers: HashMap<String, String>,
}

pub(crate) struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Waits this long before answering.
    pub delay: Duration,
    /// Sends the body with no `Content-Length`, and never ends it.
    pub endless: bool,
}

impl Reply {
    pub fn ok(content_type: &str, body: Vec<u8>) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type".to_owned(), content_type.to_owned())],
            body,
            delay: Duration::ZERO,
            endless: false,
        }
    }

    pub fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
            delay: Duration::ZERO,
            endless: false,
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    pub fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

/// How many requests are being answered, and the most there were at once. Shared by
/// several servers to count across hosts.
#[derive(Default)]
pub(crate) struct Gauge {
    pub active: AtomicUsize,
    pub peak: AtomicUsize,
}

pub(crate) struct Server {
    pub base: String,
    pub gauge: Arc<Gauge>,
    seen: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl Server {
    pub fn start(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        Self::start_counting(Arc::new(Gauge::default()), handler)
    }

    pub fn start_counting(
        gauge: Arc<Gauge>,
        handler: impl Fn(&Request) -> Reply + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handler = Arc::new(handler);
        {
            let (gauge, seen, stop) = (Arc::clone(&gauge), Arc::clone(&seen), Arc::clone(&stop));
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(stream) = stream else { continue };
                    let (handler, gauge, seen) =
                        (Arc::clone(&handler), Arc::clone(&gauge), Arc::clone(&seen));
                    std::thread::spawn(move || serve(stream, &*handler, &gauge, &seen));
                }
            });
        }
        Self {
            base,
            gauge,
            seen,
            stop,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// The paths asked for, in order of arrival.
    pub fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    pub fn hits(&self) -> usize {
        self.seen().len()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wakes the acceptor so it sees the flag.
        let _ = TcpStream::connect(self.base.trim_start_matches("http://"));
    }
}

fn serve(
    mut stream: TcpStream,
    handler: &(dyn Fn(&Request) -> Reply + Send + Sync),
    gauge: &Gauge,
    seen: &Mutex<Vec<String>>,
) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
    }
    let text = String::from_utf8_lossy(&head).into_owned();
    let mut lines = text.split("\r\n");
    let path = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or("/")
        .to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(": "))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.to_owned()))
        .collect();
    seen.lock().unwrap().push(path.clone());
    let active = gauge.active.fetch_add(1, Ordering::SeqCst) + 1;
    gauge.peak.fetch_max(active, Ordering::SeqCst);
    let reply = handler(&Request { path, headers });
    std::thread::sleep(reply.delay);
    let mut response = format!("HTTP/1.1 {} Reply\r\nConnection: close\r\n", reply.status);
    for (name, value) in &reply.headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    if !reply.endless {
        response.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
    }
    response.push_str("\r\n");
    let sent =
        stream.write_all(response.as_bytes()).is_ok() && stream.write_all(&reply.body).is_ok();
    if sent && reply.endless {
        let chunk = [0x42u8; 16 * 1024];
        while stream.write_all(&chunk).is_ok() {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    gauge.active.fetch_sub(1, Ordering::SeqCst);
}

/// A proxy that tunnels: an HTTP client asks it to `CONNECT` to a host and then talks to
/// that host through the connection, which is how `ureq` uses an HTTP proxy even for
/// plain `http` addresses. Records the targets it was asked to reach.
pub(crate) struct ConnectProxy {
    pub base: String,
    connects: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl ConnectProxy {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let connects = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        {
            let (connects, stop) = (Arc::clone(&connects), Arc::clone(&stop));
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(stream) = stream else { continue };
                    let connects = Arc::clone(&connects);
                    std::thread::spawn(move || tunnel(stream, &connects));
                }
            });
        }
        Self {
            base,
            connects,
            stop,
        }
    }

    pub fn connects(&self) -> Vec<String> {
        self.connects.lock().unwrap().clone()
    }
}

impl Drop for ConnectProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.base.trim_start_matches("http://"));
    }
}

fn tunnel(mut client: TcpStream, connects: &Mutex<Vec<String>>) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match client.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
    }
    let text = String::from_utf8_lossy(&head).into_owned();
    let Some(target) = text
        .strip_prefix("CONNECT ")
        .and_then(|rest| rest.split(' ').next())
    else {
        let _ = client.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
        return;
    };
    connects.lock().unwrap().push(target.to_owned());
    let Ok(upstream) = TcpStream::connect(target) else {
        let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n");
        return;
    };
    if client
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .is_err()
    {
        return;
    }
    let (mut client_read, mut upstream_write) =
        (client.try_clone().unwrap(), upstream.try_clone().unwrap());
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut client_read, &mut upstream_write);
        let _ = upstream_write.shutdown(std::net::Shutdown::Write);
    });
    let (mut upstream_read, mut client_write) = (upstream, client);
    let _ = std::io::copy(&mut upstream_read, &mut client_write);
}
