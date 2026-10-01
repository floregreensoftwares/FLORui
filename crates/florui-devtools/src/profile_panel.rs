//! The inspector's profile panel: a strip of recent frame times to pick a
//! frame from, and for the chosen one where its time went by phase, what each
//! stage counted, and what caused it, with the element and source line when
//! the cause was an event on one.
//!
//! The model is plain data built from [`florui_profile::FrameProfile`]s, so
//! what the panel says is tested without a window.

use florui_profile::{Counter, FrameProfile, Phase};

use crate::inspector::InspectorNode;

/// How many recent frames the strip shows.
pub const STRIP_FRAMES: usize = 120;

/// One frame's wall time budget at 60 Hz, in milliseconds.
pub const BUDGET_MS: f64 = 1000.0 / 60.0;

/// What the panel shows. `Default` is "nothing recorded and not built in".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProfileModel {
    /// Whether this build can measure at all (`florui_profile::ENABLED`).
    pub available: bool,
    pub recording: bool,
    /// Recent frames, oldest first.
    pub strip: Vec<StripBar>,
    /// The chosen frame, or the latest when none is chosen.
    pub frame: Option<FrameReport>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StripBar {
    pub index: u64,
    pub wall_ms: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameReport {
    pub index: u64,
    pub wall_ms: f64,
    pub phases: Vec<PhaseLine>,
    pub counters: Vec<(String, u64)>,
    pub causes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhaseLine {
    pub name: &'static str,
    /// 0 for a phase of its own, 1 for one inside `update` or `present`.
    pub depth: u8,
    pub calls: u32,
    pub ms: f64,
    /// Of the frame's wall time, `0.0..=1.0` (above 1 only if phases overlap).
    pub share: f64,
}

/// What the user did in the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileAction {
    ToggleRecording,
    Clear,
    SelectFrame(u64),
}

fn depth_of(phase: Phase) -> u8 {
    match phase {
        Phase::Render
        | Phase::Async
        | Phase::ArenaBuild
        | Phase::Sync
        | Phase::Cascade
        | Phase::Layout
        | Phase::PostLayout
        | Phase::Observers
        | Phase::Upload
        | Phase::Acquire
        | Phase::Submit
        | Phase::Flip => 1,
        Phase::Update
        | Phase::Restyle
        | Phase::PaintParts
        | Phase::Accessibility
        | Phase::Raster
        | Phase::Overlay
        | Phase::Present => 0,
    }
}

fn millis(duration: std::time::Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Builds the panel's model from `frames` (oldest first). `chosen` is the
/// frame index the user picked; the latest frame is shown when it is `None` or
/// no longer retained. `nodes` name the elements events were dispatched on.
pub fn build(
    frames: &[FrameProfile],
    nodes: &[InspectorNode],
    recording: bool,
    chosen: Option<u64>,
) -> ProfileModel {
    let shown = chosen
        .and_then(|index| frames.iter().find(|f| f.index == index))
        .or(frames.last());
    ProfileModel {
        available: florui_profile::ENABLED,
        recording,
        strip: frames
            .iter()
            .map(|f| StripBar {
                index: f.index,
                wall_ms: millis(f.wall()),
            })
            .collect(),
        frame: shown.map(|f| report(f, nodes)),
    }
}

fn report(frame: &FrameProfile, nodes: &[InspectorNode]) -> FrameReport {
    let wall_ms = millis(frame.wall());
    FrameReport {
        index: frame.index,
        wall_ms,
        phases: frame
            .phases
            .iter()
            .map(|p| PhaseLine {
                name: p.phase.name(),
                depth: depth_of(p.phase),
                calls: p.calls,
                ms: millis(p.total),
                share: if wall_ms > 0.0 {
                    millis(p.total) / wall_ms
                } else {
                    0.0
                },
            })
            .collect(),
        counters: frame
            .counters
            .iter()
            .map(|c| (counter_label(c.counter).to_string(), c.value))
            .collect(),
        causes: cause_lines(frame, nodes),
    }
}

fn counter_label(counter: Counter) -> &'static str {
    counter.name()
}

/// One line per distinct cause, with how many times it happened.
fn cause_lines(frame: &FrameProfile, nodes: &[InspectorNode]) -> Vec<String> {
    let mut lines: Vec<(String, usize)> = Vec::new();
    for cause in &frame.causes {
        let line = describe_cause(cause, nodes);
        match lines.iter_mut().find(|(existing, _)| *existing == line) {
            Some((_, count)) => *count += 1,
            None => lines.push((line, 1)),
        }
    }
    lines
        .into_iter()
        .map(|(line, count)| {
            if count > 1 {
                format!("{line}  x{count}")
            } else {
                line
            }
        })
        .collect()
}

fn describe_cause(cause: &florui_profile::Cause, nodes: &[InspectorNode]) -> String {
    let in_component = match cause.component {
        Some(component) => format!(" in {component}"),
        None => String::new(),
    };
    let during = cause.event.map(
        |(name, target)| match nodes.iter().find(|n| n.id == target) {
            Some(node) => match &node.source {
                Some(source) => format!(
                    "{name} on <{}> at {}:{}:{}",
                    node.tag, source.file, source.line, source.column
                ),
                None => format!("{name} on <{}>", node.tag),
            },
            None => format!("{name} on an element no longer in the tree"),
        },
    );
    match (cause.kind, during) {
        ("event", Some(during)) => during,
        ("signal-write", Some(during)) => format!("signal write{in_component}, during {during}"),
        ("signal-write", None) => format!("signal write{in_component}"),
        ("resource-started", _) => format!("resource started{in_component}"),
        ("resource-completed", _) => format!("resource completed{in_component}"),
        (kind, _) => format!("{kind}{in_component}"),
    }
}

/// Draws the panel and reports what the user did.
pub fn draw(ui: &mut egui::Ui, model: &ProfileModel) -> Option<ProfileAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        ui.heading("Profile");
        if !model.available {
            return;
        }
        let label = if model.recording { "Stop" } else { "Record" };
        if ui.button(label).clicked() {
            action = Some(ProfileAction::ToggleRecording);
        }
        if ui.button("Clear").clicked() {
            action = Some(ProfileAction::Clear);
        }
        if model.recording {
            ui.label("recording");
        }
    });
    if cfg!(debug_assertions) && model.available {
        ui.label(
            "Debug build: the engine runs unoptimized, so these times are far higher than a \
             release build's.",
        );
    }
    if !model.available {
        ui.label(
            "Profiling is not built into this build: it is on in a debug build, and in release \
             with the `profiling` feature.",
        );
        return action;
    }
    let Some(frame) = &model.frame else {
        ui.label(if model.recording {
            "waiting for the next frame"
        } else {
            "press Record, then use the app"
        });
        return action;
    };

    if let Some(picked) = draw_strip(ui, model, frame.index) {
        action = Some(ProfileAction::SelectFrame(picked));
    }
    ui.add_space(4.0);
    ui.monospace(format!(
        "frame {}: {:.2} ms ({:.0}% of a 60 Hz frame)",
        frame.index,
        frame.wall_ms,
        frame.wall_ms / BUDGET_MS * 100.0
    ));
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.label("Caused by");
        if frame.causes.is_empty() {
            ui.label("nothing recorded (a resize, an animation step or a repaint)");
        }
        for cause in &frame.causes {
            ui.monospace(cause);
        }
        ui.separator();
        for line in &frame.phases {
            ui.horizontal(|ui| {
                ui.add_space(f32::from(line.depth) * 14.0);
                ui.add(
                    egui::ProgressBar::new(line.share.clamp(0.0, 1.0) as f32)
                        .desired_width(120.0)
                        .text(format!("{:.2} ms", line.ms)),
                );
                ui.monospace(format!("{} x{}", line.name, line.calls));
            });
        }
        if !frame.counters.is_empty() {
            ui.separator();
            for (name, value) in &frame.counters {
                ui.monospace(format!("{name}: {value}"));
            }
        }
        ui.separator();
        ui.label(
            "The overlay phase includes the inspector's own work, so it is not what the \
             application costs.",
        );
    });
    action
}

