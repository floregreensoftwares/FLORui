//! `<input>` needs behavior declared per `type`, not presumed for every
//! value HTML defines.

/// `type` values `<input>` accepts for now. Starting narrow and explicit;
/// expand deliberately as each type gets real behavior, not to match the
/// full HTML list up front.
pub const INITIAL_INPUT_TYPES: &[&str] =
    &["text", "password", "checkbox", "radio", "range", "hidden"];

pub fn is_supported_input_type(value: &str) -> bool {
    INITIAL_INPUT_TYPES.contains(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_initial_types() {
        for ty in INITIAL_INPUT_TYPES {
            assert!(is_supported_input_type(ty));
        }
    }

    #[test]
    fn rejects_unlisted_types() {
        assert!(!is_supported_input_type("date"));
        assert!(!is_supported_input_type("color"));
    }
}
