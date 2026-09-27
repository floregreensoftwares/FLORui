use std::rc::Rc;
use std::time::Duration;

use florui::prelude::*;
use florui_platform::use_window_controls;
use florui_reactive::{Resource, use_resource};

use crate::audio::{PLAYLIST, Player};
use crate::components::now_playing::{NowPlaying, NowPlayingProps};
use crate::components::playlist::{Playlist, PlaylistProps};
use crate::components::title_bar::{TitleBar, TitleBarProps};
use crate::components::transport::{Transport, TransportProps};
use crate::theme::Skin;

/// One tick of the progress bar -- chained one-shot background sleeps
/// via `use_resource`/`spawn_blocking`, the same safe cross-thread wake
/// pattern `UiRuntime`'s own tests use for a background completion (see
/// `crates/florui-platform/src/runtime.rs`'s
/// `a_background_completion_notifies_on_needs_update_without_a_polling_loop`)
/// -- there is no dedicated interval/timer hook in `florui-reactive` yet.
const TICK: Duration = Duration::from_millis(200);

#[component]
pub fn App(player: Rc<Player>) -> Element {
    let controls = use_window_controls();
    let focused = controls.as_ref().is_none_or(|c| c.is_focused());

    let skin = use_signal(|| Skin::Terminal);
    let current_index = use_signal(|| 0usize);
    let paused = use_signal(|| false);
    let volume = use_signal(|| 0.8f32);
    let shuffle = use_signal(|| false);
    let repeat = use_signal(|| false);
    let elapsed_ms = use_signal(|| 0u64);
    let tick_key = use_signal(|| 0u64);

    // Starts (or restarts) playback whenever the selected track changes.
    {
        let player = Rc::clone(&player);
        let current_index = current_index.clone();
        let elapsed_ms = elapsed_ms.clone();
        let paused = paused.clone();
        let tick_key = tick_key.clone();
        use_effect(current_index.get(), move || {
            player.play(&PLAYLIST[current_index.get()]);
            elapsed_ms.set(0);
            paused.set(false);
            tick_key.set(tick_key.get() + 1);
            None
        });
    }

    // The progress tick chain: each resolved sleep advances elapsed time
    // (if not paused) and re-arms the next one by changing the key.
    {
        let resource = use_resource(tick_key.get(), |_| {
            florui_reactive::blocking::spawn_blocking(move || {
                std::thread::sleep(TICK);
                Ok::<(), std::convert::Infallible>(())
            })
        });
        let ready = matches!(resource.get(), Resource::Ready(_));
        let elapsed_ms = elapsed_ms.clone();
        let tick_key = tick_key.clone();
        let paused = paused.clone();
        let current_index = current_index.clone();
        let repeat = repeat.clone();
        let shuffle = shuffle.clone();
        use_effect(ready, move || {
            if ready && !paused.get() {
                let track = &PLAYLIST[current_index.get()];
                let next_elapsed = elapsed_ms.get() + TICK.as_millis() as u64;
                if next_elapsed >= track.duration().as_millis() as u64 {
                    if repeat.get() {
                        elapsed_ms.set(0);
                    } else {
                        let next = advance_index(current_index.get(), shuffle.get());
                        current_index.set(next);
                    }
                } else {
                    elapsed_ms.set(next_elapsed);
                }
                tick_key.set(tick_key.get() + 1);
            }
            None
        });
    }

    let track = &PLAYLIST[current_index.get()];
    let progress = elapsed_ms.get() as f32 / track.duration().as_millis().max(1) as f32;

    let on_cycle_skin = {
        let skin = skin.clone();
        Handler::new(move || skin.set(skin.get().next()))
    };

    let on_play_pause = {
        let player = Rc::clone(&player);
        let paused = paused.clone();
        Handler::new(move || {
            let next_paused = !paused.get();
            paused.set(next_paused);
            player.set_paused(next_paused);
        })
    };

    let on_previous = {
        let current_index = current_index.clone();
        let shuffle = shuffle.clone();
        Handler::new(move || {
            let prev = if shuffle.get() {
                advance_index(current_index.get(), true)
            } else {
                (current_index.get() + PLAYLIST.len() - 1) % PLAYLIST.len()
            };
            current_index.set(prev);
        })
    };

    let on_next = {
        let current_index = current_index.clone();
        let shuffle = shuffle.clone();
        Handler::new(move || {
            let next = advance_index(current_index.get(), shuffle.get());
            current_index.set(next);
        })
    };

    let on_volume_down = {
        let player = Rc::clone(&player);
        let volume = volume.clone();
        Handler::new(move || {
            let next = (volume.get() - 0.1).max(0.0);
            volume.set(next);
            player.set_volume(next);
        })
    };
    let on_volume_up = {
        let player = Rc::clone(&player);
        let volume = volume.clone();
        Handler::new(move || {
            let next = (volume.get() + 0.1).min(1.0);
            volume.set(next);
            player.set_volume(next);
        })
    };

    let on_shuffle = {
        let shuffle = shuffle.clone();
        Handler::new(move || shuffle.set(!shuffle.get()))
    };
    let on_repeat = {
        let repeat = repeat.clone();
        Handler::new(move || repeat.set(!repeat.get()))
    };

    let on_select: Rc<dyn Fn(usize)> = {
        let current_index = current_index.clone();
        Rc::new(move |index| current_index.set(index))
    };

    view! {
        <div class={format!("app {}", skin.get().class())}>
            <TitleBar
                focused={focused}
                skin={skin.get()}
                on_cycle_skin={on_cycle_skin}
                controls={controls}
            />
            <div class="body">
                <NowPlaying
                    title={track.title.to_string()}
                    artist={track.artist.to_string()}
                    track_index={current_index.get()}
                />
                <Transport
                    paused={paused.get()}
                    on_play_pause={on_play_pause}
                    on_previous={on_previous}
                    on_next={on_next}
                    volume={volume.get()}
                    on_volume_down={on_volume_down}
                    on_volume_up={on_volume_up}
                    shuffle={shuffle.get()}
                    on_shuffle={on_shuffle}
                    repeat={repeat.get()}
                    on_repeat={on_repeat}
                    progress={progress}
                />
                <Playlist current_index={current_index.get()} on_select={on_select} />
            </div>
        </div>
    }
}

/// The next track index -- a random *different* one when shuffling,
/// otherwise the plain next-with-wraparound.
fn advance_index(current: usize, shuffle: bool) -> usize {
    if shuffle && PLAYLIST.len() > 1 {
        let mut candidate = pseudo_random(PLAYLIST.len());
        while candidate == current {
            candidate = pseudo_random(PLAYLIST.len());
        }
        candidate
    } else {
        (current + 1) % PLAYLIST.len()
    }
}

/// A real, if unglamorous, source of randomness with no extra
/// dependency: the low bits of the current time.
fn pseudo_random(bound: usize) -> usize {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos as usize) % bound
}
