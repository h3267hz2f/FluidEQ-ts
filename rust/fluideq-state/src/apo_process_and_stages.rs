/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! FluidEQ System-APO Format Negotiation, COM Identity & Aggregation, Endpoint Property
//! Extractor, `LockForProcess` / `APOProcess` Real-Time Frame Loop, Frequency-Sampled &
//! Cepstral Minimum-Phase Graphic-EQ FIR Designer, Warm-Handover Curve Stage, and 3-Pass
//! State-Carrying Biquad Cascade (`Slice-M1M8-11` — Final Slice `11/11` of M1–M8).
//!
//! 1:1 faithful port of:
//! - `/native/system-apo/src/graphic_eq.h` & `graphic_eq.cpp` (`CurveWalk`, `design_graphic_kernel`, `design_minimum_graphic_kernel`)
//! - `/native/system-apo/src/curve_stage.h` & `curve_stage.cpp` (`CurveStage`, `adopt`, `process`, 20 ms fade + warmup behind delay ring)
//! - `/native/system-apo/src/iir_cascade.h` & `iir_cascade.cpp` (`IirBand`, `to_core_type`, `design_band`, `IirCascade`, 3-pass band matching + 20 ms equal-gain raised cosine)
//! - `/native/system-apo/src/apo_format.cpp` (`ConnectionFormat`, `describe_format`, `suggest_float`, `IsInputFormatSupported`, `IsOutputFormatSupported`)
//! - `/native/system-apo/src/apo_endpoint.cpp` (`read_endpoint_properties`, `read_endpoint`)
//! - `/native/system-apo/src/apo_identity.cpp` (`Apo` COM aggregation, `InnerUnknown`, `query_inner`, `g_object_count`)
//! - `/native/system-apo/src/apo.cpp` (`Reset`, `GetLatency`, `GetRegistrationProperties`, `Initialize` v1/v2/v3 exact layout check, `GetEffectsList`, `GetControllableSystemEffectsList`, `SetAudioSystemEffectState`)
//! - `/native/system-apo/src/apo_process.cpp` (`kSilenceFloor = 3.2e-5f`, `LockForProcess`, `UnlockForProcess`, `release_locked_state`, `APOProcess`)

use crate::apo::ApoGraphicPoint;
use crate::apo_analysis_and_status::{
    output_block_timing, output_carried_flag, trace, EngineLog,
};
use crate::apo_engine_runtime::SignatureBand;
use crate::apo_graph_and_guard::{describe_timing, timing_step, BlockTiming};
use crate::apo_watcher::{GraphSlot, WatcherGraphInstance};
use crate::biquad::{biquad_coefficients_cookbook, biquad_coefficients_designed};
use crate::linear_phase::fft_radix2_in_place;
use fluideq_dsp_sys::{FeqBiquadCoefficients, FeqBiquadState, FeqEqModel, FeqFilterType};
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;

// ============================================================================
// 1. Graphic-EQ Linear-Phase & Minimum-Phase FIR Designer (`graphic_eq.cpp`)
// ============================================================================

/// Forward-only piecewise-linear `log10(f)` interpolator (`CurveWalk` in `graphic_eq.cpp`).
struct CurveWalk<'a> {
    points: &'a [ApoGraphicPoint],
    next: usize,
}

impl<'a> CurveWalk<'a> {
    fn new(points: &'a [ApoGraphicPoint]) -> Self {
        Self { points, next: 1 }
    }

    fn gain_db(&mut self, frequency: f64) -> f64 {
        if self.points.is_empty() {
            return 0.0;
        }
        if frequency <= self.points[0].frequency {
            return self.points[0].gain_db;
        }
        let last = self.points.len() - 1;
        if frequency >= self.points[last].frequency {
            return self.points[last].gain_db;
        }
        while self.next < self.points.len() && self.points[self.next].frequency < frequency {
            self.next += 1;
        }
        let lo = &self.points[self.next - 1];
        let hi = &self.points[self.next];
        let log_lo = lo.frequency.log10();
        let log_hi = hi.frequency.log10();
        let denom = log_hi - log_lo;
        if denom.abs() < 1e-15 {
            return lo.gain_db;
        }
        let t = (frequency.log10() - log_lo) / denom;
        lo.gain_db + t * (hi.gain_db - lo.gain_db)
    }
}

