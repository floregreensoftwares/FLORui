//! [`Dialog`]: a real modal overlay built on [`crate::Portal`] — Escape
//! and a real focus trap (Tab/Shift+Tab cycle only within it, never
//! escaping to the document behind it) are handled structurally by
//! [`crate::focus`]/[`crate::UiRuntime`], keyed off [`MODAL_ROOT_CLASS`]
//! rather than anything `Dialog` itself does at render time — the same
//! "reserved marker the runtime recognizes structurally" pattern
//! [`crate::WINDOW_DRAG_REGION_ID`]/[`crate::WINDOW_INPUT_REGION_CLASS`]
//! already established, not a new kind of tag.
//!
//! `onclose` fires on Escape; wire the same [`Handler`] to `onclick`
//! yourself for backdrop-click dismissal too (real click dispatch never
//! bubbles, so a click on the dialog's own content can't trigger a
//! backdrop handler placed on an ancestor).
//!
//! Single-modal-at-a-time is this slice's own deliberate scope limit —
//! see [`crate::focus::modal_root`]'s own doc for the tie-break if a
//! second `Dialog` is ever open at once (not a real stacking contract
//! yet). Deferred, not silently dropped: anchor-tracked non-modal
//! popovers/menus (viewport collision, click-outside dismissal, and
//! explicitly no focus trap) are a separate, later feature.

use florui::{Children, Element, Event, Handler, component};

use crate::components::portal::{Portal, PortalProps};

/// A live `<div>` carrying this class is what [`crate::focus::modal_root`]
/// looks for — reserved, not meant to be styled directly by an app (style
/// its own child instead).
pub const MODAL_ROOT_CLASS: &str = "florui-modal-root";

/// Renders `children` as a real modal overlay — see the module doc. `label`
/// is the dialog's accessible name.
#[component]
pub fn Dialog(children: Children, onclose: Handler, label: String) -> Element {
    florui::view! {
        <Portal>
            <div
                class={MODAL_ROOT_CLASS}
                role="dialog"
                accessible_label={label}
                onclose={move || onclose.call(&Event::new())}
            >
                {children}
            </div>
        </Portal>
    }
}
