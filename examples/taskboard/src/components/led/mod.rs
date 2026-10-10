//! A lamp: dark glass when off, a lit jewel with a glow when on.

use florui::prelude::*;

stylesheet!("./led.css");

/// What color a lamp shines when lit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Amber,
    Green,
    Red,
    Blue,
}

impl Tone {
    pub fn class(self) -> &'static str {
        match self {
            Tone::Amber => "amber",
            Tone::Green => "green",
            Tone::Red => "red",
            Tone::Blue => "blue",
        }
    }
}

#[component]
pub fn Led(tone: Tone, lit: bool) -> Element {
    let class = if lit {
        format!("led {} lit", tone.class())
    } else {
        format!("led {}", tone.class())
    };
    view! { <span class={class}></span> }
}
