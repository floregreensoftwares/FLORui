//! Persistent local component state: [`use_signal`], [`use_memo`],
//! [`use_context`], [`use_ref`], [`use_effect`], [`use_child_scope`]/
//! [`use_child_scope_keyed`], and the [`ComponentScope`] that gives them somewhere
//! stable to live across repeated renders of the same tree. Also
//! [`KeyedExtents`]/[`ScrollAnchor`]: keyed-measurement and scroll-anchor
//! *contracts* for a future virtualized collection, not a virtualized
//! collection itself.
//!
//! # ComponentScope
//!
//! Not built yet: automatic dependency tracking for `use_memo`/`use_effect`
//! (deps are compared by equality, not inferred). One `ComponentScope` has one
//! flat, call-ordered slot list for its *positional* hooks, so every
//! order-sensitive hook call in the tree it renders must run in the same
//! order and count every time — `use_context` is the exception (a
//! provided value is looked up by type, not call position), and so is
//! [`use_child_scope_keyed`] (a child looked up by [`Key`] instead of
//! position, so a list can reorder without losing each item's own state).
//! A host learns about a [`Signal::set`] anywhere under its root either by
//! polling [`DirtyFlag::get`] or, better, registering a
//! [`DirtyFlag::on_mark`] callback to hear about it the instant it
//! happens — [`batch`] coalesces that notification across every write in
//! one synchronous unit of work (an event handler), rather than firing it
//! once per [`Signal::set`]. `Signal::get`'s read-after-write contract is
//! always synchronous, batched or not: it returns whatever the most
//! recent `set` stored, immediately — `batch` defers *notifying a host*,
//! never the write or a subsequent read of it.

mod attachment;
mod batch;
mod binding;
pub mod blocking;
mod context;
mod dirty;
mod effect;
mod error_boundary;
pub mod executor;
mod focus_host;
mod key;
mod loading;
mod memo;
mod refs;
mod resource;
mod scope;
mod signal;
pub mod testing;
pub mod trace;
mod virtualization;

pub use attachment::use_attachment;
pub use batch::batch;
pub use binding::Binding;
pub use context::{provide_context, use_context};
pub use dirty::DirtyFlag;
pub use effect::{Cleanup, use_effect};
pub use error_boundary::{ErrorBoundary, ErrorReporter, error_boundary, use_error_boundary};
pub use executor::Executor;
pub use focus_host::{FocusHost, use_focus_host};
pub use key::Key;
pub use loading::{TrackedRead, loading_boundary};
pub use memo::use_memo;
pub use refs::{Ref, use_ref};
pub use resource::{Resource, ResourceHandle, use_resource};
pub use scope::{ComponentScope, render_once, use_child_scope, use_child_scope_keyed};
pub use signal::{Signal, use_signal};
pub use virtualization::{Extent, KeyedExtents, ScrollAnchor};
