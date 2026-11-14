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

use fluideq_dsp_sys::{
    FeqChainSettings, FeqCompanderBand, FeqNormalizerMode, FEQ_COMPANDER_BANDS,
};
use std::os::raw::c_int;

/// The stages the rack page shows, in the order the chain runs them.
pub const STAGE_IDS: [&str; 13] = [
    "normalizer",
    "denoise",
    "deesser",
    "exciter",
    "bass-forge",
    "equaliser",
    "footsteps",
    "bass-punch",
    "dimension",
    "room",
    "dynamics",
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
        "footsteps" => settings.footsteps.enabled = flag(enabled),
        "bass-punch" => settings.bass_punch.enabled = flag(enabled),
        "dimension" => settings.dimension.enabled = flag(enabled),
        "room" => settings.room.enabled = flag(enabled),
        "dynamics" => settings.compander.enabled = flag(enabled),
        "maximizer" => settings.maximizer.enabled = flag(enabled),
        "master" => settings.master.enabled = flag(enabled),
        _ => return Err(format!("there is no rack stage named \"{stage}\"")),
    }
    Ok(())
}

/// The stages that can pass only their difference to the output, for tuning.
pub fn stage_supports_isolate(stage: &str) -> bool {
    matches!(
        stage,
        "denoise"
            | "deesser"
            | "exciter"
            | "equaliser"
            | "footsteps"
            | "bass-forge"
            | "bass-punch"
    )
}

/// Turns the solo/difference monitor of a stage on or off. These flags are
/// transient: they are cleared on load so a saved file never leaves a stage
/// soloed.
pub fn set_stage_isolate(
    settings: &mut FeqChainSettings,
    stage: &str,
    enabled: bool,
) -> Result<(), String> {
    let value = flag(enabled);
    match stage {
        "denoise" => settings.denoise.isolate = value,
        "deesser" => settings.deesser.isolate = value,
        "exciter" => settings.exciter.isolate = value,
        "equaliser" => settings.eq.isolate = value,
        "footsteps" => settings.footsteps.isolate = value,
        "bass-forge" => settings.bass_forge.isolate = value,
        "bass-punch" => settings.bass_punch.isolate = value,
        _ => return Err(format!("the {stage} stage has no difference monitor")),
    }
    Ok(())
}

pub fn stage_isolate(settings: &FeqChainSettings, stage: &str) -> Option<bool> {
    match stage {
        "denoise" => Some(settings.denoise.isolate != 0),
        "deesser" => Some(settings.deesser.isolate != 0),
        "exciter" => Some(settings.exciter.isolate != 0),
        "equaliser" => Some(settings.eq.isolate != 0),
        "footsteps" => Some(settings.footsteps.isolate != 0),
        "bass-forge" => Some(settings.bass_forge.isolate != 0),
        "bass-punch" => Some(settings.bass_punch.isolate != 0),
        _ => None,
    }
}

/// The normalizer's mode *is* its power switch, so the window names a mode
/// rather than flipping a boolean that does not exist in the settings.
pub fn set_normalizer_mode(settings: &mut FeqChainSettings, mode: &str) -> Result<(), String> {
    let id = match mode.trim().to_ascii_lowercase().as_str() {
        "off" => FeqNormalizerMode::FEQ_NORMALIZER_OFF,
        "guard" => FeqNormalizerMode::FEQ_NORMALIZER_GUARD,
        "leveler" | "level" => FeqNormalizerMode::FEQ_NORMALIZER_LEVELER,
        _ => return Err(format!("there is no normalizer mode named \"{mode}\"")),
    };
    settings.normalizer.mode = id as c_int;
    Ok(())
}

pub fn normalizer_mode_name(settings: &FeqChainSettings) -> &'static str {
    match settings.normalizer.mode {
        mode if mode == FeqNormalizerMode::FEQ_NORMALIZER_LEVELER as c_int => "leveler",
        mode if mode == FeqNormalizerMode::FEQ_NORMALIZER_GUARD as c_int => "guard",
        _ => "off",
    }
}

