//! Addresses kept once for the life of the program, so a value that names one
//! can stay `Copy`. A stylesheet names few.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

/// `text`, as a string that lives for the whole program and is the same one for
/// every call with the same text.
pub(crate) fn intern(text: &str) -> &'static str {
    static KEPT: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut kept = KEPT
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(found) = kept.get(text) {
        return found;
    }
    let leaked: &'static str = Box::leak(text.to_owned().into_boxed_str());
    kept.insert(leaked);
    leaked
}
