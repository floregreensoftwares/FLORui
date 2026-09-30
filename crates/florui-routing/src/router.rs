//! [`Router`]: owned history (push/replace/back/forward), a guard chain
//! with the same stale-navigation-rejection guarantee
//! [`florui_reactive::use_resource`] already proves for async fetches,
//! and per-entry [`ScrollAnchor`] storage. Never constructed directly by
//! an app -- see [`crate::provide_router`].

use std::rc::Rc;

use florui_reactive::executor::{Executor, LocalBoxFuture};
use florui_reactive::{FocusHost, Ref, ScrollAnchor, Signal};

use crate::routable::Routable;

/// Which of [`Router`]'s own operations produced a navigation -- passed
/// to every [`Guard`] so it can, for example, always allow `Back`/
/// `Forward` while still gating `Push`/`Replace`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavKind {
    Push,
    Replace,
    Back,
    Forward,
}

/// What a [`Router`] navigation call actually did. `Denied` also covers
/// [`Router::back`]/[`Router::forward`] called with nowhere to go --
/// check [`Router::can_go_back`]/[`Router::can_go_forward`] first to
/// distinguish that from a real guard veto if the difference matters to
/// a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavOutcome {
    Committed,
    Denied,
    /// A guard is still resolving ([`GuardDecision::Defer`]); this
    /// specific call did not (yet) change [`Router::current`]. A later,
    /// overlapping navigation always wins -- this one silently never
    /// commits if one starts before it resolves.
    Deferred,
}

/// An in-flight navigation's disposition, decided by application code
/// before [`Router`] commits it. Not an authorization boundary (a guard
/// can always be worked around by an app calling [`Router::push`]
/// directly elsewhere) -- purely a UX mechanism, e.g. an "unsaved
/// changes" confirmation.
pub enum GuardDecision {
    Allow,
    Deny,
    /// Resolves to `true` (keep evaluating the remaining guards) or
    /// `false` (deny). Spawned on the same [`Executor`] a
    /// [`Router`] fetched at [`crate::provide_router`] time.
    Defer(LocalBoxFuture<'static, bool>),
}

/// `from`, `to`, and which kind of navigation this is. Called
/// synchronously; must not block.
pub type Guard<R> = Rc<dyn Fn(&R, &R, NavKind) -> GuardDecision>;

/// One entry in a [`Router`]'s history. `scroll_anchor` is entirely
/// application-set (via [`Router::set_current_scroll_anchor`]) and
/// application-read (via [`Router::current_entry`]) -- a [`Router`] has
/// no viewport of its own to scroll, it only carries this along per
/// entry so an app can restore it after navigating back.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry<R> {
    pub route: R,
    pub scroll_anchor: Option<ScrollAnchor>,
    /// The `id` of the element that had focus when this entry was left,
    /// recorded automatically when the host provides a [`FocusHost`]; see
    /// [`crate::use_route_focus`], which gives it back on return.
    pub focus_id: Option<String>,
}

/// A validated external navigation request -- see
/// [`Router::apply_external`]. Kept distinct from [`Router::push`]/
/// [`Router::replace`] only in name, so an app's own deep-link glue
/// reads as "this came from outside," not as an unremarkable internal
/// navigation.
pub enum ExternalNavigation<R> {
    /// A cold-start deep link: this process had no prior navigation
    /// state worth keeping a back-entry for.
    ReplaceTo(R),
    /// An existing-instance handoff: push, so `back` returns to
    /// whatever the user was already doing.
    NavigateTo(R),
}

