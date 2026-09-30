//! [`loading_boundary`]: coordinates an explicit set of required
//! [`ResourceHandle`]s' readiness against a fallback. This is coordination over the existing resource
//! system, not a second executor: it never starts, cancels, or retries a
//! read itself, only decides which of two subtrees to mount based on
//! state those reads already expose.
//!
//! Not addressed here (tracked gaps, not oversights): marking the pending
//! region busy for assistive technology and restoring focus predictably
//! on fallback replacement both need the accessibility/focus systems a
//! later stage builds; optional minimum-display-time policies need a
//! shared controllable clock this crate doesn't have yet.

use crate::resource::{Resource, ResourceHandle};
use crate::{use_child_scope_keyed, use_signal};

/// One value a [`loading_boundary`] can wait on. Implemented for
/// [`ResourceHandle`]; a boundary only ever needs to know whether a read
/// has reached an outcome, never what that outcome was.
pub trait TrackedRead {
    /// Whether this read has reached a state a [`loading_boundary`] no
    /// longer needs to wait on — [`Resource::Ready`] or
    /// [`Resource::Failed`], not [`Resource::Idle`] or
    /// [`Resource::Pending`]. A failed read still counts as settled: it
    /// leaves the boundary's pending state and follows the ordinary
    /// error-boundary contract from there, the same as any other reported
    /// failure — this function does not special-case it.
    fn is_settled(&self) -> bool;
}

impl<T: Clone + 'static, E: Clone + 'static> TrackedRead for ResourceHandle<T, E> {
    fn is_settled(&self) -> bool {
        matches!(self.get(), Resource::Ready(_) | Resource::Failed { .. })
    }
}

