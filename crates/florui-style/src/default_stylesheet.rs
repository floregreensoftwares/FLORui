//! The framework's own default element stylesheet — `stylesheets.md`'s
//! cascade-ordering step 1 ("Apply the framework's default element
//! stylesheet at its documented default-style precedence"), which nothing
//! previously registered. Parsed once, as real CSS through the same
//! [`crate::stylesheet_parse`] path an application's own stylesheets go
//! through (so it's checked by the same parser, not hand-built `Rule`s),
//! and always appended to the `Stylist` under `Origin::UserAgent` —
//! Stylo's own cascade-origin support, so an application rule of *any*
//! specificity overrides it, exactly like a real browser's UA stylesheet,
//! without this crate hand-rolling priority.
//!
//! Values below are Chromium's own well-established defaults (its `html.css`
//! / the WHATWG HTML "suggested rendering" defaults both browsers converge
//! on), not invented — `h1`–`h6`'s font-size/margin scale and `p`'s margin
//! are exact matches. Scoped to the tags `product.md` already names as
//! having "contracts defined by Florui" (`div`, `span`, `h2`, `button`),
//! extended to the rest of `h1`–`h6` and `p` for a coherent element set,
//! rather than attempting every HTML element up front.
//!
//! `span { display: inline; }` and `button { display: inline-block; }`
//! resolve to [`crate::cascade::Display::Inline`]/`InlineBlock` (added
//! alongside `florui-layout`'s own real inline formatting context), and
//! genuinely flow inline rather than stacking as blocks — see
//! `florui_layout`'s own module doc for that algorithm's exact, honest
//! bound (one level of mixed inline content; a plain `Inline` child, e.g.
//! a bare `<span>`, doesn't get its own box yet, only `InlineBlock`).
//!
//! `button`'s `1px solid #767676` border approximates Chromium's actual
//! default control border color (a `ButtonBorder`/`buttonborder` system
//! color, not invented) the same way the embedded sans-serif font
//! approximates Arial — a real, fixed value rather than querying the OS's
//! own theme, which this crate has no mechanism for. Default padding isn't
//! added here yet — tracked separately, not attempted in this slice.
//!
//! `input`'s own rule is checked directly against a real Chromium
//! (`getComputedStyle` on an injected `<input>`/`<button>`, not assumed):
//! `border: 2px inset rgb(118, 118, 118)`, `padding: 1px 2px`,
//! `background-color: rgb(255, 255, 255)`, `color: rgb(0, 0, 0)` (a real
//! UA reset, confirmed not to inherit an ancestor's own `color`), and
//! `display: inline-block`. The same flat-border simplification as
//! `button`'s own rule applies here too — real Chromium paints a 2px
//! inset (3D-beveled) border via native OS theming under
//! `appearance: auto`, which this crate has no mechanism to replicate (nor
//! does `button`'s own rule, which similarly flattens a real `2px outset
//! rgb(0, 0, 0)` down to `1px solid`) — only the *value* (`#767676`, the
//! same color `button` already uses) carries over, not the 3D bevel
//! rendering technique. Native OS theming as an opt-in, platform-specific
//! rendering mode (the way Expo/React Native lets an app render toward
//! each platform's own native look) is a real, deliberately separate
//! future direction, not something this default stylesheet attempts.
//!
//! `select`/`option` are checked the same way: `select` gets
//! `border: 1px solid #767676` (Chromium's own flat value here, no bevel
//! simplification needed — `border-style: solid` is what a real
//! `<select>` actually computes to, unlike `input`/`button`'s `inset`/
//! `outset`), `background-color: #ffffff`, `color: #000000`, and
//! `display: inline-block`. `option` gets `display: block` and
//! `padding: 0px 2px 1px 2px` (top/right/bottom/left, confirmed — note
//! the asymmetric top vs. `input`'s own `1px`).
//!
//! `select`'s own `padding` genuinely computes to `0` — the closed
//! control's visible text inset comes from `appearance: auto`'s internal
//! shadow-DOM rendering instead, invisible to `getComputedStyle`, so
//! there's no real property value to copy. `padding-left: 4px` here is a
//! *measured*, not confirmed, stand-in: pixel-compared (via this repo's
//! `florui compare` tool) against a zero-padding `<div>` with identical
//! text/font/border, to separate the select's own inset from the
//! glyphs' own left-bearing — one data point, one font-size, not
//! verified to scale like a real padding would. The dropdown-arrow glyph
//! is the same `appearance: auto` category with no fallback at all: no
//! CSS box of its own to even approximate a value for.
//!
//! `optgroup`/`.florui-optgroup-label` (a synthesized header for its own
//! `label` attribute — see `florui-platform`'s `select` module) are *not*
//! checked against a real Chromium the way everything above is:
//! `display: block` and a bold label are standard, undisputed
//! cross-browser behavior, not measured here.

