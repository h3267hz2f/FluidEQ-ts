/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! System APO Graph Handover, Input History, Level Prediction, Output Guard,
//! Block Timing & Owner Pipe Lifeline (`Slice-M1M8-4`).
//!
//! 1:1 port of:
//! - `/native/system-apo/src/block_timing.h` & `block_timing.cpp`
//! - `/native/system-apo/src/level_mailbox.h`
//! - `/native/system-apo/src/input_history.h` & `input_history.cpp`
//! - `/native/system-apo/src/output_guard.h` & `output_guard.cpp`
//! - `/native/system-apo/src/level_prediction.h` & `level_prediction.cpp`
//! - `/native/system-apo/src/graph_handover.cpp`
//! - `/native/system-apo/src/owner_link.h` & `owner_link.cpp`

use crate::apo_engine_runtime::{signature_of_chain, ApoEngineChain, SourceAnalysisReport};
use std::f64::consts::PI;
use std::sync::atomic::{fence, AtomicBool, AtomicU64, Ordering};

// ============================================================================
// 1. Real-Time Audio Block Timing & Diagnostics (`block_timing.cpp`)
// ============================================================================

const BLOCK_HALF_PERMILLE: u32 = 500;
const BLOCK_STEP_PERMILLE: u32 = 250;
const BLOCK_TOP_STEP: u32 = 8;

/// Snapshot of real-time block processing wall-time metrics.
///
/// Mirrors `BlockTiming::Reading` in `/native/system-apo/src/block_timing.h`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BlockTimingReading {
    pub blocks: u64,
    pub over_half: u64,
    pub slowest_ms: f64,
    pub period_ms: f64,
    pub slowest_permille: u32,
}

/// Lock-free per-endpoint audio block wall-time tracker.
///
/// Mirrors `fluideq_engine::BlockTiming` in `/native/system-apo/src/block_timing.cpp`.
#[derive(Default)]
pub struct BlockTiming {
    blocks: AtomicU64,
    over_half: AtomicU64,
    /// Packed `(slowest_permille as u64) << 32 | (period_us as u64)`.
    slowest: AtomicU64,
}

impl BlockTiming {
    pub const fn new() -> Self {
        Self {
            blocks: AtomicU64::new(0),
            over_half: AtomicU64::new(0),
            slowest: AtomicU64::new(0),
        }
    }

