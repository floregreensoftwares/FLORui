//! The arithmetic of the edit-to-present measurement, apart from the window and
//! the screen: what a window process reports about its frames, how one edit's
//! frame is told from the others, and how the intervals between the file write
//! and the composed frame are derived and summarized.
//!
//! Every time here is a QPC tick, the clock the window process, the benchmark
//! and the compositor all read, so intervals can be subtracted across processes.

use serde::{Deserialize, Serialize};

use crate::report::Environment;
use crate::stats::{self, Summary};

/// One frame the window process reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildFrame {
    /// When the watcher reported the file change that made the frame.
    pub reload: Option<i64>,
    /// The frame's first measured work.
    pub start: i64,
    /// The present call returning.
    pub end: i64,
}

/// Whether `line` from the window's stdout reports a successful reload. The
/// text is the platform's own message, so a change there has to change this.
pub fn is_reload_report(line: &str) -> bool {
    line.contains("stylesheet reloaded")
}

/// The reason in a line from the window's stderr that reports a failed reload.
pub fn reload_failure(line: &str) -> Option<String> {
    let (_, reason) = line.split_once("reload failed, keeping last good version:")?;
    Some(reason.trim().to_string())
}

pub fn format_frame_line(frame: &ChildFrame) -> String {
    let reload = frame
        .reload
        .map_or_else(|| "-".to_string(), |ticks| ticks.to_string());
    format!(
        "frame reload={reload} start={} end={}",
        frame.start, frame.end
    )
}

pub fn parse_frame_line(line: &str) -> Option<ChildFrame> {
    let rest = line.strip_prefix("frame ")?;
    let mut reload = None;
    let (mut start, mut end) = (None, None);
    for field in rest.split_whitespace() {
        let (name, value) = field.split_once('=')?;
        match name {
            "reload" => reload = (value != "-").then(|| value.parse().ok()).flatten(),
            "start" => start = value.parse().ok(),
            "end" => end = value.parse().ok(),
            _ => {}
        }
    }
    Some(ChildFrame {
        reload,
        start: start?,
        end: end?,
    })
}

/// One edit, in milliseconds. The four intervals add up to `total_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// The file written to the watcher reporting it.
    pub write_to_event_ms: f64,
    /// The watcher's report to the frame's first measured work: the event loop
    /// waking up, reading and parsing the file.
    pub event_to_frame_ms: f64,
    /// The frame: restyle, layout, raster and the present call.
    pub frame_ms: f64,
    /// The present call returning to the compositor presenting the frame.
    /// Negative when the compositor presented before the call returned.
    pub present_to_composed_ms: f64,
    pub total_ms: f64,
}

fn ms(ticks: i64, frequency: i64) -> f64 {
    ticks as f64 * 1000.0 / frequency as f64
}

/// The sample for one edit. `t0` is just before the file was written and
/// `composed` is when the compositor presented the frame that first showed the
/// new color. The frame that did it is the last one caused by this edit
/// (a watcher report at or after `t0`) that began before `composed`; frames
/// that started after it are the redundant ones a second file event makes.
pub fn chain(t0: i64, frames: &[ChildFrame], composed: i64, frequency: i64) -> Option<Sample> {
    let frame = frames
        .iter()
        .filter(|f| f.reload.is_some_and(|r| r >= t0) && f.start <= composed)
        .max_by_key(|f| f.start)?;
    let reload = frame.reload?;
    Some(Sample {
        write_to_event_ms: ms(reload - t0, frequency),
        event_to_frame_ms: ms(frame.start - reload, frequency),
        frame_ms: ms(frame.end - frame.start, frequency),
        present_to_composed_ms: ms(composed - frame.end, frequency),
        total_ms: ms(composed - t0, frequency),
    })
}

pub type Rgb = (u8, u8, u8);

/// Far apart from each other, so a frame that showed an in-between state is not
/// mistaken for one that showed the target.
pub const PALETTE: [Rgb; 6] = [
    (200, 30, 30),
    (30, 200, 30),
    (30, 30, 200),
    (200, 200, 30),
    (200, 30, 200),
    (30, 200, 200),
];

pub fn color_for_round(round: usize) -> Rgb {
    PALETTE[round % PALETTE.len()]
}

