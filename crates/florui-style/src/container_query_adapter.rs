//! Stylo's CSS parser never recognizes the `@container` at-rule itself
//! outside a `gecko` build (`"container" if cfg!(feature = "gecko")` in its
//! own rule parser) -- unlike [`crate::height_media_adapter`]'s gap (one
//! missing `@media` *feature*), this is the whole at-rule, silently dropped
//! by Stylo's own real-CSS forward-compatible "unknown at-rule" recovery.
//! `container-type`/`container-name` (the plain properties a container
//! declares itself with) are unaffected -- those are real Stylo properties,
//! gated only by a runtime pref this crate already flips (see
//! [`crate::stylesheet_parse`]'s `CONTAINER_QUERIES_ENABLED`).
//!
//! This rewrites each `@container [<name>]? (<condition>) { ... }` block's
//! own prelude, whole, into `@media (width)` (always true on a real screen)
//! or `@media (not (width))` (always false) before Stylo ever parses the
//! text -- the same "collapse an unsupported test to an equivalent
//! supported one" trick `height_media_adapter` already uses, just applied
//! to the entire at-rule instead of one feature inside an already-supported
//! one, since `@container`'s condition grammar is otherwise identical to
//! `@media`'s (both share Stylo's own `<size-feature>` query grammar).
//!
//! The truth value is resolved per node, not once per document like a
//! media query's viewport: [`resolve_container_query_signatures`] walks
//! each node's ancestors to find its nearest matching container (mirroring
//! Stylo's own `container_rule.rs` traversal, which isn't reachable from
//! outside the crate) and evaluates the condition against that container's
//! real, already-laid-out size. [`crate::stylo::compute`]'s caller is
//! responsible for grouping nodes by their resulting signature and running
//! one cascade per distinct signature -- see `florui_layout::compute_with_style`.
//!
//! Supported syntax: `width`/`height`/`inline-size`/`block-size`/
//! `aspect-ratio`/`orientation`, `min-`/`max-`/plain colon-comparison forms
//! only, composed with `and`/`or`/`not` -- the classic grammar, matching
//! `height_media_adapter`'s own "classic colon form only" scope. The newer
//! range syntax (`(width < 400px)`), style queries, a `@container` nested
//! inside `@media`, and a container-name query naming more than one
//! identifier are not recognized and silently drop that whole block (real
//! CSS's own unknown-syntax recovery), the same as an unsupported form
//! reaching Stylo directly would.

use crate::style_map::StyleMap;
use std::borrow::Cow;
use std::collections::HashMap;

use cssparser::{Delimiter, Parser, ParserInput, Token};

use crate::cascade::ContainerType;
use crate::stylesheet_parse::Rule;
use crate::tree::{Arena, NodeId};

