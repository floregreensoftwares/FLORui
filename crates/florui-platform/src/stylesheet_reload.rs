//! What one reload of the watched stylesheet does with the file: reads it,
//! says whether it is a new text, and parses it. The window installs the rules
//! it is given and prints the reason of a failure.
//!
//! The file failing to be read (missing, locked, not UTF-8) is reported and the
//! window keeps the last good stylesheet. The text failing to parse cannot
//! happen: the parser follows CSS error recovery and never refuses a text
//! (`StyleError` has no variants), so a half-written file installs whatever of
//! it parsed, and the finished one, which reads differently, replaces it. An
//! emptied file is valid CSS and is installed.

use std::path::Path;

use florui_style::Rule;

use crate::desktop::RunError;

/// What a reload should do.
pub(crate) enum Reload {
    /// The file reads the same text as the one installed.
    Unchanged,
    /// A new stylesheet that parsed.
    Install { rules: Vec<Rule>, text: String },
    /// Why the file could not be used; the last good stylesheet stays.
    Failed(String),
}

impl std::fmt::Debug for Reload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reload::Unchanged => write!(f, "Unchanged"),
            Reload::Install { rules, text } => {
                write!(f, "Install({} rules, {} bytes)", rules.len(), text.len())
            }
            Reload::Failed(reason) => write!(f, "Failed({reason:?})"),
        }
    }
}

/// Whether `read` differs from the stylesheet text last installed. A second
/// event for the same save reads the same text, and installing it again would
/// repeat the whole update for nothing.
fn is_new_stylesheet(installed: Option<&str>, read: &str) -> bool {
    installed != Some(read)
}

pub(crate) fn load(path: &Path, installed: Option<&str>) -> Reload {
    let read = {
        let _span = florui_profile::span(florui_profile::Phase::StylesheetRead);
        std::fs::read_to_string(path)
    };
    let text = match read {
        Ok(text) if !is_new_stylesheet(installed, &text) => return Reload::Unchanged,
        Ok(text) => text,
        Err(error) => return Reload::Failed(RunError::CssFile(error).to_string()),
    };
    let parsed = {
        let _span = florui_profile::span(florui_profile::Phase::StylesheetParse);
        florui_style::parse_stylesheet(&text)
    };
    match parsed {
        Ok(rules) => Reload::Install { rules, text },
        Err(error) => Reload::Failed(RunError::Stylesheet(error).to_string()),
    }
}

#[cfg(test)]
mod tests {
    use florui::Element;

    use super::*;
    use crate::{HeadlessOptions, HeadlessWindow};

    const RED: &str = ".box { width: 40px; height: 40px; background-color: #c81e1e; }";
    const GREEN: &str = ".box { width: 40px; height: 40px; background-color: #1ec81e; }";
    /// The start of a save in progress: the rule opens and the color is not there yet.
    const HALF_WRITTEN: &str = ".box { width: 40px; height: 40px; background-color: ";

    fn window(css: &str) -> HeadlessWindow {
        HeadlessWindow::new(
            css,
            || Element::node("div", vec![("class".into(), "box".into())], Vec::new()),
            HeadlessOptions {
                width: 80.0,
                height: 80.0,
                ..HeadlessOptions::default()
            },
        )
        .expect("the stylesheet parses")
    }

    #[derive(Debug)]
    enum Applied {
        Unchanged,
        Installed,
        Failed(String),
    }

    /// What the window does with a reload: installs the rules and updates.
    fn apply(window: &mut HeadlessWindow, installed: &mut Option<String>, path: &Path) -> Applied {
        match load(path, installed.as_deref()) {
            Reload::Unchanged => Applied::Unchanged,
            Reload::Install { rules, text } => {
                window.runtime_mut().set_rules(rules);
                window.update();
                *installed = Some(text);
                Applied::Installed
            }
            Reload::Failed(reason) => Applied::Failed(reason),
        }
    }

