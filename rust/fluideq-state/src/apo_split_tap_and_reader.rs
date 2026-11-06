/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! System APO Secondary Output Shared-Memory Split Tap & Adaptive Clock-Drift Sinc Reader (`Slice-M1M8-8`).
//!
//! 1:1 faithful port of:
//! - `/native/system-apo/src/split_board.h` & `split_board.cpp` (`SplitState`, `SplitStats`, `SplitEndpoint`, `SplitBoard`, `split_claim`, `split_release`)
//! - `/native/system-apo/src/split_reader.h` & `split_reader.cpp` (`SplitKernel`, `SplitReader`)
//! - `/native/system-apo/src/split_tap.h` & `split_tap.cpp` (`SplitReport`, `SplitTap`, `is_output_rate`)

use crate::apo_engine_runtime::{split_role_of, SplitRole};
use crate::apo_split_and_leveling::{
    lfe_channel_of, speaker_of_channel, split_transport_name, SplitClock, SplitRing, ROOM_SPEAKERS,
    SPLIT_RING_FRAMES, SPLIT_RING_STRIDE,
};
use crate::remote_playback::{build_sinc_kernel, SINC_PHASES, SINC_TAPS};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

// ============================================================================
// 1. `SplitState`, `SplitStats`, `SplitEndpoint` & `SplitBoard` (`split_board.h/.cpp`)
// ============================================================================

/// Secondary output playback state reported in engine status files (`SplitState` in `split_board.h`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SplitState {
    /// Not fed: no line in `fluideq-split.txt` names this output.
    #[default]
    Off = 0,
    /// Fed, and nothing fresh has come from the main output to play.
    Waiting = 1,
    /// Actively playing the main output's audio stream in time with its clock.
    Playing = 2,
}

impl SplitState {
    pub fn from_u32(raw: u32) -> Self {
        match raw {
            1 => Self::Waiting,
            2 => Self::Playing,
            _ => Self::Off,
        }
    }

    pub fn as_status_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Waiting => "waiting",
            Self::Playing => "playing",
        }
    }
}

/// Shared status atomics across all APO instances on an endpoint (`SplitStats` in `split_board.h`).
#[derive(Debug, Default)]
pub struct SplitStats {
    pub state: AtomicU32,
    /// Configured reader buffering in microseconds, excluding DSP/device delay.
    pub lag_us: AtomicU32,
    /// Times the main output's sound was not there in time since locking.
    pub underruns: AtomicU32,
}

impl SplitStats {
    pub fn new() -> Self {
        Self {
            state: AtomicU32::new(SplitState::Off as u32),
            lag_us: AtomicU32::new(0),
            underruns: AtomicU32::new(0),
        }
    }
}

/// Process-lifetime endpoint entry shared between primary writer and secondary receivers
/// (`SplitEndpoint` in `split_board.h`).
pub struct SplitEndpoint {
    pub guid: String,
    pub mapping_name: String,
    /// Token of the `SplitTap` instance writing this output's sound into `ring` (`0` = none).
    pub writer: AtomicUsize,
    /// Token of the `SplitTap` instance mixing another output's sound into this one (`0` = none).
    pub reader: AtomicUsize,
    pub stats: SplitStats,
    ring: OnceLock<Arc<SplitRing>>,
}

impl SplitEndpoint {
    pub fn new(guid: impl Into<String>, mapping_name: impl Into<String>) -> Self {
        Self {
            guid: guid.into(),
            mapping_name: mapping_name.into(),
            writer: AtomicUsize::new(0),
            reader: AtomicUsize::new(0),
            stats: SplitStats::new(),
            ring: OnceLock::new(),
        }
    }

    /// Lock-free read of the endpoint's `SplitRing` once published by the control thread.
    pub fn ring(&self) -> Option<&Arc<SplitRing>> {
        self.ring.get()
    }

    /// Control thread: allocates the 2 MiB `SplitRing` on first use and publishes it.
    pub fn get_or_init_ring(&self) -> &Arc<SplitRing> {
        self.ring.get_or_init(|| Arc::new(SplitRing::new()))
    }
}

/// Lock-free claim of `slot` for `owner_token` (`split_claim` in `split_board.h`).
/// Returns `true` if `slot` was unclaimed (`0`) and is now owned by `owner_token`,
/// or if `owner_token` already holds `slot`.
pub fn split_claim(slot: &AtomicUsize, owner_token: usize) -> bool {
    if owner_token == 0 {
        return false;
    }
    match slot.compare_exchange(0, owner_token, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => true,
        Err(actual) => actual == owner_token,
    }
}

/// Lock-free release of `slot` if currently held by `owner_token` (`split_release` in `split_board.h`).
pub fn split_release(slot: &AtomicUsize, owner_token: usize) {
    if owner_token == 0 {
        return;
    }
    let _ = slot.compare_exchange(owner_token, 0, Ordering::AcqRel, Ordering::Acquire);
}

/// Process-lifetime board of `SplitEndpoint` and `SplitRing` instances (`Board` in `split_board.cpp`).
#[derive(Default)]
pub struct SplitBoard {
    outputs: Mutex<BTreeMap<String, Arc<SplitEndpoint>>>,
}

