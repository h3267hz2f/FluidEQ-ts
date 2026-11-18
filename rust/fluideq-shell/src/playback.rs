/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! The Library's transport: which file each deck holds, what is playing, and
//! when the next track is readied so the fade lands before the current one
//! ends.
//!
//! The engine already owns both decks, the decoder and the fade; this module
//! only decides *what* to ask it and keeps the page's clock in step with the
//! host's telemetry. Position is never counted here: every number on the
//! transport comes from `FEQT`, so a stalled device shows a stalled bar rather
//! than a clock that keeps running over silence.

use crate::engine_host::EngineHost;
use crate::library::LibraryState;
use fluideq_dsp_sys::{FeqCrossfadeCurve, FeqDeckState};
use fluideq_state::library::Track;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

pub const EVENT: &str = "player";
const SETTINGS_FILE: &str = "player.json";
const TICK: Duration = Duration::from_millis(200);
/// How long before a track ends the next one is decoded onto the free deck.
const PRELOAD_LEAD_SECONDS: f64 = 2.5;
/// How early the fade starts, covering one telemetry tick of jitter.
const FADE_TRIGGER_LEAD_SECONDS: f64 = 0.3;
const MAX_CROSSFADE_SECONDS: f64 = 12.0;
const DEFAULT_CROSSFADE_SECONDS: f64 = 4.0;
/// A transition whose deck never appears is not a slow device: the player was
/// rebuilt under it.
const AWAIT_TIMEOUT: Duration = Duration::from_secs(3);
/// Below this, Previous starts the current track again instead of stepping.
const RESTART_MS: u64 = 3_000;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerSnapshot {
    pub playing: bool,
    pub paused: bool,
    pub path: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub queue_index: Option<usize>,
    pub queue_len: usize,
    pub crossfade_seconds: f64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FadeStep {
    Idle,
    Preload,
    Fade,
}

/// The transport's next move, from the numbers the host reported.
fn fade_step(
    remaining_seconds: f64,
    crossfade_seconds: f64,
    has_next: bool,
    preloaded: bool,
) -> FadeStep {
    if !has_next || crossfade_seconds <= 0.0 || !remaining_seconds.is_finite() {
        return FadeStep::Idle;
    }
    if remaining_seconds <= crossfade_seconds + FADE_TRIGGER_LEAD_SECONDS {
        if preloaded {
            FadeStep::Fade
        } else {
            FadeStep::Preload
        }
    } else if !preloaded && remaining_seconds <= crossfade_seconds + PRELOAD_LEAD_SECONDS {
        FadeStep::Preload
    } else {
        FadeStep::Idle
    }
}

fn next_target(index: usize, len: usize) -> Option<usize> {
    let next = index + 1;
    (next < len).then_some(next)
}

fn previous_target(index: usize) -> Option<usize> {
    index.checked_sub(1)
}

fn clamp_crossfade(seconds: f64) -> f64 {
    if !seconds.is_finite() {
        DEFAULT_CROSSFADE_SECONDS
    } else {
        seconds.clamp(0.0, MAX_CROSSFADE_SECONDS)
    }
}

fn seconds_to_ms(seconds: f64) -> u64 {
    if !seconds.is_finite() || seconds <= 0.0 {
        0
    } else {
        (seconds * 1000.0).round() as u64
    }
}

struct Inner {
    queue: Vec<Track>,
    index: Option<usize>,
    current_deck: u32,
    playing: bool,
    paused: bool,
    at_end: bool,
    position_ms: u64,
    duration_ms: u64,
    crossfade_seconds: f64,
    /// `(queue index, deck)` readied while the current track still plays.
    preloaded: Option<(usize, u32)>,
    /// A track the decoder refused, so the ticker stops asking every tick.
    preload_failed: Option<usize>,
    /// Set by a transition until its deck answers in telemetry, so the frame
    /// that still describes the outgoing track cannot end the new one.
    awaiting: Option<Instant>,
    last_generation: Option<u32>,
    error: Option<String>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            queue: Vec::new(),
            index: None,
            current_deck: 0,
            playing: false,
            paused: false,
            at_end: false,
            position_ms: 0,
            duration_ms: 0,
            crossfade_seconds: DEFAULT_CROSSFADE_SECONDS,
            preloaded: None,
            preload_failed: None,
            awaiting: None,
            last_generation: None,
            error: None,
        }
    }
}

