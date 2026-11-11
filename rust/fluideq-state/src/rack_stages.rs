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
    fn an_unknown_stage_is_refused_rather_than_ignored() {
        let mut settings = FeqChainSettings::default();
        assert!(set_stage_enabled(&mut settings, "crossfade", true).is_err());
        assert_eq!(stage_enabled(&settings, "crossfade"), None);
    }
}
