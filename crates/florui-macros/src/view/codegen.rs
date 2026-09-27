//! Turns a parsed `view!` body into an expression that builds an
//! `::florui::Element` tree at runtime.
//!
//! A lowercase tag becomes `Element::node(...)`. A capitalized tag is a
//! component call: it builds `<Tag>Props { ... }` from its attributes
//! (passed through exactly as written, with no implicit conversion — the
//! author writes `.into()` when a field needs it) and, for a non-self-closing
//! tag, a `children` field collected from its nested content.
//!
//! Every child — a nested element or a `{expr}` — is routed through
//! `IntoNodes::into_nodes` so a single element, a fragment, `Children`, or
//! text all compose the same way.

use proc_macro2::TokenStream;
use quote::{format_ident, quote, quote_spanned};
use syn::{Expr, Ident};

use florui_view_syntax::{AttrValue, Node};

/// `onclick`, `onmouseenter`, ... — any attribute in this shape names an
/// event handler rather than a plain string attribute; `view!` has no
/// fixed list of recognized event names; it's needed once a host actually
/// dispatches one.
fn event_name(attr_name: &str) -> Option<&str> {
    attr_name.strip_prefix("on").filter(|rest| !rest.is_empty())
}

/// `::florui::Handler::new` and `::florui::Handler::with_event` share no
/// common trait bound (a single trait can't be blanket-implemented for
/// both `Fn()` and `Fn(&Event)` at once — see `Handler`'s own doc
/// comment), so `view!` picks between them itself: a one-parameter
/// closure literal (`|event| ...`) wants the event, anything else — a
/// zero-arg closure, or an expression that isn't a closure literal at all
/// (a named function, a variable already holding a `Handler`-shaped
/// value) — takes the plain constructor, matching today's behavior.
fn handler_constructor(expr: &Expr) -> Ident {
    let wants_event = matches!(expr, Expr::Closure(closure) if closure.inputs.len() == 1);
    format_ident!("{}", if wants_event { "with_event" } else { "new" })
}

pub fn expand(nodes: Vec<Node>) -> TokenStream {
    if nodes.is_empty() {
        return quote! {
            compile_error!("view! requires at least one element or expression")
        };
    }

    let values = nodes.iter().map(|node| child_value(node, None));
    quote! {
        {
            let mut __roots: ::std::vec::Vec<::florui::Element> = ::std::vec::Vec::new();
            #( __roots.extend(::florui::IntoNodes::into_nodes(#values)); )*
            if __roots.len() == 1 {
                __roots.pop().expect("just checked len() == 1")
            } else {
                ::florui::Element::Fragment(__roots)
            }
        }
    }
}

