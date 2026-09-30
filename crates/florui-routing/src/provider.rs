//! [`provide_router`]: what an app calls inside its own `root()` to
//! create and provide a [`Router`] -- the same "plain generic fn an
//! app's own `#[component]` calls into" shape as
//! [`florui_reactive::loading_boundary`]/[`florui_reactive::error_boundary`],
//! not a `#[component]` itself.

use std::rc::Rc;

use florui::Element;
use florui_reactive::executor::Executor;
use florui_reactive::{
    provide_context, use_context, use_effect, use_focus_host, use_ref, use_signal,
};

use crate::routable::Routable;
use crate::router::{Guard, HistoryEntry, NavKind, Router};

/// Creates (on first render) or resumes (on every later render) this
/// route type's [`Router`] and provides it via context for
/// [`use_router`]/[`use_route`]/[`route_outlet`](crate::route_outlet) to
/// read, then renders `children`. `provide_context` is re-run
/// unconditionally on every call -- a provided value is cleared at the
/// start of every render, so this must never be skipped or gated behind
/// a condition.
///
/// # Panics
///
/// Panics if no [`Executor`] is reachable via
/// [`florui_reactive::use_context`] -- the same requirement
/// [`florui_reactive::use_resource`] already has; provide one (e.g.
/// `Rc::new(florui_reactive::executor::LocalExecutor::new()) as Rc<dyn Executor>`)
/// once at the app's render root.
pub fn provide_router<R: Routable>(
    initial: R,
    guards: Vec<Guard<R>>,
    children: impl FnOnce() -> Element,
) -> Element {
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
    let executor = use_context::<Rc<dyn Executor>>().unwrap_or_else(|| {
        panic!(
            "provide_router needs an Executor reachable via use_context -- provide one \
             (e.g. Rc::new(florui_reactive::executor::LocalExecutor::new()) as Rc<dyn Executor>) \
             at the app's render root, the same one use_resource itself requires"
        )
    });
    let router = Router::new(
        entries,
        index,
        Rc::new(guards),
        generation,
        executor,
        transition_generation,
        last_transition_kind,
    )
    .with_focus_host(use_focus_host());
    provide_context(router);
    children()
}

/// Reads the [`Router`] a [`provide_router`] above this component
/// provided.
///
/// # Panics
///
/// Panics if there is no matching `provide_router::<R>` anywhere above
/// this call in the tree.
pub fn use_router<R: Routable>() -> Router<R> {
    use_context::<Router<R>>()
        .unwrap_or_else(|| panic!("use_router::<R> called with no provide_router::<R> above it"))
}

/// The current route -- shorthand for `use_router::<R>().current()`.
pub fn use_route<R: Routable>() -> R {
    use_router::<R>().current()
}

/// `on_committed` runs once after every navigation that actually commits
/// (including the very first render, reported as [`NavKind::Replace`] --
/// there is no prior navigation to distinguish it from). Use this for
/// app-driven focus/scroll restoration: [`crate::Router`] has no
/// viewport or window handle of its own to act on either directly.
pub fn use_route_transition<R: Routable>(on_committed: impl Fn(NavKind) + 'static) {
    let router = use_router::<R>();
    let generation = router.transition_generation_value();
    let router_for_effect = router;
    use_effect(generation, move || {
        on_committed(router_for_effect.last_transition_kind());
        None
    });
}