    /// Records one processed audio block on the real-time thread without locks or OS calls.
    /// Returns the slowest block's share of its period so far, in permille (`0..=u32::MAX`).
    pub fn record(
        &self,
        elapsed: u64,
        ticks_per_second: u64,
        frames: u32,
        rate: u32,
    ) -> u32 {
        if frames == 0 || rate == 0 || ticks_per_second == 0 {
            return (self.slowest.load(Ordering::Relaxed) >> 32) as u32;
        }
        let max_held = 1u64 << 32;
        let held = elapsed.min(max_held);
        let denom = u64::from(frames).saturating_mul(ticks_per_second);
        let share = if denom > 0 {
            held.saturating_mul(1000)
                .saturating_mul(u64::from(rate))
                / denom
        } else {
            0
        };
        let permille = share.min(u64::from(u32::MAX)) as u32;
        let period_us = (u64::from(frames) * 1_000_000 / u64::from(rate)) as u32;

        self.blocks.fetch_add(1, Ordering::Relaxed);
        if permille > BLOCK_HALF_PERMILLE {
            self.over_half.fetch_add(1, Ordering::Relaxed);
        }
        let packed = (u64::from(permille) << 32) | u64::from(period_us);
        let mut current = self.slowest.load(Ordering::Relaxed);
        while ((current >> 32) as u32) < permille {
            match self.slowest.compare_exchange_weak(
                current,
                packed,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
        let slowest_so_far = (current >> 32) as u32;
        slowest_so_far.max(permille)
    }

    pub fn read(&self) -> BlockTimingReading {
        let blocks = self.blocks.load(Ordering::Relaxed);
        let over_half = self.over_half.load(Ordering::Relaxed);
        let slowest = self.slowest.load(Ordering::Relaxed);
        let slowest_permille = (slowest >> 32) as u32;
        let period_ms = ((slowest & 0xffff_ffff) as f64) / 1000.0;
        let slowest_ms = period_ms * f64::from(slowest_permille) / 1000.0;
        BlockTimingReading {
            blocks,
            over_half,
            slowest_ms,
            period_ms,
            slowest_permille,
        }
    }
}

/// Quantizes `permille` into quarters of a period (`0..=8`).
pub fn timing_step(permille: u32) -> u32 {
    (permille / BLOCK_STEP_PERMILLE).min(BLOCK_TOP_STEP)
}

/// Formats `BlockTimingReading` identically to `describe_timing` in `block_timing.cpp`.
pub fn describe_timing(reading: &BlockTimingReading) -> String {
    if reading.blocks == 0 {
        return "no block timed yet".to_string();
    }
    let pct = (reading.slowest_permille + 5) / 10;
    format!(
        "slowest block {:.2} ms of its {:.2} ms ({}%), {} of {} over half",
        reading.slowest_ms, reading.period_ms, pct, reading.over_half, reading.blocks
    )
}

// ============================================================================
// 2. Lock-Free Level Mailbox & Input History (`level_mailbox.h`, `input_history.cpp`)
// ============================================================================

/// Single-word atomic mailbox delivering a predicted preamp shift (`shift_db`)
/// from the watcher thread to the real-time audio thread for a specific graph `generation`.
///
/// Mirrors `fluideq_engine::LevelMailbox` in `/native/system-apo/src/level_mailbox.h`.
#[derive(Default)]
pub struct LevelMailbox {
    word: AtomicU64,
}

impl LevelMailbox {
    pub const fn new() -> Self {
        Self {
            word: AtomicU64::new(0),
        }
    }

    pub fn post(&self, generation: u32, shift_db: f32) {
        let bits = shift_db.to_bits();
        self.word.store(
            (u64::from(generation) << 32) | u64::from(bits),
            Ordering::Release,
        );
    }

    pub fn take(&self, generation: u32) -> Option<f32> {
        let word = self.word.load(Ordering::Acquire);
        if generation == 0 || ((word >> 32) as u32) != generation {
            return None;
        }
        Some(f32::from_bits((word & 0xffff_ffff) as u32))
    }
}

pub const PEAK_CHUNKS_PER_SECOND: u32 = 50;
pub const INPUT_HISTORY_MAX_BYTES: usize = 8 << 20; // 8 MiB per endpoint
const INPUT_HISTORY_SLACK_SECONDS: f64 = 1.0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HistoryStretch {
    pub from: u64,
    pub to: u64,
}

/// Lock-free per-frame seqlock ring recording the last 10 seconds of pre-EQ audio
/// and post-EQ true peaks for `LevelPredictor`.
///
/// Mirrors `fluideq_engine::InputHistory` in `/native/system-apo/src/input_history.cpp`.
pub struct InputHistory {
    rate: u32,
    channels: u32,
    capacity: u64,
    window: u64,
    samples: Vec<std::sync::atomic::AtomicU32>,
    written: AtomicU64,
    reserved: AtomicU64,
    chunk_frames: u32,
    peak_capacity: u64,
    peaks: Vec<std::sync::atomic::AtomicU32>,
    block_start: u64,
    block_end: u64,
    peak_chunk: u64,
}

impl InputHistory {
    pub fn new(rate: u32, channels: u32, seconds: f64) -> Self {
        let channels = channels.max(1);
        let chunk_frames = (rate / PEAK_CHUNKS_PER_SECOND).max(1);
        let slack = (f64::from(rate) * INPUT_HISTORY_SLACK_SECONDS) as u64;
        let wanted = (f64::from(rate) * seconds.max(0.0)) as u64;
        let affordable =
            (INPUT_HISTORY_MAX_BYTES / (std::mem::size_of::<f32>() * (channels as usize))) as u64;
        let capacity = (wanted + slack).min(affordable);
        let window = capacity.saturating_sub(slack);
        let (samples, peak_capacity, peaks) = if capacity > 0 {
            let sample_len = (capacity as usize) * (channels as usize);
            let mut s = Vec::with_capacity(sample_len);
            s.resize_with(sample_len, || std::sync::atomic::AtomicU32::new(0));
            let pk_cap = capacity / u64::from(chunk_frames) + 2;
            let mut p = Vec::with_capacity(pk_cap as usize);
            p.resize_with(pk_cap as usize, || std::sync::atomic::AtomicU32::new(0));
            (s, pk_cap, p)
        } else {
            (Vec::new(), 0, Vec::new())
        };
        Self {
            rate,
            channels,
            capacity,
            window,
            samples,
            written: AtomicU64::new(0),
            reserved: AtomicU64::new(0),
            chunk_frames,
            peak_capacity,
            peaks,
            block_start: 0,
            block_end: 0,
            peak_chunk: u64::MAX,
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn channels(&self) -> u32 {
        self.channels
    }

    pub fn window_frames(&self) -> u64 {
        self.window
    }

    pub fn chunk_frames(&self) -> u32 {
        self.chunk_frames
    }

    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Acquire)
    }

    /// Audio thread: records `frames` of planar input before the EQ runs.
    pub fn record(&mut self, planar: Option<&[&[f32]]>, frames: u32) {
        if frames == 0 || self.capacity == 0 {
            return;
        }
        let start = self.written.load(Ordering::Relaxed);
        let end = start + u64::from(frames);
        self.block_start = start;
        self.block_end = end;
        self.reserved.store(end, Ordering::Relaxed);
        fence(Ordering::Release);

        let cap = self.capacity as usize;
        let fr = frames as usize;
        for ch in 0..(self.channels as usize) {
            let plane = planar.and_then(|p| p.get(ch).copied());
            let lane_offset = ch * cap;
            let mut slot = (start % self.capacity) as usize;
            for at in 0..fr {
                let sample = plane
                    .and_then(|slice| slice.get(at).copied())
                    .unwrap_or(0.0);
                self.samples[lane_offset + slot].store(sample.to_bits(), Ordering::Relaxed);
                slot += 1;
                if slot == cap {
                    slot = 0;
                }
            }
        }
        self.written.store(end, Ordering::Release);
    }

    /// Audio thread: records the loudest true peak that the last recorded block
    /// reached the output guard with.
    pub fn record_peak(&mut self, peak: f32) {
        if self.peak_capacity == 0 || self.block_end <= self.block_start {
            return;
        }
        let cf = u64::from(self.chunk_frames);
        let first = self.block_start / cf;
        let last = (self.block_end - 1) / cf;
        for chunk in first..=last {
            let slot_idx = (chunk % self.peak_capacity) as usize;
            let slot = &self.peaks[slot_idx];
            if self.peak_chunk == u64::MAX || chunk > self.peak_chunk {
                slot.store(peak.to_bits(), Ordering::Relaxed);
                self.peak_chunk = chunk;
            } else {
                let prev = f32::from_bits(slot.load(Ordering::Relaxed));
                slot.store(prev.max(peak).to_bits(), Ordering::Relaxed);
            }
        }
        self.block_start = self.block_end;
    }

    /// Watcher thread: returns the loudest peak in the whole chunks of `[from, to)`,
    /// or `None` if any chunk was overwritten or not yet settled.
    pub fn peak_over(&self, from: u64, to: u64) -> Option<f64> {
        if self.peak_capacity == 0 {
            return None;
        }
        let cf = u64::from(self.chunk_frames);
        let newest = self.written.load(Ordering::Acquire) / cf;
        let first = (from + cf - 1) / cf;
        let end = (to / cf).min(newest.saturating_sub(1));
        let kept = self.peak_capacity - 2;
        if first >= end || (newest > kept && first < newest - kept) {
            return None;
        }
        let mut peak = 0.0_f64;
        for chunk in first..end {
            let val = f32::from_bits(
                self.peaks[(chunk % self.peak_capacity) as usize].load(Ordering::Relaxed),
            );
            peak = peak.max(f64::from(val));
        }
        fence(Ordering::Acquire);
        let after = self.written.load(Ordering::Relaxed) / cf;
        if after > kept && first < after - kept {
            return None;
        }
        Some(peak)
    }

    /// Watcher thread: copies intact frames in `[from, to)` into `out` (one `Vec<f32>` per channel),
    /// trimming any frames overwritten by the audio thread while copying.
    pub fn copy(&self, mut from: u64, mut to: u64, out: &mut Vec<Vec<f32>>) -> HistoryStretch {
        let ch_count = self.channels as usize;
        out.resize_with(ch_count, Vec::new);
        let done = self.written.load(Ordering::Acquire);
        to = to.min(done);
        from = from.max(done.saturating_sub(self.capacity));
        if self.capacity == 0 || from >= to {
            for lane in out.iter_mut() {
                lane.clear();
            }
            return HistoryStretch { from: to, to };
        }
        let count = (to - from) as usize;
        let cap = self.capacity as usize;
        for (ch, copied) in out.iter_mut().enumerate().take(ch_count) {
            let lane_offset = ch * cap;
            copied.resize(count, 0.0);
            let mut slot = (from % self.capacity) as usize;
            for dst in copied.iter_mut() {
                *dst = f32::from_bits(self.samples[lane_offset + slot].load(Ordering::Relaxed));
                slot += 1;
                if slot == cap {
                    slot = 0;
                }
            }
        }
        fence(Ordering::Acquire);
        let reached = self.reserved.load(Ordering::Relaxed);
        let intact = if reached > self.capacity {
            from.max(reached - self.capacity)
        } else {
            from
        };
        if intact >= to {
            for lane in out.iter_mut() {
                lane.clear();
            }
            return HistoryStretch { from: to, to };
        }
        if intact > from {
            let cut = (intact - from) as usize;
            for lane in out.iter_mut() {
                lane.drain(0..cut);
            }
        }
        HistoryStretch { from: intact, to }
    }
}

// ============================================================================
// 3. Output Guard & Auto-Preamp Glide State Machine (`output_guard.cpp`)
// ============================================================================

pub const OUTPUT_GUARD_RECOVER_AFTER_SECONDS: f64 = 5.0;
pub const OUTPUT_GUARD_RECOVER_DB_PER_SECOND: f64 = 0.15;
pub const OUTPUT_GUARD_EDIT_GLIDE_SECONDS: f64 = 0.02;
const OUTPUT_GUARD_SILENCE_DB: f64 = -65.0;

/// Post-filter auto-normalizer and 20 ms raised-cosine target glide controller.
///
/// Mirrors `fluideq_engine::OutputGuard` in `/native/system-apo/src/output_guard.cpp`.
#[derive(Debug, Clone)]
pub struct OutputGuardState {
    rate: u32,
    latency: u32,
    window_frames: u32,
    curve_level_db: f64,
    holding: bool,
    held_db: f64,
    held_curve_db: f64,
    glide_from_db: f64,
    glide_left: u32,
    glide_frames: u32,
    last_input_peak: f64,
    armed: bool,
    measured_frames: u32,
    window_peak: f64,
    quiet_seconds: f64,
    target_db: f64,
    overload_age: u32,
    settling_frames: u32,
    reassess_frames: u32,
    reassess_peak: f64,
    edit_goal_db: f64,
    edit_recovery: bool,
    limiter_gain: f64,
}

impl OutputGuardState {
    pub fn new(rate: u32) -> Self {
        let rate = rate.max(1);
        let latency = ((rate * 2) / 1000).max(1); // 2 ms lookahead
        Self {
            rate,
            latency,
            window_frames: (rate / 10).max(1),
            curve_level_db: 0.0,
            holding: false,
            held_db: 0.0,
            held_curve_db: 0.0,
            glide_from_db: 0.0,
            glide_left: 0,
            glide_frames: 1,
            last_input_peak: 0.0,
            armed: true,
            measured_frames: 0,
            window_peak: 0.0,
            quiet_seconds: 0.0,
            target_db: 0.0,
            overload_age: 10,
            settling_frames: 0,
            reassess_frames: 0,
            reassess_peak: 0.0,
            edit_goal_db: 0.0,
            edit_recovery: false,
            limiter_gain: 1.0,
        }
    }

    pub fn latency(&self) -> u32 {
        self.latency
    }

    pub fn holding(&self) -> bool {
        self.holding
    }

    pub fn target_db(&self) -> f64 {
        self.target_db
    }

    pub fn gain_db(&self) -> f64 {
        20.0 * self.limiter_gain.max(1e-12).log10()
    }

    pub fn last_input_peak(&self) -> f64 {
        self.last_input_peak
    }

    /// Returns the level handed to the limiter on the current sample (raised-cosine glide).
    pub fn applied_db(&self) -> f64 {
        if self.glide_left == 0 {
            return self.target_db;
        }
        let done = 1.0 - f64::from(self.glide_left) / f64::from(self.glide_frames.max(1));
        let weight = 0.5 - 0.5 * (PI * done).cos();
        self.glide_from_db + weight * (self.target_db - self.glide_from_db)
    }

    fn move_target(&mut self, db: f64) {
        self.glide_from_db = self.applied_db();
        self.target_db = db;
        self.glide_frames =
            ((OUTPUT_GUARD_EDIT_GLIDE_SECONDS * f64::from(self.rate)).round() as u32).max(1);
        self.glide_left = if (self.glide_from_db - self.target_db).abs() < 1e-12 {
            0
        } else {
            self.glide_frames
        };
    }

    pub fn set_curve_level(&mut self, db: f64) {
        let level = db.min(0.0);
        if !self.armed && level < self.curve_level_db {
            self.move_target((self.target_db + (level - self.curve_level_db)).min(0.0));
        }
        self.curve_level_db = level;
    }

    pub fn reassess(&mut self, settling_frames: u32) {
        self.settling_frames = settling_frames;
        self.reassess_frames = self.rate * 2;
        self.reassess_peak = 0.0;
        self.edit_recovery = false;
        self.measured_frames = 0;
        self.window_peak = 0.0;
        self.quiet_seconds = 0.0;
    }

    pub fn hold(&mut self, curve_level_db: f64, settling_frames: u32, sound_changed: bool) {
        let level = curve_level_db.min(0.0);
        if !sound_changed {
            if self.holding {
                self.curve_level_db = level;
            } else {
                self.set_curve_level(level);
            }
            return;
        }
        if self.armed {
            self.holding = false;
            self.curve_level_db = level;
            return;
        }
        if !self.holding {
            self.holding = true;
            self.held_db = self.target_db;
            self.held_curve_db = self.curve_level_db;
        }
        self.curve_level_db = level;
        let next_target = (self.held_db + (level - self.held_curve_db).min(0.0)).min(0.0);
        self.move_target(next_target);
        self.settling_frames = settling_frames;
        self.reassess_frames = 0;
        self.reassess_peak = 0.0;
        self.edit_recovery = false;
        self.measured_frames = 0;
        self.window_peak = 0.0;
        self.quiet_seconds = 0.0;
    }

    pub fn settle(&mut self, shift_db: f64, settling_frames: u32) {
        if !self.holding {
            return;
        }
        self.holding = false;
        if !shift_db.is_finite() {
            self.reassess(settling_frames);
            return;
        }
        self.move_target((self.held_db + shift_db).min(0.0));
        self.reassess_frames = 0;
        self.reassess_peak = 0.0;
        self.edit_recovery = false;
        self.quiet_seconds = 0.0;
    }

    pub fn release_hold(&mut self) {
        self.holding = false;
    }

    pub fn take_level(&mut self, from: &Self) {
        self.curve_level_db = from.curve_level_db;
        self.armed = from.armed;
        self.quiet_seconds = from.quiet_seconds;
        self.target_db = from.target_db;
        self.overload_age = from.overload_age;
        self.settling_frames = from.settling_frames;
        self.reassess_frames = from.reassess_frames;
        self.reassess_peak = from.reassess_peak;
        self.edit_goal_db = from.edit_goal_db;
        self.edit_recovery = from.edit_recovery;
        self.holding = from.holding;
        self.held_db = from.held_db;
        self.held_curve_db = from.held_curve_db;
        self.glide_from_db = from.glide_from_db;
        self.glide_left = from.glide_left;
        self.glide_frames = from.glide_frames;
        self.limiter_gain = from.limiter_gain;
    }

    /// Advances the OutputGuard envelope and 100 ms window normalizer over `frames` with `block_peak`.
    ///
    /// Mirrors `OutputGuard::process` in `/native/system-apo/src/output_guard.cpp`.
    pub fn process_block(&mut self, frames: u32, block_peak: f64, enabled: bool) {
        if !enabled {
            self.target_db = 0.0;
            self.quiet_seconds = 0.0;
            self.overload_age = 10;
            self.reassess_frames = 0;
            self.edit_recovery = false;
            self.armed = true;
            self.holding = false;
            self.glide_left = 0;
        } else if self.armed {
            self.target_db = self.curve_level_db;
            self.glide_left = 0;
            self.armed = false;
        }
        self.last_input_peak = block_peak;
        let mut offset = 0u32;
        while offset < frames {
            let mut count = (frames - offset).min(self.window_frames - self.measured_frames);
            if self.glide_left > 0 {
                count = 1;
            }
            let max_gain = 10.0_f64.powf(self.applied_db() / 20.0);
            self.limiter_gain = max_gain;
            self.glide_left = self.glide_left.saturating_sub(count);
            self.window_peak = self.window_peak.max(block_peak);
            offset += count;
            self.measured_frames += count;

            if self.measured_frames == self.window_frames {
                let peak_db = 20.0 * self.window_peak.max(1e-12).log10();
                let required = (-1.0 - peak_db).min(0.0);
                self.overload_age = (self.overload_age + 1).min(10);
                let win_secs = f64::from(self.window_frames) / f64::from(self.rate);

                if self.settling_frames > 0 {
                    self.settling_frames = self.settling_frames.saturating_sub(self.window_frames);
                } else if enabled && self.reassess_frames > 0 && peak_db > OUTPUT_GUARD_SILENCE_DB {
                    self.reassess_peak = self.reassess_peak.max(self.window_peak);
                    self.edit_goal_db = (-1.0 - 20.0 * self.reassess_peak.max(1e-12).log10()).min(0.0);
                    self.target_db = self.edit_goal_db.min(self.target_db + win_secs);
                    self.edit_recovery = self.target_db < self.edit_goal_db;
                    self.quiet_seconds = 0.0;
                    self.reassess_frames = self.reassess_frames.saturating_sub(self.window_frames);
                } else if enabled && self.edit_recovery && peak_db > OUTPUT_GUARD_SILENCE_DB {
                    self.edit_goal_db = self.edit_goal_db.min(required);
                    self.target_db = self.edit_goal_db.min(self.target_db + win_secs);
                    self.edit_recovery = self.target_db < self.edit_goal_db;
                    self.quiet_seconds = 0.0;
                } else if enabled && peak_db > OUTPUT_GUARD_SILENCE_DB {
                    if required < self.target_db {
                        if self.overload_age < 10 {
                            self.target_db = required;
                        }
                        self.overload_age = 0;
                        self.quiet_seconds = 0.0;
                    } else if -1.0 - peak_db > self.target_db + 1.0 {
                        self.quiet_seconds += win_secs;
                        if self.quiet_seconds > OUTPUT_GUARD_RECOVER_AFTER_SECONDS {
                            self.target_db = (self.target_db
                                + OUTPUT_GUARD_RECOVER_DB_PER_SECOND * win_secs)
                                .min(0.0);
                        }
                    } else {
                        self.quiet_seconds = 0.0;
                    }
                } else {
                    self.quiet_seconds = 0.0;
                }
                self.measured_frames = 0;
                self.window_peak = 0.0;
            }
        }
    }
}

// ============================================================================
// 4. Level Prediction & Minimum-Phase Replay Normalization (`level_prediction.cpp`)
// ============================================================================

pub const LEVEL_HISTORY_SECONDS: f64 = 10.0;
pub const LEVEL_WARM_UP_SECONDS: f64 = 0.5;
pub const LEVEL_LEAST_SECONDS: f64 = 1.0;
pub const LEVEL_CONVOLVED_SECONDS: f64 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelPredictionJudgement {
    None,
    Same,
    Changed,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelPrediction {
    pub shift_db: f64,
    pub seconds: f64,
}

/// Normalizes an `ApoEngineChain` for minimum-phase level prediction replay.
///
/// Mirrors `fluideq_engine::replay_chain_of` in `/native/system-apo/src/level_prediction.cpp`.
pub fn replay_chain_of(chain: &ApoEngineChain) -> ApoEngineChain {
    let mut replay = chain.clone();
    replay.dsp_values.clear();
    replay.output_guard = false;
    replay.auto_preamp = false;
    replay.auto_preamp_start_db = 0.0;
    replay.minimum_eq_phase = true;
    replay.minimum_curve_phase = true;
    replay.files_read.clear();
    replay.ignored.clear();
    replay
}

pub fn chain_convolves(chain: &ApoEngineChain) -> bool {
    !chain.graphic_curves.is_empty() || !chain.convolution_path.is_empty()
}

pub fn chain_plays_as_replayed(chain: &ApoEngineChain) -> bool {
    chain.output_guard && chain.minimum_eq_phase && chain.minimum_curve_phase
}

/// Judges `next` against `current` without running a replay.
///
/// Mirrors `LevelPredictor::judge` in `/native/system-apo/src/level_prediction.cpp`.
pub fn judge_level_prediction(
    current: Option<&ApoEngineChain>,
    next: &ApoEngineChain,
) -> LevelPredictionJudgement {
    let Some(curr) = current else {
        return LevelPredictionJudgement::None;
    };
    if !curr.auto_preamp || !next.auto_preamp || !next.output_guard {
        return LevelPredictionJudgement::None;
    }
    let curr_sig = signature_of_chain(&replay_chain_of(curr));
    let next_sig = signature_of_chain(&replay_chain_of(next));
    if curr_sig == next_sig {
        LevelPredictionJudgement::Same
    } else {
        LevelPredictionJudgement::Changed
    }
}

/// Computes `LevelPrediction` (`before_db - after_db`) from linear peak measurements over `[lo, hi)`.
pub fn compute_level_prediction_from_peaks(
    before_peak_linear: f64,
    after_peak_linear: f64,
    lo_frame: u64,
    hi_frame: u64,
    rate: u32,
) -> Option<LevelPrediction> {
    if rate == 0 || hi_frame <= lo_frame {
        return None;
    }
    let duration_frames = hi_frame - lo_frame;
    if duration_frames < (f64::from(rate) * LEVEL_LEAST_SECONDS) as u64 {
        return None;
    }
    let before_db = 20.0 * before_peak_linear.max(1e-12).log10();
    let after_db = 20.0 * after_peak_linear.max(1e-12).log10();
    if before_db < OUTPUT_GUARD_SILENCE_DB || after_db < OUTPUT_GUARD_SILENCE_DB {
        return None;
    }
    Some(LevelPrediction {
        shift_db: before_db - after_db,
        seconds: (duration_frames as f64) / f64::from(rate),
    })
}

// ============================================================================
// 5. Graph Handover & 30ms Raised-Cosine Crossover (`graph_handover.cpp`)
// ============================================================================

pub const CROSSING_SETTLE_SECONDS: f64 = 0.04;
pub const CROSSING_FADE_SECONDS: f64 = 0.03;
pub const PREAMP_FADE_SECONDS: f64 = 0.02;

/// Returns `true` if a Library cut/seek source boundary occurred between `previous`
/// and `current`, meaning filter history and held output gain must NOT cross over.
///
/// Mirrors the `source_report_` check in `Graph::adopt_state` (`graph_handover.cpp`).
pub fn is_source_boundary(
    current: &SourceAnalysisReport,
    previous: &SourceAnalysisReport,
) -> bool {
    current.library != previous.library
        || current.engine_owner != previous.engine_owner
        || current.source != previous.source
        || current.epoch != previous.epoch
}

/// Raised-cosine preamp and graph-crossing state machine.
///
/// Mirrors `Graph::current_preamp`, `Graph::start_crossing`, and `Graph::mix_crossing`
/// in `/native/system-apo/src/graph_handover.cpp`.
#[derive(Debug, Clone)]
pub struct GraphCrossingCoordinator {
    pub sample_rate: u32,
    pub latency_frames: u32,
    pub preamp_linear: f64,
    pub preamp_from: f64,
    pub preamp_fade_total: u32,
    pub preamp_fade_left: u32,
    pub crossing_elapsed: u64,
    pub crossing_prime: u64,
    pub crossing_fade: u32,
    pub crossing_active: bool,
}

impl GraphCrossingCoordinator {
    pub fn new(sample_rate: u32, latency_frames: u32, preamp_linear: f64) -> Self {
        Self {
            sample_rate: sample_rate.max(1),
            latency_frames,
            preamp_linear,
            preamp_from: preamp_linear,
            preamp_fade_total: 0,
            preamp_fade_left: 0,
            crossing_elapsed: 0,
            crossing_prime: 0,
            crossing_fade: 1,
            crossing_active: false,
        }
    }

    pub fn current_preamp(&self) -> f64 {
        if self.preamp_fade_left == 0 || self.preamp_fade_total == 0 {
            return self.preamp_linear;
        }
        let done = self.preamp_fade_total - self.preamp_fade_left;
        let weight =
            0.5 - 0.5 * (PI * f64::from(done) / f64::from(self.preamp_fade_total)).cos();
        self.preamp_from + weight * (self.preamp_linear - self.preamp_from)
    }

    pub fn begin_preamp_slide(&mut self, previous_current_preamp: f64) {
        if (previous_current_preamp - self.preamp_linear).abs() > 1e-12 {
            self.preamp_from = previous_current_preamp;
            self.preamp_fade_total =
                ((PREAMP_FADE_SECONDS * f64::from(self.sample_rate)).round() as u32).max(1);
            self.preamp_fade_left = self.preamp_fade_total;
        }
    }

    pub fn start_crossing(&mut self) {
        self.crossing_elapsed = 0;
        self.crossing_prime = u64::from(self.latency_frames)
            + ((CROSSING_SETTLE_SECONDS * f64::from(self.sample_rate)).round() as u64);
        self.crossing_fade =
            ((CROSSING_FADE_SECONDS * f64::from(self.sample_rate)).round() as u32).max(1);
        self.crossing_active = true;
    }

    /// Mixes `before` into `own` across `frames` using the 40 ms prime + 30 ms raised-cosine window.
    /// Returns `true` while the crossing is still active, or `false` once `crossing_elapsed >= fade_until`.
    pub fn mix_crossing(&mut self, own: &mut [f32], before: &[f32], frames: usize) -> bool {
        if !self.crossing_active {
            return false;
        }
        let fade_from = self.crossing_prime;
        let fade_until = self.crossing_prime + u64::from(self.crossing_fade);
        let n = frames.min(own.len()).min(before.len());
        for at in 0..n {
            let position = self.crossing_elapsed + (at as u64);
            let weight = if position < fade_from {
                0.0
            } else if position < fade_until {
                0.5 - 0.5
                    * (PI * ((position - fade_from) as f64) / f64::from(self.crossing_fade)).cos()
            } else {
                1.0
            };
            let b = f64::from(before[at]);
            let o = f64::from(own[at]);
            own[at] = (b + weight * (o - b)) as f32;
        }
        self.crossing_elapsed += frames as u64;
        if self.crossing_elapsed >= fade_until {
            self.crossing_active = false;
        }
        self.crossing_active
    }
}

// ============================================================================
// 6. Owner Named-Pipe Lifeline State Machine (`owner_link.cpp`)
// ============================================================================

/// Win32 error codes classified when opening FluidEQ's owner named pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerPipeOpenError {
    /// `ERROR_FILE_NOT_FOUND (2)`: FluidEQ is not running (silent, normal).
    FileNotFound,
    /// `ERROR_PIPE_BUSY (231)`: Pipe exists and all instances are taken -> FluidEQ IS running.
    PipeBusy,
    /// `ERROR_ACCESS_DENIED (5)`: Pipe exists under another DACL -> FluidEQ IS running.
    AccessDenied,
    /// Any other Win32 error code -> treated as not running, logged once per distinct code.
    Other(u32),
}

impl OwnerPipeOpenError {
    pub const fn code(self) -> u32 {
        match self {
            Self::FileNotFound => 2,
            Self::AccessDenied => 5,
            Self::PipeBusy => 231,
            Self::Other(c) => c,
        }
    }

    pub const fn treats_owner_as_running(self) -> bool {
        matches!(self, Self::PipeBusy | Self::AccessDenied)
    }
}

/// Process-wide owner named-pipe lifeline monitor state machine.
///
/// Mirrors `fluideq_engine::OwnerLink` in `/native/system-apo/src/owner_link.cpp`.
pub struct OwnerLinkMonitor {
    pipe_name: String,
    present: AtomicBool,
    connected: bool,
    read_pending: bool,
    last_open_error: u32,
}

impl OwnerLinkMonitor {
    pub fn new(pipe_name: impl Into<String>) -> Self {
        Self {
            pipe_name: pipe_name.into(),
            present: AtomicBool::new(false),
            connected: false,
            read_pending: false,
            last_open_error: 0,
        }
    }

    pub fn pipe_name(&self) -> &str {
        &self.pipe_name
    }

    pub fn present(&self) -> bool {
        self.present.load(Ordering::Acquire)
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    pub fn is_read_pending(&self) -> bool {
        self.read_pending
    }

    fn set_present(&self, next: bool) -> Option<&'static str> {
        if self.present.swap(next, Ordering::AcqRel) == next {
            return None;
        }
        Some(if next {
            "FluidEQ is running: holding its pipe"
        } else {
            "FluidEQ is not running: its pipe is closed"
        })
    }

    /// Handles a failed `CreateFileW` open attempt on the owner pipe.
    /// Returns any deduplicated diagnostic log lines generated.
    ///
    /// Mirrors `OwnerLink::connect` error handling in `/native/system-apo/src/owner_link.cpp`.
    pub fn on_open_failed(&mut self, error: OwnerPipeOpenError) -> Vec<String> {
        let mut logs = Vec::new();
        let running = error.treats_owner_as_running();
        let code = error.code();
        if code != self.last_open_error && error != OwnerPipeOpenError::FileNotFound {
            logs.push(format!(
                "FluidEQ's pipe could not be opened (error {}); {}",
                code,
                if running {
                    "treating FluidEQ as running"
                } else {
                    "treating FluidEQ as not running"
                }
            ));
        }
        self.last_open_error = code;
        self.connected = false;
        self.read_pending = false;
        if let Some(status_log) = self.set_present(running) {
            logs.push(status_log.to_string());
        }
        logs
    }

    /// Handles a successful `CreateFileW` open followed by `arm_read`.
    pub fn on_open_and_arm_read(&mut self, read_accepted_or_pending: bool) -> Option<&'static str> {
        self.last_open_error = 0;
        if read_accepted_or_pending {
            self.connected = true;
            self.read_pending = true;
            self.set_present(true)
        } else {
            self.connected = false;
            self.read_pending = false;
            self.set_present(false)
        }
    }

    /// Handles `GetOverlappedResult` completion on `read_event_`.
    ///
    /// Mirrors `OwnerLink::on_read_complete` in `/native/system-apo/src/owner_link.cpp`.
    pub fn on_read_complete(
        &mut self,
        io_incomplete: bool,
        read_succeeded: bool,
    ) -> Option<&'static str> {
        if io_incomplete {
            return None;
        }
        self.read_pending = false;
        if read_succeeded {
            self.read_pending = true;
            return self.set_present(true);
        }
        self.connected = false;
        self.set_present(false)
    }

