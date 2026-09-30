//! Real CSS selectors, cascade, and inheritance against a
//! `florui::Element` tree, via [Stylo](https://github.com/servo/stylo) —
//! the "style" stage between elements and boxes in the engine's pipeline
//! (components → elements → **style** → boxes → layout → text →
//! painting).
//!
//! # Scope
//!
//! Selector matching, cascade, and inheritance are Stylo's real
//! implementation (see [`stylo`] for the bridge onto [`Arena`]) — not a
//! closed, hand-validated subset. CSS this crate's own downstream
//! consumers don't yet render (a child combinator, `!important`, most
//! properties beyond the seven below) parses
//! successfully; it simply has no visible effect yet, the same way an
//! unsupported property behaves in a real browser rather than being
//! rejected as invalid input.
//!
//! [`ComputedStyle`] currently exposes seven resolved properties:
//! `background-color`/`color` (colors; `color` inherits,
//! `background-color` doesn't), `width`/`height` (a resolved pixel length
//! or `None` for `auto` — or anything else this crate can't yet resolve
//! to one concrete length, like a percentage), `font-size` (inherits;
//! initial `16px`), and the `margin-*`/`padding-*` longhands (margin also
//! distinguishes an explicit `auto` from an unset `0`; padding has no
//! `auto`, matching real CSS). Growing this list is a `florui-style`
//! change only — Stylo already computes every property real CSS defines.
//!
//! `:hover`/`:focus`/`:focus-visible`/`:active` are matched against an
//! [`InteractionState`](interaction::InteractionState) the caller builds
//! directly (see its docs) — this crate has no pointer/keyboard event
//! system of its own to derive it from.

mod animation;
mod cascade;
mod color;
mod container_query_adapter;
mod default_stylesheet;
mod error;
mod focus;
mod height_media_adapter;
mod interaction;
mod reduced_motion_adapter;
mod rounded;
mod scope_adapter;
mod scroll_behavior_adapter;
mod stylesheet_parse;
mod stylo;
mod tree;

pub use animation::AnimationTimeline;
pub use cascade::{
    Appearance, AspectRatio, BorderSide, BoxShadow, ComputedStyle, ContainerType, ContentAlignment,
    Corners, Display, Edges, FilterFunction, FlexDirection, FlexWrap, FontFamily, GridPlacement,
    GridTrackSize, ItemAlignment, LengthPercentage, ObjectFit, Position, Resize, TransformFunction,
    Viewport, compute, compute_with_container_query_signature,
};
pub use color::{ColorParseError, Rgba, parse_hex_color};
pub use container_query_adapter::{ContentBoxSize, resolve_container_query_signatures};
pub use error::StyleError;
pub use focus::FocusPath;
pub use interaction::{FormState, InteractionState};
pub use rounded::RoundedRect;
pub use stylesheet_parse::{Rule, compile_sources, parse_stylesheet};
pub use tree::{AccessibleRole, Arena, InlineItem, NodeId};

#[cfg(test)]
mod integration_tests {
    use florui::prelude::*;

    use super::*;

    /// The flagship stylesheet example, styled for real: a class
    /// selector matching a view!-built button, with a real :hover cascade
    /// on top of it — end to end through this crate's public API only.
    #[test]
    fn styles_the_button_flagship_example_end_to_end() {
        #[component]
        fn Button(label: String) -> Element {
            view! { <button class="button">{label}</button> }
        }

        let css = "
            .button {
                background-color: #42734f;
                color: #ffffff;
            }
            .button:hover {
                background-color: #345c3e;
            }
        ";

        let tree = render_once(|| {
            Button(ButtonProps {
                label: "Open projects".to_string(),
            })
        });
        let arena = Arena::build(&tree);
        let rules = parse_stylesheet(css).unwrap();
        let button = arena.find(|a, id| a.tag(id) == "button").unwrap();

        let idle = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );
        assert_eq!(
            idle[&button].background_color,
            Rgba::opaque(0x42, 0x73, 0x4f)
        );
        assert_eq!(idle[&button].color, Rgba::opaque(0xff, 0xff, 0xff));

        let hovered_state = InteractionState::new().with_hovered(button);
        let hovered = compute(
            &arena,
            &rules,
            &hovered_state,
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );
        assert_eq!(
            hovered[&button].background_color,
            Rgba::opaque(0x34, 0x5c, 0x3e)
        );
        assert_eq!(
            hovered[&button].color,
            Rgba::opaque(0xff, 0xff, 0xff),
            "hover rule doesn't touch color, so it stays"
        );
    }
}
