//! [`manual_future`]: a future a test resolves explicitly, instead of one
//! driven by real I/O or timers: async tests are to
//! "use controlled futures and a deterministic scheduler rather than real
//! network timing," and this is that control point for [`crate::use_resource`]
//! tests specifically.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

struct Shared<T> {
    value: RefCell<Option<T>>,
    waker: RefCell<Option<Waker>>,
}

/// Stays [`Poll::Pending`] until its paired [`Resolver::resolve`] is
/// called, however many times it's polled in between.
pub struct ManualFuture<T> {
    shared: Rc<Shared<T>>,
}

/// Resolves the [`ManualFuture`] it was created alongside.
pub struct Resolver<T> {
    shared: Rc<Shared<T>>,
}

impl<T> Resolver<T> {
    /// Delivers `value` and wakes the future if it was already polled and
    /// parked. Resolving a future more than once overwrites the pending
    /// value; a future that already observed a previous value ignores it.
    pub fn resolve(&self, value: T) {
        *self.shared.value.borrow_mut() = Some(value);
        if let Some(waker) = self.shared.waker.borrow_mut().take() {
            waker.wake();
        }
    }
}

impl<T> Future for ManualFuture<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        if let Some(value) = self.shared.value.borrow_mut().take() {
            Poll::Ready(value)
        } else {
            *self.shared.waker.borrow_mut() = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

/// A future and the handle that resolves it, for tests that need to
/// control exactly when async work completes relative to other events
/// (a key change, a cancellation, another resource's completion).
pub fn manual_future<T>() -> (ManualFuture<T>, Resolver<T>) {
    let shared = Rc::new(Shared {
        value: RefCell::new(None),
        waker: RefCell::new(None),
    });
    (
        ManualFuture {
            shared: Rc::clone(&shared),
        },
        Resolver { shared },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{Executor, LocalExecutor};
    use std::cell::Cell;

    #[test]
    fn stays_pending_until_resolved() {
        let executor = LocalExecutor::new();
        let (future, resolver) = manual_future::<i32>();
        let observed = Rc::new(Cell::new(None));
        let observed_in_task = Rc::clone(&observed);

        executor.spawn(Box::pin(async move {
            observed_in_task.set(Some(future.await));
        }));

        executor.run_until_stalled();
        assert_eq!(
            observed.get(),
            None,
            "must not resolve before Resolver::resolve is called"
        );

        resolver.resolve(42);
        executor.run_until_stalled();
        assert_eq!(observed.get(), Some(42));
    }
}
