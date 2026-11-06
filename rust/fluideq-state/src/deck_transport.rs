/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Dual-Deck Gapless Transport, Crossfade Curve Table, 31-Word Noise Profile Wire Decoder,
//! and Offline WAV Render Ceiling Validator.
//!
//! Ported from `/native/dsp-host/src/host_deck_commands.cpp` and `/native/dsp-host/src/host_commands.cpp`.

use fluideq_dsp_sys::{
    FeqChainResetReason, FeqCrossfadeCurve, FeqCrossfadeTable, FeqNoiseProfile,
    FEQ_CROSSFADE_TABLE_POINTS, FEQ_DECK_HANDOFF, FEQ_DECK_SPARE, FEQ_DENOISE_MAX_HUM_PARTIALS,
    FEQ_DENOISE_PROFILE_BANDS, FEQ_DENOISE_PROFILE_WIRE, FEQ_PLAYER_DECKS,
};
use serde::{Deserialize, Serialize};

pub const MAX_WIRE_PATH_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeckNote {
    pub path: String,
    pub cue_seconds: f64,
    pub fresh: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeckLoadOutcome {
    pub loaded: bool,
    /// `0` or `1` when landed on a concrete deck, or `-1` when queued as `FEQ_DECK_SPARE`.
    pub assigned_deck: i32,
    pub chain_reset: Option<FeqChainResetReason>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DualDeckCoordinator {
    pub deck_notes: [DeckNote; FEQ_PLAYER_DECKS],
    pub deck_audible: [bool; FEQ_PLAYER_DECKS],
    pub active_deck: usize,
    pub fading: bool,
    pub reserved_deck: usize,
    pub spare_path: String,
    pub spare_seconds: f64,
    pub spare_wanted: bool,
    pub track_level_gains_db: [f64; 2],
    pub track_level_enabled: bool,
}

impl Default for DualDeckCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl DualDeckCoordinator {
    pub fn new() -> Self {
        Self {
            deck_notes: [DeckNote::default(), DeckNote::default()],
            deck_audible: [false, false],
            active_deck: 0,
            fading: false,
            reserved_deck: FEQ_PLAYER_DECKS,
            spare_path: String::new(),
            spare_seconds: 0.0,
            spare_wanted: false,
            track_level_gains_db: [0.0, 0.0],
            track_level_enabled: false,
        }
    }

    /// Returns the handoff deck (`0` or `1`): the deck not currently active.
    pub fn handoff_deck(&self) -> usize {
        if self.active_deck == 0 {
            1
        } else {
            0
        }
    }

    /// Readies the queued spare track onto the handoff deck if no fade is running and no deck is reserved.
    /// Exact port of `ready_spare` in `/native/dsp-host/src/host_audio.cpp` (lines 192–216).
    pub fn ready_spare(&mut self) -> Option<usize> {
        if !self.spare_wanted
            || self.spare_path.is_empty()
            || self.fading
            || self.reserved_deck < FEQ_PLAYER_DECKS
        {
            return None;
        }
        // Tried once per naming: a bad file is not retried on every cut.
        self.spare_wanted = false;
        let deck = self.handoff_deck();
        if deck >= FEQ_PLAYER_DECKS {
            return None;
        }
        let note = &mut self.deck_notes[deck];
        if note.fresh
            && note.path == self.spare_path
            && (note.cue_seconds - self.spare_seconds).abs() < 1e-3
        {
            return Some(deck);
        }
        *note = DeckNote {
            path: self.spare_path.clone(),
            cue_seconds: self.spare_seconds,
            fresh: true,
        };
        Some(deck)
    }

    /// Called when `deck` takes the active path via `select_deck` or `crossfade`.
    /// Exact port of `took_the_path` in `/native/dsp-host/src/host_audio.cpp` (lines 223–230).
    fn took_the_path(&mut self, deck: usize) {
        self.reserved_deck = FEQ_PLAYER_DECKS;
        if deck < FEQ_PLAYER_DECKS {
            self.active_deck = deck;
            self.deck_notes[deck].fresh = false;
        }
        let _ = self.ready_spare();
    }

    /// Exact port of `load_deck` in `/native/dsp-host/src/host_deck_commands.cpp`.
    pub fn load_deck(&mut self, asked: u32, path: &str, cue_seconds: f64) -> DeckLoadOutcome {
        if path.is_empty() || path.len() > MAX_WIRE_PATH_BYTES {
            return DeckLoadOutcome {
                loaded: false,
                assigned_deck: -1,
                chain_reset: None,
            };
        }
        let cue = if cue_seconds > 0.0 { cue_seconds } else { 0.0 };

        if asked == FEQ_DECK_SPARE {
            self.spare_path = path.to_string();
            self.spare_seconds = cue;
            self.spare_wanted = true;
            let _ = self.ready_spare();
            return DeckLoadOutcome {
                loaded: true,
                assigned_deck: -1,
                chain_reset: None,
            };
        }

        if asked == FEQ_DECK_HANDOFF {
            let deck = self.handoff_deck();
            let note = &mut self.deck_notes[deck];
            if !self.fading && note.fresh && note.path == path {
                if (cue - note.cue_seconds).abs() >= 1e-3 {
                    note.cue_seconds = cue;
                    note.fresh = true;
                }
            } else {
                note.path = path.to_string();
                note.cue_seconds = cue;
                note.fresh = true;
            }
            self.reserved_deck = deck;
            if self.spare_wanted && self.spare_path == path {
                self.spare_wanted = false;
            }
            // Never reset the chain on handoff load: the outgoing track is still playing!
            return DeckLoadOutcome {
                loaded: true,
                assigned_deck: deck as i32,
                chain_reset: None,
            };
        }

        let deck = asked as usize;
        if deck < FEQ_PLAYER_DECKS {
            if self.reserved_deck == deck {
                self.reserved_deck = FEQ_PLAYER_DECKS;
            }
            let heard = self.deck_audible[deck];
            self.deck_notes[deck] = DeckNote {
                path: path.to_string(),
                cue_seconds: 0.0,
                fresh: true,
            };
            self.deck_audible[deck] = true;
            self.active_deck = deck;
            return DeckLoadOutcome {
                loaded: true,
                assigned_deck: deck as i32,
                chain_reset: if heard {
                    Some(FeqChainResetReason::FEQ_CHAIN_RESET_SOURCE_CHANGE)
                } else {
                    None
                },
            };
        }

        DeckLoadOutcome {
            loaded: false,
            assigned_deck: -1,
            chain_reset: None,
        }
    }

    /// Exact port of `unload_deck` in `/native/dsp-host/src/host_deck_commands.cpp`.
    pub fn unload_deck(&mut self, deck: usize) {
        if deck < FEQ_PLAYER_DECKS {
            self.deck_notes[deck] = DeckNote::default();
            self.deck_audible[deck] = false;
            if self.reserved_deck == deck {
                self.reserved_deck = FEQ_PLAYER_DECKS;
            }
            // Emptying a deck cancels any queued spare track so nothing is decoded for nobody.
            self.spare_wanted = false;
        }
    }

    /// Exact port of `seek_deck`: only resets the chain (`FEQ_CHAIN_RESET_SEEK`) if the deck is audible.
    pub fn seek_deck(&mut self, deck: usize, position_seconds: f64) -> Option<FeqChainResetReason> {
        if deck >= FEQ_PLAYER_DECKS {
            return None;
        }
        let heard = self.deck_audible[deck];
        self.deck_notes[deck].cue_seconds = position_seconds.max(0.0);
        if heard {
            Some(FeqChainResetReason::FEQ_CHAIN_RESET_SEEK)
        } else {
            None
        }
    }

    /// Exact port of `select_deck`: cuts to `deck`, resetting the chain (`FEQ_CHAIN_RESET_SOURCE_CHANGE`)
    /// only if `deck` was not already being heard.
    pub fn select_deck(&mut self, deck: usize) -> Option<FeqChainResetReason> {
        if deck >= FEQ_PLAYER_DECKS {
            return None;
        }
        let cut = !self.deck_audible[deck];
        for i in 0..FEQ_PLAYER_DECKS {
            self.deck_audible[i] = i == deck;
        }
        self.fading = false;
        self.took_the_path(deck);
        if cut {
            Some(FeqChainResetReason::FEQ_CHAIN_RESET_SOURCE_CHANGE)
        } else {
            None
        }
    }

    /// Exact port of `crossfade`: validates `curve_id` before casting (falling back to `FEQ_CROSSFADE_EQUAL_POWER`),
    /// and resets the chain only on a zero-length cut (`seconds <= 0.0`) to an inaudible deck.
    pub fn start_crossfade(
        &mut self,
        to_deck: usize,
        seconds: f64,
        curve_id: u32,
    ) -> (FeqCrossfadeCurve, Option<FeqChainResetReason>) {
        let curve = match curve_id {
            0 => FeqCrossfadeCurve::FEQ_CROSSFADE_EQUAL_POWER,
            1 => FeqCrossfadeCurve::FEQ_CROSSFADE_SMOOTH,
            2 => FeqCrossfadeCurve::FEQ_CROSSFADE_LINEAR,
            3 => FeqCrossfadeCurve::FEQ_CROSSFADE_CUSTOM,
            _ => FeqCrossfadeCurve::FEQ_CROSSFADE_EQUAL_POWER,
        };
        if to_deck >= FEQ_PLAYER_DECKS {
            return (curve, None);
        }
        let cut = seconds <= 0.0 && !self.deck_audible[to_deck];
        if seconds <= 0.0 {
            for i in 0..FEQ_PLAYER_DECKS {
                self.deck_audible[i] = i == to_deck;
            }
            self.fading = false;
        } else {
            self.deck_audible[to_deck] = true;
            self.fading = true;
        }
        self.took_the_path(to_deck);
        let reset = if cut {
            Some(FeqChainResetReason::FEQ_CHAIN_RESET_SOURCE_CHANGE)
        } else {
            None
        };
        (curve, reset)
    }

    /// Exact port of `set_track_gains` in `/native/dsp-host/src/host_deck_commands.cpp`.
    pub fn set_track_gains(&mut self, gain_a_db: f64, gain_b_db: f64, enabled: bool) {
        self.track_level_gains_db = [gain_a_db, gain_b_db];
        self.track_level_enabled = enabled;
    }
}

/// Decodes a 128-double wire payload (`64` outgoing + `64` incoming points) into `FeqCrossfadeTable`.
/// Exact port of `set_crossfade_table` in `/native/dsp-host/src/host_deck_commands.cpp`.
pub fn decode_crossfade_table_wire(points: &[f64]) -> Option<FeqCrossfadeTable> {
    if points.len() != FEQ_CROSSFADE_TABLE_POINTS * 2 {
        return None;
    }
    let mut table = FeqCrossfadeTable::default();
    for at in 0..FEQ_CROSSFADE_TABLE_POINTS {
        table.outgoing[at] = points[at] as f32;
        table.incoming[at] = points[FEQ_CROSSFADE_TABLE_POINTS + at] as f32;
    }
    Some(table)
}

/// Decodes the 63-word `FEQ_DENOISE_PROFILE_WIRE` payload into `FeqNoiseProfile`
/// (or `Ok(None)` when `values` is empty to clear a previous track's noise profile).
/// Exact port of `set_noise_profile` in `/native/dsp-host/src/host_commands.cpp`.
pub fn decode_noise_profile_wire(values: &[f64]) -> Result<Option<FeqNoiseProfile>, String> {
    if values.is_empty() {
        return Ok(None);
    }
    if values.len() != FEQ_DENOISE_PROFILE_WIRE {
        return Err(format!(
            "Expected {} noise profile doubles, got {}",
            FEQ_DENOISE_PROFILE_WIRE,
            values.len()
        ));
    }
    let mut profile = FeqNoiseProfile::default();
    let mut at = 0usize;
    for band in 0..FEQ_DENOISE_PROFILE_BANDS {
        profile.bands_db[band] = values[at];
        at += 1;
    }
    profile.floor_dbfs = values[at];
    at += 1;
    profile.hum_hz = values[at];
    at += 1;
    let count = values[at];
    at += 1;
    profile.hum_partial_count =
        if count > 0.0 && count <= (FEQ_DENOISE_MAX_HUM_PARTIALS as f64) {
            count as u32
        } else {
            0
        };
    for i in 0..FEQ_DENOISE_MAX_HUM_PARTIALS {
        profile.hum_partial_hz[i] = values[at];
        at += 1;
    }
    for i in 0..FEQ_DENOISE_MAX_HUM_PARTIALS {
        profile.hum_partial_excess_db[i] = values[at];
        at += 1;
    }
    Ok(Some(profile))
}

/// Validates whether `total_frames` fits within the 32-bit RIFF/WAVE data chunk ceiling
/// (`(0xFFFFFFFF - 64) / (channels.max(1) * 4)`).
/// Exact port of the WAV ceiling check in `render_to_file` (`/native/dsp-host/src/host_commands.cpp`).
pub fn validate_offline_render_wav_ceiling(total_frames: u32, channels: u32) -> Result<u32, String> {
    let render_channels = channels.max(1);
    let max_render_frames = (0xFFFF_FFFFu32 - 64u32) / (render_channels * 4u32);
    if total_frames > max_render_frames {
        Err(format!(
            "Render of {} frames exceeds 32-bit WAV address ceiling ({})",
            total_frames, max_render_frames
        ))
    } else {
        Ok(max_render_frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_and_crossfade_preserve_delay_lines_while_cut_resets_source() {
        let mut decks = DualDeckCoordinator::new();
        // Initial load on Deck A (not yet audible -> becomes audible)
        let first = decks.load_deck(0, "C:\\Music\\track1.flac", 0.0);
        assert!(first.loaded);
        assert_eq!(first.assigned_deck, 0);
        assert_eq!(first.chain_reset, None);

        // Loading onto FEQ_DECK_HANDOFF while Deck A is playing never resets the chain
        let handoff = decks.load_deck(FEQ_DECK_HANDOFF, "C:\\Music\\track2.flac", 1.25);
        assert!(handoff.loaded);
        assert_eq!(handoff.assigned_deck, 1);
        assert_eq!(handoff.chain_reset, None);

        // Non-zero crossfade (3.0s) mixes both decks -> no chain reset
        let (curve, fade_reset) = decks.start_crossfade(1, 3.0, 2);
        assert_eq!(curve, FeqCrossfadeCurve::FEQ_CROSSFADE_LINEAR);
        assert_eq!(fade_reset, None);

        // Zero-length cut to an inaudible deck triggers FEQ_CHAIN_RESET_SOURCE_CHANGE
        decks.unload_deck(0);
        let cut_reset = decks.select_deck(0);
        assert_eq!(
            cut_reset,
            Some(FeqChainResetReason::FEQ_CHAIN_RESET_SOURCE_CHANGE)
        );
    }

    #[test]
    fn decodes_63_word_noise_profile_and_checks_wav_render_ceiling() {
        let cleared = decode_noise_profile_wire(&[]).expect("empty clears profile");
        assert!(cleared.is_none());

        let mut wire = vec![0.0; FEQ_DENOISE_PROFILE_WIRE];
        wire[0] = -62.5;
        wire[FEQ_DENOISE_PROFILE_BANDS] = -58.0; // floor_dbfs
        wire[FEQ_DENOISE_PROFILE_BANDS + 1] = 60.0; // hum_hz
        wire[FEQ_DENOISE_PROFILE_BANDS + 2] = 2.0; // hum_partial_count
        wire[FEQ_DENOISE_PROFILE_BANDS + 3] = 60.0;
        wire[FEQ_DENOISE_PROFILE_BANDS + 4] = 120.0;
        let profile = decode_noise_profile_wire(&wire)
            .expect("valid 63-word wire")
            .expect("some profile");
        assert_eq!(profile.bands_db[0], -62.5);
        assert_eq!(profile.floor_dbfs, -58.0);
        assert_eq!(profile.hum_hz, 60.0);
        assert_eq!(profile.hum_partial_count, 2);
        assert_eq!(profile.hum_partial_hz[1], 120.0);

        // Stereo 32-bit float WAV ceiling is (0xFFFFFFFF - 64) / 8 = 536,870,903 frames
        assert!(validate_offline_render_wav_ceiling(10_000_000, 2).is_ok());
        assert!(validate_offline_render_wav_ceiling(600_000_000, 2).is_err());
    }
}
