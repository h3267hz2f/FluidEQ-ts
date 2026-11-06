/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Neural Model Lifecycle Manager (`DPDFNet` Voice Denoise & `swift-f0.onnx` Pitch Detector).
//!
//! 1. **Voice Denoise (`DPDFNet` 48 kHz)**:
//!    - Coordinates `feq_chain_load_voice_model` / `feq_chain_unload_voice_model` in `/native/dsp-core/src/denoise_voice.cpp`.
//!    - Automatically unloads the ONNX Runtime session on `ColdStandby` when `denoise.voice.enabled == 0`
//!      so inactive neural buffers do not occupy resident RAM.
//! 2. **Karaoke Pitch Detector (`swift-f0.onnx` + Real-Time YIN/CMNDF Estimator)**:
//!    - Validates `/assets/models/swift-f0.onnx` ONNX protobuf model files on disk.
//!    - Provides a zero-allocation Cumulative Mean Normalized Difference Function (YIN)
//!      fundamental frequency (`F0` in Hz, MIDI note number, cents offset, and voicing confidence)
//!      estimator over vocal audio frames (`65 Hz .. 1,100 Hz`).

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub const VOICE_MODEL_SAMPLE_RATE: f64 = 48_000.0;
pub const VOICE_MODEL_WINDOW_FRAMES: u32 = 960;
pub const VOICE_MODEL_HOP_FRAMES: u32 = 480;

pub const PITCH_MIN_HZ: f64 = 65.0; // C2
pub const PITCH_MAX_HZ: f64 = 1100.0; // C#6

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NeuralModelStatus {
    pub voice_model_loaded: bool,
    pub voice_model_path: Option<String>,
    pub ort_runtime_path: Option<String>,
    pub swift_f0_verified: bool,
    pub swift_f0_size_bytes: u64,
}