impl SplitBoard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Control thread only: retrieves or creates the `SplitEndpoint` for `guid`.
    /// Mirrors `fluideq_engine::split_endpoint` in `/native/system-apo/src/split_board.cpp`.
    pub fn split_endpoint(&self, guid: &str, config_identity: &str) -> Option<Arc<SplitEndpoint>> {
        if guid.is_empty() {
            return None;
        }
        let mapping = if config_identity.is_empty() {
            String::new()
        } else {
            split_transport_name(config_identity, guid).ok()?
        };
        let key = if mapping.is_empty() {
            guid.to_lowercase()
        } else {
            mapping.clone()
        };
        let mut guard = match self.outputs.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        Some(
            guard
                .entry(key)
                .or_insert_with(|| Arc::new(SplitEndpoint::new(guid, mapping)))
                .clone(),
        )
    }

    /// Control thread only: ensures `endpoint`'s `SplitRing` is allocated and returns it.
    /// Mirrors `fluideq_engine::split_ring` in `/native/system-apo/src/split_board.cpp`.
    pub fn split_ring(&self, endpoint: &Arc<SplitEndpoint>) -> Arc<SplitRing> {
        endpoint.get_or_init_ring().clone()
    }
}

// ============================================================================
// 2. `SplitKernel` & `SplitReader` (`/native/system-apo/src/split_reader.cpp`)
// ============================================================================

/// The kernel reads 31 frames behind a position and 32 past it (`kLead = 31`, `kTail = 33`).
pub const SPLIT_KERNEL_LEAD_FRAMES: f64 = 31.0;
pub const SPLIT_KERNEL_TAIL_FRAMES: f64 = 33.0;

pub const SPLIT_MARGIN_START_SECONDS: f64 = 0.002;
pub const SPLIT_MARGIN_STEP_SECONDS: f64 = 0.001;
pub const SPLIT_MARGIN_MOST_SECONDS: f64 = 0.040;
pub const SPLIT_MARGIN_GIVE_BACK_SECONDS: f64 = 0.0005;
pub const SPLIT_STEADY_SECONDS: f64 = 30.0;
pub const SPLIT_HOLD_SECONDS: f64 = 2.0;
pub const SPLIT_LEARN_SECONDS: f64 = 10.0;
pub const SPLIT_MOST_TRIM: f64 = 0.005;
pub const SPLIT_LAG_SECONDS: f64 = 0.5;
pub const SPLIT_TOO_FAR_SECONDS: f64 = 0.050;
pub const SPLIT_LATE_BLOCKS: f64 = 3.0;
pub const SPLIT_FADE_SECONDS: f64 = 0.005;
pub const SPLIT_VOLUME_SECONDS: f64 = 0.020;
const HALF_POWER: f32 = 0.70710678;

/// Windowed-sinc interpolation table (`1025 * 64 = 65,600` floats, ~260 kB) for one `(source_rate, target_rate)` pair.
/// Built on the watcher control thread and shared lock-free with the audio thread.
///
/// Mirrors `fluideq_engine::SplitKernel` in `/native/system-apo/src/split_reader.h/.cpp`.
#[derive(Debug, Clone)]
pub struct SplitKernel {
    source_rate: u32,
    target_rate: u32,
    table: Vec<f32>,
}

impl SplitKernel {
    pub fn new(source_rate: u32, target_rate: u32) -> Self {
        let ratio = if source_rate > 0 {
            f64::from(target_rate) / f64::from(source_rate)
        } else {
            1.0
        };
        Self {
            source_rate,
            target_rate,
            table: build_sinc_kernel(ratio),
        }
    }

    pub fn source_rate(&self) -> u32 {
        self.source_rate
    }

    pub fn target_rate(&self) -> u32 {
        self.target_rate
    }

    pub fn table(&self) -> &[f32] {
        &self.table
    }
}

/// Secondary output adaptive resampler and clock-drift servo reader.
///
/// Mirrors `fluideq_engine::SplitReader` in `/native/system-apo/src/split_reader.h/.cpp`.
#[derive(Debug, Clone)]
pub struct SplitReader {
    rate: u32,
    channels: u32,
    mask: u32,
    clock: SplitClock,
    have_clock: bool,
    generation: u32,
    synced: bool,
    state: SplitState,
    wanted_rate: u32,
    late_pending: bool,
    late_from: i64,
    position: f64,
    margin: f64,
    lag: f64,
    drift: f64,
    steady_frames: u64,
    gain: f32,
    fade: f32,
    matrix: [[f32; SPLIT_RING_STRIDE]; SPLIT_RING_STRIDE],
    last: [f32; SPLIT_RING_STRIDE],
}

impl SplitReader {
    pub fn new(rate: u32, channels: u32, mask: u32) -> Self {
        let mut reader = Self {
            rate,
            channels: channels.min(SPLIT_RING_STRIDE as u32),
            mask,
            clock: SplitClock::default(),
            have_clock: false,
            generation: 0,
            synced: false,
            state: SplitState::Off,
            wanted_rate: 0,
            late_pending: false,
            late_from: 0,
            position: 0.0,
            margin: SPLIT_MARGIN_START_SECONDS,
            lag: 0.0,
            drift: 0.0,
            steady_frames: 0,
            gain: 0.0,
            fade: 0.0,
            matrix: [[0.0; SPLIT_RING_STRIDE]; SPLIT_RING_STRIDE],
            last: [0.0; SPLIT_RING_STRIDE],
        };
        reader.restart();
        reader
    }

    /// Starts over, as for a new main output (`SplitReader::restart` in `split_reader.cpp`).
    /// Note: just like C++ `restart()`, `gain_` is intentionally preserved while `fade_` resets to `0.0`.
    pub fn restart(&mut self) {
        self.have_clock = false;
        self.generation = 0;
        self.synced = false;
        self.state = SplitState::Off;
        self.wanted_rate = 0;
        self.late_pending = false;
        self.late_from = 0;
        self.position = 0.0;
        self.margin = SPLIT_MARGIN_START_SECONDS;
        self.lag = 0.0;
        self.drift = 0.0;
        self.steady_frames = 0;
        self.fade = 0.0;
        self.last = [0.0; SPLIT_RING_STRIDE];
        self.matrix = [[0.0; SPLIT_RING_STRIDE]; SPLIT_RING_STRIDE];
    }

