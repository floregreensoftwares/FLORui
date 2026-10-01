//! [`use_resource`]: component-owned async work with typed idle/pending/
//! ready/failed states.
//!
//! Every fetch is tied to an internal generation counter: starting a new
//! one (because the key changed, or [`ResourceHandle::retry`] was called)
//! both best-effort aborts whatever was still in flight *and* bumps the
//! generation, so even a completion that slips past the abort is rejected
//! at commit time — a slow old response can never overwrite a newer
//! result. Owner disposal (the component unmounting) cancels the same way,
//! reusing [`crate::use_effect`]'s existing "cleanup on unmount" contract
//! rather than adding a second lifecycle mechanism.

use std::future::Future;
use std::rc::Rc;

use futures::future::AbortHandle;

use crate::executor::Executor;
use crate::{Cleanup, Ref, Signal, use_context, use_effect, use_ref, use_signal};

/// One [`use_resource`] run's outcome for its current request key.
#[derive(Debug, Clone)]
pub enum Resource<T, E> {
    /// No fetch has started for the current key yet.
    Idle,
    /// A fetch for the current key is in flight. `stale` carries the
    /// previous key's result, if any is available, so a caller can keep
    /// showing it instead of flashing to empty while waiting.
    Pending { stale: Option<T> },
    /// The current key's fetch completed successfully.
    Ready(T),
    /// The current key's fetch failed. `stale` carries the previous
    /// key's result the same way [`Self::Pending`]'s does.
    Failed { error: E, stale: Option<T> },
}

impl<T, E> Resource<T, E> {
    /// The most recent data available: fresh from [`Self::Ready`], or
    /// retained from before a refetch or failure. `None` only when no
    /// fetch for any key has ever completed.
    pub fn data(&self) -> Option<&T> {
        match self {
            Resource::Idle => None,
            Resource::Pending { stale } | Resource::Failed { stale, .. } => stale.as_ref(),
            Resource::Ready(value) => Some(value),
        }
    }

    /// The current key's error, if its fetch failed.
    pub fn error(&self) -> Option<&E> {
        match self {
            Resource::Failed { error, .. } => Some(error),
            _ => None,
        }
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, Resource::Pending { .. })
    }
}

/// What [`use_resource`] returns: the current [`Resource`] state plus the
/// explicit actions it comes with — retry and
/// cancellation are never automatic.
pub struct ResourceHandle<T, E> {
    state: Signal<Resource<T, E>>,
    retry_nonce: Signal<u64>,
    generation: Ref<u64>,
    abort_handle: Ref<Option<AbortHandle>>,
}

impl<T, E> Clone for ResourceHandle<T, E> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            retry_nonce: self.retry_nonce.clone(),
            generation: self.generation.clone(),
            abort_handle: self.abort_handle.clone(),
        }
    }
}

impl<T: Clone + 'static, E: Clone + 'static> ResourceHandle<T, E> {
    pub fn get(&self) -> Resource<T, E> {
        self.state.get()
    }

    /// Starts the current key's fetch over again — the explicit,
    /// user-triggered retry action for a failed (or otherwise unsatisfying)
    /// result; a resource never retries on its own.
    pub fn retry(&self) {
        self.retry_nonce.set(self.retry_nonce.get() + 1);
    }

    /// Best-effort aborts the in-flight fetch, if any, and rejects
    /// whatever it would have completed with. The resource returns to
    /// [`Resource::Idle`]: an explicit cancellation leaves no fetch
    /// outcome for the current key to report.
    pub fn cancel(&self) {
        self.generation.with_mut(|generation| *generation += 1);
        if let Some(handle) = self.abort_handle.with_mut(Option::take) {
            handle.abort();
        }
        self.state.set(Resource::Idle);
    }
}

