//! Parses CSS text via Stylo's real parser — real CSS's own error-recovery
//! model applies: a malformed declaration or rule is skipped, not
//! rejected, matching how a browser behaves, not the previous hand-rolled
//! parser's closed-subset rejection. Selectors and properties this crate
//! doesn't yet render (a child combinator, `!important`)
//! parse successfully; they simply have no visible effect until
//! `florui-layout`/`florui-paint` grow support for them.

use std::sync::{Arc, LazyLock, Mutex};

use florui::StylesheetSource;
use style::media_queries::MediaList;
use style::servo_arc::Arc as StyloArc;
use style::stylesheets::{AllowImportRules, Origin, Stylesheet};

use crate::container_query_adapter::{self, ContainerQueryBlock};
use crate::error::StyleError;
use crate::height_media_adapter::substitute_height_features;
use crate::reduced_motion_adapter::substitute_reduced_motion_feature;
use crate::scope_adapter::scope_class_selectors;
use crate::stylo::shared_lock;

/// Stylo gates `display: grid`/`inline-grid` and the `grid-*` longhands
/// behind a runtime pref, off by default in the `servo` build this crate
/// uses — forced on once, process-wide, before any CSS is parsed.
static GRID_ENABLED: LazyLock<()> =
    LazyLock::new(|| stylo_config::set_bool("layout.grid.enabled", true));

/// `backdrop-filter` shares this pref with several properties this crate
/// doesn't read (`contain`, `mask-image`, `text-overflow`, ...), so
/// enabling it has no other observable effect here.
static BACKDROP_FILTER_ENABLED: LazyLock<()> =
    LazyLock::new(|| stylo_config::set_bool("layout.unimplemented", true));

/// `container-type`/`container-name` (the plain properties, distinct from
/// the `@container` at-rule itself — see
/// [`crate::container_query_adapter`]'s own module doc for that gap) are
/// real Stylo properties in both engines, gated only by this runtime pref.
static CONTAINER_QUERIES_ENABLED: LazyLock<()> =
    LazyLock::new(|| stylo_config::set_bool("layout.container-queries.enabled", true));

/// How many distinct substituted-text results a [`RuleKind::Dynamic`] keeps
/// parsed at once. A resize only produces a new entry when it crosses a
/// `min-height`/`max-height` breakpoint or a container-query threshold, so
/// real usage rarely needs more than a handful; this just bounds the worst
/// case.
const DYNAMIC_CACHE_CAPACITY: usize = 8;

/// One parsed stylesheet. Opaque: `florui-style` is the only crate that
/// reads what's inside — everything else only holds, clones, and passes
/// this to [`crate::cascade::compute`]. Cloning is cheap and shares the
/// same underlying state (an [`Arc`]), including the cache below.
#[derive(Clone)]
pub struct Rule(Arc<RuleKind>, bool);

enum RuleKind {
    /// The common case: nothing in the authored CSS could possibly be a
    /// `height` media feature or a `@container` block, so this is the same
    /// parsed stylesheet every version of this crate has always produced,
    /// reused as-is regardless of viewport or container sizes.
    Static(StyloArc<Stylesheet>),
    /// The CSS mentions `height` inside an `@media` prelude (see
    /// [`crate::height_media_adapter`]) and/or contains one or more
    /// `@container` blocks (see [`crate::container_query_adapter`]). Real
    /// parsing is deferred to the first [`Rule::stylesheet`] call for a
    /// given viewport height/container-query signature, and cached by the
    /// resulting substituted text: two calls that resolve every dynamic
    /// condition the same way produce identical text, so they safely share
    /// one parse.
    Dynamic {
        css: String,
        origin: Origin,
        container_blocks: Vec<ContainerQueryBlock>,
        cache: Mutex<Vec<(String, StyloArc<Stylesheet>)>>,
    },
}