fn sort_graphic_curves(curves: &[Vec<ApoGraphicPoint>]) -> Vec<Vec<ApoGraphicPoint>> {
    let mut sorted = curves.to_vec();
    for curve in &mut sorted {
        curve.sort_by(|a, b| {
            a.frequency
                .partial_cmp(&b.frequency)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    sorted
}

/// Exact port of `design_graphic_kernel` in `/native/system-apo/src/graphic_eq.cpp`:
/// Designs an odd-length (`taps | 1`) linear-phase FIR approximating the sum of all curves
/// in `curves` via zero-phase frequency sampling on the smallest power-of-two FFT `m >= 2 * n`,
/// centered at `n / 2` and windowed with a symmetric Hann window.
pub fn design_graphic_kernel(
    curves: &[Vec<ApoGraphicPoint>],
    sample_rate: u32,
    taps: u32,
) -> Vec<f32> {
    let n = (taps | 1) as usize;
    let mut m = 1usize;
    while m < 2 * n {
        m <<= 1;
    }

    let sorted = sort_graphic_curves(curves);
    let mut walks: Vec<CurveWalk<'_>> = sorted.iter().map(|c| CurveWalk::new(c)).collect();

    let mut real = vec![0.0_f64; m];
    let mut imaginary = vec![0.0_f64; m];
    let half = m / 2;
    let sr = sample_rate as f64;
    let m_f64 = m as f64;

    for k in 0..=half {
        let frequency = (k as f64) * sr / m_f64;
        let mut gain_db = 0.0_f64;
        for walk in &mut walks {
            gain_db += walk.gain_db(frequency);
        }
        let magnitude = 10.0_f64.powf(gain_db / 20.0);
        real[k] = magnitude;
        if k != 0 && k != half {
            real[m - k] = magnitude;
        }
    }

    fft_radix2_in_place(&mut real, &mut imaginary, true);

    let half_n = n / 2;
    let mut kernel = vec![0.0_f32; n];
    for i in 0..n {
        let source = (i + m - half_n) % m;
        let value = real[source] / m_f64;
        let window = if n > 1 {
            0.5 - 0.5 * (2.0 * PI * (i as f64) / ((n - 1) as f64)).cos()
        } else {
            1.0
        };
        kernel[i] = (value * window) as f32;
    }
    kernel
}

/// Exact port of `design_minimum_graphic_kernel` in `/native/system-apo/src/graphic_eq.cpp`:
/// Designs a minimum-phase FIR via the real cepstrum on a `4 * n` power-of-two grid,
/// flooring log magnitude at `-100.0 dB` and tapering the final quarter with a half-cosine window.
pub fn design_minimum_graphic_kernel(
    curves: &[Vec<ApoGraphicPoint>],
    sample_rate: u32,
    taps: u32,
) -> Vec<f32> {
    let n = taps.max(1) as usize;
    let mut m = 1usize;
    while m < 4 * n {
        m <<= 1;
    }

    let sorted = sort_graphic_curves(curves);
    let mut walks: Vec<CurveWalk<'_>> = sorted.iter().map(|c| CurveWalk::new(c)).collect();

    const FLOOR_DB: f64 = -100.0;
    let nepers_per_db = 10.0_f64.ln() / 20.0;
    let mut real = vec![0.0_f64; m];
    let mut imaginary = vec![0.0_f64; m];
    let half = m / 2;
    let sr = sample_rate as f64;
    let m_f64 = m as f64;

    for k in 0..=half {
        let frequency = (k as f64) * sr / m_f64;
        let mut gain_db = 0.0_f64;
        for walk in &mut walks {
            gain_db += walk.gain_db(frequency);
        }
        let log_magnitude = gain_db.max(FLOOR_DB) * nepers_per_db;
        real[k] = log_magnitude;
        if k != 0 && k != half {
            real[m - k] = log_magnitude;
        }
    }

    // Real cepstrum folded onto positive quefrency
    fft_radix2_in_place(&mut real, &mut imaginary, true);
    for q in 0..m {
        let weight = if q == 0 || q == half {
            1.0
        } else if q < half {
            2.0
        } else {
            0.0
        };
        real[q] = real[q] * weight / m_f64;
        imaginary[q] = 0.0;
    }

    fft_radix2_in_place(&mut real, &mut imaginary, false);
    for k in 0..m {
        let magnitude = real[k].exp();
        let phase = imaginary[k];
        real[k] = magnitude * phase.cos();
        imaginary[k] = magnitude * phase.sin();
    }
    fft_radix2_in_place(&mut real, &mut imaginary, true);

    let mut kernel = vec![0.0_f32; n];
    let quarter = n / 4;
    let taper_from = n - quarter;
    for i in 0..n {
        let window = if i >= taper_from && quarter > 0 {
            let t = ((i - taper_from) as f64) / (quarter as f64);
            0.5 + 0.5 * (PI * t).cos()
        } else {
            1.0
        };
        kernel[i] = (real[i] / m_f64 * window) as f32;
    }
    kernel
}

// ============================================================================
// 2. Warm-Handover Graphic Curve Stage (`curve_stage.h` & `curve_stage.cpp`)
// ============================================================================

/// Crossfade duration (`kFadeSeconds = 0.02` in `curve_stage.h`).
pub const CURVE_STAGE_FADE_SECONDS: f64 = 0.02;

/// Direct-form FIR convolver state for one channel in `CurveStage`.
#[derive(Debug, Clone)]
pub struct ChannelConvolverState {
    kernel: Arc<Vec<f32>>,
    history: Vec<f32>,
    write_pos: usize,
    previous_kernel: Option<Arc<Vec<f32>>>,
    previous_history: Vec<f32>,
    previous_write_pos: usize,
    transfer_fade_total: u32,
    transfer_fade_left: u32,
}

impl ChannelConvolverState {
    pub fn new(kernel: Arc<Vec<f32>>) -> Self {
        let len = kernel.len().max(1);
        Self {
            kernel,
            history: vec![0.0; len],
            write_pos: 0,
            previous_kernel: None,
            previous_history: Vec::new(),
            previous_write_pos: 0,
            transfer_fade_total: 0,
            transfer_fade_left: 0,
        }
    }

    /// Transfers history from `previous` when both kernels have the same tap count
    /// (`feq_convolver_transfer` in `curve_stage.cpp`).
    pub fn transfer_from(&mut self, previous: &Self, fade_frames: u32) -> bool {
        if self.kernel.len() != previous.kernel.len() || self.kernel.is_empty() {
            return false;
        }
        self.history.copy_from_slice(&previous.history);
        self.write_pos = previous.write_pos;
        self.previous_kernel = Some(previous.kernel.clone());
        self.previous_history = previous.history.clone();
        self.previous_write_pos = previous.write_pos;
        self.transfer_fade_total = fade_frames.max(1);
        self.transfer_fade_left = fade_frames;
        true
    }

    pub fn process(&mut self, buffer: &mut [f32]) {
        let k_len = self.kernel.len();
        if k_len == 0 {
            return;
        }
        for sample in buffer.iter_mut() {
            let x = *sample;
            self.history[self.write_pos] = x;
            let mut acc = 0.0_f64;
            let mut h_idx = self.write_pos;
            for &tap in self.kernel.iter() {
                acc += (self.history[h_idx] as f64) * (tap as f64);
                if h_idx == 0 {
                    h_idx = k_len - 1;
                } else {
                    h_idx -= 1;
                }
            }
            let mut out = acc;

            if self.transfer_fade_left > 0 {
                if let Some(prev_k) = &self.previous_kernel {
                    self.previous_history[self.previous_write_pos] = x;
                    let mut prev_acc = 0.0_f64;
                    let mut p_idx = self.previous_write_pos;
                    for &tap in prev_k.iter() {
                        prev_acc += (self.previous_history[p_idx] as f64) * (tap as f64);
                        if p_idx == 0 {
                            p_idx = k_len - 1;
                        } else {
                            p_idx -= 1;
                        }
                    }
                    self.previous_write_pos = (self.previous_write_pos + 1) % k_len;
                    let done = self.transfer_fade_total - self.transfer_fade_left;
                    let weight = 0.5
                        - 0.5
                            * (PI * (done as f64) / (self.transfer_fade_total as f64)).cos();
                    out = prev_acc + weight * (acc - prev_acc);
                }
                self.transfer_fade_left -= 1;
                if self.transfer_fade_left == 0 {
                    self.previous_kernel = None;
                }
            }

            self.write_pos = (self.write_pos + 1) % k_len;
            *sample = out as f32;
        }
    }
}

/// Complete 1:1 port of `fluideq_engine::CurveStage` in `/native/system-apo/src/curve_stage.cpp`:
/// Runs the EQ page's graphic curves as one convolution stage, or as a bit-identical delay ring
/// of the same latency when no curve is loaded, warming new convolvers behind the delay ring
/// before fading across over 20 ms (`CURVE_STAGE_FADE_SECONDS`).
#[derive(Debug, Clone)]
pub struct CurveStage {
    channels: u32,
    max_frames: u32,
    delay: u32,
    fade_frames: u32,
    ring: Vec<Vec<f32>>,
    write: usize,
    identity: Option<Arc<Vec<f32>>>,
    convolvers: Vec<ChannelConvolverState>,
    outgoing: Vec<ChannelConvolverState>,
    own_mix: f64,
    outgoing_mix: f64,
    warm_left: u64,
    delayed: Vec<f32>,
    own_out: Vec<f32>,
    outgoing_out: Vec<f32>,
    failed: bool,
}

impl CurveStage {
    pub fn new(
        kernel: &[f32],
        delay_frames: u32,
        sample_rate: u32,
        channels: u32,
        max_frames: u32,
    ) -> Self {
        let channels = channels.max(1);
        let max_frames = max_frames.max(1);
        let delay = delay_frames;
        let fade_frames = ((CURVE_STAGE_FADE_SECONDS * (sample_rate as f64)).round() as u32).max(1);
        let ring_len = (delay as usize) + (max_frames as usize) + 1;
        let ring = vec![vec![0.0_f32; ring_len]; channels as usize];

        let mut stage = Self {
            channels,
            max_frames,
            delay,
            fade_frames,
            ring,
            write: 0,
            identity: None,
            convolvers: Vec::new(),
            outgoing: Vec::with_capacity(channels as usize),
            own_mix: 0.0,
            outgoing_mix: 0.0,
            warm_left: 0,
            delayed: vec![0.0_f32; max_frames as usize],
            own_out: vec![0.0_f32; max_frames as usize],
            outgoing_out: vec![0.0_f32; max_frames as usize],
            failed: false,
        };

        if kernel.is_empty() {
            return stage;
        }

        let shared_kernel = Arc::new(kernel.to_vec());
        stage.convolvers = (0..channels)
            .map(|_| ChannelConvolverState::new(shared_kernel.clone()))
            .collect();
        stage.identity = Some(shared_kernel);
        stage.own_mix = 1.0;
        stage
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    pub fn latency(&self) -> u32 {
        self.delay
    }

    pub fn convolving(&self) -> bool {
        !self.convolvers.is_empty()
    }

    pub fn identity(&self) -> Option<&Arc<Vec<f32>>> {
        self.identity.as_ref()
    }

    fn take_as_outgoing(&mut self, previous: &mut Self, mix: f64) {
        std::mem::swap(&mut self.outgoing, &mut previous.convolvers);
        self.outgoing_mix = mix;
    }

    /// Exact port of `CurveStage::adopt(CurveStage& previous)` in `/native/system-apo/src/curve_stage.cpp`.
    pub fn adopt(&mut self, previous: &mut Self) {
        if previous.channels != self.channels || previous.max_frames != self.max_frames {
            return;
        }

        if previous.ring[0].len() == self.ring[0].len() {
            std::mem::swap(&mut self.ring, &mut previous.ring);
            self.write = previous.write;
        } else {
            let size = self.ring[0].len();
            let before = previous.ring[0].len();
            let keep = size.min(before);
            for ch in 0..(self.channels as usize) {
                for back in 1..=keep {
                    self.ring[ch][(size - back) % size] =
                        previous.ring[ch][(previous.write + before - back) % before];
                }
            }
            self.write = 0;
        }

        // Identical kernel: keep existing convolvers and ramp state directly
        if let (Some(own_id), Some(prev_id)) = (&self.identity, &previous.identity) {
            if own_id.as_slice() == prev_id.as_slice() {
                std::mem::swap(&mut self.convolvers, &mut previous.convolvers);
                std::mem::swap(&mut self.outgoing, &mut previous.outgoing);
                self.own_mix = previous.own_mix;
                self.outgoing_mix = previous.outgoing_mix;
                self.warm_left = previous.warm_left;
                return;
            }
        }

        if !self.convolvers.is_empty() {
            let mut handed_over = previous.convolvers.len() == self.convolvers.len();
            let transfer_fade = self.fade_frames * 5 / 2;
            for ch in 0..self.convolvers.len() {
                if !handed_over {
                    break;
                }
                handed_over = self.convolvers[ch].transfer_from(&previous.convolvers[ch], transfer_fade);
            }
            if handed_over {
                self.own_mix = previous.own_mix;
                self.warm_left = previous.warm_left;
                std::mem::swap(&mut self.outgoing, &mut previous.outgoing);
                self.outgoing_mix = previous.outgoing_mix;
                return;
            }
            self.own_mix = 0.0;
            self.warm_left = self
                .identity
                .as_ref()
                .map(|k| k.len() as u64)
                .unwrap_or(0);
        } else {
            self.own_mix = 0.0;
            self.warm_left = 0;
        }

        if !previous.convolvers.is_empty() && previous.own_mix > 0.0 {
            let mix = (previous.own_mix + previous.outgoing_mix).min(1.0);
            self.take_as_outgoing(previous, mix);
        } else if !previous.outgoing.is_empty() && previous.outgoing_mix > 0.0 {
            std::mem::swap(&mut self.outgoing, &mut previous.outgoing);
            self.outgoing_mix = previous.outgoing_mix;
        }
    }

    /// Exact port of `CurveStage::process` in `/native/system-apo/src/curve_stage.cpp`.
    pub fn process(&mut self, planar: &mut [&mut [f32]], frames: usize) {
        if frames == 0 || frames > (self.max_frames as usize) || planar.len() < (self.channels as usize) {
            return;
        }
        let size = self.ring[0].len();
        let delay = self.delay as usize;
        let own_heard = !self.convolvers.is_empty()
            && self.own_mix >= 1.0
            && self.outgoing_mix <= 0.0
            && self.warm_left == 0;
        let step = 1.0 / (self.fade_frames as f64);
        let mut final_own_mix = self.own_mix;
        let mut final_outgoing_mix = self.outgoing_mix;

        for ch in 0..(self.channels as usize) {
            let buffer = &mut planar[ch][..frames];
            let ring = &mut self.ring[ch];
            let mut at = self.write;
            for frame in 0..frames {
                ring[at] = buffer[frame];
                self.delayed[frame] = ring[(at + size - delay) % size];
                at = if at + 1 == size { 0 } else { at + 1 };
            }

            if own_heard {
                self.convolvers[ch].process(buffer);
                continue;
            }
            if self.convolvers.is_empty() && self.outgoing_mix <= 0.0 {
                buffer.copy_from_slice(&self.delayed[..frames]);
                continue;
            }
            if !self.convolvers.is_empty() {
                self.own_out[..frames].copy_from_slice(buffer);
                self.convolvers[ch].process(&mut self.own_out[..frames]);
            }
            let outgoing_heard = !self.outgoing.is_empty() && self.outgoing_mix > 0.0;
            if outgoing_heard {
                self.outgoing_out[..frames].copy_from_slice(buffer);
                self.outgoing[ch].process(&mut self.outgoing_out[..frames]);
            }

            let mut own_mix = self.own_mix;
            let mut outgoing_mix = self.outgoing_mix;
            let mut warm = self.warm_left;
            for frame in 0..frames {
                if warm > 0 {
                    warm -= 1;
                } else if !self.convolvers.is_empty() && own_mix < 1.0 {
                    own_mix = (own_mix + step).min(1.0);
                }
                if outgoing_mix > 0.0 && (self.convolvers.is_empty() || warm == 0) {
                    outgoing_mix = (outgoing_mix - step).max(0.0);
                }
                let dry = (1.0 - own_mix - outgoing_mix).max(0.0);
                let mut sample = dry * (self.delayed[frame] as f64);
                if !self.convolvers.is_empty() {
                    sample += own_mix * (self.own_out[frame] as f64);
                }
                if outgoing_heard {
                    sample += outgoing_mix * (self.outgoing_out[frame] as f64);
                }
                buffer[frame] = sample as f32;
            }
            final_own_mix = own_mix;
            final_outgoing_mix = outgoing_mix;
        }

        self.write = (self.write + frames) % size;
        if !own_heard {
            self.own_mix = final_own_mix;
            self.outgoing_mix = final_outgoing_mix;
            self.warm_left = self.warm_left.saturating_sub(frames as u64);
        }
    }
}

// ============================================================================
// 3. 3-Pass State-Carrying Biquad Cascade (`iir_cascade.h` & `iir_cascade.cpp`)
// ============================================================================

/// Crossfade duration (`IirCascade::kFadeSeconds = 0.02` in `iir_cascade.h`).
pub const IIR_CASCADE_FADE_SECONDS: f64 = 0.02;

/// Maximum bands a cascade can fade out from (`IirCascade::kMaxFadeBands = 128` in `iir_cascade.h`).
pub const MAX_FADE_BANDS: usize = 128;

/// One band as `IirCascade` runs it (`IirBand` in `iir_cascade.h`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct IirBand {
    pub filter_type: FeqFilterType,
    pub frequency: f64,
    pub quality: f64,
    pub coefficients: FeqBiquadCoefficients,
    pub channels: u32,
}

/// Maps `SignatureBand::filter_type` to `FeqFilterType` (`to_core_type` in `iir_cascade.cpp`).
pub fn to_core_type(filter_type_code: i32) -> FeqFilterType {
    match filter_type_code {
        1 => FeqFilterType::FEQ_FILTER_NO,
        2 => FeqFilterType::FEQ_FILTER_LSC,
        3 => FeqFilterType::FEQ_FILTER_HSC,
        4 => FeqFilterType::FEQ_FILTER_LPQ,
        5 => FeqFilterType::FEQ_FILTER_HPQ,
        6 => FeqFilterType::FEQ_FILTER_BP,
        _ => FeqFilterType::FEQ_FILTER_PK,
    }
}

/// Exact port of `design_band` in `/native/system-apo/src/iir_cascade.cpp`.
pub fn design_band(band: &SignatureBand, sample_rate: u32, active_channels_mask: u32) -> IirBand {
    let filter_type = to_core_type(band.filter_type);
    let rate = sample_rate as f64;
    let coefficients = if band.matched {
        biquad_coefficients_designed(
            filter_type,
            band.frequency,
            band.gain_db,
            band.quality,
            rate,
            FeqEqModel::FEQ_EQ_MODEL_CLEAN,
            0.0,
            true,
        )
    } else {
        biquad_coefficients_cookbook(filter_type, band.frequency, band.gain_db, band.quality, rate)
    };
    IirBand {
        filter_type,
        frequency: band.frequency,
        quality: band.quality,
        coefficients,
        channels: if band.channels_all {
            !0u32
        } else {
            active_channels_mask
        },
    }
}

#[inline]
fn runs_on(channels: u32, channel: u32) -> bool {
    channel < 32 && (channels & (1u32 << channel)) != 0
}

#[inline]
fn process_biquad_in_place(
    state: &mut FeqBiquadState,
    buffer: &mut [f32],
    coeffs: &FeqBiquadCoefficients,
) {
    let mut s1 = state.s1;
    let mut s2 = state.s2;
    for sample in buffer.iter_mut() {
        let x = *sample as f64;
        let y = coeffs.b0 * x + s1;
        s1 = coeffs.b1 * x - coeffs.a1 * y + s2;
        s2 = coeffs.b2 * x - coeffs.a2 * y;
        *sample = y as f32;
    }
    state.s1 = s1;
    state.s2 = s2;
}

/// Complete 1:1 port of `fluideq_engine::IirCascade` in `/native/system-apo/src/iir_cascade.cpp`:
/// Carries biquad states across graph rebuilds via a 3-pass matching algorithm (exact match,
/// same frequency with new Q, or same index with dragged frequency) while running the outgoing
/// cascade in parallel over a 20 ms equal-gain raised-cosine crossfade.
#[derive(Debug, Clone)]
pub struct IirCascade {
    bands: Vec<IirBand>,
    states: Vec<FeqBiquadState>,
    outgoing: Vec<FeqBiquadCoefficients>,
    outgoing_channels: Vec<u32>,
    outgoing_states: Vec<FeqBiquadState>,
    outgoing_count: usize,
    scratch: Vec<f32>,
    channels: u32,
    max_frames: u32,
    fade_frames: u32,
    fade_total: u32,
    fade_left: u32,
}

impl IirCascade {
    pub fn new(bands: Vec<IirBand>, sample_rate: u32, channels: u32, max_frames: u32) -> Self {
        let channels = channels.max(1);
        let max_frames = max_frames.max(1);
        let count = bands.len();
        let fade_frames = ((IIR_CASCADE_FADE_SECONDS * (sample_rate as f64)).round() as u32).max(1);
        Self {
            bands,
            states: vec![FeqBiquadState::default(); (channels as usize) * count],
            outgoing: vec![FeqBiquadCoefficients::default(); MAX_FADE_BANDS],
            outgoing_channels: vec![!0u32; MAX_FADE_BANDS],
            outgoing_states: vec![FeqBiquadState::default(); (channels as usize) * MAX_FADE_BANDS],
            outgoing_count: 0,
            scratch: vec![0.0_f32; max_frames as usize],
            channels,
            max_frames,
            fade_frames,
            fade_total: 0,
            fade_left: 0,
        }
    }

    pub fn empty(&self) -> bool {
        self.bands.is_empty()
    }

    pub fn fading(&self) -> bool {
        self.fade_left > 0
    }

    pub fn bands(&self) -> &[IirBand] {
        &self.bands
    }

    pub fn same_response(&self, other: &Self) -> bool {
        if self.bands.len() != other.bands.len() {
            return false;
        }
        for (a, b) in self.bands.iter().zip(other.bands.iter()) {
            if a.coefficients != b.coefficients || a.channels != b.channels {
                return false;
            }
        }
        true
    }

    /// Exact port of `IirCascade::adopt(const IirCascade& previous)` in `/native/system-apo/src/iir_cascade.cpp`.
    pub fn adopt(&mut self, previous: &Self) {
        if previous.channels != self.channels {
            return;
        }
        let count = self.bands.len();
        let before = previous.bands.len();

        if previous.fade_left > 0 {
            self.outgoing_count = previous.outgoing_count;
            self.outgoing[..self.outgoing_count]
                .copy_from_slice(&previous.outgoing[..self.outgoing_count]);
            self.outgoing_channels[..self.outgoing_count]
                .copy_from_slice(&previous.outgoing_channels[..self.outgoing_count]);
            self.outgoing_states.copy_from_slice(&previous.outgoing_states);
            self.fade_total = previous.fade_total;
            self.fade_left = previous.fade_left;
        } else if !self.same_response(previous) && before <= MAX_FADE_BANDS {
            self.outgoing_count = before;
            for band in 0..before {
                self.outgoing[band] = previous.bands[band].coefficients;
                self.outgoing_channels[band] = previous.bands[band].channels;
            }
            for ch in 0..(self.channels as usize) {
                let src_start = ch * before;
                let dst_start = ch * MAX_FADE_BANDS;
                self.outgoing_states[dst_start..dst_start + before]
                    .copy_from_slice(&previous.states[src_start..src_start + before]);
            }
            self.fade_total = self.fade_frames;
            self.fade_left = self.fade_frames;
        }

        // 3-pass band state carry-over (`iir_cascade.cpp` lines 140-176)
        let mut taken = [false; MAX_FADE_BANDS];
        let mut found = [false; MAX_FADE_BANDS];
        let searched = before.min(MAX_FADE_BANDS);

        for pass in 0..3 {
            for band in 0..count.min(MAX_FADE_BANDS) {
                if found[band] {
                    continue;
                }
                let now = &self.bands[band];
                if pass == 2 {
                    if band < searched
                        && !taken[band]
                        && previous.bands[band].filter_type == now.filter_type
                        && previous.bands[band].channels == now.channels
                    {
                        for ch in 0..(self.channels as usize) {
                            self.states[ch * count + band] = previous.states[ch * before + band];
                        }
                        taken[band] = true;
                        found[band] = true;
                    }
                    continue;
                }
                for from in 0..searched {
                    let then = &previous.bands[from];
                    if taken[from]
                        || then.filter_type != now.filter_type
                        || then.channels != now.channels
                        || then.frequency != now.frequency
                        || (pass == 0 && then.quality != now.quality)
                    {
                        continue;
                    }
                    for ch in 0..(self.channels as usize) {
                        self.states[ch * count + band] = previous.states[ch * before + from];
                    }
                    taken[from] = true;
                    found[band] = true;
                    break;
                }
            }
        }
    }

    /// Exact port of `IirCascade::fade_in()` in `/native/system-apo/src/iir_cascade.cpp`.
    pub fn fade_in(&mut self) {
        if self.bands.is_empty() {
            return;
        }
        self.outgoing_count = 0;
        self.fade_total = self.fade_frames;
        self.fade_left = self.fade_frames;
    }

    /// Exact port of `IirCascade::process` in `/native/system-apo/src/iir_cascade.cpp`.
    pub fn process(&mut self, planar: &mut [&mut [f32]], frames: usize) {
        if frames == 0 || frames > (self.max_frames as usize) || planar.len() < (self.channels as usize) {
            return;
        }
        let count = self.bands.len();
        let fading = self.fade_left > 0;
        let done = self.fade_total.saturating_sub(self.fade_left);

        for ch in 0..(self.channels as usize) {
            let buffer = &mut planar[ch][..frames];
            if fading {
                self.scratch[..frames].copy_from_slice(buffer);
                let old_base = ch * MAX_FADE_BANDS;
                for band in 0..self.outgoing_count {
                    if runs_on(self.outgoing_channels[band], ch as u32) {
                        process_biquad_in_place(
                            &mut self.outgoing_states[old_base + band],
                            &mut self.scratch[..frames],
                            &self.outgoing[band],
                        );
                    }
                }
            }

            let base = ch * count;
            for band in 0..count {
                if runs_on(self.bands[band].channels, ch as u32) {
                    process_biquad_in_place(
                        &mut self.states[base + band],
                        buffer,
                        &self.bands[band].coefficients,
                    );
                }
            }

            if fading {
                for frame in 0..frames {
                    let at = done + (frame as u32);
                    if at >= self.fade_total {
                        break;
                    }
                    let weight =
                        0.5 - 0.5 * (PI * (at as f64) / (self.fade_total as f64)).cos();
                    let outgoing = self.scratch[frame] as f64;
                    let current = buffer[frame] as f64;
                    buffer[frame] = (outgoing + weight * (current - outgoing)) as f32;
                }
            }
        }

        if fading {
            self.fade_left = self.fade_left.saturating_sub(frames as u32);
        }
    }

    /// Exact port of `IirCascade::reset()` in `/native/system-apo/src/iir_cascade.cpp`.
    pub fn reset(&mut self) {
        for state in &mut self.states {
            *state = FeqBiquadState::default();
        }
        self.fade_left = 0;
        self.outgoing_count = 0;
    }
}

// ============================================================================
// 4. APO Format Negotiation (`apo_format.cpp`)
// ============================================================================

/// Maximum supported channels (`kMaxChannels = 8` in `apo_format.cpp`).
pub const MAX_APO_CHANNELS: u16 = 8;

/// Windows format tags (`WAVE_FORMAT_IEEE_FLOAT = 0x0003`, `WAVE_FORMAT_EXTENSIBLE = 0xFFFE`).
pub const WAVE_FORMAT_IEEE_FLOAT_TAG: u16 = 0x0003;
pub const WAVE_FORMAT_EXTENSIBLE_TAG: u16 = 0xFFFE;

/// `sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX) = 22`.
pub const EXTENSIBLE_CB_SIZE: u16 = 22;

/// Input wave format descriptor modeling `WAVEFORMATEX` / `WAVEFORMATEXTENSIBLE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaveFormatDescriptor {
    pub format_tag: u16,
    pub channels: u16,
    pub samples_per_sec: u32,
    pub avg_bytes_per_sec: u32,
    pub block_align: u16,
    pub bits_per_sample: u16,
    pub cb_size: u16,
    pub valid_bits_per_sample: u16,
    pub channel_mask: u32,
    pub is_ieee_float_subformat: bool,
}

/// Described connection format (`ConnectionFormat` in `apo.h`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionFormat {
    pub acceptable: bool,
    pub channels: u32,
    pub rate: u32,
    pub mask: u32,
}

/// Exact port of `describe_format` in `/native/system-apo/src/apo_format.cpp`.
pub fn describe_format(format: Option<&WaveFormatDescriptor>) -> ConnectionFormat {
    let Some(fmt) = format else {
        return ConnectionFormat::default();
    };
    let mut described = ConnectionFormat {
        acceptable: false,
        channels: fmt.channels as u32,
        rate: fmt.samples_per_sec,
        mask: 0,
    };

    if fmt.bits_per_sample != 32
        || fmt.channels == 0
        || fmt.channels > MAX_APO_CHANNELS
        || fmt.samples_per_sec == 0
        || fmt.block_align != fmt.channels.saturating_mul(4)
        || (fmt.avg_bytes_per_sec as u64)
            != (fmt.samples_per_sec as u64) * (fmt.block_align as u64)
    {
        return described;
    }

    if fmt.format_tag == WAVE_FORMAT_IEEE_FLOAT_TAG {
        described.acceptable = true;
        described.mask = 0;
        return described;
    }

    if fmt.format_tag != WAVE_FORMAT_EXTENSIBLE_TAG || fmt.cb_size < EXTENSIBLE_CB_SIZE {
        return described;
    }

    described.mask = fmt.channel_mask;
    if fmt.valid_bits_per_sample != 32 && fmt.valid_bits_per_sample != 0 {
        return described;
    }
    described.acceptable = fmt.is_ieee_float_subformat;
    described
}

/// Exact port of `suggest_float` in `/native/system-apo/src/apo_format.cpp`.
pub fn suggest_float(requested: Option<&WaveFormatDescriptor>) -> WaveFormatDescriptor {
    let mut channels = requested.map(|r| r.channels).unwrap_or(2);
    if channels == 0 {
        channels = 2;
    }
    if channels > MAX_APO_CHANNELS {
        channels = MAX_APO_CHANNELS;
    }
    let mut rate = requested.map(|r| r.samples_per_sec).unwrap_or(48_000);
    if rate == 0 {
        rate = 48_000;
    }
    let block_align = channels * 4;
    let channel_mask = match requested {
        Some(req)
            if req.channels == channels
                && req.format_tag == WAVE_FORMAT_EXTENSIBLE_TAG
                && req.cb_size >= EXTENSIBLE_CB_SIZE =>
        {
            req.channel_mask
        }
        _ => 0,
    };

    WaveFormatDescriptor {
        format_tag: WAVE_FORMAT_EXTENSIBLE_TAG,
        channels,
        samples_per_sec: rate,
        avg_bytes_per_sec: rate * (block_align as u32),
        block_align,
        bits_per_sample: 32,
        cb_size: EXTENSIBLE_CB_SIZE,
        valid_bits_per_sample: 32,
        channel_mask,
        is_ieee_float_subformat: true,
    }
}

/// Format negotiation outcome returned by `is_input_format_supported` / `is_output_format_supported`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatNegotiationOutcome {
    /// `S_OK`: `requested` is accepted as-is.
    Accepted(WaveFormatDescriptor),
    /// `S_FALSE`: `suggested` (either `opposite` or `suggest_float(requested)`) is returned in `*supported`.
    Suggested(WaveFormatDescriptor),
    /// `S_FALSE` when `supported == nullptr` (`want_suggestion == false`).
    RejectedWithoutSuggestion,
    /// `E_POINTER`: `requested == nullptr`.
    NullPointer,
    /// `APOERR_FORMAT_NOT_SUPPORTED`: `opposite` is unacceptable.
    UnsupportedOppositeFormat,
}

