//! The one event loop tests may use. winit allows a single event loop per
//! process, and the tests that need a real window run on different threads, so
//! they all hand their work to one thread that owns the loop.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};

use winit::event_loop::EventLoop;
use winit::platform::windows::EventLoopBuilderExtWindows;

use crate::desktop::UserEvent;

type Job = Box<dyn FnOnce(&mut EventLoop<UserEvent>) + Send>;

fn worker() -> &'static Mutex<Sender<Job>> {
    static WORKER: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();
    WORKER.get_or_init(|| {
        let (sender, receiver) = channel::<Job>();
        std::thread::spawn(move || {
            let mut event_loop = EventLoop::<UserEvent>::with_user_event()
                .with_any_thread(true)
                .build()
                .expect("an event loop needs a desktop session");
            for job in receiver {
                job(&mut event_loop);
            }
        });
        Mutex::new(sender)
    })
}

/// Runs `job` on the thread that owns the test event loop and returns what it
/// returns, or panics the way it did.
pub(crate) fn on_event_loop<R: Send + 'static>(
    job: impl FnOnce(&mut EventLoop<UserEvent>) -> R + Send + 'static,
) -> R {
    let (result_sender, result_receiver) = channel();
    worker()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .send(Box::new(move |event_loop| {
            let result = catch_unwind(AssertUnwindSafe(|| job(event_loop)));
            let _ = result_sender.send(result);
        }))
        .expect("the event loop thread is alive");
    match result_receiver
        .recv()
        .expect("the event loop thread answers")
    {
        Ok(value) => value,
        Err(panic) => resume_unwind(panic),
    }
}

/// Drives `handler` on the shared loop. winit reports `resumed` only the first
/// time a loop runs, so a handler that opens its window there would never get
/// it on a later test; this gives it the call on its first iteration.
pub(crate) fn pump<H: winit::application::ApplicationHandler<UserEvent>>(
    event_loop: &mut EventLoop<UserEvent>,
    handler: &mut H,
    timeout: std::time::Duration,
) {
    use winit::platform::pump_events::EventLoopExtPumpEvents;

    struct EnsureResumed<'a, H> {
        inner: &'a mut H,
        resumed: bool,
    }

    impl<H: winit::application::ApplicationHandler<UserEvent>>
        winit::application::ApplicationHandler<UserEvent> for EnsureResumed<'_, H>
    {
        fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
            self.resumed = true;
            self.inner.resumed(event_loop);
        }

        fn user_event(
            &mut self,
            event_loop: &winit::event_loop::ActiveEventLoop,
            event: UserEvent,
        ) {
            self.inner.user_event(event_loop, event);
        }

        fn window_event(
            &mut self,
            event_loop: &winit::event_loop::ActiveEventLoop,
            window_id: winit::window::WindowId,
            event: winit::event::WindowEvent,
        ) {
            self.inner.window_event(event_loop, window_id, event);
        }

        fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
            if !self.resumed {
                self.resumed = true;
                self.inner.resumed(event_loop);
            }
            self.inner.about_to_wait(event_loop);
        }
    }

    event_loop.pump_app_events(
        Some(timeout),
        &mut EnsureResumed {
            inner: handler,
            resumed: false,
        },
    );
}