/// A container's own content-box size, at whatever point it was last laid
/// out -- the only size real CSS container-query lengths ever resolve
/// against. Provided by the caller (`florui-layout`'s own [`BoxLayout`]'s
/// `width`/`height` are already content-box, see that crate's own module
/// doc) since `florui-style` cannot depend on `florui-layout` without a
/// dependency cycle (`florui-layout` already depends on this crate).
///
/// [`BoxLayout`]: ../florui_layout/struct.BoxLayout.html
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContentBoxSize {
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Feature {
    Width,
    Height,
    InlineSize,
    BlockSize,
    AspectRatio,
    Orientation,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Comparison {
    Min,
    Max,
    Exact,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum OrientationKeyword {
    Portrait,
    Landscape,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum FeatureValue {
    Length(f32),
    Ratio(f32, f32),
    Orientation(OrientationKeyword),
}

#[derive(Debug, Clone, PartialEq)]
enum ConditionNode {
    Feature {
        feature: Feature,
        comparison: Comparison,
        value: FeatureValue,
    },
    Not(Box<ConditionNode>),
    And(Vec<ConditionNode>),
    Or(Vec<ConditionNode>),
}

/// One extracted `@container` block -- see the module doc. Opaque outside
/// this crate, same as [`Rule`] itself.
#[derive(Debug, Clone)]
pub(crate) struct ContainerQueryBlock {
    /// `@container <name> (...)` -- at most one identifier is recognized;
    /// real CSS's own space-separated multi-name query filter is not.
    name: Option<String>,
    condition: ConditionNode,
    /// Byte span of the whole `@container [name]? (condition)` prelude,
    /// from the `@container` keyword through (not including) the `{`.
    span: (usize, usize),
    needs_width: bool,
    needs_height: bool,
}

/// See the module doc. Returns an empty list, unallocated, when `css`
/// contains no "container" text at all -- the same cheap early-out
/// `height_media_adapter` uses (a real class named e.g. `.container` still
/// costs one avoidable tokenizer pass on a false positive, tolerated the
/// same way there).
pub(crate) fn extract_container_queries(css: &str) -> Vec<ContainerQueryBlock> {
    if !css.to_ascii_lowercase().contains("container") {
        return Vec::new();
    }

    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    let mut blocks = Vec::new();

    loop {
        let start = parser.position().byte_index();
        let token = match parser.next_including_whitespace() {
            Ok(token) => token.clone(),
            Err(_) => break,
        };
        if let Token::AtKeyword(name) = &token
            && name.eq_ignore_ascii_case("container")
        {
            let parsed: Result<
                Option<(Option<String>, ConditionNode)>,
                cssparser::ParseError<'_, ()>,
            > = parser.parse_until_before(Delimiter::CurlyBracketBlock, |prelude| {
                Ok(parse_container_prelude(prelude))
            });
            let end = parser.position().byte_index();
            if let Ok(Some((name, condition))) = parsed {
                let (needs_width, needs_height) = required_axes(&condition);
                blocks.push(ContainerQueryBlock {
                    name,
                    condition,
                    span: (start, end),
                    needs_width,
                    needs_height,
                });
            }
        }
        // A block not explicitly entered above is auto-skipped whole by the
        // next call, same as `height_media_adapter`'s own top-level loop.
    }

    blocks
}

/// See the module doc. `signature[i]` is whether `blocks[i]`'s condition
/// currently matches, for whatever single cascade this call is for --
/// `blocks`/`signature` must be the same length, in the same order
/// [`extract_container_queries`] produced.
pub(crate) fn literalize<'a>(
    css: &'a str,
    blocks: &[ContainerQueryBlock],
    signature: &[bool],
) -> Cow<'a, str> {
    if blocks.is_empty() {
        return Cow::Borrowed(css);
    }
    debug_assert_eq!(blocks.len(), signature.len());

    let mut out = String::with_capacity(css.len());
    let mut cursor = 0;
    for (block, &matched) in blocks.iter().zip(signature) {
        let (start, end) = block.span;
        out.push_str(&css[cursor..start]);
        // The trailing space is deliberate: `parse_until_before`'s own end
        // position lands exactly at `{`, having already consumed any
        // whitespace between the original prelude and it — the
        // replacement supplies its own so `@media (width){` doesn't
        // collide into one token run (harmless to real CSS either way,
        // but keeps the output legible).
        out.push_str(if matched {
            "@media (width) "
        } else {
            "@media (not (width)) "
        });
        cursor = end;
    }
    out.push_str(&css[cursor..]);
    Cow::Owned(out)
}

/// Every node's own signature -- one bool per block extracted across
/// `rules`, in that flattened order -- for the caller to group nodes by
/// and drive one [`crate::stylo::compute`] cascade per distinct signature.
/// `styles`/`sizes` are `arena`'s own most recent base-pass results (see
/// `florui_layout::compute_with_style`'s own doc): every node not present
/// in `styles` is skipped (never produced by a base pass — e.g. `display:
/// none`, out of scope here the same way it is everywhere else).
pub fn resolve_container_query_signatures(
    arena: &Arena,
    rules: &[Rule],
    styles: &StyleMap,
    sizes: &HashMap<NodeId, ContentBoxSize>,
) -> HashMap<NodeId, Vec<bool>> {
    let blocks: Vec<&ContainerQueryBlock> = rules
        .iter()
        .flat_map(|rule| rule.container_query_blocks().iter())
        .collect();

    let mut result = HashMap::with_capacity(styles.len());
    for &id in styles.keys() {
        let signature = blocks
            .iter()
            .map(|block| {
                find_container(arena, styles, sizes, id, block)
                    .map(|size| evaluate(&block.condition, size))
                    .unwrap_or(false)
            })
            .collect();
        result.insert(id, signature);
    }
    result
}

/// Walks upward from (not including) `start` for the nearest ancestor whose
/// own `container-type` provides every axis `block`'s condition needs, and
/// whose `container-name` includes `block`'s own name filter if it has one
/// -- mirrors Stylo's own `container_rule.rs::traverse_container` (not
/// reachable from outside the crate) rather than using a plain parent's
/// size as a stand-in, matching this crate's own real-containment
/// requirement.
fn find_container(
    arena: &Arena,
    styles: &StyleMap,
    sizes: &HashMap<NodeId, ContentBoxSize>,
    start: NodeId,
    block: &ContainerQueryBlock,
) -> Option<ContentBoxSize> {
    let mut current = arena.parent(start);
    while let Some(id) = current {
        if let Some(style) = styles.get(&id) {
            let (provides_width, provides_height) = match style.container_type {
                ContainerType::Normal => (false, false),
                ContainerType::InlineSize => (true, false),
                ContainerType::Size => (true, true),
            };
            let axes_ok =
                (!block.needs_width || provides_width) && (!block.needs_height || provides_height);
            let name_ok = block
                .name
                .as_ref()
                .is_none_or(|name| style.container_name.iter().any(|n| n == name));
            if axes_ok && name_ok {
                return sizes.get(&id).copied();
            }
        }
        current = arena.parent(id);
    }
    None
}

fn required_axes(condition: &ConditionNode) -> (bool, bool) {
    match condition {
        ConditionNode::Feature { feature, .. } => match feature {
            Feature::Width | Feature::InlineSize => (true, false),
            Feature::Height | Feature::BlockSize => (false, true),
            Feature::AspectRatio | Feature::Orientation => (true, true),
        },
        ConditionNode::Not(inner) => required_axes(inner),
        ConditionNode::And(operands) | ConditionNode::Or(operands) => {
            operands.iter().fold((false, false), |(w, h), operand| {
                let (ow, oh) = required_axes(operand);
                (w || ow, h || oh)
            })
        }
    }
}

fn evaluate(node: &ConditionNode, size: ContentBoxSize) -> bool {
    match node {
        ConditionNode::Not(inner) => !evaluate(inner, size),
        ConditionNode::And(operands) => operands.iter().all(|o| evaluate(o, size)),
        ConditionNode::Or(operands) => operands.iter().any(|o| evaluate(o, size)),
        ConditionNode::Feature {
            feature,
            comparison,
            value,
        } => match (feature, value) {
            (Feature::Width, FeatureValue::Length(px)) => compare(size.width, *comparison, *px),
            (Feature::Height, FeatureValue::Length(px)) => compare(size.height, *comparison, *px),
            (Feature::InlineSize, FeatureValue::Length(px)) => {
                compare(size.width, *comparison, *px)
            }
            (Feature::BlockSize, FeatureValue::Length(px)) => {
                compare(size.height, *comparison, *px)
            }
            (Feature::AspectRatio, FeatureValue::Ratio(num, den)) => {
                if size.height <= 0.0 || *den <= 0.0 {
                    return false;
                }
                compare(size.width / size.height, *comparison, num / den)
            }
            (Feature::Orientation, FeatureValue::Orientation(keyword)) => {
                let is_portrait = size.height >= size.width;
                match keyword {
                    OrientationKeyword::Portrait => is_portrait,
                    OrientationKeyword::Landscape => !is_portrait,
                }
            }
            // The parser never pairs a feature with a value of the wrong
            // shape -- `parse_size_feature` picks the value parser from
            // the same match on `feature` this reads back.
            _ => false,
        },
    }
}

fn compare(actual: f32, comparison: Comparison, threshold: f32) -> bool {
    match comparison {
        Comparison::Min => actual >= threshold,
        Comparison::Max => actual <= threshold,
        Comparison::Exact => (actual - threshold).abs() < 0.01,
    }
}

/// `[<container-name>]? <container-query>` -- see the module doc for the
/// supported subset. `None` for anything this can't confidently parse,
/// left for Stylo's own real-CSS unknown-syntax recovery to drop whole
/// rather than guess at.
fn parse_container_prelude<'i>(
    parser: &mut Parser<'i, '_>,
) -> Option<(Option<String>, ConditionNode)> {
    // `not`/`and`/`or` are the condition grammar's own keywords, not valid
    // names, in this position -- `<container-name> (min-width: ...)` has
    // no other way to tell "name" from "the condition already started".
    let name = parser
        .try_parse(|p| {
            let ident = p.expect_ident()?.clone();
            let lower = ident.to_ascii_lowercase();
            if lower == "not" || lower == "and" || lower == "or" {
                return Err(p
                    .new_basic_unexpected_token_error(Token::Ident(ident))
                    .into());
            }
            Ok::<_, cssparser::ParseError<'_, ()>>(ident.to_string())
        })
        .ok();
    let condition = parse_container_query(parser).ok()?;
    Some((name, condition))
}

/// `<container-query> = not <query-in-parens> | <query-in-parens> [ [ and
/// <query-in-parens> ]* | [ or <query-in-parens> ]* ]` -- real CSS's own
/// grammar, shared verbatim with `@media`'s `<media-condition>`.
fn parse_container_query<'i>(
    parser: &mut Parser<'i, '_>,
) -> Result<ConditionNode, cssparser::ParseError<'i, ()>> {
    if parser.try_parse(|p| p.expect_ident_matching("not")).is_ok() {
        let operand = parse_query_in_parens(parser)?;
        return Ok(ConditionNode::Not(Box::new(operand)));
    }

    let first = parse_query_in_parens(parser)?;
    if parser.try_parse(|p| p.expect_ident_matching("and")).is_ok() {
        let mut operands = vec![first, parse_query_in_parens(parser)?];
        while parser.try_parse(|p| p.expect_ident_matching("and")).is_ok() {
            operands.push(parse_query_in_parens(parser)?);
        }
        return Ok(ConditionNode::And(operands));
    }
    if parser.try_parse(|p| p.expect_ident_matching("or")).is_ok() {
        let mut operands = vec![first, parse_query_in_parens(parser)?];
        while parser.try_parse(|p| p.expect_ident_matching("or")).is_ok() {
            operands.push(parse_query_in_parens(parser)?);
        }
        return Ok(ConditionNode::Or(operands));
    }
    Ok(first)
}

