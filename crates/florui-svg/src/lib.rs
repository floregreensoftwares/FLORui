//! SVG resources that CSS points at with `url(#id)`: a `<mask>`, a `<clipPath>`
//! or a `<filter>`, defined by an inline `<svg>` of the document or by a file.
//!
//! The elements of the document's inline SVGs are written back as markup, put
//! in a small SVG document with the shape or the picture they are applied to, and
//! drawn by the SVG renderer (`resvg`); what comes out is a coverage for a mask or
//! a clip, or the filtered pixels. Nothing of SVG is reimplemented here.

mod document;
mod filter;
mod render;

pub use document::{Reference, is_reference, reference};
pub use filter::filter_region;
pub use render::{Canvas, MaskKind, clip_coverage, filter_pixels, mask_coverage, render_svg};