    #[test]
    fn a_stylesheet_is_reinstalled_only_when_its_text_changed() {
        assert!(is_new_stylesheet(None, ".a {}"), "nothing installed yet");
        assert!(
            !is_new_stylesheet(Some(".a {}"), ".a {}"),
            "the same save, read twice"
        );
        assert!(
            is_new_stylesheet(Some(".a {}"), ".a { color: red; }"),
            "an edit"
        );
        assert!(
            is_new_stylesheet(Some(".a { color: red; }"), ""),
            "a half-written file is read, then the finished one replaces it"
        );
        assert!(
            is_new_stylesheet(Some(""), ".a { color: red; }"),
            "and an emptied file is a real change too"
        );
    }

    #[test]
    fn a_file_that_cannot_be_read_leaves_the_window_as_it_was_and_the_next_good_one_applies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.css");
        std::fs::write(&path, RED).unwrap();
        let mut window = window(RED);
        let mut installed = Some(RED.to_string());
        let red = window.frame().rgba;

        std::fs::remove_file(&path).unwrap();
        let missing = apply(&mut window, &mut installed, &path);
        assert!(
            matches!(&missing, Applied::Failed(m) if m.contains("could not read")),
            "{missing:?}"
        );
        assert_eq!(
            window.frame().rgba,
            red,
            "a missing file changed the window"
        );

        std::fs::write(&path, [0xff, 0xfe, 0x00, 0x80]).unwrap();
        let not_text = apply(&mut window, &mut installed, &path);
        assert!(matches!(not_text, Applied::Failed(_)), "{not_text:?}");
        assert_eq!(
            window.frame().rgba,
            red,
            "bytes that are not text changed the window"
        );
        assert_eq!(
            installed.as_deref(),
            Some(RED),
            "the last good text stays installed"
        );

        std::fs::write(&path, GREEN).unwrap();
        let next = apply(&mut window, &mut installed, &path);
        assert!(matches!(next, Applied::Installed), "{next:?}");
        assert_eq!(
            window.frame().rgba,
            self::window(GREEN).frame().rgba,
            "the next good file did not apply"
        );
    }

    #[test]
    fn a_half_written_file_installs_what_parsed_and_the_finished_one_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.css");
        let mut window = window(RED);
        let mut installed = Some(RED.to_string());
        let red = window.frame().rgba;
        let green = self::window(GREEN).frame().rgba;

        std::fs::write(&path, HALF_WRITTEN).unwrap();
        let during = apply(&mut window, &mut installed, &path);
        assert!(
            matches!(during, Applied::Installed),
            "the parser never refuses a text: {during:?}"
        );
        let half = window.frame().rgba;
        assert_ne!(half, red, "the half-written file left the old color");
        assert_ne!(
            half, green,
            "the half-written file already had the new color"
        );

        std::fs::write(&path, GREEN).unwrap();
        let after = apply(&mut window, &mut installed, &path);
        assert!(matches!(after, Applied::Installed), "{after:?}");
        assert_eq!(
            window.frame().rgba,
            green,
            "the finished file did not replace the half-written one"
        );
    }

    #[test]
    fn saving_the_same_text_again_is_not_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.css");
        std::fs::write(&path, GREEN).unwrap();
        let mut window = window(RED);
        let mut installed = Some(RED.to_string());

        let first = apply(&mut window, &mut installed, &path);
        let second = apply(&mut window, &mut installed, &path);

        assert!(matches!(first, Applied::Installed));
        assert!(matches!(second, Applied::Unchanged), "{second:?}");
    }

    #[test]
    fn an_emptied_file_is_installed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.css");
        std::fs::write(&path, "").unwrap();
        let mut window = window(RED);
        let mut installed = Some(RED.to_string());
        let red = window.frame().rgba;

        let outcome = apply(&mut window, &mut installed, &path);

        assert!(matches!(outcome, Applied::Installed), "{outcome:?}");
        assert_ne!(window.frame().rgba, red, "the box kept its color");
    }
}
