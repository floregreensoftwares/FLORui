//! Where `view!` says each element was written.

use florui::prelude::*;
use florui::{Element, SOURCE_LOCATIONS, SourceLocation};
use florui_reactive::ComponentScope;

/// Every located element, in document order, with its tag.
fn located(element: &Element) -> Vec<(&'static str, SourceLocation)> {
    let mut found = Vec::new();
    let mut pending = vec![element];
    while let Some(element) = pending.pop() {
        match element {
            Element::Node(node) => {
                if let Some(location) = node.source.location() {
                    found.push((node.tag, location));
                }
                pending.extend(node.children.iter().rev());
            }
            Element::Fragment(children) | Element::Portal(children) => {
                pending.extend(children.iter().rev());
            }
            Element::Text(_) => {}
        }
    }
    found
}

#[test]
fn every_element_carries_its_own_line_and_column() {
    let marker = line!();
    let tree = view! {
        <div>
            <ul>
                <li>{"a"}</li>
                <li>{"b"}</li>
            </ul>
        </div>
    };
    let found = located(&tree);
    let summary: Vec<_> = found
        .iter()
        .map(|(tag, at)| (*tag, at.line - marker, at.column, at.path))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("div", 2, 9, "div"),
            ("ul", 3, 13, "div > ul"),
            ("li", 4, 17, "div > ul > li[1]"),
            ("li", 5, 17, "div > ul > li[2]"),
        ]
    );
    assert!(found[0].1.file.ends_with("source_location.rs"));
}

#[test]
fn an_element_on_the_line_of_the_invocation_is_located_from_its_own_column() {
    let marker = line!();
    let tree = view! { <p>{"x"}</p> };
    let (_, at) = located(&tree)[0];
    assert_eq!(at.line, marker + 1);
    // 4 spaces, `let tree = ` (11), `view! { ` (8), then the `<` itself.
    assert_eq!(at.column, 4 + 11 + 8 + 1);
}

#[component]
fn Left() -> Element {
    view! { <span>{"l"}</span> }
}

#[component]
fn Right() -> Element {
    view! { <span>{"r"}</span> }
}

#[test]
fn the_component_that_built_an_element_is_recorded() {
    let (scope, _dirty) = ComponentScope::new();
    let tree = scope.render(|| {
        view! {
            <div>
                <Left />
                <Right />
            </div>
        }
    });
    let spans: Vec<_> = located(&tree)
        .into_iter()
        .filter(|(tag, _)| *tag == "span")
        .map(|(_, at)| at.component)
        .collect();
    assert_eq!(spans, vec![Some("Left"), Some("Right")]);
}

#[component]
fn Frame(children: Children) -> Element {
    view! { <section>{children}</section> }
}

#[test]
fn markup_slotted_into_a_component_keeps_the_path_through_it() {
    let (scope, _dirty) = ComponentScope::new();
    let tree = scope.render(|| {
        view! {
            <div>
                <Frame>
                    <em>{"hi"}</em>
                </Frame>
            </div>
        }
    });
    let em = located(&tree)
        .into_iter()
        .find(|(tag, _)| *tag == "em")
        .expect("the slotted element is located");
    assert_eq!(em.1.path, "div > Frame > em");
}

#[test]
fn an_element_inside_an_expression_is_located_by_its_own_block() {
    let marker = line!();
    let tree = view! {
        <div>
            {view! {
                <p>{"inner"}</p>
            }}
        </div>
    };
    let b = located(&tree)
        .into_iter()
        .find(|(tag, _)| *tag == "p")
        .expect("the inner element is located");
    assert_eq!(b.1.line, marker + 4);
    assert_eq!(b.1.path, "p");
}

#[test]
fn where_an_element_was_written_is_not_part_of_what_it_is() {
    let written = view! { <div class="a">{"x"}</div> };
    let built = Element::node(
        "div",
        vec![("class".to_string(), "a".to_string())],
        vec![Element::text("x")],
    );
    assert!(written.source().is_some() || !SOURCE_LOCATIONS);
    assert!(built.source().is_none());
    assert_eq!(written, built);
}

#[test]
fn locations_are_recorded_exactly_when_the_flag_says_so() {
    assert_eq!(
        SOURCE_LOCATIONS,
        cfg!(debug_assertions) || cfg!(feature = "source-locations")
    );
    let tree = view! { <div /> };
    assert_eq!(tree.source().is_some(), SOURCE_LOCATIONS);
}

#[test]
fn an_element_pays_for_a_reference_and_a_component_name_not_a_copy_of_the_site() {
    assert!(std::mem::size_of::<florui::Provenance>() <= 24);
}