/// Exact port of `Apo::IsInputFormatSupported` / `Apo::IsOutputFormatSupported` in `apo_format.cpp`.
pub fn negotiate_apo_format(
    endpoint_guid: &str,
    locked: bool,
    opposite: Option<&WaveFormatDescriptor>,
    requested: Option<&WaveFormatDescriptor>,
    want_suggestion: bool,
) -> FormatNegotiationOutcome {
    if !locked {
        trace(endpoint_guid, "format asked about");
    }
    let Some(req) = requested else {
        return FormatNegotiationOutcome::NullPointer;
    };
    let input = describe_format(Some(req));
    let output = describe_format(opposite);

    if let Some(other) = opposite {
        if !output.acceptable {
            trace(
                endpoint_guid,
                &format!(
                    "format refused: the other side is {} ch, {} Hz, {} bits, tag {}",
                    other.channels, other.samples_per_sec, other.bits_per_sample, other.format_tag
                ),
            );
            return FormatNegotiationOutcome::UnsupportedOppositeFormat;
        }
    }

    if input.acceptable
        && (opposite.is_none() || (input.channels == output.channels && input.rate == output.rate))
    {
        return FormatNegotiationOutcome::Accepted(*req);
    }

    if !want_suggestion {
        return FormatNegotiationOutcome::RejectedWithoutSuggestion;
    }

    if let Some(opp) = opposite {
        return FormatNegotiationOutcome::Suggested(*opp);
    }

    trace(
        endpoint_guid,
        &format!(
            "format not taken as offered: {} ch, {} Hz, {} bits, tag {}; float32 suggested",
            req.channels, req.samples_per_sec, req.bits_per_sample, req.format_tag
        ),
    );
    FormatNegotiationOutcome::Suggested(suggest_float(Some(req)))
}

