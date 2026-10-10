//! Taskboard: a task manager laid out as a small workshop desk.
//!
//! The tree is arranged so that each kind of thing has one home:
//!
//! - `theme/`: the tokens (colors, light, radii), the surfaces built from them
//!   (wood, leather, brass, paper, glass) and the type. The only place a color
//!   is written down.
//! - `components/`: the physical parts (a switch, a knob, a lamp, a key, a
//!   sheet of paper). Each is one folder with its markup and its stylesheet,
//!   knows nothing about tasks, and is tested by itself.
//! - `features/`: what the application does (the tasks, the settings, the
//!   overview), with the state it owns. A feature uses components and never
//!   another feature's insides.
//! - `pages/`: one screen each, which only arrange features.
//! - `routes.rs` and `app.rs`: where the screens are and the frame around them.

// The stylesheets are collected in the order the modules are declared here, so
// the theme comes first and what is built on it after.
pub mod theme;

pub mod components;
pub mod features;
pub mod pages;

pub mod app;
pub mod routes;

include!(concat!(env!("OUT_DIR"), "/florui_stylesheets.rs"));

/// Every collected stylesheet, in cascade order, as one sheet.
pub fn stylesheet() -> String {
    FLORUI_STYLESHEETS
        .iter()
        .map(|sheet| sheet.css)
        .collect::<Vec<_>>()
        .join("\n")
}
