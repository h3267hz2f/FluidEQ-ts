/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Biquad filter synthesis (RBJ Audio EQ Cookbook + Martin Vicanek analog-matched),
//! character model Q shaping, and worst-case auto-headroom calculation in Rust.

// The Rust matched-biquad solver is the no-C++ fallback. When the native core
// is linked, its C++ counterpart is authoritative and these helpers are unused.
#![cfg_attr(feature = "link-cpp-core", allow(dead_code))]

use fluideq_dsp_sys::{FeqBiquadCoefficients, FeqEqModel, FeqFilterType};
use serde::{Deserialize, Serialize};
use std::f64::consts::{LN_2, PI, SQRT_2};

const MIN_FREQUENCY_HZ: f64 = 10.0;
const NYQUIST_MARGIN: f64 = 0.499;
const MIN_QUALITY: f64 = 0.1;
const MAX_QUALITY: f64 = 18.0;
const GAIN_EPSILON_DB: f64 = 1e-6;
const SHELF_BUTTERWORTH_TOLERANCE: f64 = 0.05;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct EqBandSpec {
    pub id: u32,
    pub enabled: bool,
    pub filter_type: BandFilterKind,
    pub frequency: f64,
    pub gain_db: f64,
    pub quality: f64,
    pub dynamic: bool,
    pub threshold_db: f64,
    /// 0 = Both, 1 = Left, 2 = Right
    pub ear: u8,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BandFilterKind {
    PK,
    NO,
    LSC,
    HSC,
    LPQ,
    HPQ,
    BP,
}

impl BandFilterKind {
    pub fn to_feq_type(self) -> FeqFilterType {
        match self {
            Self::PK => FeqFilterType::FEQ_FILTER_PK,
            Self::NO => FeqFilterType::FEQ_FILTER_NO,
            Self::LSC => FeqFilterType::FEQ_FILTER_LSC,
            Self::HSC => FeqFilterType::FEQ_FILTER_HSC,
            Self::LPQ => FeqFilterType::FEQ_FILTER_LPQ,
            Self::HPQ => FeqFilterType::FEQ_FILTER_HPQ,
            Self::BP => FeqFilterType::FEQ_FILTER_BP,
        }
    }
}

pub fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

/// Exact port of `modelled_quality` in `/native/dsp-core/src/biquad.cpp` (lines 31–79).
pub fn modelled_quality(
    filter_type: FeqFilterType,
    gain_db: f64,
    quality: f64,
    model: FeqEqModel,
    amount: f64,
) -> f64 {
    let q = clamp(
        if quality.is_finite() { quality } else { 1.0 / SQRT_2 },
        MIN_QUALITY,
        MAX_QUALITY,
    );
    let no_gain = matches!(
        filter_type,
        FeqFilterType::FEQ_FILTER_BP
            | FeqFilterType::FEQ_FILTER_LPQ
            | FeqFilterType::FEQ_FILTER_HPQ
            | FeqFilterType::FEQ_FILTER_NO
    );
    if model == FeqEqModel::FEQ_EQ_MODEL_CLEAN
        || !amount.is_finite()
        || amount <= 0.0
        || !gain_db.is_finite()
        || gain_db == 0.0
        || no_gain
    {
        return q;
    }

    let amount = clamp(amount, 0.0, 1.0);
    let driven = gain_db.abs().min(20.0);
    // √(1 + |G|/12) gives ~1.35× Q at ±10 dB and ~1.5× at ±15 dB.
    let narrowing = (1.0 + driven / 12.0).sqrt();
    let factor = 1.0 + amount * (narrowing - 1.0);

    match model {
        FeqEqModel::FEQ_EQ_MODEL_PROPORTIONAL => (q * factor).min(MAX_QUALITY),
        FeqEqModel::FEQ_EQ_MODEL_ASYMMETRIC => {
            // Boosts broaden slightly; cuts narrow for surgical notch-like removal.
            let shaped = if gain_db > 0.0 {
                q / factor
            } else {
                q * factor
            };
            clamp(shaped, 0.25, MAX_QUALITY)
        }
        FeqEqModel::FEQ_EQ_MODEL_WIDE => {
            // Shelves soften more than bells so low/high-end tilts feel like a broad tone control.
            let is_shelf = matches!(
                filter_type,
                FeqFilterType::FEQ_FILTER_LSC | FeqFilterType::FEQ_FILTER_HSC
            );
            let full = if is_shelf { 0.4 } else { 0.45 };
            (q * (1.0 - amount * (1.0 - full))).max(0.25)
        }
        FeqEqModel::FEQ_EQ_MODEL_CLEAN => q,
    }
}

/// Exact port of `feq_biquad_coefficients` in `/native/dsp-core/src/biquad.cpp` (lines 85–186).
pub fn biquad_coefficients_cookbook(
    filter_type: FeqFilterType,
    frequency: f64,
    gain_db: f64,
    quality: f64,
    sample_rate: f64,
) -> FeqBiquadCoefficients {
    let mut result = FeqBiquadCoefficients::default();
    if !sample_rate.is_finite()
        || sample_rate <= 0.0
        || !frequency.is_finite()
        || frequency <= 0.0
        || frequency >= sample_rate * 0.5
        || !quality.is_finite()
        || quality <= 0.0
    {
        return result;
    }

    let db = if gain_db.is_finite() { gain_db } else { 0.0 };
    let omega = 2.0 * PI * frequency / sample_rate;
    let sine = omega.sin();
    let cosine = omega.cos();
    let alpha = sine / (2.0 * quality);
    let amplitude = 10.0_f64.powf(db / 40.0);

    let (b0, b1, b2, a0, a1, a2) = match filter_type {
        FeqFilterType::FEQ_FILTER_PK => (
            1.0 + alpha * amplitude,
            -2.0 * cosine,
            1.0 - alpha * amplitude,
            1.0 + alpha / amplitude,
            -2.0 * cosine,
            1.0 - alpha / amplitude,
        ),
        FeqFilterType::FEQ_FILTER_LSC => {
            let root = 2.0 * amplitude.sqrt() * alpha;
            (
                amplitude * ((amplitude + 1.0) - (amplitude - 1.0) * cosine + root),
                2.0 * amplitude * ((amplitude - 1.0) - (amplitude + 1.0) * cosine),
                amplitude * ((amplitude + 1.0) - (amplitude - 1.0) * cosine - root),
                (amplitude + 1.0) + (amplitude - 1.0) * cosine + root,
                -2.0 * ((amplitude - 1.0) + (amplitude + 1.0) * cosine),
                (amplitude + 1.0) + (amplitude - 1.0) * cosine - root,
            )
        }
        FeqFilterType::FEQ_FILTER_HSC => {
            let root = 2.0 * amplitude.sqrt() * alpha;
            (
                amplitude * ((amplitude + 1.0) + (amplitude - 1.0) * cosine + root),
                -2.0 * amplitude * ((amplitude - 1.0) + (amplitude + 1.0) * cosine),
                amplitude * ((amplitude + 1.0) + (amplitude - 1.0) * cosine - root),
                (amplitude + 1.0) - (amplitude - 1.0) * cosine + root,
                2.0 * ((amplitude - 1.0) - (amplitude + 1.0) * cosine),
                (amplitude + 1.0) - (amplitude - 1.0) * cosine - root,
            )
        }
        FeqFilterType::FEQ_FILTER_LPQ => (
            (1.0 - cosine) * 0.5,
            1.0 - cosine,
            (1.0 - cosine) * 0.5,
            1.0 + alpha,
            -2.0 * cosine,
            1.0 - alpha,
        ),
        FeqFilterType::FEQ_FILTER_HPQ => (
            (1.0 + cosine) * 0.5,
            -(1.0 + cosine),
            (1.0 + cosine) * 0.5,
            1.0 + alpha,
            -2.0 * cosine,
            1.0 - alpha,
        ),
        FeqFilterType::FEQ_FILTER_BP => (
            alpha,
            0.0,
            -alpha,
            1.0 + alpha,
            -2.0 * cosine,
            1.0 - alpha,
        ),
        FeqFilterType::FEQ_FILTER_NO => (
            1.0,
            -2.0 * cosine,
            1.0,
            1.0 + alpha,
            -2.0 * cosine,
            1.0 - alpha,
        ),
    };

    if !a0.is_finite() || a0 == 0.0 {
        return result;
    }
    let _ = (LN_2, GAIN_EPSILON_DB, MIN_FREQUENCY_HZ, NYQUIST_MARGIN);
    result.b0 = b0 / a0;
    result.b1 = b1 / a0;
    result.b2 = b2 / a0;
    result.a1 = a1 / a0;
    result.a2 = a2 / a0;
    result
}

pub use biquad_coefficients_cookbook as biquad_coefficients_rbj;

// ============================================================================
// Martin Vicanek Analog-Matched Biquad Solver (`biquad_matched.cpp`)
// ============================================================================

const BUTTERWORTH_Q: f64 = 0.707_106_781_186_547_5;
const BUTTERWORTH_TOLERANCE: f64 = 0.02;

#[derive(Debug, Clone, Copy)]
struct Phis {
    p0: f64,
    p1: f64,
    p2: f64,
}

impl Phis {
    fn at(omega: f64) -> Self {
        let s = (0.5 * omega).sin();
        let p1 = s * s;
        Self {
            p0: 1.0 - p1,
            p1,
            p2: 4.0 * p1 * (1.0 - p1),
        }
    }

    fn dot(self, w0: f64, w1: f64, w2: f64) -> f64 {
        w0 * self.p0 + w1 * self.p1 + w2 * self.p2
    }
}

/// Impulse-invariant denominator from the continuous-time pole pair `s^2 + (w0/q)s + w0^2`.
fn matched_poles(omega: f64, q: f64) -> (f64, f64) {
    let q = q.max(1e-4);
    let alpha = omega / (2.0 * q);
    if alpha >= 100.0 {
        return (0.0, 0.0);
    }
    let decay = (-alpha).exp();
    let a1 = if q >= 0.5 {
        let discriminant = (1.0 - 1.0 / (4.0 * q * q)).max(0.0);
        -2.0 * decay * (omega * discriminant.sqrt()).cos()
    } else {
        let discriminant = (1.0 / (4.0 * q * q) - 1.0).max(0.0);
        -2.0 * decay * (omega * discriminant.sqrt()).cosh()
    };
    let a2 = decay * decay;
    (a1, a2)
}

fn denominator_weights(a1: f64, a2: f64) -> (f64, f64, f64) {
    let a0 = (1.0 + a1 + a2) * (1.0 + a1 + a2);
    let a1_w = (1.0 - a1 + a2) * (1.0 - a1 + a2);
    let a2_w = -4.0 * a2;
    (a0, a1_w, a2_w)
}

fn unity_on(a1: f64, a2: f64) -> FeqBiquadCoefficients {
    FeqBiquadCoefficients {
        b0: 1.0,
        b1: a1,
        b2: a2,
        a1,
        a2,
    }
}

fn matched_bell(omega: f64, gain_db: f64, q: f64) -> FeqBiquadCoefficients {
    let g = 10.0_f64.powf(gain_db / 20.0);
    if (g - 1.0).abs() < 1e-9 {
        let (a1, a2) = matched_poles(omega, q);
        return unity_on(a1, a2);
    }
    // Cut is built as the exact reciprocal of the boost of the same depth.
    if g < 1.0 {
        let boost = matched_bell(omega, -gain_db, q);
        if boost.b0 != 0.0 && boost.b0.is_finite() {
            let inv = 1.0 / boost.b0;
            return FeqBiquadCoefficients {
                b0: inv,
                b1: boost.a1 * inv,
                b2: boost.a2 * inv,
                a1: boost.b1 * inv,
                a2: boost.b2 * inv,
            };
        }
    }

    let sqrt_g = g.sqrt();
    let (a1, a2) = matched_poles(omega, q * sqrt_g);
    let (a0, a1_w, a2_w) = denominator_weights(a1, a2);
    let phi = Phis::at(omega);

    let r1 = (a0 * phi.p0 + a1_w * phi.p1 + a2_w * phi.p2) * g * g;
    let b0_w = a0;
    let b1_w = r1;
    let b2_w = (r1 - b0_w * phi.p0 - b1_w * phi.p1) / phi.p2.max(1e-30);

    let sqrt_b0 = b0_w.max(0.0).sqrt();
    let sqrt_b1 = b1_w.max(0.0).sqrt();
    let w = 0.5 * (sqrt_b0 + sqrt_b1);
    let disc = (w * w + b2_w).max(0.0);
    let b0 = 0.5 * (w + disc.sqrt());
    let b1 = 0.5 * (sqrt_b0 - sqrt_b1);
    let b2 = -b2_w / (4.0 * b0.max(1e-30));
    FeqBiquadCoefficients { b0, b1, b2, a1, a2 }
}

fn matched_low_pass(omega: f64, q: f64) -> FeqBiquadCoefficients {
    let (a1, a2) = matched_poles(omega, q);
    let (a0, a1_w, a2_w) = denominator_weights(a1, a2);
    let phi = Phis::at(omega);

    let sqrt_b0 = a0.max(0.0).sqrt();
    let b0_w = a0;
    let r1 = phi.dot(a0, a1_w, a2_w) * q * q;
    let b2_w = (r1 - b0_w * phi.p0) / phi.p2.max(1e-30);

    let w = 0.5 * sqrt_b0;
    let disc = (w * w + b2_w).max(0.0);
    let b0 = 0.5 * (w + disc.sqrt());
    let b1 = 0.5 * sqrt_b0;
    let b2 = -b2_w / (4.0 * b0.max(1e-30));
    FeqBiquadCoefficients { b0, b1, b2, a1, a2 }
}

fn matched_high_pass(omega: f64, q: f64) -> FeqBiquadCoefficients {
    let (a1, a2) = matched_poles(omega, q);
    let (a0, a1_w, a2_w) = denominator_weights(a1, a2);
    let phi = Phis::at(omega);

    let r1 = phi.dot(a0, a1_w, a2_w) * q * q;
    let b0 = r1.max(0.0).sqrt() / (4.0 * phi.p1.max(1e-30));
    FeqBiquadCoefficients {
        b0,
        b1: -2.0 * b0,
        b2: b0,
        a1,
        a2,
    }
}

fn matched_band_pass(omega: f64, q: f64) -> FeqBiquadCoefficients {
    let (a1, a2) = matched_poles(omega, q);
    let (a0, a1_w, a2_w) = denominator_weights(a1, a2);
    let phi = Phis::at(omega);

    let r1 = phi.dot(a0, a1_w, a2_w);
    let r2 = -a0 + a1_w + 4.0 * (phi.p0 - phi.p1) * a2_w;

    let b2_w = (r1 - r2 * phi.p1) / (4.0 * phi.p1 * phi.p1).max(1e-30);
    let b1_w = r2 + 4.0 * (phi.p1 - phi.p0) * b2_w;

    let sqrt_b1 = b1_w.max(0.0).sqrt();
    let b1 = -0.5 * sqrt_b1;
    let disc = (b1_w + 4.0 * b2_w).max(0.0);
    let b0 = 0.25 * (disc.sqrt() + sqrt_b1);
    let b2 = -b0 - b1;
    FeqBiquadCoefficients { b0, b1, b2, a1, a2 }
}

fn matched_shelf(omega: f64, gain_db: f64, high: bool) -> (bool, FeqBiquadCoefficients) {
    let mut g = 10.0_f64.powf(gain_db / 20.0);
    if (g - 1.0).abs() < 1e-9 {
        let (a1, a2) = matched_poles(omega, BUTTERWORTH_Q);
        return (true, unity_on(a1, a2));
    }
    if high {
        g = 1.0 / g;
    }

    let g_fourth = g.powf(0.25);
    let (a1, a2) = matched_poles(omega / g_fourth, BUTTERWORTH_Q);
    let (b_hat1, b_hat2) = matched_poles(omega * g_fourth, BUTTERWORTH_Q);

    let (a0, a1_w, a2_w) = denominator_weights(a1, a2);
    let (u0, u1, u2) = denominator_weights(b_hat1, b_hat2);

    let phi_c = Phis::at(omega);
    let phi_m = Phis::at((omega / g_fourth).min(PI * 0.99));

    let num_c = phi_c.dot(u0, u1, u2);
    let num_m = phi_m.dot(u0, u1, u2);
    if num_c <= 0.0 || num_m <= 0.0 {
        return (false, FeqBiquadCoefficients::default());
    }
    let target_c = g * phi_c.dot(a0, a1_w, a2_w) / num_c;
    let target_m = g.sqrt() * phi_m.dot(a0, a1_w, a2_w) / num_m;

    let det = phi_c.p1 * phi_m.p2 - phi_c.p2 * phi_m.p1;
    let k0 = g * g * a0 / u0.max(1e-30);
    let mut k1 = target_c;
    if det.abs() > 1e-24 {
        let rhs_c = target_c - k0 * phi_c.p0;
        let rhs_m = target_m - k0 * phi_m.p0;
        k1 = (rhs_c * phi_m.p2 - rhs_m * phi_c.p2) / det;
    }
    if k0 < 0.0 || k1 < 0.0 {
        return (false, FeqBiquadCoefficients::default());
    }

    let b0_w = k0 * u0;
    let b1_w = k1 * u1;
    let r_c = target_c * num_c;
    let b2_w = (r_c - b0_w * phi_c.p0 - b1_w * phi_c.p1) / phi_c.p2.max(1e-30);

    let sqrt_b0 = b0_w.max(0.0).sqrt();
    let sqrt_b1 = b1_w.max(0.0).sqrt();
    let w = 0.5 * (sqrt_b0 + sqrt_b1);
    let disc = w * w + b2_w;
    if disc < 0.0 {
        return (false, FeqBiquadCoefficients::default());
    }
    let b0 = 0.5 * (w + disc.sqrt());
    if b0 <= 1e-15 {
        return (false, FeqBiquadCoefficients::default());
    }
    let mut b1 = 0.5 * (sqrt_b0 - sqrt_b1);
    let mut b2 = -b2_w / (4.0 * b0);
    let mut final_b0 = b0;

    if high {
        let scale = 1.0 / g;
        final_b0 *= scale;
        b1 *= scale;
        b2 *= scale;
    }

    (
        true,
        FeqBiquadCoefficients {
            b0: final_b0,
            b1,
            b2,
            a1,
            a2,
        },
    )
}

/// Exact port of `feq_biquad_coefficients_matched` in `/native/dsp-core/src/biquad_matched.cpp`.
pub fn biquad_coefficients_matched(
    filter_type: FeqFilterType,
    frequency: f64,
    gain_db: f64,
    quality: f64,
    sample_rate: f64,
) -> FeqBiquadCoefficients {
    #[cfg(feature = "link-cpp-core")]
    unsafe {
        return fluideq_dsp_sys::feq_biquad_coefficients_matched(
            filter_type,
            frequency,
            gain_db,
            quality,
            sample_rate,
        );
    }

    #[cfg(not(feature = "link-cpp-core"))]
    {
        if !sample_rate.is_finite()
            || sample_rate <= 0.0
            || !frequency.is_finite()
            || frequency <= 0.0
            || frequency >= sample_rate * 0.5
            || !quality.is_finite()
            || quality <= 0.0
        {
            return biquad_coefficients_cookbook(filter_type, frequency, gain_db, quality, sample_rate);
        }

        let omega = 2.0 * PI * frequency / sample_rate;
        let result = match filter_type {
            FeqFilterType::FEQ_FILTER_PK => matched_bell(omega, gain_db, quality),
            FeqFilterType::FEQ_FILTER_LPQ => matched_low_pass(omega, quality),
            FeqFilterType::FEQ_FILTER_HPQ => matched_high_pass(omega, quality),
            FeqFilterType::FEQ_FILTER_BP => matched_band_pass(omega, quality),
            FeqFilterType::FEQ_FILTER_LSC | FeqFilterType::FEQ_FILTER_HSC => {
                if (quality - BUTTERWORTH_Q).abs() > BUTTERWORTH_TOLERANCE {
                    return biquad_coefficients_cookbook(
                        filter_type,
                        frequency,
                        gain_db,
                        quality,
                        sample_rate,
                    );
                }
                let (ok, shelf) =
                    matched_shelf(omega, gain_db, filter_type == FeqFilterType::FEQ_FILTER_HSC);
                if !ok {
                    return biquad_coefficients_cookbook(
                        filter_type,
                        frequency,
                        gain_db,
                        quality,
                        sample_rate,
                    );
                }
                shelf
            }
            FeqFilterType::FEQ_FILTER_NO => {
                return biquad_coefficients_cookbook(
                    filter_type,
                    frequency,
                    gain_db,
                    quality,
                    sample_rate,
                );
            }
        };

        let finite = result.b0.is_finite()
            && result.b1.is_finite()
            && result.b2.is_finite()
            && result.a1.is_finite()
            && result.a2.is_finite();
        let stable = result.a2.abs() < 1.0 && result.a1.abs() < 1.0 + result.a2;
        if !finite || !stable {
            return biquad_coefficients_cookbook(filter_type, frequency, gain_db, quality, sample_rate);
        }
        result
    }
}

pub fn biquad_coefficients_designed(
    filter_type: FeqFilterType,
    frequency: f64,
    gain_db: f64,
    quality: f64,
    sample_rate: f64,
    model: FeqEqModel,
    amount: f64,
    matched: bool,
) -> FeqBiquadCoefficients {
    #[cfg(feature = "link-cpp-core")]
    unsafe {
        return fluideq_dsp_sys::feq_biquad_coefficients_designed(
            filter_type,
            frequency,
            gain_db,
            quality,
            sample_rate,
            model,
            amount,
            if matched { 1 } else { 0 },
        );
    }
    #[cfg(not(feature = "link-cpp-core"))]
    {
        let _ = SHELF_BUTTERWORTH_TOLERANCE;
        let q = modelled_quality(filter_type, gain_db, quality, model, amount);
        if matched {
            biquad_coefficients_matched(filter_type, frequency, gain_db, q, sample_rate)
        } else {
            biquad_coefficients_cookbook(filter_type, frequency, gain_db, q, sample_rate)
        }
    }
}

pub fn evaluate_biquad_magnitude_db(
    coeffs: &FeqBiquadCoefficients,
    freq_hz: f64,
    sample_rate: f64,
) -> f64 {
    let w = (2.0 * PI * freq_hz) / sample_rate;
    let cos1 = w.cos();
    let cos2 = (2.0 * w).cos();
    let b0 = coeffs.b0;
    let b1 = coeffs.b1;
    let b2 = coeffs.b2;
    let a1 = coeffs.a1;
    let a2 = coeffs.a2;

    let num = b0 * b0 + b1 * b1 + b2 * b2 + 2.0 * (b0 * b1 + b1 * b2) * cos1 + 2.0 * b0 * b2 * cos2;
    let den = 1.0 + a1 * a1 + a2 * a2 + 2.0 * (a1 + a1 * a2) * cos1 + 2.0 * a2 * cos2;
    if den <= 1e-18 || num <= 1e-18 {
        return -120.0;
    }
    10.0 * (num / den).log10()
}

/// Computes the worst-case auto-headroom preamp offset across log-spaced frequencies
/// (20 Hz .. 20 kHz) so positive EQ peaks never clip the output stage.
pub fn compute_auto_preamp_db(coeffs_list: &[FeqBiquadCoefficients], sample_rate: f64) -> f64 {
    let mut max_boost_db = 0.0_f64;
    let steps = 160usize;
    let log_min = 20.0_f64.ln();
    let log_max = 20000.0_f64.ln();
    for i in 0..=steps {
        let t = (i as f64) / (steps as f64);
        let freq = (log_min + t * (log_max - log_min)).exp();
        let mut total_db = 0.0;
        for c in coeffs_list {
            total_db += evaluate_biquad_magnitude_db(c, freq, sample_rate);
        }
        if total_db > max_boost_db {
            max_boost_db = total_db;
        }
    }
    if max_boost_db > 0.05 {
        -((max_boost_db * 10.0).round() / 10.0)
    } else {
        0.0
    }
}