#[derive(Clone)]
pub struct PlaybackState {
    inner: Arc<Mutex<Inner>>,
    /// Serialises transitions: the page and the ticker can both ask for the
    /// next track at the same moment, and a load waits for its deck.
    ops: Arc<Mutex<()>>,
    host: EngineHost,
    data_dir: PathBuf,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PlayerFile {
    crossfade_seconds: Option<f64>,
}

impl PlaybackState {
    pub fn new(data_dir: PathBuf, host: EngineHost) -> Self {
        let crossfade_seconds =
            clamp_crossfade(load_crossfade(&data_dir).unwrap_or(DEFAULT_CROSSFADE_SECONDS));
        Self {
            inner: Arc::new(Mutex::new(Inner {
                crossfade_seconds,
                ..Inner::default()
            })),
            ops: Arc::new(Mutex::new(())),
            host,
            data_dir,
        }
    }

    /// Starts the loop that follows the host's clock and readies the next
    /// track before the current one runs out.
    pub fn start(&self, app: AppHandle) {
        let state = self.clone();
        let _ = std::thread::Builder::new()
            .name(String::from("fluideq-player-ticker"))
            .spawn(move || {
                let mut last: Option<PlayerSnapshot> = None;
                loop {
                    std::thread::sleep(TICK);
                    let snapshot = state.tick();
                    if last.as_ref() != Some(&snapshot) {
                        let _ = app.emit(EVENT, &snapshot);
                        last = Some(snapshot);
                    }
                }
            });
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        snapshot_of(&lock(&self.inner))
    }

    pub fn play(&self, app: &AppHandle, path: &str) -> Result<PlayerSnapshot, String> {
        let tracks = app.state::<LibraryState>().snapshot().tracks;
        let target = tracks
            .iter()
            .position(|track| track.path == path)
            .ok_or_else(|| String::from("that track is not in the library"))?;
        let _ops = lock(&self.ops);
        let was_playing = {
            let mut inner = lock(&self.inner);
            inner.queue = tracks;
            // A track readied from the previous queue may sit at the same
            // index as a different file now.
            inner.preloaded = None;
            inner.preload_failed = None;
            inner.playing
        };
        self.transition(target, was_playing, 0.0)?;
        self.host.player_set_playing(true)?;
        let mut inner = lock(&self.inner);
        inner.playing = true;
        inner.paused = false;
        inner.error = None;
        Ok(snapshot_of(&inner))
    }

    pub fn toggle(&self) -> Result<PlayerSnapshot, String> {
        let _ops = lock(&self.ops);
        let (index, playing, at_end) = {
            let inner = lock(&self.inner);
            let index = inner
                .index
                .ok_or_else(|| String::from("nothing has been played yet"))?;
            (index, inner.playing, inner.at_end)
        };
        if playing {
            self.host.player_set_playing(false)?;
            let mut inner = lock(&self.inner);
            inner.playing = false;
            inner.paused = true;
            return Ok(snapshot_of(&inner));
        }
        if at_end {
            // The deck is spent; Play starts the track again rather than
            // asking the engine to unpause silence.
            self.transition(index, false, 0.0)?;
        }
        self.host.player_set_playing(true)?;
        let mut inner = lock(&self.inner);
        inner.playing = true;
        inner.paused = false;
        inner.at_end = false;
        inner.error = None;
        Ok(snapshot_of(&inner))
    }