// ============================================================================
// 5. APO Identity, Registration, Initialization & Real-Time `APOProcess`
//    (`apo_identity.cpp`, `apo_endpoint.cpp`, `apo.cpp`, `apo_process.cpp`)
// ============================================================================

/// `{B7E2C4D1-5A8F-4C3E-9D2B-6F1A0C8E7D34}` (`kEngineClsid` in `apo.cpp`).
pub const ENGINE_CLSID_STR: &str = "{B7E2C4D1-5A8F-4C3E-9D2B-6F1A0C8E7D34}";

/// `{6E2B7F3C-1A9D-4C5E-8B7A-2F4D6C8E1B39}` (`kEffectId` in `apo.cpp`).
pub const EFFECT_ID_STR: &str = "{6E2B7F3C-1A9D-4C5E-8B7A-2F4D6C8E1B39}";

/// `{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}` (`kDefaultProcessingMode` in `apo.cpp`).
pub const DEFAULT_PROCESSING_MODE_STR: &str = "{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}";

/// Exact x64 byte sizes of `APOInitBaseStruct` (16), `APOInitSystemEffects` (56),
/// `APOInitSystemEffects2` (88), and `APOInitSystemEffects3` (80) (`apo.cpp` lines 43-54).
pub const APO_INIT_BASE_SIZE: u32 = 16;
pub const APO_INIT_V1_SIZE_X64: u32 = 56;
pub const APO_INIT_V2_SIZE_X64: u32 = 88;
pub const APO_INIT_V3_SIZE_X64: u32 = 80;

