//! Named-mutex ownership plus a named pipe, secured to the current user
//! session, for handing an activation payload to an already-running
//! process — see `crate::activation`'s own doc for the pure logic (queue,
//! event types, name derivation) this builds on. The restrictive
//! security descriptor and the message-mode pipe framing were both
//! confirmed against two real separate processes in a disposable scratch
//! experiment before this module was written.

use std::io;
use std::ptr::null_mut;
use std::sync::mpsc;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_MESSAGE,
    PIPE_TYPE_MESSAGE, PIPE_WAIT, WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex};

use crate::activation::{self, ActivationEvent};

/// Caps one activation message on the wire -- "bound... payload sizes."
const MAX_MESSAGE_BYTES: u32 = 64 * 1024;

/// A second launch waits at most this long for the primary's own pipe
/// instance to exist before giving up -- covers the one genuine start-up
/// race (this process wins the mutex fractionally before the loser tries
/// to connect), not a real handoff round trip.
const CONNECT_TIMEOUT_MS: u32 = 500;

const ACK: &[u8] = b"ack";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Carries a raw Win32 handle across a spawned thread boundary. Sound
/// because ownership transfers completely -- the spawning thread never
/// touches the handle again after handing it off.
struct SendableHandle(HANDLE);
// Safety: see this struct's own doc.
unsafe impl Send for SendableHandle {}

/// Holds the named mutex this process owns. Releasing and closing it on
/// drop is exactly what tells the OS -- even on an unclean process exit,
/// crash included -- that this identifier is free for the next launch to
/// claim; no PID file, no staleness possible.
pub(crate) struct MutexOwnership(HANDLE);

impl Drop for MutexOwnership {
    fn drop(&mut self) {
        // Safety: self.0 is a live mutex handle this instance uniquely
        // acquired in `acquire` below and never shares with anything else.
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

pub(crate) enum InstanceRole {
    Primary(MutexOwnership),
    Secondary,
}

/// Atomically claims (or detects an existing claim on) the named mutex
/// derived from `app_identifier`. Two processes calling this within
/// microseconds of each other is not actually ambiguous -- the OS's own
/// named-object namespace insert is atomic, so exactly one sees
/// [`InstanceRole::Primary`].
pub(crate) fn acquire(app_identifier: &str) -> io::Result<InstanceRole> {
    let name = wide(&activation::mutex_name(app_identifier));
    // Safety: name is a valid null-terminated wide string; no security
    // attributes needed -- this process's own default token already owns
    // whatever it creates, and the `Local\` prefix already keeps the
    // object out of every other Windows session.
    let handle = unsafe { CreateMutexW(null_mut(), 1, name.as_ptr()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // Safety: handle was just returned above; GetLastError reflects this
    // same call per its own documented contract.
    let already_existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already_existed {
        // This process never actually acquired ownership (bInitialOwner is
        // ignored for a name that already existed) -- just release the
        // handle, nothing to hold onto.
        // Safety: handle is valid and otherwise unused from here on.
        unsafe {
            CloseHandle(handle);
        }
        Ok(InstanceRole::Secondary)
    } else {
        Ok(InstanceRole::Primary(MutexOwnership(handle)))
    }
}

/// SDDL granting generic-all only to this object's creator (`OW` --
/// `CREATOR_OWNER`, i.e. this process's own user) and to `SYSTEM`: the
/// spec's "authenticated local IPC with restrictive access." No other ACE,
/// no inheritance.
const RESTRICTIVE_PIPE_SDDL: &str = "D:(A;;GA;;;OW)(A;;GA;;;SY)";

/// Caller frees the returned descriptor with `LocalFree` once
/// `CreateNamedPipeW` has consumed it.
fn build_pipe_security_attributes() -> io::Result<(SECURITY_ATTRIBUTES, PSECURITY_DESCRIPTOR)> {
    let sddl = wide(RESTRICTIVE_PIPE_SDDL);
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // Safety: sddl is a valid null-terminated wide string; descriptor
    // receives a `LocalAlloc`'d pointer on success only.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1, // SDDL_REVISION_1
            &mut descriptor,
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        },
        descriptor,
    ))
}

fn create_pipe_instance(pipe_name: &str) -> io::Result<HANDLE> {
    let name = wide(pipe_name);
    let (attrs, descriptor) = build_pipe_security_attributes()?;
    // Safety: name is valid; attrs points at a live local holding the
    // security descriptor just built above, which outlives this call.
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
            1,
            MAX_MESSAGE_BYTES,
            MAX_MESSAGE_BYTES,
            0,
            &attrs,
        )
    };
    // Safety: descriptor was allocated by `ConvertStringSecurityDescriptorToSecurityDescriptorW`
    // above; `CreateNamedPipeW` only reads from it during the call and
    // does not retain it, so it's safe to free immediately after.
    unsafe {
        LocalFree(descriptor);
    }
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(handle)
}