use std::sync::LazyLock;

use style::stylesheets::Origin;

use crate::stylesheet_parse::{Rule, parse_stylesheet_with_origin};

const CSS: &str = "
    div, p, h1, h2, h3, h4, h5, h6 {
        display: block;
    }

    span {
        display: inline;
    }

    button {
        display: inline-block;
        border: 1px solid #767676;
    }

    input {
        display: inline-block;
        border: 1px solid #767676;
        background-color: #ffffff;
        color: #000000;
        padding-top: 1px;
        padding-right: 2px;
        padding-bottom: 1px;
        padding-left: 2px;
    }

    select {
        display: inline-block;
        border: 1px solid #767676;
        background-color: #ffffff;
        color: #000000;
        padding-left: 4px;
    }

    option {
        display: block;
        padding-top: 0px;
        padding-right: 2px;
        padding-bottom: 1px;
        padding-left: 2px;
    }

    optgroup {
        display: block;
    }

    .florui-optgroup-label {
        display: block;
        font-weight: bold;
    }

    h1, h2, h3, h4, h5, h6 {
        font-weight: bold;
    }

    h1 { font-size: 2em; margin-top: 0.67em; margin-bottom: 0.67em; }
    h2 { font-size: 1.5em; margin-top: 0.83em; margin-bottom: 0.83em; }
    h3 { font-size: 1.17em; margin-top: 1em; margin-bottom: 1em; }
    h4 { font-size: 1em; margin-top: 1.33em; margin-bottom: 1.33em; }
    h5 { font-size: 0.83em; margin-top: 1.67em; margin-bottom: 1.67em; }
    h6 { font-size: 0.67em; margin-top: 2.33em; margin-bottom: 2.33em; }

    p { margin-top: 1em; margin-bottom: 1em; }
";

/// The parsed default stylesheet, built once and reused — `Rule` wraps a
/// cheaply-`Clone`able `Arc`, so every [`crate::cascade::compute`] call
/// reuses this same parse rather than re-parsing static CSS text on every
/// render.
pub(crate) fn rule() -> Rule {
    static RULE: LazyLock<Rule> = LazyLock::new(|| {
        parse_stylesheet_with_origin(CSS, Origin::UserAgent)
            .expect("the framework's own default stylesheet is always valid CSS")
    });
    RULE.clone()
}

#[cfg(test)]
mod tests {
    use florui::prelude::*;

    use crate::cascade::{Display, Viewport, compute};
    use crate::interaction::InteractionState;
    use crate::stylesheet_parse::parse_stylesheet;
    use crate::tree::Arena;

    // `to_computed_style`'s margin resolution goes through Stylo's own
    // `em`-to-pixel math, not this crate's — a tight tolerance catches a
    // real regression without chasing float-rounding noise.
    const TOLERANCE: f32 = 0.01;