/// `<query-in-parens> = ( <container-query> ) | ( <size-feature> )` --
/// tries a leaf `<size-feature>` first, falling back to a nested condition
/// when that doesn't fully consume the group, same fallback strategy
/// `height_media_adapter::scan_prelude` already uses.
fn parse_query_in_parens<'i>(
    parser: &mut Parser<'i, '_>,
) -> Result<ConditionNode, cssparser::ParseError<'i, ()>> {
    parser.expect_parenthesis_block()?;
    parser.parse_nested_block(|inner| {
        let leaf = inner.try_parse(parse_size_feature);
        match leaf {
            Ok(feature) if inner.expect_exhausted().is_ok() => Ok(feature),
            _ => parse_container_query(inner),
        }
    })
}

/// `<ident> : <value>` -- see the module doc for exactly which idents and
/// value shapes are recognized.
fn parse_size_feature<'i>(
    parser: &mut Parser<'i, '_>,
) -> Result<ConditionNode, cssparser::ParseError<'i, ()>> {
    let ident = parser.expect_ident()?.clone();
    let lower = ident.to_ascii_lowercase();
    let (feature, comparison) = match lower.as_str() {
        "width" => (Feature::Width, Comparison::Exact),
        "min-width" => (Feature::Width, Comparison::Min),
        "max-width" => (Feature::Width, Comparison::Max),
        "height" => (Feature::Height, Comparison::Exact),
        "min-height" => (Feature::Height, Comparison::Min),
        "max-height" => (Feature::Height, Comparison::Max),
        "inline-size" => (Feature::InlineSize, Comparison::Exact),
        "min-inline-size" => (Feature::InlineSize, Comparison::Min),
        "max-inline-size" => (Feature::InlineSize, Comparison::Max),
        "block-size" => (Feature::BlockSize, Comparison::Exact),
        "min-block-size" => (Feature::BlockSize, Comparison::Min),
        "max-block-size" => (Feature::BlockSize, Comparison::Max),
        "aspect-ratio" => (Feature::AspectRatio, Comparison::Exact),
        "min-aspect-ratio" => (Feature::AspectRatio, Comparison::Min),
        "max-aspect-ratio" => (Feature::AspectRatio, Comparison::Max),
        "orientation" => (Feature::Orientation, Comparison::Exact),
        _ => {
            return Err(parser
                .new_basic_unexpected_token_error(Token::Ident(ident))
                .into());
        }
    };
    parser.expect_colon()?;
    let value = match feature {
        Feature::AspectRatio => {
            let (num, den) = parse_ratio(parser)?;
            FeatureValue::Ratio(num, den)
        }
        Feature::Orientation => FeatureValue::Orientation(parse_orientation(parser)?),
        _ => FeatureValue::Length(parse_px_length(parser)?),
    };
    Ok(ConditionNode::Feature {
        feature,
        comparison,
        value,
    })
}

