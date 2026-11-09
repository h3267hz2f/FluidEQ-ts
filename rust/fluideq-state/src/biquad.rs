/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Biquad filter synthesis (RBJ Audio EQ Cookbook + Martin Vicanek analog-matched),
//! character model Q shaping, and worst-case auto-headroom calculation in Rust.

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
struct Denominator {
    a1: f64,
    a2: f64,
    cap_a0: f64,
    cap_a1: f64,
    cap_a2: f64,
}

/// Exact port of `matched_poles` in `/native/dsp-core/src/biquad_matched.cpp` (lines 58–69).
fn matched_poles(w: f64, zeta: f64) -> Denominator {
    let decay = (-zeta * w).exp();
    let a1 = if zeta <= 1.0 {
        -2.0 * decay * (w * (1.0 - zeta * zeta).sqrt()).cos()
    } else {
        -2.0 * decay * (w * (zeta * zeta - 1.0).sqrt()).cosh()
    };
    let a2 = (-2.0 * zeta * w).exp();
    let cap_a0 = (1.0 + a1 + a2) * (1.0 + a1 + a2);
    let cap_a1 = (1.0 - a1 + a2) * (1.0 - a1 + a2);
    let cap_a2 = -4.0 * a2;
    Denominator {
        a1,
        a2,
        cap_a0,
        cap_a1,
        cap_a2,
    }
}

#[derive(Debug, Clone, Copy)]
struct Phis {
    phi0: f64,
    phi1: f64,
    phi2: f64,
}

impl Phis {
    fn at(w: f64) -> Self {
        let half = (w / 2.0).sin();
        let phi1 = half * half;
        let phi0 = 1.0 - phi1;
        let phi2 = 4.0 * phi0 * phi1;
        Self { phi0, phi1, phi2 }
    }
}

#[inline]
fn root(value: f64) -> f64 {
    if value > 0.0 {
        value.sqrt()
    } else {
        0.0
    }
}

