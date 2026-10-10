//! The preferences, and what each one does to the desk: a class on the root
//! element, or a custom property the renderer reads.

use florui::prelude::*;

/// How many detents the grain knob has: smooth, light, heavy.
pub const GRAIN_STEPS: usize = 3;

#[derive(Clone)]
pub struct Preferences {
    glow: Signal<bool>,
    grain: Signal<usize>,
    fast_glass: Signal<bool>,
}

impl Preferences {
    /// Creates the preferences. A hook: call it from a component.
    pub fn use_new() -> Self {
        Self {
            glow: use_signal(|| true),
            grain: use_signal(|| 1),
            fast_glass: use_signal(|| false),
        }
    }

    pub fn glow(&self) -> Binding<bool> {
        self.glow.binding()
    }

    pub fn grain(&self) -> Binding<usize> {
        self.grain.binding()
    }

    pub fn fast_glass(&self) -> Binding<bool> {
        self.fast_glass.binding()
    }

    /// The classes of the root element: the surfaces, and the preferences that
    /// are a class the stylesheets react to.
    pub fn desk_classes(&self) -> String {
        let grain = self.grain.get().min(GRAIN_STEPS - 1);
        let glow = if self.glow.get() {
            "glow-on"
        } else {
            "glow-off"
        };
        format!("desk wood {glow} grain-{grain}")
    }

    /// The inline style of the root element: the preferences that are a custom
    /// property the renderer reads, inherited by everything inside.
    pub fn desk_style(&self) -> String {
        if self.fast_glass.get() {
            "--florui-backdrop-blur: fast;".to_string()
        } else {
            String::new()
        }
    }

    /// What the grain knob's detent is called.
    pub fn grain_name(step: usize) -> &'static str {
        match step {
            0 => "smooth",
            1 => "light",
            _ => "heavy",
        }
    }
}
