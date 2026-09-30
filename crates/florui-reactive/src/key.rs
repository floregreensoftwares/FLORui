//! [`Key`]: the identity a caller assigns a component instance so its
//! state survives even when its position among its siblings changes — see
//! [`crate::use_child_scope_keyed`].

/// An identity distinct from call position. Built via [`From`] from
/// anything [`std::fmt::Display`] (a `String`, an integer id, ...), so
/// `key={item.id}` works whether `id` is a string or a number — two keys
/// that stringify the same are the same key, matching how most UI
/// frameworks treat keys in practice.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key(String);

impl Key {
    /// The key's string form, for building an id from it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<T: std::fmt::Display> From<T> for Key {
    fn from(value: T) -> Self {
        Key(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_from_different_displayable_types_compare_by_their_string_form() {
        assert_eq!(Key::from("42"), Key::from(42));
        assert_ne!(Key::from("a"), Key::from("b"));
    }
}