impl Rule {
    /// `container_query_signature[i]` is whether [`Self::container_query_blocks`]`()[i]`'s
    /// condition currently matches, for the single cascade this call is
    /// for — see [`crate::container_query_adapter`]'s own module doc. Must
    /// be exactly [`Self::container_query_blocks`]`().len()` long.
    pub(crate) fn stylesheet(
        &self,
        viewport_height: f32,
        container_query_signature: &[bool],
        prefers_reduced_motion: bool,
    ) -> StyloArc<Stylesheet> {
        match &*self.0 {
            RuleKind::Static(sheet) => sheet.clone(),
            RuleKind::Dynamic {
                css,
                origin,
                container_blocks,
                cache,
            } => {
                let after_height = substitute_height_features(css, viewport_height);
                let after_containers = container_query_adapter::literalize(
                    &after_height,
                    container_blocks,
                    container_query_signature,
                );
                let after_motion =
                    substitute_reduced_motion_feature(&after_containers, prefers_reduced_motion);
                let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(pos) = cache
                    .iter()
                    .position(|(key, _)| key == after_motion.as_ref())
                {
                    let (key, sheet) = cache.remove(pos);
                    cache.push((key, sheet.clone()));
                    return sheet;
                }
                let sheet = parse_str(&after_motion, *origin);
                if cache.len() >= DYNAMIC_CACHE_CAPACITY {
                    cache.remove(0);
                }
                cache.push((after_motion.into_owned(), sheet.clone()));
                sheet
            }
        }
    }

    /// Whether this rule's CSS declares `scroll-behavior`, which is what
    /// makes the user-agent reset of its custom property worth applying.
    pub(crate) fn uses_scroll_behavior(&self) -> bool {
        self.1
    }

    /// Every `@container` block this rule's own CSS contains, in source
    /// order — the order [`Self::stylesheet`]'s own `container_query_signature`
    /// must align to. Empty for a [`RuleKind::Static`] rule.
    pub(crate) fn container_query_blocks(&self) -> &[ContainerQueryBlock] {
        match &*self.0 {
            RuleKind::Static(_) => &[],
            RuleKind::Dynamic {
                container_blocks, ..
            } => container_blocks,
        }
    }

    /// Whether this rule's own CSS contains at least one `@container`
    /// block — `florui_layout::compute_with_style`'s own cheap early-out:
    /// a stylesheet with none of these costs nothing beyond what it
    /// already did before this feature existed.
    pub fn has_container_queries(&self) -> bool {
        !self.container_query_blocks().is_empty()
    }
}

/// Parses `css` as `Origin::Author` — every application/component
/// stylesheet, always. [`crate::default_stylesheet`] is the only other
/// caller of CSS parsing in this crate, and it needs a different origin
/// (`Origin::UserAgent`), which is why the actual parsing logic lives in
/// [`parse_stylesheet_with_origin`] instead of being inlined here.
pub fn parse_stylesheet(css: &str) -> Result<Vec<Rule>, StyleError> {
    parse_stylesheet_with_origin(css, Origin::Author).map(|rule| vec![rule])
}

/// Compiles a whole application's collected `StylesheetSource`s (typically
/// `FLORUI_STYLESHEETS`, in their already-cascade-ordered sequence) into
/// one `Rule` per source. A source with `scope: Some(_)` (from
/// `stylesheet_scoped!`) has [`scope_class_selectors`] applied to its CSS
/// text *before* parsing — a one-time, compile-time-fixed rewrite, unlike
/// `RuleKind::Dynamic`'s own per-viewport/per-container re-substitution —
/// so its class selectors carry the same scope suffix `view!`'s
/// `scope={...}` directive already applied to the matching elements.
/// A source with `scope: None` is parsed exactly as [`parse_stylesheet`]
/// always has. One `Rule` per source (not a concatenated single string)
/// keeps every source's own scope boundary intact for this rewrite.
pub fn compile_sources(sources: &[StylesheetSource]) -> Result<Vec<Rule>, StyleError> {
    sources
        .iter()
        .map(|source| {
            let css = match source.scope {
                Some(scope) => scope_class_selectors(source.css, scope),
                None => std::borrow::Cow::Borrowed(source.css),
            };
            parse_stylesheet_with_origin(&css, Origin::Author)
        })
        .collect()
}

pub(crate) fn parse_stylesheet_with_origin(css: &str, origin: Origin) -> Result<Rule, StyleError> {
    LazyLock::force(&GRID_ENABLED);
    LazyLock::force(&BACKDROP_FILTER_ENABLED);
    LazyLock::force(&CONTAINER_QUERIES_ENABLED);
    let css = if origin == Origin::Author {
        crate::scroll_behavior_adapter::rewrite_stylesheet(css)
    } else {
        std::borrow::Cow::Borrowed(css)
    };
    let uses_scroll_behavior = matches!(css, std::borrow::Cow::Owned(_));
    let css = css.as_ref();
    let container_blocks = container_query_adapter::extract_container_queries(css);
    let lower_css = css.to_ascii_lowercase();
    if !container_blocks.is_empty()
        || lower_css.contains("height")
        || lower_css.contains("prefers-reduced-motion")
    {
        return Ok(Rule(
            Arc::new(RuleKind::Dynamic {
                css: css.to_string(),
                origin,
                container_blocks,
                cache: Mutex::new(Vec::new()),
            }),
            uses_scroll_behavior,
        ));
    }
    Ok(Rule(
        Arc::new(RuleKind::Static(parse_str(css, origin))),
        uses_scroll_behavior,
    ))
}