/// Owns one route type's history and guard chain. Cheap to clone (every
/// field is `Rc`/[`Signal`]/[`Ref`]-backed); every clone reads and
/// writes the same underlying state. Constructed only by
/// [`crate::provide_router`].
pub struct Router<R: Routable> {
    entries: Signal<Vec<HistoryEntry<R>>>,
    index: Signal<usize>,
    guards: Rc<Vec<Guard<R>>>,
    /// Bumped at the start of every [`Self::navigate`] call; a deferred
    /// guard's continuation checks this before committing, exactly
    /// mirroring `use_resource`'s own stale-completion rejection.
    generation: Ref<u64>,
    executor: Rc<dyn Executor>,
    /// Bumped only once a navigation actually commits -- distinct from
    /// `generation`, which bumps on every *attempt*. What
    /// [`crate::use_route_transition`] watches.
    transition_generation: Signal<u64>,
    last_transition_kind: Ref<NavKind>,
    /// The host's focus access, when it provides one: each entry records the
    /// element that had focus as it is left.
    focus_host: Option<Rc<dyn FocusHost>>,
}

impl<R: Routable> Clone for Router<R> {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            index: self.index.clone(),
            guards: Rc::clone(&self.guards),
            generation: self.generation.clone(),
            executor: Rc::clone(&self.executor),
            transition_generation: self.transition_generation.clone(),
            last_transition_kind: self.last_transition_kind.clone(),
            focus_host: self.focus_host.clone(),
        }
    }
}

impl<R: Routable> Router<R> {
    /// `entries` must be non-empty -- [`crate::provide_router`] is the
    /// only caller, and it always seeds exactly one entry from the
    /// app's initial route before constructing this.
    pub(crate) fn new(
        entries: Signal<Vec<HistoryEntry<R>>>,
        index: Signal<usize>,
        guards: Rc<Vec<Guard<R>>>,
        generation: Ref<u64>,
        executor: Rc<dyn Executor>,
        transition_generation: Signal<u64>,
        last_transition_kind: Ref<NavKind>,
    ) -> Self {
        Self {
            entries,
            index,
            guards,
            generation,
            executor,
            transition_generation,
            last_transition_kind,
            focus_host: None,
        }
    }

    /// Gives this router the host's [`FocusHost`], if any.
    pub(crate) fn with_focus_host(mut self, focus_host: Option<Rc<dyn FocusHost>>) -> Self {
        self.focus_host = focus_host;
        self
    }

    fn clamped_index(&self, entries_len: usize) -> usize {
        self.index.get().min(entries_len.saturating_sub(1))
    }

    pub fn current(&self) -> R {
        self.current_entry().route
    }

    pub fn current_entry(&self) -> HistoryEntry<R> {
        let entries = self.entries.get();
        let index = self.clamped_index(entries.len());
        entries[index].clone()
    }

    pub fn can_go_back(&self) -> bool {
        self.clamped_index(self.entries.get().len()) > 0
    }

    pub fn can_go_forward(&self) -> bool {
        let entries = self.entries.get();
        self.clamped_index(entries.len()) + 1 < entries.len()
    }

    /// Records `anchor` (or clears it) against the *current* entry --
    /// call this just before navigating away from it, so
    /// [`Self::current_entry`] returns it again on the way back.
    pub fn set_current_scroll_anchor(&self, anchor: Option<ScrollAnchor>) {
        let mut entries = self.entries.get();
        let index = self.clamped_index(entries.len());
        if let Some(entry) = entries.get_mut(index) {
            entry.scroll_anchor = anchor;
        }
        self.entries.set(entries);
    }

    pub fn push(&self, route: R) -> NavOutcome {
        self.navigate(route, NavKind::Push)
    }

    pub fn replace(&self, route: R) -> NavOutcome {
        self.navigate(route, NavKind::Replace)
    }

    pub fn back(&self) -> NavOutcome {
        let entries = self.entries.get();
        let index = self.clamped_index(entries.len());
        let Some(target) = index.checked_sub(1).and_then(|i| entries.get(i)) else {
            return NavOutcome::Denied;
        };
        self.navigate(target.route.clone(), NavKind::Back)
    }

    pub fn forward(&self) -> NavOutcome {
        let entries = self.entries.get();
        let index = self.clamped_index(entries.len());
        let Some(target) = entries.get(index + 1) else {
            return NavOutcome::Denied;
        };
        self.navigate(target.route.clone(), NavKind::Forward)
    }

