/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Remote Audio Playback Polyphase Kaiser-Bessel Sinc Resampler (`64` taps x `1025` phases),
//! Canonical Windows Surround Fold-Down Matrix (`8x8`), and Adaptive Jitter Cushion (`30 ms` / `300 ms`).
//!
//! Ported from `/native/remote-audio-playback/src/sinc_kernel.cpp` and
//! `/native/remote-audio-playback/src/playback_core.cpp`.

use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

pub const SINC_TAPS: usize = 64;
pub const SINC_PHASES: usize = 1024;
pub const MAX_REMOTE_CHANNELS: usize = 8;
pub const MAX_REMOTE_PEERS: usize = 8;
pub const MAX_PACKET_FRAMES: u32 = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct JitterCushionProfile {
    pub start_ms: f64,
    pub floor_ms: f64,
    pub ceiling_ms: f64,
    pub step_ms: f64,
}

/// Ordinary low-latency cushion (`30 ms` start, `15 ms` floor, `160 ms` ceiling, `10 ms` dropout step).
pub const ORDINARY_CUSHION: JitterCushionProfile = JitterCushionProfile {
    start_ms: 30.0,
    floor_ms: 15.0,
    ceiling_ms: 160.0,
    step_ms: 10.0,
};

/// Smooth music cushion (`300 ms` start/floor, `2000 ms` ceiling, `150 ms` dropout step).
pub const SMOOTH_CUSHION: JitterCushionProfile = JitterCushionProfile {
    start_ms: 300.0,
    floor_ms: 300.0,
    ceiling_ms: 2000.0,
    step_ms: 150.0,
};

pub fn cushion_profile(smooth: bool) -> JitterCushionProfile {
    if smooth {
        SMOOTH_CUSHION
    } else {
        ORDINARY_CUSHION
    }
}

/// Zeroth-order modified Bessel function of the first kind $I_0(x)$.
/// Exact port of `bessel` in `/native/remote-audio-playback/src/sinc_kernel.cpp`.
pub fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    for i in 1..40u32 {
        let denom = 4.0 * (i as f64) * (i as f64);
        term *= (x * x) / denom;
        sum += term;
        if term < sum * 1e-15 {
            break;
        }
    }
    sum
}

/// Builds the `(SINC_PHASES + 1) * SINC_TAPS` (`65,600` floats) polyphase Kaiser-Bessel ($\beta = 10$)
/// windowed Sinc interpolation table for `ratio = output_rate / source_rate`.
/// Exact port of `sinc_kernel` in `/native/remote-audio-playback/src/sinc_kernel.cpp`.
pub fn build_sinc_kernel(ratio: f64) -> Vec<f32> {
    let mut table = vec![0.0f32; (SINC_PHASES + 1) * SINC_TAPS];
    let cutoff = ratio.min(1.0) * 0.94;
    let denominator = bessel_i0(10.0);

    for phase in 0..=SINC_PHASES {
        let mut sum = 0.0;
        let phase_frac = (phase as f64) / (SINC_PHASES as f64);
        let row_offset = phase * SINC_TAPS;
        for tap in 0..SINC_TAPS {
            let x = (tap as f64) - 31.0 - phase_frac;
            let n = x / 32.0;
            let window = bessel_i0(10.0 * (1.0 - n * n).max(0.0).sqrt()) / denominator;
            let sinc = if x.abs() < 1e-12 {
                cutoff
            } else {
                (PI * x * cutoff).sin() / (PI * x)
            };
            let value = sinc * window;
            table[row_offset + tap] = value as f32;
            sum += value;
        }
        let inv_sum = (1.0 / sum) as f32;
        for tap in 0..SINC_TAPS {
            table[row_offset + tap] *= inv_sum;
        }
    }
    table
}