    pub fn faded(&self) -> bool {
        self.fade <= 0.0
    }

    pub fn wanted_rate(&self) -> u32 {
        self.wanted_rate
    }

    pub fn margin_seconds(&self) -> f64 {
        self.margin
    }

    pub fn drift_ratio(&self) -> f64 {
        self.drift
    }

    pub fn matrix(&self) -> &[[f32; SPLIT_RING_STRIDE]; SPLIT_RING_STRIDE] {
        &self.matrix
    }

    fn become_state(&mut self, state: SplitState, stats: &SplitStats) -> bool {
        if state == self.state {
            return false;
        }
        self.state = state;
        stats.state.store(state as u32, Ordering::Relaxed);
        true
    }

    /// Builds the channel routing/fold-down matrix between `clock` (source) and `(self.channels, self.mask)` (target).
    /// Exact 1:1 port of `SplitReader::build_matrix` in `/native/system-apo/src/split_reader.cpp` (lines 104–188).
    pub fn build_matrix(&mut self, clock: &SplitClock) {
        self.matrix = [[0.0; SPLIT_RING_STRIDE]; SPLIT_RING_STRIDE];
        let in_ch = (clock.channels as u16).min(SPLIT_RING_STRIDE as u16);
        let out_ch = (self.channels as u16).min(SPLIT_RING_STRIDE as u16);
        if in_ch == out_ch && clock.mask == self.mask {
            for ch in 0..(out_ch as usize) {
                self.matrix[ch][ch] = 1.0;
            }
            return;
        }

        // The second output's channel for each of the room's speakers, or -1.
        let mut to = [-1i32; ROOM_SPEAKERS];
        for ch in 0..u32::from(out_ch) {
            let speaker = speaker_of_channel(self.mask, out_ch, ch);
            if speaker >= 0 && (speaker as usize) < ROOM_SPEAKERS && to[speaker as usize] < 0 {
                to[speaker as usize] = ch as i32;
            }
        }

        if out_ch == 1 {
            // A mono output: the fronts at half each, the centre at half power and
            // the surrounds at 0.35, which keeps a 5.1 mix's balance in one speaker.
            for ch in 0..u32::from(in_ch) {
                let speaker = speaker_of_channel(clock.mask, in_ch, ch);
                self.matrix[0][ch as usize] = if in_ch == 1 {
                    1.0
                } else if speaker <= 1 {
                    if speaker < 0 {
                        0.0
                    } else {
                        0.5
                    }
                } else if speaker == 2 {
                    HALF_POWER
                } else {
                    0.35
                };
            }
            return;
        }

        let mut route = |from: usize, speaker: i32, weight: f32| -> bool {
            if speaker >= 0 && (speaker as usize) < ROOM_SPEAKERS {
                let target = to[speaker as usize];
                if target >= 0 {
                    self.matrix[target as usize][from] += weight;
                    return true;
                }
            }
            false
        };

        let lfe = lfe_channel_of(clock.mask, in_ch);
        for ch in 0..u32::from(in_ch) {
            if (ch as i32) == lfe {
                continue; // Its bass is in the main channels already, as in any fold.
            }
            let speaker = if in_ch == 1 {
                -2
            } else {
                speaker_of_channel(clock.mask, in_ch, ch)
            };
            let from = ch as usize;
            if speaker == -2 {
                // Mono: both fronts, at half power each.
                route(from, 0, HALF_POWER);
                route(from, 1, HALF_POWER);
                continue;
            }
            if speaker < 0 {
                // A position the layout has no name for keeps its place, if it can.
                if ch < u32::from(out_ch) && speaker_of_channel(self.mask, out_ch, ch) < 0 {
                    self.matrix[from][from] = 1.0;
                }
                continue;
            }
            if route(from, speaker, 1.0) {
                continue;
            }
            // The second output has no such speaker: the nearest one it has.
            let left = speaker == 0 || speaker == 3 || speaker == 5;
            match speaker {
                2 => {
                    route(from, 0, HALF_POWER);
                    route(from, 1, HALF_POWER);
                }
                3 | 4 => {
                    if !route(from, speaker + 2, 1.0) {
                        route(from, if left { 0 } else { 1 }, HALF_POWER);
                    }
                }
                5 | 6 => {
                    if !route(from, speaker - 2, 1.0) {
                        route(from, if left { 0 } else { 1 }, HALF_POWER);
                    }
                }
                _ => {
                    route(from, 2, HALF_POWER);
                }
            }
        }
    }

    fn fade_out(&mut self, planes: &mut [&mut [f32]], frames: u32) {
        if self.rate == 0 {
            self.fade = 0.0;
            return;
        }
        let step = (1.0 / (SPLIT_FADE_SECONDS * f64::from(self.rate))) as f32;
        let ch_count = (self.channels as usize).min(planes.len());
        for frame in 0..(frames as usize) {
            if self.fade <= 0.0 {
                break;
            }
            self.fade = (self.fade - step).max(0.0);
            for ch in 0..ch_count {
                if frame < planes[ch].len() {
                    planes[ch][frame] += self.last[ch] * self.gain * self.fade;
                }
            }
        }
    }

    /// Audio thread: nothing to read — fades out what was playing; returns `true` on a state change.
    /// Mirrors `SplitReader::idle` in `/native/system-apo/src/split_reader.cpp`.
    pub fn idle(&mut self, planes: &mut [&mut [f32]], frames: u32, stats: &SplitStats) -> bool {
        self.synced = false;
        self.fade_out(planes, frames);
        self.become_state(SplitState::Waiting, stats)
    }

