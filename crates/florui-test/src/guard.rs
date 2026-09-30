//! Limits that keep a runaway test from taking the machine: a watchdog that
//! ends the process when a mounted component outlives its time budget, and a
//! ceiling on how large a frame the harness will paint.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How long a component may stay mounted before the watchdog acts, unless
/// `FLORUI_TEST_TIMEOUT` (seconds) says otherwise.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// The most physical pixels one frame may have (4000 by 4000). A frame is
/// four bytes a pixel and is copied when read, so this keeps one from
/// costing gigabytes.
pub(crate) const MAX_FRAME_PIXELS: u64 = 16_000_000;

pub(crate) fn timeout() -> Duration {
    timeout_from(std::env::var("FLORUI_TEST_TIMEOUT").ok().as_deref())
}

fn timeout_from(setting: Option<&str>) -> Duration {
    setting
        .and_then(|seconds| seconds.trim().parse::<f64>().ok())
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .map_or(DEFAULT_TIMEOUT, Duration::from_secs_f64)
}

/// Runs `on_expire` once if it is still alive when `limit` has passed.
/// Dropping it disarms it.
pub(crate) struct Watchdog {
    done: Arc<AtomicBool>,
}

impl Watchdog {
    pub(crate) fn start(limit: Duration, on_expire: impl FnOnce() + Send + 'static) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let watched = Arc::clone(&done);
        let started = Instant::now();
        std::thread::Builder::new()
            .name("florui-test watchdog".to_string())
            .spawn(move || {
                while started.elapsed() < limit {
                    if watched.load(Ordering::Acquire) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                if !watched.load(Ordering::Acquire) {
                    on_expire();
                }
            })
            .expect("a watchdog thread can be started");
        Self { done }
    }

    /// The watchdog mounted components carry: past `limit` it says so and ends
    /// the process, because a loop that never returns cannot be interrupted
    /// from the test thread.
    pub(crate) fn abort_after(limit: Duration) -> Self {
        Self::start(limit, move || {
            eprintln!(
                "florui-test: a mounted component ran for more than {} s; ending the test \
                 process so a runaway loop cannot take the machine. Set FLORUI_TEST_TIMEOUT \
                 (seconds) to allow longer.",
                limit.as_secs_f64()
            );
            std::process::abort();
        })
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    #[test]
    fn a_watchdog_that_outlives_its_limit_acts_once() {
        let fired = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&fired);
        let _watchdog = Watchdog::start(Duration::from_millis(60), move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(fired.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_watchdog_dropped_in_time_never_acts() {
        let fired = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&fired);
        let watchdog = Watchdog::start(Duration::from_millis(150), move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        drop(watchdog);
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(fired.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn the_timeout_is_the_setting_when_it_is_a_positive_number_and_the_default_otherwise() {
        assert_eq!(timeout_from(None), DEFAULT_TIMEOUT);
        assert_eq!(timeout_from(Some("5")), Duration::from_secs(5));
        assert_eq!(timeout_from(Some(" 0.5 ")), Duration::from_millis(500));
        for nonsense in ["", "soon", "0", "-3", "NaN", "inf"] {
            assert_eq!(
                timeout_from(Some(nonsense)),
                DEFAULT_TIMEOUT,
                "{nonsense:?}"
            );
        }
    }
}