/// Computes the `8x8` channel routing / fold-down matrix (`matrix[out_ch][src_ch]`)
/// between `source_channels` and `(output_channels, speaker_mask)`.
/// Exact port of the matrix builder in `PeerPlayback::output_format` (`/native/remote-audio-playback/src/playback_core.cpp`).
pub fn build_speaker_folddown_matrix(
    source_channels: u16,
    output_channels: u16,
    speaker_mask: u32,
) -> Option<[[f32; MAX_REMOTE_CHANNELS]; MAX_REMOTE_CHANNELS]> {
    let src_ch = source_channels as usize;
    let out_ch = output_channels as usize;
    if src_ch == 0
        || src_ch > MAX_REMOTE_CHANNELS
        || out_ch == 0
        || out_ch > MAX_REMOTE_CHANNELS
    {
        return None;
    }

    let mut matrix = [[0.0f32; MAX_REMOTE_CHANNELS]; MAX_REMOTE_CHANNELS];

    if out_ch >= src_ch && src_ch != 1 {
        for (channel, row) in matrix[..src_ch].iter_mut().enumerate() {
            row[channel] = 1.0;
        }
    } else if src_ch == 1 {
        matrix[0][0] = 1.0;
        if out_ch > 1 {
            matrix[1][0] = 1.0;
        }
    } else {
        let mut source = [0u32; MAX_REMOTE_CHANNELS];
        let mask: u32 = if src_ch == 6 {
            0x3f // 5.1 canonical mask (FL, FR, FC, LFE, BL, BR)
        } else if src_ch == 8 {
            0x63f // 7.1 canonical mask (FL, FR, FC, LFE, BL, BR, SL, SR)
        } else {
            (1u32 << src_ch) - 1
        };

        let mut n = 0usize;
        for bit in 0..18u32 {
            if n < src_ch && (mask & (1u32 << bit)) != 0 {
                source[n] = 1u32 << bit;
                n += 1;
            }
        }

        if out_ch == 2 || out_ch == 1 {
            for c in 0..src_ch {
                let speaker = source[c];
                matrix[0][c] = match speaker {
                    1 => 1.0,
                    4 => std::f32::consts::FRAC_1_SQRT_2, // Center (-3 dB)
                    8 => 0.25,                            // LFE
                    16 | 512 => 0.5,                      // Back Left / Side Left
                    _ => 0.0,
                };
                matrix[1][c] = match speaker {
                    2 => 1.0,
                    4 => std::f32::consts::FRAC_1_SQRT_2, // Center (-3 dB)
                    8 => 0.25,                            // LFE
                    32 | 1024 => 0.5,                     // Back Right / Side Right
                    _ => 0.0,
                };
                if out_ch == 1 {
                    matrix[0][c] = 0.5 * (matrix[0][c] + matrix[1][c]);
                    matrix[1][c] = 0.0;
                }
            }
        } else {
            let mut out = 0usize;
            for bit in 0..18u32 {
                if out >= out_ch {
                    break;
                }
                if (speaker_mask & (1u32 << bit)) == 0 {
                    continue;
                }
                let dst = 1u32 << bit;
                for c in 0..src_ch {
                    let src = source[c];
                    let left_surround_match =
                        matches!(src, 16 | 512) && matches!(dst, 16 | 512);
                    let right_surround_match =
                        matches!(src, 32 | 1024) && matches!(dst, 32 | 1024);
                    if src == dst || left_surround_match || right_surround_match {
                        matrix[out][c] = 1.0;
                    }
                }
                out += 1;
            }
        }
    }

    Some(matrix)
}

/// Adaptive jitter cushion state machine for a remote audio peer (`PeerPlayback` in `playback_core.cpp`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdaptivePlaybackCushion {
    pub smooth: bool,
    pub target_ms: f64,
    pub learning: bool,
    pub starve_base_ms: f64,
    pub starve_frames: u64,
    pub stable_frames: u64,
    pub underruns: u64,
}

impl Default for AdaptivePlaybackCushion {
    fn default() -> Self {
        Self::new(false)
    }
}

impl AdaptivePlaybackCushion {
    pub fn new(smooth: bool) -> Self {
        let profile = cushion_profile(smooth);
        Self {
            smooth,
            target_ms: profile.start_ms,
            learning: false,
            starve_base_ms: 0.0,
            starve_frames: 0,
            stable_frames: 0,
            underruns: 0,
        }
    }

    pub fn set_smooth(&mut self, smooth: bool) {
        if self.smooth != smooth {
            self.smooth = smooth;
            self.target_ms = cushion_profile(smooth).start_ms;
            self.learning = false;
            self.starve_frames = 0;
            self.stable_frames = 0;
        }
    }

