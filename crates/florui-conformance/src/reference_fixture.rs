//! Loads a reference fixture: an HTML/CSS pair (the Chromium side) plus a
//! [`FloruiSpec`] (the same element, rendered through Florui's own real
//! style/layout/paint pipeline — see [`crate::engine`]) and a manifest
//! tying the two together.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ViewportSpec {
    pub width_css_px: u32,
    pub height_css_px: u32,
    pub device_pixel_ratio: f64,
}

impl ViewportSpec {
    pub fn width_physical_px(&self) -> u32 {
        (self.width_css_px as f64 * self.device_pixel_ratio).round() as u32
    }

    pub fn height_physical_px(&self) -> u32 {
        (self.height_css_px as f64 * self.device_pixel_ratio).round() as u32
    }
}

/// The same element under test, rendered through Florui's own real
/// style/layout/paint pipeline (see [`crate::engine::render_fixture`])
/// instead of the Chromium HTML/CSS pair — no expected pixel/geometry
/// values are hand-typed anywhere in this crate any more: both sides are
/// real renders, compared against each other by [`crate::pixels`]/
/// [`crate::geometry`].
///
/// `tag`/`class` describe the single element under test (wrapped in a synthetic
/// `<div>` matching the HTML side's `<body>`, so a bare `<span>`'s real
/// inline-vs-block distinction isn't lost to Stylo's own root-element
/// blockification — see `florui_style::stylo`'s `to_display` doc for
/// that rule).
///
/// `css` is real author CSS through the same `florui_style::parse_stylesheet`
/// path an application uses; empty for a bare-element fixture, so the
/// framework's own default stylesheet is what's actually under test.
/// Only meaningful on the top-level spec a [`FixtureManifest`] points
/// at — [`crate::engine::render_fixture`] takes it as its own
/// `css` parameter rather than reading it back off `spec`, so a nested
/// [`FloruiChild::Element`]'s own `css` field is always empty in
/// practice; one shared stylesheet (with real selectors, including
/// descendant combinators like `.card p`) styles the whole tree, the
/// same way one `<style>` element styles a whole HTML fixture.
///
/// `text` is a plain-content shorthand — equivalent to a single
/// `children: [Text(text)]` entry, kept so every fixture written before
/// `children` existed still loads unchanged. A fixture needs `children`
/// instead once it has to nest a real element (a card's own heading and
/// paragraph) or interleave text with one (mixed inline content, `Hello
/// <span>world</span>!`) — `text` alone can only ever describe one flat
/// run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FloruiSpec {
    pub tag: String,
    #[serde(default)]
    pub class: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub css: String,
    #[serde(default)]
    pub children: Vec<FloruiChild>,
}

/// One entry in [`FloruiSpec::children`]: either a literal text run, or a
/// nested element (itself a full [`FloruiSpec`], so nesting is
/// unbounded — a card's `<p>` can contain its own mixed inline content,
/// the same way a real element tree can).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FloruiChild {
    Text(String),
    Element(FloruiSpec),
}

fn default_geometry_tolerance_px() -> f64 {
    1.5
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Classification {
    Exact,
    Tolerant {
        threshold_percent: f64,
        /// Absolute pixel tolerance for the tested element's own box
        /// (`compare_geometry`'s own `tolerance_px`). Defaults to 1.5px,
        /// the value every fixture up to and including this field's own
        /// addition was implicitly measured against — a fixture wrapping
        /// across many more lines than those (long, multi-line content)
        /// can accumulate a few more pixels of real per-line
        /// float-rounding in text shaping/line-height math before this
        /// crate's own module doc, not a fixture-specific guess.
        #[serde(default = "default_geometry_tolerance_px")]
        geometry_tolerance_px: f64,
        /// How far a pixel's channels may differ from the reference, in
        /// 0-255 units, before the pixel counts as differing. Defaults to 0,
        /// where any difference counts. A blur is built differently by each
        /// engine (kernel shape, rounding), so a fixture of one sets a few
        /// levels here and lets `threshold_percent` stay small: that is a far
        /// better guard than a high percentage of pixels that differ by 1.
        #[serde(default)]
        channel_tolerance: u8,
        reason: String,
    },
}

impl Classification {
    pub fn geometry_tolerance_px(&self) -> f64 {
        match self {
            Classification::Exact => 0.0,
            Classification::Tolerant {
                geometry_tolerance_px,
                ..
            } => *geometry_tolerance_px,
        }
    }

    /// See `Tolerant`'s `channel_tolerance`; an exact fixture allows none.
    pub fn channel_tolerance(&self) -> u8 {
        match self {
            Classification::Exact => 0,
            Classification::Tolerant {
                channel_tolerance, ..
            } => *channel_tolerance,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FixtureManifest {
    pub id: String,
    pub description: String,
    pub html: String,
    pub css: String,
    pub element_id: String,
    pub viewport: ViewportSpec,
    /// Written into the Chromium HTML/CSS pair's own `body`
    /// `background-color` *and* passed as Florui's own `paint_to_buffer`
    /// canvas color — one declared value for both renders, rather than
    /// two hand-typed strings that could silently drift apart.
    pub canvas_color: String,
    pub florui: FloruiSpec,
    pub classification: Classification,
}

#[derive(Debug, Clone)]
pub struct ReferenceFixture {
    pub dir: PathBuf,
    pub manifest: FixtureManifest,
}

impl ReferenceFixture {
    pub fn html_path(&self) -> PathBuf {
        self.dir.join(&self.manifest.html)
    }

    pub fn css_path(&self) -> PathBuf {
        self.dir.join(&self.manifest.css)
    }
}

#[derive(Debug)]
pub enum FixtureError {
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    MissingAsset {
        path: PathBuf,
    },
}

impl fmt::Display for FixtureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FixtureError::Read { path, source } => {
                write!(f, "could not read {}: {source}", path.display())
            }
            FixtureError::Parse { path, source } => {
                write!(f, "could not parse manifest {}: {source}", path.display())
            }
            FixtureError::MissingAsset { path } => {
                write!(f, "fixture asset not found: {}", path.display())
            }
        }
    }
}

impl std::error::Error for FixtureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FixtureError::Read { source, .. } => Some(source),
            FixtureError::Parse { source, .. } => Some(source),
            FixtureError::MissingAsset { .. } => None,
        }
    }
}

