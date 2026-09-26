//! [`Portal`]: renders its children as an independent overlay root
//! instead of at its own call site — unclipped by any ancestor, painted
//! after (on top of) the document, and winning hit-testing over it.
//!
//! A real, structural [`florui::Element::Portal`] node — not a
//! side-channel registry. [`florui_style::Arena::build`] discovers it
//! while walking the tree and extracts its content into its own overlay
//! root, level by level, so a `Portal` nested inside another `Portal`'s
//! own content always lands *after* its ancestor (see `Arena::build`'s
//! own doc for why that's what makes nested overlays — a submenu inside
//! a menu, say — paint above their own parent, not behind it).

use florui::{Children, Element, IntoNodes, component};

/// Renders `children` as an overlay root — see the module doc.
#[component]
pub fn Portal(children: Children) -> Element {
    Element::Portal(children.into_nodes())
}
