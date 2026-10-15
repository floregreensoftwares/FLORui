//! Opens a URL in the desktop's default handler through `xdg-open`, the
//! command every desktop environment ships for this. See `crate::open_url`'s
//! own doc for why this call is synchronous.

use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use winit::window::Window;

use crate::open_url::OpenUrlOutcome;

/// `xdg-open` normally hands the URL to its handler and exits within a few
/// milliseconds. Some desktops keep it running while the handler is up; past
/// this wait the handler was launched, so that is a success.
const LAUNCH_WAIT: Duration = Duration::from_secs(3);

pub(crate) fn open_url(_window: &Window, url: &str) -> OpenUrlOutcome {
    open_with("xdg-open", url)
}

fn open_with(program: &str, url: &str) -> OpenUrlOutcome {
    // An argument that starts with a dash would be read as an option.
    if url.is_empty() || url.starts_with('-') {
        return OpenUrlOutcome::Failed(format!("{url:?} is not a URL"));
    }
    let mut child = match Command::new(program)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return OpenUrlOutcome::Unavailable;
        }
        Err(error) => return OpenUrlOutcome::Failed(format!("could not run {program}: {error}")),
    };
    // The thread reaps the child however long it takes, so a slow handler
    // never leaves a zombie behind.
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(child.wait());
    });
    match receiver.recv_timeout(LAUNCH_WAIT) {
        Ok(Ok(status)) if status.success() => OpenUrlOutcome::Opened,
        Ok(Ok(status)) => OpenUrlOutcome::Failed(format!("{program} exited with {status}")),
        Ok(Err(error)) => OpenUrlOutcome::Failed(format!("could not wait for {program}: {error}")),
        Err(_) => OpenUrlOutcome::Opened,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handler_that_succeeds_means_the_url_was_opened() {
        assert_eq!(
            open_with("true", "https://example.org"),
            OpenUrlOutcome::Opened
        );
    }

    #[test]
    fn a_handler_that_fails_is_reported_with_its_status() {
        let outcome = open_with("false", "https://example.org");
        assert!(
            matches!(&outcome, OpenUrlOutcome::Failed(message) if message.contains("exited with")),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_missing_command_means_the_service_is_unavailable() {
        assert_eq!(
            open_with("florui-no-such-command", "https://example.org"),
            OpenUrlOutcome::Unavailable
        );
    }

    #[test]
    fn a_value_that_would_be_read_as_an_option_is_refused() {
        for bad in ["", "-h", "--version"] {
            assert!(
                matches!(open_with("true", bad), OpenUrlOutcome::Failed(_)),
                "{bad:?} was passed on"
            );
        }
    }

    #[test]
    fn a_handler_that_stays_running_still_counts_as_launched() {
        // `sleep` ignores its argument shape here: 5 is longer than the wait.
        let start = std::time::Instant::now();
        assert_eq!(open_with("sleep", "5"), OpenUrlOutcome::Opened);
        assert!(start.elapsed() < Duration::from_secs(4));
    }
}
