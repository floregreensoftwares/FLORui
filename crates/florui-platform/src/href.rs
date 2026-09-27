//! Whether an `<a href>` is worth handing to [`crate::open_url`] at all --
//! a relative path, `mailto:`, `javascript:`, or empty `href` is left
//! alone by design (that's `florui_routing::Link`'s job, or the app's own
//! `onclick`, not this primitive's default action). The OS shell is the
//! real validator beyond this scheme check.

/// True only for an ASCII-case-insensitive `http://`/`https://` prefix.
pub(crate) fn is_openable(href: &str) -> bool {
    let lower = href.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_http_url_is_openable() {
        assert!(is_openable("http://example.com"));
    }

    #[test]
    fn a_plain_https_url_is_openable() {
        assert!(is_openable("https://example.com"));
    }

    #[test]
    fn the_scheme_check_is_case_insensitive() {
        assert!(is_openable("HTTPS://example.com"));
        assert!(is_openable("HtTp://example.com"));
    }

    #[test]
    fn a_relative_path_is_not_openable() {
        assert!(!is_openable("/about"));
    }

    #[test]
    fn a_mailto_link_is_not_openable() {
        assert!(!is_openable("mailto:hi@example.com"));
    }

    #[test]
    fn a_javascript_pseudo_url_is_not_openable() {
        assert!(!is_openable("javascript:alert(1)"));
    }

    #[test]
    fn an_empty_href_is_not_openable() {
        assert!(!is_openable(""));
    }

    #[test]
    fn a_bare_fragment_is_not_openable() {
        assert!(!is_openable("#section"));
    }
}
