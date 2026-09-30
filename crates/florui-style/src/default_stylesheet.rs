//! The framework's own default element stylesheet, the first step of the
//! cascade ordering ("Apply the framework's default element
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
//! are exact matches. Scoped to the tags the product scope already names as
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
//! That 4px stand-in belongs only to the closed, single-line trigger —
//! confirmed against a real Chromium's own `getComputedStyle` that
//! `select[multiple]` reports `padding: 0` on every side, same as the
//! closed control, and visually shows no left-only inset at all: real
//! Chromium's UA stylesheet already renders a multi-select listbox as a
//! plain `appearance: listbox` box, distinct from the closed control's
//! `appearance: menulist`, with each row's own inset coming entirely from
//! `option`'s own padding. `select[multiple="true"]` below overrides the
//! stand-in back to `0` for that reason — not a florui-specific quirk,
//! the real native default itself draws no such inset there.
//!
//! `optgroup`/`.florui-optgroup-label` (a synthesized header for its own
//! `label` attribute — see `florui-platform`'s `select` module) are *not*
//! checked against a real Chromium the way everything above is:
//! `display: block` and a bold label are standard, undisputed
//! cross-browser behavior, not measured here.
//!
//! `a:link`'s `color: #0000ee` is checked directly against a real
//! Chromium (headless, `getComputedStyle` on an injected `<a href>`, not
//! assumed) -- confirmed `rgb(0, 0, 238)`. `a:visited`'s `color:
//! #551a8b` is the paired well-established value from the same suggested
//! rendering both browsers converge on (same "not invented" tier as
//! `h1`-`h6`'s scale above) -- a live measurement was attempted too, but
//! Chromium deliberately excludes `file://` URLs from `:visited` history
//! (a privacy restriction, not a bug), so there was no way to make a
//! local one register as visited to check against. `underline`/`pointer`
//! apply to both states alike, matching real Chromium.
//!
//! `img { display: block; }` is a deliberate, documented divergence from
//! real CSS, not an oversight: a real, unstyled `<img>` is inline-level
//! (it flows inside a line of text). Leaving `display` unset here instead
//! computes to CSS's own generic initial value, `inline` — and
//! `florui_layout`'s inline-formatting-context algorithm (see its own
//! module doc) only understands plain text and `InlineBlock` children so
//! far, not a replaced element's own intrinsic content; an `<img>` swept
//! into that path silently got no box of its own at all — a real,
//! observed regression (a single `<img>` inside an otherwise-empty
//! container), not a hypothetical one. `display: block` sidesteps that
//! entirely until inline flow for a replaced element is its own,
//! separate follow-up. `icon` (a themable SVG icon, see
//! `florui-platform::icon`'s own doc) is the exact same kind of replaced
//! element, so it needs the identical rule for the identical reason.

use std::sync::LazyLock;

use style::stylesheets::Origin;

use crate::stylesheet_parse::{Rule, parse_stylesheet_with_origin};