    /// Audio thread: resamples and mixes `ring` into `planes`, steering fractional position by up to
    /// `±0.5%` (`SPLIT_MOST_TRIM`) to track the writer's QPC clock (`now`, `ticks_per_second`).
    ///
    /// Exact 1:1 port of `SplitReader::mix` in `/native/system-apo/src/split_reader.cpp` (lines 207–387).
    #[allow(clippy::too_many_arguments)]
    pub fn mix(
        &mut self,
        ring: &SplitRing,
        kernel: Option<&SplitKernel>,
        volume: f32,
        planes: &mut [&mut [f32]],
        frames: u32,
        now: i64,
        ticks_per_second: i64,
        stats: &SplitStats,
    ) -> bool {
        if let Some(read) = ring.clock() {
            self.clock = read;
            self.have_clock = true;
        }
        if !self.have_clock
            || self.clock.rate == 0
            || self.clock.channels == 0
            || (self.clock.channels as usize) > SPLIT_RING_STRIDE
            || ticks_per_second <= 0
            || self.rate == 0
            || self.channels == 0
        {
            self.synced = false;
            self.fade_out(planes, frames);
            return self.become_state(SplitState::Waiting, stats);
        }

        let Some(k) = kernel.filter(|k| k.source_rate() == self.clock.rate) else {
            let asked = self.wanted_rate != self.clock.rate;
            self.wanted_rate = self.clock.rate;
            self.synced = false;
            self.fade_out(planes, frames);
            return self.become_state(SplitState::Waiting, stats) || asked;
        };
        self.wanted_rate = 0;

        if self.clock.generation != self.generation {
            self.generation = self.clock.generation;
            let clk = self.clock;
            self.build_matrix(&clk);
            self.synced = false;
        }

        let source_rate = f64::from(self.clock.rate);
        let since = (now.wrapping_sub(self.clock.ticks) as f64) / (ticks_per_second as f64);
        let block = (self.clock.end.saturating_sub(self.clock.block_start) as f64) / source_rate;
        let mut changed = false;

        // Judge whether a previous missed block was a late writer (grow margin) or a stopped stream (no fault).
        if self.late_pending && self.clock.ticks != self.late_from {
            self.late_pending = false;
            let gap =
                (self.clock.ticks.wrapping_sub(self.late_from) as f64) / (ticks_per_second as f64);
            if gap < SPLIT_LATE_BLOCKS * block {
                self.margin = (self.margin + SPLIT_MARGIN_STEP_SECONDS).min(SPLIT_MARGIN_MOST_SECONDS);
                stats.underruns.fetch_add(1, Ordering::Relaxed);
                changed = true;
            }
        }

        // Nothing written for 2 blocks + start margin: the main output has stopped.
        if since > 2.0 * block + SPLIT_MARGIN_START_SECONDS || since < -block {
            self.synced = false;
            self.fade_out(planes, frames);
            return self.become_state(SplitState::Waiting, stats) || changed;
        }

        let writer_at = (self.clock.block_start as f64) + since * source_rate;
        let ratio = source_rate / f64::from(self.rate);
        let span = f64::from(frames) * ratio * (1.0 + SPLIT_MOST_TRIM) + SPLIT_KERNEL_TAIL_FRAMES;

        if !self.synced {
            if self.fade > 0.0 {
                self.fade_out(planes, frames);
                return self.become_state(SplitState::Waiting, stats) || changed;
            }
            self.position = writer_at - (span + self.margin * source_rate);
            self.lag = writer_at - self.position;
            self.steady_frames = 0;
            self.synced = true;
        }

        let oldest =
            ((self.clock.end as f64) - (SPLIT_RING_FRAMES as f64)).max(0.0) + SPLIT_KERNEL_LEAD_FRAMES;
        let is_unreadable = |pos: f64| -> bool {
            pos + span > (self.clock.end as f64) || pos < oldest
        };
        let late = self.position + span > (self.clock.end as f64);
        if is_unreadable(self.position)
            || writer_at - self.position
                > span + (self.margin + SPLIT_TOO_FAR_SECONDS) * source_rate
        {
            if late && self.state == SplitState::Playing && !self.late_pending {
                self.late_pending = true;
                self.late_from = self.clock.ticks;
            }
            changed = true;
            self.synced = false;
            if self.fade > 0.0 {
                self.fade_out(planes, frames);
                return self.become_state(SplitState::Waiting, stats) || changed;
            }
            self.position = writer_at - (span + self.margin * source_rate);
            self.lag = writer_at - self.position;
            self.steady_frames = 0;
            self.synced = true;
            if is_unreadable(self.position) {
                self.synced = false;
                self.fade_out(planes, frames);
                return self.become_state(SplitState::Waiting, stats) || changed;
            }
        }

        self.lag += (writer_at - self.position - self.lag)
            * (f64::from(frames) / (SPLIT_LAG_SECONDS * f64::from(self.rate))).min(1.0);
        let target = span + self.margin * source_rate;
        let error = (self.lag - target) / (source_rate * SPLIT_HOLD_SECONDS);
        self.drift = (self.drift
            + error * f64::from(frames) / (SPLIT_LEARN_SECONDS * f64::from(self.rate)))
        .clamp(-SPLIT_MOST_TRIM, SPLIT_MOST_TRIM);
        let trim = (error + self.drift).clamp(-SPLIT_MOST_TRIM, SPLIT_MOST_TRIM);
        let step = ratio * (1.0 + trim);

        let table = k.table();
        let fade_step = (1.0 / (SPLIT_FADE_SECONDS * f64::from(self.rate))) as f32;
        let volume_step = (1.0 / (SPLIT_VOLUME_SECONDS * f64::from(self.rate))) as f32;
        let first = self.position;
        let mut weights = [0.0f32; SINC_TAPS];
        let mut input = [0.0f32; SPLIT_RING_STRIDE];
        let in_channels = self.clock.channels as usize;
        let out_channels = (self.channels as usize).min(planes.len());

        for frame in 0..(frames as usize) {
            let whole = self.position.floor();
            let phase = (self.position - whole) * (SINC_PHASES as f64);
            let lower = (phase as usize).min(SINC_PHASES - 1);
            let blend = (phase - (lower as f64)) as f32;
            let row_a = lower * SINC_TAPS;
            let row_b = row_a + SINC_TAPS;
            for tap in 0..SINC_TAPS {
                let a = table[row_a + tap];
                let b = table[row_b + tap];
                weights[tap] = a + (b - a) * blend;
            }
            let origin = (whole as i64).wrapping_sub(31) as u64;
            for ch in 0..in_channels {
                let mut val = 0.0f32;
                for tap in 0..SINC_TAPS {
                    val += ring.at(origin.wrapping_add(tap as u64), ch as u32) * weights[tap];
                }
                input[ch] = val;
            }
            self.fade = (self.fade + fade_step).min(1.0);
            self.gain = if self.gain < volume {
                (self.gain + volume_step).min(volume)
            } else {
                (self.gain - volume_step).max(volume)
            };
            for out_ch in 0..out_channels {
                let mut sample = 0.0f32;
                for ch in 0..in_channels {
                    sample += self.matrix[out_ch][ch] * input[ch];
                }
                self.last[out_ch] = sample;
                if frame < planes[out_ch].len() {
                    planes[out_ch][frame] += sample * self.gain * self.fade;
                }
            }
            self.position += step;
        }

        // Check if the writer lapped our starting frame while we were reading.
        if (ring.end() as f64) - (SPLIT_RING_FRAMES as f64) + SPLIT_KERNEL_LEAD_FRAMES > first {
            self.synced = false;
        }

        self.steady_frames += u64::from(frames);
        if self.steady_frames >= (f64::from(self.rate) * SPLIT_STEADY_SECONDS) as u64 {
            self.steady_frames = 0;
            if self.margin > SPLIT_MARGIN_START_SECONDS {
                self.margin =
                    (self.margin - SPLIT_MARGIN_GIVE_BACK_SECONDS).max(SPLIT_MARGIN_START_SECONDS);
            }
        }

        let lag_us = (((span + self.margin * source_rate) / source_rate) * 1e6).round() as u32;
        let told = stats.lag_us.load(Ordering::Relaxed);
        if lag_us > told.saturating_add(250) || told > lag_us.saturating_add(250) {
            stats.lag_us.store(lag_us, Ordering::Relaxed);
            changed = true;
        }
        self.become_state(SplitState::Playing, stats) || changed
    }
}