/// Starts (or continues) an async fetch keyed on `key`: `fetch` runs again
/// only when `key` changes from the previous render's (compared the same
/// way as [`crate::use_effect`]'s dependency) or [`ResourceHandle::retry`]
/// is called — never merely because the owning component re-rendered.
///
/// Requires an [`Executor`] reachable via [`crate::use_context`]; a host
/// provides one (typically a [`crate::executor::LocalExecutor`] or its own
/// adapter) once per render at its root, the same way [`crate::use_context`]
/// requires any other provider to.
///
/// # Panics
///
/// Panics outside a [`crate::ComponentScope::render`] pass, if hooks ran in a
/// different order or count than last render, or if no [`Executor`] is
/// reachable via context.
pub fn use_resource<K, T, E, F, Fut>(key: K, fetch: F) -> ResourceHandle<T, E>
where
    K: PartialEq + Clone + 'static,
    T: Clone + 'static,
    E: Clone + 'static,
    F: FnOnce(K) -> Fut + 'static,
    Fut: Future<Output = Result<T, E>> + 'static,
{
    let state = use_signal(|| Resource::<T, E>::Idle);
    let retry_nonce = use_signal(|| 0_u64);
    let generation = use_ref(|| 0_u64);
    let abort_handle = use_ref(|| None::<AbortHandle>);
    let executor = use_context::<Rc<dyn Executor>>().unwrap_or_else(|| {
        panic!(
            "use_resource needs an Executor reachable via use_context — the host must \
             provide_context an Executor (e.g. Rc<LocalExecutor>) at its render root"
        )
    });

    use_effect((key.clone(), retry_nonce.get()), {
        let state = state.clone();
        let generation = generation.clone();
        let abort_handle = abort_handle.clone();
        move || start_fetch(key, fetch, state, generation, abort_handle, executor)
    });

    ResourceHandle {
        state,
        retry_nonce,
        generation,
        abort_handle,
    }
}