    pub fn apply_external(&self, nav: ExternalNavigation<R>) -> NavOutcome {
        match nav {
            ExternalNavigation::ReplaceTo(route) => self.replace(route),
            ExternalNavigation::NavigateTo(route) => self.push(route),
        }
    }

    pub(crate) fn transition_generation_value(&self) -> u64 {
        self.transition_generation.get()
    }

    pub(crate) fn last_transition_kind(&self) -> NavKind {
        self.last_transition_kind.get()
    }

    /// Runs the guard chain for `target`, committing synchronously if
    /// every guard resolves without deferring, or spawning the
    /// remainder on [`Self::executor`] and returning
    /// [`NavOutcome::Deferred`] the moment one does.
    fn navigate(&self, target: R, kind: NavKind) -> NavOutcome {
        let from = self.current();
        let my_generation = self.generation.get() + 1;
        self.generation.set(my_generation);

        for (index, guard) in self.guards.iter().enumerate() {
            match guard(&from, &target, kind) {
                GuardDecision::Allow => continue,
                GuardDecision::Deny => return NavOutcome::Denied,
                GuardDecision::Defer(future) => {
                    self.spawn_deferred_continuation(
                        future,
                        self.guards[index + 1..].to_vec(),
                        from,
                        target,
                        kind,
                        my_generation,
                    );
                    return NavOutcome::Deferred;
                }
            }
        }

        self.commit(target, kind);
        NavOutcome::Committed
    }

    fn spawn_deferred_continuation(
        &self,
        first: LocalBoxFuture<'static, bool>,
        remaining: Vec<Guard<R>>,
        from: R,
        target: R,
        kind: NavKind,
        my_generation: u64,
    ) {
        let router = self.clone();
        self.executor.spawn(Box::pin(async move {
            let mut allowed = first.await;
            if allowed {
                for guard in remaining.iter() {
                    match guard(&from, &target, kind) {
                        GuardDecision::Allow => continue,
                        GuardDecision::Deny => {
                            allowed = false;
                            break;
                        }
                        GuardDecision::Defer(future) => {
                            allowed = future.await;
                            if !allowed {
                                break;
                            }
                        }
                    }
                }
            }
            // Mirrors use_resource's own stale-completion check: an
            // overlapping newer navigate() call already bumped
            // `generation`, so this continuation's own commit is
            // silently dropped instead of overwriting a newer result.
            if allowed && router.generation.get() == my_generation {
                router.commit(target, kind);
            }
        }));
    }

    /// Records which element has focus against the entry being left, so
    /// returning to it can give that focus back. A replaced entry is gone,
    /// so there is nothing to keep.
    fn record_focus(&self) {
        let Some(host) = &self.focus_host else {
            return;
        };
        let mut entries = self.entries.get();
        let index = self.clamped_index(entries.len());
        if let Some(entry) = entries.get_mut(index) {
            entry.focus_id = host.focused_id();
        }
        self.entries.set(entries);
    }

