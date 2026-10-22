//! Configuring how `http://` and `https://` sources are loaded: the `src` of an `<img>`
//! and the `url(...)` of a stylesheet. Needs the `net` feature.
//!
//! Every picture of the process loads through one fetcher, so its queue, its limits and
//! its disk cache are shared by all windows. Call [`configure`] before the first window
//! opens; without it the defaults apply (no disk cache, proxy from the environment). See
//! `florui_assets::net` for the loading contract.

use std::path::PathBuf;

pub use florui_assets::net::{NetConfig, ProxyMode};

/// Sets the configuration of the process-wide fetcher. Returns `false`, changing
/// nothing, when a picture has already been asked for.
pub fn configure(config: NetConfig) -> bool {
    florui_assets::net::configure(config)
}

/// A per-user, per-application directory for [`NetConfig::cache_dir`]:
/// `%LOCALAPPDATA%\<app>\http-cache` on Windows, `$XDG_CACHE_HOME/<app>/http-cache`
/// (else `~/.cache/<app>/http-cache`) on Linux. `None` where the platform has no rule
/// here yet, or when the variables it needs are not set.
///
/// `None` too for an identifier that is not one name: empty, `.` or `..`, or with a path
/// separator in it. The directory is cleaned of the files this crate writes, so it must not
/// be able to point outside the cache of the user.
pub fn cache_dir(app_identifier: &str) -> Option<PathBuf> {
    let one_name = !app_identifier.is_empty()
        && app_identifier != "."
        && app_identifier != ".."
        && !app_identifier.contains(['/', '\\', '\0', ':']);
    if !one_name {
        return None;
    }
    cache_root().map(|root| root.join(app_identifier).join("http-cache"))
}

fn cache_root() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    }
    #[cfg(target_os = "linux")]
    {
        xdg_cache_root(std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME"))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        None
    }
}

/// The XDG base directory rule: `$XDG_CACHE_HOME` if it is an absolute path, else
/// `$HOME/.cache`. A relative value is ignored, as the specification requires.
#[cfg(any(target_os = "linux", test))]
fn xdg_cache_root(
    cache_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let absolute =
        |value: std::ffi::OsString| Some(PathBuf::from(value)).filter(|p| p.is_absolute());
    cache_home
        .and_then(absolute)
        .or_else(|| home.and_then(absolute).map(|home| home.join(".cache")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn an_identifier_that_is_not_one_name_has_no_cache_directory() {
        for bad in [
            "", ".", "..", "../..", "/tmp/x", "a/b", "a\\b", "C:\\x", "a:b", "a\0b",
        ] {
            assert_eq!(cache_dir(bad), None, "{bad:?}");
        }
        // Whether the platform has a root at all, a reverse-domain name is a name.
        if cache_root().is_some() {
            let dir = cache_dir("com.example.app").unwrap();
            assert!(
                dir.ends_with("com.example.app/http-cache")
                    || dir.ends_with("com.example.app\\http-cache")
            );
        }
    }

    #[test]
    fn the_xdg_cache_home_wins_when_it_is_absolute() {
        let root = xdg_cache_root(
            Some(OsString::from("/var/cache")),
            Some(OsString::from("/home/a")),
        );
        assert_eq!(root, Some(PathBuf::from("/var/cache")));
    }

    #[test]
    fn a_relative_or_missing_cache_home_falls_back_to_the_home_cache() {
        let home = Some(OsString::from("/home/a"));
        assert_eq!(
            xdg_cache_root(Some(OsString::from("relative")), home.clone()),
            Some(PathBuf::from("/home/a/.cache"))
        );
        assert_eq!(
            xdg_cache_root(None, home),
            Some(PathBuf::from("/home/a/.cache"))
        );
        assert_eq!(xdg_cache_root(None, Some(OsString::from("home"))), None);
        assert_eq!(xdg_cache_root(None, None), None);
    }
}