#[inline]
fn is_finite_coeffs(k: &FeqBiquadCoefficients) -> bool {
    k.b0.is_finite()
        && k.b1.is_finite()
        && k.b2.is_finite()
        && k.a1.is_finite()
        && k.a2.is_finite()
        && k.b0 != 0.0
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

/// Exact port of `bell` in `/native/dsp-core/src/biquad_matched.cpp` (lines 122–156).
fn matched_bell(w0: f64, gain_db: f64, quality: f64) -> FeqBiquadCoefficients {
    if gain_db == 0.0 {
        let d = matched_poles(w0, 1.0 / (2.0 * quality));
        return unity_on(d.a1, d.a2);
    }
    let cut = gain_db < 0.0;
    let g = 10.0_f64.powf(gain_db.abs() / 20.0);
    let d = matched_poles(w0, 1.0 / (2.0 * quality * g.sqrt()));
    let p = Phis::at(w0);
    let b0_w = d.cap_a0;
    let r1 = (d.cap_a0 * p.phi0 + d.cap_a1 * p.phi1 + d.cap_a2 * p.phi2) * g * g;
    let r2 = (-d.cap_a0 + d.cap_a1 + 4.0 * (p.phi0 - p.phi1) * d.cap_a2) * g * g;
    let b2_w = (r1 - r2 * p.phi1 - b0_w) / (4.0 * p.phi1 * p.phi1);
    let b1_w = r2 + b0_w + 4.0 * (p.phi1 - p.phi0) * b2_w;
    let w = 0.5 * (root(b0_w) + root(b1_w));
    let boost_b0 = 0.5 * (w + root(w * w + b2_w));
    let boost_b1 = 0.5 * (root(b0_w) - root(b1_w));
    let boost_b2 = -b2_w / (4.0 * boost_b0);
    let boost = FeqBiquadCoefficients {
        b0: boost_b0,
        b1: boost_b1,
        b2: boost_b2,
        a1: d.a1,
        a2: d.a2,
    };
    if !cut {
        return boost;
    }
    FeqBiquadCoefficients {
        b0: 1.0 / boost.b0,
        b1: boost.a1 / boost.b0,
        b2: boost.a2 / boost.b0,
        a1: boost.b1 / boost.b0,
        a2: boost.b2 / boost.b0,
    }
}

/// Exact port of `low_pass` in `/native/dsp-core/src/biquad_matched.cpp` (lines 159–172).
fn matched_low_pass(w0: f64, quality: f64) -> FeqBiquadCoefficients {
    let d = matched_poles(w0, 1.0 / (2.0 * quality));
    let p = Phis::at(w0);
    let b0_w = d.cap_a0;
    let r1 = (d.cap_a0 * p.phi0 + d.cap_a1 * p.phi1 + d.cap_a2 * p.phi2) * quality * quality;
    let b1_w = (r1 - b0_w * p.phi0) / p.phi1;
    let b0 = 0.5 * (root(b0_w) + root(b1_w));
    let b1 = root(b0_w) - b0;
    FeqBiquadCoefficients {
        b0,
        b1,
        b2: 0.0,
        a1: d.a1,
        a2: d.a2,
    }
}

/// Exact port of `high_pass` in `/native/dsp-core/src/biquad_matched.cpp` (lines 175–186).
fn matched_high_pass(w0: f64, quality: f64) -> FeqBiquadCoefficients {
    let d = matched_poles(w0, 1.0 / (2.0 * quality));
    let p = Phis::at(w0);
    let b0 = quality * root(d.cap_a0 * p.phi0 + d.cap_a1 * p.phi1 + d.cap_a2 * p.phi2)
        / (4.0 * p.phi1);
    FeqBiquadCoefficients {
        b0,
        b1: -2.0 * b0,
        b2: b0,
        a1: d.a1,
        a2: d.a2,
    }
}

/// Exact port of `band_pass` in `/native/dsp-core/src/biquad_matched.cpp` (lines 189–203).
fn matched_band_pass(w0: f64, quality: f64) -> FeqBiquadCoefficients {
    let d = matched_poles(w0, 1.0 / (2.0 * quality));
    let p = Phis::at(w0);
    let r1 = d.cap_a0 * p.phi0 + d.cap_a1 * p.phi1 + d.cap_a2 * p.phi2;
    let r2 = -d.cap_a0 + d.cap_a1 + 4.0 * (p.phi0 - p.phi1) * d.cap_a2;
    let b2_w = (r1 - r2 * p.phi1) / (4.0 * p.phi1 * p.phi1);
    let b1_w = r2 + 4.0 * (p.phi1 - p.phi0) * b2_w;
    let b1 = -0.5 * root(b1_w);
    let b0 = 0.5 * (root(b2_w + b1 * b1) - b1);
    let b2 = -b0 - b1;
    FeqBiquadCoefficients {
        b0,
        b1,
        b2,
        a1: d.a1,
        a2: d.a2,
    }
}

/// Exact port of `shelf` in `/native/dsp-core/src/biquad_matched.cpp` (lines 210–258).
fn matched_shelf(high: bool, fc: f64, gain_db: f64) -> FeqBiquadCoefficients {
    let cap_g = 10.0_f64.powf(gain_db / 20.0);
    let mut g = if high { cap_g } else { 1.0 / cap_g };
    if (1.0 - g).abs() < 1e-6 {
        g = 1.00001;
    }
    let fc4 = fc * fc * fc * fc;
    let h_nyquist = (fc4 + g) / (fc4 + 1.0 / g);
    let f1 = fc / (0.160 + 1.543 * fc * fc).sqrt();
    let f2 = fc / (0.947 + 3.806 * fc * fc).sqrt();
    let h_at = |f: f64| -> f64 {
        let f4 = f * f * f * f;
        (fc4 + f4 * g) / (fc4 + f4 / g)
    };
    let h1 = h_at(f1);
    let h2 = h_at(f2);
    let s1 = (PI * f1 / 2.0).sin();
    let s2 = (PI * f2 / 2.0).sin();
    let p1 = s1 * s1;
    let p2 = s2 * s2;
    let d1 = (h1 - 1.0) * (1.0 - p1);
    let d2 = (h2 - 1.0) * (1.0 - p2);
    let c11 = -p1 * d1;
    let c12 = p1 * p1 * (h_nyquist - h1);
    let c21 = -p2 * d2;
    let c22 = p2 * p2 * (h_nyquist - h2);
    let alpha = (c22 * d1 - c12 * d2) / (c11 * c22 - c12 * c21);
    let aa1 = (d1 - c11 * alpha) / c12;
    let bb1 = h_nyquist * aa1;
    let aa2 = (alpha - aa1) / 4.0;
    let bb2 = (alpha - bb1) / 4.0;
    let v = 0.5 * (1.0 + root(aa1));
    let w = 0.5 * (1.0 + root(bb1));
    let a0 = 0.5 * (v + root(v * v + aa2));
    let beta0 = 0.5 * (w + root(w * w + bb2));
    let mut k = FeqBiquadCoefficients {
        a1: (1.0 - v) / a0,
        a2: -aa2 / (4.0 * a0 * a0),
        b0: beta0 / a0,
        b1: (1.0 - w) / a0,
        b2: -bb2 / (4.0 * beta0 * a0),
    };
    if !high {
        k.b0 *= cap_g;
        k.b1 *= cap_g;
        k.b2 *= cap_g;
    }
    k
}

/// Pure-Rust port of `feq_biquad_coefficients_matched` in `/native/dsp-core/src/biquad_matched.cpp` (lines 264–317).
pub fn biquad_coefficients_matched_rust(
    filter_type: FeqFilterType,
    frequency: f64,
    gain_db: f64,
    quality: f64,
    sample_rate: f64,
) -> FeqBiquadCoefficients {
    let cookbook = biquad_coefficients_cookbook(filter_type, frequency, gain_db, quality, sample_rate);
    if !(sample_rate > 0.0) || !(frequency > 0.0) || !(quality > 0.0) {
        return cookbook;
    }
    let safe = frequency.min(sample_rate * 0.499);
    let w0 = 2.0 * PI * safe / sample_rate;
    let matched = match filter_type {
        FeqFilterType::FEQ_FILTER_PK => matched_bell(w0, gain_db, quality),
        FeqFilterType::FEQ_FILTER_LPQ => matched_low_pass(w0, quality),
        FeqFilterType::FEQ_FILTER_HPQ => matched_high_pass(w0, quality),
        FeqFilterType::FEQ_FILTER_BP => matched_band_pass(w0, quality),
        FeqFilterType::FEQ_FILTER_LSC | FeqFilterType::FEQ_FILTER_HSC => {
            if (quality - BUTTERWORTH_Q).abs() > BUTTERWORTH_TOLERANCE {
                return cookbook;
            }
            let fc = frequency.min(2.0 * sample_rate) / (sample_rate / 2.0);
            let mut s = matched_shelf(filter_type == FeqFilterType::FEQ_FILTER_HSC, fc, gain_db);
            if gain_db == 0.0 {
                s = unity_on(s.a1, s.a2);
            }
            s
        }
        FeqFilterType::FEQ_FILTER_NO => {
            return cookbook;
        }
    };

    let stable = matched.a2.abs() < 1.0 && matched.a1.abs() < 1.0 + matched.a2;
    if is_finite_coeffs(&matched) && stable {
        matched
    } else {
        cookbook
    }
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
        biquad_coefficients_matched_rust(filter_type, frequency, gain_db, quality, sample_rate)
    }
}

