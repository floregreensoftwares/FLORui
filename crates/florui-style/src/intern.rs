//! Addresses kept once for the life of the program, so a value that names one
//! can stay `Copy`. A stylesheet names few.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

/// How many addresses are kept.
const LIMIT: usize = 4096;

/// `text`, as a string that lives for the whole program and is the same one for
/// every call with the same text.
pub(crate) fn intern(text: &str) -> &'static str {
    static KEPT: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut kept = KEPT
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    intern_in(&mut kept, LIMIT, text)
}

/// [`intern`] in `kept`, which holds at most `limit` addresses: past that, new
/// ones name nothing (the empty string), since a stylesheet that makes that many
/// is generating them and each would be kept for ever.
fn intern_in(kept: &mut HashSet<&'static str>, limit: usize, text: &str) -> &'static str {
    if let Some(found) = kept.get(text) {
        return found;
    }
    if kept.len() >= limit {
        return "";
    }
    let leaked: &'static str = Box::leak(text.to_owned().into_boxed_str());
    kept.insert(leaked);
    leaked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_text_is_the_same_string() {
        assert!(std::ptr::eq(intern("a#b"), intern("a#b")));
    }

    #[test]
    fn a_table_past_its_limit_names_nothing_new_and_keeps_what_it_has() {
        let mut kept = HashSet::new();
        let first = intern_in(&mut kept, 2, "one");
        intern_in(&mut kept, 2, "two");
        assert_eq!(intern_in(&mut kept, 2, "three"), "");
        assert!(std::ptr::eq(intern_in(&mut kept, 2, "one"), first));
    }
}