fn parse_px_length<'i>(parser: &mut Parser<'i, '_>) -> Result<f32, cssparser::ParseError<'i, ()>> {
    let token = parser.next()?.clone();
    match &token {
        Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("px") => Ok(*value),
        Token::Number { value, .. } if *value == 0.0 => Ok(0.0),
        _ => Err(parser.new_unexpected_token_error(token)),
    }
}

/// `<ratio> = <number> [ / <number> ]?` -- a bare number implies `/1`, the
/// same as real CSS.
fn parse_ratio<'i>(
    parser: &mut Parser<'i, '_>,
) -> Result<(f32, f32), cssparser::ParseError<'i, ()>> {
    let numerator = parser.expect_number()?;
    let denominator = parser
        .try_parse(|p| {
            p.expect_delim('/')?;
            p.expect_number()
        })
        .unwrap_or(1.0);
    Ok((numerator, denominator))
}

fn parse_orientation<'i>(
    parser: &mut Parser<'i, '_>,
) -> Result<OrientationKeyword, cssparser::ParseError<'i, ()>> {
    let ident = parser.expect_ident()?.clone();
    match_ignore_ascii_case(&ident).ok_or_else(|| {
        parser
            .new_basic_unexpected_token_error(Token::Ident(ident))
            .into()
    })
}

