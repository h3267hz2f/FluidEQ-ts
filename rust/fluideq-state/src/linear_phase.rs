/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Linear-Phase FIR Kernel Synthesizer (`16,384` taps, `8,192` samples causal centre).
//!
//! Mirrors `/native/dsp-core/src/linear_phase.cpp`:
//! - When compiled with `--features fluideq-dsp-sys/link-cpp-core`, delegates directly
//!   to `feq_build_linear_phase_kernel`.
//! - Otherwise runs the identical algorithm in pure Rust:
//!   1. Pushes a unit impulse through all static enabled bands (Serial cascade or Parallel sum)
//!      plus the optional subsonic high-pass filter.
//!   2. Computes the 16,384-point Radix-2 FFT, discards phase (`hypot(re, im)`),
//!      runs the inverse FFT, and rotates by `FEQ_LINEAR_PHASE_KERNEL_LATENCY` (`8,192`)
//!      so the symmetric impulse peak sits at the causal midpoint.

use crate::biquad::{biquad_coefficients_designed, biquad_coefficients_rbj};
use fluideq_dsp_sys::{
    FeqBiquadCoefficients, FeqChainEqSettings, FeqEqEngine, FeqEqModel, FeqFilterType,
    FEQ_LINEAR_PHASE_KERNEL_LATENCY, FEQ_LINEAR_PHASE_KERNEL_SIZE,
};
use std::f64::consts::PI;

/// In-place Radix-2 Cooley-Tukey FFT over power-of-two `real` and `imag` slices.
/// Matches `feq_fft_in_place` in `/native/dsp-core/src/convolver.cpp` (unnormalized inverse).
pub fn fft_radix2_in_place(real: &mut [f64], imag: &mut [f64], inverse: bool) {
    let n = real.len();
    if n == 0 || !n.is_power_of_two() || imag.len() != n {
        return;
    }

    // Bit-reversal permutation
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while (j & bit) != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            real.swap(i, j);
            imag.swap(i, j);
        }
    }

    // Butterfly stages
    let mut len = 2usize;
    let sign = if inverse { 1.0 } else { -1.0 };
    while len <= n {
        let half = len >> 1;
        let angle = sign * 2.0 * PI / (len as f64);
        let wlen_re = angle.cos();
        let wlen_im = angle.sin();

        let mut start = 0usize;
        while start < n {
            let mut w_re = 1.0;
            let mut w_im = 0.0;
            for k in 0..half {
                let u_idx = start + k;
                let v_idx = u_idx + half;
                let u_re = real[u_idx];
                let u_im = imag[u_idx];
                let v_re = real[v_idx] * w_re - imag[v_idx] * w_im;
                let v_im = real[v_idx] * w_im + imag[v_idx] * w_re;

                real[u_idx] = u_re + v_re;
                imag[u_idx] = u_im + v_im;
                real[v_idx] = u_re - v_re;
                imag[v_idx] = u_im - v_im;

                let next_w_re = w_re * wlen_re - w_im * wlen_im;
                w_im = w_re * wlen_im + w_im * wlen_re;
                w_re = next_w_re;
            }
            start += len;
        }
        len <<= 1;
    }
}

#[inline]
fn process_biquad_slice(buffer: &mut [f32], coeffs: &FeqBiquadCoefficients) {
    let mut x1 = 0.0_f64;
    let mut x2 = 0.0_f64;
    let mut y1 = 0.0_f64;
    let mut y2 = 0.0_f64;
    for sample in buffer.iter_mut() {
        let x0 = *sample as f64;
        let y0 = coeffs.b0 * x0 + coeffs.b1 * x1 + coeffs.b2 * x2 - coeffs.a1 * y1 - coeffs.a2 * y2;
        x2 = x1;
        x1 = x0;
        y2 = y1;
        y1 = y0;
        *sample = y0 as f32;
    }
}

