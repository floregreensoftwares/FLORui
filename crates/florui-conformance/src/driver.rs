//! Drives a Chromium-family browser over the Chrome DevTools Protocol to
//! capture a deterministic reference screenshot and element geometry for a
//! [`ReferenceFixture`].
//!
//! This is a plain, blocking facade: [`ChromiumDriver`] owns a private tokio
//! runtime internally, so no async type crosses into `florui-devtools` or
//! `florui-cli`, which stay fully synchronous.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use chromiumoxide::Browser;
use chromiumoxide::browser::BrowserConfig;
use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::error::CdpError;
use chromiumoxide::page::ScreenshotParams;
use futures::StreamExt;
use image::RgbaImage;
use serde::Deserialize;

use crate::geometry::BoxGeometryPx;
use crate::reference_fixture::ReferenceFixture;

pub struct ChromiumOptions {
    /// Path to a Chromium-family binary (Chrome, Chromium, or Edge).
    ///
    /// This slice takes a caller-supplied path rather than a vendored,
    /// pinned binary: fetching and pinning an exact "Chrome for Testing"
    /// revision is tracked as a follow-up, not implemented here.
    pub executable: PathBuf,
    /// A directory dedicated to this browser instance's profile. **Must be
    /// isolated** (e.g. `target/florui-conformance/chrome-profile-<pid>`) —
    /// without it, launching against a Chromium binary that already has a
    /// running instance performs a single-instance IPC handoff into that
    /// existing (possibly the user's real, already-open) session instead of
    /// starting an independent one.
    pub user_data_dir: PathBuf,
    pub headless: bool,
    pub launch_timeout: Duration,
}

impl Default for ChromiumOptions {
    fn default() -> Self {
        Self {
            executable: PathBuf::new(),
            user_data_dir: PathBuf::new(),
            headless: true,
            launch_timeout: Duration::from_secs(30),
        }
    }
}

pub struct ChromiumCapture {
    pub image: RgbaImage,
    pub element_box_css_px: BoxGeometryPx,
}

pub struct ChromiumDriver {
    runtime: tokio::runtime::Runtime,
    browser: Browser,
    _handler_task: tokio::task::JoinHandle<()>,
}

#[derive(Debug)]
pub enum DriverError {
    Configure {
        message: String,
    },
    Launch {
        executable: PathBuf,
        source: CdpError,
    },
    InvalidPath {
        path: PathBuf,
    },
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Navigate {
        url: String,
        source: CdpError,
    },
    Evaluate {
        expression: String,
        source: CdpError,
    },
    DecodeValue {
        expression: String,
        source: serde_json::Error,
    },
    Screenshot {
        source: CdpError,
    },
    DecodeScreenshot {
        source: image::ImageError,
    },
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DriverError::Configure { message } => {
                write!(f, "invalid Chromium configuration: {message}")
            }
            DriverError::Launch { executable, source } => write!(
                f,
                "could not launch Chromium at {}: {source}",
                executable.display()
            ),
            DriverError::InvalidPath { path } => {
                write!(f, "could not turn {} into a file:// URL", path.display())
            }
            DriverError::Io { path, source } => {
                write!(f, "could not resolve {}: {source}", path.display())
            }
            DriverError::Navigate { url, source } => {
                write!(f, "could not navigate to {url}: {source}")
            }
            DriverError::Evaluate { expression, source } => {
                write!(f, "could not evaluate `{expression}`: {source}")
            }
            DriverError::DecodeValue { expression, source } => {
                write!(f, "could not decode the result of `{expression}`: {source}")
            }
            DriverError::Screenshot { source } => {
                write!(f, "could not capture screenshot: {source}")
            }
            DriverError::DecodeScreenshot { source } => {
                write!(f, "could not decode captured screenshot: {source}")
            }
        }
    }
}

impl std::error::Error for DriverError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DriverError::Configure { .. } | DriverError::InvalidPath { .. } => None,
            DriverError::Launch { source, .. } => Some(source),
            DriverError::Io { source, .. } => Some(source),
            DriverError::Navigate { source, .. } => Some(source),
            DriverError::Evaluate { source, .. } => Some(source),
            DriverError::DecodeValue { source, .. } => Some(source),
            DriverError::Screenshot { source } => Some(source),
            DriverError::DecodeScreenshot { source } => Some(source),
        }
    }
}

