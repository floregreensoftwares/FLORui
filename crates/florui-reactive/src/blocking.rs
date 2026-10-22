//! [`spawn_blocking`] and [`sleep`]: the minimal, dependency-free way real
//! background work reaches a future [`crate::use_resource`] can await.
//!
//! [`crate::executor::LocalExecutor`] only polls futures to completion —
//! it has no timer or I/O reactor of its own, so nothing here makes a
//! `tokio`- or `async-std`-flavored future work. A fetch that needs an
//! actual async runtime's own primitives (non-blocking sockets, a timer
//! wheel) should bring that runtime and await it directly; this module is
//! for the common case of bridging one blocking call — a filesystem read,
//! a synchronous HTTP client, `std::thread::sleep` — onto its own OS
//! thread so it doesn't block the caller.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

struct Shared<T> {
    result: Mutex<Option<T>>,
    waker: Mutex<Option<Waker>>,
}

/// The future [`spawn_blocking`] returns.
pub struct BlockingTask<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Future for BlockingTask<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let mut result = self.shared.result.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(value) = result.take() {
            Poll::Ready(value)
        } else {
            *self.shared.waker.lock().unwrap_or_else(|e| e.into_inner()) = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

/// Runs `work` on its own OS thread and resolves once it finishes. Each
/// call spawns a fresh thread rather than drawing from a shared pool —
/// fine for the occasional fetch a UI kicks off, not a substitute for a
/// real thread pool under sustained concurrent load.
pub fn spawn_blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> BlockingTask<T> {
    let shared = Arc::new(Shared {
        result: Mutex::new(None),
        waker: Mutex::new(None),
    });
    let shared_for_thread = Arc::clone(&shared);
    std::thread::spawn(move || {
        let value = work();
        let waker = {
            let mut result = shared_for_thread
                .result
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *result = Some(value);
            shared_for_thread
                .waker
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    });
    BlockingTask { shared }
}

/// Resolves after `duration` — a timer built the same way [`spawn_blocking`]
/// bridges any other blocking call, not an efficient primitive for many
/// concurrent timers (each is its own parked OS thread).
pub fn sleep(duration: Duration) -> BlockingTask<()> {
    spawn_blocking(move || std::thread::sleep(duration))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{Executor, LocalExecutor};
    use std::sync::mpsc;

    /// Gets the result of the task the executor runs, the way a host does: waits for the
    /// executor to report real background progress (see [`LocalExecutor::on_woken`]) and drains
    /// it, standing in for a host's event loop without needing one. A background thread that
    /// finishes before the task's first poll leaves nothing to wake (the poll finds the result
    /// in hand), so `results` is looked at before waiting.
    fn drain_until_done<T>(
        executor: &LocalExecutor,
        woken: &mpsc::Receiver<()>,
        results: &mpsc::Receiver<T>,
    ) -> T {
        if let Ok(value) = results.try_recv() {
            return value;
        }
        woken
            .recv_timeout(Duration::from_secs(5))
            .expect("the background thread must eventually wake the executor");
        executor.run_until_stalled();
        results
            .recv_timeout(Duration::from_secs(5))
            .expect("the task must have completed once the executor was drained again")
    }

    #[test]
    fn spawn_blocking_resolves_with_the_closures_return_value() {
        let executor = LocalExecutor::new();
        let (woken_tx, woken_rx) = mpsc::channel();
        executor.on_woken(move || {
            let _ = woken_tx.send(());
        });

        let (result_tx, result_rx) = mpsc::channel();
        executor.spawn(Box::pin(async move {
            let value = spawn_blocking(|| 1 + 1).await;
            result_tx.send(value).unwrap();
        }));
        executor.run_until_stalled();

        assert_eq!(drain_until_done(&executor, &woken_rx, &result_rx), 2);
    }

    #[test]
    fn a_thread_that_finishes_before_the_first_poll_needs_no_wake() {
        let executor = LocalExecutor::new();
        let (woken_tx, woken_rx) = mpsc::channel();
        executor.on_woken(move || {
            let _ = woken_tx.send(());
        });

        let (result_tx, result_rx) = mpsc::channel();
        executor.spawn(Box::pin(async move {
            let task = spawn_blocking(|| 1 + 1);
            // The thread is long done when the task is first polled, as it can be on a busy
            // machine; the poll finds the result and nothing is left to wake.
            std::thread::sleep(Duration::from_millis(100));
            result_tx.send(task.await).unwrap();
        }));
        executor.run_until_stalled();

        assert_eq!(drain_until_done(&executor, &woken_rx, &result_rx), 2);
    }

    #[test]
    fn sleep_resolves_after_a_real_background_thread_wakes_it() {
        let executor = LocalExecutor::new();
        let (woken_tx, woken_rx) = mpsc::channel();
        executor.on_woken(move || {
            let _ = woken_tx.send(());
        });

        let (done_tx, done_rx) = mpsc::channel();
        executor.spawn(Box::pin(async move {
            sleep(Duration::from_millis(10)).await;
            done_tx.send(()).unwrap();
        }));
        executor.run_until_stalled();

        drain_until_done(&executor, &woken_rx, &done_rx);
    }
}
