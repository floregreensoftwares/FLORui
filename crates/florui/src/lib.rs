//! Component composition core: the element tree, the `view!` macro, and the
//! `#[component]` attribute. No styling, layout, text shaping, painting, or
//! rendering exists here yet.

mod children;
mod element;
mod handler;
mod into_nodes;
mod stylesheets;

pub use children::Children;
pub use element::{Element, ElementNode};
/// Named slots — a component's own typed, non-`children` props — are
/// required at compile time exactly like any other prop: omitting one
/// leaves the generated props struct literal incomplete.
///
/// ```compile_fail
/// use florui::prelude::*;
///
/// #[component]
/// fn Dialog(header: Element, body: Element) -> Element {
///     view! { <div>{header}{body}</div> }
/// }
///
/// fn missing_required_slot() -> Element {
///     // `body` is required and not provided — must not compile.
///     view! { <Dialog header={view! { <h1>{"Title"}</h1> }} /> }
/// }
/// ```
pub use florui_macros::component;
pub use florui_macros::{stylesheet, stylesheet_scoped, view};
pub use florui_reactive as reactive;
pub use handler::{BoolHandler, FloatHandler, Handler, ValueHandler};
pub use into_nodes::IntoNodes;
pub use stylesheets::{
    StyleScope, StylesheetSource, apply_scope_to_class_attr, dedup as dedup_stylesheets,
    style_scope_hash,
};

pub mod prelude {
    pub use crate::reactive::{
        Binding, Cleanup, ComponentScope, ErrorBoundary, ErrorReporter, Executor, Key, Ref,
        Resource, ResourceHandle, Signal, TrackedRead, batch, error_boundary, loading_boundary,
        provide_context, render_once, use_attachment, use_child_scope, use_child_scope_keyed,
        use_context, use_effect, use_error_boundary, use_memo, use_ref, use_resource, use_signal,
    };
    pub use crate::{
        BoolHandler, Children, Element, FloatHandler, Handler, IntoNodes, StyleScope, ValueHandler,
        component, stylesheet, stylesheet_scoped, view,
    };
}