    fn assert_close(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < TOLERANCE,
            "{what}: expected {expected}, got {actual}"
        );
    }

    /// `compute` always registers this module's `rule()` under
    /// `Origin::UserAgent` before any author rules
    /// (`crate::stylo::compute_in_layout_state`) — so zero author CSS still
    /// exercises the default stylesheet, not a bare initial-value fallback.
    fn computed_style_of(tag_markup: Element) -> crate::cascade::ComputedStyle {
        let arena = Arena::build(&tag_markup);
        let rules = parse_stylesheet("").unwrap();
        let computed = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );
        computed[&arena.roots()[0]].clone()
    }

    #[test]
    fn div_defaults_to_block_with_zero_author_css() {
        let style = computed_style_of(view! { <div /> });
        assert_eq!(style.display, Display::Block);
    }

    #[test]
    fn p_defaults_to_block_with_one_em_margins() {
        let style = computed_style_of(view! { <p /> });
        assert_eq!(style.display, Display::Block);
        assert_close(style.margin.top.unwrap(), 16.0, "p margin-top");
        assert_close(style.margin.bottom.unwrap(), 16.0, "p margin-bottom");
    }

    /// Nested under a `<div>`, not at the tree root — real CSS blockifies a
    /// root element's `display` regardless of what it's authored as
    /// (<https://drafts.csswg.org/css-display/#blockify>; Stylo applies
    /// this on its own, not something this crate implements), so a bare
    /// `<span>`/`<button>` with no parent would read back as `Block` for a
    /// reason that has nothing to do with this module's own rules — nesting
    /// them is what actually exercises `span { display: inline; }`/
    /// `button { display: inline-block; }`.
    #[test]
    fn span_and_button_resolve_inline_and_inline_block() {
        let tree: Element = view! {
            <div>
                <span />
                <button />
            </div>
        };
        let arena = Arena::build(&tree);
        let div = arena.roots()[0];
        let span = arena.children(div)[0];
        let button = arena.children(div)[1];
        let rules = parse_stylesheet("").unwrap();
        let computed = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );

        assert_eq!(computed[&span].display, Display::Inline);
        assert_eq!(computed[&button].display, Display::InlineBlock);
    }

    #[test]
    fn button_resolves_a_visible_default_border_on_every_side_with_zero_author_css() {
        let tree: Element = view! {
            <div>
                <button />
            </div>
        };
        let arena = Arena::build(&tree);
        let div = arena.roots()[0];
        let button = arena.children(div)[0];
        let rules = parse_stylesheet("").unwrap();
        let computed = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );

        let border = computed[&button].border;
        for side in [border.top, border.right, border.bottom, border.left] {
            assert_close(side.width, 1.0, "button border-width");
            assert_eq!(side.color, crate::color::Rgba::opaque(0x76, 0x76, 0x76));
        }
    }

    #[test]
    fn input_resolves_chromiums_real_default_appearance_with_zero_author_css() {
        // Verified directly against a real Chromium (`getComputedStyle` on
        // an injected `<input>`), not assumed -- see this module's own doc
        // for the exact values and the flat-border simplification, shared
        // with `button`'s own already-established one.
        // Wrapped in a `<div>`, matching `button`'s own test above --
        // real CSS blockifies a root element's own `display` (an
        // `inline`/`inline-block` root computes to `block`), which would
        // otherwise mask the real `inline-block` this rule declares.
        let tree: Element = view! {
            <div>
                <input type="text" />
            </div>
        };
        let arena = Arena::build(&tree);
        let div = arena.roots()[0];
        let input = arena.children(div)[0];
        let rules = parse_stylesheet("").unwrap();
        let computed = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );
        let style = &computed[&input];
        assert_eq!(style.display, Display::InlineBlock);
        for side in [
            style.border.top,
            style.border.right,
            style.border.bottom,
            style.border.left,
        ] {
            assert_close(side.width, 1.0, "input border-width");
            assert_eq!(side.color, crate::color::Rgba::opaque(0x76, 0x76, 0x76));
        }
        assert_eq!(
            style.background_color,
            crate::color::Rgba::opaque(0xff, 0xff, 0xff)
        );
        assert_eq!(style.color, crate::color::Rgba::opaque(0x00, 0x00, 0x00));
        assert_close(style.padding.top, 1.0, "input padding-top");
        assert_close(style.padding.right, 2.0, "input padding-right");
        assert_close(style.padding.bottom, 1.0, "input padding-bottom");
        assert_close(style.padding.left, 2.0, "input padding-left");
    }

    #[test]
    fn select_and_option_resolve_chromiums_real_default_appearance_with_zero_author_css() {
        // Verified directly against a real Chromium (`getComputedStyle` on
        // an injected `<select>`/`<option>`), not assumed -- see this
        // module's own doc for the exact values, and for why
        // `select_style.padding.left` alone is measured, not confirmed.
        let tree: Element = view! {
            <div>
                <select>
                    <option>{"A"}</option>
                </select>
            </div>
        };
        let arena = Arena::build(&tree);
        let div = arena.roots()[0];
        let select = arena.children(div)[0];
        let option = arena.children(select)[0];
        let rules = parse_stylesheet("").unwrap();
        let computed = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );

        let select_style = &computed[&select];
        assert_eq!(select_style.display, Display::InlineBlock);
        for side in [
            select_style.border.top,
            select_style.border.right,
            select_style.border.bottom,
            select_style.border.left,
        ] {
            assert_close(side.width, 1.0, "select border-width");
            assert_eq!(side.color, crate::color::Rgba::opaque(0x76, 0x76, 0x76));
        }
        assert_eq!(
            select_style.background_color,
            crate::color::Rgba::opaque(0xff, 0xff, 0xff)
        );
        assert_eq!(
            select_style.color,
            crate::color::Rgba::opaque(0x00, 0x00, 0x00)
        );
        assert_close(select_style.padding.top, 0.0, "select padding-top");
        assert_close(select_style.padding.right, 0.0, "select padding-right");
        assert_close(select_style.padding.left, 4.0, "select padding-left");

        let option_style = &computed[&option];
        assert_eq!(option_style.display, Display::Block);
        assert_close(option_style.padding.top, 0.0, "option padding-top");
        assert_close(option_style.padding.right, 2.0, "option padding-right");
        assert_close(option_style.padding.bottom, 1.0, "option padding-bottom");
        assert_close(option_style.padding.left, 2.0, "option padding-left");
    }

    #[test]
    fn div_has_no_default_border() {
        let style = computed_style_of(view! { <div /> });
        for side in [
            style.border.top,
            style.border.right,
            style.border.bottom,
            style.border.left,
        ] {
            assert_eq!(side.width, 0.0, "div has no default border, unlike button");
        }
    }

    #[test]
    fn h1_through_h6_resolve_the_chromium_font_size_and_margin_scale() {
        let cases: [(Element, f32, f32); 6] = [
            (view! { <h1 /> }, 32.0, 21.44),
            (view! { <h2 /> }, 24.0, 19.92),
            (view! { <h3 /> }, 18.72, 18.72),
            (view! { <h4 /> }, 16.0, 21.28),
            (view! { <h5 /> }, 13.28, 22.1776),
            (view! { <h6 /> }, 10.72, 24.9776),
        ];
        for (markup, expected_font_size, expected_margin) in cases {
            let style = computed_style_of(markup);
            assert_eq!(style.display, Display::Block);
            assert_close(style.font_size, expected_font_size, "font-size");
            assert_eq!(style.font_weight, 700.0, "h1-h6 are bold by default");
            assert_close(style.margin.top.unwrap(), expected_margin, "margin-top");
            assert_close(
                style.margin.bottom.unwrap(),
                expected_margin,
                "margin-bottom",
            );
        }
    }

    #[test]
    fn p_and_div_are_not_bold_by_default() {
        assert_eq!(computed_style_of(view! { <div /> }).font_weight, 400.0);
        assert_eq!(computed_style_of(view! { <p /> }).font_weight, 400.0);
    }

    /// The actual point of `Origin::UserAgent`: an author rule overrides
    /// the default stylesheet regardless of specificity, not merely because
    /// it happens to match with equal or higher specificity. `*` has zero
    /// specificity — far below `h1`'s type-selector specificity — yet still
    /// wins, because origin precedence outranks specificity in the real CSS
    /// cascade. If this crate ever regressed to a hand-rolled "last one
    /// wins" merge instead of real `Origin`-aware cascading, this is the
    /// test that would catch it; equal-specificity author-vs-author cases
    /// wouldn't.
    #[test]
    fn a_lower_specificity_author_rule_still_overrides_the_higher_specificity_default_rule() {
        let tree: Element = view! { <h1 /> };
        let arena = Arena::build(&tree);
        let rules = parse_stylesheet("* { font-size: 40px; }").unwrap();
        let computed = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );
        let style = &computed[&arena.roots()[0]];
        assert_close(style.font_size, 40.0, "author-overridden font-size");
    }
}