    pub fn next(&self) -> Result<PlayerSnapshot, String> {
        let _ops = lock(&self.ops);
        let (index, playing, len) = {
            let inner = lock(&self.inner);
            (
                inner
                    .index
                    .ok_or_else(|| String::from("nothing has been played yet"))?,
                inner.playing,
                inner.queue.len(),
            )
        };
        let target =
            next_target(index, len).ok_or_else(|| String::from("that was the last track"))?;
        self.transition(target, playing, 0.0)?;
        Ok(self.snapshot())
    }

    pub fn previous(&self) -> Result<PlayerSnapshot, String> {
        let _ops = lock(&self.ops);
        let (index, position_ms, deck) = {
            let inner = lock(&self.inner);
            (
                inner
                    .index
                    .ok_or_else(|| String::from("nothing has been played yet"))?,
                inner.position_ms,
                inner.current_deck,
            )
        };
        if position_ms > RESTART_MS {
            self.host.player_seek(deck, 0.0)?;
            let mut inner = lock(&self.inner);
            inner.position_ms = 0;
            inner.at_end = false;
            return Ok(snapshot_of(&inner));
        }
        let Some(target) = previous_target(index) else {
            self.host.player_seek(deck, 0.0)?;
            let mut inner = lock(&self.inner);
            inner.position_ms = 0;
            inner.at_end = false;
            return Ok(snapshot_of(&inner));
        };
        let playing = lock(&self.inner).playing;
        self.transition(target, playing, 0.0)?;
        Ok(self.snapshot())
    }

    pub fn seek(&self, position_ms: u64) -> Result<PlayerSnapshot, String> {
        let _ops = lock(&self.ops);
        let (deck, duration_ms) = {
            let inner = lock(&self.inner);
            if inner.index.is_none() {
                return Err(String::from("nothing has been played yet"));
            }
            (inner.current_deck, inner.duration_ms)
        };
        let position = if duration_ms > 0 {
            position_ms.min(duration_ms)
        } else {
            position_ms
        };
        self.host.player_seek(deck, position as f64 / 1000.0)?;
        let mut inner = lock(&self.inner);
        inner.position_ms = position;
        inner.at_end = false;
        inner.error = None;
        Ok(snapshot_of(&inner))
    }

    pub fn stop(&self) -> Result<PlayerSnapshot, String> {
        let _ops = lock(&self.ops);
        let _ = self.host.player_set_playing(false);
        let _ = self.host.player_unload(0);
        let _ = self.host.player_unload(1);
        let mut inner = lock(&self.inner);
        inner.index = None;
        inner.current_deck = 0;
        inner.playing = false;
        inner.paused = false;
        inner.at_end = false;
        inner.position_ms = 0;
        inner.duration_ms = 0;
        inner.preloaded = None;
        inner.preload_failed = None;
        inner.awaiting = None;
        inner.error = None;
        Ok(snapshot_of(&inner))
    }

    pub fn set_crossfade(&self, seconds: f64) -> Result<PlayerSnapshot, String> {
        let seconds = clamp_crossfade(seconds);
        save_crossfade(&self.data_dir, seconds)?;
        let mut inner = lock(&self.inner);
        inner.crossfade_seconds = seconds;
        Ok(snapshot_of(&inner))
    }