/// Synthesizes a `16,384`-sample linear-phase FIR kernel for one ear channel (`ear_channel`: `1` = Left, `2` = Right)
/// from `FeqChainEqSettings`. Bands with `ear == 0` (both) or `ear == ear_channel` are included.
pub fn synthesize_linear_phase_kernel_for_ear(
    eq: &FeqChainEqSettings,
    sample_rate: f64,
    ear_channel: i32,
) -> Vec<f32> {
    let size = FEQ_LINEAR_PHASE_KERNEL_SIZE;
    let mut dry = vec![0.0_f32; size];
    dry[0] = 1.0;
    let mut buffer = dry.clone();
    let mut wet = vec![0.0_f32; size];

    let model = match eq.model {
        1 => FeqEqModel::FEQ_EQ_MODEL_PROPORTIONAL,
        2 => FeqEqModel::FEQ_EQ_MODEL_WIDE,
        3 => FeqEqModel::FEQ_EQ_MODEL_ASYMMETRIC,
        _ => FeqEqModel::FEQ_EQ_MODEL_CLEAN,
    };
    let is_parallel = eq.engine == FeqEqEngine::FEQ_EQ_PARALLEL as i32;
    let matched = eq.matched != 0;
    let band_count = (eq.band_count as usize).min(eq.bands.len());

    for idx in 0..band_count {
        let band = &eq.bands[idx];
        // Dynamic bands are excluded from the fixed FIR kernel and run after it
        if band.enabled == 0 || band.dynamic != 0 {
            continue;
        }
        if band.ear != 0 && band.ear != ear_channel {
            continue;
        }

        let coeffs = biquad_coefficients_designed(
            band.filter_type,
            band.frequency,
            band.gain_db,
            band.quality,
            sample_rate,
            model,
            eq.model_amount,
            matched,
        );

        if !is_parallel {
            process_biquad_slice(&mut buffer, &coeffs);
        } else {
            wet.copy_from_slice(&dry);
            process_biquad_slice(&mut wet, &coeffs);
            for at in 0..size {
                buffer[at] =
                    ((buffer[at] as f64) + ((wet[at] as f64) - (dry[at] as f64))) as f32;
            }
        }
    }

    if eq.subsonic_hz > 0.0 {
        let subsonic = biquad_coefficients_rbj(
            FeqFilterType::FEQ_FILTER_HPQ,
            eq.subsonic_hz,
            0.0,
            0.707,
            sample_rate,
        );
        process_biquad_slice(&mut buffer, &subsonic);
    }

    // Transform impulse response -> discard phase -> inverse transform -> rotate by half
    let mut real: Vec<f64> = buffer.iter().map(|&x| x as f64).collect();
    let mut imag = vec![0.0_f64; size];
    fft_radix2_in_place(&mut real, &mut imag, false);
    for bin in 0..size {
        real[bin] = real[bin].hypot(imag[bin]);
        imag[bin] = 0.0;
    }
    fft_radix2_in_place(&mut real, &mut imag, true);

    let half = FEQ_LINEAR_PHASE_KERNEL_LATENCY;
    let scale = 1.0 / (size as f64);
    let mut kernel = vec![0.0_f32; size];
    for at in 0..size {
        kernel[at] = (real[(at + half) % size] * scale) as f32;
    }
    kernel
}

/// Synthesizes a stereo pair `(left_kernel, right_kernel)` of `16,384`-sample linear-phase FIR kernels.
pub fn synthesize_stereo_linear_phase_kernels(
    eq: &FeqChainEqSettings,
    sample_rate: f64,
) -> (Vec<f32>, Vec<f32>) {
    let left = synthesize_linear_phase_kernel_for_ear(eq, sample_rate, 1);
    let has_per_ear_bands = (0..(eq.band_count as usize).min(eq.bands.len()))
        .any(|i| eq.bands[i].enabled != 0 && eq.bands[i].ear != 0);
    let right = if has_per_ear_bands {
        synthesize_linear_phase_kernel_for_ear(eq, sample_rate, 2)
    } else {
        left.clone()
    };
    (left, right)
}

