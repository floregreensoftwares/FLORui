//! The frame around the screens: the desk itself, the glass lid with the
//! name and the lamps, the leather rail with the keys that go from screen to
//! screen, and the place where the current screen is shown. It also owns the
//! two pieces of state every screen shares, and hands them down.

use florui::prelude::*;
use florui_routing::{provide_router, route_outlet, use_route, use_router};

use crate::components::led::{Led, LedProps, Tone};
use crate::features::settings::Preferences;
use crate::features::tasks::{TaskStore, counts};
use crate::pages;
use crate::routes::Page;

stylesheet!("./app.css");

/// How many sample tasks the desk starts with.
const SAMPLE_TASKS: usize = 2_000;

#[component]
pub fn App() -> Element {
    let store = TaskStore::use_new(SAMPLE_TASKS);
    let prefs = Preferences::use_new();
    provide_context(store);
    provide_context(prefs);
    provide_router(Page::Overview, Vec::new(), frame)
}

fn nav_key(page: Page, current: Page) -> Element {
    let router = use_router::<Page>();
    view! {
        <button
            id={format!("nav-{}", page.label().to_lowercase())}
            class={if page == current { "nav-key on" } else { "nav-key" }}
            onclick={move || { router.push(page); }}
        >
            {page.label()}
        </button>
    }
}

fn frame() -> Element {
    let current = use_route::<Page>();
    let prefs = use_context::<Preferences>().expect("the app provides the preferences");
    let store = use_context::<TaskStore>().expect("the app provides the task store");
    let counts = counts(&store.tasks());

    let body = route_outlet(&current, |page| match page {
        Page::Overview => pages::overview::page(),
        Page::Tasks => pages::tasks::page(),
        Page::Settings => pages::settings::page(),
    });
    let keys: Vec<Element> = Page::ALL
        .iter()
        .map(|&page| nav_key(page, current))
        .collect();

    view! {
        <div class={prefs.desk_classes()} style={prefs.desk_style()}>
            <header class="lid glass-lid">
                <div class="brand">
                    <h1 class="title">{"Taskboard"}</h1>
                    <span class="label">{"A WORKSHOP DESK FOR THINGS TO DO"}</span>
                </div>
                <div class="lamps">
                    <span class="label">
                        {format!("{} OPEN", counts.todo + counts.doing + counts.blocked)}
                    </span>
                    <Led tone={Tone::Red} lit={counts.late > 0} />
                    <span class="label">{format!("{} LATE", counts.late)}</span>
                </div>
            </header>
            <div class="body">
                <nav class="rail leather">
                    <div class="stitching"></div>
                    {keys}
                </nav>
                <main class="surface">{body}</main>
            </div>
        </div>
    }
}
