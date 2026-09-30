//! What a `<form>` reports when submitted: [`FormData`] through a
//! [`SubmitHandler`] (`onsubmit={move |data: FormData| ...}`).

use std::fmt;
use std::rc::Rc;

/// The named values of a form's controls at the moment of submission, in
/// tree order — real HTML's own `FormData` entry list. Collected from each
/// control's current attributes; the form never owns a value itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormData {
    entries: Vec<(String, String)>,
}

impl FormData {
    pub fn new(entries: Vec<(String, String)>) -> Self {
        Self { entries }
    }

    /// The first value entered under `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Every value entered under `name`, in order — a `<select multiple>`
    /// contributes one per selected option.
    pub fn get_all(&self, name: &str) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    pub fn has(&self, name: &str) -> bool {
        self.entries.iter().any(|(key, _)| key == name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A callback captured from `onsubmit` in `view!`.
#[derive(Clone)]
pub struct SubmitHandler(Rc<dyn Fn(FormData)>);

impl SubmitHandler {
    pub fn new(f: impl Fn(FormData) + 'static) -> Self {
        Self(Rc::new(f))
    }

    pub fn call(&self, data: FormData) {
        (self.0)(data);
    }
}

impl fmt::Debug for SubmitHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SubmitHandler(..)")
    }
}

impl PartialEq for SubmitHandler {
    /// Identity, not content — same as [`crate::Handler`].
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for SubmitHandler {}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[test]
    fn get_returns_the_first_value_and_get_all_every_one() {
        let data = FormData::new(vec![
            ("tag".into(), "a".into()),
            ("other".into(), "x".into()),
            ("tag".into(), "b".into()),
        ]);
        assert_eq!(data.get("tag"), Some("a"));
        assert_eq!(data.get_all("tag"), ["a", "b"]);
        assert!(data.has("other"));
        assert!(!data.has("missing"));
        assert_eq!(data.len(), 3);
    }

    #[test]
    fn handler_receives_the_data() {
        let seen = Rc::new(RefCell::new(None));
        let sink = seen.clone();
        let handler = SubmitHandler::new(move |data| *sink.borrow_mut() = Some(data));
        handler.call(FormData::new(vec![("a".into(), "1".into())]));
        assert_eq!(seen.borrow().as_ref().and_then(|d| d.get("a")), Some("1"));
    }
}