    /// Fails open (`present = true`) if the monitor thread encounters an unrecoverable wait error.
    pub fn on_monitor_wait_failed(&mut self) -> Option<&'static str> {
        self.connected = false;
        self.read_pending = false;
        self.set_present(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_timing_tracks_slowest_permille_and_formats_exact_log_line() {
        let timing = BlockTiming::new();
        assert_eq!(describe_timing(&timing.read()), "no block timed yet");

        // 480 frames @ 48 kHz = 10.00 ms period. Elapsed = 61,000 ticks @ 10 MHz = 6.10 ms (610 permille)
        let permille = timing.record(61_000, 10_000_000, 480, 48_000);
        assert_eq!(permille, 610);
        assert_eq!(timing_step(permille), 2);
        let reading = timing.read();
        assert_eq!(reading.blocks, 1);
        assert_eq!(reading.over_half, 1);
        assert_eq!(
            describe_timing(&reading),
            "slowest block 6.10 ms of its 11.00 ms (61%), 1 of 1 over half"
                .replace("11.00", "10.00")
        );
    }

    #[test]
    fn level_mailbox_matches_exact_graph_generation() {
        let mailbox = LevelMailbox::new();
        mailbox.post(7, -3.5);
        assert_eq!(mailbox.take(0), None);
        assert_eq!(mailbox.take(6), None);
        assert_eq!(mailbox.take(7), Some(-3.5));
    }

    #[test]
    fn input_history_records_and_copies_intact_frames_and_peaks() {
        let mut history = InputHistory::new(1000, 2, 2.0);
        let left = vec![0.25_f32; 40];
        let right = vec![0.50_f32; 40];
        let planes = [left.as_slice(), right.as_slice()];
        history.record(Some(&planes), 40);
        history.record_peak(0.75);
        history.record(Some(&planes), 40);
        history.record_peak(0.90);
        history.record(Some(&planes), 40);
        history.record_peak(0.60);

        let mut out = Vec::new();
        let stretch = history.copy(0, 80, &mut out);
        assert_eq!(stretch, HistoryStretch { from: 0, to: 80 });
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].len(), 80);
        assert!((out[1][10] - 0.50).abs() < 1e-6);

