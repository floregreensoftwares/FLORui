//! Opt-in single-instance behavior, plus deep-link/file classification:
//! run this example, then run it again -- with no arguments, with a
//! `florui-demo://...` URL, or with one or more `*.floruidemo` file paths
//! -- while the first is still open. The second launch hands the
//! classified activation event off to the first instead of opening a new
//! window, and the first instance's own window updates to show what it
//! received.
//!
//! The page also has a button that asks the user to make this application the
//! default for the demo's URL scheme and file type. That is the permission an
//! application has to ask for (`[app.activation] request_default`): it opens the
//! system's own settings page and sets nothing itself. It needs the example to be
//! registered first (`florui register`), or the answer is "not registered".
//!
//! ```text
//! cargo run --example single_instance -p florui-example-app
//! cargo run --example single_instance -p florui-example-app -- "florui-demo://open?id=42"
//! cargo run --example single_instance -p florui-example-app -- a.floruidemo b.floruidemo
//! ```

use std::sync::OnceLock;

use florui::prelude::*;
use florui_config::{DefaultRequest, ResolvedConfig};
use florui_platform::{
    ActivationEvent, DefaultHandlerPermission, DefaultTarget, RunOutcome, SingleInstance,
    WindowSpec, classify_launch, request_default_handler, run_single_instance, run_windows,
    use_activation_events,
};
use florui_reactive::use_signal;

const CSS: &str = include_str!("two_windows.css");

/// Resolves this package's real `florui.config.toml` once -- same pattern
/// `two_windows.rs` already uses for window persistence. `None` (with a
/// logged reason) on any resolution failure.
fn resolve_config() -> Option<ResolvedConfig> {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let facts = florui_config::resolve_cargo_project(&manifest_dir, None)
        .inspect_err(|error| eprintln!("single_instance: could not resolve the project: {error}"))
        .ok()?;
    florui_config::resolve(&facts, Some(florui_config::Target::Native), None)
        .inspect_err(|error| {
            eprintln!("single_instance: could not resolve florui.config.toml: {error}")
        })
        .ok()
        .map(|resolution| resolution.config)
}

/// What this application declared it may ask to be the default of, set once
/// from the configuration before the window opens.
static PERMISSION: OnceLock<DefaultHandlerPermission> = OnceLock::new();

fn default_permission(config: &ResolvedConfig) -> Option<DefaultHandlerPermission> {
    // The same identifier the registration is named after, not the suffixed
    // single-instance scope.
    let identifier = config.app.identifier.clone()?;
    let allowed = config
        .app
        .activation
        .request_default
        .iter()
        .map(|request| match request {
            DefaultRequest::UrlScheme(scheme) => DefaultTarget::UrlScheme(scheme.clone()),
            DefaultRequest::FileExtension(extension) => {
                DefaultTarget::FileExtension(extension.clone())
            }
        })
        .collect();
    Some(DefaultHandlerPermission {
        identifier,
        allowed,
    })
}

/// `None` (an ordinary, expected configuration choice, not a failure) if
/// `[app.activation].single_instance` isn't enabled or `app.identifier` is
/// unset.
fn resolve_single_instance(config: &ResolvedConfig) -> Option<SingleInstance> {
    if !config.app.activation.single_instance {
        return None;
    }
    let app_identifier = config.app.identifier.clone()?;
    Some(SingleInstance {
        // This crate's `florui.config.toml` is shared with every other
        // example here -- this suffix keeps this demo's own
        // single-instance scope from colliding with any of them.
        app_identifier: format!("{app_identifier}.single-instance-demo"),
        ..SingleInstance::default()
    })
}

/// Turns this process's own `argv` into a typed [`ActivationEvent`] using
/// this package's own declared `url_schemes`/file extensions -- `argv` is
/// read via `args_os` and lossily converted rather than the panic-on-
/// non-Unicode `std::env::args`, so a malformed OS-supplied argument
/// degrades to mangled text in a plain `Launch` instead of crashing.
fn classify_own_launch(config: &ResolvedConfig) -> ActivationEvent {
    let args = std::env::args_os()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let file_extensions: Vec<String> = config
        .app
        .activation
        .file_associations
        .iter()
        .map(|association| association.extension.clone())
        .collect();
    classify_launch(args, &config.app.activation.url_schemes, &file_extensions)
}

fn build_window_spec() -> WindowSpec {
    WindowSpec::new(
        "Florui -- single instance",
        CSS,
        florui_style::Rgba::opaque(0x1e, 0x1e, 0x22),
        Default::default(),
        root,
    )
    .expect("this example's stylesheet should parse")
}

fn main() {
    let Some(config) = resolve_config() else {
        eprintln!("single_instance: could not resolve florui.config.toml at all -- exiting");
        return;
    };
    if let Some(permission) = default_permission(&config) {
        let _ = PERMISSION.set(permission);
    }
    let launch_event = classify_own_launch(&config);
    let Some(single_instance) = resolve_single_instance(&config) else {
        eprintln!(
            "single_instance: [app.activation].single_instance is not enabled (or app.identifier is unset) -- running without single-instance enforcement"
        );
        run_windows(vec![build_window_spec()])
            .expect("event loop should not fail on a real desktop session");
        return;
    };

    match run_single_instance(single_instance, launch_event, vec![build_window_spec()]) {
        Ok(RunOutcome::Ran) => {}
        Ok(RunOutcome::HandedOff) => {
            println!(
                "single_instance: handed this launch's activation event off to the already-running instance"
            );
        }
        Ok(RunOutcome::HandoffFailed(event)) => {
            eprintln!(
                "single_instance: an existing instance owns this identifier but did not acknowledge the hand-off ({event:?}) -- becoming a fresh primary instead"
            );
            run_windows(vec![build_window_spec()])
                .expect("event loop should not fail on a real desktop session");
        }
        Err(error) => panic!("single_instance example failed: {error}"),
    }
}

fn root() -> Element {
    let pending = use_activation_events()
        .map(|events| events.take_pending())
        .unwrap_or_default();
    let answer = use_signal(|| "The default handler has not been asked for.".to_owned());
    let ask = {
        let answer = answer.clone();
        move || {
            let outcome = match PERMISSION.get() {
                Some(permission) => request_default_handler(
                    permission,
                    &DefaultTarget::UrlScheme("florui-demo".to_owned()),
                ),
                None => florui_platform::DefaultHandlerOutcome::NotDeclared,
            };
            answer.set(format!("Asked for florui-demo://: {outcome:?}"));
        }
    };
    let hint = if pending.is_empty() {
        "No activation event received yet on this render.".to_owned()
    } else {
        pending
            .iter()
            .map(|event| format!("{event:?}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    view! {
        <div class="page">
            <p class="label">{"Single-instance demo"}</p>
            <p class="hint">
                {"Run this example again -- plain, with a florui-demo://... URL, or with *.floruidemo file paths -- the second launch hands the classified event off instead of opening a new window."}
            </p>
            <p class="hint">{hint}</p>
            <button onclick={ask}>{"Ask to be the default for florui-demo://"}</button>
            <p class="hint">{answer.get()}</p>
        </div>
    }
}