#[derive(Deserialize)]
struct RawRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// Chromium starts its sandbox with an unprivileged user namespace, which it
/// cannot do as root or where the kernel forbids them (Ubuntu 23.10 and later
/// restrict them by default). The harness loads only this repository's own
/// fixture files, so there it starts Chromium without one rather than not at
/// all.
fn sandbox_unavailable() -> bool {
    #[cfg(target_os = "linux")]
    {
        let restrict =
            std::fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns").ok();
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        sandbox_blocked(restrict.as_deref(), effective_uid(&status) == Some(0))
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

#[cfg(any(target_os = "linux", test))]
fn sandbox_blocked(restrict_userns: Option<&str>, root: bool) -> bool {
    root || restrict_userns.is_some_and(|value| value.trim() == "1")
}

/// The effective user id in the text of `/proc/self/status`.
#[cfg(any(target_os = "linux", test))]
fn effective_uid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

impl ChromiumDriver {
    pub fn launch(options: ChromiumOptions) -> Result<Self, DriverError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to start the conformance harness's internal tokio runtime");

        let mut builder = BrowserConfig::builder()
            .chrome_executable(&options.executable)
            .user_data_dir(&options.user_data_dir)
            .launch_timeout(options.launch_timeout);
        builder = if options.headless {
            builder.new_headless_mode()
        } else {
            builder.with_head()
        };
        if sandbox_unavailable() {
            builder = builder.no_sandbox();
        }
        // Extra command-line switches for Chromium, for finding out what a
        // difference between the two renderers depends on.
        if let Ok(extra) = std::env::var("FLORUI_CHROMIUM_ARGS") {
            for switch in extra.split_whitespace() {
                builder = builder.arg(switch.trim_start_matches("--"));
            }
        }
        // Chromium on Linux draws text with subpixel anti-aliasing where the
        // background is opaque; Florui draws it in grayscale, as Chromium does
        // on Windows, so the comparison asks for the same.
        #[cfg(target_os = "linux")]
        {
            builder = builder.arg("disable-lcd-text");
        }
        let config = builder
            .build()
            .map_err(|message| DriverError::Configure { message })?;

        let (browser, mut handler) =
            runtime
                .block_on(Browser::launch(config))
                .map_err(|source| DriverError::Launch {
                    executable: options.executable.clone(),
                    source,
                })?;

        // chromiumoxide requires the Handler stream to be polled continuously
        // for the CDP connection to make progress; this background task is
        // its documented usage pattern.
        let handler_task = runtime.spawn(async move { while handler.next().await.is_some() {} });

        Ok(Self {
            runtime,
            browser,
            _handler_task: handler_task,
        })
    }

    pub fn capture(&self, fixture: &ReferenceFixture) -> Result<ChromiumCapture, DriverError> {
        self.runtime.block_on(self.capture_async(fixture))
    }

    async fn capture_async(
        &self,
        fixture: &ReferenceFixture,
    ) -> Result<ChromiumCapture, DriverError> {
        let html_path = fixture
            .html_path()
            .canonicalize()
            .map_err(|source| DriverError::Io {
                path: fixture.html_path(),
                source,
            })?;
        let url = url::Url::from_file_path(&html_path).map_err(|()| DriverError::InvalidPath {
            path: html_path.clone(),
        })?;

        let page = self
            .browser
            .new_page("about:blank")
            .await
            .map_err(|source| DriverError::Navigate {
                url: "about:blank".to_owned(),
                source,
            })?;

        let viewport = &fixture.manifest.viewport;
        let metrics = SetDeviceMetricsOverrideParams::new(
            i64::from(viewport.width_css_px),
            i64::from(viewport.height_css_px),
            viewport.device_pixel_ratio,
            false,
        );
        page.execute(metrics)
            .await
            .map_err(|source| DriverError::Navigate {
                url: url.to_string(),
                source,
            })?;

        page.goto(url.as_str())
            .await
            .map_err(|source| DriverError::Navigate {
                url: url.to_string(),
                source,
            })?;
        page.wait_for_navigation()
            .await
            .map_err(|source| DriverError::Navigate {
                url: url.to_string(),
                source,
            })?;

        // Determinism: wait for web fonts before screenshotting. This
        // fixture has none, but wiring this now avoids a silent trap on the
        // next fixture that does.
        let fonts_ready_js = "document.fonts.ready.then(() => true)";
        let _: bool = page
            .evaluate(fonts_ready_js)
            .await
            .map_err(|source| DriverError::Evaluate {
                expression: fonts_ready_js.to_owned(),
                source,
            })?
            .into_value()
            .map_err(|source| DriverError::DecodeValue {
                expression: fonts_ready_js.to_owned(),
                source,
            })?;

        let rect_js = format!(
            "(() => {{ const r = document.getElementById({:?}).getBoundingClientRect(); return {{x:r.x,y:r.y,width:r.width,height:r.height}}; }})()",
            fixture.manifest.element_id
        );
        let raw_rect: RawRect = page
            .evaluate(rect_js.as_str())
            .await
            .map_err(|source| DriverError::Evaluate {
                expression: rect_js.clone(),
                source,
            })?
            .into_value()
            .map_err(|source| DriverError::DecodeValue {
                expression: rect_js.clone(),
                source,
            })?;

        let screenshot_bytes = page
            .screenshot(
                ScreenshotParams::builder()
                    .format(CaptureScreenshotFormat::Png)
                    .build(),
            )
            .await
            .map_err(|source| DriverError::Screenshot { source })?;
        let image = image::load_from_memory(&screenshot_bytes)
            .map_err(|source| DriverError::DecodeScreenshot { source })?
            .to_rgba8();

        Ok(ChromiumCapture {
            image,
            element_box_css_px: BoxGeometryPx {
                x: raw_rect.x,
                y: raw_rect.y,
                width: raw_rect.width,
                height: raw_rect.height,
            },
        })
    }
}

impl Drop for ChromiumDriver {
    fn drop(&mut self) {
        let _ = self.runtime.block_on(self.browser.close());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sandbox_is_given_up_only_as_root_or_where_user_namespaces_are_restricted() {
        assert!(!sandbox_blocked(None, false));
        assert!(!sandbox_blocked(Some("0\n"), false));
        assert!(sandbox_blocked(Some("1\n"), false));
        assert!(sandbox_blocked(Some("0"), true));
        assert!(sandbox_blocked(None, true));
    }

    #[test]
    fn the_effective_user_id_is_the_second_one_listed() {
        let status = "Name:\tflorui\nUid:\t1000\t0\t1000\t1000\nGid:\t1000\t1000\t1000\t1000\n";
        assert_eq!(effective_uid(status), Some(0));
        assert_eq!(effective_uid("Name:\tflorui\n"), None);
        assert_eq!(effective_uid("Uid:\tx\ty\n"), None);
    }
}
