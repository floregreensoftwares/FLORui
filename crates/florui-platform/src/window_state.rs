//! Opt-in window-bounds persistence: versioned JSON under
//! `%LOCALAPPDATA%\<app identifier>\window-state\<key>.json`. `LOCALAPPDATA`
//! (not `APPDATA`) because this is host-local, non-roaming state, and it's
//! already per-Windows-user by construction -- no separate user component
//! needed anywhere in the path. Windows-only for now; a macOS/Linux app-data
//! root is a future adapter's problem, not this module's.
//!
//! A corrupted, unreadable, or version-mismatched file is never fatal --
//! [`load_or_default`] always degrades to `None` (configured defaults),
//! logging one `eprintln!` the same way this crate's other non-fatal
//! failures already do (see `desktop.rs`'s icon-load/CSS-reload handling).

use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use winit::window::Window;

pub(crate) const SUPPORTED_WINDOW_STATE_VERSION: u32 = 1;

/// Normal/restored bounds only -- `maximized` is a separate flag so a
/// maximized session never clobbers the pre-maximize geometry that should
/// come back on a later non-maximized launch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PersistedWindowState {
    pub(crate) version: u32,
    pub(crate) logical_size: (f64, f64),
    pub(crate) position: Option<(i32, i32)>,
    /// Save-time context only, for the revalidation math -- never applied
    /// directly to a restored window.
    pub(crate) monitor: MonitorContext,
    pub(crate) maximized: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct MonitorContext {
    /// A soft diagnostic hint only -- a renamed/reordered/reconnected
    /// monitor is a normal event, not something this name is ever matched
    /// against.
    pub(crate) name: Option<String>,
    pub(crate) position: (i32, i32),
    pub(crate) physical_size: (u32, u32),
    pub(crate) scale_factor: f64,
}

#[derive(Debug)]
enum WindowStateError {
    Io(std::io::Error),
    Decode(serde_json::Error),
    UnsupportedVersion { found: u32, supported: u32 },
}

impl std::fmt::Display for WindowStateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WindowStateError::Io(error) => write!(f, "{error}"),
            WindowStateError::Decode(error) => write!(f, "{error}"),
            WindowStateError::UnsupportedVersion { found, supported } => write!(
                f,
                "unsupported version {found} (this build supports {supported})"
            ),
        }
    }
}

/// `%LOCALAPPDATA%\<app_identifier>\window-state\<key>.json` -- `None` when
/// `LOCALAPPDATA` itself isn't set (never expected for a normally-launched
/// desktop session; treated the same as "persistence unavailable" by every
/// caller, not a panic).
pub(crate) fn window_state_path(app_identifier: &str, key: &str) -> Option<PathBuf> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")?;
    Some(
        PathBuf::from(local_app_data)
            .join(app_identifier)
            .join("window-state")
            .join(format!("{key}.json")),
    )
}

fn read_and_parse(path: &Path) -> Result<PersistedWindowState, WindowStateError> {
    let contents = std::fs::read_to_string(path).map_err(WindowStateError::Io)?;
    let state: PersistedWindowState =
        serde_json::from_str(&contents).map_err(WindowStateError::Decode)?;
    if state.version != SUPPORTED_WINDOW_STATE_VERSION {
        return Err(WindowStateError::UnsupportedVersion {
            found: state.version,
            supported: SUPPORTED_WINDOW_STATE_VERSION,
        });
    }
    Ok(state)
}

/// Never returns an error at this boundary -- a missing file (first run,
/// or after [`reset_window_state`](crate::reset_window_state)) is silently
/// `None`; any other failure (I/O, corrupt JSON, unsupported version) is
/// also `None`, but logged once, since that case is worth a developer
/// noticing even though it's not fatal.
pub(crate) fn load_or_default(path: &Path) -> Option<PersistedWindowState> {
    match read_and_parse(path) {
        Ok(state) => Some(state),
        Err(WindowStateError::Io(error)) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => {
            eprintln!(
                "florui-platform: window state at {} could not be used ({error}), using configured defaults",
                path.display()
            );
            None
        }
    }
}

