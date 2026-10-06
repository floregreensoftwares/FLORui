//! The comparison report written after every `florui compare` run: metrics
//! plus paths to the minimum failure artifacts a mismatch should leave
//! behind for diagnosis (reference, result, diff, overlay).

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::geometry::GeometryReport;
use crate::pixels::PixelSummary;
use crate::reference_fixture::Classification;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactPaths {
    pub reference: PathBuf,
    pub result: PathBuf,
    pub diff: PathBuf,
    pub overlay: PathBuf,
    /// The red/cyan anaglyph (`pixels::anaglyph_overlay`) — a real match
    /// reads as gray at a glance, a mismatch as a colored fringe.
    pub anaglyph: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Pass,
    Fail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub fixture_id: String,
    pub captured_at_unix_seconds: u64,
    pub chromium_executable: PathBuf,
    pub pixels: PixelSummary,
    pub geometry: GeometryReport,
    pub artifacts: ArtifactPaths,
    pub outcome: Outcome,
    /// The elements drawn where the pixels differ, innermost first, with where
    /// each was written when the tree recorded it. Empty when nothing differs.
    #[serde(default)]
    pub culprits: Vec<String>,
}

/// A fixture passes when its pixel comparison meets its declared
/// classification (zero differing pixels if `Exact`, at or under the
/// declared threshold if `Tolerant`) and its geometry is within tolerance.
pub fn classify(
    classification: &Classification,
    pixels: &PixelSummary,
    geometry: &GeometryReport,
) -> Outcome {
    let pixels_pass = match classification {
        Classification::Exact => pixels.is_exact_match(),
        Classification::Tolerant {
            threshold_percent, ..
        } => pixels.percent_different <= *threshold_percent,
    };

    if pixels_pass && geometry.within_tolerance {
        Outcome::Pass
    } else {
        Outcome::Fail
    }
}

#[derive(Debug)]
pub enum ReportError {
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    Encode {
        source: serde_json::Error,
    },
    SaveImage {
        path: PathBuf,
        source: image::ImageError,
    },
}

impl fmt::Display for ReportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReportError::Write { path, source } => {
                write!(f, "could not write {}: {source}", path.display())
            }
            ReportError::Encode { source } => write!(f, "could not encode report: {source}"),
            ReportError::SaveImage { path, source } => {
                write!(f, "could not save image {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for ReportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReportError::Write { source, .. } => Some(source),
            ReportError::Encode { source } => Some(source),
            ReportError::SaveImage { source, .. } => Some(source),
        }
    }
}

