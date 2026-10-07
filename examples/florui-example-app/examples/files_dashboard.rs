//! A file-manager dashboard with hardcoded data, a custom title bar and a
//! translucent window (the desktop shows through the glass). The stylesheet
//! reloads when saved:
//! a sidebar with typed routes (one nested), stat and storage cards, a
//! file table with selection and a type filter, and a settings page with
//! switches.
//!
//! `cargo run --example files_dashboard -p florui-example-app`

use std::rc::Rc;

use florui::prelude::*;
use florui_platform::appearance::DecorationMode;
use florui_platform::{WINDOW_DRAG_REGION_ID, WindowOptions, use_window_controls};
use florui_reactive::executor::{Executor, LocalExecutor};
use florui_reactive::{Signal, provide_context, use_ref, use_signal};
use florui_routing::{Routable, RouteError, provide_router, route_outlet, use_route, use_router};
use florui_style::Rgba;

const CSS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/files_dashboard.css");

#[derive(Debug, Clone, PartialEq)]
enum Page {
    Overview,
    Files(Filter),
    Shared,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Filter {
    All,
    Documents,
    Images,
    Archives,
}

impl Filter {
    const ALL: [Filter; 4] = [
        Filter::All,
        Filter::Documents,
        Filter::Images,
        Filter::Archives,
    ];

    fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Documents => "Documents",
            Filter::Images => "Images",
            Filter::Archives => "Archives",
        }
    }

    fn path(self) -> &'static str {
        match self {
            Filter::All => "/all",
            Filter::Documents => "/documents",
            Filter::Images => "/images",
            Filter::Archives => "/archives",
        }
    }
}

impl Routable for Filter {
    fn parse(path: &str) -> Result<Self, RouteError> {
        Filter::ALL
            .into_iter()
            .find(|filter| filter.path() == path)
            .ok_or_else(|| RouteError::Unknown {
                path: path.to_owned(),
            })
    }

    fn format(&self) -> String {
        self.path().to_owned()
    }
}

impl Routable for Page {
    fn parse(path: &str) -> Result<Self, RouteError> {
        let unknown = || RouteError::Unknown {
            path: path.to_owned(),
        };
        match path {
            "/" => Ok(Page::Overview),
            "/shared" => Ok(Page::Shared),
            "/settings" => Ok(Page::Settings),
            "/files" => Ok(Page::Files(Filter::All)),
            _ => path
                .strip_prefix("/files")
                .ok_or_else(unknown)
                .and_then(|rest| Filter::parse(rest).map_err(|_| unknown()))
                .map(Page::Files),
        }
    }

    fn format(&self) -> String {
        match self {
            Page::Overview => "/".to_owned(),
            Page::Files(filter) => format!("/files{}", filter.path()),
            Page::Shared => "/shared".to_owned(),
            Page::Settings => "/settings".to_owned(),
        }
    }
}

struct File {
    name: &'static str,
    kind: Filter,
    size: &'static str,
    modified: &'static str,
    owner: &'static str,
}

const fn file(
    name: &'static str,
    kind: Filter,
    size: &'static str,
    modified: &'static str,
    owner: &'static str,
) -> File {
    File {
        name,
        kind,
        size,
        modified,
        owner,
    }
}

const FILES: [File; 10] = [
    file(
        "Quarterly report.pdf",
        Filter::Documents,
        "2.4 MB",
        "Today, 09:12",
        "Ana",
    ),
    file(
        "Brand assets.zip",
        Filter::Archives,
        "148 MB",
        "Today, 08:40",
        "Bruno",
    ),
    file(
        "Team photo.jpg",
        Filter::Images,
        "5.1 MB",
        "Yesterday",
        "Carla",
    ),
    file(
        "Budget 2027.xlsx",
        Filter::Documents,
        "860 KB",
        "Yesterday",
        "Ana",
    ),
    file("Logo final.png", Filter::Images, "312 KB", "Mon", "Diego"),
    file(
        "Backup March.tar.gz",
        Filter::Archives,
        "1.2 GB",
        "Mon",
        "Bruno",
    ),
    file(
        "Meeting notes.docx",
        Filter::Documents,
        "94 KB",
        "Sun",
        "Elisa",
    ),
    file("Wireframes.png", Filter::Images, "2.9 MB", "Sat", "Carla"),
    file(
        "Contract draft.pdf",
        Filter::Documents,
        "1.1 MB",
        "Fri",
        "Ana",
    ),
    file("Release 1.4.zip", Filter::Archives, "64 MB", "Thu", "Diego"),
];