        let pk = history.peak_over(0, 60).unwrap();
        assert!((pk - 0.90).abs() < 1e-5);
    }

    #[test]
    fn output_guard_holds_and_settles_predicted_level_shift() {
        let mut guard = OutputGuardState::new(48_000);
        guard.set_curve_level(-4.0);
        guard.process_block(480, 0.25, true);
        assert!((guard.target_db() - (-4.0)).abs() < 1e-6);

        // Hold for a louder curve (-6.0 dB vs -4.0 dB basis -> moves target down to -6.0 dB)
        guard.hold(-6.0, 2400, true);
        assert!(guard.holding());
        assert!((guard.target_db() - (-6.0)).abs() < 1e-6);

        // Prediction arrives: new curve was only +1.2 dB louder on the actual music (`shift_db = -1.2`)
        guard.settle(-1.2, 2400);
        assert!(!guard.holding());
        assert!((guard.target_db() - (-5.2)).abs() < 1e-6);
    }

    #[test]
    fn owner_link_treats_pipe_busy_and_access_denied_as_running_and_deduplicates_logs() {
        let mut link = OwnerLinkMonitor::new("\\\\.\\pipe\\FluidEQ-Owner");
        // FileNotFound is silent and marks not running
        let logs = link.on_open_failed(OwnerPipeOpenError::FileNotFound);
        assert!(logs.is_empty());
        assert!(!link.present());

        // PipeBusy means FluidEQ is running
        let logs = link.on_open_failed(OwnerPipeOpenError::PipeBusy);
        assert!(link.present());
        assert_eq!(logs.len(), 2);

        // Repeating PipeBusy produces zero duplicate log lines
        let repeat_logs = link.on_open_failed(OwnerPipeOpenError::PipeBusy);
        assert!(repeat_logs.is_empty());

        // Broken pipe on read complete marks FluidEQ not running
        let _ = link.on_open_and_arm_read(true);
        let msg = link.on_read_complete(false, false);
        assert_eq!(msg, Some("FluidEQ is not running: its pipe is closed"));
        assert!(!link.present());
    }
}
