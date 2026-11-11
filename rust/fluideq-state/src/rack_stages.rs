/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! The DSP rack's stage switches.
//!
//! One name per stage the window shows, and one place that turns it off and
//! on again. The settings a stage keeps are not reset: switching it off only
//! makes the chain skip it, so turning it back on sounds like it did before.
//! The normalizer has no `enabled` field — its mode *is* the switch — so the
//! two shapes meet here rather than in the window.

use fluideq_dsp_sys::{FeqChainSettings, FeqNormalizerMode};
use std::os::raw::c_int;

/// The stages the rack page shows, in the order the chain runs them.
pub const STAGE_IDS: [&str; 11] = [
    "normalizer",
    "denoise",
    "deesser",
    "exciter",
    "bass-forge",
    "equaliser",
    "bass-punch",
    "dimension",
    "room",
    "maximizer",
    "master",
];

fn flag(enabled: bool) -> c_int {
    if enabled {
        1
    } else {
        0
    }
}

pub fn set_stage_enabled(
    settings: &mut FeqChainSettings,
    stage: &str,
    enabled: bool,
) -> Result<(), String> {
    match stage {
        "normalizer" => {
            if enabled {
                if settings.normalizer.mode == FeqNormalizerMode::FEQ_NORMALIZER_OFF as c_int {
                    settings.normalizer.mode = FeqNormalizerMode::FEQ_NORMALIZER_GUARD as c_int;
                }
            } else {
                settings.normalizer.mode = FeqNormalizerMode::FEQ_NORMALIZER_OFF as c_int;
            }
        }
        "denoise" => settings.denoise.enabled = flag(enabled),
        "deesser" => settings.deesser.enabled = flag(enabled),
        "exciter" => settings.exciter.enabled = flag(enabled),
        "bass-forge" => settings.bass_forge.enabled = flag(enabled),
        "equaliser" => settings.eq.enabled = flag(enabled),
        "bass-punch" => settings.bass_punch.enabled = flag(enabled),
        "dimension" => settings.dimension.enabled = flag(enabled),
        "room" => settings.room.enabled = flag(enabled),
        "maximizer" => settings.maximizer.enabled = flag(enabled),
        "master" => settings.master.enabled = flag(enabled),
        _ => return Err(format!("there is no rack stage named \"{stage}\"")),
    }
    Ok(())
}

/// The parameters the rack page may write, with the range each one accepts.
/// The window draws the same ranges; the clamp here is what the engine gets.
const PARAMETER_RANGES: [(&str, &str, f64, f64); 24] = [
    ("normalizer", "target_lufs", -35.0, -5.0),
    ("normalizer", "ceiling_db", -6.0, 0.0),
    ("denoise", "hiss_amount", 0.0, 1.0),
    ("denoise", "hiss_floor_db", -90.0, -30.0),
    ("deesser", "sensitivity", 0.0, 1.0),
    ("deesser", "range_db", 0.0, 24.0),
    ("deesser", "low_hz", 1000.0, 16000.0),
    ("deesser", "high_hz", 2000.0, 20000.0),
    ("exciter", "organic_amount", 0.0, 1.0),
    ("bass-forge", "drive_db", 0.0, 18.0),
    ("bass-forge", "sub_amount", 0.0, 1.0),
    ("bass-forge", "mix", 0.0, 1.0),
    ("equaliser", "model_amount", 0.0, 1.0),
    ("bass-punch", "attack", 0.0, 1.0),
    ("bass-punch", "sustain", 0.0, 1.0),
    ("bass-punch", "mix", 0.0, 1.0),
    ("dimension", "mid_width", 0.0, 2.0),
    ("dimension", "decorrelation", 0.0, 1.0),
    ("room", "size_m", 2.0, 20.0),
    ("room", "distance_m", 0.3, 10.0),
    ("room", "ambience_mix", 0.0, 1.0),
    ("maximizer", "drive_db", 0.0, 24.0),
    ("master", "output_trim_db", -24.0, 12.0),
    ("master", "loudness_target_lufs", -24.0, -8.0),
];

