//! Typed native routing: an app defines its own route enum implementing
//! [`Routable`], mounts it with [`provide_router`], and reads/navigates
//! it via [`use_router`]/[`use_route`]/[`route_outlet`]/[`link`].
//! Deliberately
//! platform-independent -- this crate depends only on `florui` and
//! `florui-reactive`, never `florui-platform`, so it stays usable from a
//! future Web host too. A native app bridges its own deep-link events
//! (e.g. `florui_platform`'s activation events) into navigation itself,
//! via [`Router::apply_external`] -- see that method's own doc.
//!
//! Leaving a route disposes its whole subtree by default (every signal,
//! effect, and nested outlet it owns) -- there is no keep-alive cache;
//! an app that needs one builds it itself on top of [`route_outlet`].
//! Each history entry also carries what it needs to be re-entered as the
//! user left it. Focus is handled for the app: when the host provides a
//! [`florui_reactive::FocusHost`], the router records the focused element's
//! `id` as an entry is left and [`use_route_focus`] gives it back on Back or
//! Forward, or lands on the page's entry point otherwise. Scroll stays the
//! app's own job, since only it knows which region scrolls: the router stores
//! and hands back a [`florui_reactive::ScrollAnchor`] per entry (see
//! [`Router::set_current_scroll_anchor`]) and reports when a navigation
//! committed (see [`use_route_transition`]) -- it never scrolls anything
//! itself.

mod link;
mod outlet;
mod provider;
mod query;
mod routable;
mod router;
pub mod testing;

pub use link::link;
pub use outlet::route_outlet;
pub use provider::{provide_router, use_route, use_route_focus, use_route_transition, use_router};
pub use query::{decode_query_pairs, split_query};
pub use routable::{Routable, RouteError};
pub use router::{
    ExternalNavigation, Guard, GuardDecision, HistoryEntry, NavKind, NavOutcome, Router,
};