/// Serializes `report` to `<out_dir>/report.json`. Image artifacts are
/// saved separately by the caller (they're already `image::RgbaImage`
/// values produced by the pixel comparator and the driver) at the paths
/// recorded in `report.artifacts`.
pub fn write_report(report: &Report, out_dir: &Path) -> Result<(), ReportError> {
    std::fs::create_dir_all(out_dir).map_err(|source| ReportError::Write {
        path: out_dir.to_owned(),
        source,
    })?;
    let path = out_dir.join("report.json");
    let json =
        serde_json::to_string_pretty(report).map_err(|source| ReportError::Encode { source })?;
    std::fs::write(&path, json).map_err(|source| ReportError::Write {
        path: path.clone(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{BoxGeometryPx, compare_geometry};
    use crate::pixels::{PixelDiffOptions, compare_pixels};
    use image::{Rgba as ImageRgba, RgbaImage};

    fn solid(color: [u8; 4]) -> RgbaImage {
        RgbaImage::from_fn(4, 4, |_, _| ImageRgba(color))
    }

    #[test]
    fn exact_classification_requires_zero_diff() {
        let pixels_exact = compare_pixels(
            &solid([1, 2, 3, 255]),
            &solid([1, 2, 3, 255]),
            &PixelDiffOptions::default(),
        )
        .unwrap()
        .summary;
        let pixels_off = compare_pixels(
            &solid([1, 2, 3, 255]),
            &solid([9, 2, 3, 255]),
            &PixelDiffOptions::default(),
        )
        .unwrap()
        .summary;
        let geometry = compare_geometry(
            BoxGeometryPx {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            BoxGeometryPx {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            0.0,
        );

        assert_eq!(
            classify(&Classification::Exact, &pixels_exact, &geometry),
            Outcome::Pass
        );
        assert_eq!(
            classify(&Classification::Exact, &pixels_off, &geometry),
            Outcome::Fail
        );
    }

    #[test]
    fn tolerant_classification_allows_declared_threshold() {
        let pixels = compare_pixels(
            &solid([1, 2, 3, 255]),
            &solid([1, 2, 3, 255]),
            &PixelDiffOptions::default(),
        )
        .unwrap()
        .summary;
        let geometry = compare_geometry(
            BoxGeometryPx {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            BoxGeometryPx {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            0.0,
        );
        let tolerant = Classification::Tolerant {
            threshold_percent: 1.0,
            geometry_tolerance_px: 1.5,
            channel_tolerance: 0,
            reason: "known antialiasing difference".to_owned(),
        };
        assert_eq!(classify(&tolerant, &pixels, &geometry), Outcome::Pass);
    }

    #[test]
    fn report_round_trips_through_json() {
        let report = Report {
            fixture_id: "inset-rect-exact".to_owned(),
            captured_at_unix_seconds: 1_700_000_000,
            chromium_executable: PathBuf::from("C:/chrome.exe"),
            pixels: PixelSummary {
                differing_pixels: 0,
                total_pixels: 100,
                percent_different: 0.0,
                mean_error: 0.0,
                max_error: 0,
            },
            geometry: compare_geometry(
                BoxGeometryPx {
                    x: 56.0,
                    y: 8.0,
                    width: 240.0,
                    height: 192.0,
                },
                BoxGeometryPx {
                    x: 56.0,
                    y: 8.0,
                    width: 240.0,
                    height: 192.0,
                },
                0.0,
            ),
            artifacts: ArtifactPaths {
                reference: PathBuf::from("reference.png"),
                result: PathBuf::from("result.png"),
                diff: PathBuf::from("diff.png"),
                overlay: PathBuf::from("overlay.png"),
                anaglyph: PathBuf::from("anaglyph.png"),
            },
            outcome: Outcome::Pass,
            culprits: vec!["div > div.card > h2".to_owned()],
        };

        let json = serde_json::to_string(&report).unwrap();
        let round_tripped: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped.fixture_id, report.fixture_id);
        assert_eq!(round_tripped.outcome, report.outcome);
        assert_eq!(round_tripped.geometry, report.geometry);
        assert_eq!(round_tripped.culprits, report.culprits);
    }

    #[test]
    fn a_report_written_before_culprits_existed_still_loads() {
        let mut value = serde_json::to_value(sample_report()).unwrap();
        value.as_object_mut().unwrap().remove("culprits");
        let loaded: Report = serde_json::from_value(value).unwrap();
        assert!(loaded.culprits.is_empty());
    }

    fn sample_report() -> Report {
        Report {
            fixture_id: "x".to_owned(),
            captured_at_unix_seconds: 1,
            chromium_executable: PathBuf::from("chrome"),
            pixels: PixelSummary {
                differing_pixels: 0,
                total_pixels: 1,
                percent_different: 0.0,
                mean_error: 0.0,
                max_error: 0,
            },
            geometry: compare_geometry(
                BoxGeometryPx {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
                BoxGeometryPx {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
                0.0,
            ),
            artifacts: ArtifactPaths {
                reference: PathBuf::from("a"),
                result: PathBuf::from("b"),
                diff: PathBuf::from("c"),
                overlay: PathBuf::from("d"),
                anaglyph: PathBuf::from("e"),
            },
            outcome: Outcome::Pass,
            culprits: vec!["div > a".to_owned()],
        }
    }
}