// ============================================================================
// 3. `SplitReport`, `is_output_rate` & `SplitTap` (`/native/system-apo/src/split_tap.cpp`)
// ============================================================================

/// Standard Windows audio output sample rates (`kRates` in `/native/system-apo/src/split_tap.cpp`).
/// Bounds `SplitTap::ensure_kernel` so a corrupted shared-memory rate cannot allocate unbounded 260 kB tables.
pub const WINDOWS_OUTPUT_RATES: [u32; 15] = [
    8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000,
    352_800, 384_000, 705_600, 768_000,
];

/// Returns `true` if `rate` is one of the 15 supported Windows audio output sample rates.
pub fn is_output_rate(rate: u32) -> bool {
    WINDOWS_OUTPUT_RATES.contains(&rate)
}

/// Secondary output status report (`SplitReport` in `split_tap.h`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitReport {
    pub from: String,
    pub state: SplitState,
    pub lag_us: u32,
    pub underruns: u32,
}

static NEXT_SPLIT_TAP_TOKEN: AtomicUsize = AtomicUsize::new(1);

/// One APO engine instance's participation in secondary-output sharing:
/// writing its output's sound for another endpoint to play, playing another's, or neither.
///
/// Mirrors `fluideq_engine::SplitTap` in `/native/system-apo/src/split_tap.h/.cpp`.
pub struct SplitTap {
    instance_token: usize,
    endpoint: String,
    config_identity: String,
    own: Option<Arc<SplitEndpoint>>,
    rate: u32,
    channels: u32,
    mask: u32,
    default_mode: bool,
    ticks_per_second: i64,

    // Published by watcher thread (`follow`/`prepare`), snapshotted by audio thread (`begin`).
    route: Mutex<Option<Arc<SplitEndpoint>>>,
    source_active: AtomicBool,
    volume_bits: AtomicU32,
    active_kernel: Mutex<Option<Arc<SplitKernel>>>,
    wanted_rate: AtomicU32,

    // Audio thread state.
    reader: SplitReader,
    route_at_block: Option<Arc<SplitEndpoint>>,
    source_at_block: bool,
    reading: Option<Arc<SplitEndpoint>>,
    mixing: bool,
    writing: bool,
    output_gain: f32,

    // Watcher thread state.
    from: String,
    source_enabled: bool,
    transport_failed: bool,
    kernels: Vec<Arc<SplitKernel>>,
}

