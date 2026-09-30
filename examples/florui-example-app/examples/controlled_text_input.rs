//! Real single-line text editing, live: caret positioning (click, drag
//! to select, double-click selects a word), Ctrl+A/C/X/V against the
//! real OS clipboard, and Ctrl+Z/Ctrl+Shift+Z undo/redo.
//!
//! Four fields. "Name" and "Nickname" are the two authoring contracts
//! the optional-convenience and explicit-control
//! contracts both require: "Name" carries a real `Binding<String>` that rejects
//! anything over 12 characters (so a real "the owner rejected this"
//! resync is visibly exercised, not just accepted edits); "Nickname"
//! carries a plain value plus `oninput` instead, with no `Binding`
//! anywhere in its own code. Both sibling `<span>`s below only update
//! once the owner actually accepts a change — never on every keystroke,
//! and never on a rejected one.
//!
//! "Default text"/"Default password" carry no `class` at all — real
//! `default_stylesheet.rs` UA styling only, checked directly against a
//! real Chromium: gray border, white background, black text, real
//! padding. Password masking paints a real substitute glyph run at the
//! real glyph positions, not a parallel masked buffer.
//!
//! Every `<label for="...">` here really names its control's real
//! accessible name through the platform accessibility bridge (Narrator,
//! on Windows) — not just visually adjacent text.
//!
//! `cargo run --example controlled_text_input -p florui-example-app`

use florui::prelude::*;
use florui_style::Rgba;

const CONTROLLED_TEXT_INPUT_CSS_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/controlled_text_input.css"
);

const MAX_NAME_LEN: usize = 12;

fn main() {
    florui_platform::run_with_css_reload(
        "Florui controlled text input",
        CONTROLLED_TEXT_INPUT_CSS_PATH,
        Rgba::opaque(0x10, 0x10, 0x14),
        app,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn app() -> Element {
    let name = use_signal(|| "Ada".to_string());
    let name_for_binding = name.clone();
    let name_binding = Binding::new(name.get(), move |requested: String| {
        if requested.chars().count() <= MAX_NAME_LEN {
            name_for_binding.set(requested);
        }
    });

    let nickname = use_signal(|| "Grace".to_string());
    let nickname_for_input = nickname.clone();

    let default_text = use_signal(|| "Hello".to_string());
    let default_text_binding =
        Binding::new(default_text.get(), move |value| default_text.set(value));

    let default_password = use_signal(|| "secret".to_string());
    let default_password_binding = Binding::new(default_password.get(), move |value| {
        default_password.set(value)
    });

    view! {
        <div class="page">
            <p class="instructions">
                {"Click to place the caret, drag or double-click to select, Ctrl+A/C/X/V, \
                  Ctrl+Z / Ctrl+Shift+Z to undo/redo."}
            </p>

            <label class="field-label" for="name-input">
                {"Name (Binding, rejects over 12 characters)"}
            </label>
            <input
                id="name-input"
                class="text-field"
                type="text"
                value={name_binding}
            />
            <p class="status">{format!("Committed: {:?}", name.get())}</p>

            <label class="field-label" for="nickname-input">
                {"Nickname (explicit value + oninput, no Binding)"}
            </label>
            <input
                id="nickname-input"
                class="text-field"
                type="text"
                value={nickname.get()}
                oninput={move |value: String| nickname_for_input.set(value)}
            />
            <p class="status">{format!("Committed: {:?}", nickname.get())}</p>

            <label class="field-label" for="default-text-input">
                {"Default text (no author CSS -- pure UA default)"}
            </label>
            <input id="default-text-input" type="text" value={default_text_binding} />

            <label class="field-label" for="default-password-input">
                {"Default password (no author CSS -- pure UA default)"}
            </label>
            <input id="default-password-input" type="password" value={default_password_binding} />
        </div>
    }
}
