//! Workloads for composition effects: a panel over busy content, once opaque,
//! once translucent with no filter, and then with a filter or a backdrop
//! filter, varying its area, blur radius, overlap and display scale.
//!
//! The baselines are what make an effect's cost readable. A translucent panel
//! of the same area and content with no filter is the equivalent work without
//! the effect, so the difference between it and a filtered panel is what the
//! filter costs; the opaque panel is the cheapest way to cover the same pixels.
//! Each effect workload asserts when it is built that it paints differently
//! from the translucent baseline of its area, so a declaration the engine
//! ignores cannot quietly be measured as the baseline.
//!
//! One sample is a full paint of the scene (the engine repaints everything, so
//! a moving background costs the same as a still one). The heap columns of a
//! run give the intermediate memory the effect needs.

use std::hint::black_box;

use florui::Element;
use florui_platform::{HeadlessOptions, HeadlessWindow};
use florui_style::Rgba;

use crate::workloads::Workload;

/// How many 25 px tiles fill the 800 x 600 window.
const TILES: usize = 32 * 24;

const COLORS: [&str; 8] = [
    "#e63946", "#f4a261", "#e9c46a", "#2a9d8f", "#264653", "#8338ec", "#3a86ff", "#06d6a0",
];

/// What the panel declares, besides its geometry and background.
#[derive(Clone, Copy)]
enum Effect {
    None,
    Backdrop(u32),
    Blur(u32),
    Chain,
    /// A backdrop blur with the refracting glass material and a rim light.
    Glass {
        blur: u32,
        refraction: u32,
        edge: u32,
        reduced: bool,
    },
}

impl Effect {
    fn css(self) -> String {
        match self {
            Effect::None => String::new(),
            Effect::Backdrop(radius) => format!("backdrop-filter: blur({radius}px);"),
            Effect::Blur(radius) => format!("filter: blur({radius}px);"),
            Effect::Chain => "filter: brightness(1.2) contrast(1.3) saturate(1.5);".into(),
            Effect::Glass {
                blur,
                refraction,
                edge,
                reduced,
            } => format!(
                "backdrop-filter: blur({blur}px); --florui-glass: refract;                  --florui-glass-refraction: {refraction}px; --florui-glass-edge: {edge}px;                  --florui-glass-light-strength: 0.5; --florui-glass-quality: {};",
                if reduced { "reduced" } else { "full" }
            ),
        }
    }
}

/// One panel: where it sits, how big it is and what it declares.
#[derive(Clone, Copy)]
struct Panel {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
    opaque: bool,
    effect: Effect,
}

#[derive(Clone)]
struct Scene {
    panels: Vec<Panel>,
    scale_factor: f64,
}

fn node(class: &str, children: Vec<Element>) -> Element {
    Element::node("div", vec![("class".into(), class.into())], children)
}

impl Scene {
    fn css(&self) -> String {
        let mut css = String::from(
            ".busy { display: flex; flex-direction: row; flex-wrap: wrap; width: 800px; \
             height: 600px; }\n.tile { width: 25px; height: 25px; }\n\
             .inner { width: 60px; height: 40px; margin: 8px; }\n",
        );
        for (i, color) in COLORS.iter().enumerate() {
            css.push_str(&format!(".c{i} {{ background-color: {color}; }}\n"));
        }
        for (i, panel) in self.panels.iter().enumerate() {
            let background = if panel.opaque {
                "#f2f2f2"
            } else {
                "rgba(255, 255, 255, 0.1)"
            };
            css.push_str(&format!(
                ".panel{i} {{ position: absolute; left: {}px; top: {}px; width: {}px; \
                 height: {}px; background-color: {background}; {} }}\n",
                panel.left,
                panel.top,
                panel.width,
                panel.height,
                panel.effect.css()
            ));
        }
        css
    }

    fn tree(&self) -> Element {
        let mut children: Vec<Element> = (0..TILES)
            .map(|i| node(&format!("tile c{}", i % COLORS.len()), Vec::new()))
            .collect();
        for (i, _) in self.panels.iter().enumerate() {
            // Content inside the panel, so a filter on it has something to filter.
            let inner = (0..6)
                .map(|k| node(&format!("inner c{}", (i + k) % COLORS.len()), Vec::new()))
                .collect();
            children.push(node(&format!("panel{i}"), inner));
        }
        node("busy", children)
    }

    fn window(&self) -> HeadlessWindow {
        HeadlessWindow::new(
            &self.css(),
            {
                let scene = self.clone();
                move || scene.tree()
            },
            HeadlessOptions {
                scale_factor: self.scale_factor,
                canvas_color: Rgba::opaque(0x10, 0x10, 0x14),
                ..HeadlessOptions::default()
            },
        )
        .expect("the benchmark stylesheet is valid")
    }

