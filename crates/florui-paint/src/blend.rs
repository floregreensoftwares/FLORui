//! `mix-blend-mode`: blending a group with what is behind it, and which
//! stacking contexts must be groups for that to mean the right thing.
//!
//! A group is blended by the compositor of the surface it lands on. That
//! surface is the backdrop only if it is the nearest stacking context's: a
//! stacking context that is not painted as a group of its own (a positioned box
//! with a `z-index`, say) would let a blend inside it reach what is behind it.
//! So such a box that holds a blended descendant is painted as a group too
//! ([`isolates_blend`]), and what blends inside it blends with its own content.

use florui_style::{BlendMode, ComputedStyle, Display, NodeId, Position, StyleMap, Tree};

/// The compositor's blend mode for `mode`.
pub(crate) fn to_tiny_skia(mode: BlendMode) -> tiny_skia::BlendMode {
    use tiny_skia::BlendMode as T;
    match mode {
        BlendMode::Normal => T::SourceOver,
        BlendMode::Multiply => T::Multiply,
        BlendMode::Screen => T::Screen,
        BlendMode::Overlay => T::Overlay,
        BlendMode::Darken => T::Darken,
        BlendMode::Lighten => T::Lighten,
        BlendMode::ColorDodge => T::ColorDodge,
        BlendMode::ColorBurn => T::ColorBurn,
        BlendMode::HardLight => T::HardLight,
        BlendMode::SoftLight => T::SoftLight,
        BlendMode::Difference => T::Difference,
        BlendMode::Exclusion => T::Exclusion,
        BlendMode::Hue => T::Hue,
        BlendMode::Saturation => T::Saturation,
        BlendMode::Color => T::Color,
        BlendMode::Luminosity => T::Luminosity,
        BlendMode::PlusLighter => T::Plus,
    }
}

/// Whether the node is a stacking context. `parent_display` is its parent's, since
/// a flex or grid item with a `z-index` is one even when it is not positioned.
pub(crate) fn creates_stacking_context(
    style: &ComputedStyle,
    parent_display: Option<Display>,
) -> bool {
    let positioned = style.position != Position::Static;
    let in_flex_or_grid = matches!(parent_display, Some(Display::Flex) | Some(Display::Grid));
    matches!(style.position, Position::Fixed | Position::Sticky)
        || (style.z_index.is_some() && (positioned || in_flex_or_grid))
        || style.opacity < 1.0
        || style.has_transform()
        || style.perspective.is_some()
        || style.preserve_3d
        || style.has_group_effect()
        || !style.backdrop_filter.is_empty()
        || style.isolate
        || style.container_type != florui_style::ContainerType::Normal
}

/// Whether something under `node` blends with what is behind it in `node`'s
/// own stacking context: a descendant with a blend mode, looked for through
/// the descendants that are not stacking contexts (a stacking context keeps
/// what blends inside it to itself).
fn subtree_blends(arena: &Tree, styles: &StyleMap, node: NodeId) -> bool {
    let parent_display = styles.get(&node).map(|s| s.display);
    arena.children(node).iter().any(|&child| {
        let Some(style) = styles.get(&child) else {
            return false;
        };
        if style.mix_blend_mode != BlendMode::Normal {
            return true;
        }
        !creates_stacking_context(style, parent_display) && subtree_blends(arena, styles, child)
    })
}

/// Whether `node`, a stacking context that is not a group for another reason, must
/// be painted as one so that what blends inside it is cut off from what is behind it.
pub(crate) fn isolates_blend(
    arena: &Tree,
    styles: &StyleMap,
    node: NodeId,
    parent_display: Option<Display>,
) -> bool {
    let Some(style) = styles.get(&node) else {
        return false;
    };
    // Cheap first: almost no node is a stacking context at all.
    if !creates_stacking_context(style, parent_display) {
        return false;
    }
    subtree_blends(arena, styles, node)
}

/// [`isolates_blend`] for a node whose parent is looked up in `arena`.
pub(crate) fn isolates(arena: &Tree, styles: &StyleMap, node: NodeId) -> bool {
    let parent_display = arena
        .parent(node)
        .and_then(|parent| styles.get(&parent))
        .map(|style| style.display);
    isolates_blend(arena, styles, node, parent_display)
}