fn start_fetch<K, T, E, F, Fut>(
    key: K,
    fetch: F,
    state: Signal<Resource<T, E>>,
    generation: Ref<u64>,
    abort_handle: Ref<Option<AbortHandle>>,
    executor: Rc<dyn Executor>,
) -> Option<Cleanup>
where
    T: Clone + 'static,
    E: Clone + 'static,
    F: FnOnce(K) -> Fut + 'static,
    Fut: Future<Output = Result<T, E>> + 'static,
{
    let my_generation = generation.with_mut(|generation| {
        *generation += 1;
        *generation
    });

    let stale = state.get().data().cloned();
    crate::trace::record_kind(crate::trace::TraceKind::ResourceStarted);
    crate::trace::with_origin(crate::trace::Origin::ResourceStart, || {
        state.set(Resource::Pending { stale })
    });

    let (abortable, handle) = futures::future::abortable(fetch(key));
    abort_handle.set(Some(handle));

    // The commit below usually lands well after this render — via a real
    // async completion, outside any component's render call — so the
    // trace it produces would otherwise be attributed to nothing. Capture
    // whichever component started this fetch now, while it's still on the
    // stack, and restore it around that later write.
    let component = crate::trace::current_component();
    let state_for_task = state.clone();
    let generation_for_task = generation.clone();
    executor.spawn(Box::pin(async move {
        // `Err` here means this run was aborted; either way it was
        // rejected before reaching the generation check, so it never
        // gets a chance to overwrite a newer run's result.
        if let Ok(result) = abortable.await
            && generation_for_task.get() == my_generation
        {
            let commit = || {
                crate::trace::record_kind(crate::trace::TraceKind::ResourceCompleted);
                crate::trace::with_origin(crate::trace::Origin::ResourceCompletion, || {
                    let stale = state_for_task.get().data().cloned();
                    state_for_task.set(match result {
                        Ok(value) => Resource::Ready(value),
                        Err(error) => Resource::Failed { error, stale },
                    });
                });
            };
            match component {
                Some(component) => crate::trace::with_component(component, commit),
                None => commit(),
            }
        }
    }));

    Some(Box::new(move || {
        if let Some(handle) = abort_handle.with_mut(Option::take) {
            handle.abort();
        }
    }) as Cleanup)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::executor::LocalExecutor;
    use crate::testing::{Resolver, manual_future};
    use crate::{ComponentScope, provide_context};

    fn with_executor<R>(f: impl FnOnce(&Rc<LocalExecutor>) -> R) -> R {
        f(&Rc::new(LocalExecutor::new()))
    }

    #[test]
    fn starts_idle_and_becomes_pending_once_the_fetch_is_running() {
        with_executor(|executor| {
            let (scope, _dirty) = ComponentScope::new();
            let executor = Rc::clone(executor);

            let handle = scope.render(move || {
                provide_context(Rc::clone(&executor) as Rc<dyn Executor>);
                use_resource("key", |_| async { Ok::<i32, &'static str>(1) })
            });

            assert!(
                handle.get().is_pending(),
                "the effect that starts the fetch runs synchronously after commit, \
                 before the future itself is ever polled"
            );
        });
    }

    #[test]
    fn a_completed_fetch_becomes_ready_and_keeps_that_data_after_a_refetch_starts() {
        with_executor(|executor| {
            let (scope, _dirty) = ComponentScope::new();
            let (future, resolver) = manual_future::<Result<i32, &'static str>>();
            let executor_for_render = Rc::clone(executor);

            let handle = scope.render(move || {
                provide_context(Rc::clone(&executor_for_render) as Rc<dyn Executor>);
                use_resource("a", move |_| future)
            });
            executor.run_until_stalled();
            resolver.resolve(Ok(7));
            executor.run_until_stalled();
            assert!(matches!(handle.get(), Resource::Ready(7)));

            // A refetch (new key) goes back to Pending, but keeps the old
            // value observable as `stale` instead of dropping it.
            let executor_for_render = Rc::clone(executor);
            scope.render(move || {
                provide_context(Rc::clone(&executor_for_render) as Rc<dyn Executor>);
                use_resource("b", |_| async { Ok::<i32, &'static str>(99) })
            });
            match handle.get() {
                Resource::Pending { stale: Some(7) } => {}
                other => {
                    panic!("expected Pending with the previous key's data retained, got {other:?}")
                }
            }
        });
    }

    #[test]
    fn a_failed_fetch_reports_the_error_and_keeps_prior_data_as_stale() {
        with_executor(|executor| {
            let (scope, _dirty) = ComponentScope::new();
            let (first, first_resolver) = manual_future::<Result<i32, &'static str>>();
            let executor_for_render = Rc::clone(executor);

            let handle = scope.render(move || {
                provide_context(Rc::clone(&executor_for_render) as Rc<dyn Executor>);
                use_resource("a", move |_| first)
            });
            executor.run_until_stalled();
            first_resolver.resolve(Ok(1));
            executor.run_until_stalled();
            assert!(matches!(handle.get(), Resource::Ready(1)));

            let (second, second_resolver) = manual_future::<Result<i32, &'static str>>();
            let executor_for_render = Rc::clone(executor);
            scope.render(move || {
                provide_context(Rc::clone(&executor_for_render) as Rc<dyn Executor>);
                use_resource("b", move |_| second)
            });
            executor.run_until_stalled();
            second_resolver.resolve(Err("boom"));
            executor.run_until_stalled();

            match handle.get() {
                Resource::Failed {
                    error: "boom",
                    stale: Some(1),
                } => {}
                other => panic!(
                    "expected Failed(\"boom\") with the previous key's data retained, got {other:?}"
                ),
            }
        });
    }

    #[test]
    fn out_of_order_responses_do_not_let_an_older_run_overwrite_a_newer_one() {
        with_executor(|executor| {
            let (scope, _dirty) = ComponentScope::new();
            let (slow, slow_resolver) = manual_future::<Result<i32, &'static str>>();
            let executor_for_render = Rc::clone(executor);

            let handle = scope.render(move || {
                provide_context(Rc::clone(&executor_for_render) as Rc<dyn Executor>);
                use_resource("a", move |_| slow)
            });
            executor.run_until_stalled();

            // Re-key before the slow first run ever resolves.
            let (fast, fast_resolver) = manual_future::<Result<i32, &'static str>>();
            let executor_for_render = Rc::clone(executor);
            scope.render(move || {
                provide_context(Rc::clone(&executor_for_render) as Rc<dyn Executor>);
                use_resource("b", move |_| fast)
            });
            executor.run_until_stalled();

            fast_resolver.resolve(Ok(2));
            executor.run_until_stalled();
            assert!(matches!(handle.get(), Resource::Ready(2)));

            // The stale run finally resolves — must not overwrite "b"'s result.
            slow_resolver.resolve(Ok(1));
            executor.run_until_stalled();
            assert!(
                matches!(handle.get(), Resource::Ready(2)),
                "a completion for an old key/generation must be rejected, not committed"
            );
        });
    }

    #[test]
    fn cancelling_before_completion_prevents_the_result_from_committing() {
        with_executor(|executor| {
            let (scope, _dirty) = ComponentScope::new();
            let (future, resolver) = manual_future::<Result<i32, &'static str>>();
            let executor_for_render = Rc::clone(executor);

            let handle = scope.render(move || {
                provide_context(Rc::clone(&executor_for_render) as Rc<dyn Executor>);
                use_resource("a", move |_| future)
            });
            executor.run_until_stalled();

            handle.cancel();
            assert!(matches!(handle.get(), Resource::Idle));

            resolver.resolve(Ok(1));
            executor.run_until_stalled();
            assert!(
                matches!(handle.get(), Resource::Idle),
                "a completion after explicit cancellation must not commit"
            );
        });
    }

    #[test]
    fn cancelling_after_completion_is_a_harmless_no_op() {
        with_executor(|executor| {
            let (scope, _dirty) = ComponentScope::new();
            let (future, resolver) = manual_future::<Result<i32, &'static str>>();
            let executor_for_render = Rc::clone(executor);

            let handle = scope.render(move || {
                provide_context(Rc::clone(&executor_for_render) as Rc<dyn Executor>);
                use_resource("a", move |_| future)
            });
            executor.run_until_stalled();
            resolver.resolve(Ok(1));
            executor.run_until_stalled();
            assert!(matches!(handle.get(), Resource::Ready(1)));

            handle.cancel();
            assert!(matches!(handle.get(), Resource::Idle));
        });
    }

    #[test]
    fn unmounting_cancels_the_in_flight_fetch_and_a_late_completion_is_dropped() {
        with_executor(|executor| {
            let (scope, _dirty) = ComponentScope::new();
            let (future, resolver) = manual_future::<Result<i32, &'static str>>();
            let executor_for_render = Rc::clone(executor);

            let handle = scope.render(move || {
                provide_context(Rc::clone(&executor_for_render) as Rc<dyn Executor>);
                use_resource("a", move |_| future)
            });
            executor.run_until_stalled();

            drop(scope);

            resolver.resolve(Ok(1));
            executor.run_until_stalled();
            assert!(
                matches!(handle.get(), Resource::Pending { stale: None }),
                "a completion after the owning scope is disposed must not commit"
            );
        });
    }

    type PendingResolvers = Rc<RefCell<Vec<Resolver<Result<i32, &'static str>>>>>;

    #[test]
    fn retry_reruns_the_fetch_for_the_same_key() {
        with_executor(|executor| {
            let (scope, _dirty) = ComponentScope::new();
            let call_count = Rc::new(std::cell::Cell::new(0));
            let resolvers: PendingResolvers = Rc::new(RefCell::new(Vec::new()));

            let render = {
                let call_count = Rc::clone(&call_count);
                let resolvers = Rc::clone(&resolvers);
                let executor = Rc::clone(executor);
                move || {
                    provide_context(Rc::clone(&executor) as Rc<dyn Executor>);
                    let call_count = Rc::clone(&call_count);
                    let resolvers = Rc::clone(&resolvers);
                    use_resource("a", move |_| {
                        call_count.set(call_count.get() + 1);
                        let (future, resolver) = manual_future::<Result<i32, &'static str>>();
                        resolvers.borrow_mut().push(resolver);
                        future
                    })
                }
            };

            let handle = scope.render(render.clone());
            executor.run_until_stalled();
            resolvers.borrow_mut().remove(0).resolve(Ok(1));
            executor.run_until_stalled();
            assert!(matches!(handle.get(), Resource::Ready(1)));
            assert_eq!(call_count.get(), 1);

            handle.retry();
            scope.render(render);
            executor.run_until_stalled();
            assert_eq!(
                call_count.get(),
                2,
                "retry must start a new fetch for the same key"
            );
            assert!(matches!(handle.get(), Resource::Pending { stale: Some(1) }));

            resolvers.borrow_mut().remove(0).resolve(Ok(2));
            executor.run_until_stalled();
            assert!(matches!(handle.get(), Resource::Ready(2)));
        });
    }
}