/// Above this a sample is sound, under it the stream is idle: `-90 dBFS` (`kSilenceFloor = 3.2e-5f` in `apo_process.cpp`).
pub const SILENCE_FLOOR: f32 = 3.2e-5;

/// Global live COM object counter (`g_object_count` in `apo.cpp` / `apo_identity.cpp`).
static G_OBJECT_COUNT: AtomicI64 = AtomicI64::new(0);

pub fn active_apo_object_count() -> i64 {
    G_OBJECT_COUNT.load(Ordering::Relaxed)
}

/// COM interface IDs supported by `Apo::query_inner` (`apo_identity.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApoComInterface {
    IUnknown,
    IAudioProcessingObject,
    IAudioProcessingObjectRT,
    IAudioProcessingObjectConfiguration,
    IAudioSystemEffects,
    IAudioSystemEffects2,
    IAudioSystemEffects3,
}

/// HRESULT status codes returned by APO COM methods (`apo.cpp`, `apo_process.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApoHResult {
    SucceedOk,
    SucceedFalse,
    ErrPointer,
    ErrInvalidArg,
    ErrNoInterface,
    ErrOutOfMemory,
    ErrNotImpl,
    ErrFail,
    ApoErrLocked,
    ApoErrAlreadyUnlocked,
    ApoErrAlreadyInitialized,
    ApoErrNumConnectionsInvalid,
    ApoErrInvalidConnectionFormat,
    ApoErrFormatNotSupported,
    ApoErrInvalidOutputMaxFrameCount,
}

/// Registration properties returned by `Apo::GetRegistrationProperties` (`apo.cpp`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApoRegistrationProperties {
    pub clsid: String,
    pub friendly_name: String,
    pub copyright_info: String,
    pub major_version: u32,
    pub minor_version: u32,
    pub min_input_connections: u32,
    pub max_input_connections: u32,
    pub min_output_connections: u32,
    pub max_output_connections: u32,
    pub max_instances: u32,
    pub inplace: bool,
    pub samples_per_frame_must_match: bool,
    pub frames_per_second_must_match: bool,
    pub bits_per_sample_must_match: bool,
}

/// Controllable effect entry returned by `Apo::GetControllableSystemEffectsList` (`apo.cpp`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllableSystemEffect {
    pub id: String,
    pub can_set_state: bool,
    pub state_on: bool,
}

