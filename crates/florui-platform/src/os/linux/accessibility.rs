//! The "reduce animations" preference, read from GNOME's
//! `org.gnome.desktop.interface enable-animations` through `gsettings`.
//! Other desktops report no preference.

use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const REFRESH_EVERY: Duration = Duration::from_secs(2);

static REDUCED: AtomicBool = AtomicBool::new(false);
static POLLER: OnceLock<()> = OnceLock::new();

/// Called on every update, so it never spawns a process itself: the first
/// call reads once, then a background thread refreshes the answer.
pub fn prefers_reduced_motion() -> bool {
    POLLER.get_or_init(|| {
        REDUCED.store(read().unwrap_or(false), Ordering::Relaxed);
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(REFRESH_EVERY);
                REDUCED.store(read().unwrap_or(false), Ordering::Relaxed);
            }
        });
    });
    REDUCED.load(Ordering::Relaxed)
}

fn read() -> Option<bool> {
    let output = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "enable-animations"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| parse(&String::from_utf8_lossy(&output.stdout)))?
}

/// `Some(true)` when animations are switched off.
fn parse(output: &str) -> Option<bool> {
    match output.trim() {
        "false" => Some(true),
        "true" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animations_off_means_reduced_motion() {
        assert_eq!(parse("false\n"), Some(true));
        assert_eq!(parse("true\n"), Some(false));
    }

    #[test]
    fn unexpected_output_is_no_preference() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("'maybe'"), None);
    }
}
