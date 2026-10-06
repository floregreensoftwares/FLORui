//! The arithmetic of the presentation measurement, apart from the window: what
//! a window process reports about each frame's presenting, the sizes asked for,
//! and how the frames of one size are summarized.

use serde::{Deserialize, Serialize};

use crate::edit_chain::IntervalSummary;
use crate::report::Environment;
use crate::stats;

/// One frame's time in each phase of showing it, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PresentFrame {
    /// Copying the buffer into a GPU texture.
    pub upload: f64,
    /// Waiting for the window's next surface texture: this is where the wait for
    /// the display's refresh shows, not work.
    pub acquire: f64,
    /// Recording and submitting the copy to the surface.
    pub submit: f64,
    pub flip: f64,
    /// The whole present, which contains the four above.
    pub present: f64,
    pub raster: f64,
    pub update: f64,
    pub wall: f64,
}

fn micros(ms: f64) -> u64 {
    (ms * 1000.0).round() as u64
}

pub fn format_present_line(frame: &PresentFrame) -> String {
    format!(
        "present up={} acq={} sub={} flip={} pres={} raster={} update={} wall={}",
        micros(frame.upload),
        micros(frame.acquire),
        micros(frame.submit),
        micros(frame.flip),
        micros(frame.present),
        micros(frame.raster),
        micros(frame.update),
        micros(frame.wall),
    )
}

pub fn parse_present_line(line: &str) -> Option<PresentFrame> {
    let rest = line.strip_prefix("present ")?;
    let mut values = [None; 8];
    for field in rest.split_whitespace() {
        let (name, value) = field.split_once('=')?;
        let slot = match name {
            "up" => 0,
            "acq" => 1,
            "sub" => 2,
            "flip" => 3,
            "pres" => 4,
            "raster" => 5,
            "update" => 6,
            "wall" => 7,
            _ => continue,
        };
        values[slot] = Some(value.parse::<f64>().ok()? / 1000.0);
    }
    let [up, acq, sub, flip, pres, raster, update, wall] = values;
    Some(PresentFrame {
        upload: up?,
        acquire: acq?,
        submit: sub?,
        flip: flip?,
        present: pres?,
        raster: raster?,
        update: update?,
        wall: wall?,
    })
}

/// `1920x1080` as a width and a height.
pub fn parse_size(text: &str) -> Result<(u32, u32), String> {
    let invalid = || format!("\"{text}\" is not a size like 1920x1080");
    let (width, height) = text.split_once('x').ok_or_else(invalid)?;
    let (width, height): (u32, u32) = (
        width.parse().map_err(|_| invalid())?,
        height.parse().map_err(|_| invalid())?,
    );
    if width == 0 || height == 0 {
        return Err(invalid());
    }
    Ok((width, height))
}

pub fn parse_sizes(text: &str) -> Result<Vec<(u32, u32)>, String> {
    text.split(',')
        .map(|size| parse_size(size.trim()))
        .collect()
}

type Pick = fn(&PresentFrame) -> f64;

const COLUMNS: [(&str, Pick); 8] = [
    ("upload (copy into a GPU texture)", |f| f.upload),
    ("acquire (waits for the display)", |f| f.acquire),
    ("submit", |f| f.submit),
    ("flip", |f| f.flip),
    ("present, all of the four", |f| f.present),
    ("raster", |f| f.raster),
    ("update", |f| f.update),
    ("frame, first work to present returned", |f| f.wall),
];

pub fn summarize_frames(frames: &[PresentFrame]) -> Vec<IntervalSummary> {
    COLUMNS
        .iter()
        .filter_map(|(name, pick)| {
            let values: Vec<f64> = frames.iter().map(pick).collect();
            stats::summarize(&values).map(|summary| IntervalSummary {
                name: (*name).to_string(),
                summary,
            })
        })
        .collect()
}