const SHARED: [(&str, &str, &str); 4] = [
    ("Design system", "Carla, Diego", "12 files"),
    ("Finance", "Ana, Elisa", "8 files"),
    ("Releases", "Bruno, Diego", "21 files"),
    ("Onboarding", "Elisa", "5 files"),
];

const ACTIVITY: [(&str, &str); 5] = [
    ("Ana uploaded Quarterly report.pdf", "3 min ago"),
    ("Bruno shared Brand assets with Design", "1 h ago"),
    ("Carla edited Wireframes.png", "3 h ago"),
    ("Diego restored Logo final.png", "Yesterday"),
    ("Elisa commented on Meeting notes", "Yesterday"),
];

fn main() {
    florui_platform::run_with_css_reload_and_options(
        "Florui -- files",
        CSS_PATH,
        Rgba::TRANSPARENT,
        WindowOptions {
            decorations: DecorationMode::Custom,
            size: Some((1180.0, 760.0)),
            min_size: Some((900.0, 600.0)),
            ..WindowOptions::default()
        },
        root,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn root() -> Element {
    let executor = use_ref(|| Rc::new(LocalExecutor::new()) as Rc<dyn Executor>);
    provide_context(executor.get());
    provide_router(Page::Overview, Vec::new(), app)
}

fn nav_item(page: Page, label: &'static str, current: &Page) -> Element {
    let router = use_router::<Page>();
    let active = std::mem::discriminant(&page) == std::mem::discriminant(current);
    let target = page.clone();
    view! {
        <button
            id={format!("nav-{}", label.to_lowercase())}
            class={if active { "nav-item active" } else { "nav-item" }}
            onclick={move || { router.push(target.clone()); }}
        >
            {label}
        </button>
    }
}

fn app() -> Element {
    let current = use_route::<Page>();
    let selected = use_signal(|| Some(0usize));

    let body = route_outlet(&current, |page| match page {
        Page::Overview => overview(),
        Page::Files(filter) => files(*filter, selected.clone()),
        Page::Shared => shared(),
        Page::Settings => settings(),
    });

    let controls = use_window_controls();
    let maximized = controls.as_ref().is_some_and(|c| c.is_maximized());
    let focused = controls.as_ref().is_none_or(|c| c.is_focused());
    let minimize = controls.clone();
    let toggle_maximize = controls.clone();
    let close = controls.clone();

    view! {
        <div class="shell">
            <div class={if focused { "titlebar" } else { "titlebar inactive" }}>
                <div class="brand-mark"></div>
                <span class="brand-name">{"File manager"}</span>
                <span class="crumb">{current.format()}</span>
                <div id={WINDOW_DRAG_REGION_ID} class="drag-region" />
                <span class="avatar">{"AN"}</span>
                <div class="window-buttons">
                    <button
                        class="window-button"
                        accessible_label={"Minimize".to_string()}
                        onclick={move || if let Some(controls) = &minimize { controls.minimize(); }}
                    >
                        {"_"}
                    </button>
                    <button
                        class="window-button"
                        accessible_label={if maximized { "Restore" } else { "Maximize" }.to_string()}
                        onclick={move || if let Some(controls) = &toggle_maximize { controls.toggle_maximize(); }}
                    >
                        {if maximized { "[ ]" } else { "[]" }}
                    </button>
                    <button
                        class="window-button close"
                        accessible_label={"Close".to_string()}
                        onclick={move || if let Some(controls) = &close { controls.close(); }}
                    >
                        {"x"}
                    </button>
                </div>
            </div>
            <div class="app">
                <div class="sidebar surface">
                    {nav_item(Page::Overview, "Overview", &current)}
                    {nav_item(Page::Files(Filter::All), "Files", &current)}
                    {nav_item(Page::Shared, "Shared", &current)}
                    {nav_item(Page::Settings, "Settings", &current)}
                    <div class="sidebar-spacer"></div>
                    <div class="quota">
                        <span class="quota-label">{"68% of 500 GB used"}</span>
                        <div class="bar"><div class="bar-fill"></div></div>
                    </div>
                </div>
                <div class="content">{body}</div>
            </div>
        </div>
    }
}

fn stat(label: &'static str, value: &'static str, delta: &'static str) -> Element {
    view! {
        <div class="card surface stat">
            <span class="stat-label">{label}</span>
            <span class="stat-value">{value}</span>
            <span class="stat-delta">{delta}</span>
        </div>
    }
}

fn overview() -> Element {
    view! {
        <div class="page">
            <h1 class="title">{"Overview"}</h1>
            <div class="row">
                {stat("Files", "12,408", "+128 this week")}
                {stat("Storage", "341 GB", "+6.2 GB")}
                {stat("Shared", "46", "4 new")}
                {stat("Trash", "312 MB", "auto-clears in 9d")}
            </div>
            <div class="row grow">
                <div class="card surface wide">
                    <h2 class="card-title">{"Recent files"}</h2>
                    {FILES.iter().take(5).map(|file| view! {
                        <div class="line">
                            <span class="line-name">{file.name}</span>
                            <span class="muted">{file.modified}</span>
                        </div>
                    }).collect::<Vec<_>>()}
                </div>
                <div class="card surface side">
                    <h2 class="card-title">{"Activity"}</h2>
                    {ACTIVITY.iter().map(|(what, when)| view! {
                        <div class="activity">
                            <span>{*what}</span>
                            <span class="muted">{*when}</span>
                        </div>
                    }).collect::<Vec<_>>()}
                </div>
            </div>
        </div>
    }
}

fn files(filter: Filter, selected: Signal<Option<usize>>) -> Element {
    let router = use_router::<Page>();
    let chips = Filter::ALL
        .iter()
        .map(|&option| {
            let router = router.clone();
            view! {
                <button
                    class={if option == filter { "chip on" } else { "chip" }}
                    onclick={move || { router.push(Page::Files(option)); }}
                >
                    {option.label()}
                </button>
            }
        })
        .collect::<Vec<_>>();
    let rows = FILES
        .iter()
        .enumerate()
        .filter(|(_, file)| filter == Filter::All || file.kind == filter)
        .map(|(index, file)| {
            let selected_now = selected.get() == Some(index);
            let selected = selected.clone();
            view! {
                <div
                    class={if selected_now { "file selected" } else { "file" }}
                    onclick={move || selected.set(Some(index))}
                >
                    <span class="file-name">{file.name}</span>
                    <span class="file-col">{file.owner}</span>
                    <span class="file-col">{file.size}</span>
                    <span class="file-col muted">{file.modified}</span>
                </div>
            }
        })
        .collect::<Vec<_>>();
    let detail = selected
        .get()
        .and_then(|index| FILES.get(index))
        .map(|file| {
            view! {
                <div class="card surface detail">
                    <h2 class="card-title">{file.name}</h2>
                    <span class="muted">{format!("{} - {}", file.size, file.owner)}</span>
                    <span class="muted">{format!("Modified {}", file.modified)}</span>
                    <button class="primary">{"Download"}</button>
                </div>
            }
        });
    view! {
        <div class="page">
            <h1 class="title">{"Files"}</h1>
            <div class="chips">{chips}</div>
            <div class="row grow">
                <div class="card surface wide table">{rows}</div>
                {detail}
            </div>
        </div>
    }
}

fn shared() -> Element {
    view! {
        <div class="page">
            <h1 class="title">{"Shared"}</h1>
            <div class="row wrap">
                {SHARED.iter().map(|(name, people, count)| view! {
                    <div class="card surface folder">
                        <span class="folder-name">{*name}</span>
                        <span class="muted">{*people}</span>
                        <span class="muted">{*count}</span>
                    </div>
                }).collect::<Vec<_>>()}
            </div>
        </div>
    }
}

fn setting(label: &'static str, hint: &'static str, on: Signal<bool>) -> Element {
    let toggle = on.clone();
    view! {
        <div class="setting">
            <div class="setting-text">
                <span>{label}</span>
                <span class="muted">{hint}</span>
            </div>
            <button
                class={if on.get() { "switch on" } else { "switch" }}
                onclick={move || toggle.set(!toggle.get())}
            >
                <div class="knob"></div>
            </button>
        </div>
    }
}

fn settings() -> Element {
    let sync = use_signal(|| true);
    let previews = use_signal(|| true);
    let notify = use_signal(|| false);
    view! {
        <div class="page">
            <h1 class="title">{"Settings"}</h1>
            <div class="card surface settings">
                {setting("Sync automatically", "Keep this device up to date", sync)}
                {setting("Generate previews", "Thumbnails for images and documents", previews)}
                {setting("Email notifications", "A weekly summary of activity", notify)}
            </div>
        </div>
    }
}