impl SplitTap {
    pub fn new(
        board: &SplitBoard,
        endpoint: impl Into<String>,
        rate: u32,
        channels: u32,
        mask: u32,
        default_mode: bool,
        ticks_per_second: i64,
        config_identity: impl Into<String>,
    ) -> Self {
        let endpoint_str = endpoint.into();
        let config_id_str = config_identity.into();
        let own = board.split_endpoint(&endpoint_str, &config_id_str);
        let token = NEXT_SPLIT_TAP_TOKEN.fetch_add(1, Ordering::Relaxed);
        Self {
            instance_token: token,
            endpoint: endpoint_str,
            config_identity: config_id_str,
            own,
            rate,
            channels,
            mask,
            default_mode,
            ticks_per_second,
            route: Mutex::new(None),
            source_active: AtomicBool::new(false),
            volume_bits: AtomicU32::new(1.0_f32.to_bits()),
            active_kernel: Mutex::new(None),
            wanted_rate: AtomicU32::new(0),
            reader: SplitReader::new(rate, channels, mask),
            route_at_block: None,
            source_at_block: false,
            reading: None,
            mixing: false,
            writing: false,
            output_gain: 1.0,
            from: String::new(),
            source_enabled: false,
            transport_failed: false,
            kernels: Vec::new(),
        }
    }

    /// Watcher thread: updates role from `fluideq-split.txt` (`text`) and `owner` liveness.
    /// Returns `true` when engine status JSON should be rewritten.
    ///
    /// Mirrors `SplitTap::follow` in `/native/system-apo/src/split_tap.cpp` (lines 64–127).
    pub fn follow(&mut self, text: Option<&str>, owner: bool, board: &SplitBoard) -> bool {
        let was_failed = self.transport_failed;
        let was_source = self.source_enabled;

        self.transport_failed = self.own.is_none() && !self.endpoint.is_empty();
        let role: SplitRole = match (owner, text, self.own.as_ref()) {
            (true, Some(txt), Some(_)) => split_role_of(txt, &self.endpoint),
            _ => SplitRole::default(),
        };

        let changed = role.from != self.from;
        self.from = role.from.clone();
        if !self.default_mode {
            return changed;
        }

        let source: Option<Arc<SplitEndpoint>> = if role.primary {
            self.own.clone()
        } else if role.is_target() {
            board.split_endpoint(&role.from, &self.config_identity)
        } else {
            None
        };
        if role.is_target() && source.is_none() {
            self.transport_failed = true;
        }
        self.source_active.store(role.source, Ordering::Relaxed);

        if role.primary {
            // Publish primary protection before ring allocation so a former receiver's fade/trim is never inherited.
            if let Ok(mut g) = self.route.lock() {
                *g = self.own.clone();
            }
            if role.source
                && let Some(ref own_ep) = self.own
            {
                let _ = board.split_ring(own_ep);
            }
        }

        let enabled = role.source && self.own.as_ref().and_then(|ep| ep.ring()).is_some();
        self.source_enabled = enabled;
        self.volume_bits
            .store(role.volume.to_bits(), Ordering::Relaxed);

        if let (Some(src), Some(own_ep)) = (source.as_ref(), self.own.as_ref())
            && !Arc::ptr_eq(src, own_ep)
        {
            let ring = board.split_ring(src);
            if let Some(clk) = ring.clock()
                && clk.rate != 0
            {
                self.ensure_kernel(clk.rate);
            }
        }

        self.prepare();
        if let Ok(mut g) = self.route.lock() {
            *g = source;
        }

        changed || was_source != self.source_enabled || was_failed != self.transport_failed
    }

    /// Watcher thread: builds the `SplitKernel` for `wanted_rate` if requested by the audio thread.
    /// Mirrors `SplitTap::prepare` in `/native/system-apo/src/split_tap.cpp`.
    pub fn prepare(&mut self) {
        let rate = self.wanted_rate.load(Ordering::Relaxed);
        if rate != 0 {
            self.ensure_kernel(rate);
        }
    }

    fn ensure_kernel(&mut self, rate: u32) {
        if !is_output_rate(rate) {
            return;
        }
        for existing in &self.kernels {
            if existing.source_rate() == rate {
                if let Ok(mut g) = self.active_kernel.lock() {
                    *g = Some(existing.clone());
                }
                return;
            }
        }
        let built = Arc::new(SplitKernel::new(rate, self.rate));
        self.kernels.push(built.clone());
        if let Ok(mut g) = self.active_kernel.lock() {
            *g = Some(built);
        }
    }

    pub fn transport_failed(&self) -> bool {
        self.transport_failed
    }

    /// Watcher thread: returns `Some(SplitReport)` when this instance is configured as a secondary receiver.
    /// Note that `SplitState::Off` is reported as `SplitState::Waiting` ("Asked to play and not started yet is waiting").
    ///
    /// Mirrors `SplitTap::report` in `/native/system-apo/src/split_tap.cpp` (lines 173–186).
    pub fn report(&self) -> Option<SplitReport> {
        let own = self.own.as_ref()?;
        if self.from.is_empty() {
            return None;
        }
        let raw_state = SplitState::from_u32(own.stats.state.load(Ordering::Relaxed));
        let state = if raw_state == SplitState::Off {
            SplitState::Waiting
        } else {
            raw_state
        };
        Some(SplitReport {
            from: self.from.clone(),
            state,
            lag_us: own.stats.lag_us.load(Ordering::Relaxed),
            underruns: own.stats.underruns.load(Ordering::Relaxed),
        })
    }

