//! A plain (not virtualized) list of tracks -- this playlist is a
//! handful of built-in synthesized melodies, not a large dataset, so
//! `use_virtual_list` would be reaching for the wrong tool; see
//! `crate::audio::PLAYLIST`.

use std::rc::Rc;

use florui::prelude::*;

use crate::audio::PLAYLIST;

#[component]
pub fn Playlist(current_index: usize, on_select: Rc<dyn Fn(usize)>) -> Element {
    view! {
        <div class="playlist">
            {PLAYLIST.iter().enumerate().map(|(index, track)| {
                let select = on_select.clone();
                let row_class = if index == current_index {
                    "playlist-row playlist-row-active"
                } else {
                    "playlist-row"
                };
                let label = format!("{}, {}", track.title, track.artist);
                view! {
                    <button
                        class={row_class}
                        accessible_label={label}
                        onclick={move || select(index)}
                    >
                        <span class="playlist-track-title">{track.title}</span>
                        <span class="playlist-track-artist">{track.artist}</span>
                    </button>
                }
            }).collect::<Vec<_>>()}
        </div>
    }
}