/// Delivery-target presets for the master stage: the target loudness and the
/// true-peak ceiling move together, so one choice is a whole mastering stance.
pub fn set_master_target(settings: &mut FeqChainSettings, target: &str) -> Result<(), String> {
    let (target_lufs, ceiling_db) = match target.trim().to_ascii_lowercase().as_str() {
        "streaming" => (-14.0, -1.0),
        "podcast" => (-16.0, -1.0),
        "reference" => (-18.0, -1.0),
        "cinema" => (-24.0, -2.0),
        _ => return Err(format!("there is no delivery target named \"{target}\"")),
    };
    settings.master.loudness_target_lufs = target_lufs;
    settings.master.ceiling_db = ceiling_db;
    settings.master.loudness_maximize = 1;
    Ok(())
}

/// The parameters the rack page may write, with the range each one accepts.
/// The window draws the same ranges; the clamp here is what the engine gets.
const PARAMETER_RANGES: [(&str, &str, f64, f64); 58] = [
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
    ("footsteps", "lift", 0.0, 1.0),
    ("footsteps", "calm", 0.0, 1.0),
    ("footsteps", "heavy", 0.0, 1.0),
    ("footsteps", "environment", -1.0, 1.0),
    ("footsteps", "voices", 0.0, 1.0),
    ("footsteps", "duck", 0.0, 1.0),
    ("bass-punch", "attack", 0.0, 1.0),
    ("bass-punch", "sustain", 0.0, 1.0),
    ("bass-punch", "mix", 0.0, 1.0),
    ("dimension", "mid_width", 0.0, 2.0),
    ("dimension", "decorrelation", 0.0, 1.0),
    ("room", "size_m", 2.0, 12.0),
    ("room", "distance_m", 0.5, 6.0),
    ("room", "ambience_mix", 0.0, 1.0),
    ("room", "walls", 0.0, 1.0),
    ("room", "ambience_decay_s", 0.1, 1.8),
    ("room", "crossover_hz", 40.0, 160.0),
    ("room", "centre_db", -12.0, 6.0),
    ("room", "sub_db", -12.0, 6.0),
    ("room", "early_reflection_db", -60.0, 0.0),
    ("room", "ambience_damping_hz", 1000.0, 12000.0),
    ("room", "music_upmix", 0.0, 1.0),
    ("room", "upmix_amount", 0.0, 1.0),
    ("dynamics", "band1_up_threshold_db", -80.0, -10.0),
    ("dynamics", "band1_up_ratio", 1.0, 4.0),
    ("dynamics", "band1_up_max_db", 0.0, 18.0),
    ("dynamics", "band1_down_threshold_db", -60.0, 0.0),
    ("dynamics", "band1_down_ratio", 1.0, 20.0),
    ("dynamics", "band2_up_threshold_db", -80.0, -10.0),
    ("dynamics", "band2_up_ratio", 1.0, 4.0),
    ("dynamics", "band2_up_max_db", 0.0, 18.0),
    ("dynamics", "band2_down_threshold_db", -60.0, 0.0),
    ("dynamics", "band2_down_ratio", 1.0, 20.0),
    ("dynamics", "band3_up_threshold_db", -80.0, -10.0),
    ("dynamics", "band3_up_ratio", 1.0, 4.0),
    ("dynamics", "band3_up_max_db", 0.0, 18.0),
    ("dynamics", "band3_down_threshold_db", -60.0, 0.0),
    ("dynamics", "band3_down_ratio", 1.0, 20.0),
    ("dynamics", "attack_ms", 1.0, 100.0),
    ("dynamics", "release_ms", 20.0, 1000.0),
    ("dynamics", "mix", 0.0, 1.0),
    ("dynamics", "makeup_db", -12.0, 12.0),
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
        ("footsteps", "lift") => settings.footsteps.lift = value,
        ("footsteps", "calm") => settings.footsteps.calm = value,
        ("footsteps", "heavy") => settings.footsteps.heavy = if value >= 0.5 { 1 } else { 0 },
        ("footsteps", "environment") => settings.footsteps.environment = value,
        ("footsteps", "voices") => settings.footsteps.voices = value,
        ("footsteps", "duck") => settings.footsteps.duck = if value >= 0.5 { 1 } else { 0 },
        ("bass-punch", "attack") => settings.bass_punch.attack = value,
        ("bass-punch", "sustain") => settings.bass_punch.sustain = value,
        ("bass-punch", "mix") => settings.bass_punch.mix = value,
        ("dimension", "mid_width") => settings.dimension.mid_width = value,
        ("dimension", "decorrelation") => settings.dimension.decorrelation = value,
        ("room", "size_m") => settings.room.size_m = value,
        ("room", "distance_m") => settings.room.distance_m = value,
        ("room", "ambience_mix") => settings.room.ambience_mix = value,
        ("room", "walls") => settings.room.walls = value,
        ("room", "ambience_decay_s") => settings.room.ambience_decay_s = value,
        ("room", "crossover_hz") => settings.room.crossover_hz = value,
        ("room", "centre_db") => settings.room.centre_db = value,
        ("room", "sub_db") => settings.room.sub_db = value,
        ("room", "early_reflection_db") => settings.room.early_reflection_db = value,
        ("room", "ambience_damping_hz") => settings.room.ambience_damping_hz = value,
        ("room", "music_upmix") => {
            settings.room.music_upmix = if value >= 0.5 { 1 } else { 0 }
        }
        ("room", "upmix_amount") => settings.room.upmix_amount = value,
        ("dynamics", "band1_up_threshold_db") => {
            settings.compander.bands[0].up_threshold_db = value
        }
        ("dynamics", "band1_up_ratio") => settings.compander.bands[0].up_ratio = value,
        ("dynamics", "band1_up_max_db") => settings.compander.bands[0].up_max_db = value,
        ("dynamics", "band1_down_threshold_db") => {
            settings.compander.bands[0].down_threshold_db = value
        }
        ("dynamics", "band1_down_ratio") => settings.compander.bands[0].down_ratio = value,
        ("dynamics", "band2_up_threshold_db") => {
            settings.compander.bands[1].up_threshold_db = value
        }
        ("dynamics", "band2_up_ratio") => settings.compander.bands[1].up_ratio = value,
        ("dynamics", "band2_up_max_db") => settings.compander.bands[1].up_max_db = value,
        ("dynamics", "band2_down_threshold_db") => {
            settings.compander.bands[1].down_threshold_db = value
        }
        ("dynamics", "band2_down_ratio") => settings.compander.bands[1].down_ratio = value,
        ("dynamics", "band3_up_threshold_db") => {
            settings.compander.bands[2].up_threshold_db = value
        }
        ("dynamics", "band3_up_ratio") => settings.compander.bands[2].up_ratio = value,
        ("dynamics", "band3_up_max_db") => settings.compander.bands[2].up_max_db = value,
        ("dynamics", "band3_down_threshold_db") => {
            settings.compander.bands[2].down_threshold_db = value
        }
        ("dynamics", "band3_down_ratio") => settings.compander.bands[2].down_ratio = value,
        ("dynamics", "attack_ms") => settings.compander.attack_ms = value,
        ("dynamics", "release_ms") => settings.compander.release_ms = value,
        ("dynamics", "mix") => settings.compander.mix = value,
        ("dynamics", "makeup_db") => settings.compander.makeup_db = value,
        ("maximizer", "drive_db") => settings.maximizer.drive_db = value,
        ("master", "output_trim_db") => settings.master.output_trim_db = value,
        ("master", "loudness_target_lufs") => settings.master.loudness_target_lufs = value,
        _ => unreachable!("the range table and the writes must name the same parameters"),
    }
    if stage == "room" {
        // Every room dial is part of a preset's shape, so moving one leaves
        // the preset it came from behind.
        settings.room.preset = crate::room_catalog::CUSTOM_PRESET;
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
        ("footsteps", "lift") => Some(settings.footsteps.lift),
        ("footsteps", "calm") => Some(settings.footsteps.calm),
        ("footsteps", "heavy") => Some(settings.footsteps.heavy as f64),
        ("footsteps", "environment") => Some(settings.footsteps.environment),
        ("footsteps", "voices") => Some(settings.footsteps.voices),
        ("footsteps", "duck") => Some(settings.footsteps.duck as f64),
        ("bass-punch", "attack") => Some(settings.bass_punch.attack),
        ("bass-punch", "sustain") => Some(settings.bass_punch.sustain),
        ("bass-punch", "mix") => Some(settings.bass_punch.mix),
        ("dimension", "mid_width") => Some(settings.dimension.mid_width),
        ("dimension", "decorrelation") => Some(settings.dimension.decorrelation),
        ("room", "size_m") => Some(settings.room.size_m),
        ("room", "distance_m") => Some(settings.room.distance_m),
        ("room", "ambience_mix") => Some(settings.room.ambience_mix),
        ("room", "walls") => Some(settings.room.walls),
        ("room", "ambience_decay_s") => Some(settings.room.ambience_decay_s),
        ("room", "crossover_hz") => Some(settings.room.crossover_hz),
        ("room", "centre_db") => Some(settings.room.centre_db),
        ("room", "sub_db") => Some(settings.room.sub_db),
        ("room", "early_reflection_db") => Some(settings.room.early_reflection_db),
        ("room", "ambience_damping_hz") => Some(settings.room.ambience_damping_hz),
        ("room", "music_upmix") => Some(settings.room.music_upmix as f64),
        ("room", "upmix_amount") => Some(settings.room.upmix_amount),
        ("dynamics", "band1_up_threshold_db") => Some(settings.compander.bands[0].up_threshold_db),
        ("dynamics", "band1_up_ratio") => Some(settings.compander.bands[0].up_ratio),
        ("dynamics", "band1_up_max_db") => Some(settings.compander.bands[0].up_max_db),
        ("dynamics", "band1_down_threshold_db") => {
            Some(settings.compander.bands[0].down_threshold_db)
        }
        ("dynamics", "band1_down_ratio") => Some(settings.compander.bands[0].down_ratio),
        ("dynamics", "band2_up_threshold_db") => Some(settings.compander.bands[1].up_threshold_db),
        ("dynamics", "band2_up_ratio") => Some(settings.compander.bands[1].up_ratio),
        ("dynamics", "band2_up_max_db") => Some(settings.compander.bands[1].up_max_db),
        ("dynamics", "band2_down_threshold_db") => {
            Some(settings.compander.bands[1].down_threshold_db)
        }
        ("dynamics", "band2_down_ratio") => Some(settings.compander.bands[1].down_ratio),
        ("dynamics", "band3_up_threshold_db") => Some(settings.compander.bands[2].up_threshold_db),
        ("dynamics", "band3_up_ratio") => Some(settings.compander.bands[2].up_ratio),
        ("dynamics", "band3_up_max_db") => Some(settings.compander.bands[2].up_max_db),
        ("dynamics", "band3_down_threshold_db") => {
            Some(settings.compander.bands[2].down_threshold_db)
        }
        ("dynamics", "band3_down_ratio") => Some(settings.compander.bands[2].down_ratio),
        ("dynamics", "attack_ms") => Some(settings.compander.attack_ms),
        ("dynamics", "release_ms") => Some(settings.compander.release_ms),
        ("dynamics", "mix") => Some(settings.compander.mix),
        ("dynamics", "makeup_db") => Some(settings.compander.makeup_db),
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
        "footsteps" => Some(settings.footsteps.enabled != 0),
        "bass-punch" => Some(settings.bass_punch.enabled != 0),
        "dimension" => Some(settings.dimension.enabled != 0),
        "room" => Some(settings.room.enabled != 0),
        "dynamics" => Some(settings.compander.enabled != 0),
        "maximizer" => Some(settings.maximizer.enabled != 0),
        "master" => Some(settings.master.enabled != 0),
        _ => None,
    }
}