fn parse_str(css: &str, origin: Origin) -> StyloArc<Stylesheet> {
    let lock = shared_lock();
    let url = url::Url::parse("about:florui").expect("a fixed, valid URL literal");
    let sheet = Stylesheet::from_str(
        css,
        url.into(),
        origin,
        StyloArc::new(lock.wrap(MediaList::empty())),
        lock.clone(),
        None,
        None,
        style::context::QuirksMode::NoQuirks,
        AllowImportRules::No,
    );
    StyloArc::new(sheet)
}

#[cfg(test)]
mod tests {
    use florui::prelude::*;

    use super::*;
    use crate::cascade::{Viewport, compute};
    use crate::color::Rgba;
    use crate::interaction::InteractionState;
    use crate::tree::Arena;

    #[test]
    fn a_class_selector_resolves_a_property() {
        let tree: Element = view! { <div class="card" /> };
        let arena = Arena::build(&tree);
        let rules = parse_stylesheet(".card { background-color: #42734f; }").unwrap();
        let computed = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );
        assert_eq!(
            computed[&arena.roots()[0]].background_color,
            Rgba::opaque(0x42, 0x73, 0x4f)
        );
    }

    #[test]
    fn a_stylesheet_can_be_parsed_and_reused_across_multiple_computes() {
        let tree: Element = view! { <div class="card" /> };
        let arena = Arena::build(&tree);
        let rules = parse_stylesheet(".card { background-color: #111111; }").unwrap();
        let first = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );
        let second = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );
        assert_eq!(
            first[&arena.roots()[0]].background_color,
            second[&arena.roots()[0]].background_color
        );
    }

    #[test]
    fn a_property_florui_does_not_yet_render_still_parses_successfully() {
        // Real CSS: an unrendered property is not a parse error.
        assert!(parse_stylesheet(".a { border: 1px solid black; }").is_ok());
    }

    #[test]
    fn malformed_css_does_not_error_the_whole_stylesheet() {
        // Real CSS's own error-recovery model: an unclosed block or a
        // stray declaration is dropped, not rejected outright.
        assert!(parse_stylesheet(".a { color: #fff;").is_ok());
    }

    /// The spec's literal acceptance test for explicit style scoping: two
    /// components using the same local class name (`.box`) must not
    /// collide — proven end to end here through the real Stylo cascade,
    /// not just the text-rewrite unit tests in `scope_adapter`.
    #[test]
    fn two_scoped_stylesheets_with_the_same_local_class_resolve_independently() {
        use florui::StyleScope;

        let card_scope = StyleScope::new("pkg:card.rs:./card.css");
        let widget_scope = StyleScope::new("pkg:widget.rs:./widget.css");
        let sources = [
            StylesheetSource {
                id: "pkg:card.rs:./card.css",
                source_path: "./card.css",
                css: ".box { background-color: #ff0000; }",
                scope: Some(card_scope),
            },
            StylesheetSource {
                id: "pkg:widget.rs:./widget.css",
                source_path: "./widget.css",
                css: ".box { background-color: #0000ff; }",
                scope: Some(widget_scope),
            },
        ];
        let rules = compile_sources(&sources).unwrap();

        let card_class = format!("box{}", card_scope.suffix());
        let widget_class = format!("box{}", widget_scope.suffix());
        let tree: florui::Element = florui::prelude::view! {
            <div>
                <div class={card_class}></div>
                <div class={widget_class}></div>
            </div>
        };
        let arena = crate::tree::Arena::build(&tree);
        let computed = crate::cascade::compute(
            &arena,
            &rules,
            &crate::interaction::InteractionState::new(),
            crate::cascade::Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );

        let roots = arena.roots();
        let container = roots[0];
        let children = arena.children(container);
        assert_eq!(
            computed[&children[0]].background_color,
            crate::color::Rgba::opaque(0xff, 0x00, 0x00)
        );
        assert_eq!(
            computed[&children[1]].background_color,
            crate::color::Rgba::opaque(0x00, 0x00, 0xff)
        );
    }
}