    /// Records a buffer underrun (dropout) and raises `target_ms` by `step_ms` up to `ceiling_ms`.
    pub fn on_underrun(&mut self) {
        let p = cushion_profile(self.smooth);
        self.underruns += 1;
        self.stable_frames = 0;
        if self.smooth {
            self.learning = true;
            self.starve_base_ms = self.target_ms;
            self.starve_frames = 0;
        } else {
            self.target_ms = (self.target_ms + p.step_ms).min(p.ceiling_ms);
        }
    }

    /// Accumulates starvation frames during a smooth-mode Wi-Fi stall and returns the required `wanted_ms`
    /// to resume playback once packets arrive again.
    pub fn accumulate_stall_and_wanted_ms(&mut self, frames: u32, output_rate: u32) -> f64 {
        if self.learning {
            self.starve_frames += frames as u64;
            let stall_ms = (self.starve_frames as f64) * 1000.0 / (output_rate.max(1) as f64);
            let grown = self.starve_base_ms + SMOOTH_CUSHION.step_ms + stall_ms;
            SMOOTH_CUSHION.ceiling_ms.min(self.target_ms.max(grown))
        } else {
            self.target_ms
        }
    }

    /// Commits the learned cushion when smooth playback resumes after a stall.
    pub fn on_resumed(&mut self, output_rate: u32) {
        if self.learning {
            self.target_ms = self.accumulate_stall_and_wanted_ms(0, output_rate);
            self.learning = false;
            self.starve_frames = 0;
        }
    }

    /// Decays `target_ms` by `1 ms` after every 60 seconds of dropout-free playback down to `floor_ms`.
    pub fn on_stable_block(&mut self, frames: u32, output_rate: u32) {
        let p = cushion_profile(self.smooth);
        self.stable_frames += frames as u64;
        let sixty_seconds = (output_rate as u64) * 60;
        if sixty_seconds > 0 && self.stable_frames >= sixty_seconds {
            self.stable_frames = 0;
            self.target_ms = (self.target_ms - 1.0).max(p.floor_ms);
        }
    }
}

// ============================================================================
// 2. PeerPlayback Ring-Buffer Sinc Resampler, PI Clock Servo & 3ms Clickless Fade
//    (`/native/remote-audio-playback/src/playback_core.cpp` lines 112–285)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PlaybackStats {
    pub buffered_ms: f64,
    pub target_ms: f64,
    pub peak: f64,
    pub rms: f64,
    pub underruns: u64,
    pub discontinuities: u64,
    pub trimmed_frames: u64,
}

impl Default for PlaybackStats {
    fn default() -> Self {
        Self {
            buffered_ms: 0.0,
            target_ms: 30.0,
            peak: 0.0,
            rms: 0.0,
            underruns: 0,
            discontinuities: 0,
            trimmed_frames: 0,
        }
    }
}

/// Complete per-peer ring-buffer resampler, PI clock-drift servo, and 3ms clickless fade-in/out engine.
/// Exact port of `PeerPlayback` in `/native/remote-audio-playback/src/playback_core.cpp`.
#[derive(Debug, Clone)]
pub struct PeerPlaybackEngine {
    pub source_rate: u32,
    pub source_channels: u16,
    pub output_rate: u32,
    pub output_channels: u16,
    pub speaker_mask: u32,
    pub capacity: u32,
    pub ring: Vec<f32>,
    pub sinc_table: Vec<f32>,
    pub matrix: [[f32; MAX_REMOTE_CHANNELS]; MAX_REMOTE_CHANNELS],
    pub write_pos: u64,
    pub read_pos: f64,
    pub next_seq: u32,
    pub has_seq: bool,
    pub primed: bool,
    pub integral: f64,
    pub filtered_error: f64,
    pub fade: f32,
    pub last_sample: [f32; MAX_REMOTE_CHANNELS],
    pub dropped_packets: u64,
    pub trimmed_frames: u64,
    pub last_peak: f64,
    pub last_rms: f64,
    pub cushion: AdaptivePlaybackCushion,
}

