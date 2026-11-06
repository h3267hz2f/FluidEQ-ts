/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Per-Output-Device Profile Registry (`device_profiles.rs`).
//!
//! Ensures FluidEQ follows the output Windows plays through: when a headset or USB DAC
//! is plugged in and becomes the default render endpoint (`OutputWatchCoordinator`),
//! the outgoing endpoint's EQ bands, preamp, tone shelves, and 11-stage rack settings
//! are saved under its endpoint GUID, and the newly active endpoint's profile is
//! automatically restored (or initialized if seen for the first time).

use crate::biquad::EqBandSpec;
use crate::ToneSettings;
use fluideq_dsp_sys::FeqChainSettings;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceEndpointProfile {
    pub guid: String,
    pub device_name: String,
    pub active_preset_name: String,
    pub preamp_db: f64,
    pub matched_treble: bool,
    pub eq_model_amount: f64,
    pub tone: ToneSettings,
    pub bands: Vec<EqBandSpec>,
    pub chain_settings: FeqChainSettings,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceProfileRegistry {
    pub active_device_guid: Option<String>,
    pub active_device_name: Option<String>,
    pub profiles: BTreeMap<String, DeviceEndpointProfile>,
}

pub fn normalize_endpoint_guid(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(idx) = trimmed.rfind('{') {
        trimmed[idx..].to_ascii_lowercase()
    } else {
        trimmed.to_ascii_lowercase()
    }
}

impl DeviceProfileRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Saves the current live state under `self.active_device_guid` (if any),
    /// then switches to `new_guid` and returns the stored `DeviceEndpointProfile`
    /// if `new_guid` was previously configured.
    pub fn switch_device(
        &mut self,
        new_guid: &str,
        new_device_name: &str,
        current_preset_name: &str,
        current_preamp_db: f64,
        current_matched_treble: bool,
        current_eq_model_amount: f64,
        current_tone: &ToneSettings,
        current_bands: &[EqBandSpec],
        current_chain: &FeqChainSettings,
    ) -> Option<DeviceEndpointProfile> {
        let normalized_new = normalize_endpoint_guid(new_guid);
        if normalized_new.is_empty() {
            return None;
        }

        // Save outgoing device state first
        if let Some(prev_guid) = self.active_device_guid.clone() {
            let prev_name = self
                .active_device_name
                .clone()
                .unwrap_or_else(|| prev_guid.clone());
            self.profiles.insert(
                prev_guid.clone(),
                DeviceEndpointProfile {
                    guid: prev_guid,
                    device_name: prev_name,
                    active_preset_name: current_preset_name.to_string(),
                    preamp_db: current_preamp_db,
                    matched_treble: current_matched_treble,
                    eq_model_amount: current_eq_model_amount,
                    tone: current_tone.clone(),
                    bands: current_bands.to_vec(),
                    chain_settings: *current_chain,
                },
            );
        }

        self.active_device_guid = Some(normalized_new.clone());
        self.active_device_name = Some(new_device_name.trim().to_string());

        if let Some(existing) = self.profiles.get(&normalized_new).cloned() {
            Some(existing)
        } else {
            // Seed the newly discovered device with the current state
            let seeded = DeviceEndpointProfile {
                guid: normalized_new.clone(),
                device_name: new_device_name.trim().to_string(),
                active_preset_name: current_preset_name.to_string(),
                preamp_db: current_preamp_db,
                matched_treble: current_matched_treble,
                eq_model_amount: current_eq_model_amount,
                tone: current_tone.clone(),
                bands: current_bands.to_vec(),
                chain_settings: *current_chain,
            };
            self.profiles.insert(normalized_new, seeded);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_between_speakers_and_headset_restores_each_devices_eq() {
        let mut reg = DeviceProfileRegistry::new();
        let tone = ToneSettings::default();
        let chain = FeqChainSettings::default();

        // 1. Bind initial Speakers endpoint
        let _ = reg.switch_device(
            "{11111111-1111-1111-1111-111111111111}",
            "Desktop Speakers",
            "Speakers Warm",
            -1.5,
            true,
            1.0,
            &tone,
            &[],
            &chain,
        );

        // 2. Plug in USB Headset -> saves Speakers ("Speakers Warm", -1.5 dB) and seeds Headset
        let restored_headset = reg.switch_device(
            "{22222222-2222-2222-2222-222222222222}",
            "Sennheiser HD 600 USB",
            "Speakers Warm",
            -1.5,
            true,
            1.0,
            &tone,
            &[],
            &chain,
        );
        assert!(restored_headset.is_none());

        // 3. Unplug USB Headset -> switch back to Speakers while Headset had "HD 600 Harman" (-5.2 dB)
        let restored_speakers = reg
            .switch_device(
                "{11111111-1111-1111-1111-111111111111}",
                "Desktop Speakers",
                "HD 600 Harman",
                -5.2,
                true,
                1.0,
                &tone,
                &[],
                &chain,
            )
            .expect("Speakers profile should be restored");
        assert_eq!(restored_speakers.active_preset_name, "Speakers Warm");
        assert_eq!(restored_speakers.preamp_db, -1.5);

        // 4. Plug USB Headset back in -> restores "HD 600 Harman" (-5.2 dB)
        let restored_headset_again = reg
            .switch_device(
                "{22222222-2222-2222-2222-222222222222}",
                "Sennheiser HD 600 USB",
                &restored_speakers.active_preset_name,
                restored_speakers.preamp_db,
                true,
                1.0,
                &tone,
                &[],
                &chain,
            )
            .expect("Headset profile should be restored");
        assert_eq!(restored_headset_again.active_preset_name, "HD 600 Harman");
        assert_eq!(restored_headset_again.preamp_db, -5.2);
    }
}
