//! Recursive-descent parsing of the `view!` grammar:
//!
//! ```text
//! nodes    := node*
//! node     := "{" expr "}" | text | element
//! text     := any run of tokens not starting with "<" or "{"
//! element  := "<" tag attr* ( "/>" | ">" nodes "<" "/" tag ">" )
//! attr     := ident "=" ( string-literal | "{" expr "}" )
//! ```
//!
//! A lowercase tag is checked against the shared [`florui_primitives`]
//! registry: unknown tags, void elements written with children, and
//! unsupported `<input type="...">` values are all rejected here, with one
//! source of truth instead of scattered ad hoc checks. See [`crate::text`]
//! for how a `text` run is reconstructed and its limits.

use florui_primitives::Content;
use syn::ext::IdentExt;
use syn::parse::{Parse, ParseStream};
use syn::{Ident, LitStr, Result, Token};

use crate::ast::{AttrName, AttrValue, Node};
use crate::text;

#[cfg_attr(test, derive(Debug))]
pub struct Nodes(pub Vec<Node>);

impl Parse for Nodes {
    fn parse(input: ParseStream) -> Result<Self> {
        let nodes = parse_nodes(input)?;
        if !input.is_empty() {
            return Err(input.error("unexpected trailing tokens in view!"));
        }
        Ok(Nodes(nodes))
    }
}

fn parse_nodes(input: ParseStream) -> Result<Vec<Node>> {
    let mut nodes = Vec::new();
    while !input.is_empty() && !peek_closing_tag(input) {
        nodes.push(parse_node(input)?);
    }
    Ok(nodes)
}

fn peek_closing_tag(input: ParseStream) -> bool {
    input.peek(Token![<]) && input.peek2(Token![/])
}

fn parse_node(input: ParseStream) -> Result<Node> {
    if input.peek(syn::token::Brace) {
        let content;
        syn::braced!(content in input);
        let expr = content.parse()?;
        return Ok(Node::Expr(expr));
    }

    if !input.peek(Token![<]) {
        return Ok(Node::Text(text::parse_text_run(input)?));
    }

    input.parse::<Token![<]>()?;
    let tag: Ident = input.parse()?;
    let tag_name = tag.to_string();
    let is_component = Node::is_component(&tag);
    let primitive = if is_component {
        None
    } else {
        Some(require_known_primitive(&tag)?)
    };

    let mut attrs = Vec::new();
    while !input.peek(Token![/]) && !input.peek(Token![>]) {
        // Attribute names must tolerate Rust keywords: `type` (as on
        // `<input>`) and `for` (as on `<label>`) are ordinary HTML
        // attributes but reserved words in Rust, so a plain `Ident` parse
        // would reject them.
        let name = parse_attr_name(input)?;
        input.parse::<Token![=]>()?;
        let value = if input.peek(LitStr) {
            AttrValue::Lit(input.parse()?)
        } else {
            let content;
            syn::braced!(content in input);
            AttrValue::Expr(content.parse()?)
        };
        attrs.push((name, value));
    }

    if tag_name == "input" {
        check_input_type(&attrs)?;
    }

    if input.peek(Token![/]) {
        input.parse::<Token![/]>()?;
        input.parse::<Token![>]>()?;
        return Ok(Node::Element {
            tag,
            attrs,
            children: Vec::new(),
            self_closing: true,
        });
    }

    if let Some(primitive) = primitive
        && primitive.content == Content::Void
    {
        return Err(syn::Error::new(
            tag.span(),
            format!("`<{tag}>` cannot have children; write `<{tag} ... />`"),
        ));
    }

    input.parse::<Token![>]>()?;

    let children = parse_nodes(input)?;

    input.parse::<Token![<]>()?;
    input.parse::<Token![/]>()?;
    let closing: Ident = input.parse()?;
    if closing != tag {
        return Err(syn::Error::new(
            closing.span(),
            format!("closing tag `</{closing}>` does not match opening tag `<{tag}>`"),
        ));
    }
    input.parse::<Token![>]>()?;

    Ok(Node::Element {
        tag,
        attrs,
        children,
        self_closing: false,
    })
}

/// An attribute name: an identifier, then any number of `-` or `:` and another,
/// as in `stop-color` and `xlink:href`.
fn parse_attr_name(input: ParseStream) -> Result<AttrName> {
    let first = input.call(Ident::parse_any)?;
    let mut text = first.to_string();
    loop {
        let separator = if input.peek(Token![-]) {
            input.parse::<Token![-]>()?;
            '-'
        } else if input.peek(Token![:]) && !input.peek(Token![::]) {
            input.parse::<Token![:]>()?;
            ':'
        } else {
            break;
        };
        let next = input.call(Ident::parse_any)?;
        text.push(separator);
        text.push_str(&next.to_string());
    }
    Ok(AttrName::new(first, text))
}

