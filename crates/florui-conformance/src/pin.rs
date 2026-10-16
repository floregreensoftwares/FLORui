//! The pinned Chromium ("Chrome for Testing") build this harness expects,
//! closing the revision-pinning gap this crate previously left open.
//!
//! `scripts/fetch-chromium.ps1` (Windows) and `scripts/fetch-chromium.sh`
//! (Linux) read the same `chromium-pin.json` this module embeds, so there is
//! exactly one place recording the pinned version — updating it means
//! updating that one file.

use std::path::PathBuf;

use serde::Deserialize;

const PIN_JSON: &str = include_str!("../chromium-pin.json");

#[derive(Debug, Clone, Deserialize)]
pub struct ChromiumPin {
    pub version: String,
    pub revision: String,
    pub channel: String,
    /// The Windows build: `platform`, `url` and `sha256`.
    pub platform: String,
    pub url: String,
    pub sha256: String,
    /// The same version for Linux.
    pub linux64: Option<PlatformBuild>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlatformBuild {
    pub url: String,
    pub sha256: String,
}

/// The pinned build declared in `chromium-pin.json`, embedded at compile
/// time. Panics if that file is missing or malformed — a build-time
/// invariant of this crate, not a runtime condition callers should expect
/// to recover from.
pub fn pin() -> ChromiumPin {
    serde_json::from_str(PIN_JSON)
        .expect("crates/florui-conformance/chromium-pin.json must be valid JSON")
}

/// Where the fetch script for this host extracts the pinned build to,
/// relative to the current working directory — matching how this tool's
/// other defaults (e.g. `--fixture`) already resolve relative to cwd rather
/// than to the crate/repo root.
pub fn default_executable_path() -> PathBuf {
    let pin = pin();
    let root = PathBuf::from(".tools").join("chromium").join(pin.version);
    if cfg!(windows) {
        root.join("chrome-win64").join("chrome.exe")
    } else {
        root.join("chrome-linux64").join("chrome")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_file_parses_and_is_internally_consistent() {
        let pin = pin();
        assert!(!pin.version.is_empty());
        assert!(pin.url.contains(&pin.version));
        assert_eq!(
            pin.sha256.len(),
            64,
            "sha256 should be a 64-hex-char digest"
        );
    }

    #[test]
    fn the_linux_build_is_the_same_version_with_its_own_digest() {
        let pin = pin();
        let linux = pin.linux64.expect("a Linux build is pinned");
        assert!(linux.url.contains(&pin.version));
        assert!(linux.url.contains("/linux64/"));
        assert_eq!(linux.sha256.len(), 64);
        assert_ne!(linux.sha256, pin.sha256, "a different archive");
    }

    #[test]
    fn default_executable_path_is_scoped_under_tools_and_the_pinned_version() {
        let path = default_executable_path();
        let pin = pin();
        assert!(path.starts_with(PathBuf::from(".tools").join("chromium")));
        assert!(path.to_string_lossy().contains(&pin.version));
        let name = if cfg!(windows) {
            "chrome.exe"
        } else {
            "chrome"
        };
        assert_eq!(path.file_name().unwrap(), name);
    }
}