/// Writes `state` to a sibling `.tmp` file, then renames it over `path` --
/// `std::fs::rename` on Windows already overwrites an existing destination
/// (confirmed with a standalone experiment, including with the destination
/// held open by another handle), so no `MoveFileExW` fallback is needed.
/// Same-directory, same-volume rename is atomic: a reader never observes a
/// partially-written file, only the old complete one or the new complete
/// one.
pub(crate) fn save(path: &Path, state: &PersistedWindowState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(state)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    std::fs::write(&tmp_path, json)?;
    std::fs::rename(&tmp_path, path)
}

/// The plain, already-resolved values an app passes in to opt one window
/// into bounds persistence -- deliberately not `florui-config`'s own
/// `WindowPersistenceConfig`/`AppConfig`, so this crate never gains a
/// `florui-config` dependency; the app resolves both itself and hands over
/// plain strings.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowPersistence {
    pub app_identifier: String,
    pub key: String,
}

/// A monitor's bounds in physical pixels plus its scale factor -- the
/// plain shape a real `winit::monitor::MonitorHandle` gets mapped into
/// once at the call site, so the actual revalidation math below stays a
/// pure, directly testable function (`MonitorHandle` has no public
/// constructor, so it can't be fabricated in a unit test).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MonitorRect {
    pub(crate) position: (i32, i32),
    pub(crate) physical_size: (u32, u32),
    pub(crate) scale_factor: f64,
}

/// A saved rect needs at least this many physical pixels of overlap on
/// each axis with a connected monitor to count as "still reachable" -- a
/// token one-pixel touch at an edge doesn't; this is "revalidate against
/// currently connected monitors," not "any pixel in common."
const MIN_OVERLAP_PIXELS: i64 = 32;

/// Finds a connected monitor whose bounds meaningfully overlap the
/// persisted logical size/position (converted to physical pixels via the
/// *persisted* scale factor -- the only scale this rect was ever actually
/// laid out against). `None` means a full miss: the caller falls back to
/// its own configured defaults entirely rather than partially honoring an
/// unreachable position, so a title/drag region is never left off-screen.
pub(crate) fn overlapping_monitor(
    logical_size: (f64, f64),
    position: (i32, i32),
    saved_scale_factor: f64,
    monitors: &[MonitorRect],
) -> Option<MonitorRect> {
    let physical_width = (logical_size.0 * saved_scale_factor) as i64;
    let physical_height = (logical_size.1 * saved_scale_factor) as i64;
    let (x0, y0) = (position.0 as i64, position.1 as i64);
    let (x1, y1) = (x0 + physical_width, y0 + physical_height);
    monitors.iter().copied().find(|monitor| {
        let (mx0, my0) = (monitor.position.0 as i64, monitor.position.1 as i64);
        let (mx1, my1) = (
            mx0 + monitor.physical_size.0 as i64,
            my0 + monitor.physical_size.1 as i64,
        );
        let overlap_x = x1.min(mx1) - x0.max(mx0);
        let overlap_y = y1.min(my1) - y0.max(my0);
        overlap_x >= MIN_OVERLAP_PIXELS && overlap_y >= MIN_OVERLAP_PIXELS
    })
}

/// A restored size must never come back smaller than what the app itself
/// configured as its floor.
pub(crate) fn clamp_to_min(size: (f64, f64), min_size: Option<(f64, f64)>) -> (f64, f64) {
    match min_size {
        Some((min_width, min_height)) => (size.0.max(min_width), size.1.max(min_height)),
        None => size,
    }
}