const CSS: &str = "
    div, p, h1, h2, h3, h4, h5, h6, img, icon, form, fieldset, legend {
        display: block;
    }

    span, a {
        display: inline;
    }

    a:link, a:visited {
        text-decoration: underline;
        cursor: pointer;
    }

    a:link {
        color: #0000ee;
    }

    a:visited {
        color: #551a8b;
    }

    fieldset {
        margin: 0px 2px;
        border: 2px groove #f0f0f0;
        padding: 0.35em 0.75em 0.625em;
    }

    legend {
        padding: 0px 2px;
    }

    button {
        display: inline-block;
        border: 1px solid #767676;
    }

    input[type=\"hidden\"] {
        display: none;
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

    .florui-validation-bubble {
        position: absolute;
        top: 0px;
        left: 0px;
        display: flex;
        flex-direction: row;
        align-items: flex-start;
        padding-top: 8px;
        padding-right: 9px;
        padding-bottom: 11px;
        padding-left: 7px;
        background-color: #ffffff;
        border: 1px solid #838383;
        border-radius: 3px;
        box-shadow: 2px 2px 6px rgba(0, 0, 0, 0.3);
        color: #000000;
        font-size: 14px;
    }

    .florui-validation-bubble-icon {
        position: relative;
        width: 23px;
        height: 23px;
        margin-right: 9px;
        background-color: #ed5e01;
        border-radius: 2px;
    }

    .florui-validation-bubble-stem {
        position: absolute;
        left: 10px;
        top: 5px;
        width: 3px;
        height: 7px;
        background-color: #ffffff;
    }

    .florui-validation-bubble-dot {
        position: absolute;
        left: 10px;
        top: 14px;
        width: 3px;
        height: 4px;
        background-color: #ffffff;
    }

    .florui-validation-bubble-text {
        max-width: 317px;
        margin-top: 2px;
        margin-bottom: 2px;
    }

    .florui-validation-bubble-arrow {
        position: absolute;
        width: 9px;
        height: 9px;
        background-color: #ffffff;
        border-left: 1px solid #838383;
        border-top: 1px solid #838383;
        transform: rotate(45deg);
    }

    .florui-validation-bubble-arrow-below {
        position: absolute;
        width: 9px;
        height: 9px;
        background-color: #ffffff;
        border-right: 1px solid #838383;
        border-bottom: 1px solid #838383;
        transform: rotate(45deg);
    }

    input:not([type=\"checkbox\"]):not([type=\"radio\"]):not([type=\"range\"]):not([type=\"hidden\"]):enabled:hover,
    textarea:enabled:hover {
        border-color: #4f4f4f;
    }

    textarea {
        display: inline-block;
        border: 1px solid #767676;
        background-color: #ffffff;
        color: #000000;
        padding-top: 2px;
        padding-right: 2px;
        padding-bottom: 2px;
        padding-left: 2px;
        font-family: monospace;
        font-size: 13.3333px;
    }

    select {
        display: inline-block;
        border: 1px solid #767676;
        background-color: #ffffff;
        color: #000000;
        padding-left: 4px;
    }

    select[multiple=\"true\"] {
        padding-left: 0px;
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

/// Resets `--florui-scroll-behavior` on every element, because the real
/// property does not inherit and a custom property does. Only registered
/// when something declares the property: the universal rule costs the
/// cascade a few percent, which an app that never scrolls smoothly should
/// not pay.
pub(crate) fn scroll_behavior_reset_rule() -> Rule {
    static RULE: LazyLock<Rule> = LazyLock::new(|| {
        parse_stylesheet_with_origin("* { --florui-scroll-behavior: auto; }", Origin::UserAgent)
            .expect("a fixed, valid stylesheet")
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
    fn a_resolves_inline_with_zero_author_css() {
        let tree: Element = view! {
            <div>
                <a href="https://example.com" />
            </div>
        };
        let arena = Arena::build(&tree);
        let div = arena.roots()[0];
        let a = arena.children(div)[0];
        let rules = parse_stylesheet("").unwrap();
        let computed = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );

        assert_eq!(computed[&a].display, Display::Inline);
    }

    #[test]
    fn an_unvisited_link_gets_the_real_chromium_default_link_look_with_zero_author_css() {
        let tree: Element = view! {
            <div>
                <a href="https://example.com" />
            </div>
        };
        let arena = Arena::build(&tree);
        let div = arena.roots()[0];
        let a = arena.children(div)[0];
        let rules = parse_stylesheet("").unwrap();
        let computed = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );

        // Measured directly against a real headless Chromium -- see this
        // module's own doc.
        assert_eq!(
            computed[&a].color,
            crate::color::Rgba::opaque(0x00, 0x00, 0xee)
        );
        assert!(computed[&a].text_decoration_underline);
        assert!(computed[&a].cursor_pointer);
    }

    #[test]
    fn a_visited_link_gets_the_paired_default_color_with_zero_author_css() {
        let tree: Element = view! {
            <div>
                <a href="https://example.com" />
            </div>
        };
        let arena = Arena::build(&tree);
        let div = arena.roots()[0];
        let a = arena.children(div)[0];
        let rules = parse_stylesheet("").unwrap();
        let visited = std::collections::HashSet::from(["https://example.com".to_string()]);
        let state = InteractionState::new().with_visited(&visited);
        let computed = compute(
            &arena,
            &rules,
            &state,
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );

        assert_eq!(
            computed[&a].color,
            crate::color::Rgba::opaque(0x55, 0x1a, 0x8b)
        );
        assert!(computed[&a].text_decoration_underline);
        assert!(computed[&a].cursor_pointer);
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
    /// Real Chromium's own `getComputedStyle` on a `<select multiple>`
    /// reports `padding: 0` on every side (unlike the closed control's
    /// measured `padding-left: 4px` stand-in above) and renders no
    /// left-only inset — the multiple-mode override must actually zero it
    /// back out, not just leave the closed-control default in place.
    fn a_multiple_select_has_no_left_inset_unlike_the_closed_control() {
        let tree: Element = view! {
            <select multiple="true">
                <option>{"A"}</option>
            </select>
        };
        let arena = Arena::build(&tree);
        let select = arena.roots()[0];
        let rules = parse_stylesheet("").unwrap();
        let computed = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );

        let select_style = &computed[&select];
        assert_close(
            select_style.padding.left,
            0.0,
            "select[multiple] padding-left",
        );
        assert_close(
            select_style.padding.right,
            0.0,
            "select[multiple] padding-right",
        );
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

    /// Measured in Edge: a text control's border goes from `#767676` to
    /// `#4f4f4f` under the pointer; a checkbox, a disabled field, and an
    /// author-styled border are left alone.
    #[test]
    fn a_hovered_text_control_darkens_its_border_but_nothing_else_does() {
        let tree: Element = view! {
            <div>
                <input id="t" type="text" />
                <textarea id="a"></textarea>
                <input id="c" type="checkbox" />
                <input id="d" type="text" disabled="true" />
                <input id="m" type="text" style="border: 1px solid #ff0000;" />
            </div>
        };
        let arena = Arena::build(&tree);
        let rules = parse_stylesheet("").unwrap();
        let border_of = |id: &str, hovered: bool| {
            let node = arena.find(|a, n| a.id_attr(n) == Some(id)).unwrap();
            let state = if hovered {
                InteractionState::new().with_hovered(node)
            } else {
                InteractionState::new()
            };
            let computed = compute(
                &arena,
                &rules,
                &state,
                Viewport::default(),
                &mut crate::AnimationTimeline::default(),
            );
            computed[&node].border.top.color
        };
        let (idle, hover) = (
            crate::Rgba::opaque(118, 118, 118),
            crate::Rgba::opaque(79, 79, 79),
        );
        for id in ["t", "a"] {
            assert_eq!(border_of(id, false), idle, "{id} idle");
            assert_eq!(border_of(id, true), hover, "{id} hovered");
        }
        assert_eq!(
            border_of("c", true),
            border_of("c", false),
            "a checkbox is untouched"
        );
        assert_eq!(
            border_of("d", true),
            border_of("d", false),
            "a disabled field is untouched"
        );
        assert_eq!(
            border_of("m", true),
            crate::Rgba::opaque(255, 0, 0),
            "an author border wins"
        );
    }
}
