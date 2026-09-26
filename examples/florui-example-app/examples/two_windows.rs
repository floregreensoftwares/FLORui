//! Two real windows open at once, each independently titled and iconed:
//! window A loads `florui-config`'s resolved `app.icons.source` at
//! runtime (`florui_icon::load_icon_at_size` against a real, resolved
//! path); window B starts with a build-time embedded icon
//! (`florui_icon::embed`, via `build.rs` -- no source file needed at ship
//! time) and its button swaps it live, through
//! `WindowControls::set_icon`, to window A's icon -- a visible,
//! deterministic proof of the update path, not a timer.
//!
//! Both windows also opt into bounds persistence (`[window.persistence]`
//! in this package's own `florui.config.toml`, `enabled = true`) under
//! distinct keys (`"window-a"`/`"window-b"`) -- move or resize either one,
//! relaunch, and each reopens where it was left, independently of the
//! other.
//!
//! Each window also shows its own real `use_window_controls().is_focused()`
//! (click between the two windows to see them restyle independently), and
//! window A has a "Close window A" button -- closing it must never affect
//! window B's own state (try swapping window B's icon first, then close A,
//! then confirm B is still open with "Icon updated live" showing).
//!
//! `cargo run --example two_windows -p florui-example-app`

use florui::prelude::*;
use florui_icon::RawIcon;
use florui_platform::{
    WindowOptions, WindowPersistence, WindowSpec, run_windows, use_window_controls,
};
use florui_reactive::use_signal;
use florui_style::Rgba;

const CSS: &str = include_str!("two_windows.css");

mod second_window_icon {
    include!(concat!(env!("OUT_DIR"), "/second_window_icon.rs"));
}

/// Resolves this package's real `florui.config.toml` and loads
/// `app.icons.source` at runtime -- exactly what `florui-cli`'s own
/// `main.rs`/`doctor.rs` already do at ordinary process runtime, so doing
/// the same here isn't anything special. `None` (with a logged reason) on
/// any failure -- a missing/unreadable icon shouldn't take the whole demo
/// down.
fn load_config_resolved_icon() -> Option<RawIcon> {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let facts = florui_config::resolve_cargo_project(&manifest_dir, None)
        .inspect_err(|error| eprintln!("two_windows: could not resolve the project: {error}"))
        .ok()?;
    let resolution = florui_config::resolve(&facts, Some(florui_config::Target::Native), None)
        .inspect_err(|error| {
            eprintln!("two_windows: could not resolve florui.config.toml: {error}")
        })
        .ok()?;
    let path = resolution.config.app.icons.source?;
    florui_icon::load_icon_at_size(&path, florui_icon::DEFAULT_ICON_SIZE)
        .inspect_err(|error| eprintln!("two_windows: could not load {}: {error}", path.display()))
        .ok()
}

/// Resolves `florui.config.toml` again (a fresh, independent resolution --
/// `florui-config` doesn't cache, and this crate never depends on it, so
/// each app-side call site resolves what it needs on its own) and, if
/// `[window.persistence]` is enabled and `app.identifier` is set, returns
/// a `WindowPersistence` for `key` -- `None` (no persistence, logged
/// nowhere further; a missing identifier is an ordinary, expected
/// configuration choice, not a failure) otherwise. Both windows here share
/// one `enabled` flag from config but get their own distinct `key`,
/// exactly the "independent state for multiple windows" the config schema
/// itself doesn't have a multi-window shape for -- that's this call site's
/// own decision, not something `florui-config` resolves for it.
fn resolve_window_persistence(key: &str) -> Option<WindowPersistence> {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let facts = florui_config::resolve_cargo_project(&manifest_dir, None)
        .inspect_err(|error| eprintln!("two_windows: could not resolve the project: {error}"))
        .ok()?;
    let resolution = florui_config::resolve(&facts, Some(florui_config::Target::Native), None)
        .inspect_err(|error| {
            eprintln!("two_windows: could not resolve florui.config.toml: {error}")
        })
        .ok()?;
    let app_identifier = resolution.config.app.identifier?;
    resolution
        .config
        .window
        .persistence
        .enabled
        .then(|| WindowPersistence {
            app_identifier,
            key: key.to_owned(),
        })
}

fn main() {
    let icon_a = load_config_resolved_icon();

    let icon_b = RawIcon {
        rgba: second_window_icon::SECOND_WINDOW_ICON_RGBA.to_vec(),
        width: second_window_icon::SECOND_WINDOW_ICON_WIDTH,
        height: second_window_icon::SECOND_WINDOW_ICON_HEIGHT,
    };

    let spec_a = WindowSpec::new(
        "Florui -- Window A (config icon)",
        CSS,
        Rgba::opaque(0x1e, 0x1e, 0x22),
        WindowOptions {
            icon: icon_a.clone(),
            persistence: resolve_window_persistence("window-a"),
            ..WindowOptions::default()
        },
        window_a,
    )
    .expect("window A's stylesheet should parse");

    let spec_b = WindowSpec::new(
        "Florui -- Window B (embedded icon)",
        CSS,
        Rgba::opaque(0x22, 0x1a, 0x1e),
        WindowOptions {
            icon: Some(icon_b),
            persistence: resolve_window_persistence("window-b"),
            ..WindowOptions::default()
        },
        move || window_b(icon_a.clone()),
    )
    .expect("window B's stylesheet should parse");

    run_windows(vec![spec_a, spec_b])
        .expect("event loop should not fail on a real desktop session");
}

/// Also demonstrates two-window independence: closing this window (the
/// button below, or the real OS close) must not affect window B's own
/// state or lifecycle at all, and this window's own `use_window_controls`
/// reflects only this window's real focus, never window B's.
fn window_a() -> Element {
    let controls = use_window_controls();
    let focused = controls.as_ref().is_none_or(|c| c.is_focused());
    let close = controls.clone();

    view! {
        <div class="page">
            <p class="label">{"Window A"}</p>
            <p class="hint">
                {"Icon loaded at runtime from florui-config's resolved app.icons.source."}
            </p>
            <p class="hint">
                {if focused { "Focused" } else { "Not focused" }}
            </p>
            <button
                class="button"
                onclick={move || if let Some(controls) = &close {
                    controls.close();
                }}
            >
                {"Close window A"}
            </button>
        </div>
    }
}

/// `swap_to` is window A's own icon (if it loaded), reused here to prove
/// the update path with a plain `RawIcon` regardless of where it came
/// from -- embedded or runtime-loaded, `WindowControls::set_icon` doesn't
/// care.
fn window_b(swap_to: Option<RawIcon>) -> Element {
    let controls = use_window_controls();
    let focused = controls.as_ref().is_none_or(|c| c.is_focused());
    let updated = use_signal(|| false);

    let handle_click = {
        let controls = controls.clone();
        let swap_to = swap_to.clone();
        let updated = updated.clone();
        move || {
            if let (Some(controls), Some(icon)) = (&controls, &swap_to)
                && controls.set_icon(icon).is_ok()
            {
                updated.set(true);
            }
        }
    };

    view! {
        <div class="page">
            <p class="label">{"Window B"}</p>
            <p class="hint">
                {"Icon embedded at build time -- no source file needed at ship time."}
            </p>
            <p class="hint">
                {if focused { "Focused" } else { "Not focused" }}
            </p>
            <button class="button" onclick={handle_click}>
                {if updated.get() {
                    "Icon updated live -- now matches window A"
                } else {
                    "Swap to window A's icon"
                }}
            </button>
        </div>
    }
}