/// Synthesizes a `16,384`-sample sparse differential linear-phase FIR kernel for a single
/// dynamic EQ band (`band_index`), matching `feq_build_linear_phase_change_kernel` in
/// `/native/dsp-core/src/linear_phase.cpp`.
///
/// The differential kernel represents `|H_rack(w)| * (|H_band(w)| - 1.0)` so that the convolver
/// can scale and sum `change_kernel * dynamic_scale` onto the static linear-phase kernel in real time.
pub fn synthesize_linear_phase_change_kernel(
    eq: &FeqChainEqSettings,
    band_index: usize,
    sample_rate: f64,
    ear_slot: i32,
) -> Vec<f32> {
    let size = FEQ_LINEAR_PHASE_KERNEL_SIZE;
    let count = (eq.band_count as usize).min(eq.bands.len());
    if band_index >= count || !(sample_rate > 0.0) {
        return vec![0.0_f32; size];
    }
    let chosen = &eq.bands[band_index];
    if chosen.enabled == 0 || (chosen.ear != 0 && chosen.ear != ear_slot) {
        return vec![0.0_f32; size];
    }

    let model = match eq.model {
        1 => FeqEqModel::FEQ_EQ_MODEL_PROPORTIONAL,
        2 => FeqEqModel::FEQ_EQ_MODEL_WIDE,
        3 => FeqEqModel::FEQ_EQ_MODEL_ASYMMETRIC,
        _ => FeqEqModel::FEQ_EQ_MODEL_CLEAN,
    };
    let matched = eq.matched != 0;

    // 1. Impulse response of the chosen dynamic band alone
    let chosen_coeffs = biquad_coefficients_designed(
        chosen.filter_type,
        chosen.frequency,
        chosen.gain_db,
        chosen.quality,
        sample_rate,
        model,
        eq.model_amount,
        matched,
    );
    let mut alone = vec![0.0_f32; size];
    alone[0] = 1.0;
    process_biquad_slice(&mut alone, &chosen_coeffs);

    let mut own_real: Vec<f64> = alone.iter().map(|&x| x as f64).collect();
    let mut own_imag = vec![0.0_f64; size];
    fft_radix2_in_place(&mut own_real, &mut own_imag, false);

    // 2. Magnitude spectrum of the static rack
    let rack_kernel = synthesize_linear_phase_kernel_for_ear(eq, sample_rate, ear_slot);
    let mut change_real: Vec<f64> = rack_kernel.iter().map(|&x| x as f64).collect();
    let mut change_imag = vec![0.0_f64; size];
    fft_radix2_in_place(&mut change_real, &mut change_imag, false);

    for bin in 0..size {
        let rack_mag = change_real[bin].hypot(change_imag[bin]);
        let own_mag = own_real[bin].hypot(own_imag[bin]);
        change_real[bin] = rack_mag * (own_mag - 1.0);
        change_imag[bin] = 0.0;
    }

    fft_radix2_in_place(&mut change_real, &mut change_imag, true);
    let half = FEQ_LINEAR_PHASE_KERNEL_LATENCY;
    let scale = 1.0 / (size as f64);
    let mut out = vec![0.0_f32; size];
    for at in 0..size {
        out[at] = (change_real[(at + half) % size] * scale) as f32;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_eq_produces_centered_unit_impulse_kernel() {
        let eq = FeqChainEqSettings::default();
        let kernel = synthesize_linear_phase_kernel_for_ear(&eq, 48_000.0, 1);
        assert_eq!(kernel.len(), FEQ_LINEAR_PHASE_KERNEL_SIZE);
        let peak = kernel[FEQ_LINEAR_PHASE_KERNEL_LATENCY];
        assert!(
            (peak - 1.0).abs() < 1e-5,
            "Expected unit peak at causal midpoint, got {}",
            peak
        );
    }

    #[test]
    fn boosted_band_produces_symmetric_linear_phase_kernel() {
        let mut eq = FeqChainEqSettings::default();
        eq.band_count = 1;
        eq.bands[0].enabled = 1;
        eq.bands[0].filter_type = FeqFilterType::FEQ_FILTER_PK;
        eq.bands[0].frequency = 1000.0;
        eq.bands[0].gain_db = 6.0;
        eq.bands[0].quality = 1.41;

        let kernel = synthesize_linear_phase_kernel_for_ear(&eq, 48_000.0, 1);
        let mid = FEQ_LINEAR_PHASE_KERNEL_LATENCY;
        // Verify causal symmetry around `mid`
        for offset in 1..64 {
            let left_tap = kernel[mid - offset];
            let right_tap = kernel[mid + offset];
            assert!(
                (left_tap - right_tap).abs() < 1e-5,
                "Kernel not symmetric at offset {}: {} vs {}",
                offset,
                left_tap,
                right_tap
            );
        }
    }

    #[test]
    fn dynamic_band_change_kernel_is_zero_at_zero_gain_and_symmetric_when_boosted() {
        let mut eq = FeqChainEqSettings::default();
        eq.band_count = 1;
        eq.bands[0].enabled = 1;
        eq.bands[0].dynamic = 1;
        eq.bands[0].filter_type = FeqFilterType::FEQ_FILTER_PK;
        eq.bands[0].frequency = 1000.0;
        eq.bands[0].gain_db = 0.0;
        eq.bands[0].quality = 1.41;

        let zero_change = synthesize_linear_phase_change_kernel(&eq, 0, 48_000.0, 1);
        assert!(zero_change.iter().all(|v| v.abs() < 1e-5));

        eq.bands[0].gain_db = 6.0;
        let active_change = synthesize_linear_phase_change_kernel(&eq, 0, 48_000.0, 1);
        let mid = FEQ_LINEAR_PHASE_KERNEL_LATENCY;
        assert!(active_change[mid].abs() > 1e-3);
        assert!((active_change[mid - 5] - active_change[mid + 5]).abs() < 1e-5);
    }
}