/// One Dynamics profile, as the rack page's chips show it.
///
/// A profile owns the sound, never whether the stage runs: applying one to a
/// switched-off stage leaves it switched off, so auditioning the chips cannot
/// silently put a compressor into a chain that did not have one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DynamicsPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub bands: [FeqCompanderBand; FEQ_COMPANDER_BANDS],
    pub attack_ms: f64,
    pub release_ms: f64,
    pub makeup_db: f64,
}

const fn band(
    up_threshold_db: f64,
    up_ratio: f64,
    up_max_db: f64,
    down_threshold_db: f64,
    down_ratio: f64,
) -> FeqCompanderBand {
    FeqCompanderBand {
        up_threshold_db,
        up_ratio,
        up_max_db,
        down_threshold_db,
        down_ratio,
    }
}

/// A band that does nothing.
const OFF_BAND: FeqCompanderBand = band(-40.0, 1.0, 0.0, -20.0, 1.0);
/// Lift under `threshold` by `ratio`, at most `max` dB.
const fn up(threshold: f64, ratio: f64, max: f64) -> FeqCompanderBand {
    band(threshold, ratio, max, -20.0, 1.0)
}
/// Hold over `threshold` by `ratio`.
const fn down(threshold: f64, ratio: f64) -> FeqCompanderBand {
    band(-40.0, 1.0, 0.0, threshold, ratio)
}
/// Both at once, for the profiles that do both.
const fn up_down(
    up_threshold: f64,
    up_ratio: f64,
    up_max: f64,
    down_threshold: f64,
    down_ratio: f64,
) -> FeqCompanderBand {
    band(up_threshold, up_ratio, up_max, down_threshold, down_ratio)
}