impl PeerPlaybackEngine {
    pub fn new(
        source_rate: u32,
        source_channels: u16,
        output_rate: u32,
        output_channels: u16,
        speaker_mask: u32,
        smooth: bool,
    ) -> Option<Self> {
        if !(8_000..=384_000).contains(&source_rate)
            || !(8_000..=384_000).contains(&output_rate)
        {
            return None;
        }
        let matrix =
            build_speaker_folddown_matrix(source_channels, output_channels, speaker_mask)?;
        let capacity = (source_rate * 5) / 2 + 128; // 2.5 seconds + 128-frame Sinc margin
        let ring = vec![0.0f32; (capacity as usize) * (source_channels as usize)];
        let ratio = (output_rate as f64) / (source_rate as f64);
        let sinc_table = build_sinc_kernel(ratio);

        Some(Self {
            source_rate,
            source_channels,
            output_rate,
            output_channels,
            speaker_mask,
            capacity,
            ring,
            sinc_table,
            matrix,
            write_pos: 0,
            read_pos: 0.0,
            next_seq: 0,
            has_seq: false,
            primed: false,
            integral: 0.0,
            filtered_error: 0.0,
            fade: 0.0,
            last_sample: [0.0; MAX_REMOTE_CHANNELS],
            dropped_packets: 0,
            trimmed_frames: 0,
            last_peak: 0.0,
            last_rms: 0.0,
            cushion: AdaptivePlaybackCushion::new(smooth),
        })
    }

    pub fn set_smooth(&mut self, smooth: bool) {
        if self.cushion.smooth != smooth {
            self.cushion.set_smooth(smooth);
            self.primed = false;
        }
    }

    pub fn stats(&self) -> PlaybackStats {
        PlaybackStats {
            buffered_ms: self.latency_ms(),
            target_ms: self.cushion.target_ms,
            peak: self.last_peak,
            rms: self.last_rms,
            underruns: self.cushion.underruns,
            discontinuities: self.dropped_packets,
            trimmed_frames: self.trimmed_frames,
        }
    }

    pub fn buffered_frames(&self) -> f64 {
        (self.write_pos as f64) - self.read_pos
    }

    pub fn latency_ms(&self) -> f64 {
        let held = self.buffered_frames().max(0.0);
        held * 1000.0 / (self.source_rate.max(1) as f64)
    }

    /// Pushes a decoded `LAN1` packet into the peer ring buffer (`PeerPlayback::push`).
    pub fn push(&mut self, packet_seq: u32, frames: u32, samples: &[f32]) -> bool {
        let src_ch = self.source_channels as usize;
        if frames == 0
            || frames > MAX_PACKET_FRAMES
            || samples.len() < (frames as usize) * src_ch
        {
            return false;
        }

        if self.has_seq && packet_seq != self.next_seq {
            let gap = packet_seq.wrapping_sub(self.next_seq);
            if gap < 0x8000_0000 {
                self.dropped_packets += gap as u64;
            }
        }
        self.next_seq = packet_seq.wrapping_add(1);
        self.has_seq = true;

        let w = self.write_pos;
        let held = (w as f64) - self.read_pos;
        let held_limit = if self.cushion.smooth {
            self.capacity as f64
        } else {
            ((self.source_rate / 2) + 128) as f64
        };
        if held + (frames as f64) > held_limit {
            let target =
                ((self.source_rate as f64) * self.cushion.target_ms / 1000.0).max(64.0);
            self.read_pos = ((w + (frames as u64)) as f64) - target;
            self.integral = 0.0;
            self.filtered_error = 0.0;
        }

        let cap = self.capacity as u64;
        for f in 0..(frames as u64) {
            let dst = (((w + f) % cap) as usize) * src_ch;
            let src = (f as usize) * src_ch;
            self.ring[dst..dst + src_ch].copy_from_slice(&samples[src..src + src_ch]);
        }
        self.write_pos = w + (frames as u64);
        true
    }

