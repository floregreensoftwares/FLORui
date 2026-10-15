//! Open and save dialogs through the desktop portal's file chooser, which is
//! the desktop's own dialog (GNOME, KDE, ...) and is parented to the window.
//! `rfd` returns no answer both for "cancelled" and for "no portal running",
//! so the portal's presence is checked up front.

use std::path::Path;

use rfd::FileDialog;
use winit::window::Window;

use crate::file_dialog::{
    FileDialogFilter, OpenFileDialogOptions, OpenFileDialogOutcome, SaveFileDialogOptions,
    SaveFileDialogOutcome,
};

const PORTAL_MARKERS: [&str; 2] = [
    "/usr/share/dbus-1/services/org.freedesktop.portal.Desktop.service",
    "/usr/share/xdg-desktop-portal",
];

fn portal_installed(markers: &[&str]) -> bool {
    markers.iter().any(|marker| Path::new(marker).exists())
}

fn with_filters(mut dialog: FileDialog, filters: &[FileDialogFilter]) -> FileDialog {
    for filter in filters {
        dialog = dialog.add_filter(filter.label.as_str(), &filter.extensions);
    }
    dialog
}

fn base(window: &Window, title: Option<&str>, directory: Option<&Path>) -> FileDialog {
    let mut dialog = FileDialog::new().set_parent(window);
    if let Some(title) = title {
        dialog = dialog.set_title(title);
    }
    if let Some(directory) = directory {
        dialog = dialog.set_directory(directory);
    }
    dialog
}

pub(crate) fn spawn_open_dialog(
    window: &Window,
    options: OpenFileDialogOptions,
    on_complete: impl Fn(OpenFileDialogOutcome) + Send + 'static,
) {
    if !portal_installed(&PORTAL_MARKERS) {
        on_complete(OpenFileDialogOutcome::Unavailable);
        return;
    }
    let dialog = with_filters(
        base(
            window,
            options.title.as_deref(),
            options.starting_directory.as_deref(),
        ),
        &options.filters,
    );
    std::thread::spawn(move || {
        let picked = if options.allow_multiple {
            dialog.pick_files()
        } else {
            dialog.pick_file().map(|path| vec![path])
        };
        on_complete(match picked {
            Some(paths) if !paths.is_empty() => OpenFileDialogOutcome::Selected(paths),
            _ => OpenFileDialogOutcome::Cancelled,
        });
    });
}

pub(crate) fn spawn_save_dialog(
    window: &Window,
    options: SaveFileDialogOptions,
    on_complete: impl Fn(SaveFileDialogOutcome) + Send + 'static,
) {
    if !portal_installed(&PORTAL_MARKERS) {
        on_complete(SaveFileDialogOutcome::Unavailable);
        return;
    }
    let mut dialog = with_filters(
        base(
            window,
            options.title.as_deref(),
            options.starting_directory.as_deref(),
        ),
        &options.filters,
    );
    if let Some(name) = &options.suggested_file_name {
        dialog = dialog.set_file_name(name.as_str());
    }
    std::thread::spawn(move || {
        on_complete(match dialog.save_file() {
            Some(path) => SaveFileDialogOutcome::Selected(path),
            None => SaveFileDialogOutcome::Cancelled,
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_portal_counts_as_installed_when_any_marker_exists() {
        let dir = std::env::temp_dir();
        let present = dir.to_string_lossy().into_owned();
        assert!(portal_installed(&["/no/such/marker", &present]));
    }

    #[test]
    fn no_marker_means_no_portal() {
        assert!(!portal_installed(&["/no/such/marker", "/also/missing"]));
        assert!(!portal_installed(&[]));
    }
}