/// Reads `window`'s current live geometry and writes it unconditionally --
/// used for the close-time flush, and (once debouncing lands) a debounced
/// periodic save. Does nothing if the window's current monitor can't be
/// determined (a transient state not worth saving against) or if
/// `LOCALAPPDATA` itself is unavailable -- both silent, matching
/// [`load_or_default`]'s own "unavailable is not a diagnostic" treatment,
/// since neither is a corruption.
pub(crate) fn capture_and_save(window: &Window, persistence: &WindowPersistence) {
    let Some(path) = window_state_path(&persistence.app_identifier, &persistence.key) else {
        return;
    };
    let Some(monitor) = window.current_monitor() else {
        return;
    };
    let scale_factor = window.scale_factor();
    let logical_size = window.inner_size().to_logical::<f64>(scale_factor);
    let position = window
        .outer_position()
        .ok()
        .map(|position| (position.x, position.y));
    let state = PersistedWindowState {
        version: SUPPORTED_WINDOW_STATE_VERSION,
        logical_size: (logical_size.width, logical_size.height),
        position,
        monitor: MonitorContext {
            name: monitor.name(),
            position: (monitor.position().x, monitor.position().y),
            physical_size: (monitor.size().width, monitor.size().height),
            scale_factor: monitor.scale_factor(),
        },
        maximized: window.is_maximized(),
    };
    if let Err(error) = save(&path, &state) {
        eprintln!(
            "florui-platform: could not save window state to {}: {error}",
            path.display()
        );
    }
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Deletes any persisted window state for `persistence`, if it exists.
/// Idempotent -- a file that never existed is not an error. Independent of
/// whether any window using this key is currently open, so an app-level
/// "restore default layout" action works regardless of what's live.
pub fn reset_window_state(persistence: &WindowPersistence) -> std::io::Result<()> {
    match window_state_path(&persistence.app_identifier, &persistence.key) {
        Some(path) => remove_if_present(&path),
        None => Ok(()),
    }
}

/// A dedicated key `florui doctor`'s own capability probe writes under --
/// never a real window's own key, so this can never collide with (or
/// disturb) anything actually persisted.
const PROBE_KEY: &str = "florui-doctor-probe";

/// Real, observed evidence -- not an assumption from `LOCALAPPDATA` merely
/// being set -- that a window-state file can actually be written to and
/// removed from the OS application-data location for `app_identifier`.
/// Used by `florui doctor` to report actual capability rather than
/// inferring it from configuration alone; leaves nothing behind on either
/// success or failure.
pub fn probe_persistence_capability(app_identifier: &str) -> bool {
    let Some(path) = window_state_path(app_identifier, PROBE_KEY) else {
        return false;
    };
    let probe_state = PersistedWindowState {
        version: SUPPORTED_WINDOW_STATE_VERSION,
        logical_size: (1.0, 1.0),
        position: None,
        monitor: MonitorContext {
            name: None,
            position: (0, 0),
            physical_size: (1, 1),
            scale_factor: 1.0,
        },
        maximized: false,
    };
    let wrote = save(&path, &probe_state).is_ok();
    let _ = remove_if_present(&path);
    wrote
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_state() -> PersistedWindowState {
        PersistedWindowState {
            version: SUPPORTED_WINDOW_STATE_VERSION,
            logical_size: (1024.0, 768.0),
            position: Some((120, 80)),
            monitor: MonitorContext {
                name: Some("\\\\.\\DISPLAY1".to_owned()),
                position: (0, 0),
                physical_size: (1920, 1080),
                scale_factor: 1.25,
            },
            maximized: false,
        }
    }

    #[test]
    fn save_then_load_round_trips_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.json");
        let state = sample_state();
        save(&path, &state).unwrap();
        let loaded = load_or_default(&path).unwrap();
        assert_eq!(loaded, state);
    }

    #[test]
    fn missing_file_is_none_without_a_diagnostic_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");
        assert_eq!(load_or_default(&path), None);
    }

    #[test]
    fn corrupted_json_falls_back_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.json");
        std::fs::write(&path, "{ not valid json").unwrap();
        assert_eq!(load_or_default(&path), None);
    }

    #[test]
    fn unsupported_version_falls_back_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.json");
        let mut state = sample_state();
        state.version = SUPPORTED_WINDOW_STATE_VERSION + 1;
        let json = serde_json::to_string_pretty(&state).unwrap();
        std::fs::write(&path, json).unwrap();
        assert_eq!(load_or_default(&path), None);
    }

    #[test]
    fn save_overwrites_a_previous_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.json");
        save(&path, &sample_state()).unwrap();
        let mut second = sample_state();
        second.logical_size = (640.0, 480.0);
        save(&path, &second).unwrap();
        assert_eq!(load_or_default(&path).unwrap(), second);
    }

    #[test]
    fn distinct_keys_never_alias_the_same_path() {
        let a = window_state_path("com.floregreen.garden", "main").unwrap();
        let b = window_state_path("com.floregreen.garden", "editor").unwrap();
        let c = window_state_path("com.floregreen.garden.dev", "main").unwrap();
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);
    }

    fn primary_monitor() -> MonitorRect {
        MonitorRect {
            position: (0, 0),
            physical_size: (1920, 1080),
            scale_factor: 1.0,
        }
    }

    #[test]
    fn overlapping_position_finds_the_monitor() {
        let monitors = [primary_monitor()];
        let found = overlapping_monitor((800.0, 600.0), (100, 100), 1.0, &monitors);
        assert_eq!(found, Some(monitors[0]));
    }

    #[test]
    fn fully_off_every_monitor_finds_nothing() {
        let monitors = [primary_monitor()];
        let found = overlapping_monitor((800.0, 600.0), (5000, 5000), 1.0, &monitors);
        assert_eq!(found, None);
    }

    #[test]
    fn negative_coordinates_can_still_overlap_a_monitor_at_a_non_zero_origin() {
        let monitors = [MonitorRect {
            position: (-1920, 0),
            physical_size: (1920, 1080),
            scale_factor: 1.0,
        }];
        let found = overlapping_monitor((800.0, 600.0), (-1800, 100), 1.0, &monitors);
        assert_eq!(found, Some(monitors[0]));
    }

    #[test]
    fn overlap_below_the_minimum_threshold_does_not_count() {
        let monitors = [primary_monitor()];
        // Only a few pixels hang onto the monitor's left edge.
        let barely_off = overlapping_monitor(
            (800.0, 600.0),
            (-800 + (MIN_OVERLAP_PIXELS as i32 - 1), 100),
            1.0,
            &monitors,
        );
        assert_eq!(barely_off, None);
        let just_enough = overlapping_monitor(
            (800.0, 600.0),
            (-800 + MIN_OVERLAP_PIXELS as i32, 100),
            1.0,
            &monitors,
        );
        assert_eq!(just_enough, Some(monitors[0]));
    }

    #[test]
    fn mixed_dpi_saved_rect_validates_against_a_differently_scaled_monitor_list() {
        // Saved at 1.0x, monitor list expressed at 2.0x physical pixels --
        // the persisted scale factor (not the current monitor's) converts
        // the candidate rect.
        let monitors = [MonitorRect {
            position: (0, 0),
            physical_size: (3840, 2160),
            scale_factor: 2.0,
        }];
        let found = overlapping_monitor((800.0, 600.0), (100, 100), 1.0, &monitors);
        assert_eq!(found, Some(monitors[0]));
    }

    #[test]
    fn clamp_to_min_raises_a_size_below_the_floor() {
        assert_eq!(
            clamp_to_min((400.0, 300.0), Some((800.0, 600.0))),
            (800.0, 600.0)
        );
        assert_eq!(
            clamp_to_min((1000.0, 200.0), Some((800.0, 600.0))),
            (1000.0, 600.0)
        );
    }

    #[test]
    fn clamp_to_min_is_a_no_op_without_a_configured_floor() {
        assert_eq!(clamp_to_min((400.0, 300.0), None), (400.0, 300.0));
    }

    #[test]
    fn remove_if_present_removes_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.json");
        save(&path, &sample_state()).unwrap();
        assert!(path.exists());
        remove_if_present(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn remove_if_present_on_a_missing_file_is_a_harmless_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("never-saved.json");
        remove_if_present(&path).unwrap();
        remove_if_present(&path).unwrap();
    }

    #[test]
    #[ignore = "needs a real Windows session: cargo test -p florui-platform -- --ignored"]
    fn probe_persistence_capability_writes_and_cleans_up_after_itself() {
        let identifier = "florui-platform-test-probe";
        assert!(probe_persistence_capability(identifier));
        // Leaves nothing behind -- a second probe run must not find a
        // stale file from the first.
        let path = window_state_path(identifier, PROBE_KEY).unwrap();
        assert!(!path.exists());
    }
}
