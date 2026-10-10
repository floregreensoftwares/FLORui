//! The look of the desk, in three stylesheets that cascade in this order: the
//! tokens (every color, the light and the radii), the type, and the surfaces
//! (wood, leather, brass, paper, glass) built from the tokens. Nothing outside
//! this folder writes a color down; a component asks for `var(--paper)` or
//! puts a surface class on an element.
//!
//! One `stylesheet!` fits in a module, so each sheet is a module of its own.

pub mod surfaces;
pub mod tokens;
pub mod typography;
