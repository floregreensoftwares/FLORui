use selectors::parser::{Combinator, Component, Selector, SelectorList};
use style::selector_parser::SelectorImpl;
use style::shared_lock::SharedRwLockReadGuard;
use style::stylesheets::{CssRule, CssRules};

use crate::stylesheet_parse::Rule;

/// How far the change of a node's state (hover, focus, press, form validity)
/// can reach in what a stylesheet matches, read from its selectors.
///
/// A state in the subject of a selector (`.row:hover`) restyles that node alone.
/// A state earlier in the selector (`.row:hover .cell`, `.item:hover + .item`)
/// restyles the nodes the combinators after it lead to: below the node, or
/// after it among its siblings. A state inside `:has`, a selector with `&`, or
/// a scope, can reach anywhere.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StateReach {
    /// Any node may be reached: nothing short of a whole cascade is safe.
    pub everything: bool,
    /// The nodes below a node whose state changed.
    pub descendants: bool,
    /// The siblings after a node whose state changed, and what is below them.
    pub siblings: bool,
}

const BELOW: u8 = 1;
const AFTER: u8 = 2;

impl StateReach {
    /// The reach of every selector of `sheets`.
    pub(crate) fn of(
        rules: &[Rule],
        viewport_height: f32,
        prefers_reduced_motion: bool,
        guard: &SharedRwLockReadGuard,
    ) -> Self {
        let mut reach = StateReach::default();
        let sheets = std::iter::once(crate::default_stylesheet::rule())
            .chain(std::iter::once(
                crate::default_stylesheet::scroll_behavior_reset_rule(),
            ))
            .chain(rules.iter().cloned());
        for rule in sheets {
            // A `:has()` that names a state is a class of the element it is asked
            // of that a change deep below it flips: no selector shows where from.
            if rule.has_specs().iter().any(|&number| {
                crate::has_adapter::spec(number).is_some_and(|spec| spec.mentions_state)
            }) {
                reach.everything = true;
            }
            let sheet = rule.stylesheet(viewport_height, &[], prefers_reduced_motion);
            reach.walk_rules(sheet.contents.rules.read_with(guard), guard);
        }
        reach
    }

    fn walk_rules(&mut self, rules: &CssRules, guard: &SharedRwLockReadGuard) {
        for rule in &rules.0 {
            match rule {
                CssRule::Style(style) => {
                    let style = style.read_with(guard);
                    for selector in style.selectors.slice() {
                        self.selector(selector, None);
                    }
                    if let Some(nested) = &style.rules {
                        self.walk_rules(nested.read_with(guard), guard);
                    }
                }
                CssRule::Media(rule) => self.walk_rules(rule.rules.read_with(guard), guard),
                CssRule::Container(rule) => self.walk_rules(rule.rules.read_with(guard), guard),
                CssRule::Supports(rule) => self.walk_rules(rule.rules.read_with(guard), guard),
                CssRule::Document(rule) => self.walk_rules(rule.rules.read_with(guard), guard),
                CssRule::LayerBlock(rule) => self.walk_rules(rule.rules.read_with(guard), guard),
                CssRule::StartingStyle(rule) => self.walk_rules(rule.rules.read_with(guard), guard),
                // A scope matches against an element the rule names and gives
                // its rules a place in the tree no selector of theirs shows.
                CssRule::Scope(_) => self.everything = true,
                CssRule::Namespace(_)
                | CssRule::Import(_)
                | CssRule::FontFace(_)
                | CssRule::FontFeatureValues(_)
                | CssRule::FontPaletteValues(_)
                | CssRule::CounterStyle(_)
                | CssRule::Keyframes(_)
                | CssRule::Margin(_)
                | CssRule::Page(_)
                | CssRule::Property(_)
                | CssRule::LayerStatement(_)
                | CssRule::PositionTry(_)
                | CssRule::NestedDeclarations(_) => {}
            }
        }
    }

    /// Adds the reach of `selector`. `inner` is where this selector sits inside
    /// another one's `:not`, `:is`, `:where` or `:nth-child(of)`: `None` for a
    /// selector of a rule, `Some(flags)` for one whose subject is a compound
    /// of an outer selector, with the combinators that lie to the right of
    /// that compound.
    fn selector(&mut self, selector: &Selector<SelectorImpl>, inner: Option<u8>) {
        // Matching order is from the subject to the left, combinators included.
        let mut right = 0u8;
        let mut in_subject = true;
        for component in selector.iter_raw_match_order() {
            match component {
                // What is in front of a pseudo-element is the element it belongs
                // to, which a change of state restyles together with its boxes.
                Component::Combinator(Combinator::PseudoElement) => {}
                Component::Combinator(combinator) => {
                    in_subject = false;
                    match combinator {
                        Combinator::Child | Combinator::Descendant => right |= BELOW,
                        Combinator::NextSibling | Combinator::LaterSibling => right |= AFTER,
                        _ => self.everything = true,
                    }
                }
                Component::NonTSPseudoClass(_) => {
                    if !in_subject {
                        self.add(right | inner.unwrap_or(0));
                    } else if let Some(outer) = inner {
                        self.add(outer);
                    }
                }
                Component::Negation(list) | Component::Is(list) | Component::Where(list) => {
                    self.list(list, in_subject, right, inner)
                }
                Component::NthOf(data) => self.list(
                    &SelectorList::from_iter(data.selectors().iter().cloned()),
                    in_subject,
                    right,
                    inner,
                ),
                Component::Has(relative) => {
                    if relative.iter().any(|r| mentions_state(&r.selector)) {
                        self.everything = true;
                    }
                }
                Component::ParentSelector => self.everything = true,
                Component::Slotted(_) | Component::Host(_) | Component::Part(_) => {
                    self.everything = true
                }
                _ => {}
            }
        }
    }

    /// A selector list inside a compound: its own subject is that compound.
    fn list(
        &mut self,
        list: &SelectorList<SelectorImpl>,
        in_subject: bool,
        right: u8,
        inner: Option<u8>,
    ) {
        let outer = if in_subject {
            inner
        } else {
            Some(right | inner.unwrap_or(0))
        };
        for selector in list.slice() {
            self.selector(selector, outer);
        }
    }

    fn add(&mut self, flags: u8) {
        if flags == 0 {
            // A state somewhere other than the subject with no combinator
            // after it cannot be told apart from anywhere.
            self.everything = true;
        }
        if flags & BELOW != 0 {
            self.descendants = true;
        }
        if flags & AFTER != 0 {
            self.siblings = true;
        }
    }
}

/// Whether `selector`, or a selector inside it, names a state.
fn mentions_state(selector: &Selector<SelectorImpl>) -> bool {
    selector
        .iter_raw_match_order()
        .any(|component| match component {
            Component::NonTSPseudoClass(_) => true,
            Component::Negation(list) | Component::Is(list) | Component::Where(list) => {
                list.slice().iter().any(mentions_state)
            }
            Component::NthOf(data) => data.selectors().iter().any(mentions_state),
            Component::Has(relative) => relative.iter().any(|r| mentions_state(&r.selector)),
            Component::Slotted(inner) => mentions_state(inner),
            Component::Host(Some(inner)) => mentions_state(inner),
            _ => false,
        })
}