/// The presets the rack page offers, ported from upstream's
/// `dynamicsPresets.ts` without the localisation keys (the labels here are
/// the English the rest of this window is written in).
pub const DYNAMICS_PRESETS: [DynamicsPreset; 14] = [
    DynamicsPreset {
        id: "gentle",
        label: "Gentle",
        group: "basic",
        bands: [
            up_down(-31.0, 1.4, 4.0, -19.0, 1.5),
            up_down(-27.0, 1.4, 4.0, -17.0, 1.5),
            up_down(-44.0, 1.4, 4.0, -30.0, 1.5),
        ],
        attack_ms: 15.0,
        release_ms: 300.0,
        makeup_db: 0.5,
    },
    DynamicsPreset {
        id: "transparent",
        label: "Transparent",
        group: "basic",
        bands: [down(-18.0, 1.3), down(-16.0, 1.2), down(-28.0, 1.3)],
        attack_ms: 30.0,
        release_ms: 400.0,
        makeup_db: 0.0,
    },
    DynamicsPreset {
        id: "punch",
        label: "Punch",
        group: "basic",
        bands: [down(-24.0, 2.5), down(-20.0, 1.8), down(-32.0, 1.5)],
        attack_ms: 40.0,
        release_ms: 100.0,
        makeup_db: 1.0,
    },
    DynamicsPreset {
        id: "acoustic",
        label: "Acoustic",
        group: "basic",
        bands: [OFF_BAND, up(-30.0, 1.3, 3.0), up(-46.0, 1.4, 3.0)],
        attack_ms: 35.0,
        release_ms: 450.0,
        makeup_db: 0.0,
    },
    DynamicsPreset {
        id: "vocal",
        label: "Vocal",
        group: "scene",
        bands: [
            down(-25.0, 2.0),
            up_down(-29.0, 1.6, 5.0, -18.0, 2.0),
            down(-32.0, 2.0),
        ],
        attack_ms: 12.0,
        release_ms: 220.0,
        makeup_db: 0.5,
    },
    DynamicsPreset {
        id: "warm",
        label: "Warm",
        group: "basic",
        bands: [up(-33.0, 1.5, 3.0), OFF_BAND, down(-35.0, 2.5)],
        attack_ms: 18.0,
        release_ms: 350.0,
        makeup_db: 0.0,
    },
    DynamicsPreset {
        id: "movie",
        label: "Movie",
        group: "scene",
        bands: [
            down(-22.0, 2.5),
            up_down(-30.0, 1.7, 6.0, -16.0, 1.8),
            up_down(-46.0, 1.3, 3.0, -29.0, 2.0),
        ],
        attack_ms: 10.0,
        release_ms: 300.0,
        makeup_db: 0.8,
    },
    DynamicsPreset {
        id: "lateNight",
        label: "Late night",
        group: "scene",
        bands: [
            down(-24.0, 4.0),
            up_down(-27.0, 2.0, 10.0, -19.0, 3.0),
            up_down(-42.0, 1.6, 6.0, -30.0, 3.0),
        ],
        attack_ms: 5.0,
        release_ms: 200.0,
        makeup_db: 2.3,
    },
    DynamicsPreset {
        id: "speech",
        label: "Speech",
        group: "scene",
        bands: [
            down(-26.0, 3.0),
            up_down(-27.0, 1.8, 8.0, -17.0, 2.5),
            up_down(-42.0, 1.5, 5.0, -28.0, 2.0),
        ],
        attack_ms: 8.0,
        release_ms: 250.0,
        makeup_db: 1.6,
    },
    DynamicsPreset {
        id: "gameQuietLift",
        label: "Quiet lift",
        group: "gaming",
        bands: [OFF_BAND, up(-26.0, 2.0, 10.0), up(-38.0, 2.0, 10.0)],
        attack_ms: 8.0,
        release_ms: 250.0,
        makeup_db: 0.0,
    },
    DynamicsPreset {
        id: "gameBlastGuard",
        label: "Blast guard",
        group: "gaming",
        bands: [down(-24.0, 4.0), down(-18.0, 2.5), OFF_BAND],
        attack_ms: 1.0,
        release_ms: 120.0,
        makeup_db: 1.6,
    },
    DynamicsPreset {
        id: "gameNight",
        label: "Night",
        group: "gaming",
        bands: [
            up_down(-28.0, 2.0, 10.0, -24.0, 6.0),
            up_down(-25.0, 2.0, 12.0, -20.0, 6.0),
            up_down(-38.0, 2.0, 12.0, -32.0, 6.0),
        ],
        attack_ms: 3.0,
        release_ms: 180.0,
        makeup_db: 2.9,
    },
    DynamicsPreset {
        id: "gameLongSession",
        label: "Long session",
        group: "gaming",
        bands: [
            up_down(-30.0, 1.5, 6.0, -24.0, 1.5),
            up_down(-26.0, 1.5, 6.0, -20.0, 1.5),
            up_down(-40.0, 1.5, 6.0, -32.0, 1.5),
        ],
        attack_ms: 40.0,
        release_ms: 600.0,
        makeup_db: 1.1,
    },
    DynamicsPreset {
        id: "gameCompetitiveLight",
        label: "Competitive",
        group: "gaming",
        bands: [down(-24.0, 3.0), OFF_BAND, up(-44.0, 1.6, 5.0)],
        attack_ms: 3.0,
        release_ms: 150.0,
        makeup_db: 0.6,
    },
];

