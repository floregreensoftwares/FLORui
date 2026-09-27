//! Decodes a window icon (SVG or PNG) into plain RGBA pixels, and embeds
//! an already-decoded icon into a generated Rust source file at build
//! time so a shipped binary never depends on the source asset being
//! present on disk. No `winit` (or any windowing) dependency — converting
//! [`RawIcon`] into a real platform icon type belongs to whichever crate
//! actually creates windows.
//!
//! [`RawIcon`] itself has no dependencies; decoding and embedding live
//! behind the `decode` feature (default-on) so a consumer that only needs
//! the type (e.g. the runtime side of an already build-time-embedded
//! icon) doesn't have to link `florui-assets`/`resvg`/`image`.
//!
//! The actual resolution/parsing/decoding is `florui-assets`'s
//! (`decode.rs` only adapts it to this crate's own [`RawIcon`]/
//! [`IconError`] shape and the square-icon-specific default size) — see
//! that crate's own doc for the SVG library choice and format scope.

#[cfg(feature = "decode")]
mod decode;
#[cfg(feature = "decode")]
pub mod embed;

#[cfg(feature = "decode")]
pub use decode::{IconError, decode_png, decode_svg, load_icon, load_icon_at_size};

/// Decoded, straight (non-premultiplied) 32bpp RGBA pixels, tightly
/// packed row-major — exactly what `winit::window::Icon::from_rgba` (or
/// any similar platform icon constructor) wants, `rgba.len() ==
/// (width * height * 4) as usize`.
#[derive(Debug, Clone, PartialEq)]
pub struct RawIcon {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// The side length an SVG source rasterizes to when no explicit size is
/// requested. Windows' own guidance (see `winit`'s `set_window_icon` doc)
/// is to pick a multiple of the 16x16 base size to account for scaling;
/// 256 comfortably covers every common display scale factor without
/// producing an unreasonably large buffer.
pub const DEFAULT_ICON_SIZE: u32 = 256;