fn match_ignore_ascii_case(ident: &str) -> Option<OrientationKeyword> {
    if ident.eq_ignore_ascii_case("portrait") {
        Some(OrientationKeyword::Portrait)
    } else if ident.eq_ignore_ascii_case("landscape") {
        Some(OrientationKeyword::Landscape)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stylesheet_with_no_container_text_at_all_extracts_nothing() {
        let css = ".card { background-color: #ff0000; }";
        assert!(extract_container_queries(css).is_empty());
    }

    #[test]
    fn extracts_a_single_min_width_block_with_no_name() {
        let css = "@container (min-width: 400px) { .card { color: red; } }";
        let blocks = extract_container_queries(css);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].name, None);
        assert!(blocks[0].needs_width);
        assert!(!blocks[0].needs_height);
    }

    #[test]
    fn extracts_a_named_container_query() {
        let css = "@container sidebar (min-width: 400px) { .card {} }";
        let blocks = extract_container_queries(css);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].name.as_deref(), Some("sidebar"));
    }

    #[test]
    fn literalize_rewrites_a_matched_block_to_an_always_true_media_query() {
        let css = "@container (min-width: 400px) { .card { color: red; } }";
        let blocks = extract_container_queries(css);
        let result = literalize(css, &blocks, &[true]);
        assert_eq!(&*result, "@media (width) { .card { color: red; } }");
    }

    #[test]
    fn literalize_rewrites_an_unmatched_block_to_an_always_false_media_query() {
        let css = "@container (min-width: 400px) { .card { color: red; } }";
        let blocks = extract_container_queries(css);
        let result = literalize(css, &blocks, &[false]);
        assert_eq!(&*result, "@media (not (width)) { .card { color: red; } }");
    }

    #[test]
    fn literalize_preserves_a_name_filter_by_dropping_it_along_with_the_rest_of_the_prelude() {
        let css = "@container sidebar (min-width: 400px) { .a {} }";
        let blocks = extract_container_queries(css);
        let result = literalize(css, &blocks, &[true]);
        assert_eq!(&*result, "@media (width) { .a {} }");
    }

    #[test]
    fn a_width_only_query_needs_only_the_width_axis() {
        let blocks = extract_container_queries("@container (min-width: 1px) {}");
        assert!(blocks[0].needs_width);
        assert!(!blocks[0].needs_height);
    }

    #[test]
    fn an_aspect_ratio_query_needs_both_axes() {
        let blocks = extract_container_queries("@container (min-aspect-ratio: 16/9) {}");
        assert!(blocks[0].needs_width);
        assert!(blocks[0].needs_height);
    }

    #[test]
    fn and_or_not_composition_extracts_correctly() {
        let css = "@container not ((min-width: 300px) and (max-width: 900px)) { .a {} }";
        let blocks = extract_container_queries(css);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0].condition, ConditionNode::Not(_)));
    }

    #[test]
    fn evaluate_min_width_matches_at_and_above_the_threshold() {
        let blocks = extract_container_queries("@container (min-width: 400px) {}");
        let at = ContentBoxSize {
            width: 400.0,
            height: 0.0,
        };
        let below = ContentBoxSize {
            width: 399.0,
            height: 0.0,
        };
        assert!(evaluate(&blocks[0].condition, at));
        assert!(!evaluate(&blocks[0].condition, below));
    }

    #[test]
    fn evaluate_and_requires_every_operand() {
        let blocks =
            extract_container_queries("@container (min-width: 300px) and (max-width: 500px) {}");
        assert!(evaluate(
            &blocks[0].condition,
            ContentBoxSize {
                width: 400.0,
                height: 0.0
            }
        ));
        assert!(!evaluate(
            &blocks[0].condition,
            ContentBoxSize {
                width: 600.0,
                height: 0.0
            }
        ));
    }

    #[test]
    fn evaluate_orientation_compares_width_and_height() {
        let blocks = extract_container_queries("@container (orientation: portrait) {}");
        assert!(evaluate(
            &blocks[0].condition,
            ContentBoxSize {
                width: 100.0,
                height: 200.0
            }
        ));
        assert!(!evaluate(
            &blocks[0].condition,
            ContentBoxSize {
                width: 200.0,
                height: 100.0
            }
        ));
    }

    #[test]
    fn multiple_container_blocks_each_extract_independently() {
        let css = "@container (min-width: 100px) { .a {} } @container (min-width: 900px) { .b {} }";
        let blocks = extract_container_queries(css);
        assert_eq!(blocks.len(), 2);
        let result = literalize(css, &blocks, &[true, false]);
        assert_eq!(
            &*result,
            "@media (width) { .a {} } @media (not (width)) { .b {} }"
        );
    }

    #[test]
    fn not_wrapping_an_and_group_is_not_mistaken_for_a_container_name() {
        let css = "@container not ((min-width: 300px) and (max-width: 900px)) { .a {} }";
        let blocks = extract_container_queries(css);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].name, None, "`not` must not be parsed as a name");
        assert!(matches!(blocks[0].condition, ConditionNode::Not(_)));
    }

    #[test]
    fn a_mention_inside_a_declaration_value_is_not_touched() {
        let css = ".card { --label: \"container query\"; }";
        let result = literalize(css, &extract_container_queries(css), &[]);
        assert_eq!(&*result, css);
    }

    mod resolution {
        use florui::prelude::*;

        use super::*;
        use crate::cascade::{Viewport, compute, compute_with_container_query_signature};
        use crate::interaction::InteractionState;
        use crate::stylesheet_parse::parse_stylesheet;

        #[test]
        fn nested_containers_use_the_nearest_matching_ancestor_not_a_farther_one() {
            let tree: Element = view! {
                <div class="outer">
                    <div class="inner">
                        <div class="card" />
                    </div>
                </div>
            };
            let css = "
                .outer { container-type: inline-size; }
                .inner { container-type: inline-size; }
                @container (min-width: 400px) { .card { background-color: #ff0000; } }
            ";
            let arena = Arena::build(&tree);
            let rules = parse_stylesheet(css).unwrap();
            let mut timeline = crate::AnimationTimeline::default();
            let base_styles = compute(
                &arena,
                &rules,
                &InteractionState::new(),
                Viewport::default(),
                &mut timeline,
            );

            let outer = arena.roots()[0];
            let inner = arena.children(outer)[0];
            let card = arena.children(inner)[0];

            // The nearer container (inner, 500px) is wide enough; the
            // farther one (outer) is deliberately too narrow -- real
            // containment must pick the nearer one, not an arbitrary
            // ancestor's size.
            let mut sizes = HashMap::new();
            sizes.insert(
                outer,
                ContentBoxSize {
                    width: 300.0,
                    height: 0.0,
                },
            );
            sizes.insert(
                inner,
                ContentBoxSize {
                    width: 500.0,
                    height: 0.0,
                },
            );

            let signatures =
                resolve_container_query_signatures(&arena, &rules, &base_styles, &sizes);
            assert_eq!(signatures[&card], vec![true]);

            let final_styles = compute_with_container_query_signature(
                &arena,
                &rules,
                &InteractionState::new(),
                Viewport::default(),
                &mut timeline,
                &signatures[&card],
            );
            assert_eq!(
                final_styles[&card].background_color,
                crate::color::Rgba::opaque(0xff, 0, 0)
            );
        }

        #[test]
        fn an_element_with_no_own_container_type_is_skipped_in_the_ancestor_search() {
            let tree: Element = view! {
                <div class="outer">
                    <div class="plain">
                        <div class="card" />
                    </div>
                </div>
            };
            let css = "
                .outer { container-type: inline-size; }
                @container (min-width: 400px) { .card { background-color: #ff0000; } }
            ";
            let arena = Arena::build(&tree);
            let rules = parse_stylesheet(css).unwrap();
            let mut timeline = crate::AnimationTimeline::default();
            let base_styles = compute(
                &arena,
                &rules,
                &InteractionState::new(),
                Viewport::default(),
                &mut timeline,
            );

            let outer = arena.roots()[0];
            let card = arena.children(arena.children(outer)[0])[0];

            let mut sizes = HashMap::new();
            sizes.insert(
                outer,
                ContentBoxSize {
                    width: 500.0,
                    height: 0.0,
                },
            );

            let signatures =
                resolve_container_query_signatures(&arena, &rules, &base_styles, &sizes);
            assert_eq!(
                signatures[&card],
                vec![true],
                "`.plain` establishes no containment, so the search must reach past it to `.outer`"
            );
        }

        #[test]
        fn a_container_name_filter_skips_a_nearer_container_with_the_wrong_name() {
            let tree: Element = view! {
                <div class="named">
                    <div class="unnamed">
                        <div class="card" />
                    </div>
                </div>
            };
            let css = "
                .named { container-type: inline-size; container-name: sidebar; }
                .unnamed { container-type: inline-size; }
                @container sidebar (min-width: 400px) { .card { background-color: #ff0000; } }
            ";
            let arena = Arena::build(&tree);
            let rules = parse_stylesheet(css).unwrap();
            let mut timeline = crate::AnimationTimeline::default();
            let base_styles = compute(
                &arena,
                &rules,
                &InteractionState::new(),
                Viewport::default(),
                &mut timeline,
            );

            let named = arena.roots()[0];
            let unnamed = arena.children(named)[0];
            let card = arena.children(unnamed)[0];

            // The nearer container (`unnamed`) is wide enough but doesn't
            // carry the `sidebar` name the query filters on; only the
            // farther, correctly-named one should count.
            let mut sizes = HashMap::new();
            sizes.insert(
                named,
                ContentBoxSize {
                    width: 500.0,
                    height: 0.0,
                },
            );
            sizes.insert(
                unnamed,
                ContentBoxSize {
                    width: 500.0,
                    height: 0.0,
                },
            );

            let signatures =
                resolve_container_query_signatures(&arena, &rules, &base_styles, &sizes);
            assert_eq!(signatures[&card], vec![true]);
        }

        #[test]
        fn a_container_name_filter_never_matches_when_no_ancestor_carries_it() {
            let tree: Element = view! {
                <div class="outer">
                    <div class="card" />
                </div>
            };
            let css = "
                .outer { container-type: inline-size; }
                @container sidebar (min-width: 100px) { .card { background-color: #ff0000; } }
            ";
            let arena = Arena::build(&tree);
            let rules = parse_stylesheet(css).unwrap();
            let mut timeline = crate::AnimationTimeline::default();
            let base_styles = compute(
                &arena,
                &rules,
                &InteractionState::new(),
                Viewport::default(),
                &mut timeline,
            );

            let outer = arena.roots()[0];
            let card = arena.children(outer)[0];
            let mut sizes = HashMap::new();
            sizes.insert(
                outer,
                ContentBoxSize {
                    width: 500.0,
                    height: 0.0,
                },
            );

            let signatures =
                resolve_container_query_signatures(&arena, &rules, &base_styles, &sizes);
            assert_eq!(signatures[&card], vec![false]);
        }
    }
}