/// Pure-Rust port of `feq_biquad_coefficients_designed` in `/native/dsp-core/src/biquad.cpp`.
pub fn biquad_coefficients_designed_rust(
    filter_type: FeqFilterType,
    frequency: f64,
    gain_db: f64,
    quality: f64,
    sample_rate: f64,
    model: FeqEqModel,
    amount: f64,
    matched: bool,
) -> FeqBiquadCoefficients {
    let _ = SHELF_BUTTERWORTH_TOLERANCE;
    let q = modelled_quality(filter_type, gain_db, quality, model, amount);
    if matched {
        biquad_coefficients_matched_rust(filter_type, frequency, gain_db, q, sample_rate)
    } else {
        biquad_coefficients_cookbook(filter_type, frequency, gain_db, q, sample_rate)
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
        biquad_coefficients_designed_rust(
            filter_type,
            frequency,
            gain_db,
            quality,
            sample_rate,
            model,
            amount,
            matched,
        )
    }
}

/// Pure-Rust port of `feq_biquad_process` in `/native/dsp-core/src/biquad.cpp`.
pub fn biquad_process_rust(
    state: &mut fluideq_dsp_sys::FeqBiquadState,
    buffer: &mut [f32],
    coeffs: &FeqBiquadCoefficients,
) {
    let mut x1 = state.x1;
    let mut x2 = state.x2;
    let mut y1 = state.y1;
    let mut y2 = state.y2;
    for sample in buffer.iter_mut() {
        let x0 = f64::from(*sample);
        let y0 = coeffs.b0 * x0 + coeffs.b1 * x1 + coeffs.b2 * x2 - coeffs.a1 * y1 - coeffs.a2 * y2;
        x2 = x1;
        x1 = x0;
        y2 = y1;
        y1 = y0;
        *sample = y0 as f32;
    }
    state.x1 = x1;
    state.x2 = x2;
    state.y1 = y1;
    state.y2 = y2;
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

/// 12-tap 4x polyphase Blackman-windowed sinc true-peak detector (`FeqTruePeak` in `/native/dsp-core/src/primitives.cpp`).
#[derive(Debug, Clone)]
pub struct TruePeakDetector {
    factor: u32,
    history: [f64; 12],
    position: usize,
    phases: [[f64; 12]; 4],
}

impl TruePeakDetector {
    pub fn new(factor: u32) -> Self {
        let factor = if matches!(factor, 1 | 2 | 4) { factor } else { 4 };
        let mut phases = [[0.0_f64; 12]; 4];
        let half = 6.0_f64;
        for (phase, row) in phases.iter_mut().enumerate() {
            let offset = (phase as f64) / 4.0;
            let mut sum = 0.0_f64;
            for (at, tap_slot) in row.iter_mut().enumerate() {
                let x = (at as f64) - half + 1.0 - offset;
                let sinc = if x == 0.0 {
                    1.0
                } else {
                    (PI * x).sin() / (PI * x)
                };
                let t = (at as f64) / 11.0;
                let window = 0.42 - 0.5 * (2.0 * PI * t).cos() + 0.08 * (4.0 * PI * t).cos();
                let val = sinc * window;
                *tap_slot = val;
                sum += val;
            }
            for tap_slot in row.iter_mut() {
                *tap_slot /= sum;
            }
        }
        Self {
            factor,
            history: [0.0; 12],
            position: 0,
            phases,
        }
    }

    pub fn process_sample(&mut self, sample: f64) -> f64 {
        if self.factor == 1 {
            return sample.abs();
        }
        self.history[self.position] = sample;
        self.position = if self.position + 1 == 12 {
            0
        } else {
            self.position + 1
        };

        let mut peak = 0.0_f64;
        let phase_step = (4 / self.factor) as usize;
        let mut phase = 0usize;
        while phase < 4 {
            let taps = &self.phases[phase];
            let mut sum = 0.0_f64;
            let mut tap = 0usize;
            for read in self.position..12 {
                sum += self.history[read] * taps[tap];
                tap += 1;
            }
            let mut read = 0usize;
            while tap < 12 {
                sum += self.history[read] * taps[tap];
                tap += 1;
                read += 1;
            }
            let mag = sum.abs();
            if mag > peak {
                peak = mag;
            }
            phase += phase_step;
        }
        peak
    }

    pub fn process_block(&mut self, input: &[f32]) -> f64 {
        let mut peak = 0.0_f64;
        for &s in input {
            let mag = self.process_sample(f64::from(s));
            if mag > peak {
                peak = mag;
            }
        }
        peak
    }
}

#[cfg(all(test, feature = "link-cpp-core"))]
mod cpp_parity_tests {
    use super::*;
    use fluideq_dsp_sys::{
        feq_biquad_coefficients_designed, feq_biquad_process, FeqBiquadState,
    };

    #[test]
    fn biquad_designed_and_matched_coefficients_match_cpp_core_sample_for_sample() {
        let filter_types = [
            FeqFilterType::FEQ_FILTER_PK,
            FeqFilterType::FEQ_FILTER_NO,
            FeqFilterType::FEQ_FILTER_LSC,
            FeqFilterType::FEQ_FILTER_HSC,
            FeqFilterType::FEQ_FILTER_LPQ,
            FeqFilterType::FEQ_FILTER_HPQ,
            FeqFilterType::FEQ_FILTER_BP,
        ];
        let models = [
            FeqEqModel::FEQ_EQ_MODEL_CLEAN,
            FeqEqModel::FEQ_EQ_MODEL_PROPORTIONAL,
            FeqEqModel::FEQ_EQ_MODEL_WIDE,
            FeqEqModel::FEQ_EQ_MODEL_ASYMMETRIC,
        ];
        let frequencies = [60.0, 440.0, 2500.0, 12_000.0, 18_500.0];
        let gains = [-12.0, -3.5, 0.0, 4.5, 12.0];
        let qualities = [0.5, 0.70710678, 1.414, 4.0];

        for &ft in &filter_types {
            for &model in &models {
                for &matched in &[false, true] {
                    for &freq in &frequencies {
                        for &gain in &gains {
                            for &q in &qualities {
                                let rust_c = biquad_coefficients_designed_rust(
                                    ft, freq, gain, q, 48_000.0, model, 0.75, matched,
                                );
                                let cpp_c = unsafe {
                                    feq_biquad_coefficients_designed(
                                        ft,
                                        freq,
                                        gain,
                                        q,
                                        48_000.0,
                                        model,
                                        0.75,
                                        if matched { 1 } else { 0 },
                                    )
                                };
                                let diff = (rust_c.b0 - cpp_c.b0)
                                    .abs()
                                    .max((rust_c.b1 - cpp_c.b1).abs())
                                    .max((rust_c.b2 - cpp_c.b2).abs())
                                    .max((rust_c.a1 - cpp_c.a1).abs())
                                    .max((rust_c.a2 - cpp_c.a2).abs());
                                assert!(
                                    diff < 1e-11,
                                    "Coefficient mismatch for {:?} {:?} matched={} freq={} gain={} q={}: rust={:?} cpp={:?} diff={}",
                                    ft,
                                    model,
                                    matched,
                                    freq,
                                    gain,
                                    q,
                                    rust_c,
                                    cpp_c,
                                    diff
                                );
                            }
                        }
                    }
                }
            }
        }

        // Also verify sample-for-sample biquad_process_rust parity against C++ feq_biquad_process
        let coeffs = biquad_coefficients_designed_rust(
            FeqFilterType::FEQ_FILTER_PK,
            3200.0,
            6.5,
            1.8,
            48_000.0,
            FeqEqModel::FEQ_EQ_MODEL_ASYMMETRIC,
            1.0,
            true,
        );
        let mut rust_buf = vec![0.0_f32; 512];
        for (i, s) in rust_buf.iter_mut().enumerate() {
            let t = (i as f32) / 48_000.0;
            *s = 0.4 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()
                + 0.3 * (2.0 * std::f32::consts::PI * 3200.0 * t).cos();
        }
        let mut cpp_buf = rust_buf.clone();
        let mut rust_state = FeqBiquadState::default();
        let mut cpp_state = FeqBiquadState::default();
        biquad_process_rust(&mut rust_state, &mut rust_buf, &coeffs);
        unsafe {
            feq_biquad_process(
                &mut cpp_state,
                cpp_buf.as_mut_ptr(),
                cpp_buf.len() as u32,
                &coeffs,
            );
        }
        for i in 0..rust_buf.len() {
            assert!(
                (rust_buf[i] - cpp_buf[i]).abs() <= 1e-7,
                "Sample mismatch at {}: rust={} cpp={}",
                i,
                rust_buf[i],
                cpp_buf[i]
            );
        }
    }
}



