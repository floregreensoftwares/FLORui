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
//! A network source is not an [`source::AssetSource`] variant: that type is a local
//! path or embedded bytes, and a download has a loading contract of its own (timeouts,
//! redirects, a size cap while streaming, the media type against the bytes, an HTTP
//! disk cache, bounded and cancellable requests). It lives in the `net` module behind
//! the `net` feature, which hands back the bytes of an image; those decode through
//! [`AssetCache::get_or_decode`], under an [`AssetId::Url`], like any other source.
//!
//! ## Format support
//!
//! - **Raster**: PNG, JPEG, WebP and GIF (the first frame of an animated one),
//!   via the `image` crate, whose decoders are pure Rust. The size is read
//!   from the header and checked before a pixel is decoded, a JPEG is turned
//!   by its Exif orientation, and a decoded image keeps what painting needs
//!   (premultiplied pixels and a chain of smaller copies, see
//!   [`RasterImage::level_for`]) so a page with many of them paints each
//!   without work per frame. Colour profiles are ignored (the pixels are taken
//!   as sRGB). Adding another format is a feature flag plus a name in
//!   [`raster::Format`], not a redesign.
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
#[cfg(feature = "net")]
pub mod net;
mod raster;
mod source;
mod svg;
pub mod url_image;
#[cfg(feature = "watch")]
mod watch;

pub use cache::{AssetCache, DecodeParams};
pub use error::{AssetError, FetchError};
pub use raster::{Format, Level, RasterImage, decode_png, decode_raster};
pub use source::{AssetId, AssetSource};
pub use svg::{
    ObjectBounds, RasterFit, Size2D, SvgFonts, VectorImage, parse_svg, parse_svg_with_fonts,
    rasterize_svg, substitute_current_color,
};
#[cfg(feature = "watch")]
pub use watch::{WatchError, watch_path};