/// One blocking read from a connected pipe handle, bounded to
/// [`MAX_MESSAGE_BYTES`]. Message-mode framing (confirmed in the scratch
/// experiment) means one `WriteFile` call is read back whole by one
/// `ReadFile` call -- no length-prefix framing needed on top.
fn read_message(handle: HANDLE) -> io::Result<Vec<u8>> {
    let mut buffer = vec![0u8; MAX_MESSAGE_BYTES as usize];
    let mut read = 0u32;
    // Safety: buffer is a valid, appropriately-sized local buffer; read
    // receives the actual byte count on success.
    let ok = unsafe {
        ReadFile(
            handle,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            &mut read,
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(read as usize);
    Ok(buffer)
}

fn write_message(handle: HANDLE, message: &[u8]) -> io::Result<()> {
    let mut written = 0u32;
    // Safety: message is a valid immutable buffer for the duration of the
    // call; written receives the actual byte count on success.
    let ok = unsafe {
        WriteFile(
            handle,
            message.as_ptr(),
            message.len() as u32,
            &mut written,
            null_mut(),
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Spawns the pipe-listener thread once this process has become primary
/// (see [`acquire`]). One connection is one message: a client connects,
/// this loop reads exactly one encoded [`ActivationEvent`], hands it to
/// `on_event` (already running on this background thread, hence its own
/// `Send` bound), writes a fixed acknowledgement confirming only receipt,
/// then disconnects and waits for the next client. A message that fails
/// to decode is logged and skipped without an ack -- the sender's own
/// bounded wait then correctly times out rather than being told the
/// handoff succeeded. Runs until the pipe itself errors (e.g. this
/// process is shutting down), at which point the listener stops quietly.
pub(crate) fn spawn_activation_listener(
    app_identifier: &str,
    on_event: impl Fn(ActivationEvent) + Send + 'static,
) -> io::Result<()> {
    let pipe_name = activation::pipe_name(app_identifier);
    let handle = SendableHandle(create_pipe_instance(&pipe_name)?);
    std::thread::spawn(move || {
        let handle = handle;
        loop {
            // Safety: handle.0 is a valid named-pipe server instance; no
            // overlapped IO requested, so this blocks until a client
            // connects.
            let connected = unsafe { ConnectNamedPipe(handle.0, null_mut()) };
            if connected == 0 {
                eprintln!(
                    "florui-platform: activation pipe accept failed ({}), stopping the listener",
                    io::Error::last_os_error()
                );
                break;
            }
            match read_message(handle.0) {
                Ok(bytes) => match serde_json::from_slice::<ActivationEvent>(&bytes) {
                    Ok(event) => {
                        on_event(event);
                        let _ = write_message(handle.0, ACK);
                    }
                    Err(error) => {
                        eprintln!(
                            "florui-platform: malformed activation payload, ignoring it ({error})"
                        );
                    }
                },
                Err(error) => {
                    eprintln!("florui-platform: could not read activation payload ({error})");
                }
            }
            // Safety: handle.0 is this same valid pipe instance, ready to
            // accept its next client.
            unsafe {
                DisconnectNamedPipe(handle.0);
            }
        }
        // Safety: handle.0 is still valid and owned solely by this thread.
        unsafe {
            CloseHandle(handle.0);
        }
    });
    Ok(())
}

pub(crate) enum HandoffOutcome {
    Delivered,
    Failed,
}

fn connect_and_send(pipe_name: &str, payload: &[u8]) -> io::Result<()> {
    let name = wide(pipe_name);
    // Safety: name is a valid null-terminated wide string. Absorbs the one
    // genuine start-up race: this process may reach here before the
    // primary's own listener thread has finished creating its pipe
    // instance.
    unsafe {
        WaitNamedPipeW(name.as_ptr(), CONNECT_TIMEOUT_MS);
    }
    // Safety: name is valid; an ordinary client-side open of an existing
    // named-pipe instance, no template file.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            null_mut(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let result = write_message(handle, payload).and_then(|()| read_message(handle).map(|_| ()));
    // Safety: handle was opened just above and is closed exactly once,
    // here, regardless of whether the exchange succeeded.
    unsafe {
        CloseHandle(handle);
    }
    result
}

/// Hands `event` to whichever process already owns `app_identifier`'s
/// mutex, waiting up to `timeout` for its acknowledgement. The whole
/// blocking exchange runs on its own thread so a stuck or crashed primary
/// can never hang this call past `timeout` -- this process is exiting
/// either way once this returns, so a thread left blocked in a dead
/// syscall past that point is harmless to abandon.
pub(crate) fn handoff(
    app_identifier: &str,
    event: &ActivationEvent,
    timeout: Duration,
) -> HandoffOutcome {
    let pipe_name = activation::pipe_name(app_identifier);
    let Ok(payload) = serde_json::to_vec(event) else {
        return HandoffOutcome::Failed;
    };
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let delivered = connect_and_send(&pipe_name, &payload).is_ok();
        let _ = sender.send(delivered);
    });
    match receiver.recv_timeout(timeout) {
        Ok(true) => HandoffOutcome::Delivered,
        Ok(false) | Err(_) => HandoffOutcome::Failed,
    }
}

/// A dedicated identifier `florui doctor`'s own capability probe uses --
/// never a real app's own identifier, so this can never collide with (or
/// interfere with) an actually-running instance of anything.
const PROBE_IDENTIFIER: &str = "florui-doctor-capability-probe";

/// Real, observed evidence that the named-mutex and named-pipe mechanisms
/// this module depends on actually work on this machine -- not an
/// assumption from the OS merely being Windows. Leaves nothing behind:
/// any acquired mutex releases via [`MutexOwnership`]'s own `Drop`, and
/// the probe pipe is closed immediately, before ever accepting a
/// connection.
pub(crate) fn probe_capability() -> bool {
    let mutex_ok = match acquire(PROBE_IDENTIFIER) {
        // Either this call became owner (and releases on drop below) or
        // found a real existing owner (another concurrent probe, or --
        // vanishingly unlikely -- a real app coincidentally sharing this
        // exact identifier) -- both prove the mechanism itself works.
        Ok(_role) => true,
        Err(_) => false,
    };
    let pipe_ok = match create_pipe_instance(&activation::pipe_name(PROBE_IDENTIFIER)) {
        Ok(handle) => {
            // Safety: handle was just created above by this same call and
            // is closed immediately -- this probe never accepts a real
            // connection.
            unsafe {
                CloseHandle(handle);
            }
            true
        }
        Err(_) => false,
    };
    mutex_ok && pipe_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs a real Windows session: cargo test -p florui-platform -- --ignored"]
    fn probe_capability_succeeds_on_a_real_windows_session() {
        assert!(probe_capability());
    }
}
