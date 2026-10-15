//! Single-instance activation on Linux: an exclusive `flock` on a file in the
//! user's runtime directory says which process is primary, and a Unix socket
//! next to it carries an activation payload to that process. See
//! `crate::activation` for the event types and the queue this feeds.
//!
//! The kernel drops the lock when the holder exits, crash included, so no PID
//! file can go stale. The directory is `$XDG_RUNTIME_DIR`, which the desktop
//! creates private to the user; without it, a `0700` directory under `/tmp`
//! that is checked to be ours.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::activation::ActivationEvent;

/// Caps one activation message on the wire.
const MAX_MESSAGE_BYTES: u64 = 64 * 1024;

/// A second launch retries the connection for this long: this process can win
/// the lock a moment before the primary has bound its socket.
const CONNECT_RETRY: Duration = Duration::from_millis(500);

/// The longest a primary waits for one client to send its message.
const READ_TIMEOUT: Duration = Duration::from_secs(2);

const ACK: &[u8] = b"ack";

/// Unix socket paths are limited to about 108 bytes.
const MAX_NAME_LEN: usize = 40;

/// Holds the lock this process owns; dropping it frees the identifier. The
/// lock file stays (a second process may be opening it right now), but the
/// socket is this process's own and goes with it.
pub(crate) struct LockOwnership {
    _file: File,
    socket: PathBuf,
}

impl Drop for LockOwnership {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}

pub(crate) enum InstanceRole {
    Primary(LockOwnership),
    Secondary,
}

pub(crate) enum HandoffOutcome {
    Delivered,
    Failed,
}

/// A file-name stem for `app_identifier`: its safe characters when it is
/// short, else a prefix plus a stable hash, so two long identifiers that share
/// a prefix still differ and the socket path stays short enough.
fn name_stem(app_identifier: &str) -> String {
    let safe: String = app_identifier
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.len() <= MAX_NAME_LEN && safe == app_identifier {
        return safe;
    }
    // FNV-1a: unlike `DefaultHasher` it is the same in every build, and a
    // second launch may come from another build of the application.
    let hash = app_identifier
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    let prefix: String = safe.chars().take(MAX_NAME_LEN - 17).collect();
    format!("{prefix}-{hash:016x}")
}

fn lock_path(dir: &Path, app_identifier: &str) -> PathBuf {
    dir.join(format!("florui-{}.lock", name_stem(app_identifier)))
}

fn socket_path(dir: &Path, app_identifier: &str) -> PathBuf {
    dir.join(format!("florui-{}.sock", name_stem(app_identifier)))
}

/// The private directory for the lock and socket.
fn runtime_dir() -> io::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
        && dir.is_absolute()
        && dir.is_dir()
    {
        return Ok(dir);
    }
    // `/proc/self` belongs to the user this process runs as.
    let uid = std::fs::metadata("/proc/self")?.uid();
    let dir = PathBuf::from(format!("/tmp/florui-{uid}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = std::fs::symlink_metadata(&dir)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a private directory of this user", dir.display()),
        ));
    }
    Ok(dir)
}

pub(crate) fn acquire(app_identifier: &str) -> io::Result<InstanceRole> {
    acquire_in(&runtime_dir()?, app_identifier)
}

fn acquire_in(dir: &Path, app_identifier: &str) -> io::Result<InstanceRole> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(lock_path(dir, app_identifier))?;
    match file.try_lock() {
        Ok(()) => Ok(InstanceRole::Primary(LockOwnership {
            _file: file,
            socket: socket_path(dir, app_identifier),
        })),
        Err(TryLockError::WouldBlock) => Ok(InstanceRole::Secondary),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

pub(crate) fn spawn_activation_listener(
    app_identifier: &str,
    on_event: impl Fn(ActivationEvent) + Send + 'static,
) -> io::Result<()> {
    listen_in(&runtime_dir()?, app_identifier, on_event)
}

/// Binds the socket and serves it from a thread. Whoever holds the lock may
/// remove what is at the path: a socket left by a process that died.
fn listen_in(
    dir: &Path,
    app_identifier: &str,
    on_event: impl Fn(ActivationEvent) + Send + 'static,
) -> io::Result<()> {
    let path = socket_path(dir, app_identifier);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                eprintln!(
                    "florui-platform: activation socket accept failed, stopping the listener"
                );
                break;
            };
            let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
            let mut bytes = Vec::new();
            if let Err(error) = (&mut stream)
                .take(MAX_MESSAGE_BYTES)
                .read_to_end(&mut bytes)
            {
                eprintln!("florui-platform: could not read activation payload ({error})");
                continue;
            }
            match serde_json::from_slice::<ActivationEvent>(&bytes) {
                Ok(event) => {
                    on_event(event);
                    let _ = stream.write_all(ACK);
                }
                // No ack: the sender's own wait then times out instead of
                // being told the handoff worked.
                Err(error) => eprintln!(
                    "florui-platform: malformed activation payload, ignoring it ({error})"
                ),
            }
        }
    });
    Ok(())
}

