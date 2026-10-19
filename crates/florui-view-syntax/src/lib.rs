//! The `view!` grammar's own parsed shape, shared between
//! `florui-macros`'s real macro expansion and `florui-fmt`'s own
//! formatter — one grammar, not two independently evolving ones. This
//! crate is deliberately an ordinary library, not a `proc-macro` crate
//! (`florui-macros` is, and a `proc-macro = true` crate's public items
//! are unusable as a normal library by anything outside its own macro
//! entry points) — that split is the whole reason this crate exists
//! separately at all.
//!
//! Real, accurate span byte ranges (`proc_macro2::Span::byte_range`) are
//! only meaningful outside an actual macro-expansion context on stable
//! Rust (see `proc_macro2`'s own doc on `Span::byte_range`) — exactly
//! the situation `florui-fmt` runs in, parsing a `.rs` file as plain
//! text, never inside `rustc`'s own proc-macro server. That's what makes
//! a source-preserving formatter over this same grammar possible at all.

pub mod ast;
pub mod parse;
pub mod text;

pub use ast::{AttrName, AttrValue, Node};
pub use parse::Nodes;