    /// Loads `target` onto a deck and puts it on the path. The caller holds
    /// [`Self::ops`].
    fn transition(&self, target: usize, fade: bool, cue_seconds: f64) -> Result<(), String> {
        let (track, from_index, current_deck, preloaded, playing, crossfade_seconds) = {
            let inner = lock(&self.inner);
            let track = inner
                .queue
                .get(target)
                .cloned()
                .ok_or_else(|| String::from("that track is not in the queue"))?;
            (
                track,
                inner.index,
                inner.current_deck,
                inner.preloaded,
                inner.playing,
                inner.crossfade_seconds,
            )
        };
        let (deck, readied) = match preloaded {
            Some((index, deck)) if index == target => (deck, true),
            _ => {
                // The other deck, not FEQ_DECK_HANDOFF: during a fade the host
                // answers "handoff" with the deck fading in, which is the one
                // being heard.
                let deck = if from_index.is_some() {
                    1 - current_deck
                } else {
                    0
                };
                let assigned = self.host.player_load(deck, &track.path, cue_seconds)?;
                (assigned as u32, false)
            }
        };
        if readied && cue_seconds > 0.0 {
            self.host.player_seek(deck, cue_seconds)?;
        }
        let fading = fade && from_index.is_some() && playing && crossfade_seconds > 0.0;
        if fading {
            self.host.player_crossfade(
                deck,
                crossfade_seconds,
                FeqCrossfadeCurve::FEQ_CROSSFADE_EQUAL_POWER as u32,
            )?;
        } else {
            self.host.player_select(deck)?;
        }
        let mut inner = lock(&self.inner);
        inner.index = Some(target);
        inner.current_deck = deck;
        inner.position_ms = seconds_to_ms(cue_seconds);
        inner.duration_ms = track.duration_ms.unwrap_or(0);
        inner.preloaded = None;
        inner.preload_failed = None;
        inner.at_end = false;
        inner.awaiting = Some(Instant::now());
        inner.error = None;
        if inner.playing {
            inner.paused = false;
        }
        Ok(())
    }

    /// Decodes the next track onto the free deck without putting it on the
    /// path. The caller holds [`Self::ops`].
    fn preload(&self, target: usize) -> Result<(), String> {
        let (track, deck) = {
            let inner = lock(&self.inner);
            let track = inner
                .queue
                .get(target)
                .cloned()
                .ok_or_else(|| String::from("that track is not in the queue"))?;
            if inner.index.is_none() {
                return Ok(());
            }
            (track, 1 - inner.current_deck)
        };
        match self.host.player_load(deck, &track.path, 0.0) {
            Ok(deck) => {
                let mut inner = lock(&self.inner);
                inner.preloaded = Some((target, deck as u32));
                inner.preload_failed = None;
                Ok(())
            }
            Err(error) => {
                let mut inner = lock(&self.inner);
                inner.preload_failed = Some(target);
                inner.error = Some(error.clone());
                Err(error)
            }
        }
    }

    fn tick(&self) -> PlayerSnapshot {
        let _ops = lock(&self.ops);
        let Some(telemetry) = self.host.telemetry() else {
            return self.note_silence();
        };
        let action = self.read_telemetry(telemetry);
        match action {
            Action::Nothing => {}
            Action::Recue => {
                let (index, position_ms, playing) = {
                    let mut inner = lock(&self.inner);
                    // The old player's decks went with the old endpoint; a
                    // readied track on one of them is gone too.
                    inner.preloaded = None;
                    (inner.index, inner.position_ms, inner.playing)
                };
                if let Some(index) = index {
                    if self
                        .transition(index, false, position_ms as f64 / 1000.0)
                        .is_ok()
                    {
                        let _ = self.host.player_set_playing(playing);
                    }
                }
            }
            Action::Advance(target) => {
                if self.transition(target, false, 0.0).is_ok() {
                    let _ = self.host.player_set_playing(true);
                }
            }
            Action::Fade(target) => {
                if self.transition(target, true, 0.0).is_ok() {
                    let _ = self.host.player_set_playing(true);
                }
            }
            Action::Preload(target) => {
                let _ = self.preload(target);
            }
            Action::Finish => {
                let _ = self.host.player_set_playing(false);
                let mut inner = lock(&self.inner);
                inner.playing = false;
                inner.paused = false;
                inner.at_end = true;
                if inner.duration_ms > 0 {
                    inner.position_ms = inner.duration_ms;
                }
            }
            Action::Lost => {
                let _ = self.host.player_set_playing(false);
                let mut inner = lock(&self.inner);
                inner.playing = false;
                inner.paused = false;
                inner.error = Some(String::from(
                    "the engine rebuilt its player; press Play to cue the track again",
                ));
            }
        }
        self.snapshot()
    }

