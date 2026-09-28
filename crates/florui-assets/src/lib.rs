//! Shared, platform-independent infrastructure for resolving, decoding,
//! and caching image assets (raster and vector), reused by anything that
//! needs pixels from a source file or embedded byte slice —
//! `florui-icon`'s window/app icons today, and content elements like
//! `<img>` and themable icons in later work.
//!
//! ## Scope and non-goals
//!
//! This crate is synchronous and owns no executor, thread pool, or event
//! loop of its own: [`cache::AssetCache::get_or_load`] blocks the calling
//! thread for as long as reading and decoding takes. A consumer that must
//! not block its own UI thread (e.g. `florui-platform` loading an `<img>`)
//! is responsible for dispatching the call onto a background thread and
//! delivering the result back — including waking a host event loop when
//! it completes. That runtime/wakeup/renderer integration belongs to the
//! consuming layer, not here, the same way this crate has no opinion on
//! *when* a repaint happens, only on what pixels a source decodes to.
//!
//! There is no remote/HTTP source in this delivery. [`source::AssetSource`]
//! has exactly two variants — a local path and an embedded byte slice —
//! and adding a network-backed one is explicitly deferred: it would need
//! its own loading-boundary contract (retry policy, offline behavior,
//! transport failure diagnostics) that doesn't exist yet, not a silent
//! third `AssetSource` case bolted onto a local/embedded-shaped API.
//!
//! ## Format support
//!
//! - **Raster**: PNG only, via the `image` crate — the only raster format
//!   any consumer of this crate has actually needed so far (see
//!   `florui-icon`'s own prior scope). Adding another format is a feature
//!   flag plus a dispatch arm in `raster.rs`, not a redesign.
//! - **Vector**: SVG, via `resvg`/`usvg`/`tiny-skia`. This was already a
//!   dependency of `florui-icon` (window/app icon rasterization) before
//!   this crate existed, so reusing it here is a zero-net-new-dependency
//!   choice, not a fresh evaluation: it's a pure-Rust, actively maintained
//!   renderer with no FFI/build-system risk, it's MPL-2.0 licensed (file-
//!   level copyleft; compatible with linking into this project's MIT/
//!   Apache-dual-licensed binaries), and it already proved itself
//!   correct for this project's real icon assets. No other SVG
//!   parser/rasterizer was evaluated because none of those reasons changed.
//!
//! ## What this delivery does *not* cover
//!
//! No `<img>` element, no CSS `object-fit`/`object-position` integration,
//! no themable (`currentColor`) icon contract — those need this crate's
//! resolve/decode/cache primitives plus real layout and paint wiring,
//! which is separate, later work. This crate only has to be correct and
//! complete on its own terms: given a source, produce the right pixels or
//! a precise diagnostic, once, and let a cache avoid repeating that work.

mod cache;
mod error;
pub mod limits;
mod raster;
mod source;
mod svg;
#[cfg(feature = "watch")]
mod watch;

pub use cache::{AssetCache, DecodeParams};
pub use error::AssetError;
pub use raster::{RasterImage, decode_png};
pub use source::{AssetId, AssetSource};
pub use svg::{RasterFit, Size2D, VectorImage, parse_svg, rasterize_svg, substitute_current_color};
#[cfg(feature = "watch")]
pub use watch::{WatchError, watch_path};
