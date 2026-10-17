//! Lists a text style names (a `font-family` stack, OpenType features and
//! variation settings) kept once under a number, so a [`crate::TextStyle`]
//! stays small and `Copy` and two styles with the same list compare equal.
//! The lists are CSS source text, which Parley reads as is.

use std::sync::{Arc, Mutex, OnceLock};

#[derive(Default)]
struct Table {
    sources: Vec<Arc<str>>,
    /// The number of each list, so one already known is found without a scan.
    index: std::collections::HashMap<Arc<str>, u32>,
}

impl Table {
    fn intern(&mut self, css: &str) -> u32 {
        if let Some(&id) = self.index.get(css) {
            return id;
        }
        let source: Arc<str> = Arc::from(css);
        self.sources.push(Arc::clone(&source));
        let id = self.sources.len() as u32;
        self.index.insert(source, id);
        id
    }

    fn get(&self, id: u32) -> Option<Arc<str>> {
        let at = usize::try_from(id.checked_sub(1)?).ok()?;
        self.sources.get(at).cloned()
    }
}

fn lock(table: &'static OnceLock<Mutex<Table>>) -> std::sync::MutexGuard<'static, Table> {
    table
        .get_or_init(|| Mutex::new(Table::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

static FAMILIES: OnceLock<Mutex<Table>> = OnceLock::new();
static FEATURES: OnceLock<Mutex<Table>> = OnceLock::new();
static VARIATIONS: OnceLock<Mutex<Table>> = OnceLock::new();

/// A `font-family` stack in CSS syntax (`Arial, "Open Sans", serif`), as a number
/// for [`crate::FontFamily::Stack`].
pub fn intern_family_stack(css: &str) -> u32 {
    lock(&FAMILIES).intern(css)
}

pub(crate) fn family_stack(id: u32) -> Option<Arc<str>> {
    if id == 0 {
        return None;
    }
    lock(&FAMILIES).get(id)
}

/// OpenType features in CSS syntax (`"liga" 0, "ss01"`); `0` is none.
pub fn intern_features(css: &str) -> u32 {
    if css.trim().is_empty() {
        return 0;
    }
    lock(&FEATURES).intern(css)
}

pub(crate) fn features(id: u32) -> Option<Arc<str>> {
    if id == 0 {
        return None;
    }
    lock(&FEATURES).get(id)
}

/// Variable font axes in CSS syntax (`"wght" 650, "wdth" 90`); `0` is none.
pub fn intern_variations(css: &str) -> u32 {
    if css.trim().is_empty() {
        return 0;
    }
    lock(&VARIATIONS).intern(css)
}

pub(crate) fn variations(id: u32) -> Option<Arc<str>> {
    if id == 0 {
        return None;
    }
    lock(&VARIATIONS).get(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_keeps_its_number_and_zero_is_none() {
        let a = intern_features("\"liga\" 0, \"ss01\"");
        let b = intern_features("\"smcp\"");
        assert_ne!(a, 0);
        assert_ne!(a, b);
        assert_eq!(a, intern_features("\"liga\" 0, \"ss01\""));
        assert_eq!(&*features(a).unwrap(), "\"liga\" 0, \"ss01\"");
        assert_eq!(intern_features("  "), 0);
        assert!(features(0).is_none());
        let stack = intern_family_stack("Arial, serif");
        assert_eq!(&*family_stack(stack).unwrap(), "Arial, serif");
    }
}