    fn read_telemetry(&self, telemetry: crate::engine_host::TransportTelemetry) -> Action {
        let mut inner = lock(&self.inner);
        if inner.last_generation != Some(telemetry.device_generation) {
            let first = inner.last_generation.is_none();
            inner.last_generation = Some(telemetry.device_generation);
            if !first && inner.index.is_some() && (inner.playing || inner.paused) {
                // A reopened endpoint builds a new player with no decks; the
                // file has to be cued again or the stream plays silence.
                return Action::Recue;
            }
            return Action::Nothing;
        }
        if !inner.playing {
            return Action::Nothing;
        }
        let active = telemetry.active_deck == inner.current_deck;
        let empty = telemetry.deck_state == FeqDeckState::FEQ_DECK_EMPTY as u32;
        if !active || empty {
            match inner.awaiting {
                Some(at) if at.elapsed() > AWAIT_TIMEOUT => return Action::Lost,
                Some(_) => {}
                // A mismatch with no transition behind it is the first frame
                // of a rebuilt player; give it the same grace as a load.
                None => inner.awaiting = Some(Instant::now()),
            }
            return Action::Nothing;
        }
        inner.awaiting = None;
        inner.position_ms = seconds_to_ms(telemetry.position_seconds);
        if telemetry.duration_seconds > 0.0 {
            inner.duration_ms = seconds_to_ms(telemetry.duration_seconds);
        }
        if telemetry.deck_state == FeqDeckState::FEQ_DECK_ENDED as u32 {
            inner.at_end = true;
            if inner.duration_ms > 0 {
                inner.position_ms = inner.duration_ms;
            }
            return match inner
                .index
                .and_then(|index| next_target(index, inner.queue.len()))
            {
                Some(target) => Action::Advance(target),
                None => Action::Finish,
            };
        }
        inner.at_end = false;
        inner.error = None;
        let Some(index) = inner.index else {
            return Action::Nothing;
        };
        if telemetry.duration_seconds <= 0.0 {
            // The decoder could not say how long it is; guessing would fade
            // the track into the next one on its first report.
            return Action::Nothing;
        }
        let remaining = (telemetry.duration_seconds - telemetry.position_seconds).max(0.0);
        let next = next_target(index, inner.queue.len());
        let preloaded = inner.preloaded.map(|(ready, _)| ready) == next;
        if !preloaded && inner.preload_failed == next {
            // The decoder refused it; the end of this track will try once
            // more with a cut instead of opening it every tick.
            return Action::Nothing;
        }
        match fade_step(
            remaining,
            inner.crossfade_seconds,
            next.is_some(),
            preloaded,
        ) {
            FadeStep::Preload => Action::Preload(index + 1),
            FadeStep::Fade => Action::Fade(index + 1),
            FadeStep::Idle => Action::Nothing,
        }
    }