    /// Mixes `frames` output frames into `output` (`PeerPlayback::mix` in `playback_core.cpp`).
    /// Includes fractional-phase 64-tap Sinc interpolation, PI clock-drift servo (`[-0.001, +0.001]`),
    /// and 3ms clickless fade-in/out on underrun.
    pub fn mix(&mut self, output: &mut [f32], frames: u32) {
        let out_ch = self.output_channels as usize;
        let src_ch = self.source_channels as usize;
        if out_ch == 0
            || src_ch == 0
            || self.capacity == 0
            || output.len() < (frames as usize) * out_ch
        {
            return;
        }

        let w = self.write_pos;
        let fade_frames = ((self.output_rate * 3) / 1000).max(1);
        let fade_step = 1.0f32 / (fade_frames as f32);
        let available = (w as f64) - self.read_pos;
        let wanted_ms = self
            .cushion
            .accumulate_stall_and_wanted_ms(frames, self.output_rate);
        let target = ((self.source_rate as f64) * wanted_ms / 1000.0).max(96.0);

        if !self.primed {
            if available < target {
                for f in 0..(frames as usize) {
                    if self.fade <= 0.0 {
                        break;
                    }
                    self.fade = (self.fade - fade_step).max(0.0);
                    let row = f * out_ch;
                    for c in 0..out_ch {
                        output[row + c] += self.last_sample[c] * self.fade;
                    }
                }
                return;
            }
            self.cushion.on_resumed(self.output_rate);
            self.primed = true;
            self.read_pos = (w as f64) - target;
            self.integral = 0.0;
            self.filtered_error = 0.0;
        }

        let nominal = (self.source_rate as f64) / (self.output_rate as f64);
        if ((w as f64) - self.read_pos) < (frames as f64) * nominal + 36.0 {
            self.primed = false;
            self.cushion.on_underrun();
            for f in 0..(frames as usize) {
                if self.fade <= 0.0 {
                    break;
                }
                self.fade = (self.fade - fade_step).max(0.0);
                let row = f * out_ch;
                for c in 0..out_ch {
                    output[row + c] += self.last_sample[c] * self.fade;
                }
            }
            return;
        }

        let error = (((w as f64) - self.read_pos) - target) / (self.source_rate as f64);
        self.filtered_error += (error - self.filtered_error) * 0.04;
        self.integral = (self.integral + self.filtered_error * 1e-8).clamp(-8e-4, 8e-4);
        let proportional = (self.filtered_error * 0.003).clamp(-2e-6, 2e-6);
        let correction = (proportional + self.integral).clamp(-0.001, 0.001);
        let step = nominal * (1.0 + correction);

        let cap = self.capacity as i64;
        let mut block_peak = 0.0f64;
        let mut block_sq = 0.0f64;
        for f in 0..(frames as usize) {
            let base = self.read_pos.floor() as i64;
            let frac = (self.read_pos - (base as f64)) as f32;
            let scaled = frac * (SINC_PHASES as f32);
            let phase = (scaled as usize).min(SINC_PHASES - 1);
            let blend = scaled - (phase as f32);
            let k0 = phase * SINC_TAPS;
            let k1 = (phase + 1) * SINC_TAPS;

            let mut src_frame = [0.0f32; MAX_REMOTE_CHANNELS];
            for tap in 0..SINC_TAPS {
                let idx = base - 31 + (tap as i64);
                if idx < 0 {
                    continue;
                }
                let coeff =
                    self.sinc_table[k0 + tap] + blend * (self.sinc_table[k1 + tap] - self.sinc_table[k0 + tap]);
                let ring_offset = ((idx.rem_euclid(cap)) as usize) * src_ch;
                for (channel, sample) in src_frame[..src_ch].iter_mut().enumerate() {
                    *sample += self.ring[ring_offset + channel] * coeff;
                }
            }

            if self.fade < 1.0 {
                self.fade = (self.fade + fade_step).min(1.0);
            }
            let row = f * out_ch;
            for out_c in 0..out_ch {
                let mut mixed = 0.0f32;
                for (channel, sample) in src_frame[..src_ch].iter().enumerate() {
                    mixed += *sample * self.matrix[out_c][channel];
                }
                self.last_sample[out_c] = mixed;
                let sample = mixed * self.fade;
                output[row + out_c] += sample;
                let s_abs = (sample as f64).abs();
                if s_abs > block_peak {
                    block_peak = s_abs;
                }
                block_sq += (sample as f64) * (sample as f64);
            }
            self.read_pos += step;
        }

        self.last_peak = block_peak;
        self.last_rms = if frames == 0 || out_ch == 0 {
            0.0
        } else {
            (block_sq / ((frames as usize * out_ch) as f64)).sqrt()
        };
        self.cushion.on_stable_block(frames, self.output_rate);
    }
}

// ============================================================================
// 3. Remote Audio Capture 5ms Packetizer & MirrorControl Command State Machine
//    (`/native/remote-audio-capture/src/capture_queue.cpp` & `mirror_control.cpp`)
// ============================================================================

pub const CAPTURE_QUEUE_SLOTS: usize = 64;
pub const MAX_MIRROR_OUTPUTS: usize = 16;

