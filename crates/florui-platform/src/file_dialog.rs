//! Native open/save file dialogs — see
//! [`crate::os::windows::file_dialog`] for the real implementation.
//! `spawn_open_dialog`/`spawn_save_dialog` call back with
//! [`OpenFileDialogOutcome::Unavailable`]/[`SaveFileDialogOutcome::Unavailable`]
//! immediately on any other platform: no equivalent mechanism exists yet.
//! Apps never call these two directly -- see
//! [`crate::WindowControls::open_file_dialog`]/
//! [`crate::WindowControls::save_file_dialog`] for the public entry
//! point.

use std::path::PathBuf;

/// One filter entry a dialog offers in its file-type dropdown.
/// `extensions` never carry a leading dot, matching
/// `florui_config::FileAssociationConfig::extension`'s own convention.
#[derive(Debug, Clone, PartialEq)]
pub struct FileDialogFilter {
    pub label: String,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpenFileDialogOptions {
    pub title: Option<String>,
    pub filters: Vec<FileDialogFilter>,
    pub allow_multiple: bool,
    pub starting_directory: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SaveFileDialogOptions {
    pub title: Option<String>,
    pub filters: Vec<FileDialogFilter>,
    pub suggested_file_name: Option<String>,
    pub starting_directory: Option<PathBuf>,
}

/// `Selected` carries one or more paths (`allow_multiple` requests more
/// than one; the dialog may still only return one) -- never empty.
/// Selecting a file does not open, read, or authorize executing it.
#[derive(Debug, Clone, PartialEq)]
pub enum OpenFileDialogOutcome {
    Selected(Vec<PathBuf>),
    Cancelled,
    /// No equivalent mechanism exists on this platform, or the dialog
    /// itself could not be created (e.g. COM initialization failed, or no
    /// real platform window handle was available).
    Unavailable,
    /// A real, unexpected failure -- the message is for diagnostics only,
    /// never a selected path.
    Failed(String),
}

/// Selecting a save destination is not writing the file -- the caller
/// still owns actually creating/writing it.
#[derive(Debug, Clone, PartialEq)]
pub enum SaveFileDialogOutcome {
    Selected(PathBuf),
    Cancelled,
    Unavailable,
    Failed(String),
}

/// Joins each filter's extensions into the `*.ext1;*.ext2` pattern a
/// Windows file-type dropdown expects, alongside its own label -- pure
/// string building, kept separate from the real dialog's wide-string/
/// pointer plumbing so it's testable without any real OS type. An empty
/// `extensions` list joins to an empty pattern (matches nothing) rather
/// than panicking or matching everything -- a filter entry with nothing
/// to filter on is a caller mistake this function doesn't need an opinion
/// about beyond not crashing.
#[cfg(any(windows, test))]
pub(crate) fn filter_patterns(filters: &[FileDialogFilter]) -> Vec<(String, String)> {
    filters
        .iter()
        .map(|filter| {
            let pattern = filter
                .extensions
                .iter()
                .map(|extension| format!("*.{extension}"))
                .collect::<Vec<_>>()
                .join(";");
            (filter.label.clone(), pattern)
        })
        .collect()
}

#[cfg(target_os = "windows")]
pub(crate) use crate::os::windows::file_dialog::{spawn_open_dialog, spawn_save_dialog};

#[cfg(not(target_os = "windows"))]
pub(crate) use stub::{spawn_open_dialog, spawn_save_dialog};

#[cfg(not(target_os = "windows"))]
mod stub {
    use winit::window::Window;

    use super::{
        OpenFileDialogOptions, OpenFileDialogOutcome, SaveFileDialogOptions, SaveFileDialogOutcome,
    };

    pub(crate) fn spawn_open_dialog(
        _window: &Window,
        _options: OpenFileDialogOptions,
        on_complete: impl Fn(OpenFileDialogOutcome) + Send + 'static,
    ) {
        on_complete(OpenFileDialogOutcome::Unavailable);
    }

    pub(crate) fn spawn_save_dialog(
        _window: &Window,
        _options: SaveFileDialogOptions,
        on_complete: impl Fn(SaveFileDialogOutcome) + Send + 'static,
    ) {
        on_complete(SaveFileDialogOutcome::Unavailable);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_filters_produces_no_patterns() {
        assert_eq!(filter_patterns(&[]), Vec::new());
    }

    #[test]
    fn a_single_extension_becomes_one_glob() {
        let filters = [FileDialogFilter {
            label: "PNG".to_owned(),
            extensions: vec!["png".to_owned()],
        }];
        assert_eq!(
            filter_patterns(&filters),
            vec![("PNG".to_owned(), "*.png".to_owned())]
        );
    }

    #[test]
    fn multiple_extensions_join_with_semicolons() {
        let filters = [FileDialogFilter {
            label: "Images".to_owned(),
            extensions: vec!["png".to_owned(), "jpg".to_owned()],
        }];
        assert_eq!(
            filter_patterns(&filters),
            vec![("Images".to_owned(), "*.png;*.jpg".to_owned())]
        );
    }

    #[test]
    fn multiple_filters_preserve_order_and_labels() {
        let filters = [
            FileDialogFilter {
                label: "Images".to_owned(),
                extensions: vec!["png".to_owned()],
            },
            FileDialogFilter {
                label: "Text".to_owned(),
                extensions: vec!["txt".to_owned(), "md".to_owned()],
            },
        ];
        assert_eq!(
            filter_patterns(&filters),
            vec![
                ("Images".to_owned(), "*.png".to_owned()),
                ("Text".to_owned(), "*.txt;*.md".to_owned()),
            ]
        );
    }

    #[test]
    fn a_filter_with_no_extensions_produces_an_empty_pattern() {
        let filters = [FileDialogFilter {
            label: "Nothing".to_owned(),
            extensions: vec![],
        }];
        assert_eq!(
            filter_patterns(&filters),
            vec![("Nothing".to_owned(), String::new())]
        );
    }
}