    /// The same scene with every panel's effect removed and the panels made
    /// translucent: the equivalent work without the effect.
    fn baseline(&self) -> Scene {
        Scene {
            panels: self
                .panels
                .iter()
                .map(|panel| Panel {
                    opaque: false,
                    effect: Effect::None,
                    ..*panel
                })
                .collect(),
            scale_factor: self.scale_factor,
        }
    }
}

fn panel(width: u32, height: u32, effect: Effect) -> Panel {
    Panel {
        left: 100,
        top: 100,
        width,
        height,
        opaque: false,
        effect,
    }
}

/// Builds the workload's operation: a paint of the scene. With `compare`, it
/// first asserts that the scene paints differently from `compare`.
fn paint(scene: Scene, compare: Option<Scene>) -> Box<dyn FnMut()> {
    let mut win = scene.window();
    let painted = win.frame().rgba;
    if let Some(baseline) = compare {
        let reference = baseline.window().frame().rgba;
        assert!(
            painted != reference,
            "the effect workload paints exactly what its translucent baseline does, so the \
             effect is ignored and it would measure the baseline"
        );
    }
    Box::new(move || {
        black_box(win.frame());
    })
}

fn with_effect(scene: Scene) -> Box<dyn FnMut()> {
    let baseline = scene.baseline();
    paint(scene, Some(baseline))
}

fn glass(blur: u32, refraction: u32, edge: u32, reduced: bool) -> Effect {
    Effect::Glass {
        blur,
        refraction,
        edge,
        reduced,
    }
}

/// A glass workload: it must differ from its translucent baseline and from the
/// same scene with only the backdrop blur, so the material is what is measured.
fn with_glass(scene: Scene, blur: u32) -> Box<dyn FnMut()> {
    let mut basic = scene.clone();
    for panel in &mut basic.panels {
        panel.effect = Effect::Backdrop(blur);
    }
    let reference = basic.window().frame().rgba;
    let mut win = scene.window();
    assert_ne!(
        win.frame().rgba,
        reference,
        "the material changed nothing, so the workload would measure the basic backdrop blur"
    );
    drop(win);
    with_effect(scene)
}

fn one(width: u32, height: u32, effect: Effect, opaque: bool, scale_factor: f64) -> Scene {
    Scene {
        panels: vec![Panel {
            opaque,
            ..panel(width, height, effect)
        }],
        scale_factor,
    }
}

/// Three panels that overlap each other, each over the content and over the
/// ones beneath it.
fn overlapping(effect: Effect, opaque: bool) -> Scene {
    Scene {
        panels: [(60, 60), (160, 130), (260, 200)]
            .into_iter()
            .map(|(left, top)| Panel {
                left,
                top,
                width: 320,
                height: 240,
                opaque,
                effect,
            })
            .collect(),
        scale_factor: 1.0,
    }
}

