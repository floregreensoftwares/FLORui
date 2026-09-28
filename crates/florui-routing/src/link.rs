//! [`link`]: a real `<a>` wired to a [`crate::Router`] -- the missing
//! piece between `<a>` itself (real focus, keyboard activation, `:link`/
//! `:visited` styling) and this crate's own navigation.

use florui::{Element, Event, view};

use crate::provider::use_router;
use crate::routable::Routable;

/// Renders `<a href={to.format()}>{children()}</a>`; a plain click calls
/// `event.prevent_default()` and `router.push(to.clone())` instead of
/// `<a>`'s own default action (which would no-op anyway --
/// `to.format()` is always an internal route path, never an absolute
/// `http(s)://` URL `href.rs::is_openable` would match -- calling
/// `prevent_default()` regardless is still the honest way to say this
/// function fully owns the click, not an unrelated gate's incidental
/// non-match).
///
/// A plain generic function, not a `#[component]`: the macro rejects
/// generic type parameters outright, and this crate already has two
/// functions in exactly this shape for the same reason -- see
/// [`crate::provide_router`]/[`crate::route_outlet`]'s own doc. Called
/// as an expression child the same way `route_outlet` already is:
/// `{link(&AppRoute::About, || view! { "About" })}`.
///
/// # Panics
///
/// Panics if there is no matching `provide_router::<R>` anywhere above
/// this call in the tree -- see [`use_router`].
pub fn link<R: Routable>(to: &R, children: impl FnOnce() -> Element) -> Element {
    let router = use_router::<R>();
    let href = to.format();
    let target = to.clone();
    view! {
        <a
            href={href}
            onclick={move |event: &Event| {
                event.prevent_default();
                router.push(target.clone());
            }}
        >
            {children()}
        </a>
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use florui::Element;
    use florui_reactive::executor::{Executor, LocalExecutor};
    use florui_reactive::{ComponentScope, provide_context};
    use florui_style::Arena;

    use super::*;
    use crate::provider::provide_router;
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

    #[test]
    fn href_matches_the_targets_own_formatted_path() {
        let (scope, _dirty) = ComponentScope::new();
        let tree = with_executor_provided(&scope, || {
            provide_router(TestRoute::Home, Vec::new(), || {
                link(&TestRoute::About, || Element::text("About"))
            })
        });
        let arena = Arena::build(&tree);
        let a = arena.roots()[0];
        assert_eq!(arena.tag(a), "a");
        assert_eq!(arena.href(a), Some("/about"));
        assert_eq!(arena.text_content(a), "About");
    }

    #[test]
    fn clicking_prevents_default_and_pushes_the_target_route() {
        let (scope, _dirty) = ComponentScope::new();
        let (tree, router) = with_executor_provided(&scope, || {
            let tree = provide_router(TestRoute::Home, Vec::new(), || {
                link(&TestRoute::About, || Element::text("About"))
            });
            (tree, use_router::<TestRoute>())
        });
        let arena = Arena::build(&tree);
        let a = arena.roots()[0];
        let handler = arena.handler(a, "click").expect("link must have onclick");
        let event = Event::new();

        handler.call(&event);

        assert!(event.default_prevented());
        assert_eq!(router.current(), TestRoute::About);
    }

    #[test]
    fn two_links_navigate_independently() {
        let (scope, _dirty) = ComponentScope::new();
        let (tree, router) = with_executor_provided(&scope, || {
            let tree = provide_router(TestRoute::Home, Vec::new(), || {
                Element::Fragment(vec![
                    link(&TestRoute::About, || Element::text("About")),
                    link(&TestRoute::Home, || Element::text("Home")),
                ])
            });
            (tree, use_router::<TestRoute>())
        });
        let arena = Arena::build(&tree);
        let about_link = arena.find(|a, id| a.href(id) == Some("/about")).unwrap();
        let home_link = arena.find(|a, id| a.href(id) == Some("/")).unwrap();

        arena
            .handler(about_link, "click")
            .unwrap()
            .call(&Event::new());
        assert_eq!(router.current(), TestRoute::About);

        arena
            .handler(home_link, "click")
            .unwrap()
            .call(&Event::new());
        assert_eq!(router.current(), TestRoute::Home);
    }
}
