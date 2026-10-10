//! Where the screens are: three of them, each one path.

use florui_routing::{Routable, RouteError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Overview,
    Tasks,
    Settings,
}

impl Page {
    pub const ALL: [Page; 3] = [Page::Overview, Page::Tasks, Page::Settings];

    pub fn label(self) -> &'static str {
        match self {
            Page::Overview => "Desk",
            Page::Tasks => "Stack",
            Page::Settings => "Settings",
        }
    }
}

impl Routable for Page {
    fn parse(path: &str) -> Result<Self, RouteError> {
        match path {
            "/" => Ok(Page::Overview),
            "/tasks" => Ok(Page::Tasks),
            "/settings" => Ok(Page::Settings),
            _ => Err(RouteError::Unknown {
                path: path.to_owned(),
            }),
        }
    }

    fn format(&self) -> String {
        match self {
            Page::Overview => "/",
            Page::Tasks => "/tasks",
            Page::Settings => "/settings",
        }
        .to_owned()
    }
}
