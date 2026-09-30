//! A real `<form>`: controls take part by `name`, Enter in a field or a
//! click on the submit button validates and reports `onsubmit`, and the
//! reset button reports `onreset`.
//!
//! Constraints are `required`, `minlength`, and `pattern`; a failed
//! submit focuses the first invalid field instead of reporting. Fields
//! turn red under `:user-invalid` once edited and left, or after a
//! submit attempt. The form never owns a value: `onsubmit` receives
//! `FormData` collected from what each control currently holds, and
//! `onreset` is where this app puts its own state back.
//!
//! `cargo run --example controlled_form -p florui-example-app`

use florui::prelude::*;
use florui_style::Rgba;

const CONTROLLED_FORM_CSS_PATH: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/examples/controlled_form.css");

fn main() {
    florui_platform::run_with_css_reload(
        "Florui controlled form",
        CONTROLLED_FORM_CSS_PATH,
        Rgba::opaque(0x10, 0x10, 0x14),
        app,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn app() -> Element {
    let username = use_signal(String::new);
    let password = use_signal(String::new);
    let email = use_signal(String::new);
    let qty = use_signal(|| "1".to_string());
    let plan = use_signal(|| "free".to_string());
    let newsletter = use_signal(|| false);
    let contact = use_signal(|| "email".to_string());
    let status = use_signal(|| "Nothing submitted yet.".to_string());

    let (username_input, password_input) = (username.clone(), password.clone());
    let (email_input, qty_input) = (email.clone(), qty.clone());
    let (plan_open, plan_value) = (use_signal(|| false), plan.clone());
    let plan_toggle = plan_open.clone();
    let plan_close = plan_open.clone();
    let newsletter_click = newsletter.clone();
    let (contact_email, contact_phone) = (contact.clone(), contact.clone());

    let reset_state = (
        username.clone(),
        password.clone(),
        email.clone(),
        qty.clone(),
        plan.clone(),
        newsletter.clone(),
        contact.clone(),
        status.clone(),
    );
    let submit_status = status.clone();

    let pick = |choice: &'static str| {
        let (plan, plan_open) = (plan_value.clone(), plan_open.clone());
        move || {
            plan.set(choice.to_string());
            plan_open.set(false);
        }
    };

    view! {
        <div class="page">
            <p class="instructions">
                {"Try submitting empty, with a short name, or with spaces in the name. \
                  Press Enter in a field or click Create account."}
            </p>
            <form
                id="signup"
                class="form"
                onsubmit={move |data: FormData| {
                    let summary: Vec<String> = data
                        .iter()
                        .map(|(name, value)| {
                            if name == "password" {
                                format!("{name}=({} chars)", value.chars().count())
                            } else {
                                format!("{name}={value}")
                            }
                        })
                        .collect();
                    submit_status.set(format!("Submitted: {}", summary.join(", ")));
                }}
                onreset={move || {
                    let (username, password, email, qty, plan, newsletter, contact, status) =
                        &reset_state;
                    username.set(String::new());
                    password.set(String::new());
                    email.set(String::new());
                    qty.set("1".to_string());
                    plan.set("free".to_string());
                    newsletter.set(false);
                    contact.set("email".to_string());
                    status.set("Reset.".to_string());
                }}
            >
                <label class="field-label" for="username">{"Username"}</label>
                <input
                    id="username"
                    class="text-field"
                    type="text"
                    name="username"
                    required="true"
                    minlength="3"
                    pattern="[a-z0-9]+"
                    value={username.get()}
                    oninput={move |value: String| username_input.set(value)}
                />

                <label class="field-label" for="password">{"Password"}</label>
                <input
                    id="password"
                    class="text-field"
                    type="password"
                    name="password"
                    required="true"
                    value={password.get()}
                    oninput={move |value: String| password_input.set(value)}
                />

                <label class="field-label" for="email">{"Email"}</label>
                <input
                    id="email"
                    class="text-field"
                    type="email"
                    name="email"
                    required="true"
                    value={email.get()}
                    oninput={move |value: String| email_input.set(value)}
                />

                <label class="field-label" for="qty">{"Seats (1 to 10)"}</label>
                <input
                    id="qty"
                    class="text-field"
                    type="number"
                    name="qty"
                    min="1"
                    max="10"
                    value={qty.get()}
                    oninput={move |value: String| qty_input.set(value)}
                />

                <input type="hidden" name="source" value="signup" />

                <label class="field-label" for="plan">{"Plan"}</label>
                <select
                    id="plan"
                    class="select"
                    name="plan"
                    open={plan_open.get()}
                    onclick={move || plan_toggle.set(!plan_toggle.get())}
                    ondismiss={move || plan_close.set(false)}
                >
                    <option value="free" selected={plan.get() == "free"} onclick={pick("free")}>
                        {"Free"}
                    </option>
                    <option value="pro" selected={plan.get() == "pro"} onclick={pick("pro")}>
                        {"Pro"}
                    </option>
                </select>

                <div class="row">
                    <input
                        id="newsletter"
                        class="checkbox"
                        type="checkbox"
                        name="newsletter"
                        checked={newsletter.get()}
                        onclick={move || newsletter_click.set(!newsletter_click.get())}
                    />
                    <label class="field-label" for="newsletter">{"Send me the newsletter"}</label>
                </div>

                <div class="row">
                    <input
                        id="contact-email"
                        class="radio"
                        type="radio"
                        name="contact"
                        value="email"
                        checked={contact.get() == "email"}
                        onclick={move || contact_email.set("email".to_string())}
                    />
                    <label class="field-label" for="contact-email">{"Email"}</label>
                    <input
                        id="contact-phone"
                        class="radio"
                        type="radio"
                        name="contact"
                        value="phone"
                        checked={contact.get() == "phone"}
                        onclick={move || contact_phone.set("phone".to_string())}
                    />
                    <label class="field-label" for="contact-phone">{"Phone"}</label>
                </div>

                <div class="row">
                    <button id="create" class="button" name="action" value="create">
                        {"Create account"}
                    </button>
                    <button id="reset" class="button" type="reset">{"Reset"}</button>
                </div>
            </form>
            <p class="status">{status.get()}</p>
        </div>
    }
}