/// Connection descriptor passed to `Apo::LockForProcess` (`APO_CONNECTION_DESCRIPTOR` in `apo_process.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApoConnectionDescriptor {
    pub format: Option<WaveFormatDescriptor>,
    pub max_frame_count: u32,
    pub has_buffer: bool,
}

/// Buffer flags on `APO_CONNECTION_PROPERTY` (`BUFFER_INVALID = 0`, `BUFFER_VALID = 1`, `BUFFER_SILENT = 2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApoBufferFlags {
    Invalid = 0,
    Valid = 1,
    Silent = 2,
}

/// Exact port of `Apo::read_endpoint_properties` and `Apo::read_endpoint` in `/native/system-apo/src/apo_endpoint.cpp`:
/// Canonicalizes the endpoint GUID (`{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`) and prefers the
/// endpoint property store over the device collection fallback.
pub fn resolve_apo_endpoint_identity(
    endpoint_store_guid: Option<&str>,
    endpoint_store_friendly_name: Option<&str>,
    collection_device_guid: Option<&str>,
    collection_device_friendly_name: Option<&str>,
) -> (String, String) {
    let normalize_guid = |raw: &str| -> Option<String> {
        let trimmed = raw.trim();
        let inner = if trimmed.len() == 38 && trimmed.starts_with('{') && trimmed.ends_with('}') {
            &trimmed[1..37]
        } else if trimmed.len() == 36 {
            trimmed
        } else {
            return None;
        };
        for (at, b) in inner.bytes().enumerate() {
            if matches!(at, 8 | 13 | 18 | 23) {
                if b != b'-' {
                    return None;
                }
            } else if !b.is_ascii_hexdigit() {
                return None;
            }
        }
        Some(format!("{{{}}}", inner.to_ascii_uppercase()))
    };

    let mut guid = endpoint_store_guid
        .and_then(normalize_guid)
        .unwrap_or_default();
    let mut name = endpoint_store_friendly_name
        .unwrap_or_default()
        .to_string();

    if guid.is_empty() {
        if let Some(fallback_guid) = collection_device_guid.and_then(normalize_guid) {
            guid = fallback_guid;
        }
        if name.is_empty() {
            if let Some(fallback_name) = collection_device_friendly_name {
                name = fallback_name.to_string();
            }
        }
    }
    (guid, name)
}

/// Complete 1:1 port of `fluideq_engine::Apo` across `apo_identity.cpp`, `apo_endpoint.cpp`,
/// `apo.cpp`, and `apo_process.cpp`.
pub struct ApoInstance {
    references: AtomicU32,
    aggregated: bool,
    pub endpoint_guid: String,
    pub endpoint_friendly_name: String,
    pub processing_mode: String,
    pub initialized: bool,
    pub locked: bool,
    pub channels: u32,
    pub sample_rate: AtomicU32,
    pub max_frames: u32,
    pub scratch: Vec<f32>,
    pub carried: Option<Arc<AtomicBool>>,
    pub carried_told: bool,
    pub timing: Option<Arc<BlockTiming>>,
    pub ticks_per_second: u64,
    pub timing_step_told: u32,
    pub blocks_timed: u64,
    pub reported_graph_id: usize,
    pub slot: Arc<GraphSlot>,
    pub log: Option<EngineLog>,
    pub reset_requests: u32,
    pub carried_wakes: u32,
    pub slow_wakes: u32,
    pub adopted_wakes: u32,
}

impl ApoInstance {
    pub fn new(aggregated: bool) -> Self {
        G_OBJECT_COUNT.fetch_add(1, Ordering::Relaxed);
        Self {
            references: AtomicU32::new(1),
            aggregated,
            endpoint_guid: String::new(),
            endpoint_friendly_name: String::new(),
            processing_mode: String::new(),
            initialized: false,
            locked: false,
            channels: 0,
            sample_rate: AtomicU32::new(0),
            max_frames: 0,
            scratch: Vec::new(),
            carried: None,
            carried_told: false,
            timing: None,
            ticks_per_second: 10_000_000,
            timing_step_told: 0,
            blocks_timed: 0,
            reported_graph_id: 0,
            slot: Arc::new(GraphSlot::new()),
            log: None,
            reset_requests: 0,
            carried_wakes: 0,
            slow_wakes: 0,
            adopted_wakes: 0,
        }
    }

    pub fn is_aggregated(&self) -> bool {
        self.aggregated
    }

