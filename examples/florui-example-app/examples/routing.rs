//! Typed native routing: two top-level routes plus one nested route,
//! back/forward history, a "confirm before leaving" guard, and a deep
//! link bridged in through the same activation events
//! `single_instance.rs` demonstrates on their own.
//!
//! Toggle "mark form dirty" on Home, then try navigating away -- the
//! guard denies it until you clear the flag again.
//!
//! Focus follows the history: go to a settings page with its nav button and
//! focus lands on that page's own control; press Back and focus returns to
//! the nav button you left from.
//!
//! ```text
//! cargo run --example routing -p florui-example-app
//! ```

use std::rc::Rc;

use florui::prelude::*;
use florui_platform::{ActivationEvent, use_activation_events};
use florui_reactive::executor::{Executor, LocalExecutor};
use florui_reactive::{provide_context, use_ref, use_signal};
use florui_routing::{
    ExternalNavigation, Guard, GuardDecision, Routable, RouteError, link, provide_router,
    route_outlet, use_route, use_route_focus, use_router,
};
use florui_style::Rgba;

const CSS: &str = include_str!("routing.css");

#[derive(Debug, Clone, PartialEq)]
enum AppRoute {
    Home,
    Settings(SettingsRoute),
}

#[derive(Debug, Clone, PartialEq)]
enum SettingsRoute {
    General,
    Profile,
}

impl Routable for SettingsRoute {
    fn parse(path: &str) -> Result<Self, RouteError> {
        match path {
            "/general" => Ok(SettingsRoute::General),
            "/profile" => Ok(SettingsRoute::Profile),
            _ => Err(RouteError::Unknown {
                path: path.to_owned(),
            }),
        }
    }

    fn format(&self) -> String {
        match self {
            SettingsRoute::General => "/general".to_owned(),
            SettingsRoute::Profile => "/profile".to_owned(),
        }
    }
}

impl Routable for AppRoute {
    fn parse(path: &str) -> Result<Self, RouteError> {
        if path == "/" {
            return Ok(AppRoute::Home);
        }
        let Some(rest) = path.strip_prefix("/settings") else {
            return Err(RouteError::Unknown {
                path: path.to_owned(),
            });
        };
        let rest = if rest.is_empty() { "/general" } else { rest };
        SettingsRoute::parse(rest)
            .map(AppRoute::Settings)
            .map_err(|_| RouteError::Unknown {
                path: path.to_owned(),
            })
    }

    fn format(&self) -> String {
        match self {
            AppRoute::Home => "/".to_owned(),
            AppRoute::Settings(sub) => format!("/settings{}", sub.format()),
        }
    }
}

fn main() {
    florui_platform::run(
        "Florui -- typed native routing",
        CSS,
        Rgba::opaque(0x1e, 0x1e, 0x22),
        root,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn root() -> Element {
    // Created once, re-provided every render -- provide_context is
    // cleared at the start of every render, so this line must never be
    // skipped, even though the Rc it clones is only built the first time.
    let executor = use_ref(|| Rc::new(LocalExecutor::new()) as Rc<dyn Executor>);
    provide_context(executor.get());

    let dirty = use_signal(|| false);
    let last_outcome = use_signal(|| "(none yet)".to_owned());

    let dirty_for_guard = dirty.clone();
    let confirm_leave_guard: Guard<AppRoute> = Rc::new(move |_from, _to, _kind| {
        if dirty_for_guard.get() {
            GuardDecision::Deny
        } else {
            GuardDecision::Allow
        }
    });

    provide_router(AppRoute::Home, vec![confirm_leave_guard], move || {
        let router = use_router::<AppRoute>();
        let current = use_route::<AppRoute>();

        // The app's own glue bridging activation events into navigation --
        // florui-routing has no idea florui-platform or ActivationEvent
        // exist; this is the only place the two are connected.
        let pending = use_activation_events()
            .map(|events| events.take_pending())
            .unwrap_or_default();
        for event in pending {
            if let ActivationEvent::OpenUrl { url } = event
                && let Ok(route) = AppRoute::parse(&url)
            {
                router.apply_external(ExternalNavigation::NavigateTo(route));
            }
        }

        let record = {
            let last_outcome = last_outcome.clone();
            move |outcome: florui_routing::NavOutcome| last_outcome.set(format!("{outcome:?}"))
        };

        let go_home = {
            let router = router.clone();
            let record = record.clone();
            move || record(router.push(AppRoute::Home))
        };
        let go_general = {
            let router = router.clone();
            let record = record.clone();
            move || record(router.push(AppRoute::Settings(SettingsRoute::General)))
        };
        let go_profile = {
            let router = router.clone();
            let record = record.clone();
            move || record(router.push(AppRoute::Settings(SettingsRoute::Profile)))
        };
        let go_back = {
            let router = router.clone();
            let record = record.clone();
            move || record(router.back())
        };
        let go_forward = {
            let router = router.clone();
            let record = record.clone();
            move || record(router.forward())
        };
        let toggle_dirty = {
            let dirty = dirty.clone();
            move || dirty.set(!dirty.get())
        };

        // Focus follows the history: Back and Forward return to the control
        // the user left each page on (the nav button they pressed, found by
        // its id), and a fresh navigation lands on the page's own first
        // control.
        use_route_focus::<AppRoute>(|route| {
            Some(
                match route {
                    AppRoute::Home => "page-home",
                    AppRoute::Settings(SettingsRoute::General) => "page-general",
                    AppRoute::Settings(SettingsRoute::Profile) => "page-profile",
                }
                .to_owned(),
            )
        });

        let page = route_outlet(&current, |route| match route {
            AppRoute::Home => view! {
                <div id="page-home" class="page-content">
                    <p class="label">{"Home"}</p>
                    <button id="dirty-toggle" class="button" onclick={toggle_dirty}>
                        {if dirty.get() { "Form is dirty (click to clear)" } else { "Mark form dirty" }}
                    </button>
                    {link(&AppRoute::Settings(SettingsRoute::Profile), || {
                        view! { {"Go to Settings / Profile"} }
                    })}
                </div>
            },
            AppRoute::Settings(sub) => route_outlet(sub, |sub| match sub {
                SettingsRoute::General => view! {
                    <div id="page-general" class="page-content">
                        <p class="label">{"Settings / General"}</p>
                        <button id="general-action" class="button">{"General action"}</button>
                    </div>
                },
                SettingsRoute::Profile => view! {
                    <div id="page-profile" class="page-content">
                        <p class="label">{"Settings / Profile"}</p>
                        <button id="profile-action" class="button">{"Profile action"}</button>
                    </div>
                },
            }),
        });

        view! {
            <div class="page">
                <div class="nav">
                    <button id="nav-home" class="button" onclick={go_home}>{"Home"}</button>
                    <button id="nav-general" class="button" onclick={go_general}>
                        {"Settings: General"}
                    </button>
                    <button id="nav-profile" class="button" onclick={go_profile}>
                        {"Settings: Profile"}
                    </button>
                    <button id="nav-back" class="button" onclick={go_back}>{"Back"}</button>
                    <button id="nav-forward" class="button" onclick={go_forward}>{"Forward"}</button>
                </div>
                <p class="hint">{format!("Current route: {}", current.format())}</p>
                <p class="hint">{format!("Last navigation outcome: {}", last_outcome.get())}</p>
                {page}
            </div>
        }
    })
}