fn draw_strip(ui: &mut egui::Ui, model: &ProfileModel, shown: u64) -> Option<u64> {
    let (response, painter) =
        ui.allocate_painter(egui::vec2(ui.available_width(), 56.0), egui::Sense::click());
    let rect = response.rect;
    painter.rect_filled(rect, 0.0, egui::Color32::from_gray(24));
    let count = model.strip.len().max(1);
    let tallest = model
        .strip
        .iter()
        .map(|b| b.wall_ms)
        .fold(BUDGET_MS * 2.0, f64::max);
    // Few frames stay narrow bars from the left instead of stretching across.
    let bar_width = (rect.width() / count as f32).min(10.0);
    for (position, bar) in model.strip.iter().enumerate() {
        let height = (bar.wall_ms / tallest) as f32 * rect.height();
        let left = rect.left() + position as f32 * bar_width;
        let color = if bar.index == shown {
            egui::Color32::WHITE
        } else if bar.wall_ms > BUDGET_MS {
            egui::Color32::from_rgb(230, 140, 60)
        } else {
            egui::Color32::from_rgb(90, 170, 110)
        };
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(left, rect.bottom() - height),
                egui::pos2(left + (bar_width - 0.5).max(1.0), rect.bottom()),
            ),
            0.0,
            color,
        );
    }
    let budget_y = rect.bottom() - (BUDGET_MS / tallest) as f32 * rect.height();
    painter.hline(
        rect.x_range(),
        budget_y,
        egui::Stroke::new(1.0, egui::Color32::from_rgb(200, 80, 80)),
    );
    if response.clicked()
        && let Some(position) = response.interact_pointer_pos()
    {
        let at = ((position.x - rect.left()) / bar_width) as usize;
        return model
            .strip
            .get(at.min(model.strip.len().saturating_sub(1)))
            .map(|b| b.index);
    }
    None
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use florui_profile::{Cause, CounterTotal, PhaseTotal};

    use super::*;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    fn frame(index: u64, wall: u64) -> FrameProfile {
        FrameProfile {
            index,
            start: ms(1000),
            end: ms(1000 + wall),
            phases: vec![
                PhaseTotal {
                    phase: Phase::Update,
                    calls: 1,
                    total: ms(wall / 2),
                },
                PhaseTotal {
                    phase: Phase::Layout,
                    calls: 2,
                    total: ms(wall / 4),
                },
                PhaseTotal {
                    phase: Phase::Raster,
                    calls: 1,
                    total: ms(wall / 2),
                },
            ],
            causes: Vec::new(),
            counters: vec![CounterTotal {
                counter: Counter::NodesPainted,
                value: 42,
            }],
            spans: Vec::new(),
            dropped_spans: 0,
        }
    }

    #[test]
    fn the_strip_lists_every_frame_and_the_latest_is_shown_by_default() {
        let model = build(&[frame(4, 10), frame(5, 20)], &[], true, None);
        assert_eq!(model.strip.len(), 2);
        assert_eq!(
            model.strip[1],
            StripBar {
                index: 5,
                wall_ms: 20.0
            }
        );
        assert_eq!(model.frame.as_ref().unwrap().index, 5);
        assert!(model.recording);
    }

    #[test]
    fn a_chosen_frame_is_shown_and_a_gone_one_falls_back_to_the_latest() {
        let frames = [frame(4, 10), frame(5, 20)];
        assert_eq!(build(&frames, &[], false, Some(4)).frame.unwrap().index, 4);
        assert_eq!(build(&frames, &[], false, Some(99)).frame.unwrap().index, 5);
    }

    #[test]
    fn no_frames_means_no_report() {
        let model = build(&[], &[], false, None);
        assert!(model.strip.is_empty() && model.frame.is_none());
    }

    #[test]
    fn phases_carry_their_share_of_the_wall_time_and_their_nesting() {
        let report = build(&[frame(1, 40)], &[], true, None).frame.unwrap();
        let layout = report.phases.iter().find(|p| p.name == "layout").unwrap();
        assert_eq!((layout.depth, layout.calls), (1, 2));
        assert_eq!(layout.ms, 10.0);
        assert_eq!(layout.share, 0.25);
        let raster = report.phases.iter().find(|p| p.name == "raster").unwrap();
        assert_eq!(raster.depth, 0);
        assert_eq!(report.counters, [("nodes-painted".to_string(), 42)]);
    }

    fn node(id: usize, tag: &'static str, source: Option<florui::SourceLocation>) -> InspectorNode {
        use florui_style::{Edges, Rgba};
        let zero = Edges {
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
            left: 0.0,
        };
        InspectorNode {
            id,
            depth: 0,
            tag: tag.to_string(),
            display: "block".to_string(),
            background: Rgba::TRANSPARENT,
            z_index: None,
            opacity: 1.0,
            overflow_clips: false,
            content: None,
            padding: zero,
            border: zero,
            margin: Edges {
                top: None,
                right: None,
                bottom: None,
                left: None,
            },
            size_cause: None,
            diagnostics: Vec::new(),
            source,
        }
    }

    #[test]
    fn an_event_cause_names_the_element_and_where_it_was_written() {
        let nodes = [node(
            7,
            "button",
            Some(florui::SourceLocation {
                file: "src/counter.rs",
                line: 12,
                column: 9,
                component: Some("Counter"),
                path: "div > button",
            }),
        )];
        let mut f = frame(1, 10);
        f.causes = vec![
            Cause {
                at: ms(1),
                component: Some("Counter"),
                kind: "event",
                event: Some(("click", 7)),
            },
            Cause {
                at: ms(1),
                component: Some("Counter"),
                kind: "signal-write",
                event: Some(("click", 7)),
            },
        ];
        let causes = build(&[f], &nodes, true, None).frame.unwrap().causes;
        assert_eq!(
            causes,
            [
                "click on <button> at src/counter.rs:12:9",
                "signal write in Counter, during click on <button> at src/counter.rs:12:9",
            ]
        );
    }

    #[test]
    fn repeated_causes_are_counted_and_unknown_elements_are_said_so() {
        let mut f = frame(1, 10);
        f.causes = vec![
            Cause {
                at: ms(1),
                component: None,
                kind: "signal-write",
                event: None
            };
            3
        ];
        f.causes.push(Cause {
            at: ms(2),
            component: None,
            kind: "event",
            event: Some(("click", 99)),
        });
        f.causes.push(Cause {
            at: ms(3),
            component: Some("Fetcher"),
            kind: "resource-completed",
            event: None,
        });
        let causes = build(&[f], &[], true, None).frame.unwrap().causes;
        assert_eq!(
            causes,
            [
                "signal write  x3",
                "click on an element no longer in the tree",
                "resource completed in Fetcher",
            ]
        );
    }

    #[test]
    fn an_element_built_by_hand_is_named_without_a_location() {
        let nodes = [node(2, "div", None)];
        let mut f = frame(1, 10);
        f.causes = vec![Cause {
            at: ms(1),
            component: None,
            kind: "event",
            event: Some(("click", 2)),
        }];
        assert_eq!(
            build(&[f], &nodes, true, None).frame.unwrap().causes,
            ["click on <div>"]
        );
    }
}