pub fn hex((r, g, b): Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Close enough that the GPU's rounding of a solid color is not a miss.
pub fn same_color(a: Rgb, b: Rgb) -> bool {
    a.0.abs_diff(b.0) <= 2 && a.1.abs_diff(b.1) <= 2 && a.2.abs_diff(b.2) <= 2
}

/// Bytes in one filler rule of [`stylesheet`], about.
const FILLER_RULE_BYTES: usize = 58;

/// The window's stylesheet with `color` as the page background; the sampled
/// pixel sits where only the page background is drawn. `kilobytes` of rules
/// nothing uses come first and the color rule is last, so a file cut short by a
/// read in the middle of a write is missing it and the window shows it.
pub fn stylesheet(color: Rgb, kilobytes: usize) -> String {
    let mut text = String::from(".row { padding: 4px 8px; }\n");
    for i in 0..kilobytes * 1024 / FILLER_RULE_BYTES {
        text.push_str(&format!(
            ".filler{i} {{ margin: 1px; padding: 2px; color: #123456; }}\n"
        ));
    }
    text.push_str(&format!(
        ".page {{ background-color: {}; min-height: 100vh; font-family: sans-serif; \
         color: #ffffff; }}\n",
        hex(color)
    ));
    text
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntervalSummary {
    pub name: String,
    pub summary: Summary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditLatencyReport {
    pub environment: Environment,
    pub rows: usize,
    pub write_mode: String,
    pub refresh_hz: Option<f64>,
    /// The first edit after the window opened, kept out of the summaries.
    pub cold: Option<Sample>,
    pub intervals: Vec<IntervalSummary>,
    /// Edits after which the screen showed a color that was neither the old nor
    /// the new one.
    pub edits_with_an_intermediate_frame: usize,
    /// Edits after which, once the new color showed, the screen went to a
    /// different one again.
    #[serde(default)]
    pub edits_that_regressed_after_the_new_color: usize,
    /// Kilobytes of rules in the stylesheet that was rewritten.
    #[serde(default)]
    pub css_kilobytes: usize,
    /// Stylesheet reloads the window reported, successful ones.
    #[serde(default)]
    pub reloads: usize,
    /// What the window said when a reload failed, one entry per failure.
    #[serde(default)]
    pub reload_failures: Vec<String>,
    /// Edits whose new color never appeared in time.
    pub timeouts: usize,
    pub samples: Vec<Sample>,
}

type Pick = fn(&Sample) -> f64;

const COLUMNS: [(&str, Pick); 5] = [
    ("file write to watcher report", |s| s.write_to_event_ms),
    ("watcher report to frame start", |s| s.event_to_frame_ms),
    ("frame (restyle, layout, raster, present call)", |s| {
        s.frame_ms
    }),
    ("present call returned to composed", |s| {
        s.present_to_composed_ms
    }),
    ("total, file write to composed", |s| s.total_ms),
];

pub fn summarize_samples(samples: &[Sample]) -> Vec<IntervalSummary> {
    COLUMNS
        .iter()
        .filter_map(|(name, pick)| {
            let values: Vec<f64> = samples.iter().map(pick).collect();
            stats::summarize(&values).map(|summary| IntervalSummary {
                name: (*name).to_string(),
                summary,
            })
        })
        .collect()
}

/// One interval of two sample sets, `a` the baseline and `b` the candidate.
#[derive(Debug, Clone)]
pub struct IntervalComparison {
    pub name: String,
    pub a_median_ms: f64,
    pub b_median_ms: f64,
    pub comparison: Option<stats::Comparison>,
}

pub fn compare_samples(a: &[Sample], b: &[Sample], threshold: f64) -> Vec<IntervalComparison> {
    COLUMNS
        .iter()
        .filter_map(|(name, pick)| {
            let (a, b): (Vec<f64>, Vec<f64>) =
                (a.iter().map(pick).collect(), b.iter().map(pick).collect());
            Some(IntervalComparison {
                name: (*name).to_string(),
                a_median_ms: stats::summarize(&a)?.median,
                b_median_ms: stats::summarize(&b)?.median,
                comparison: stats::compare(&a, &b, threshold),
            })
        })
        .collect()
}

pub fn render_comparison(
    a_label: &str,
    b_label: &str,
    comparisons: &[IntervalComparison],
    threshold: f64,
) -> String {
    let mut out = format!("# Edit to present: {b_label} against {a_label}\n\n");
    out.push_str(&format!(
        "- A change counts when it exceeds {:.0}% and its 95% bootstrap interval excludes zero.\n\n",
        threshold * 100.0
    ));
    out.push_str(
        "| Interval | A median (ms) | B median (ms) | B - A (ms) | Change | 95% interval | Verdict |\n\
         | --- | ---: | ---: | ---: | ---: | ---: | --- |\n",
    );
    for c in comparisons {
        let (change, interval, verdict) = match &c.comparison {
            Some(cmp) => (
                format!("{:+.1}%", cmp.relative_change * 100.0),
                format!(
                    "{:+.1}% to {:+.1}%",
                    cmp.interval.0 * 100.0,
                    cmp.interval.1 * 100.0
                ),
                match cmp.verdict {
                    stats::Verdict::Faster => "faster",
                    stats::Verdict::Slower => "slower",
                    stats::Verdict::NoDifference => "no difference",
                },
            ),
            None => ("n/a".into(), "n/a".into(), "n/a"),
        };
        out.push_str(&format!(
            "| {} | {:.2} | {:.2} | {:+.2} | {change} | {interval} | {verdict} |\n",
            c.name,
            c.a_median_ms,
            c.b_median_ms,
            c.b_median_ms - c.a_median_ms
        ));
    }
    out
}

pub fn render(report: &EditLatencyReport) -> String {
    let mut out = String::new();
    out.push_str("# Edit to present\n\n");
    out.push_str(&format!(
        "- {} rows, {} writes, {} edits measured ({} timed out, {} showed an intermediate frame)\n",
        report.rows,
        report.write_mode,
        report.samples.len(),
        report.timeouts,
        report.edits_with_an_intermediate_frame
    ));
    let edits = report.samples.len() + usize::from(report.cold.is_some());
    out.push_str(&format!(
        "- {} KB of stylesheet; {} reloads for {} edits, {} reload failures; {} edits went to another color after showing the new one\n",
        report.css_kilobytes,
        report.reloads,
        edits,
        report.reload_failures.len(),
        report.edits_that_regressed_after_the_new_color
    ));
    let mut failures = std::collections::BTreeMap::<&str, usize>::new();
    for failure in &report.reload_failures {
        *failures.entry(failure.as_str()).or_default() += 1;
    }
    for (message, count) in failures {
        out.push_str(&format!("  - {count} x {message}\n"));
    }
    out.push_str(&format!(
        "- {} on {}, {}, {}\n",
        report.environment.profile,
        report.environment.os,
        report.environment.cpu,
        report.refresh_hz.map_or_else(
            || "refresh rate unknown".to_string(),
            |hz| format!("{hz:.0} Hz")
        )
    ));
    out.push_str(
        "- \"Composed\" is when the compositor presented the frame (DXGI desktop duplication), \
         on the same QPC clock as the rest. Display scan-out and panel response are not seen.\n\n",
    );
    out.push_str("| Interval | Median (ms) | Spread (MAD) | p95 | Min |\n| --- | ---: | ---: | ---: | ---: |\n");
    for interval in &report.intervals {
        let s = &interval.summary;
        out.push_str(&format!(
            "| {} | {:.2} | {:.2} | {} | {:.2} |\n",
            interval.name,
            s.median,
            s.mad,
            s.p95
                .map_or_else(|| "n/a".to_string(), |p| format!("{p:.2}")),
            s.min
        ));
    }
    if let Some(cold) = &report.cold {
        out.push_str(&format!(
            "\nThe first edit after the window opened (kept out of the table): {:.2} ms in total, \
             {:.2} ms of it the frame.\n",
            cold.total_ms, cold.frame_ms
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const HZ: i64 = 10_000_000;

    fn frame(reload: Option<i64>, start: i64, end: i64) -> ChildFrame {
        ChildFrame { reload, start, end }
    }

    #[test]
    fn a_reload_and_a_failed_reload_are_told_apart_from_other_output() {
        let ok = "florui-platform: stylesheet reloaded from C:\\app\\app.css";
        let failed = "florui-platform: stylesheet reload failed, keeping last good version: \
                      could not read the stylesheet: file not found";

        assert!(is_reload_report(ok));
        assert!(!is_reload_report(failed));
        assert_eq!(reload_failure(ok), None);
        assert_eq!(
            reload_failure(failed).as_deref(),
            Some("could not read the stylesheet: file not found")
        );
        assert_eq!(reload_failure("frame reload=- start=1 end=2"), None);
    }

    #[test]
    fn a_frame_line_survives_the_trip_through_text() {
        for original in [frame(Some(5), 10, 20), frame(None, 7, 9)] {
            assert_eq!(
                parse_frame_line(&format_frame_line(&original)),
                Some(original)
            );
        }
        assert_eq!(parse_frame_line("anchor 1 2"), None);
        assert_eq!(parse_frame_line("frame reload=- start=1"), None);
    }

    #[test]
    fn the_four_intervals_add_up_to_the_total() {
        let t0 = 1_000;
        let frames = [frame(Some(t0 + 30_000), t0 + 50_000, t0 + 190_000)];
        let composed = t0 + 250_000;

        let sample = chain(t0, &frames, composed, HZ).unwrap();

        assert_eq!(sample.write_to_event_ms, 3.0);
        assert_eq!(sample.event_to_frame_ms, 2.0);
        assert_eq!(sample.frame_ms, 14.0);
        assert_eq!(sample.present_to_composed_ms, 6.0);
        assert_eq!(sample.total_ms, 25.0);
        let sum = sample.write_to_event_ms
            + sample.event_to_frame_ms
            + sample.frame_ms
            + sample.present_to_composed_ms;
        assert!((sum - sample.total_ms).abs() < 1e-9);
    }

    #[test]
    fn frames_from_before_the_edit_or_after_the_composition_are_not_its_frame() {
        let t0 = 1_000_000;
        let earlier = frame(Some(t0 - 10), t0 - 5, t0 + 5);
        let mine = frame(Some(t0 + 20_000), t0 + 30_000, t0 + 90_000);
        let redundant = frame(Some(t0 + 25_000), t0 + 400_000, t0 + 450_000);
        let composed = t0 + 120_000;

        let sample = chain(t0, &[earlier, mine, redundant], composed, HZ).unwrap();

        assert_eq!(sample.write_to_event_ms, 2.0);
        assert_eq!(sample.frame_ms, 6.0);
    }

    #[test]
    fn an_edit_no_frame_answered_has_no_sample() {
        assert_eq!(chain(100, &[frame(None, 150, 200)], 300, HZ), None);
        assert_eq!(chain(100, &[], 300, HZ), None);
    }

    #[test]
    fn consecutive_colors_always_differ_and_are_far_apart() {
        for round in 0..PALETTE.len() * 2 {
            let (a, b) = (color_for_round(round), color_for_round(round + 1));
            assert_ne!(a, b);
            assert!(!same_color(a, b));
        }
        assert!(same_color((10, 20, 30), (12, 18, 31)));
        assert!(!same_color((10, 20, 30), (13, 20, 30)));
    }

    #[test]
    fn the_stylesheet_carries_the_color_the_screen_is_compared_against() {
        assert!(stylesheet((200, 30, 30), 0).contains("background-color: #c81e1e"));
    }

    #[test]
    fn a_sized_stylesheet_is_about_that_big_and_ends_with_the_color_rule() {
        let text = stylesheet((200, 30, 30), 20);

        let wanted = 20 * 1024;
        assert!(
            text.len().abs_diff(wanted) < wanted / 10,
            "{} bytes for 20 KB",
            text.len()
        );
        let last = text.lines().last().unwrap();
        assert!(last.contains(".page") && last.contains("#c81e1e"), "{last}");
        assert!(
            florui_style::parse_stylesheet(&text).is_ok(),
            "the padding is real CSS"
        );
        assert!(!stylesheet((200, 30, 30), 0).contains("filler"));
    }

    fn sample_with_frame(frame_ms: f64) -> Sample {
        Sample {
            write_to_event_ms: 1.0,
            event_to_frame_ms: 2.0,
            frame_ms,
            present_to_composed_ms: 5.0,
            total_ms: 8.0 + frame_ms,
        }
    }

    #[test]
    fn a_comparison_names_the_interval_that_moved_and_leaves_the_others_alone() {
        let jitter = |base: f64, i: usize| base + (i % 5) as f64 * 0.1;
        let a: Vec<Sample> = (0..40)
            .map(|i| sample_with_frame(jitter(10.0, i)))
            .collect();
        let b: Vec<Sample> = (0..40)
            .map(|i| sample_with_frame(jitter(20.0, i)))
            .collect();

        let comparisons = compare_samples(&a, &b, 0.05);
        let verdict = |name: &str| {
            comparisons
                .iter()
                .find(|c| c.name.starts_with(name))
                .and_then(|c| c.comparison.as_ref())
                .map(|c| c.verdict)
        };

        assert_eq!(verdict("frame"), Some(stats::Verdict::Slower));
        assert_eq!(verdict("total"), Some(stats::Verdict::Slower));
        assert_eq!(
            verdict("watcher report"),
            Some(stats::Verdict::NoDifference)
        );
        assert_eq!(verdict("present call"), Some(stats::Verdict::NoDifference));
        let table = render_comparison("a", "b", &comparisons, 0.05);
        assert!(table.contains("| slower |"), "{table}");
        assert!(table.contains("+10."), "{table}");
    }

    #[test]
    fn each_interval_is_summarized_on_its_own() {
        let samples = [
            Sample {
                write_to_event_ms: 1.0,
                event_to_frame_ms: 2.0,
                frame_ms: 10.0,
                present_to_composed_ms: 5.0,
                total_ms: 18.0,
            },
            Sample {
                write_to_event_ms: 3.0,
                event_to_frame_ms: 2.0,
                frame_ms: 12.0,
                present_to_composed_ms: 7.0,
                total_ms: 24.0,
            },
        ];

        let summary = summarize_samples(&samples);

        assert_eq!(summary.len(), 5);
        assert_eq!(summary[0].summary.median, 2.0);
        assert_eq!(summary[4].summary.median, 21.0);
        assert!(summarize_samples(&[]).is_empty());
    }
}