/// Moves focus on every committed navigation, for a host that provides a
/// [`florui_reactive::FocusHost`] (none: does nothing). Back and Forward
/// return focus to the element, by `id`, the user left that entry on; a Push
/// or Replace, or a return with nothing remembered, lands on
/// `entry_point(route)`, the `id` of the page's own starting element. A target
/// that has not mounted yet is waited for, and one that never appears is
/// dropped. Scroll stays the app's own, through
/// [`crate::Router::set_current_scroll_anchor`].
pub fn use_route_focus<R: Routable>(entry_point: impl Fn(&R) -> Option<String> + 'static) {
    let router = use_router::<R>();
    let host = use_focus_host();
    use_route_transition::<R>(move |kind| {
        let Some(host) = &host else {
            return;
        };
        let entry = router.current_entry();
        let saved = match kind {
            NavKind::Back | NavKind::Forward => entry.focus_id.clone(),
            NavKind::Push | NavKind::Replace => None,
        };
        if let Some(id) = saved.or_else(|| entry_point(&entry.route)) {
            host.request_focus(&id);
        }
    });
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use florui_reactive::ComponentScope;
    use florui_reactive::executor::LocalExecutor;

    use super::*;
    use crate::routable::RouteError;

    #[derive(Debug, Clone, PartialEq)]
    enum TestRoute {
        Home,
        About,
    }

    impl Routable for TestRoute {
        fn parse(path: &str) -> Result<Self, RouteError> {
            match path {
                "/" => Ok(TestRoute::Home),
                "/about" => Ok(TestRoute::About),
                _ => Err(RouteError::Unknown {
                    path: path.to_owned(),
                }),
            }
        }

        fn format(&self) -> String {
            match self {
                TestRoute::Home => "/".to_owned(),
                TestRoute::About => "/about".to_owned(),
            }
        }
    }

    fn with_executor_provided<T>(scope: &ComponentScope, render: impl FnOnce() -> T) -> T {
        let executor = Rc::new(LocalExecutor::new()) as Rc<dyn Executor>;
        scope.render(move || {
            provide_context(executor);
            render()
        })
    }

    fn empty_page() -> Element {
        Element::Fragment(Vec::new())
    }

    #[test]
    fn use_route_reads_the_initial_route() {
        let (scope, _dirty) = ComponentScope::new();
        let route = with_executor_provided(&scope, || {
            provide_router(TestRoute::Home, Vec::new(), empty_page);
            use_route::<TestRoute>()
        });
        assert_eq!(route, TestRoute::Home);
    }

    #[test]
    fn the_router_persists_navigation_across_renders() {
        let (scope, _dirty) = ComponentScope::new();
        let router = with_executor_provided(&scope, || {
            provide_router(TestRoute::Home, Vec::new(), empty_page);
            use_router::<TestRoute>()
        });
        router.push(TestRoute::About);

        // provide_context is cleared at the start of every render, so
        // provide_router re-provides a "new" Router value each time -- but
        // its Signal/Ref fields are the same persisted cells from the
        // first render (same call-site slots), so the navigation above
        // must still be visible on this second render.
        let route_on_second_render = with_executor_provided(&scope, || {
            provide_router(TestRoute::Home, Vec::new(), empty_page);
            use_route::<TestRoute>()
        });
        assert_eq!(route_on_second_render, TestRoute::About);
    }

    use std::cell::RefCell;

    use florui_reactive::FocusHost;

    #[derive(Default)]
    struct MockHost {
        focused: RefCell<Option<String>>,
        requests: RefCell<Vec<String>>,
    }

    impl FocusHost for MockHost {
        fn focused_id(&self) -> Option<String> {
            self.focused.borrow().clone()
        }

        fn request_focus(&self, id: &str) {
            self.requests.borrow_mut().push(id.to_string());
        }
    }

    /// Renders an app with `use_route_focus` (entry point `entry-<path>`),
    /// optionally under `host`, and returns its router.
    fn focus_app(scope: &ComponentScope, host: Option<&Rc<MockHost>>) -> Router<TestRoute> {
        let executor = Rc::new(LocalExecutor::new()) as Rc<dyn Executor>;
        let host = host.map(|host| Rc::clone(host) as Rc<dyn FocusHost>);
        scope.render(move || {
            provide_context(executor);
            if let Some(host) = host {
                provide_context(host);
            }
            provide_router(TestRoute::Home, Vec::new(), || {
                use_route_focus::<TestRoute>(|route| Some(format!("entry-{}", route.format())));
                empty_page()
            });
            use_router::<TestRoute>()
        })
    }

    #[test]
    fn navigation_moves_focus_to_the_entry_point_and_back_returns_to_where_it_was() {
        let (scope, _dirty) = ComponentScope::new();
        let host = Rc::new(MockHost::default());
        let router = focus_app(&scope, Some(&host));
        assert_eq!(*host.requests.borrow(), ["entry-/"], "first render");

        *host.focused.borrow_mut() = Some("search".to_string());
        router.push(TestRoute::About);
        focus_app(&scope, Some(&host));
        assert_eq!(host.requests.borrow().last().unwrap(), "entry-/about");

        *host.focused.borrow_mut() = Some("email".to_string());
        router.back();
        focus_app(&scope, Some(&host));
        assert_eq!(
            host.requests.borrow().last().unwrap(),
            "search",
            "back returns to the control the user left"
        );

        router.forward();
        focus_app(&scope, Some(&host));
        assert_eq!(
            host.requests.borrow().last().unwrap(),
            "email",
            "and forward to the one left on the other page"
        );
    }

    #[test]
    fn a_return_with_nothing_remembered_lands_on_the_entry_point() {
        let (scope, _dirty) = ComponentScope::new();
        let host = Rc::new(MockHost::default());
        let router = focus_app(&scope, Some(&host));
        router.push(TestRoute::About);
        focus_app(&scope, Some(&host));

        router.back();
        focus_app(&scope, Some(&host));
        assert_eq!(host.requests.borrow().last().unwrap(), "entry-/");
    }

    #[test]
    fn replace_goes_to_the_entry_point_and_forgets_the_old_focus() {
        let (scope, _dirty) = ComponentScope::new();
        let host = Rc::new(MockHost::default());
        let router = focus_app(&scope, Some(&host));
        *host.focused.borrow_mut() = Some("search".to_string());
        router.replace(TestRoute::About);
        focus_app(&scope, Some(&host));
        assert_eq!(host.requests.borrow().last().unwrap(), "entry-/about");
        assert_eq!(router.current_entry().focus_id, None);
    }

    #[test]
    fn without_a_focus_host_navigation_just_works() {
        let (scope, _dirty) = ComponentScope::new();
        let router = focus_app(&scope, None);
        router.push(TestRoute::About);
        focus_app(&scope, None);
        assert_eq!(router.current(), TestRoute::About);
        assert_eq!(router.current_entry().focus_id, None);
    }
    #[test]
    #[should_panic(expected = "no provide_router")]
    fn use_router_without_a_provider_panics() {
        let (scope, _dirty) = ComponentScope::new();
        scope.render(|| {
            let _ = use_router::<TestRoute>();
        });
    }

    #[test]
    #[should_panic(expected = "Executor reachable via use_context")]
    fn provide_router_without_an_executor_panics() {
        let (scope, _dirty) = ComponentScope::new();
        scope.render(|| {
            provide_router(TestRoute::Home, Vec::new(), empty_page);
        });
    }
}
