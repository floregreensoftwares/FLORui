//! Development-only tooling for Florui: a minimal native preview host, CSS
//! hot reload, structured diagnostics, offscreen capture, and — via
//! [`live`] — a real `florui-style`/`florui-layout`-backed preview paired
//! with the [`inspector`].
//!
//! [`element_scene`]/[`scene`] are an older stand-in reading a literal inline
//! `style=` attribute with no real layout; [`preview`] watches a raw CSS-only
//! fixture with no real component tree and owns its own window; [`live`] is
//! the real-tree counterpart: the application runs on `florui_platform`'s own
//! desktop host and the inspector only observes it.

pub mod capture;
pub mod color;
pub mod diagnostics;
pub mod element_preview;
pub mod element_scene;
pub mod fixture;
pub mod inspector;
pub mod live;
pub mod preview;
pub mod profile_panel;
pub mod scene;