/// Loads `dir/manifest.json` and validates that its declared `html`/`css`
/// assets actually exist alongside it.
pub fn load_reference_fixture(dir: &Path) -> Result<ReferenceFixture, FixtureError> {
    let manifest_path = dir.join("manifest.json");
    let text = std::fs::read_to_string(&manifest_path).map_err(|source| FixtureError::Read {
        path: manifest_path.clone(),
        source,
    })?;
    let manifest: FixtureManifest =
        serde_json::from_str(&text).map_err(|source| FixtureError::Parse {
            path: manifest_path.clone(),
            source,
        })?;

    let fixture = ReferenceFixture {
        dir: dir.to_owned(),
        manifest,
    };

    for path in [fixture.html_path(), fixture.css_path()] {
        if !path.exists() {
            return Err(FixtureError::MissingAsset { path });
        }
    }

    Ok(fixture)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_fixture(dir: &Path, manifest_json: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("manifest.json"), manifest_json).unwrap();
        std::fs::write(dir.join("index.html"), "<html></html>").unwrap();
        std::fs::write(dir.join("style.css"), "body {}").unwrap();
    }

    fn valid_manifest_json() -> &'static str {
        r##"{
            "id": "test-fixture",
            "description": "a test fixture",
            "html": "index.html",
            "css": "style.css",
            "element_id": "el",
            "viewport": { "width_css_px": 100, "height_css_px": 80, "device_pixel_ratio": 1.0 },
            "canvas_color": "#111111",
            "florui": { "tag": "div", "text": "", "css": "" },
            "classification": { "kind": "exact" }
        }"##
    }

    fn scratch_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "florui-conformance-fixture-test-{name}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn loads_a_valid_fixture() {
        let dir = scratch_dir("valid");
        write_fixture(&dir, valid_manifest_json());

        let fixture = load_reference_fixture(&dir).unwrap();
        assert_eq!(fixture.manifest.id, "test-fixture");
        assert_eq!(fixture.manifest.viewport.width_css_px, 100);
        assert_eq!(fixture.manifest.florui.tag, "div");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_asset_is_reported() {
        let dir = scratch_dir("missing-asset");
        write_fixture(&dir, valid_manifest_json());
        std::fs::remove_file(dir.join("style.css")).unwrap();

        let error = load_reference_fixture(&dir).unwrap_err();
        assert!(matches!(error, FixtureError::MissingAsset { .. }));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn malformed_json_is_reported() {
        let dir = scratch_dir("malformed");
        write_fixture(&dir, "{ not valid json");

        let error = load_reference_fixture(&dir).unwrap_err();
        assert!(matches!(error, FixtureError::Parse { .. }));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn viewport_scales_by_device_pixel_ratio() {
        let viewport = ViewportSpec {
            width_css_px: 320,
            height_css_px: 240,
            device_pixel_ratio: 2.0,
        };
        assert_eq!(viewport.width_physical_px(), 640);
        assert_eq!(viewport.height_physical_px(), 480);
    }

    #[test]
    fn channel_tolerance_defaults_to_none_and_is_read_when_declared() {
        let parse = |json: &str| serde_json::from_str::<Classification>(json).unwrap();
        let without = parse(r#"{"kind":"tolerant","threshold_percent":1.0,"reason":"r"}"#);
        let with = parse(
            r#"{"kind":"tolerant","threshold_percent":1.0,"channel_tolerance":8,"reason":"r"}"#,
        );
        assert_eq!(without.channel_tolerance(), 0);
        assert_eq!(with.channel_tolerance(), 8);
        assert_eq!(Classification::Exact.channel_tolerance(), 0);
    }
}