    pub fn add_ref(&self) -> u32 {
        self.references.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn release_ref(&self) -> u32 {
        self.references.fetch_sub(1, Ordering::AcqRel) - 1
    }

    /// Exact port of `Apo::is_default_processing_mode()` in `apo.cpp`.
    pub fn is_default_processing_mode(&self) -> bool {
        self.processing_mode.is_empty()
            || self.processing_mode == "{00000000-0000-0000-0000-000000000000}"
            || self
                .processing_mode
                .eq_ignore_ascii_case(DEFAULT_PROCESSING_MODE_STR)
    }

    /// Exact port of `Apo::GetLatency(HNSTIME* time)` in `/native/system-apo/src/apo.cpp`:
    /// Converts `slot_.latency()` frames into 100-nanosecond `HNSTIME` units using 64-bit integer arithmetic.
    pub fn get_latency_hnstime(&self) -> i64 {
        let frames = self.slot.latency();
        let rate = self.sample_rate.load(Ordering::Relaxed);
        if frames == 0 || rate == 0 {
            return 0;
        }
        ((frames as u64) * 10_000_000u64 / (rate as u64)) as i64
    }

    /// Exact port of `Apo::GetRegistrationProperties` in `/native/system-apo/src/apo.cpp`.
    pub fn get_registration_properties(&self) -> ApoRegistrationProperties {
        trace(&self.endpoint_guid, "registration properties asked for");
        ApoRegistrationProperties {
            clsid: ENGINE_CLSID_STR.to_string(),
            friendly_name: "FluidEQ Engine".to_string(),
            copyright_info: "Copyright (C) 2026 Ivan Carmenates Garcia. GPL-3.0-or-later."
                .to_string(),
            major_version: 1,
            minor_version: 0,
            min_input_connections: 1,
            max_input_connections: 1,
            min_output_connections: 1,
            max_output_connections: 1,
            max_instances: u32::MAX,
            inplace: true,
            samples_per_frame_must_match: true,
            frames_per_second_must_match: true,
            bits_per_sample_must_match: true,
        }
    }

    /// Exact port of `Apo::Initialize(UINT32 size, BYTE* data)` in `/native/system-apo/src/apo.cpp`:
    /// Validates `locked_`, `initialized_`, and distinguishes `APOInitSystemEffects` (56B),
    /// `APOInitSystemEffects2` (88B), and `APOInitSystemEffects3` (80B) by exact `declared_cb_size`.
    pub fn initialize(
        &mut self,
        size: u32,
        has_data: bool,
        declared_cb_size: u32,
        processing_mode: Option<&str>,
        endpoint_store_guid: Option<&str>,
        endpoint_store_friendly_name: Option<&str>,
        collection_device_guid: Option<&str>,
        collection_device_friendly_name: Option<&str>,
    ) -> ApoHResult {
        if self.locked {
            return ApoHResult::ApoErrLocked;
        }
        if self.initialized {
            return ApoHResult::ApoErrAlreadyInitialized;
        }
        if size == 0 && !has_data {
            self.initialized = true;
            return ApoHResult::SucceedOk;
        }
        if !has_data || size < APO_INIT_BASE_SIZE || declared_cb_size > size {
            return ApoHResult::ErrInvalidArg;
        }

        trace(
            "",
            &format!(
                "initialize: {} bytes, declared layout {}",
                size, declared_cb_size
            ),
        );

        match declared_cb_size {
            APO_INIT_V3_SIZE_X64 | APO_INIT_V2_SIZE_X64 => {
                self.processing_mode = processing_mode.unwrap_or_default().to_string();
                let (guid, name) = resolve_apo_endpoint_identity(
                    endpoint_store_guid,
                    endpoint_store_friendly_name,
                    collection_device_guid,
                    collection_device_friendly_name,
                );
                self.endpoint_guid = guid;
                self.endpoint_friendly_name = name;
            }
            APO_INIT_V1_SIZE_X64 => {
                self.processing_mode.clear();
                let (guid, name) = resolve_apo_endpoint_identity(
                    endpoint_store_guid,
                    endpoint_store_friendly_name,
                    collection_device_guid,
                    collection_device_friendly_name,
                );
                self.endpoint_guid = guid;
                self.endpoint_friendly_name = name;
            }
            _ => {
                trace("", "initialize refused: unsupported structure layout");
                return ApoHResult::ErrInvalidArg;
            }
        }

        trace(
            &self.endpoint_guid,
            &format!(
                "initialized for \"{}\"{}",
                self.endpoint_friendly_name,
                if self.is_default_processing_mode() {
                    ""
                } else {
                    ", not the default mode"
                }
            ),
        );
        self.initialized = true;
        ApoHResult::SucceedOk
    }

    /// Exact port of `Apo::Reset()` in `/native/system-apo/src/apo.cpp`.
    pub fn reset(&mut self) -> ApoHResult {
        if self.locked {
            self.reset_requests += 1;
        }
        ApoHResult::SucceedOk
    }

    /// Exact port of `Apo::GetEffectsList` in `/native/system-apo/src/apo.cpp`.
    pub fn get_effects_list(&self) -> Vec<String> {
        if !self.locked {
            trace(&self.endpoint_guid, "effects list asked for");
        }
        vec![EFFECT_ID_STR.to_string()]
    }

    /// Exact port of `Apo::GetControllableSystemEffectsList` in `/native/system-apo/src/apo.cpp`.
    pub fn get_controllable_system_effects_list(&self) -> Vec<ControllableSystemEffect> {
        if !self.locked {
            trace(&self.endpoint_guid, "controllable effects list asked for");
        }
        vec![ControllableSystemEffect {
            id: EFFECT_ID_STR.to_string(),
            can_set_state: false,
            state_on: true,
        }]
    }

    /// Exact port of `Apo::SetAudioSystemEffectState` in `/native/system-apo/src/apo.cpp`.
    pub fn set_audio_system_effect_state(&self, effect_id: &str) -> ApoHResult {
        if effect_id.eq_ignore_ascii_case(EFFECT_ID_STR) {
            ApoHResult::ErrNotImpl
        } else {
            ApoHResult::ErrInvalidArg
        }
    }

    pub fn calc_input_frames(&self, output_frames: u32) -> u32 {
        output_frames
    }

    pub fn calc_output_frames(&self, input_frames: u32) -> u32 {
        input_frames
    }

    /// Exact port of `Apo::LockForProcess` in `/native/system-apo/src/apo_process.cpp`.
    pub fn lock_for_process(
        &mut self,
        inputs: &[Option<ApoConnectionDescriptor>],
        outputs: &[Option<ApoConnectionDescriptor>],
        log_file_path: Option<&std::path::Path>,
    ) -> ApoHResult {
        if self.locked {
            return ApoHResult::ApoErrLocked;
        }
        if inputs.len() != 1 || outputs.len() != 1 {
            trace(
                &self.endpoint_guid,
                &format!(
                    "lock refused: {} inputs and {} outputs; this effect takes one of each",
                    inputs.len(),
                    outputs.len()
                ),
            );
            return ApoHResult::ApoErrNumConnectionsInvalid;
        }
        let (Some(in_conn), Some(out_conn)) = (inputs[0], outputs[0]) else {
            trace(&self.endpoint_guid, "lock refused: a connection was missing");
            return ApoHResult::ErrPointer;
        };
        if in_conn.format.is_none() || out_conn.format.is_none() {
            trace(
                &self.endpoint_guid,
                "lock refused: a connection carried no format",
            );
            return ApoHResult::ApoErrInvalidConnectionFormat;
        }

        let input_format = describe_format(in_conn.format.as_ref());
        let output_format = describe_format(out_conn.format.as_ref());
        let describe = |f: &ConnectionFormat| -> String {
            format!(
                "{} ch {} Hz{}",
                f.channels,
                f.rate,
                if f.acceptable {
                    ""
                } else {
                    " (not float32, or too many channels)"
                }
            )
        };

        if !input_format.acceptable || !output_format.acceptable {
            trace(
                &self.endpoint_guid,
                &format!(
                    "lock refused: format in {}, out {}",
                    describe(&input_format),
                    describe(&output_format)
                ),
            );
            return ApoHResult::ApoErrFormatNotSupported;
        }
        if input_format.channels != output_format.channels || input_format.rate != output_format.rate
        {
            trace(
                &self.endpoint_guid,
                &format!(
                    "lock refused: in {} does not match out {}",
                    describe(&input_format),
                    describe(&output_format)
                ),
            );
            return ApoHResult::ApoErrFormatNotSupported;
        }
        if in_conn.max_frame_count == 0 {
            trace(
                &self.endpoint_guid,
                "lock refused: zero-frame input connection",
            );
            return ApoHResult::ApoErrInvalidConnectionFormat;
        }
        if out_conn.max_frame_count < in_conn.max_frame_count {
            trace(
                &self.endpoint_guid,
                &format!(
                    "lock refused: output holds {} frames, input up to {}",
                    out_conn.max_frame_count, in_conn.max_frame_count
                ),
            );
            return ApoHResult::ApoErrInvalidOutputMaxFrameCount;
        }
        if !in_conn.has_buffer || !out_conn.has_buffer {
            trace(
                &self.endpoint_guid,
                "lock refused: a connection had no buffer",
            );
            return ApoHResult::ErrPointer;
        }

        self.channels = input_format.channels;
        self.sample_rate.store(input_format.rate, Ordering::Relaxed);
        self.max_frames = in_conn.max_frame_count;
        self.scratch
            .resize((self.channels as usize) * (self.max_frames as usize), 0.0);
        self.carried = output_carried_flag(&self.endpoint_guid);
        self.carried_told = false;
        self.timing = output_block_timing(&self.endpoint_guid);
        self.timing_step_told = 0;
        self.blocks_timed = 0;

        if let Some(path) = log_file_path {
            let log = EngineLog::new(path, &self.endpoint_guid);
            log.write(&format!(
                "locked: {} ch, {} Hz, up to {} frames, device \"{}\"",
                self.channels,
                input_format.rate,
                self.max_frames,
                self.endpoint_friendly_name
            ));
            if !self.is_default_processing_mode() {
                log.write("this instance is not the default signal-processing mode");
            }
            self.log = Some(log);
        }

        self.locked = true;
        ApoHResult::SucceedOk
    }

    /// Exact port of `Apo::UnlockForProcess()` and `Apo::release_locked_state()` in `apo_process.cpp`.
    pub fn unlock_for_process(&mut self) -> ApoHResult {
        if !self.locked {
            return ApoHResult::ApoErrAlreadyUnlocked;
        }
        self.release_locked_state();
        ApoHResult::SucceedOk
    }

    pub fn release_locked_state(&mut self) {
        self.slot.clear();
        self.slot.set_latency(0);
        if let (Some(log), Some(timing)) = (&self.log, &self.timing) {
            if self.blocks_timed > 0 {
                log.write(&format!("this stream: {}", describe_timing(&timing.read())));
            }
        }
        self.timing = None;
        self.log = None;
        self.scratch = Vec::new();
        self.carried = None;
        self.carried_told = false;
        self.reported_graph_id = 0;
        self.channels = 0;
        self.sample_rate.store(0, Ordering::Relaxed);
        self.max_frames = 0;
        self.locked = false;
    }

    /// Exact port of `Apo::APOProcess` in `/native/system-apo/src/apo_process.cpp`:
    /// Adopts the pending graph at the block boundary, checks `usable` (`frames <= max_frames_`),
    /// handles `BUFFER_SILENT` / `BUFFER_VALID`, detects `-90 dBFS` (`SILENCE_FLOOR`) audio arrival
    /// once per lock, deinterleaves into `scratch`, runs `process_planar` when not in bypass,
    /// re-interleaves to `output_interleaved`, records `BlockTiming` and quarter-period `timing_step`
    /// wakes, increments `slot.finish_block()`, and notifies `adopted_wakes` on graph change.
    pub fn apo_process<P>(
        &mut self,
        valid_frame_count: u32,
        input_flags: ApoBufferFlags,
        input_interleaved: Option<&[f32]>,
        output_interleaved: Option<&mut [f32]>,
        elapsed_ticks: u64,
        graph_lookup: Option<&WatcherGraphInstance>,
        mut process_planar: P,
    ) -> (u32, ApoBufferFlags)
    where
        P: FnMut(&mut [f32], usize, usize),
    {
        let active_id = self.slot.adopt_with(|_, _| {});
        let frames = valid_frame_count;
        let ch = self.channels as usize;
        let required_samples = ch.saturating_mul(frames as usize);
        let has_in_buf = input_interleaved
            .map(|b| b.len() >= required_samples)
            .unwrap_or(false);
        let has_out_buf = output_interleaved
            .as_ref()
            .map(|b| b.len() >= required_samples)
            .unwrap_or(false);

        let usable = frames <= self.max_frames && has_in_buf && has_out_buf;
        let valid = input_flags == ApoBufferFlags::Valid;
        let is_passthrough = graph_lookup.map(|g| g.is_passthrough).unwrap_or(true);

        let out_flags;
        let out_frames;

        if !usable {
            out_frames = 0;
            out_flags = ApoBufferFlags::Silent;
        } else if !valid {
            out_frames = frames;
            out_flags = input_flags;
        } else {
            let input = input_interleaved.unwrap();
            let output = output_interleaved.unwrap();
            let bypass = active_id == 0 || is_passthrough;

            if !self.carried_told {
                for &sample in &input[..required_samples] {
                    if sample > SILENCE_FLOOR || sample < -SILENCE_FLOOR {
                        self.carried_told = true;
                        if let Some(carried) = &self.carried {
                            carried.store(true, Ordering::Relaxed);
                        }
                        self.carried_wakes += 1;
                        break;
                    }
                }
            }

            if bypass {
                output[..required_samples].copy_from_slice(&input[..required_samples]);
            } else {
                let fr = frames as usize;
                let max_fr = self.max_frames as usize;
                for channel in 0..ch {
                    let plane_offset = channel * max_fr;
                    for frame in 0..fr {
                        self.scratch[plane_offset + frame] = input[frame * ch + channel];
                    }
                }

                process_planar(&mut self.scratch, ch, fr);

                for channel in 0..ch {
                    let plane_offset = channel * max_fr;
                    for frame in 0..fr {
                        output[frame * ch + channel] = self.scratch[plane_offset + frame];
                    }
                }
            }

            out_frames = frames;
            out_flags = ApoBufferFlags::Valid;

            if let Some(timing) = &self.timing {
                if frames > 0 {
                    let slowest = timing.record(
                        elapsed_ticks,
                        self.ticks_per_second,
                        frames,
                        self.sample_rate.load(Ordering::Relaxed),
                    );
                    self.blocks_timed += 1;
                    let step = timing_step(slowest);
                    if step > self.timing_step_told {
                        self.timing_step_told = step;
                        self.slow_wakes += 1;
                    }
                }
            }
        }

        self.slot.finish_block();
        if active_id != self.reported_graph_id {
            self.reported_graph_id = active_id;
            self.adopted_wakes += 1;
        }

        (out_frames, out_flags)
    }
}

impl Drop for ApoInstance {
    fn drop(&mut self) {
        self.release_locked_state();
        G_OBJECT_COUNT.fetch_sub(1, Ordering::Relaxed);
    }
}

// ============================================================================
// 6. Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphic_eq_linear_and_minimum_phase_empty_curves_produce_unit_impulses() {
        let empty: Vec<Vec<ApoGraphicPoint>> = Vec::new();
        let lin = design_graphic_kernel(&empty, 48_000, 129);
        assert_eq!(lin.len(), 129);
        // Peak of linear-phase unit impulse sits at exact centre tap `n / 2 = 64`
        assert!((lin[64] - 1.0).abs() < 1e-5);
        assert!(lin[0].abs() < 1e-5);