fn require_known_primitive(tag: &Ident) -> Result<&'static florui_primitives::Primitive> {
    let tag_name = tag.to_string();
    florui_primitives::find(&tag_name).ok_or_else(|| {
        let mut message = format!(
            "`<{tag}>` is not a known primitive tag; capitalize it to call a #[component], or use a supported primitive"
        );
        if let Some(suggestion) = florui_primitives::suggest(&tag_name) {
            message.push_str(&format!(" (did you mean `<{suggestion}>`?)"));
        }
        syn::Error::new(tag.span(), message)
    })
}

fn check_input_type(attrs: &[(AttrName, AttrValue)]) -> Result<()> {
    let Some((_, AttrValue::Lit(lit))) = attrs.iter().find(|(name, _)| name == "type") else {
        // No `type`, or a dynamic `{expr}` value that cannot be checked
        // until runtime — nothing to validate here.
        return Ok(());
    };

    let value = lit.value();
    if florui_primitives::is_supported_input_type(&value) {
        return Ok(());
    }

    Err(syn::Error::new(
        lit.span(),
        format!(
            "input type `{value}` is not supported yet; supported types are {:?}",
            florui_primitives::INITIAL_INPUT_TYPES
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    #[test]
    fn rejects_unknown_primitive_tags() {
        let result: Result<Nodes> = syn::parse2(quote! { <marquee>{"hi"}</marquee> });
        assert!(result.is_err());
    }

    #[test]
    fn suggests_a_close_misspelling_for_unknown_tags() {
        let result: Result<Nodes> = syn::parse2(quote! { <divv>{"hi"}</divv> });
        let message = result.unwrap_err().to_string();
        assert!(
            message.contains("did you mean `<div>`?"),
            "message was: {message}"
        );
    }

    #[test]
    fn accepts_known_primitive_tags() {
        let result: Result<Nodes> = syn::parse2(quote! { <div>{"hi"}</div> });
        assert!(result.is_ok());
    }

    #[test]
    fn accepts_capitalized_component_tags_without_checking_the_registry() {
        let result: Result<Nodes> = syn::parse2(quote! { <Card>{"hi"}</Card> });
        assert!(result.is_ok());
    }

    #[test]
    fn rejects_a_void_element_written_with_children() {
        let result: Result<Nodes> = syn::parse2(quote! { <br>{"hi"}</br> });
        assert!(result.is_err());
    }

    #[test]
    fn accepts_a_void_element_self_closed() {
        let result: Result<Nodes> = syn::parse2(quote! { <br /> });
        assert!(result.is_ok());
    }

    #[test]
    fn an_attribute_name_may_join_identifiers_with_a_hyphen_or_a_colon() {
        let result: Result<Nodes> = syn::parse2(
            quote! { <svg viewBox="0 0 10 10"><stop stop-color="red" xlink:href="#a" /></svg> },
        );
        let nodes = result.unwrap();
        let Node::Element { children, .. } = &nodes.0[0] else {
            panic!("an element");
        };
        let Node::Element { attrs, .. } = &children[0] else {
            panic!("an element");
        };
        assert_eq!(attrs[0].0.as_str(), "stop-color");
        assert_eq!(attrs[1].0.as_str(), "xlink:href");
        assert!(attrs[0].0.ident().is_none());
    }

    #[test]
    fn svg_elements_are_primitives_and_a_plain_name_is_one_identifier() {
        let result: Result<Nodes> = syn::parse2(quote! {
            <svg><defs><clipPath id="c"><rect width="1" height="1" /></clipPath></defs></svg>
        });
        assert!(result.is_ok());
        let result: Result<Nodes> = syn::parse2(quote! { <div class="a" /> });
        let nodes = result.unwrap();
        let Node::Element { attrs, .. } = &nodes.0[0] else {
            panic!("an element");
        };
        assert!(attrs[0].0.ident().is_some());
    }
    #[test]
    fn accepts_a_supported_input_type() {
        let result: Result<Nodes> = syn::parse2(quote! { <input type="checkbox" /> });
        assert!(result.is_ok());
    }

    #[test]
    fn rejects_an_unsupported_input_type() {
        let result: Result<Nodes> = syn::parse2(quote! { <input type="date" /> });
        assert!(result.is_err());
    }

    #[test]
    fn does_not_validate_a_dynamic_input_type() {
        let result: Result<Nodes> = syn::parse2(quote! { <input type={some_expr} /> });
        assert!(result.is_ok());
    }

    #[test]
    fn accepts_attribute_names_that_are_rust_keywords() {
        // `type` (input) and `for` (label) are ordinary HTML attributes
        // but reserved words in Rust.
        let result: Result<Nodes> = syn::parse2(quote! { <input type="text" /> });
        assert!(result.is_ok());
        let result: Result<Nodes> = syn::parse2(quote! { <label for="name">{"Name"}</label> });
        assert!(result.is_ok());
    }
}