/// Sends `event` to the primary and waits for its acknowledgement, on a
/// thread so a stuck or dead primary never holds this call past `timeout`.
pub(crate) fn handoff(
    app_identifier: &str,
    event: &ActivationEvent,
    timeout: Duration,
) -> HandoffOutcome {
    let Ok(dir) = runtime_dir() else {
        return HandoffOutcome::Failed;
    };
    handoff_in(&dir, app_identifier, event, timeout)
}

fn handoff_in(
    dir: &Path,
    app_identifier: &str,
    event: &ActivationEvent,
    timeout: Duration,
) -> HandoffOutcome {
    let Ok(payload) = serde_json::to_vec(event) else {
        return HandoffOutcome::Failed;
    };
    let path = socket_path(dir, app_identifier);
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(connect_and_send(&path, &payload, timeout).is_ok());
    });
    match receiver.recv_timeout(timeout) {
        Ok(true) => HandoffOutcome::Delivered,
        Ok(false) | Err(_) => HandoffOutcome::Failed,
    }
}

fn connect_and_send(path: &Path, payload: &[u8], timeout: Duration) -> io::Result<()> {
    let started = Instant::now();
    let mut stream = loop {
        match UnixStream::connect(path) {
            Ok(stream) => break stream,
            Err(error) if started.elapsed() < CONNECT_RETRY.min(timeout) => {
                let _ = error;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    };
    stream.set_write_timeout(Some(timeout))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.write_all(payload)?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut reply = Vec::new();
    stream.take(ACK.len() as u64).read_to_end(&mut reply)?;
    if reply == ACK {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "no acknowledgement",
        ))
    }
}

/// A dedicated identifier for the probe, never a real application's.
const PROBE_IDENTIFIER: &str = "florui-doctor-capability-probe";