    fn commit(&self, route: R, kind: NavKind) {
        if kind != NavKind::Replace {
            self.record_focus();
        }
        match kind {
            NavKind::Push => {
                let mut entries = self.entries.get();
                let index = self.clamped_index(entries.len());
                entries.truncate(index + 1);
                entries.push(HistoryEntry {
                    route,
                    scroll_anchor: None,
                    focus_id: None,
                });
                let new_index = entries.len() - 1;
                self.entries.set(entries);
                self.index.set(new_index);
            }
            NavKind::Replace => {
                let mut entries = self.entries.get();
                let index = self.clamped_index(entries.len());
                if let Some(entry) = entries.get_mut(index) {
                    *entry = HistoryEntry {
                        route,
                        scroll_anchor: None,
                        focus_id: None,
                    };
                }
                self.entries.set(entries);
            }
            NavKind::Back => {
                let index = self.clamped_index(self.entries.get().len());
                self.index.set(index.saturating_sub(1));
            }
            NavKind::Forward => {
                let entries_len = self.entries.get().len();
                let index = self.clamped_index(entries_len);
                self.index
                    .set((index + 1).min(entries_len.saturating_sub(1)));
            }
        }
        self.last_transition_kind.set(kind);
        self.transition_generation
            .set(self.transition_generation.get() + 1);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use florui_reactive::executor::LocalExecutor;
    use florui_reactive::testing::manual_future;
    use florui_reactive::{ComponentScope, use_ref, use_signal};

    use super::*;
    use crate::routable::RouteError;

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum TestRoute {
        A,
        B,
        C,
    }

    impl Routable for TestRoute {
        fn parse(path: &str) -> Result<Self, RouteError> {
            match path {
                "/a" => Ok(TestRoute::A),
                "/b" => Ok(TestRoute::B),
                "/c" => Ok(TestRoute::C),
                _ => Err(RouteError::Unknown {
                    path: path.to_owned(),
                }),
            }
        }

        fn format(&self) -> String {
            match self {
                TestRoute::A => "/a".to_owned(),
                TestRoute::B => "/b".to_owned(),
                TestRoute::C => "/c".to_owned(),
            }
        }
    }

    fn build_router(
        scope: &ComponentScope,
        executor: &Rc<LocalExecutor>,
        initial: TestRoute,
        guards: Vec<Guard<TestRoute>>,
    ) -> Router<TestRoute> {
        let executor = Rc::clone(executor) as Rc<dyn Executor>;
        scope.render(move || {
            let entries = use_signal(|| {
                vec![HistoryEntry {
                    route: initial,
                    scroll_anchor: None,
                    focus_id: None,
                }]
            });
            let index = use_signal(|| 0usize);
            let generation = use_ref(|| 0u64);
            let transition_generation = use_signal(|| 0u64);
            let last_transition_kind = use_ref(|| NavKind::Replace);
            Router::new(
                entries,
                index,
                Rc::new(guards),
                generation,
                executor,
                transition_generation,
                last_transition_kind,
            )
        })
    }

    #[test]
    fn push_appends_a_new_entry_and_becomes_current() {
        let (scope, _dirty) = ComponentScope::new();
        let executor = Rc::new(LocalExecutor::new());
        let router = build_router(&scope, &executor, TestRoute::A, Vec::new());

        assert_eq!(router.push(TestRoute::B), NavOutcome::Committed);
        assert_eq!(router.current(), TestRoute::B);
        assert!(router.can_go_back());
        assert!(!router.can_go_forward());
    }

    #[test]
    fn back_then_forward_returns_to_the_same_route() {
        let (scope, _dirty) = ComponentScope::new();
        let executor = Rc::new(LocalExecutor::new());
        let router = build_router(&scope, &executor, TestRoute::A, Vec::new());
        router.push(TestRoute::B);

        assert_eq!(router.back(), NavOutcome::Committed);
        assert_eq!(router.current(), TestRoute::A);
        assert_eq!(router.forward(), NavOutcome::Committed);
        assert_eq!(router.current(), TestRoute::B);
    }

    #[test]
    fn back_at_the_start_of_history_is_denied_and_changes_nothing() {
        let (scope, _dirty) = ComponentScope::new();
        let executor = Rc::new(LocalExecutor::new());
        let router = build_router(&scope, &executor, TestRoute::A, Vec::new());

        assert_eq!(router.back(), NavOutcome::Denied);
        assert_eq!(router.current(), TestRoute::A);
    }

    #[test]
    fn forward_past_the_end_of_history_is_denied_and_changes_nothing() {
        let (scope, _dirty) = ComponentScope::new();
        let executor = Rc::new(LocalExecutor::new());
        let router = build_router(&scope, &executor, TestRoute::A, Vec::new());

        assert_eq!(router.forward(), NavOutcome::Denied);
        assert_eq!(router.current(), TestRoute::A);
    }

    #[test]
    fn pushing_after_going_back_truncates_the_abandoned_forward_history() {
        let (scope, _dirty) = ComponentScope::new();
        let executor = Rc::new(LocalExecutor::new());
        let router = build_router(&scope, &executor, TestRoute::A, Vec::new());
        router.push(TestRoute::B);
        router.back();

        router.push(TestRoute::C);
        assert_eq!(router.current(), TestRoute::C);
        assert!(
            !router.can_go_forward(),
            "B must be gone from forward history once a new route was pushed from A"
        );
    }

    #[test]
    fn replace_overwrites_the_current_entry_without_growing_history() {
        let (scope, _dirty) = ComponentScope::new();
        let executor = Rc::new(LocalExecutor::new());
        let router = build_router(&scope, &executor, TestRoute::A, Vec::new());
        router.push(TestRoute::B);

        assert_eq!(router.replace(TestRoute::C), NavOutcome::Committed);
        assert_eq!(router.current(), TestRoute::C);
        assert_eq!(router.back(), NavOutcome::Committed);
        assert_eq!(
            router.current(),
            TestRoute::A,
            "replace must not add its own back-entry"
        );
    }

    #[test]
    fn set_current_scroll_anchor_is_returned_by_current_entry() {
        let (scope, _dirty) = ComponentScope::new();
        let executor = Rc::new(LocalExecutor::new());
        let router = build_router(&scope, &executor, TestRoute::A, Vec::new());
        let anchor = ScrollAnchor::new(florui_reactive::Key::from("item-3"), 12.0);

        router.set_current_scroll_anchor(Some(anchor.clone()));
        assert_eq!(router.current_entry().scroll_anchor, Some(anchor));

        router.push(TestRoute::B);
        assert_eq!(
            router.current_entry().scroll_anchor,
            None,
            "a freshly pushed entry starts with no scroll anchor"
        );
    }

    #[test]
    fn apply_external_navigate_to_pushes_a_new_entry() {
        let (scope, _dirty) = ComponentScope::new();
        let executor = Rc::new(LocalExecutor::new());
        let router = build_router(&scope, &executor, TestRoute::A, Vec::new());

        router.apply_external(ExternalNavigation::NavigateTo(TestRoute::B));
        assert_eq!(router.current(), TestRoute::B);
        assert!(router.can_go_back());
    }

    #[test]
    fn apply_external_replace_to_does_not_add_a_back_entry() {
        let (scope, _dirty) = ComponentScope::new();
        let executor = Rc::new(LocalExecutor::new());
        let router = build_router(&scope, &executor, TestRoute::A, Vec::new());

        router.apply_external(ExternalNavigation::ReplaceTo(TestRoute::B));
        assert_eq!(router.current(), TestRoute::B);
        assert!(!router.can_go_back());
    }

    #[test]
    fn a_synchronous_guard_can_deny_a_navigation() {
        let (scope, _dirty) = ComponentScope::new();
        let executor = Rc::new(LocalExecutor::new());
        let deny_all: Guard<TestRoute> = Rc::new(|_from, _to, _kind| GuardDecision::Deny);
        let router = build_router(&scope, &executor, TestRoute::A, vec![deny_all]);

        assert_eq!(router.push(TestRoute::B), NavOutcome::Denied);
        assert_eq!(router.current(), TestRoute::A);
    }

    #[test]
    fn a_deferred_guard_commits_once_resolved_true() {
        let (scope, _dirty) = ComponentScope::new();
        let executor = Rc::new(LocalExecutor::new());
        let (future, resolver) = manual_future::<bool>();
        let future_cell = Rc::new(RefCell::new(Some(future)));
        let defer_guard: Guard<TestRoute> = Rc::new(move |_from, _to, _kind| {
            let future = future_cell
                .borrow_mut()
                .take()
                .expect("this test navigates exactly once");
            GuardDecision::Defer(Box::pin(future))
        });
        let router = build_router(&scope, &executor, TestRoute::A, vec![defer_guard]);

        assert_eq!(router.push(TestRoute::B), NavOutcome::Deferred);
        assert_eq!(
            router.current(),
            TestRoute::A,
            "must not commit before the deferred guard resolves"
        );

        resolver.resolve(true);
        executor.run_until_stalled();
        assert_eq!(router.current(), TestRoute::B);
    }

    #[test]
    fn a_deferred_guard_that_resolves_false_denies_the_navigation() {
        let (scope, _dirty) = ComponentScope::new();
        let executor = Rc::new(LocalExecutor::new());
        let (future, resolver) = manual_future::<bool>();
        let future_cell = Rc::new(RefCell::new(Some(future)));
        let defer_guard: Guard<TestRoute> = Rc::new(move |_from, _to, _kind| {
            let future = future_cell
                .borrow_mut()
                .take()
                .expect("this test navigates exactly once");
            GuardDecision::Defer(Box::pin(future))
        });
        let router = build_router(&scope, &executor, TestRoute::A, vec![defer_guard]);

        router.push(TestRoute::B);
        resolver.resolve(false);
        executor.run_until_stalled();
        assert_eq!(router.current(), TestRoute::A);
    }

    #[test]
    fn a_guard_after_a_resolved_defer_still_runs_and_can_deny() {
        let (scope, _dirty) = ComponentScope::new();
        let executor = Rc::new(LocalExecutor::new());
        let (future, resolver) = manual_future::<bool>();
        let future_cell = Rc::new(RefCell::new(Some(future)));
        let defer_guard: Guard<TestRoute> = Rc::new(move |_from, _to, _kind| {
            let future = future_cell
                .borrow_mut()
                .take()
                .expect("this test navigates exactly once");
            GuardDecision::Defer(Box::pin(future))
        });
        let deny_guard: Guard<TestRoute> = Rc::new(|_from, _to, _kind| GuardDecision::Deny);
        let router = build_router(
            &scope,
            &executor,
            TestRoute::A,
            vec![defer_guard, deny_guard],
        );

        router.push(TestRoute::B);
        resolver.resolve(true);
        executor.run_until_stalled();
        assert_eq!(
            router.current(),
            TestRoute::A,
            "a later guard denying after an earlier one's defer resolved must still block the navigation"
        );
    }

    #[test]
    fn an_overlapping_navigation_makes_a_stale_deferred_guard_never_commit() {
        let (scope, _dirty) = ComponentScope::new();
        let executor = Rc::new(LocalExecutor::new());
        let (slow_future, slow_resolver) = manual_future::<bool>();
        let slow_future_cell = Rc::new(RefCell::new(Some(slow_future)));
        let defer_for_b_only: Guard<TestRoute> = Rc::new(move |_from, to, _kind| {
            if *to == TestRoute::B {
                let future = slow_future_cell
                    .borrow_mut()
                    .take()
                    .expect("this test navigates to B exactly once");
                GuardDecision::Defer(Box::pin(future))
            } else {
                GuardDecision::Allow
            }
        });
        let router = build_router(&scope, &executor, TestRoute::A, vec![defer_for_b_only]);

        assert_eq!(router.push(TestRoute::B), NavOutcome::Deferred);
        assert_eq!(router.current(), TestRoute::A);

        // A newer navigation, started before the slow one resolves, commits
        // immediately (no defer for C).
        assert_eq!(router.push(TestRoute::C), NavOutcome::Committed);
        assert_eq!(router.current(), TestRoute::C);

        // The stale B navigation finally resolves -- it must not overwrite
        // the newer C navigation that already committed.
        slow_resolver.resolve(true);
        executor.run_until_stalled();
        assert_eq!(
            router.current(),
            TestRoute::C,
            "a navigation that started before a newer one must never overwrite it \
             once that newer one already committed"
        );
    }
}