/// Whether the frames went through the GPU presenter. The software path has no
/// upload, acquire, submit or flip, only the present as a whole.
pub fn used_the_gpu_presenter(frames: &[PresentFrame]) -> bool {
    frames.iter().any(|f| f.upload > 0.0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SizeResult {
    pub requested: (u32, u32),
    /// The window's client area as it was, in physical pixels.
    pub actual: (u32, u32),
    pub frames: usize,
    pub gpu_presenter: bool,
    pub columns: Vec<IntervalSummary>,
}

impl SizeResult {
    pub fn megapixels(&self) -> f64 {
        f64::from(self.actual.0) * f64::from(self.actual.1) / 1e6
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresentReport {
    pub environment: Environment,
    pub results: Vec<SizeResult>,
}

fn median_of(result: &SizeResult, name_start: &str) -> Option<f64> {
    result
        .columns
        .iter()
        .find(|c| c.name.starts_with(name_start))
        .map(|c| c.summary.median)
}

pub fn render(report: &PresentReport) -> String {
    let mut out = String::from("# Presenting a frame\n\n");
    out.push_str(&format!(
        "- {} on {}, {}\n",
        report.environment.profile, report.environment.os, report.environment.cpu
    ));
    out.push_str(
        "- A window of the size shown, repainted every refresh by an animation; medians over the \
         frames after a warm-up, in milliseconds. `acquire` waits for the display, so it is not \
         work; `upload` and `submit` are the CPU's.\n\n",
    );
    out.push_str(
        "| Window | MP | Frames | upload | acquire | submit | flip | present | raster | update |\n\
         | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n",
    );
    for result in &report.results {
        let cell = |name: &str| {
            median_of(result, name).map_or_else(|| "n/a".to_string(), |v| format!("{v:.2}"))
        };
        out.push_str(&format!(
            "| {}x{} | {:.2} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            result.actual.0,
            result.actual.1,
            result.megapixels(),
            result.frames,
            cell("upload"),
            cell("acquire"),
            cell("submit"),
            cell("flip"),
            cell("present"),
            cell("raster"),
            cell("update"),
        ));
    }
    for result in &report.results {
        if result.actual != result.requested {
            out.push_str(&format!(
                "\nAsked for {}x{}, the window's client area was {}x{}.\n",
                result.requested.0, result.requested.1, result.actual.0, result.actual.1
            ));
        }
        if !result.gpu_presenter {
            out.push_str(&format!(
                "\n{}x{} did not use the GPU presenter: only the whole present is measured there.\n",
                result.actual.0, result.actual.1
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(upload: f64, acquire: f64) -> PresentFrame {
        PresentFrame {
            upload,
            acquire,
            submit: 0.2,
            flip: 0.1,
            present: upload + acquire + 0.3,
            raster: 4.0,
            update: 1.0,
            wall: 6.0,
        }
    }

    #[test]
    fn a_present_line_survives_the_trip_through_text() {
        let original = frame(0.512, 5.25);

        let parsed = parse_present_line(&format_present_line(&original)).unwrap();

        assert!((parsed.upload - 0.512).abs() < 1e-9);
        assert!((parsed.acquire - 5.25).abs() < 1e-9);
        assert!((parsed.wall - 6.0).abs() < 1e-9);
        assert_eq!(parse_present_line("frame reload=- start=1 end=2"), None);
        assert_eq!(parse_present_line("present up=1 acq=2"), None);
    }

    #[test]
    fn sizes_are_read_and_a_bad_one_is_named() {
        assert_eq!(
            parse_sizes("800x600, 1920x1080").unwrap(),
            [(800, 600), (1920, 1080)]
        );
        for bad in ["800", "0x600", "axb", "800x", "-1x5"] {
            let error = parse_sizes(bad).unwrap_err();
            assert!(error.contains(bad), "{error}");
        }
    }

    #[test]
    fn each_phase_is_summarized_on_its_own() {
        let frames = [frame(1.0, 5.0), frame(3.0, 5.0), frame(2.0, 7.0)];

        let columns = summarize_frames(&frames);

        assert_eq!(columns.len(), 8);
        assert_eq!(columns[0].summary.median, 2.0);
        assert_eq!(columns[1].summary.median, 5.0);
        assert!(summarize_frames(&[]).is_empty());
    }

    #[test]
    fn the_software_presenter_is_told_apart_by_having_no_upload() {
        assert!(used_the_gpu_presenter(&[frame(0.3, 5.0)]));
        assert!(!used_the_gpu_presenter(&[frame(0.0, 0.0)]));
    }

    #[test]
    fn the_table_names_a_window_that_was_not_the_size_asked_for() {
        let result = SizeResult {
            requested: (3840, 2160),
            actual: (1920, 1080),
            frames: 3,
            gpu_presenter: false,
            columns: summarize_frames(&[frame(0.0, 0.0)]),
        };
        let report = PresentReport {
            environment: crate::report::capture_environment(&crate::report::BuildInfo::this_build()),
            results: vec![result],
        };

        let text = render(&report);

        assert!(text.contains("| 1920x1080 | 2.07 |"), "{text}");
        assert!(text.contains("Asked for 3840x2160"), "{text}");
        assert!(text.contains("did not use the GPU presenter"), "{text}");
    }
}
