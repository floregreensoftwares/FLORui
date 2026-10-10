//! What `florui dev` runs: the same application, reloading its stylesheets
//! whenever one of them is saved, without losing what the person was doing.
//!
//! The window watches one file, and this application has one stylesheet per
//! module, so this keeps a bundle file up to date: it writes every sheet, in
//! cascade order, to `target/dev/taskboard.css` and rewrites it when any of the
//! sources changes. The window watches the bundle.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use florui_platform::WindowOptions;
use florui_style::Rgba;
use taskboard::FLORUI_STYLESHEETS;
use taskboard::app::{App, AppProps};

/// Where each stylesheet lives on disk, in cascade order. The collected sheets
/// name the Rust file that declares them and the path they wrote, as
/// `package:file:path`.
fn sources(root: &Path) -> Vec<PathBuf> {
    FLORUI_STYLESHEETS
        .iter()
        .filter_map(|sheet| {
            let (_, rest) = sheet.id.split_once(':')?;
            let (file, literal) = rest.rsplit_once(':')?;
            let declared_in = root.join(file.replace('\\', "/"));
            Some(declared_in.parent()?.join(literal))
        })
        .collect()
}

fn bundle(files: &[PathBuf]) -> String {
    files
        .iter()
        .map(|file| std::fs::read_to_string(file).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

fn stamp(files: &[PathBuf]) -> Vec<Option<SystemTime>> {
    files
        .iter()
        .map(|file| {
            std::fs::metadata(file)
                .and_then(|meta| meta.modified())
                .ok()
        })
        .collect()
}

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let files = sources(&root);
    let out_dir = root.join("target/dev");
    std::fs::create_dir_all(&out_dir).expect("a directory for the bundle");
    let bundle_path = out_dir.join("taskboard.css");
    std::fs::write(&bundle_path, bundle(&files)).expect("the first bundle");

    // Rewrite the bundle when a source changes; the window sees the bundle change.
    let watched = files.clone();
    let target = bundle_path.clone();
    std::thread::spawn(move || {
        let mut seen = stamp(&watched);
        loop {
            std::thread::sleep(Duration::from_millis(250));
            let now = stamp(&watched);
            if now != seen {
                seen = now;
                let temporary = target.with_extension("css.tmp");
                if std::fs::write(&temporary, bundle(&watched)).is_ok() {
                    let _ = std::fs::rename(&temporary, &target);
                }
            }
        }
    });

    let options = WindowOptions {
        size: Some((1180.0, 760.0)),
        min_size: Some((900.0, 600.0)),
        ..WindowOptions::default()
    };
    if let Err(error) = florui_platform::run_with_css_reload_and_options(
        "Taskboard",
        &bundle_path,
        Rgba::opaque(0x2a, 0x1c, 0x12),
        options,
        || App(AppProps {}),
    ) {
        eprintln!("taskboard: {error}");
        std::process::exit(1);
    }
}