fn parameter_range(stage: &str, parameter: &str) -> Option<(f64, f64)> {
    PARAMETER_RANGES
        .iter()
        .find(|(stage_id, parameter_id, _, _)| *stage_id == stage && *parameter_id == parameter)
        .map(|(_, _, low, high)| (*low, *high))
}

pub fn set_stage_parameter(
    settings: &mut FeqChainSettings,
    stage: &str,
    parameter: &str,
    value: f64,
) -> Result<(), String> {
    if !value.is_finite() {
        return Err(format!("{stage}.{parameter} must be a finite number"));
    }
    let Some((low, high)) = parameter_range(stage, parameter) else {
        return Err(format!(
            "there is no parameter named \"{parameter}\" on the {stage} stage"
        ));
    };
    let value = value.clamp(low, high);
    match (stage, parameter) {
        ("normalizer", "target_lufs") => settings.normalizer.target_lufs = value,
        ("normalizer", "ceiling_db") => settings.normalizer.ceiling_db = value,
        ("denoise", "hiss_amount") => settings.denoise.hiss.amount = value,
        ("denoise", "hiss_floor_db") => settings.denoise.hiss.floor_db = value,
        ("deesser", "sensitivity") => settings.deesser.sensitivity = value,
        ("deesser", "range_db") => settings.deesser.range_db = value,
        ("deesser", "low_hz") => settings.deesser.low_hz = value,
        ("deesser", "high_hz") => settings.deesser.high_hz = value,
        ("exciter", "organic_amount") => settings.exciter.organic_amount = value,
        ("bass-forge", "drive_db") => settings.bass_forge.drive_db = value,
        ("bass-forge", "sub_amount") => settings.bass_forge.sub_amount = value,
        ("bass-forge", "mix") => settings.bass_forge.mix = value,
        ("equaliser", "model_amount") => settings.eq.model_amount = value,
        ("bass-punch", "attack") => settings.bass_punch.attack = value,
        ("bass-punch", "sustain") => settings.bass_punch.sustain = value,
        ("bass-punch", "mix") => settings.bass_punch.mix = value,
        ("dimension", "mid_width") => settings.dimension.mid_width = value,
        ("dimension", "decorrelation") => settings.dimension.decorrelation = value,
        ("room", "size_m") => settings.room.size_m = value,
        ("room", "distance_m") => settings.room.distance_m = value,
        ("room", "ambience_mix") => settings.room.ambience_mix = value,
        ("maximizer", "drive_db") => settings.maximizer.drive_db = value,
        ("master", "output_trim_db") => settings.master.output_trim_db = value,
        ("master", "loudness_target_lufs") => settings.master.loudness_target_lufs = value,
        _ => unreachable!("the range table and the writes must name the same parameters"),
    }
    Ok(())
}

pub fn stage_parameter_value(
    settings: &FeqChainSettings,
    stage: &str,
    parameter: &str,
) -> Option<f64> {
    match (stage, parameter) {
        ("normalizer", "target_lufs") => Some(settings.normalizer.target_lufs),
        ("normalizer", "ceiling_db") => Some(settings.normalizer.ceiling_db),
        ("denoise", "hiss_amount") => Some(settings.denoise.hiss.amount),
        ("denoise", "hiss_floor_db") => Some(settings.denoise.hiss.floor_db),
        ("deesser", "sensitivity") => Some(settings.deesser.sensitivity),
        ("deesser", "range_db") => Some(settings.deesser.range_db),
        ("deesser", "low_hz") => Some(settings.deesser.low_hz),
        ("deesser", "high_hz") => Some(settings.deesser.high_hz),
        ("exciter", "organic_amount") => Some(settings.exciter.organic_amount),
        ("bass-forge", "drive_db") => Some(settings.bass_forge.drive_db),
        ("bass-forge", "sub_amount") => Some(settings.bass_forge.sub_amount),
        ("bass-forge", "mix") => Some(settings.bass_forge.mix),
        ("equaliser", "model_amount") => Some(settings.eq.model_amount),
        ("bass-punch", "attack") => Some(settings.bass_punch.attack),
        ("bass-punch", "sustain") => Some(settings.bass_punch.sustain),
        ("bass-punch", "mix") => Some(settings.bass_punch.mix),
        ("dimension", "mid_width") => Some(settings.dimension.mid_width),
        ("dimension", "decorrelation") => Some(settings.dimension.decorrelation),
        ("room", "size_m") => Some(settings.room.size_m),
        ("room", "distance_m") => Some(settings.room.distance_m),
        ("room", "ambience_mix") => Some(settings.room.ambience_mix),
        ("maximizer", "drive_db") => Some(settings.maximizer.drive_db),
        ("master", "output_trim_db") => Some(settings.master.output_trim_db),
        ("master", "loudness_target_lufs") => Some(settings.master.loudness_target_lufs),
        _ => None,
    }
}

