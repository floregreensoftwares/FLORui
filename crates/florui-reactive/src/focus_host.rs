//! [`FocusHost`]: what a component needs from its host to follow and steer
//! keyboard focus, without depending on any platform. A native host provides
//! one through [`crate::provide_context`] as an `Rc<dyn FocusHost>` (the way
//! it provides an [`crate::Executor`]); [`use_focus_host`] reads it back.

use std::rc::Rc;

use crate::context::use_context;

/// Reports the focused element and moves focus, by the element's `id`.
pub trait FocusHost {
    /// The `id` attribute of the element that has focus, `None` when nothing
    /// does or the focused element has no `id`.
    fn focused_id(&self) -> Option<String>;

    /// Asks the host to focus the element with this `id` (or its first
    /// focusable descendant) once it is mounted.
    fn request_focus(&self, id: &str);
}

/// The host's [`FocusHost`], or `None` in a host that provides none (a
/// headless test, a future non-native target).
///
/// # Panics
///
/// Panics if called outside a [`crate::ComponentScope::render`] pass.
pub fn use_focus_host() -> Option<Rc<dyn FocusHost>> {
    use_context::<Rc<dyn FocusHost>>()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::{ComponentScope, provide_context};

    struct Recording(RefCell<Vec<String>>);

    impl FocusHost for Recording {
        fn focused_id(&self) -> Option<String> {
            Some("current".to_string())
        }

        fn request_focus(&self, id: &str) {
            self.0.borrow_mut().push(id.to_string());
        }
    }

    #[test]
    fn a_provided_host_is_reachable_and_absence_is_none() {
        let (scope, _dirty) = ComponentScope::new();
        let host = Rc::new(Recording(RefCell::default()));
        let with_host = {
            let host = Rc::clone(&host);
            scope.render(move || {
                provide_context(host as Rc<dyn FocusHost>);
                use_focus_host().map(|host| {
                    host.request_focus("next");
                    host.focused_id()
                })
            })
        };
        assert_eq!(with_host, Some(Some("current".to_string())));

        let (bare, _dirty) = ComponentScope::new();
        assert!(bare.render(|| use_focus_host().is_none()));
    }
}