/// Real evidence that the lock and the socket work on this machine: both are
/// created and removed again.
pub(crate) fn probe_capability() -> bool {
    let Ok(dir) = runtime_dir() else {
        return false;
    };
    let locked = acquire_in(&dir, PROBE_IDENTIFIER).is_ok();
    let path = socket_path(&dir, PROBE_IDENTIFIER);
    let bound = UnixListener::bind(&path).is_ok();
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(lock_path(&dir, PROBE_IDENTIFIER));
    locked && bound
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;

    use super::*;

    fn is_primary(role: &InstanceRole) -> bool {
        matches!(role, InstanceRole::Primary(_))
    }

    #[test]
    fn the_first_launch_is_primary_and_the_next_is_secondary_until_it_exits() {
        let dir = tempfile::tempdir().unwrap();
        let first = acquire_in(dir.path(), "com.example.garden").unwrap();
        assert!(is_primary(&first));
        let second = acquire_in(dir.path(), "com.example.garden").unwrap();
        assert!(!is_primary(&second), "the lock is held");
        drop(first);
        let third = acquire_in(dir.path(), "com.example.garden").unwrap();
        assert!(
            is_primary(&third),
            "dropping the owner frees the identifier"
        );
    }

    #[test]
    fn the_owner_removes_its_socket_when_it_exits() {
        let dir = tempfile::tempdir().unwrap();
        let owner = acquire_in(dir.path(), "com.example.garden").unwrap();
        listen_in(dir.path(), "com.example.garden", |_| {}).unwrap();
        let socket = socket_path(dir.path(), "com.example.garden");
        assert!(socket.exists());
        drop(owner);
        assert!(!socket.exists());
        assert!(
            lock_path(dir.path(), "com.example.garden").exists(),
            "the lock file stays"
        );
    }

    #[test]
    fn different_identifiers_do_not_share_a_lock() {
        let dir = tempfile::tempdir().unwrap();
        let _a = acquire_in(dir.path(), "com.example.garden").unwrap();
        let b = acquire_in(dir.path(), "com.example.garden.dev").unwrap();
        assert!(is_primary(&b));
    }

    #[test]
    fn an_event_reaches_the_primary_and_is_acknowledged() {
        let dir = tempfile::tempdir().unwrap();
        let (sender, receiver) = channel();
        listen_in(dir.path(), "com.example.garden", move |event| {
            let _ = sender.send(event);
        })
        .unwrap();
        let event = ActivationEvent::OpenUrl {
            url: "garden://plant/7".into(),
        };
        let outcome = handoff_in(
            dir.path(),
            "com.example.garden",
            &event,
            Duration::from_secs(2),
        );
        assert!(matches!(outcome, HandoffOutcome::Delivered));
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            event
        );
    }

    #[test]
    fn several_events_in_a_row_all_arrive_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let (sender, receiver) = channel();
        listen_in(dir.path(), "com.example.garden", move |event| {
            let _ = sender.send(event);
        })
        .unwrap();
        for n in 0..5 {
            let event = ActivationEvent::Launch {
                args: vec!["app".into(), n.to_string()],
            };
            let outcome = handoff_in(
                dir.path(),
                "com.example.garden",
                &event,
                Duration::from_secs(2),
            );
            assert!(matches!(outcome, HandoffOutcome::Delivered));
        }
        let seen: Vec<_> = receiver.try_iter().collect();
        assert_eq!(seen.len(), 5);
        assert_eq!(
            seen[4],
            ActivationEvent::Launch {
                args: vec!["app".into(), "4".into()]
            }
        );
    }

    #[test]
    fn nobody_listening_means_the_handoff_fails_within_the_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let start = Instant::now();
        let event = ActivationEvent::Launch { args: vec![] };
        let outcome = handoff_in(
            dir.path(),
            "com.example.garden",
            &event,
            Duration::from_millis(300),
        );
        assert!(matches!(outcome, HandoffOutcome::Failed));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_malformed_payload_is_ignored_and_never_acknowledged() {
        let dir = tempfile::tempdir().unwrap();
        let (sender, receiver) = channel();
        listen_in(dir.path(), "com.example.garden", move |event| {
            let _ = sender.send(event);
        })
        .unwrap();
        let mut stream =
            UnixStream::connect(socket_path(dir.path(), "com.example.garden")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream.write_all(b"{ this is not an event").unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut reply = Vec::new();
        let _ = stream.read_to_end(&mut reply);
        assert!(reply.is_empty(), "no acknowledgement for garbage");
        assert!(receiver.try_recv().is_err());
        // The listener is still serving.
        let event = ActivationEvent::Launch { args: vec![] };
        let outcome = handoff_in(
            dir.path(),
            "com.example.garden",
            &event,
            Duration::from_secs(2),
        );
        assert!(matches!(outcome, HandoffOutcome::Delivered));
    }

    #[test]
    fn a_socket_left_by_a_dead_process_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = socket_path(dir.path(), "com.example.garden");
        drop(UnixListener::bind(&path).unwrap()); // leaves the file behind
        assert!(path.exists());
        let (sender, receiver) = channel();
        listen_in(dir.path(), "com.example.garden", move |event| {
            let _ = sender.send(event);
        })
        .unwrap();
        let event = ActivationEvent::Launch { args: vec![] };
        let outcome = handoff_in(
            dir.path(),
            "com.example.garden",
            &event,
            Duration::from_secs(2),
        );
        assert!(matches!(outcome, HandoffOutcome::Delivered));
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            event
        );
    }

    #[test]
    fn the_socket_is_private_to_the_user() {
        let dir = tempfile::tempdir().unwrap();
        listen_in(dir.path(), "com.example.garden", |_| {}).unwrap();
        let mode = std::fs::metadata(socket_path(dir.path(), "com.example.garden"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "mode {mode:o}");
    }

    #[test]
    fn long_and_odd_identifiers_give_short_distinct_stable_names() {
        let a = "com.example.".to_string() + &"a".repeat(80);
        let b = "com.example.".to_string() + &"a".repeat(79) + "b";
        let (sa, sb) = (name_stem(&a), name_stem(&b));
        assert!(sa.len() <= MAX_NAME_LEN && sb.len() <= MAX_NAME_LEN);
        assert_ne!(sa, sb);
        assert_eq!(sa, name_stem(&a));
        assert_ne!(
            name_stem("a/b"),
            name_stem("a_b"),
            "unsafe characters never alias a safe name"
        );
        assert_eq!(name_stem("com.example.garden"), "com.example.garden");
    }

    #[test]
    fn the_capability_probe_succeeds_and_leaves_nothing_behind() {
        assert!(probe_capability());
        let dir = runtime_dir().unwrap();
        assert!(!lock_path(&dir, PROBE_IDENTIFIER).exists());
        assert!(!socket_path(&dir, PROBE_IDENTIFIER).exists());
    }
}