pub fn stage_enabled(settings: &FeqChainSettings, stage: &str) -> Option<bool> {
    match stage {
        "normalizer" => Some(
            settings.normalizer.mode != FeqNormalizerMode::FEQ_NORMALIZER_OFF as c_int,
        ),
        "denoise" => Some(settings.denoise.enabled != 0),
        "deesser" => Some(settings.deesser.enabled != 0),
        "exciter" => Some(settings.exciter.enabled != 0),
        "bass-forge" => Some(settings.bass_forge.enabled != 0),
        "equaliser" => Some(settings.eq.enabled != 0),
        "bass-punch" => Some(settings.bass_punch.enabled != 0),
        "dimension" => Some(settings.dimension.enabled != 0),
        "room" => Some(settings.room.enabled != 0),
        "maximizer" => Some(settings.maximizer.enabled != 0),
        "master" => Some(settings.master.enabled != 0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stage_switch_round_trips() {
        let mut settings = FeqChainSettings::default();
        for stage in STAGE_IDS {
            set_stage_enabled(&mut settings, stage, true).expect("on");
            assert_eq!(stage_enabled(&settings, stage), Some(true), "{stage}");
            set_stage_enabled(&mut settings, stage, false).expect("off");
            assert_eq!(stage_enabled(&settings, stage), Some(false), "{stage}");
        }
    }

    #[test]
    fn the_normalizer_falls_back_to_guard_when_switched_back_on() {
        let mut settings = FeqChainSettings::default();
        settings.normalizer.mode = FeqNormalizerMode::FEQ_NORMALIZER_LEVELER as c_int;

        set_stage_enabled(&mut settings, "normalizer", false).expect("off");
        set_stage_enabled(&mut settings, "normalizer", true).expect("on");
        assert_eq!(
            settings.normalizer.mode,
            FeqNormalizerMode::FEQ_NORMALIZER_GUARD as c_int
        );
    }

    #[test]
    fn every_rack_parameter_clamps_to_its_range() {
        for (stage, parameter, low, high) in PARAMETER_RANGES {
            let mut settings = FeqChainSettings::default();
            set_stage_parameter(&mut settings, stage, parameter, high + 1000.0).expect("high");
            assert_eq!(
                stage_parameter_value(&settings, stage, parameter),
                Some(high),
                "{stage}.{parameter}"
            );
            set_stage_parameter(&mut settings, stage, parameter, low - 1000.0).expect("low");
            assert_eq!(
                stage_parameter_value(&settings, stage, parameter),
                Some(low),
                "{stage}.{parameter}"
            );
        }
    }

    #[test]
    fn an_unknown_parameter_is_refused_rather_than_ignored() {
        let mut settings = FeqChainSettings::default();
        assert!(set_stage_parameter(&mut settings, "room", "ceiling_db", 0.0).is_err());
        assert!(set_stage_parameter(&mut settings, "denoise", "target_lufs", 0.0).is_err());
        assert_eq!(stage_parameter_value(&settings, "room", "ceiling_db"), None);
    }

    #[test]
    fn an_unknown_stage_is_refused_rather_than_ignored() {
        let mut settings = FeqChainSettings::default();
        assert!(set_stage_enabled(&mut settings, "crossfade", true).is_err());
        assert_eq!(stage_enabled(&settings, "crossfade"), None);
    }
}