/// Exact port of `capture_chunk_frames` in `/native/remote-audio-capture/src/capture_queue.cpp`:
/// Computes the 5ms packet frame count `(sample_rate + 199) / 200` for valid rates (`8_000..=384_000` Hz),
/// or returns `0` for out-of-range sample rates.
pub fn capture_chunk_frames(sample_rate: u32) -> usize {
    if !(8_000..=384_000).contains(&sample_rate) {
        return 0;
    }
    (sample_rate as usize).div_ceil(200)
}

#[derive(Debug, Clone, PartialEq)]
pub struct CapturePacket {
    pub seq: u32,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u16,
    pub pcm: Vec<f32>,
}

/// Bounded 64-slot capture packet queue with reader-stall generation invalidation.
/// Exact port of `CaptureQueue` in `/native/remote-audio-capture/src/capture_queue.cpp`.
#[derive(Debug, Clone)]
pub struct CapturePacketQueue {
    slots: Vec<Option<CapturePacket>>,
    read_index: usize,
    write_index: usize,
    generation: u64,
    seq: u32,
    accum_rate: u32,
    accum_channels: u16,
    accum_chunk_frames: usize,
    accum_pcm: Vec<f32>,
}

impl Default for CapturePacketQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl CapturePacketQueue {
    pub fn new() -> Self {
        Self {
            slots: vec![None; CAPTURE_QUEUE_SLOTS],
            read_index: 0,
            write_index: 0,
            generation: 1,
            seq: 0,
            accum_rate: 0,
            accum_channels: 0,
            accum_chunk_frames: 0,
            accum_pcm: Vec::new(),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        for slot in &mut self.slots {
            *slot = None;
        }
        self.read_index = 0;
        self.write_index = 0;
        self.accum_pcm.clear();
    }

    /// Accumulates captured interleaved float32 frames (or silence when `samples` is `None`)
    /// into 5ms `CapturePacket` chunks (`CaptureQueue::push` in `capture_queue.cpp`).
    pub fn push(&mut self, samples: Option<&[f32]>, frames: usize, channels: u16, sample_rate: u32) {
        let ch = channels as usize;
        if !(1..=MAX_REMOTE_CHANNELS).contains(&ch) {
            return;
        }
        let chunk_frames = capture_chunk_frames(sample_rate);
        if chunk_frames == 0 {
            return;
        }
        if self.accum_rate != sample_rate
            || self.accum_channels != channels
            || self.accum_chunk_frames != chunk_frames
        {
            self.accum_rate = sample_rate;
            self.accum_channels = channels;
            self.accum_chunk_frames = chunk_frames;
            self.accum_pcm.clear();
        }

        let mut offset = 0usize;
        while offset < frames {
            let have_frames = self.accum_pcm.len() / ch;
            let take = (chunk_frames - have_frames).min(frames - offset);
            if let Some(src) = samples {
                let start = offset * ch;
                let end = (offset + take) * ch;
                if end <= src.len() {
                    self.accum_pcm.extend_from_slice(&src[start..end]);
                } else {
                    self.accum_pcm.resize(self.accum_pcm.len() + take * ch, 0.0);
                }
            } else {
                self.accum_pcm.resize(self.accum_pcm.len() + take * ch, 0.0);
            }
            offset += take;

            if self.accum_pcm.len() / ch == chunk_frames {
                let next_write = (self.write_index + 1) % CAPTURE_QUEUE_SLOTS;
                if next_write == self.read_index {
                    // Reader stalled: invalidate generation and reset ring
                    self.reset();
                    return;
                }
                let packet = CapturePacket {
                    seq: self.seq,
                    sample_rate,
                    channels,
                    frames: chunk_frames as u16,
                    pcm: std::mem::take(&mut self.accum_pcm),
                };
                self.seq = self.seq.wrapping_add(1);
                self.slots[self.write_index] = Some(packet);
                self.write_index = next_write;
            }
        }
    }

    pub fn pop(&mut self) -> Option<CapturePacket> {
        if self.read_index == self.write_index {
            return None;
        }
        let pkt = self.slots[self.read_index].take();
        self.read_index = (self.read_index + 1) % CAPTURE_QUEUE_SLOTS;
        pkt
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MirrorOutputSlot {
    pub id: String,
    pub endpoint_id: String,
    pub volume: f32,
    pub hold_only: bool,
}

/// Command parser and 16-slot output/hold exclusivity coordinator for `remote-audio-capture` mirror mode.
/// Exact port of `MirrorControl` in `/native/remote-audio-capture/src/mirror_control.cpp`.
#[derive(Debug, Default, Clone)]
pub struct MirrorControlState {
    slots: Vec<MirrorOutputSlot>,
}

impl MirrorControlState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn slots(&self) -> &[MirrorOutputSlot] {
        &self.slots
    }

    /// Processes a single control line (`start`, `hold`, `stop`, `volume`) and returns the stdout ack line
    /// (`ack <req> ok` or `ack <req> error <reason>`).
    pub fn handle_command(&mut self, line: &str) -> Option<String> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        let cmd = *parts.first()?;

        if cmd == "start" {
            let req = *parts.get(1).unwrap_or(&"0");
            let Some(&id) = parts.get(2) else {
                return Some(format!("ack {} error invalid-args", req));
            };
            let Some(&endpoint_id) = parts.get(3) else {
                return Some(format!("ack {} error invalid-args", req));
            };
            let volume = parts
                .get(4)
                .and_then(|v| v.parse::<f32>().ok())
                .unwrap_or(1.0)
                .clamp(0.0, 2.0);

            if let Some(pos) = self.slots.iter().position(|s| s.id == id) {
                if !self.slots[pos].hold_only && self.slots[pos].endpoint_id == endpoint_id {
                    self.slots[pos].volume = volume;
                    return Some(format!("ack {} ok", req));
                }
                self.slots.remove(pos);
            }
            if self.slots.len() >= MAX_MIRROR_OUTPUTS {
                return Some(format!("ack {} error start-failed", req));
            }
            self.slots.push(MirrorOutputSlot {
                id: id.to_string(),
                endpoint_id: endpoint_id.to_string(),
                volume,
                hold_only: false,
            });
            return Some(format!("ack {} ok", req));
        }

        if cmd == "hold" {
            let req = *parts.get(1).unwrap_or(&"0");
            let Some(&id) = parts.get(2) else {
                return Some(format!("ack {} error invalid-args", req));
            };
            let Some(&endpoint_id) = parts.get(3) else {
                return Some(format!("ack {} error invalid-args", req));
            };

            if let Some(existing) = self.slots.iter().find(|s| s.id == id) {
                if existing.endpoint_id == endpoint_id {
                    // Already active or held on same endpoint
                    return Some(format!("ack {} ok", req));
                }
            }
            self.slots.retain(|s| s.id != id);
            if self.slots.len() >= MAX_MIRROR_OUTPUTS {
                return Some(format!("ack {} error hold-failed", req));
            }
            self.slots.push(MirrorOutputSlot {
                id: id.to_string(),
                endpoint_id: endpoint_id.to_string(),
                volume: 0.0,
                hold_only: true,
            });
            return Some(format!("ack {} ok", req));
        }

        if cmd == "stop" {
            let req = *parts.get(1).unwrap_or(&"0");
            if let Some(&id) = parts.get(2) {
                if id == "all" {
                    self.slots.clear();
                } else {
                    self.slots.retain(|s| s.id != id);
                }
            }
            return Some(format!("ack {} ok", req));
        }

        if cmd == "volume" {
            let req = *parts.get(1).unwrap_or(&"0");
            let Some(&id) = parts.get(2) else {
                return Some(format!("ack {} error invalid-args", req));
            };
            let Some(vol) = parts.get(3).and_then(|v| v.parse::<f32>().ok()) else {
                return Some(format!("ack {} error invalid-args", req));
            };
            if let Some(slot) = self.slots.iter_mut().find(|s| s.id == id) {
                slot.volume = vol.clamp(0.0, 2.0);
            }
            return Some(format!("ack {} ok", req));
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sinc_kernel_rows_normalize_to_unity_and_folddown_maps_5_1_to_stereo() {
        let table = build_sinc_kernel(1.0);
        assert_eq!(table.len(), (SINC_PHASES + 1) * SINC_TAPS);
        for phase in [0usize, 512, SINC_PHASES] {
            let row = &table[phase * SINC_TAPS..(phase + 1) * SINC_TAPS];
            let sum: f32 = row.iter().sum();
            assert!((sum - 1.0).abs() < 1e-5);
        }

        // 5.1 (6 channels) -> Stereo (2 channels)
        let matrix = build_speaker_folddown_matrix(6, 2, 0x3).expect("valid 5.1 -> 2.0 matrix");
        assert_eq!(matrix[0][0], 1.0); // FL -> L
        assert_eq!(matrix[1][1], 1.0); // FR -> R
        assert!((matrix[0][2] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6); // FC -> L (-3 dB)
        assert_eq!(matrix[0][3], 0.25); // LFE -> L
        assert_eq!(matrix[0][4], 0.5); // BL -> L
        assert_eq!(matrix[1][5], 0.5); // BR -> R
    }

    #[test]
    fn matching_or_wider_outputs_preserve_each_source_channel() {
        for source_channels in 2..=MAX_REMOTE_CHANNELS as u16 {
            for output_channels in source_channels..=MAX_REMOTE_CHANNELS as u16 {
                let matrix = build_speaker_folddown_matrix(source_channels, output_channels, 0)
                    .expect("valid channel layout");
                for (output, row) in matrix.iter().enumerate() {
                    for (source, gain) in row.iter().enumerate() {
                        let expected = if output == source && source < source_channels as usize {
                            1.0
                        } else {
                            0.0
                        };
                        assert_eq!(*gain, expected);
                    }
                }
            }
        }
        assert!(build_speaker_folddown_matrix(0, 2, 0).is_none());
        assert!(build_speaker_folddown_matrix(2, 0, 0).is_none());
        assert!(build_speaker_folddown_matrix(9, 2, 0).is_none());
        assert!(build_speaker_folddown_matrix(2, 9, 0).is_none());
    }

    #[test]
    fn adaptive_cushion_grows_on_wifi_stall_and_decays_after_60s() {
        let mut cushion = AdaptivePlaybackCushion::new(true);
        assert_eq!(cushion.target_ms, 300.0);
        cushion.on_underrun();
        // Simulate 250 ms Wi-Fi stall at 48 kHz (12,000 frames)
        let wanted = cushion.accumulate_stall_and_wanted_ms(12_000, 48_000);
        // 300 + 150 step + 250 stall = 700 ms
        assert!((wanted - 700.0).abs() < 1e-6);
        cushion.on_resumed(48_000);
        assert!((cushion.target_ms - 700.0).abs() < 1e-6);

        // 60 seconds of stable playback decays target_ms by 1 ms
        cushion.on_stable_block(48_000 * 60, 48_000);
        assert!((cushion.target_ms - 699.0).abs() < 1e-6);
    }

    #[test]
    fn peer_playback_engine_resamples_and_capture_queue_chunks_5ms_packets() {
        assert_eq!(capture_chunk_frames(48_000), 240);
        assert_eq!(capture_chunk_frames(44_100), 221);
        assert_eq!(capture_chunk_frames(4_000), 0);

        let mut queue = CapturePacketQueue::new();
        let stereo_block = vec![0.25f32; 480 * 2]; // 10ms at 48kHz = two 240-frame packets
        queue.push(Some(&stereo_block), 480, 2, 48_000);
        let pkt0 = queue.pop().expect("first 5ms packet");
        let pkt1 = queue.pop().expect("second 5ms packet");
        assert_eq!(pkt0.seq, 0);
        assert_eq!(pkt0.frames, 240);
        assert_eq!(pkt1.seq, 1);
        assert!(queue.pop().is_none());

        let mut peer =
            PeerPlaybackEngine::new(48_000, 2, 48_000, 2, 0x3, false).expect("valid peer");
        // Push enough 5ms packets to prime the 30ms ordinary cushion (> 1440 frames)
        for seq in 0..10u32 {
            assert!(peer.push(seq, 240, &pkt0.pcm));
        }
        let mut out = vec![0.0f32; 240 * 2];
        peer.mix(&mut out, 240);
        assert!(peer.primed);
        assert!(out.iter().any(|&v| v > 0.05));

        let mut mirror = MirrorControlState::new();
        assert_eq!(
            mirror.handle_command("hold 1 slotA {11111111-2222-3333-4444-555555555555}"),
            Some("ack 1 ok".to_string())
        );
        assert!(mirror.slots()[0].hold_only);
        assert_eq!(
            mirror.handle_command("start 2 slotA {11111111-2222-3333-4444-555555555555} 0.85"),
            Some("ack 2 ok".to_string())
        );
        assert!(!mirror.slots()[0].hold_only);
        assert!((mirror.slots()[0].volume - 0.85).abs() < 1e-6);
    }
}