/// `scope` is the enclosing `scope={expr}` value, if any element
/// higher in this same `view!` tree declared one — inherited into every
/// literal-tag descendant here, but never into a `component_call`, whose
/// own body is a separate `view!` expansion this one has no visibility
/// into (that boundary is what keeps scoping from leaking into a child
/// component's own internals for free).
fn child_value(node: &Node, scope: Option<&TokenStream>) -> TokenStream {
    match node {
        Node::Expr(expr) => quote! { (#expr) },
        Node::Text(text) => quote! { #text },
        Node::Element {
            tag,
            attrs,
            children,
            self_closing,
        } => {
            if Node::is_component(tag) {
                component_call(tag, attrs, children, *self_closing, scope)
            } else {
                primitive_element(tag, attrs, children, scope)
            }
        }
    }
}

fn children_vec(children: &[Node], scope: Option<&TokenStream>) -> TokenStream {
    let values = children.iter().map(|node| child_value(node, scope));
    quote! {
        {
            let mut __children: ::std::vec::Vec<::florui::Element> = ::std::vec::Vec::new();
            #( __children.extend(::florui::IntoNodes::into_nodes(#values)); )*
            __children
        }
    }
}

/// `scope={...}` on a primitive element, like `key=` on a component
/// call, is a `view!`-level directive, not an attribute of the element
/// itself — it never reaches `Element::node`'s own attrs.
fn is_scope_attr(name: &Ident) -> bool {
    name == "scope"
}

fn primitive_element(
    tag: &Ident,
    attrs: &[(Ident, AttrValue)],
    children: &[Node],
    inherited_scope: Option<&TokenStream>,
) -> TokenStream {
    let tag_str = tag.to_string();
    let own_scope =
        attrs
            .iter()
            .find(|(name, _)| is_scope_attr(name))
            .map(|(_, value)| match value {
                AttrValue::Lit(lit) => quote! { #lit },
                AttrValue::Expr(expr) => quote! { #expr },
            });
    let effective_scope = own_scope.as_ref().or(inherited_scope);

    // `oninput`'s presence decides which contract `value` uses below --
    // see slots-and-bindings.md's "Optional convenience and explicit
    // control": a bare `value={expr}` is the `Binding<String>` contract,
    // `value={expr}` alongside `oninput={...}` is the explicit
    // value/callback contract instead. The macro has no type information
    // to tell a `Binding<String>` apart from a plain string expression,
    // so it dispatches on this syntactic signal; the two contracts are
    // still mutually exclusive in practice because a real `Binding` has
    // no `.to_string()` (the explicit path's own codegen below) and a
    // plain string has no `.get()` (the binding path's) -- whichever one
    // doesn't match what was actually passed simply fails to compile.
    let has_value_change_handler = attrs.iter().any(|(name, _)| name == "oninput");

    let mut attr_pairs = Vec::new();
    let mut handler_pairs = Vec::new();
    let mut binding_pairs = Vec::new();
    let mut value_handler_pairs = Vec::new();
    let mut selection_handler_pairs = Vec::new();

    for (name, value) in attrs {
        if is_scope_attr(name) {
            continue;
        }
        let name_str = name.to_string();
        if name_str == "oninput" {
            value_handler_pairs.push(match value {
                AttrValue::Expr(expr) => {
                    quote! { ("value".to_string(), ::florui::ValueHandler::new(#expr)) }
                }
                AttrValue::Lit(lit) => {
                    let message = "`oninput` needs a Rust expression in braces, e.g. \
                                    `oninput={move |value: String| ...}`, not a string literal";
                    quote_spanned! { lit.span() => compile_error!(#message) }
                }
            });
        } else if name_str == "onselectionchange" {
            selection_handler_pairs.push(match value {
                AttrValue::Expr(expr) => {
                    quote! { ("selection".to_string(), ::florui::SelectionHandler::new(#expr)) }
                }
                AttrValue::Lit(lit) => {
                    let message = "`onselectionchange` needs a Rust expression in braces, e.g. \
                                    `onselectionchange={move |values: Vec<String>| ...}`, not a \
                                    string literal";
                    quote_spanned! { lit.span() => compile_error!(#message) }
                }
            });
        } else if let Some(event) = event_name(&name_str) {
            handler_pairs.push(match value {
                AttrValue::Expr(expr) => {
                    let ctor = handler_constructor(expr);
                    quote! { (#event.to_string(), ::florui::Handler::#ctor(#expr)) }
                }
                AttrValue::Lit(lit) => {
                    let message = format!(
                        "event handler attribute `{name_str}` needs a Rust expression in \
                         braces, e.g. `{name_str}={{move || ...}}`, not a string literal"
                    );
                    quote_spanned! { lit.span() => compile_error!(#message) }
                }
            });
        } else if name_str == "value" && matches!(value, AttrValue::Expr(_)) {
            let AttrValue::Expr(expr) = value else {
                unreachable!("matched above")
            };
            if has_value_change_handler {
                // The explicit contract: `expr` is a plain value (often a
                // `Signal::get()` result), not a `Binding` -- `oninput`
                // above already carries the write-back half.
                attr_pairs.push(quote! { (#name_str.to_string(), (#expr).to_string()) });
            } else {
                // The `Binding<String>` convenience contract -- `attrs`
                // still gets the current snapshot string alongside, so
                // every existing string-only consumer (measurement,
                // paint) needs no `<input>`-specific lookup just to read
                // the displayed text.
                attr_pairs.push(quote! { (#name_str.to_string(), (#expr).get()) });
                binding_pairs.push(quote! { (#name_str.to_string(), (#expr).clone()) });
            }
        } else {
            let value_expr = match value {
                AttrValue::Lit(lit) => quote! { (#lit).to_string() },
                AttrValue::Expr(expr) => quote! { (#expr).to_string() },
            };
            let value_expr = if name_str == "class" {
                match effective_scope {
                    Some(scope) => {
                        quote! { ::florui::apply_scope_to_class_attr(&(#value_expr), #scope) }
                    }
                    None => value_expr,
                }
            } else {
                value_expr
            };
            attr_pairs.push(quote! { (#name_str.to_string(), #value_expr) });
        }
    }
    let children = children_vec(children, effective_scope);

    if !selection_handler_pairs.is_empty() {
        quote! {
            ::florui::Element::node_with_selection_handlers(
                #tag_str,
                ::std::vec![ #(#attr_pairs),* ],
                ::std::vec![ #(#handler_pairs),* ],
                ::std::vec![ #(#binding_pairs),* ],
                ::std::vec![ #(#value_handler_pairs),* ],
                ::std::vec![ #(#selection_handler_pairs),* ],
                #children,
            )
        }
    } else if !value_handler_pairs.is_empty() {
        quote! {
            ::florui::Element::node_with_value_handlers(
                #tag_str,
                ::std::vec![ #(#attr_pairs),* ],
                ::std::vec![ #(#handler_pairs),* ],
                ::std::vec![ #(#binding_pairs),* ],
                ::std::vec![ #(#value_handler_pairs),* ],
                #children,
            )
        }
    } else if !binding_pairs.is_empty() {
        quote! {
            ::florui::Element::node_with_bindings(
                #tag_str,
                ::std::vec![ #(#attr_pairs),* ],
                ::std::vec![ #(#handler_pairs),* ],
                ::std::vec![ #(#binding_pairs),* ],
                #children,
            )
        }
    } else if handler_pairs.is_empty() {
        quote! {
            ::florui::Element::node(#tag_str, ::std::vec![ #(#attr_pairs),* ], #children)
        }
    } else {
        quote! {
            ::florui::Element::node_with_handlers(
                #tag_str,
                ::std::vec![ #(#attr_pairs),* ],
                ::std::vec![ #(#handler_pairs),* ],
                #children,
            )
        }
    }
}

/// `key={...}` on a component call is its caller-assigned identity (see
/// `florui_reactive::use_child_scope_keyed`), not a prop of the component
/// itself — it never becomes a `Props` field.
fn is_key_attr(name: &Ident) -> bool {
    name == "key"
}

/// `scope` here is the enclosing `scope`, applied only to `children`:
/// markup slotted into a component call is authored in the *caller's*
/// `view!` block, so it keeps the caller's scope, exactly like any other
/// literal element there — it never affects the component's own props or
/// reaches inside the component's own separately-expanded body.
fn component_call(
    tag: &Ident,
    attrs: &[(Ident, AttrValue)],
    children: &[Node],
    self_closing: bool,
    scope: Option<&TokenStream>,
) -> TokenStream {
    let props_ident = format_ident!("{tag}Props");
    let key_attr = attrs.iter().find(|(name, _)| is_key_attr(name));
    let field_inits = attrs
        .iter()
        .filter(|(name, _)| !is_key_attr(name))
        .map(|(name, value)| {
            let value_expr = match value {
                AttrValue::Lit(lit) => quote! { #lit },
                AttrValue::Expr(expr) => quote! { #expr },
            };
            quote! { #name: #value_expr, }
        });

    let children_field = if self_closing {
        TokenStream::new()
    } else {
        let children = children_vec(children, scope);
        quote! { children: ::florui::Children::from(#children), }
    };

    let props = quote! { #props_ident { #(#field_inits)* #children_field } };

    match key_attr {
        Some((_, value)) => {
            let key_expr = match value {
                AttrValue::Lit(lit) => quote! { #lit },
                AttrValue::Expr(expr) => quote! { #expr },
            };
            let keyed_tag = format_ident!("__florui_keyed_{tag}");
            quote! { #keyed_tag(::florui::reactive::Key::from(#key_expr), #props) }
        }
        None => quote! { #tag(#props) },
    }
}