pub fn all() -> Vec<Workload> {
    vec![
        Workload {
            name: "effects_opaque_large",
            description: "Paint of busy content with one opaque 600 x 400 panel over it",
            exercises: "the baseline: covering the same pixels with an opaque fill",
            build: || paint(one(600, 400, Effect::None, true, 1.0), None),
        },
        Workload {
            name: "effects_translucent_large",
            description: "The same panel translucent, with no filter",
            exercises: "alpha blending over busy content; the equivalent work without an effect",
            build: || paint(one(600, 400, Effect::None, false, 1.0), None),
        },
        Workload {
            name: "effects_translucent_small",
            description: "A translucent 200 x 150 panel with no filter",
            exercises: "the baseline for the small backdrop-filter panel",
            build: || paint(one(200, 150, Effect::None, false, 1.0), None),
        },
        Workload {
            name: "effects_backdrop_blur_8_small",
            description: "A translucent 200 x 150 panel with backdrop-filter: blur(8px)",
            exercises: "sampling and blurring the content behind a small area",
            build: || with_effect(one(200, 150, Effect::Backdrop(8), false, 1.0)),
        },
        Workload {
            name: "effects_backdrop_blur_8_large",
            description: "A translucent 600 x 400 panel with backdrop-filter: blur(8px)",
            exercises: "the same blur over a larger area",
            build: || with_effect(one(600, 400, Effect::Backdrop(8), false, 1.0)),
        },
        Workload {
            name: "effects_backdrop_blur_24_large",
            description: "A translucent 600 x 400 panel with backdrop-filter: blur(24px)",
            exercises: "a larger blur radius over the same area",
            build: || with_effect(one(600, 400, Effect::Backdrop(24), false, 1.0)),
        },
        Workload {
            name: "effects_translucent_overlap3",
            description: "Three overlapping translucent 320 x 240 panels with no filter",
            exercises: "the baseline for overlapping backdrop-filter panels",
            build: || paint(overlapping(Effect::None, false), None),
        },
        Workload {
            name: "effects_backdrop_blur_8_overlap3",
            description: "Three overlapping translucent panels, each with backdrop-filter: blur(8px)",
            exercises: "each panel blurs the content and the panels beneath it",
            build: || with_effect(overlapping(Effect::Backdrop(8), false)),
        },
        Workload {
            name: "effects_translucent_large_dpr2",
            description: "The translucent 600 x 400 panel at a display scale of 2",
            exercises: "the baseline at 1600 x 1200 physical pixels",
            build: || paint(one(600, 400, Effect::None, false, 2.0), None),
        },
        Workload {
            name: "effects_backdrop_blur_8_large_dpr2",
            description: "The 600 x 400 backdrop-filter: blur(8px) panel at a display scale of 2",
            exercises: "four times the pixels for the same effect",
            build: || with_effect(one(600, 400, Effect::Backdrop(8), false, 2.0)),
        },
        Workload {
            name: "effects_glass_advanced_small",
            description: "A translucent 200 x 150 panel with blur(8px) and the refracting material",
            exercises: "refraction and rim light on a small panel, over the basic backdrop blur",
            build: || with_glass(one(200, 150, glass(8, 12, 24, false), false, 1.0), 8),
        },
        Workload {
            name: "effects_glass_advanced_large",
            description: "A translucent 600 x 400 panel with blur(8px) and the refracting material",
            exercises: "the cost the material adds to effects_backdrop_blur_8_large",
            build: || with_glass(one(600, 400, glass(8, 12, 24, false), false, 1.0), 8),
        },
        Workload {
            name: "effects_glass_advanced_wide_band",
            description: "The 600 x 400 material panel with a 32 px band and a 24 px refraction",
            exercises: "a wider lensing band and a larger displacement",
            build: || with_glass(one(600, 400, glass(8, 24, 32, false), false, 1.0), 8),
        },
        Workload {
            name: "effects_glass_reduced_large",
            description: "The 600 x 400 material panel at quality reduced (nearest-pixel sampling)",
            exercises: "the cheaper quality mode of the material",
            build: || with_glass(one(600, 400, glass(8, 12, 24, true), false, 1.0), 8),
        },
        Workload {
            name: "effects_glass_advanced_overlap3",
            description: "Three overlapping material panels, each with blur(8px)",
            exercises: "each panel refracts the content and the panels beneath it",
            build: || with_glass(overlapping(glass(8, 12, 24, false), false), 8),
        },
        Workload {
            name: "effects_glass_advanced_large_dpr2",
            description: "The 600 x 400 material panel at a display scale of 2",
            exercises: "four times the pixels for the same material",
            build: || with_glass(one(600, 400, glass(8, 12, 24, false), false, 2.0), 8),
        },
        Workload {
            name: "effects_filter_blur_8",
            description: "A translucent 600 x 400 panel and its content with filter: blur(8px)",
            exercises: "an element rendered to an intermediate surface and blurred",
            build: || with_effect(one(600, 400, Effect::Blur(8), false, 1.0)),
        },
        Workload {
            name: "effects_filter_chain",
            description: "The same panel with filter: brightness() contrast() saturate()",
            exercises: "an ordered chain of color filters on an intermediate surface",
            build: || with_effect(one(600, 400, Effect::Chain, false, 1.0)),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panel_sits_where_the_scene_says_and_has_its_size() {
        let scene = one(600, 400, Effect::None, false, 1.0);
        let mut win = scene.window();
        let bounds = win.frame().bounds;
        let (arena, ..) = win.runtime().geometry();
        let panel = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == "panel0"))
            .expect("the panel is in the tree");
        assert_eq!(bounds[&panel], (100.0, 100.0, 600.0, 400.0));
    }

    #[test]
    fn a_display_scale_of_two_paints_four_times_the_pixels() {
        let one_x = one(600, 400, Effect::None, false, 1.0).window().frame();
        let two_x = one(600, 400, Effect::None, false, 2.0).window().frame();
        assert_eq!(
            (two_x.width, two_x.height),
            (one_x.width * 2, one_x.height * 2)
        );
    }

    #[test]
    fn an_effect_paints_differently_from_its_baseline_and_the_baseline_has_none() {
        for effect in [
            Effect::Backdrop(8),
            Effect::Backdrop(24),
            Effect::Blur(8),
            Effect::Chain,
            glass(8, 12, 24, false),
            glass(8, 12, 24, true),
        ] {
            let scene = one(600, 400, effect, false, 1.0);
            let with = scene.window().frame().rgba;
            let without = scene.baseline().window().frame().rgba;
            assert_ne!(with, without, "an effect changes the picture");
        }
        // The baseline of a scene without an effect is that scene itself.
        let plain = one(600, 400, Effect::None, false, 1.0);
        assert_eq!(
            plain.window().frame().rgba,
            plain.baseline().window().frame().rgba
        );
    }

    #[test]
    fn every_effect_workload_builds_and_runs() {
        // Building each one asserts that its effect changes the picture.
        for workload in all() {
            let mut op = (workload.build)();
            op();
            op();
        }
    }

    #[test]
    fn the_workload_names_are_unique() {
        let mut names: Vec<&str> = all().iter().map(|w| w.name).collect();
        let count = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), count);
    }
}