pub fn dynamics_preset(id: &str) -> Option<&'static DynamicsPreset> {
    DYNAMICS_PRESETS.iter().find(|preset| preset.id == id.trim())
}

/// Adopt a profile's sound, leaving the stage's switch exactly as it was.
pub fn set_dynamics_preset(settings: &mut FeqChainSettings, id: &str) -> Result<(), String> {
    let preset = dynamics_preset(id)
        .ok_or_else(|| format!("there is no dynamics profile named \"{id}\""))?;
    settings.compander.bands = preset.bands;
    settings.compander.attack_ms = preset.attack_ms;
    settings.compander.release_ms = preset.release_ms;
    settings.compander.mix = 1.0;
    settings.compander.makeup_db = preset.makeup_db;
    Ok(())
}

/// The seven speakers around the listener, in the wire's order — front left,
/// front right, centre, side left, side right, rear left, rear right — plus
/// the subwoofer, which has no angle or distance and shares the mute list.
pub const ROOM_SPEAKERS: u32 = 7;
pub const ROOM_SUBWOOFER: u32 = 7;

/// Move one speaker. `parameter` is `angle_deg`, `distance_m`, `level_db` or
/// `mute`; the subwoofer accepts only `level_db` and `mute`. Every move makes
/// the room `custom`, because a speaker's position is part of its shape.
pub fn set_room_speaker(
    settings: &mut FeqChainSettings,
    speaker: u32,
    parameter: &str,
    value: f64,
) -> Result<(), String> {
    if speaker > ROOM_SUBWOOFER {
        return Err(format!("there is no speaker number {speaker}"));
    }
    let index = speaker as usize;
    let is_sub = speaker == ROOM_SUBWOOFER;
    let (low, high) = match parameter {
        "mute" => (0.0, 1.0),
        "level_db" if is_sub => (-12.0, 12.0),
        "level_db" => (-24.0, 12.0),
        _ if is_sub => {
            return Err(String::from(
                "the subwoofer has a level and a mute, no angle or distance",
            ))
        }
        "angle_deg" => (-180.0, 180.0),
        "distance_m" => (0.5, 6.0),
        _ => return Err(format!("there is no speaker control named \"{parameter}\"")),
    };
    let value = value.clamp(low, high);
    match parameter {
        "angle_deg" => settings.room.angle_deg[index] = value,
        "distance_m" => settings.room.speaker_distance_m[index] = value,
        "level_db" if is_sub => settings.room.sub_db = value,
        "level_db" => settings.room.level_db[index] = value,
        "mute" => settings.room.mute[index] = if value >= 0.5 { 1 } else { 0 },
        _ => unreachable!("the range table above accepts every name that reaches here"),
    }
    settings.room.preset = crate::room_catalog::CUSTOM_PRESET;
    Ok(())
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
    fn moving_a_room_dial_marks_the_room_custom() {
        let mut settings = FeqChainSettings::default();
        settings.room.preset = 12;
        set_stage_parameter(&mut settings, "room", "walls", 0.5).expect("walls");
        assert_eq!(settings.room.preset, crate::room_catalog::CUSTOM_PRESET);

        // A dial on another stage leaves the room's name alone.
        settings.room.preset = 12;
        set_stage_parameter(&mut settings, "normalizer", "target_lufs", -20.0).expect("lufs");
        assert_eq!(settings.room.preset, 12);
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

    #[test]
    fn difference_monitors_round_trip_only_where_they_exist() {
        let mut settings = FeqChainSettings::default();
        for stage in [
            "denoise",
            "deesser",
            "exciter",
            "equaliser",
            "footsteps",
            "bass-forge",
            "bass-punch",
        ] {
            assert!(stage_supports_isolate(stage), "{stage}");
            set_stage_isolate(&mut settings, stage, true).expect("solo");
            assert_eq!(stage_isolate(&settings, stage), Some(true), "{stage}");
            set_stage_isolate(&mut settings, stage, false).expect("off");
            assert_eq!(stage_isolate(&settings, stage), Some(false), "{stage}");
        }
        assert!(!stage_supports_isolate("room"));
        assert!(set_stage_isolate(&mut settings, "room", true).is_err());
        assert_eq!(stage_isolate(&settings, "room"), None);
    }

    #[test]
    fn normalizer_modes_name_the_same_values_the_switch_uses() {
        let mut settings = FeqChainSettings::default();
        set_normalizer_mode(&mut settings, "leveler").expect("leveler");
        assert_eq!(normalizer_mode_name(&settings), "leveler");
        assert_eq!(stage_enabled(&settings, "normalizer"), Some(true));
        set_normalizer_mode(&mut settings, "guard").expect("guard");
        assert_eq!(normalizer_mode_name(&settings), "guard");
        set_normalizer_mode(&mut settings, "off").expect("off");
        assert_eq!(normalizer_mode_name(&settings), "off");
        assert_eq!(stage_enabled(&settings, "normalizer"), Some(false));
        assert!(set_normalizer_mode(&mut settings, "loud").is_err());
    }

    #[test]
    fn delivery_targets_move_loudness_and_ceiling_together() {
        let mut settings = FeqChainSettings::default();
        set_master_target(&mut settings, "streaming").expect("streaming");
        assert_eq!(settings.master.loudness_target_lufs, -14.0);
        assert_eq!(settings.master.ceiling_db, -1.0);
        set_master_target(&mut settings, "cinema").expect("cinema");
        assert_eq!(settings.master.loudness_target_lufs, -24.0);
        assert_eq!(settings.master.ceiling_db, -2.0);
        assert!(set_master_target(&mut settings, "vinyl").is_err());
    }

    #[test]
    fn moving_a_speaker_marks_the_room_custom_and_clamps() {
        let mut settings = FeqChainSettings::default();
        settings.room.preset = 12;

        set_room_speaker(&mut settings, 0, "angle_deg", -400.0).expect("angle");
        assert_eq!(settings.room.angle_deg[0], -180.0);
        set_room_speaker(&mut settings, 2, "distance_m", 0.0).expect("distance");
        assert_eq!(settings.room.speaker_distance_m[2], 0.5);
        set_room_speaker(&mut settings, 1, "level_db", 99.0).expect("level");
        assert_eq!(settings.room.level_db[1], 12.0);
        set_room_speaker(&mut settings, 3, "mute", 0.7).expect("mute");
        assert_eq!(settings.room.mute[3], 1);
        set_room_speaker(&mut settings, ROOM_SUBWOOFER, "level_db", -99.0).expect("sub level");
        assert_eq!(settings.room.sub_db, -12.0);
        set_room_speaker(&mut settings, ROOM_SUBWOOFER, "mute", 1.0).expect("sub mute");
        assert_eq!(settings.room.mute[ROOM_SUBWOOFER as usize], 1);
        assert_eq!(settings.room.preset, crate::room_catalog::CUSTOM_PRESET);

        assert!(set_room_speaker(&mut settings, 8, "mute", 1.0).is_err());
        assert!(set_room_speaker(&mut settings, ROOM_SUBWOOFER, "angle_deg", 0.0).is_err());
        assert!(set_room_speaker(&mut settings, 0, "pan", 0.0).is_err());
    }

    #[test]
    fn every_dynamics_profile_applies_without_touching_the_switch() {
        for preset in DYNAMICS_PRESETS {
            let mut settings = FeqChainSettings::default();
            settings.compander.enabled = 1;
            set_dynamics_preset(&mut settings, preset.id).expect(preset.id);
            assert_eq!(settings.compander.bands, preset.bands, "{}", preset.id);
            assert_eq!(settings.compander.attack_ms, preset.attack_ms, "{}", preset.id);
            assert_eq!(settings.compander.release_ms, preset.release_ms, "{}", preset.id);
            assert_eq!(
                settings.compander.makeup_db,
                preset.makeup_db,
                "{}",
                preset.id
            );
            assert_eq!(settings.compander.mix, 1.0, "{}", preset.id);
            assert_eq!(settings.compander.enabled, 1, "{}", preset.id);

            settings.compander.enabled = 0;
            set_dynamics_preset(&mut settings, preset.id).expect(preset.id);
            assert_eq!(settings.compander.enabled, 0, "{}", preset.id);
        }
        assert!(set_dynamics_preset(&mut FeqChainSettings::default(), "loud").is_err());
    }
}