    /// No telemetry at all: a host that never started, or one that is asleep
    /// between device callbacks. Only a transition that never landed is an
    /// error.
    fn note_silence(&self) -> PlayerSnapshot {
        let mut inner = lock(&self.inner);
        if inner.playing
            && inner
                .awaiting
                .map(|at| at.elapsed() > AWAIT_TIMEOUT)
                .unwrap_or(false)
        {
            inner.playing = false;
            inner.paused = false;
            inner.error = Some(String::from("the engine is not reporting the track"));
        }
        snapshot_of(&inner)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Nothing,
    Recue,
    Preload(usize),
    Fade(usize),
    Advance(usize),
    Finish,
    Lost,
}

fn snapshot_of(inner: &Inner) -> PlayerSnapshot {
    let track = inner.index.and_then(|index| inner.queue.get(index));
    PlayerSnapshot {
        playing: inner.playing,
        paused: inner.paused,
        path: track.map(|track| track.path.clone()),
        title: track.map(|track| track.title.clone()),
        artist: track.and_then(|track| track.artist.clone()),
        album: track.and_then(|track| track.album.clone()),
        position_ms: inner.position_ms,
        duration_ms: inner.duration_ms,
        queue_index: inner.index,
        queue_len: inner.queue.len(),
        crossfade_seconds: inner.crossfade_seconds,
        error: inner.error.clone(),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn load_crossfade(data_dir: &std::path::Path) -> Option<f64> {
    let text = std::fs::read_to_string(data_dir.join(SETTINGS_FILE)).ok()?;
    let file: PlayerFile = serde_json::from_str(&text).ok()?;
    file.crossfade_seconds
}

fn save_crossfade(data_dir: &std::path::Path, seconds: f64) -> Result<(), String> {
    let file = PlayerFile {
        crossfade_seconds: Some(seconds),
    };
    let json = serde_json::to_string_pretty(&file)
        .map_err(|error| format!("the player settings could not be written: {error}"))?;
    std::fs::create_dir_all(data_dir)
        .map_err(|error| format!("the player folder could not be made: {error}"))?;
    let temporary = data_dir.join(format!("{SETTINGS_FILE}.tmp"));
    std::fs::write(&temporary, json)
        .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, data_dir.join(SETTINGS_FILE))
        .map_err(|error| format!("could not replace {SETTINGS_FILE}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fade_starts_a_little_before_the_end_and_readies_the_track_first() {
        assert_eq!(fade_step(60.0, 4.0, true, false), FadeStep::Idle);
        assert_eq!(fade_step(6.0, 4.0, true, false), FadeStep::Preload);
        assert_eq!(fade_step(6.0, 4.0, true, true), FadeStep::Idle);
        assert_eq!(fade_step(4.2, 4.0, true, true), FadeStep::Fade);
        assert_eq!(fade_step(1.0, 4.0, true, false), FadeStep::Preload);
    }

    #[test]
    fn a_zero_crossfade_never_preloads_and_never_fades() {
        assert_eq!(fade_step(5.0, 0.0, true, false), FadeStep::Idle);
        assert_eq!(fade_step(0.2, 0.0, true, true), FadeStep::Idle);
    }

    #[test]
    fn the_last_track_has_no_next_and_the_first_has_no_previous() {
        assert_eq!(next_target(0, 1), None);
        assert_eq!(next_target(0, 2), Some(1));
        assert_eq!(previous_target(0), None);
        assert_eq!(previous_target(2), Some(1));
    }

    #[test]
    fn crossfade_lengths_are_clamped_to_the_transport() {
        assert_eq!(clamp_crossfade(3.5), 3.5);
        assert_eq!(clamp_crossfade(-2.0), 0.0);
        assert_eq!(clamp_crossfade(40.0), MAX_CROSSFADE_SECONDS);
        assert_eq!(clamp_crossfade(f64::NAN), DEFAULT_CROSSFADE_SECONDS);
    }

    #[test]
    fn positions_on_the_wire_become_whole_milliseconds() {
        assert_eq!(seconds_to_ms(1.2345), 1235);
        assert_eq!(seconds_to_ms(-1.0), 0);
        assert_eq!(seconds_to_ms(f64::INFINITY), 0);
    }

    #[test]
    fn the_crossfade_length_survives_a_reload() {
        let dir = std::env::temp_dir().join(format!("fluideq-player-{}", std::process::id()));
        save_crossfade(&dir, 2.5).expect("a writable data folder");
        assert_eq!(load_crossfade(&dir), Some(2.5));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_snapshot_without_a_track_has_no_name_to_show() {
        let snapshot = snapshot_of(&Inner::default());
        assert!(!snapshot.playing);
        assert!(snapshot.path.is_none());
        assert_eq!(snapshot.crossfade_seconds, DEFAULT_CROSSFADE_SECONDS);
    }
}