    /// Audio thread: snapshots the role once at the start of the block (`SplitTap::begin`).
    /// Ensures a single block can never write as primary and mix as a receiver simultaneously.
    pub fn begin(&mut self) {
        self.route_at_block = self.route.lock().ok().and_then(|g| g.clone());
        self.source_at_block = self.source_active.load(Ordering::Relaxed);
        let primary = match (&self.own, &self.route_at_block) {
            (Some(own), Some(route)) => Arc::ptr_eq(own, route),
            _ => false,
        };
        if primary {
            self.output_gain = 1.0;
        } else if self.route_at_block.is_some() && self.reading.is_none() {
            self.output_gain = f32::from_bits(self.volume_bits.load(Ordering::Relaxed));
        }
        if primary && self.reading.is_some() {
            self.reader.restart();
            self.reading = None;
            if self.mixing {
                if let Some(ref own) = self.own {
                    own.stats
                        .state
                        .store(SplitState::Off as u32, Ordering::Relaxed);
                    split_release(&own.reader, self.instance_token);
                }
                self.mixing = false;
            }
        }
    }

    /// Audio thread: returns `true` if `SplitTap` has any active writing, reading, or trim ramp work.
    pub fn active(&self) -> bool {
        let route = self.route.lock().ok().and_then(|g| g.clone());
        let route_active = match (&self.own, route.as_ref()) {
            (Some(own), Some(r)) => {
                !Arc::ptr_eq(own, r) || self.source_active.load(Ordering::Relaxed)
            }
            (None, Some(_)) => true,
            _ => false,
        };
        self.writing || self.reading.is_some() || (self.output_gain - 1.0).abs() > f32::EPSILON || route_active
    }

    /// Audio thread: returns `true` when `mix` or `trim_output` has work on this output.
    pub fn reading(&self) -> bool {
        let is_own = match (&self.own, &self.route_at_block) {
            (Some(own), Some(r)) => Arc::ptr_eq(own, r),
            _ => false,
        };
        !is_own
            && (self.reading.is_some()
                || self.route_at_block.is_some()
                || (self.output_gain - 1.0).abs() > f32::EPSILON)
    }

    /// Audio thread after `begin()`: writes raw interleaved input (or silence if `None`) when acting as primary source.
    /// Mirrors `SplitTap::write` in `/native/system-apo/src/split_tap.cpp`.
    pub fn write(&mut self, input: Option<&[f32]>, frames: u32, ticks: i64) -> bool {
        let Some(ref own) = self.own else {
            return false;
        };
        let primary = self
            .route_at_block
            .as_ref()
            .is_some_and(|r| Arc::ptr_eq(own, r));
        let ring = if primary && self.source_at_block {
            own.ring()
        } else {
            None
        };
        let Some(ring) = ring else {
            if self.writing {
                split_release(&own.writer, self.instance_token);
                self.writing = false;
            }
            return false;
        };
        if !self.writing {
            self.writing = split_claim(&own.writer, self.instance_token);
            if !self.writing {
                return false;
            }
        }
        ring.write(input, self.channels, frames, self.rate, self.mask, ticks)
    }

    /// Audio thread: mixes raw sound from the primary output's `SplitRing` into `planes` before this endpoint's EQ graph.
    /// Returns `true` when the watcher thread should be woken (status changed or new sample rate kernel wanted).
    ///
    /// Mirrors `SplitTap::mix` in `/native/system-apo/src/split_tap.cpp` (lines 238–282).
    pub fn mix(&mut self, planes: &mut [&mut [f32]], frames: u32, now: i64) -> bool {
        let Some(own) = self.own.clone() else {
            return false;
        };
        let from = self.route_at_block.clone();
        if from.as_ref().is_some_and(|f| Arc::ptr_eq(&own, f)) {
            return false;
        }
        let Some(from_ep) = from else {
            // Stopped: fade out from where it was, never cut, and only then let go of the reader claim.
            if self.mixing {
                self.reader.idle(planes, frames, &own.stats);
                if !self.reader.faded() {
                    return false;
                }
                own.stats
                    .state
                    .store(SplitState::Off as u32, Ordering::Relaxed);
                split_release(&own.reader, self.instance_token);
                self.mixing = false;
            }
            self.reading = None;
            self.reader.restart();
            return false;
        };

        let switched_source = self
            .reading
            .as_ref()
            .is_none_or(|r| !Arc::ptr_eq(r, &from_ep));
        if switched_source {
            self.reading = Some(from_ep.clone());
            self.reader.restart();
        }
        if !self.mixing {
            self.mixing = split_claim(&own.reader, self.instance_token);
            if !self.mixing {
                return false;
            }
            own.stats.underruns.store(0, Ordering::Relaxed);
            own.stats.lag_us.store(0, Ordering::Relaxed);
        }

        let Some(ring) = from_ep.ring() else {
            return self.reader.idle(planes, frames, &own.stats);
        };
        let kernel = self.active_kernel.lock().ok().and_then(|g| g.clone());
        let wake = self.reader.mix(
            ring,
            kernel.as_deref(),
            1.0,
            planes,
            frames,
            now,
            self.ticks_per_second,
            &own.stats,
        );
        self.wanted_rate
            .store(self.reader.wanted_rate(), Ordering::Relaxed);
        wake
    }