        let min_k = design_minimum_graphic_kernel(&empty, 48_000, 128);
        assert_eq!(min_k.len(), 128);
        // Minimum-phase unit impulse starts on tap 0 with zero bulk delay
        assert!((min_k[0] - 1.0).abs() < 1e-5);
        assert!(min_k[64].abs() < 1e-5);
    }

    #[test]
    fn curve_stage_delays_when_empty_and_warms_up_before_fading_to_first_curve() {
        let mut stage_delay = CurveStage::new(&[], 4, 1000, 1, 16);
        assert!(!stage_delay.convolving());
        assert_eq!(stage_delay.latency(), 4);

        let mut buf = [1.0_f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        {
            let mut planes = [&mut buf[..]];
            stage_delay.process(&mut planes, 8);
        }
        // Delay of 4 samples: first 4 outputs are 0.0, then 1.0, 2.0, 3.0, 4.0
        assert_eq!(buf, [0.0, 0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 4.0]);

        // Adopt into a stage with a 4-tap unit impulse kernel
        let mut stage_curve = CurveStage::new(&[1.0, 0.0, 0.0, 0.0], 4, 1000, 1, 16);
        stage_curve.adopt(&mut stage_delay);
        assert_eq!(stage_curve.warm_left, 4);
        assert_eq!(stage_curve.own_mix, 0.0);
    }

    #[test]
    fn iir_cascade_three_pass_band_adopt_and_raised_cosine_crossfade() {
        let b1 = SignatureBand {
            filter_type: 0,
            frequency: 1000.0,
            gain_db: 6.0,
            quality: 1.0,
            user_eq: true,
            curve_layer: false,
            matched: false,
            channels_all: true,
            channels_speakers: 0,
            channels_indices: 0,
        };
        let mut cascade1 = IirCascade::new(vec![design_band(&b1, 48_000, !0)], 48_000, 1, 64);
        let mut audio = vec![0.5_f32; 32];
        {
            let mut planes = [&mut audio[..]];
            cascade1.process(&mut planes, 32);
        }
        assert!(!cascade1.fading());

        // Drag frequency from 1000 Hz to 1200 Hz (Pass 2 carries state by index, while starting 20ms crossfade)
        let mut b2 = b1.clone();
        b2.frequency = 1200.0;
        let mut cascade2 = IirCascade::new(vec![design_band(&b2, 48_000, !0)], 48_000, 1, 64);
        cascade2.adopt(&cascade1);
        assert!(cascade2.fading());
        assert_eq!(cascade2.states[0], cascade1.states[0]);
    }

    #[test]
    fn apo_format_negotiation_and_real_time_process_loop() {
        let pcm16 = WaveFormatDescriptor {
            format_tag: 1,
            channels: 2,
            samples_per_sec: 48_000,
            avg_bytes_per_sec: 192_000,
            block_align: 4,
            bits_per_sample: 16,
            cb_size: 0,
            valid_bits_per_sample: 16,
            channel_mask: 0x3,
            is_ieee_float_subformat: false,
        };
        assert!(!describe_format(Some(&pcm16)).acceptable);

        let outcome = negotiate_apo_format(
            "{11111111-2222-3333-4444-555555555555}",
            false,
            None,
            Some(&pcm16),
            true,
        );
        let FormatNegotiationOutcome::Suggested(float32_fmt) = outcome else {
            panic!("expected float32 suggestion");
        };
        assert!(describe_format(Some(&float32_fmt)).acceptable);

        let mut apo = ApoInstance::new(false);
        assert_eq!(
            apo.initialize(
                APO_INIT_V3_SIZE_X64,
                true,
                APO_INIT_V3_SIZE_X64,
                Some(DEFAULT_PROCESSING_MODE_STR),
                Some("{11111111-2222-3333-4444-555555555555}"),
                Some("Speakers"),
                None,
                None,
            ),
            ApoHResult::SucceedOk
        );
        assert!(apo.is_default_processing_mode());

        let conn = Some(ApoConnectionDescriptor {
            format: Some(float32_fmt),
            max_frame_count: 480,
            has_buffer: true,
        });
        assert_eq!(
            apo.lock_for_process(&[conn], &[conn], None),
            ApoHResult::SucceedOk
        );

        apo.slot.set_latency(480);
        // 480 frames @ 48 kHz = 10 ms = 100,000 HNSTIME (100ns units)
        assert_eq!(apo.get_latency_hnstime(), 100_000);

        let _ = apo.slot.publish(1);
        let graph = WatcherGraphInstance {
            id: 1,
            is_passthrough: false,
            latency_frames: 480,
            ..Default::default()
        };
        let input = [0.25_f32, -0.25, 0.5, -0.5];
        let mut output = [0.0_f32; 4];
        let (frames_out, flags_out) = apo.apo_process(
            2,
            ApoBufferFlags::Valid,
            Some(&input),
            Some(&mut output),
            50_000,
            Some(&graph),
            |scratch, ch, fr| {
                for c in 0..ch {
                    for f in 0..fr {
                        scratch[c * 480 + f] *= 2.0;
                    }
                }
            },
        );
        assert_eq!(frames_out, 2);
        assert_eq!(flags_out, ApoBufferFlags::Valid);
        assert_eq!(output, [0.5, -0.5, 1.0, -1.0]);
        assert!(apo.carried_told);
        assert_eq!(apo.carried_wakes, 1);
        assert_eq!(apo.adopted_wakes, 1);
        assert_eq!(apo.slot.blocks(), 1);
    }
}
