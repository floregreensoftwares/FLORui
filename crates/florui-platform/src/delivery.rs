//! A single value sent from any thread and awaited on the executor: what a worker thread
//! of the network fetcher uses to hand a download to the task that waits for it, since
//! the executor's own types stay on the thread that owns it.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

/// One value sent from any thread and awaited on the executor, which a worker thread of
/// the fetcher cannot do with the executor's own types.
struct Delivery<T> {
    value: Option<T>,
    waker: Option<Waker>,
    /// The sending side went away without sending, because the request was cancelled.
    abandoned: bool,
}

pub(crate) struct Sender<T>(Arc<Mutex<Delivery<T>>>);
pub(crate) struct Receiver<T>(Arc<Mutex<Delivery<T>>>);

pub(crate) fn delivery<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Mutex::new(Delivery {
        value: None,
        waker: None,
        abandoned: false,
    }));
    (Sender(Arc::clone(&shared)), Receiver(shared))
}

impl<T> Sender<T> {
    pub(crate) fn send(self, value: T) {
        let waker = {
            let mut shared = self.0.lock().unwrap_or_else(|e| e.into_inner());
            shared.value = Some(value);
            shared.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let waker = {
            let mut shared = self.0.lock().unwrap_or_else(|e| e.into_inner());
            if shared.value.is_some() {
                return;
            }
            shared.abandoned = true;
            shared.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl<T> Future for Receiver<T> {
    /// `None` when the request was cancelled.
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut shared = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(value) = shared.value.take() {
            Poll::Ready(Some(value))
        } else if shared.abandoned {
            Poll::Ready(None)
        } else {
            shared.waker = Some(context.waker().clone());
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::Wake;

    use super::*;

    struct Flag(AtomicBool);

    impl Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn poll_once<T>(receiver: &mut Receiver<T>, flag: &Arc<Flag>) -> Poll<Option<T>> {
        let waker = Waker::from(Arc::clone(flag));
        Pin::new(receiver).poll(&mut Context::from_waker(&waker))
    }

    #[test]
    fn a_value_sent_before_the_wait_is_there_at_once() {
        let flag = Arc::new(Flag(AtomicBool::new(false)));
        let (sender, mut receiver) = delivery();
        sender.send(7);
        assert_eq!(poll_once(&mut receiver, &flag), Poll::Ready(Some(7)));
    }

    #[test]
    fn a_value_sent_from_another_thread_wakes_the_waiting_task() {
        let flag = Arc::new(Flag(AtomicBool::new(false)));
        let (sender, mut receiver) = delivery();
        assert_eq!(poll_once(&mut receiver, &flag), Poll::Pending);
        std::thread::spawn(move || sender.send("done"))
            .join()
            .unwrap();
        assert!(flag.0.load(Ordering::SeqCst), "the task was woken");
        assert_eq!(poll_once(&mut receiver, &flag), Poll::Ready(Some("done")));
    }

    #[test]
    fn a_sender_dropped_without_sending_wakes_the_task_with_nothing() {
        let flag = Arc::new(Flag(AtomicBool::new(false)));
        let (sender, mut receiver) = delivery::<u8>();
        assert_eq!(poll_once(&mut receiver, &flag), Poll::Pending);
        drop(sender);
        assert!(flag.0.load(Ordering::SeqCst));
        assert_eq!(poll_once(&mut receiver, &flag), Poll::Ready(None));
    }
}