    /// Audio thread: applies the secondary output's listening volume trim (`20 ms` linear ramp)
    /// *after* this endpoint's DSP rack so the leveler and full-file source measurements are never distorted.
    ///
    /// Mirrors `SplitTap::trim_output` in `/native/system-apo/src/split_tap.cpp` (lines 284–301).
    pub fn trim_output(&mut self, planes: &mut [&mut [f32]], frames: u32) {
        let is_own = match (&self.own, &self.route_at_block) {
            (Some(own), Some(r)) => Arc::ptr_eq(own, r),
            _ => false,
        };
        if is_own || self.rate == 0 {
            return;
        }
        let target = if self.route_at_block.is_some() {
            f32::from_bits(self.volume_bits.load(Ordering::Relaxed))
        } else {
            1.0
        };
        let step = (1.0 / (0.020 * f64::from(self.rate))) as f32;
        let ch_count = (self.channels as usize).min(planes.len());
        for frame in 0..(frames as usize) {
            self.output_gain = if self.output_gain < target {
                (self.output_gain + step).min(target)
            } else {
                (self.output_gain - step).max(target)
            };
            for ch in 0..ch_count {
                if frame < planes[ch].len() {
                    planes[ch][frame] *= self.output_gain;
                }
            }
        }
    }
}

impl Drop for SplitTap {
    fn drop(&mut self) {
        if let Some(ref own) = self.own {
            split_release(&own.writer, self.instance_token);
            if self.mixing {
                own.stats
                    .state
                    .store(SplitState::Off as u32, Ordering::Relaxed);
            }
            split_release(&own.reader, self.instance_token);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_rate_allowlist_bounds_kernel_allocations() {
        assert!(is_output_rate(44_100));
        assert!(is_output_rate(48_000));
        assert!(is_output_rate(192_000));
        assert!(is_output_rate(768_000));
        assert!(!is_output_rate(0));
        assert!(!is_output_rate(48_001));
        assert!(!is_output_rate(999_999));
    }

    #[test]
    fn split_reader_matrix_folds_5_1_to_stereo_and_mono_accurately() {
        // 5.1 mask (0x3F: FL, FR, FC, LFE, BL, BR) -> Stereo (2ch, mask 0x3)
        let mut stereo_reader = SplitReader::new(48_000, 2, 0x3);
        let clk_5_1 = SplitClock {
            end: 480,
            block_start: 0,
            ticks: 1_000,
            rate: 48_000,
            channels: 6,
            mask: 0x3F,
            generation: 1,
        };
        stereo_reader.build_matrix(&clk_5_1);
        let m = stereo_reader.matrix();
        // FL -> L (1.0), FR -> R (1.0), FC -> L & R (0.7071), LFE (ch 3) -> skipped (0.0), BL (ch 4) -> L (0.7071), BR (ch 5) -> R (0.7071)
        assert!((m[0][0] - 1.0).abs() < 1e-6);
        assert!((m[1][1] - 1.0).abs() < 1e-6);
        assert!((m[0][2] - HALF_POWER).abs() < 1e-6);
        assert!((m[1][2] - HALF_POWER).abs() < 1e-6);
        assert_eq!(m[0][3], 0.0);
        assert_eq!(m[1][3], 0.0);
        assert!((m[0][4] - HALF_POWER).abs() < 1e-6);
        assert!((m[1][5] - HALF_POWER).abs() < 1e-6);

        // 5.1 -> Mono (1ch): fronts at 0.5, center at 0.7071, LFE at 0.0, surrounds at 0.35
        let mut mono_reader = SplitReader::new(48_000, 1, 0);
        mono_reader.build_matrix(&clk_5_1);
        let mm = mono_reader.matrix();
        assert!((mm[0][0] - 0.5).abs() < 1e-6);
        assert!((mm[0][1] - 0.5).abs() < 1e-6);
        assert!((mm[0][2] - HALF_POWER).abs() < 1e-6);
        assert_eq!(mm[0][3], 0.0);
        assert!((mm[0][4] - 0.35).abs() < 1e-6);
        assert!((mm[0][5] - 0.35).abs() < 1e-6);
    }

    #[test]
    fn split_tap_koordinieren_primary_write_and_secondary_mix_with_trim() {
        let board = SplitBoard::new();
        let main_guid = "{11111111-2222-3333-4444-555555555555}";
        let sec_guid = "{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}";
        let split_txt = format!(
            "# main {}\n{} {} 0.5\n",
            main_guid, main_guid, sec_guid
        );

        let mut main_tap = SplitTap::new(&board, main_guid, 48_000, 2, 0x3, true, 1_000_000, "");
        let mut sec_tap = SplitTap::new(&board, sec_guid, 48_000, 2, 0x3, true, 1_000_000, "");

        assert!(main_tap.follow(Some(&split_txt), true, &board));
        assert!(sec_tap.follow(Some(&split_txt), true, &board));

        // Before first block is mixed, secondary report shows `Waiting`
        let rep0 = sec_tap.report().expect("secondary has SplitReport");
        assert_eq!(rep0.from, main_guid);
        assert_eq!(rep0.state, SplitState::Waiting);

        // Main writes 6 blocks of 480 frames (60 ms total) at 10 ms QPC intervals
        let block_interleaved = vec![0.4f32; 480 * 2];
        for b in 0..6 {
            main_tap.begin();
            assert!(main_tap.write(Some(&block_interleaved), 480, (b + 1) * 10_000));
        }

        // Secondary mixes 480 frames at t = 62_000 us (2 ms after the 6th block arrived)
        sec_tap.begin();
        assert!(sec_tap.reading());
        let mut left = vec![0.0f32; 480];
        let mut right = vec![0.0f32; 480];
        {
            let mut planes: [&mut [f32]; 2] = [&mut left, &mut right];
            let woke = sec_tap.mix(&mut planes, 480, 62_000);
            assert!(woke); // Transitioned Waiting -> Playing and prepared/used 48kHz kernel
            sec_tap.trim_output(&mut planes, 480);
        }

        let rep1 = sec_tap.report().unwrap();
        assert_eq!(rep1.state, SplitState::Playing);
        assert!(rep1.lag_us > 10_000);
        assert!(left.iter().any(|&x| x.abs() > 0.05));
    }
}
