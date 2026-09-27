//! The current track's title/artist plus an abstract "cover" tile built
//! entirely from real CSS shapes (`transform`/`box-shadow`/`z-index`) --
//! florui has no real `<img>` rendering yet, so this is an honest
//! composition, not a placeholder standing in for a missing feature.
//!
//! The tile's fade-in on a track change alternates between two
//! identically-defined `@keyframes` names (`track-fade-even`/`-odd`,
//! keyed off the track index's parity) -- the real CSS animation engine
//! only restarts a `@keyframes` run when `animation-name`'s own computed
//! value actually changes, so a single fixed name would only ever play
//! once, on first mount.

use florui::prelude::*;

#[component]
pub fn NowPlaying(title: String, artist: String, track_index: usize) -> Element {
    let parity_class = if track_index % 2 == 0 {
        "now-playing-tile parity-even"
    } else {
        "now-playing-tile parity-odd"
    };

    view! {
        <div class="now-playing">
            <div class={parity_class}>
                <div class="tile-back" />
                <div class="tile-diamond" />
                <div class="tile-dot" />
            </div>
            <div class="now-playing-text">
                <span class="now-playing-title">{title}</span>
                <span class="now-playing-artist">{artist}</span>
            </div>
        </div>
    }
}