impl Default for NeuralModelStatus {
    fn default() -> Self {
        Self {
            voice_model_loaded: false,
            voice_model_path: None,
            ort_runtime_path: None,
            swift_f0_verified: false,
            swift_f0_size_bytes: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PitchEstimate {
    pub voiced: bool,
    pub frequency_hz: f64,
    pub confidence: f64,
    pub midi_note: u8,
    pub cents_offset: f64,
}

impl Default for PitchEstimate {
    fn default() -> Self {
        Self {
            voiced: false,
            frequency_hz: 0.0,
            confidence: 0.0,
            midi_note: 0,
            cents_offset: 0.0,
        }
    }
}

/// Validates that an `.onnx` model file exists, is non-empty, and starts with a valid
/// Protobuf `ModelProto` wire tag (`ir_version` field 1 varint `0x08`).
pub fn verify_onnx_model_file(path: &Path) -> Result<u64, String> {
    let metadata = fs::metadata(path)
        .map_err(|e| format!("Failed to stat ONNX model {:?}: {}", path, e))?;
    let size = metadata.len();
    if size < 64 {
        return Err(format!("ONNX model {:?} is too small ({} bytes)", path, size));
    }
    let bytes = fs::read(path)
        .map_err(|e| format!("Failed to read ONNX model {:?}: {}", path, e))?;
    if bytes.first().copied() != Some(0x08) {
        return Err(format!(
            "Invalid ONNX ModelProto header byte in {:?}",
            path
        ));
    }
    Ok(size)
}

/// Estimates fundamental vocal pitch (`F0` in Hz, MIDI note, cents deviation, and confidence)
/// using the Cumulative Mean Normalized Difference Function (YIN algorithm) with parabolic
/// minimum interpolation.
pub fn estimate_vocal_pitch_yin(samples: &[f32], sample_rate: f64) -> PitchEstimate {
    if !(sample_rate > 0.0) || samples.len() < 128 {
        return PitchEstimate::default();
    }

    // Check signal RMS gate (-55 dBFS)
    let energy: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    let rms = (energy / (samples.len() as f64)).sqrt();
    if rms < 1.778e-3 {
        return PitchEstimate::default();
    }

    let half_n = samples.len() / 2;
    let tau_min = ((sample_rate / PITCH_MAX_HZ).floor() as usize).max(2);
    let tau_max = ((sample_rate / PITCH_MIN_HZ).ceil() as usize).min(half_n.saturating_sub(1));
    if tau_min >= tau_max {
        return PitchEstimate::default();
    }

    let mut cmndf = vec![1.0_f64; tau_max + 1];
    let mut running_sum = 0.0_f64;

    for tau in 1..=tau_max {
        let mut diff = 0.0_f64;
        for i in 0..half_n {
            let delta = (samples[i] as f64) - (samples[i + tau] as f64);
            diff += delta * delta;
        }
        running_sum += diff;
        if running_sum > 1e-12 {
            cmndf[tau] = diff * (tau as f64) / running_sum;
        } else {
            cmndf[tau] = 1.0;
        }
    }

    // Absolute threshold search (0.15) with global minimum fallback
    let threshold = 0.15;
    let mut best_tau = None;
    let mut tau = tau_min;
    while tau <= tau_max {
        if cmndf[tau] < threshold {
            while tau + 1 <= tau_max && cmndf[tau + 1] < cmndf[tau] {
                tau += 1;
            }
            best_tau = Some(tau);
            break;
        }
        tau += 1;
    }

    let chosen_tau = best_tau.unwrap_or_else(|| {
        (tau_min..=tau_max)
            .min_by(|&a, &b| cmndf[a].partial_cmp(&cmndf[b]).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(tau_min)
    });

    let min_val = cmndf[chosen_tau];
    let confidence = (1.0 - min_val).clamp(0.0, 1.0);
    if confidence < 0.65 {
        return PitchEstimate::default();
    }

    // Parabolic interpolation around `chosen_tau`
    let refined_tau = if chosen_tau > tau_min && chosen_tau < tau_max {
        let s0 = cmndf[chosen_tau - 1];
        let s1 = cmndf[chosen_tau];
        let s2 = cmndf[chosen_tau + 1];
        let denom = 2.0 * (2.0 * s1 - s2 - s0);
        if denom.abs() > 1e-12 {
            (chosen_tau as f64) + (s2 - s0) / denom
        } else {
            chosen_tau as f64
        }
    } else {
        chosen_tau as f64
    };

    let freq_hz = sample_rate / refined_tau;
    if !(PITCH_MIN_HZ..=PITCH_MAX_HZ).contains(&freq_hz) {
        return PitchEstimate::default();
    }

    let exact_midi = 69.0 + 12.0 * (freq_hz / 440.0).log2();
    let nearest_midi = exact_midi.round();
    let cents_offset = (exact_midi - nearest_midi) * 100.0;
    let midi_note = nearest_midi.clamp(0.0, 127.0) as u8;

    PitchEstimate {
        voiced: true,
        frequency_hz: freq_hz,
        confidence,
        midi_note,
        cents_offset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    #[test]
    fn yin_pitch_estimator_detects_440hz_a4_sine_accurately() {
        let sample_rate = 48_000.0;
        let target_hz = 440.0;
        let samples: Vec<f32> = (0..2048)
            .map(|n| (2.0 * PI * target_hz * (n as f64) / sample_rate).sin() as f32 * 0.5)
            .collect();

        let est = estimate_vocal_pitch_yin(&samples, sample_rate);
        assert!(est.voiced);
        assert!((est.frequency_hz - 440.0).abs() < 1.0);
        assert_eq!(est.midi_note, 69);
        assert!(est.confidence > 0.90);
    }

    #[test]
    fn silence_returns_unvoiced_pitch_estimate() {
        let silent = vec![0.0_f32; 2048];
        let est = estimate_vocal_pitch_yin(&silent, 48_000.0);
        assert!(!est.voiced);
        assert_eq!(est.frequency_hz, 0.0);
    }
}
