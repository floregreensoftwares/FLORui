//! Which fixtures `florui compare` runs: names under the project's fixtures
//! root, or paths, with one clear error when a name matches nothing.

use std::path::{Path, PathBuf};

/// Resolves what was asked for against `available` (the fixture directories
/// under the root, sorted). Nothing asked for selects them all. A value is a
/// name under `root` first, then a directory with a `manifest.json` at that
/// path relative to `cwd`. Each fixture appears once, in the order asked.
pub fn select(
    root: &Path,
    available: &[PathBuf],
    requested: &[String],
    cwd: &Path,
) -> Result<Vec<PathBuf>, String> {
    if requested.is_empty() {
        if available.is_empty() {
            return Err(format!("no reference fixtures in {}", root.display()));
        }
        return Ok(available.to_vec());
    }
    let mut selected: Vec<PathBuf> = Vec::new();
    for value in requested {
        let by_name = root.join(value);
        let by_path = cwd.join(value);
        let found = [by_name, by_path]
            .into_iter()
            .find(|path| path.join("manifest.json").is_file());
        let Some(found) = found else {
            let names: Vec<String> = available
                .iter()
                .filter_map(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .collect();
            return Err(format!(
                "no fixture \"{value}\" under {}; available: {}",
                root.display(),
                if names.is_empty() {
                    "none".to_string()
                } else {
                    names.join(", ")
                }
            ));
        };
        if !selected.contains(&found) {
            selected.push(found);
        }
    }
    Ok(selected)
}

/// Opens a directory in the system file manager. Only starts it; whether the
/// window appears is the shell's business.
pub fn open_directory(dir: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        std::process::Command::new("explorer")
            .arg(dir)
            .spawn()
            .map(|_| ())
            .map_err(|err| format!("could not open {}: {err}", dir.display()))
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        Err("--open is not supported on this platform".to_string())
    }
}

/// `canonicalize` on Windows returns a `\\?\C:\...` path; show and open the
/// ordinary form when the verbatim prefix adds nothing.
pub fn plain(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), "{}").unwrap();
        dir
    }

    #[test]
    fn nothing_asked_for_selects_all_and_an_empty_root_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let a = fixture(tmp.path(), "a");
        let b = fixture(tmp.path(), "b");
        let all = vec![a, b];
        assert_eq!(select(tmp.path(), &all, &[], tmp.path()).unwrap(), all);
        assert!(select(tmp.path(), &[], &[], tmp.path()).is_err());
    }

    #[test]
    fn a_name_resolves_under_the_root_not_the_working_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let a = fixture(&root, "a");
        let available = vec![a.clone()];
        let selected = select(&root, &available, &["a".into()], &elsewhere);
        assert_eq!(selected.unwrap(), vec![a]);
    }

    #[test]
    fn a_path_relative_to_the_working_directory_is_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let own = fixture(tmp.path(), "mine");
        let root = tmp.path().join("root");
        let selected = select(&root, &[], &["mine".into()], tmp.path());
        assert_eq!(selected.unwrap(), vec![own]);
    }

    #[test]
    fn an_unknown_name_lists_what_is_available() {
        let tmp = tempfile::tempdir().unwrap();
        let a = fixture(tmp.path(), "alpha");
        let b = fixture(tmp.path(), "beta");
        let err = select(tmp.path(), &[a, b], &["gamma".into()], tmp.path()).unwrap_err();
        assert!(
            err.contains("\"gamma\"") && err.contains("alpha, beta"),
            "{err}"
        );
    }

    #[test]
    fn the_verbatim_prefix_is_dropped_only_from_drive_paths() {
        assert_eq!(
            plain(PathBuf::from(r"\\?\C:\a\b")),
            PathBuf::from(r"C:\a\b")
        );
        assert_eq!(
            plain(PathBuf::from(r"\\?\UNC\host\share")),
            PathBuf::from(r"\\?\UNC\host\share")
        );
        assert_eq!(plain(PathBuf::from("/a/b")), PathBuf::from("/a/b"));
    }

    #[test]
    fn a_fixture_asked_for_twice_runs_once_in_the_order_asked() {
        let tmp = tempfile::tempdir().unwrap();
        let a = fixture(tmp.path(), "a");
        let b = fixture(tmp.path(), "b");
        let asked = ["b".to_string(), "a".to_string(), "b".to_string()];
        let selected = select(tmp.path(), &[a.clone(), b.clone()], &asked, tmp.path());
        assert_eq!(selected.unwrap(), vec![b, a]);
    }
}
