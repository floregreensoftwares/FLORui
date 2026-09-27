//! Real, audible playback via `rodio` -- procedurally synthesized note
//! sequences (`rodio::source::SineWave`), not bundled audio files: no
//! licensing question, no binary assets, and the chiptune-sounding result
//! fits this player's retro aesthetic on purpose rather than as a
//! workaround.
//!
//! Owns one long-lived `OutputStream`/`Sink` pair for the process's whole
//! lifetime -- dropping either silences playback immediately, so both
//! live on `Player` and nowhere else.

use std::time::Duration;

use rodio::source::SineWave;
use rodio::{OutputStream, Sink, Source};

/// One musical note: `hz` (0.0 = a rest, no tone) held for `duration`.
#[derive(Clone, Copy)]
pub struct Note {
    pub hz: f32,
    pub duration: Duration,
}

const fn note(hz: f32, ms: u64) -> Note {
    Note {
        hz,
        duration: Duration::from_millis(ms),
    }
}

pub struct Track {
    pub title: &'static str,
    pub artist: &'static str,
    pub notes: &'static [Note],
}

impl Track {
    /// Total playback length, for the progress bar's own denominator.
    pub fn duration(&self) -> Duration {
        self.notes.iter().map(|n| n.duration).sum()
    }
}

// Three short, royalty-free-by-construction melodies -- there is no
// source audio here at all, only frequencies and durations.
const ARPEGGIO_A4: &[Note] = &[
    note(440.0, 220),
    note(554.37, 220),
    note(659.25, 220),
    note(880.0, 220),
    note(659.25, 220),
    note(554.37, 220),
];

const RISING_SCALE: &[Note] = &[
    note(261.63, 180),
    note(293.66, 180),
    note(329.63, 180),
    note(349.23, 180),
    note(392.0, 180),
    note(440.0, 180),
    note(493.88, 180),
    note(523.25, 260),
];

const OCTAVE_BOUNCE: &[Note] = &[
    note(220.0, 200),
    note(0.0, 40),
    note(440.0, 200),
    note(0.0, 40),
    note(220.0, 200),
    note(0.0, 40),
    note(440.0, 320),
];

pub const PLAYLIST: &[Track] = &[
    Track {
        title: "Arpeggio in A",
        artist: "florui-player",
        notes: ARPEGGIO_A4,
    },
    Track {
        title: "Rising Scale",
        artist: "florui-player",
        notes: RISING_SCALE,
    },
    Track {
        title: "Octave Bounce",
        artist: "florui-player",
        notes: OCTAVE_BOUNCE,
    },
];

/// Owns the real audio device and exposes transport controls -- `main.rs`
/// keeps one of these alive for the app's whole run.
pub struct Player {
    _stream: OutputStream,
    sink: Sink,
}

impl Player {
    pub fn new() -> Self {
        let (stream, handle) =
            OutputStream::try_default().expect("a real audio output device must be available");
        let sink = Sink::try_new(&handle).expect("the default output device must accept a sink");
        Self {
            _stream: stream,
            sink,
        }
    }

    /// Clears whatever was queued and plays `track` from its first note.
    pub fn play(&self, track: &Track) {
        self.sink.clear();
        for n in track.notes {
            if n.hz > 0.0 {
                self.sink
                    .append(SineWave::new(n.hz).take_duration(n.duration).amplify(0.4));
            } else {
                self.sink
                    .append(rodio::source::Zero::<f32>::new(1, 44100).take_duration(n.duration));
            }
        }
        self.sink.play();
    }

    pub fn set_paused(&self, paused: bool) {
        if paused {
            self.sink.pause();
        } else {
            self.sink.play();
        }
    }

    pub fn set_volume(&self, volume: f32) {
        self.sink.set_volume(volume.clamp(0.0, 1.0));
    }
}