/// Renders `fallback` until every read in `required` is settled, then
/// renders `content` and never falls back again for the lifetime of this
/// call site — a later refetch (any required read becoming unsettled
/// again) keeps `content` mounted and passes it `is_refreshing: true`
/// instead, so it can keep showing retained/stale data without flicker.
///
/// `content` gets its own persistent hook state (via an internally fixed
/// key, so its identity — and whatever local state it holds — survives a
/// refresh); `fallback` does not, since it is normally stateless and is
/// never shown again once `content` has mounted once.
///
/// `required` should list exactly the reads this boundary must wait on —
/// an optional read a component also uses but that shouldn't block reveal
/// simply isn't included here.
///
/// # Panics
///
/// Panics outside a [`crate::ComponentScope::render`] pass, or if hooks ran in a
/// different order or count than last render.
pub fn loading_boundary<T>(
    required: &[&dyn TrackedRead],
    content: impl FnOnce(bool) -> T,
    fallback: impl FnOnce() -> T,
) -> T {
    let revealed = use_signal(|| false);
    let all_settled = required.iter().all(|read| read.is_settled());
    let was_already_revealed = revealed.get();

    if !was_already_revealed {
        if !all_settled {
            return fallback();
        }
        revealed.set(true);
    }

    let is_refreshing = was_already_revealed && !all_settled;
    use_child_scope_keyed("loading_boundary_content", move || content(is_refreshing))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::executor::{Executor, LocalExecutor};
    use crate::testing::{ManualFuture, manual_future};
    use crate::{Cleanup, ComponentScope, provide_context, use_effect, use_resource};

    #[derive(Debug, Clone, PartialEq)]
    struct DemoError(&'static str);

    type DemoFuture = ManualFuture<Result<i32, DemoError>>;

    #[test]
    fn shows_the_fallback_until_every_required_read_settles() {
        let executor = Rc::new(LocalExecutor::new());
        let (a, _resolver_a) = manual_future::<Result<i32, DemoError>>();
        let (scope, _dirty) = ComponentScope::new();

        let value = scope.render(move || {
            provide_context(Rc::clone(&executor) as Rc<dyn Executor>);
            let a = use_resource("a", move |_| a);
            loading_boundary(&[&a], |_refreshing| "content", || "fallback")
        });
        assert_eq!(value, "fallback");
    }

    #[test]
    fn reveals_content_only_once_all_required_reads_are_ready() {
        let executor = Rc::new(LocalExecutor::new());
        let (scope, _dirty) = ComponentScope::new();
        let (a, resolver_a) = manual_future::<Result<i32, DemoError>>();
        let (b, resolver_b) = manual_future::<Result<i32, DemoError>>();

        let render = |executor: Rc<LocalExecutor>| {
            move || {
                provide_context(Rc::clone(&executor) as Rc<dyn Executor>);
                let a = use_resource("a", |_: &'static str| -> DemoFuture {
                    unreachable!("same key, must not refetch")
                });
                let b = use_resource("b", |_: &'static str| -> DemoFuture {
                    unreachable!("same key, must not refetch")
                });
                loading_boundary(&[&a, &b], |_| "content", || "fallback")
            }
        };

        let value = scope.render({
            let executor = Rc::clone(&executor);
            move || {
                provide_context(Rc::clone(&executor) as Rc<dyn Executor>);
                let a = use_resource("a", move |_| a);
                let b = use_resource("b", move |_| b);
                loading_boundary(&[&a, &b], |_| "content", || "fallback")
            }
        });
        assert_eq!(value, "fallback");
        executor.run_until_stalled();

        // Only one of the two required reads is ready.
        resolver_a.resolve(Ok(1));
        executor.run_until_stalled();
        let value = scope.render(render(Rc::clone(&executor)));
        assert_eq!(
            value, "fallback",
            "must keep waiting while any required read is still unsettled"
        );

        resolver_b.resolve(Ok(2));
        executor.run_until_stalled();
        let value = scope.render(render(executor));
        assert_eq!(value, "content");
    }

    fn render_with_refresh_state(
        executor: Rc<LocalExecutor>,
        fetch: Option<DemoFuture>,
    ) -> impl FnOnce() -> (&'static str, ResourceHandle<i32, DemoError>) {
        move || {
            provide_context(Rc::clone(&executor) as Rc<dyn Executor>);
            let a = use_resource("a", move |_| {
                fetch.expect("the fetch only runs when the key/retry actually changed")
            });
            let value = loading_boundary(
                &[&a],
                |refreshing| {
                    if refreshing {
                        "content-refreshing"
                    } else {
                        "content"
                    }
                },
                || "fallback",
            );
            (value, a)
        }
    }

    #[test]
    fn a_later_refetch_keeps_content_mounted_and_reports_refreshing() {
        let executor = Rc::new(LocalExecutor::new());
        let (scope, _dirty) = ComponentScope::new();
        let (first, first_resolver) = manual_future::<Result<i32, DemoError>>();

        let (value, _handle) =
            scope.render(render_with_refresh_state(Rc::clone(&executor), Some(first)));
        assert_eq!(value, "fallback");
        executor.run_until_stalled();
        first_resolver.resolve(Ok(1));
        executor.run_until_stalled();

        let (value, handle) = scope.render(render_with_refresh_state(Rc::clone(&executor), None));
        assert_eq!(value, "content");

        // Retry puts the resource back into Pending without changing its
        // key — content must stay mounted and report refreshing, never
        // fall back to the fallback again.
        handle.retry();

        let (second, second_resolver) = manual_future::<Result<i32, DemoError>>();
        // This render's own body still reads the pre-retry state — the
        // effect that starts the new fetch and commits Pending only runs
        // after this render's body already decided what to return, the
        // same ordering used throughout this crate for effect-driven
        // writes (see resource.rs's own tests).
        let (value, _handle) = scope.render(render_with_refresh_state(
            Rc::clone(&executor),
            Some(second),
        ));
        assert_eq!(value, "content");

        let (value, _handle) = scope.render(render_with_refresh_state(Rc::clone(&executor), None));
        assert_eq!(
            value, "content-refreshing",
            "a retry must not hide already-revealed content"
        );

        second_resolver.resolve(Ok(2));
        executor.run_until_stalled();
    }

    #[test]
    fn a_failed_required_read_still_counts_as_settled() {
        let executor = Rc::new(LocalExecutor::new());
        let (scope, _dirty) = ComponentScope::new();
        let (future, resolver) = manual_future::<Result<i32, DemoError>>();

        scope.render({
            let executor = Rc::clone(&executor);
            move || {
                provide_context(Rc::clone(&executor) as Rc<dyn Executor>);
                let a = use_resource("a", move |_| future);
                loading_boundary(&[&a], |_| "content", || "fallback")
            }
        });
        executor.run_until_stalled();
        resolver.resolve(Err(DemoError("boom")));
        executor.run_until_stalled();

        let value = scope.render(move || {
            provide_context(Rc::clone(&executor) as Rc<dyn Executor>);
            let a = use_resource("a", |_: &'static str| -> DemoFuture { unreachable!() });
            loading_boundary(&[&a], |_| "content", || "fallback")
        });
        assert_eq!(
            value, "content",
            "a failure settles the read; error display is content's own concern"
        );
    }

    #[test]
    fn hidden_pending_content_never_mounts_and_so_never_runs_its_effects() {
        let executor = Rc::new(LocalExecutor::new());
        let (scope, _dirty) = ComponentScope::new();
        let (future, _resolver) = manual_future::<Result<i32, DemoError>>();
        let mounted = Rc::new(RefCell::new(false));
        let mounted_in_content = Rc::clone(&mounted);

        scope.render(move || {
            provide_context(Rc::clone(&executor) as Rc<dyn Executor>);
            let a = use_resource("a", move |_| future);
            loading_boundary(
                &[&a],
                move |_| {
                    let mounted = Rc::clone(&mounted_in_content);
                    use_effect((), move || {
                        *mounted.borrow_mut() = true;
                        None::<Cleanup>
                    });
                    "content"
                },
                || "fallback",
            )
        });

        assert!(
            !*mounted.borrow(),
            "content must never even be called while its required read is still pending, \
             so a mount effect inside it must not run"
        );
    }

    #[test]
    fn a_nested_boundary_does_not_block_on_its_parents_own_required_reads() {
        let executor = Rc::new(LocalExecutor::new());
        let (scope, _dirty) = ComponentScope::new();
        let (outer, outer_resolver) = manual_future::<Result<i32, DemoError>>();
        let inner_slot: Rc<RefCell<Option<DemoFuture>>> = Rc::new(RefCell::new(None));
        let (inner, _inner_resolver) = manual_future::<Result<i32, DemoError>>();
        *inner_slot.borrow_mut() = Some(inner);

        fn render(
            executor: Rc<LocalExecutor>,
            outer_fetch: Option<DemoFuture>,
            inner_slot: Rc<RefCell<Option<DemoFuture>>>,
        ) -> impl FnOnce() -> &'static str {
            move || {
                provide_context(Rc::clone(&executor) as Rc<dyn Executor>);
                let outer_resource = use_resource("outer", move |_| {
                    outer_fetch.expect("the fetch only runs when the key actually changed")
                });
                loading_boundary(
                    &[&outer_resource],
                    move |_| {
                        let inner_resource = use_resource("inner", move |_| {
                            inner_slot
                                .borrow_mut()
                                .take()
                                .expect("the fetch only runs once")
                        });
                        loading_boundary(
                            &[&inner_resource],
                            |_| "inner-content",
                            || "inner-fallback",
                        )
                    },
                    || "outer-fallback",
                )
            }
        }

        let value = scope.render(render(
            Rc::clone(&executor),
            Some(outer),
            Rc::clone(&inner_slot),
        ));
        assert_eq!(value, "outer-fallback");
        executor.run_until_stalled();

        outer_resolver.resolve(Ok(1));
        executor.run_until_stalled();

        let value = scope.render(render(executor, None, inner_slot));
        assert_eq!(
            value, "inner-fallback",
            "once the outer settles it must reveal content immediately, even though the \
             nested boundary inside is still waiting on its own separate read"
        );
    }
}
