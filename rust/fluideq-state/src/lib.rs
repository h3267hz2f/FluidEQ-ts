/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Authoritative Rust State Store (`AppStateStore`) for FluidEQ 2.1.
//!
//! Keeps the complete 11-stage DSP rack (`FeqChainSettings`), 15/64-band EQ,
//! linear-phase 16,384-tap FIR synthesis, RIFF/WAVE convolution IR loader,
//! on-demand sharded OPRA headphone database cache, Tone shelves, 7.1 Room layout &
//! KEMAR HRTF head impulse banks, atomic disk persistence, and
//! Cold-Residency state in native Rust memory so the frontend WebView can
//! be destroyed on tray minimize and hydrated in < 1 ms on wake.

pub mod apo;
pub mod apo_analysis_and_status;
pub mod apo_engine_runtime;
pub mod apo_graph_and_guard;
pub mod apo_process_and_stages;
pub mod apo_setup_commands;
pub mod apo_setup_registry;
pub mod apo_setup_services_and_fs;
pub mod apo_split_and_leveling;
pub mod apo_split_tap_and_reader;
pub mod apo_watcher;
pub mod biquad;
pub mod chain_wire;
pub mod compressed_audio;
pub mod deck_transport;
pub mod device_profiles;
pub mod linear_phase;
pub mod models;
pub mod opra;
pub mod persistence;
pub mod preset_catalog;
pub mod presets;
pub mod rack_stages;
pub mod remote_audio_runtime;
pub mod remote_playback;
pub mod residency;
pub mod room_catalog;
pub mod room_heads;
pub mod wav_ir;

use biquad::{biquad_coefficients_designed, compute_auto_preamp_db, EqBandSpec};
use chain_wire::{decode_chain_settings_wire, encode_chain_settings_wire};
use deck_transport::{decode_noise_profile_wire, DeckLoadOutcome, DualDeckCoordinator};
use device_profiles::{DeviceEndpointProfile, DeviceProfileRegistry};
use fluideq_dsp_sys::{
    FeqChainEqBand, FeqChainSettings, FeqEqModel, FeqFilterType, FeqPhaseMode, NativeChainHandle,
    NativeMetersHandle, FEQ_CHAIN_MAX_EQ_BANDS,
};
use linear_phase::{
    synthesize_linear_phase_change_kernel, synthesize_stereo_linear_phase_kernels,
};
use models::{
    estimate_vocal_pitch_yin, verify_onnx_model_file, NeuralModelStatus, PitchEstimate,
};
use opra::OpraShardCache;
use persistence::{load_snapshot_or_recover, save_snapshot_atomic};
use residency::{ColdResidencyController, MemoryFootprintEstimate, WindowResidencyMode};
use room_heads::RoomHeadBank;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use wav_ir::{decode_pcm_impulse_response, DecodedImpulseResponse, IrSummary};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToneSettings {
    pub bass_db: f64,
    pub bass_freq_hz: f64,
    pub bass_q: f64,
    pub treble_db: f64,
    pub treble_freq_hz: f64,
    pub treble_q: f64,
    pub low_cut_hz: f64,
    pub high_cut_hz: f64,
}

impl Default for ToneSettings {
    fn default() -> Self {
        Self {
            bass_db: 0.0,
            bass_freq_hz: 105.0,
            bass_q: 0.71,
            treble_db: 0.0,
            treble_freq_hz: 8500.0,
            treble_q: 0.71,
            low_cut_hz: 20.0,
            high_cut_hz: 20000.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppStateSnapshot {
    pub revision: u64,
    pub master_power: bool,
    pub auto_headroom: bool,
    /// Whether the window's close button hides it to the tray instead of
    /// quitting. Defaults to on, so an old state file keeps the current habit.
    #[serde(default = "default_close_to_tray")]
    pub close_to_tray: bool,
    pub preamp_db: f64,
    pub auto_preamp_db: f64,
    #[serde(default)]
    pub preamp_total_db: f64,
    pub active_preset_name: String,
    #[serde(default)]
    pub active_device_guid: Option<String>,
    #[serde(default)]
    pub active_device_name: Option<String>,
    pub eq_model: String,
    pub eq_model_amount: f64,
    pub matched_treble: bool,
    pub tone: ToneSettings,
    pub bands: Vec<EqBandSpec>,
    #[serde(default)]
    pub eq_curve_db: Vec<f64>,
    #[serde(default)]
    pub native_abi_version: String,
    #[serde(default)]
    pub native_active_stages: u32,
    #[serde(default)]
    pub native_latency_frames: u32,
    #[serde(default)]
    pub native_working_set_bytes: u64,
    pub chain_settings: FeqChainSettings,
    pub models: NeuralModelStatus,
    /// Every endpoint's saved EQ and rack, so switching back after a restart
    /// restores the device instead of starting it from the current curve.
    #[serde(default)]
    pub device_profiles: DeviceProfileRegistry,
    pub residency: MemoryFootprintEstimate,
    /// The impulse response installed in this process. Sent to the window, but
    /// not restored from disk: a restart must not claim a convolution is
    /// running when the kernel is empty.
    #[serde(default, skip_deserializing)]
    pub impulse_response: Option<IrSummary>,
}

/// The gain the chain actually plays at: the headroom the curve needs plus the
/// user's own trim. The window shows this sum, not the trim alone, because a
/// -24 dB trim under a -38 dB guard read as a 24 dB cut while the chain sat at
/// -62 dB and looked broken.
fn default_close_to_tray() -> bool {
    true
}

pub fn total_preamp_db(manual_db: f64, auto_db: f64, auto_headroom: bool) -> f64 {
    manual_db + if auto_headroom { auto_db.min(0.0) } else { 0.0 }
}

pub struct AppStateStore {
    revision: u64,
    sample_rate: f64,
    master_power: bool,
    auto_headroom: bool,
    close_to_tray: bool,
    preamp_db: f64,
    active_preset_name: String,
    eq_model: FeqEqModel,
    eq_model_amount: f64,
    matched_treble: bool,
    tone: ToneSettings,
    bands: Vec<EqBandSpec>,
    chain_settings: FeqChainSettings,
    device_profiles: DeviceProfileRegistry,
    deck_transport: DualDeckCoordinator,
    room_head_banks: BTreeMap<String, RoomHeadBank>,
    opra_cache: OpraShardCache,
    models: NeuralModelStatus,
    residency: ColdResidencyController,
    native_chain: NativeChainHandle,
    native_meters: NativeMetersHandle,
    impulse_response: Option<IrSummary>,
    ir_kernel: Option<(Vec<f32>, Vec<f32>)>,
}

impl Default for AppStateStore {
    fn default() -> Self {
        Self::new(48_000.0)
    }
}

impl AppStateStore {
    pub fn new(sample_rate: f64) -> Self {
        let balanced = presets::EqPreset::balanced();
        let mut store = Self {
            revision: 1,
            sample_rate,
            master_power: true,
            auto_headroom: true,
            close_to_tray: true,
            preamp_db: 0.0,
            active_preset_name: balanced.name.clone(),
            eq_model: Self::parse_eq_model(&balanced.eq_model),
            eq_model_amount: balanced.eq_model_amount,
            matched_treble: balanced.matched_treble,
            tone: balanced.tone.clone(),
            bands: balanced.bands.clone(),
            chain_settings: FeqChainSettings::default(),
            device_profiles: DeviceProfileRegistry::new(),
            deck_transport: DualDeckCoordinator::new(),
            room_head_banks: BTreeMap::new(),
            opra_cache: OpraShardCache::new(),
            models: NeuralModelStatus::default(),
            residency: ColdResidencyController::new(),
            native_chain: NativeChainHandle::new(sample_rate as u32, 2, 2048),
            native_meters: NativeMetersHandle::new(2, sample_rate),
            impulse_response: None,
            ir_kernel: None,
        };
        store.chain_settings.eq.subsonic_hz = balanced.subsonic_hz;
        store.chain_settings.eq.mono_below_hz = balanced.mono_below_hz;
        store.chain_settings.eq.fuzz_amount = balanced.fuzz_amount;
        store.sync_eq_and_tone_into_chain_settings();
        store.commit_chain_and_linear_phase_kernel();
        store
    }

    fn eq_model_name(&self) -> &'static str {
        match self.eq_model {
            FeqEqModel::FEQ_EQ_MODEL_PROPORTIONAL => "proportional",
            FeqEqModel::FEQ_EQ_MODEL_WIDE => "wide",
            FeqEqModel::FEQ_EQ_MODEL_ASYMMETRIC => "asymmetric",
            FeqEqModel::FEQ_EQ_MODEL_CLEAN => "clean",
        }
    }

    fn parse_eq_model(name: &str) -> FeqEqModel {
        match name.trim().to_ascii_lowercase().as_str() {
            "proportional" | "feq_eq_model_proportional" => FeqEqModel::FEQ_EQ_MODEL_PROPORTIONAL,
            "wide" | "feq_eq_model_wide" => FeqEqModel::FEQ_EQ_MODEL_WIDE,
            "asymmetric" | "feq_eq_model_asymmetric" => FeqEqModel::FEQ_EQ_MODEL_ASYMMETRIC,
            _ => FeqEqModel::FEQ_EQ_MODEL_CLEAN,
        }
    }

    pub fn sanitize_tone(mut tone: ToneSettings) -> ToneSettings {
        tone.bass_db = if tone.bass_db.is_finite() {
            tone.bass_db.clamp(-12.0, 12.0)
        } else {
            0.0
        };
        tone.bass_freq_hz = if tone.bass_freq_hz.is_finite() && tone.bass_freq_hz > 10.0 {
            tone.bass_freq_hz.clamp(20.0, 500.0)
        } else {
            105.0
        };
        tone.bass_q = if tone.bass_q.is_finite() && tone.bass_q > 0.05 {
            tone.bass_q.clamp(0.1, 10.0)
        } else {
            0.71
        };
        tone.treble_db = if tone.treble_db.is_finite() {
            tone.treble_db.clamp(-12.0, 12.0)
        } else {
            0.0
        };
        tone.treble_freq_hz = if tone.treble_freq_hz.is_finite() && tone.treble_freq_hz > 500.0 {
            tone.treble_freq_hz.clamp(1000.0, 20000.0)
        } else {
            8500.0
        };
        tone.treble_q = if tone.treble_q.is_finite() && tone.treble_q > 0.05 {
            tone.treble_q.clamp(0.1, 10.0)
        } else {
            0.71
        };
        tone.low_cut_hz = if tone.low_cut_hz.is_finite() {
            tone.low_cut_hz.clamp(20.0, 250.0)
        } else {
            20.0
        };
        tone.high_cut_hz = if tone.high_cut_hz.is_finite() {
            tone.high_cut_hz.clamp(4000.0, 20000.0)
        } else {
            20000.0
        };
        tone
    }

    /// Synchronizes the high-level `bands` and `tone` state into `self.chain_settings`
    /// so `FeqChainSettings` is always ready for `feq_chain_configure` or wire encoding.
    fn sync_eq_and_tone_into_chain_settings(&mut self) {
        self.chain_settings.enabled = if self.master_power { 1 } else { 0 };
        self.chain_settings.eq.matched = if self.matched_treble { 1 } else { 0 };
        self.chain_settings.eq.model = self.eq_model as i32;
        self.chain_settings.eq.model_amount = self.eq_model_amount;

        let count = self.bands.len().min(FEQ_CHAIN_MAX_EQ_BANDS);
        self.chain_settings.eq.band_count = count as u32;
        for (idx, b) in self.bands.iter().take(count).enumerate() {
            let dst = &mut self.chain_settings.eq.bands[idx];
            dst.enabled = if b.enabled { 1 } else { 0 };
            dst.ear = b.ear as i32;
            dst.filter_type = b.filter_type.to_feq_type();
            dst.frequency = b.frequency;
            dst.gain_db = b.gain_db;
            dst.quality = b.quality;
            dst.dynamic = if b.dynamic { 1 } else { 0 };
            dst.threshold_db = b.threshold_db;
        }

        // Populate Tone trailer bands (Bass shelf + Treble shelf)
        self.chain_settings.tone.matched = if self.matched_treble { 1 } else { 0 };
        self.chain_settings.tone.band_count = 2;
        self.chain_settings.tone.bands[0].filter_type = FeqFilterType::FEQ_FILTER_LSC;
        self.chain_settings.tone.bands[0].frequency = self.tone.bass_freq_hz;
        self.chain_settings.tone.bands[0].gain_db = self.tone.bass_db;
        self.chain_settings.tone.bands[0].quality = self.tone.bass_q;

        self.chain_settings.tone.bands[1].filter_type = FeqFilterType::FEQ_FILTER_HSC;
        self.chain_settings.tone.bands[1].frequency = self.tone.treble_freq_hz;
        self.chain_settings.tone.bands[1].gain_db = self.tone.treble_db;
        self.chain_settings.tone.bands[1].quality = self.tone.treble_q;
    }

    /// Commits `self.chain_settings` to `NativeChainHandle` and automatically synthesizes
    /// the 16,384-tap stereo linear-phase FIR kernel when `eq.phase == FEQ_PHASE_LINEAR`.
    fn commit_chain_and_linear_phase_kernel(&mut self) {
        if let Some((left, right)) = &self.ir_kernel {
            // A loaded impulse response owns the kernel stage. Configure first,
            // because the EQ's own linear-phase refresh runs inside it and would
            // otherwise clear or replace the IR on every band change.
            let _ = self.native_chain.configure(&self.chain_settings);
            let _ = self.native_chain.set_eq_kernel(left, right);
            return;
        }
        if self.chain_settings.eq.phase == FeqPhaseMode::FEQ_PHASE_LINEAR as i32 {
            let (left, right) =
                synthesize_stereo_linear_phase_kernels(&self.chain_settings.eq, self.sample_rate);
            let _ = self.native_chain.set_eq_kernel(&left, &right);
        } else if self.native_chain.eq_kernel_len() != 0 {
            let _ = self.native_chain.set_eq_kernel(&[], &[]);
        }
        let _ = self.native_chain.configure(&self.chain_settings);
    }

    pub fn update_band_gain(
        &mut self,
        band_id: u32,
        frequency: f64,
        gain_db: f64,
        quality: f64,
    ) -> Result<(), String> {
        let Some(band) = self.bands.iter_mut().find(|b| b.id == band_id) else {
            return Err(format!("there is no band numbered {band_id}"));
        };
        band.frequency = frequency.clamp(10.0, 20000.0);
        band.gain_db = gain_db.clamp(-24.0, 24.0);
        band.quality = quality.clamp(0.1, 18.0);
        self.revision += 1;
        self.sync_eq_and_tone_into_chain_settings();
        self.commit_chain_and_linear_phase_kernel();
        Ok(())
    }

    /// Sets a band's on/bypass state, filter shape and stereo routing.
    pub fn update_band_config(
        &mut self,
        band_id: u32,
        enabled: bool,
        filter_type: &str,
        ear: u8,
    ) -> Result<AppStateSnapshot, String> {
        let kind = crate::biquad::BandFilterKind::from_name(filter_type)
            .ok_or_else(|| format!("there is no filter type named \"{filter_type}\""))?;
        let Some(band) = self.bands.iter_mut().find(|b| b.id == band_id) else {
            return Err(format!("there is no band numbered {band_id}"));
        };
        band.enabled = enabled;
        band.filter_type = kind;
        band.ear = ear.min(2);
        self.revision += 1;
        self.sync_eq_and_tone_into_chain_settings();
        self.commit_chain_and_linear_phase_kernel();
        Ok(self.snapshot())
    }

    /// Synchronizes full EQ bands, character model, analog-matched treble mode, power, preamp,
    /// and Tone settings from the UI into the native Rust + C++20 state store.
    #[allow(clippy::too_many_arguments)]
    pub fn set_eq_bands_and_config(
        &mut self,
        bands: Vec<EqBandSpec>,
        eq_model: &str,
        matched_treble: bool,
        master_power: bool,
        auto_headroom: bool,
        manual_preamp_db: f64,
        tone: ToneSettings,
    ) -> AppStateSnapshot {
        self.bands = bands
            .into_iter()
            .take(FEQ_CHAIN_MAX_EQ_BANDS)
            .enumerate()
            .map(|(idx, mut b)| {
                b.id = (idx + 1) as u32;
                b.frequency = b.frequency.clamp(10.0, 20000.0);
                b.gain_db = b.gain_db.clamp(-24.0, 24.0);
                b.quality = b.quality.clamp(0.1, 18.0);
                b
            })
            .collect();
        self.eq_model = Self::parse_eq_model(eq_model);
        self.matched_treble = matched_treble;
        self.master_power = master_power;
        self.auto_headroom = auto_headroom;
        self.preamp_db = manual_preamp_db.clamp(-24.0, 24.0);
        self.tone = Self::sanitize_tone(tone);
        self.revision += 1;
        self.sync_eq_and_tone_into_chain_settings();
        self.commit_chain_and_linear_phase_kernel();
        self.snapshot()
    }

    /// Updates the Tone shelves, low/high cut corners, EQ character model, model amount,
    /// and analog-matched treble mode in one atomic step.
    pub fn set_tone_and_voicing(
        &mut self,
        tone: ToneSettings,
        eq_model: &str,
        eq_model_amount: f64,
        matched_treble: bool,
    ) -> AppStateSnapshot {
        self.tone = Self::sanitize_tone(tone);
        self.eq_model = Self::parse_eq_model(eq_model);
        self.eq_model_amount = if eq_model_amount.is_finite() {
            eq_model_amount.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.matched_treble = matched_treble;
        self.revision += 1;
        self.sync_eq_and_tone_into_chain_settings();
        self.commit_chain_and_linear_phase_kernel();
        self.snapshot()
    }

    /// Discovers all vendor slugs available in `opra_curves_dir` (including built-in fallbacks).
    pub fn list_opra_vendors(&self, opra_curves_dir: &Path) -> Vec<String> {
        let mut vendors = OpraShardCache::discover_vendor_slugs_in_dir(opra_curves_dir);
        for fallback in ["sennheiser", "sony", "hifiman", "moondrop"] {
            if !vendors.iter().any(|v| v.eq_ignore_ascii_case(fallback)) {
                vendors.push(fallback.to_string());
            }
        }
        vendors.sort();
        vendors
    }

    /// Lists model slugs available for `vendor_slug`.
    pub fn list_opra_models(
        &mut self,
        opra_curves_dir: &Path,
        vendor_slug: &str,
    ) -> Result<Vec<String>, String> {
        self.opra_cache.list_products_for_vendor(opra_curves_dir, vendor_slug)
    }

    /// Imports an Equalizer APO / Squiglink text, replacing the bands that
    /// parsed and keeping the rest of the chain as it was. A text with no
    /// `Filter:` lines is refused rather than silently wiping the curve.
    pub fn import_apo_text(&mut self, text: &str) -> Result<AppStateSnapshot, String> {
        let (preamp, bands) = crate::apo::parse_apo_curve(text);
        if bands.is_empty() {
            return Err("no Filter lines were found in that text".to_string());
        }
        let current = self.snapshot();
        Ok(self.set_eq_bands_and_config(
            bands,
            &current.eq_model,
            current.matched_treble,
            current.master_power,
            current.auto_headroom,
            preamp.unwrap_or(current.preamp_db),
            current.tone,
        ))
    }

    /// Renders the current curve as Equalizer APO text (preamp plus filters).
    pub fn export_apo_text(&self) -> String {
        crate::apo::format_bands_as_apo_lines(self.preamp_db, &self.bands)
    }

    /// Resolves and applies an OPRA headphone curve from `/opra/curves/<vendor_slug>.json`
    /// (or the D–Z flagship fallback catalog), updating `bands`, `preamp_db`, and `active_preset_name`.
    pub fn apply_opra_headphone_curve(
        &mut self,
        opra_curves_dir: &Path,
        vendor_slug: &str,
        product_slug: &str,
        curve_id: Option<&str>,
    ) -> Result<String, String> {
        let (resolved_id, preamp_db, bands) =
            self.opra_cache
                .resolve_curve(opra_curves_dir, vendor_slug, product_slug, curve_id)?;
        self.residency
            .record_opra_cache(self.opra_cache.cached_vendors_count());
        self.preamp_db = preamp_db;
        self.bands = bands;
        self.active_preset_name = resolved_id.clone();
        self.revision += 1;
        self.sync_eq_and_tone_into_chain_settings();
        self.commit_chain_and_linear_phase_kernel();
        Ok(resolved_id)
    }

    /// Decodes a RIFF/WAVE (`.wav`) or FORM/AIFF(-C) (`.aif`/`.aiff`) impulse response byte slice
    /// and installs it into the native convolution stage (`feq_chain_set_eq_kernel`).
    pub fn load_wav_impulse_response(&mut self, wav_bytes: &[u8]) -> Result<DecodedImpulseResponse, String> {
        let decoded = decode_pcm_impulse_response(wav_bytes)?;
        if !self.native_chain.set_eq_kernel(&decoded.left, &decoded.right) {
            return Err("the native engine could not install the impulse response".to_string());
        }
        self.ir_kernel = Some((decoded.left.clone(), decoded.right.clone()));
        self.impulse_response = Some(IrSummary::from_decoded(&decoded));
        self.revision += 1;
        Ok(decoded)
    }

    /// Removes the convolution kernel. The window keeps the summary in the
    /// snapshot, so this clears it too rather than leaving a stale "loaded".
    pub fn clear_impulse_response(&mut self) {
        self.ir_kernel = None;
        self.impulse_response = None;
        self.revision += 1;
        // Put back whatever the EQ itself wanted: a linear-phase kernel, or none.
        self.commit_chain_and_linear_phase_kernel();
    }

    pub fn impulse_response(&self) -> Option<&IrSummary> {
        self.impulse_response.as_ref()
    }

    /// The current curve, headroom and voicing as a named preset.
    pub fn current_preset(&self, name: &str) -> presets::EqPreset {
        presets::EqPreset {
            name: name.trim().to_string(),
            group: "custom".to_string(),
            preamp_db: self.preamp_db,
            auto_headroom: self.auto_headroom,
            eq_model: self.eq_model_name().to_string(),
            eq_model_amount: self.eq_model_amount,
            matched_treble: self.matched_treble,
            subsonic_hz: self.chain_settings.eq.subsonic_hz,
            mono_below_hz: self.chain_settings.eq.mono_below_hz,
            fuzz_amount: self.chain_settings.eq.fuzz_amount,
            tone: self.tone.clone(),
            bands: self.bands.clone(),
            chain_settings: Some(self.chain_settings),
        }
    }

    /// Replaces the editable curve with a preset and keeps the master switch
    /// where the user left it: a preset is an EQ, not a power state.
    pub fn apply_preset(&mut self, preset: &presets::EqPreset) -> AppStateSnapshot {
        let master_power = self.master_power;
        self.set_eq_bands_and_config(
            preset.bands.clone(),
            &preset.eq_model,
            preset.matched_treble,
            master_power,
            preset.auto_headroom,
            preset.preamp_db,
            preset.tone.clone(),
        );
        // The preset saves the character amount too, so a preset applied in a
        // session that had it at another value must put it back.
        self.eq_model_amount = preset.eq_model_amount.clamp(0.0, 1.0);
        self.active_preset_name = preset.name.clone();
        let is_factory = preset.group != "custom";
        // A preset saved by this build carries the rack and Room as well, so
        // applying it restores the whole chain and not only the curve. The
        // master switch stays where the user left it: a preset is a sound, not
        // a power state.
        if let Some(mut chain) = preset.chain_settings {
            chain.enabled = if master_power { 1 } else { 0 };
            self.update_chain_settings(chain);
        }
        // A factory curve carries the voicing it was fitted on. A saved file
        // carries its whole chain instead, and must not reset these from a
        // field the user never edited.
        if is_factory {
            self.chain_settings.eq.subsonic_hz = preset.subsonic_hz.max(0.0);
            self.chain_settings.eq.mono_below_hz = preset.mono_below_hz.max(0.0);
            self.chain_settings.eq.fuzz_amount = preset.fuzz_amount.clamp(0.0, 1.0);
        }
        self.revision += 1;
        self.sync_eq_and_tone_into_chain_settings();
        self.commit_chain_and_linear_phase_kernel();
        self.snapshot()
    }

    /// Names the curve the window is showing, for a preset that was just
    /// saved under the name it already carries.
    pub fn set_active_preset_name(&mut self, name: &str) -> AppStateSnapshot {
        self.active_preset_name = name.trim().to_string();
        self.revision += 1;
        self.snapshot()
    }

    /// Puts back the curve a game interrupted: bands, voicing, headroom, the
    /// model's character amount and the preset's name. The output, engine
    /// state and meters are left alone — a game changes the sound, not the
    /// machine.
    pub fn restore_curve(&mut self, curve: &AppStateSnapshot) -> AppStateSnapshot {
        self.eq_model_amount = curve.eq_model_amount.clamp(0.0, 1.0);
        self.set_eq_bands_and_config(
            curve.bands.clone(),
            &curve.eq_model,
            curve.matched_treble,
            curve.master_power,
            curve.auto_headroom,
            curve.preamp_db,
            curve.tone.clone(),
        );
        self.active_preset_name = curve.active_preset_name.trim().to_string();
        // The curve a game interrupted included its rack and Room, so putting
        // the sound back means putting the whole chain back.
        let mut chain = curve.chain_settings;
        chain.enabled = if curve.master_power { 1 } else { 0 };
        self.update_chain_settings(chain);
        self.revision += 1;
        self.snapshot()
    }

    /// Loads a track onto a player deck (`0`, `1`, `FEQ_DECK_HANDOFF = 2`, or `FEQ_DECK_SPARE = 3`),
    /// resetting the native chain (`FEQ_CHAIN_RESET_SOURCE_CHANGE`) only if the target deck was audible.
    pub fn load_player_deck(&mut self, asked_deck: u32, path: &str, cue_seconds: f64) -> DeckLoadOutcome {
        let outcome = self.deck_transport.load_deck(asked_deck, path, cue_seconds);
        if let Some(reason) = outcome.chain_reset {
            self.native_chain.reset(reason);
        }
        self.revision += 1;
        outcome
    }

    /// Applies or clears the 31-word `FEQ_DENOISE_PROFILE_WIRE` noise floor profile on `NativeChainHandle`.
    pub fn apply_noise_profile_wire(&mut self, values: &[f64]) -> Result<bool, String> {
        let profile = decode_noise_profile_wire(values)?;
        self.native_chain.set_noise_profile(profile.as_ref());
        self.revision += 1;
        Ok(profile.is_some())
    }

    /// Replaces the 11-stage DSP rack settings (`FeqChainSettings`) and commits to `NativeChainHandle`.
    pub fn update_chain_settings(&mut self, settings: FeqChainSettings) {
        self.chain_settings = settings;
        crate::room_catalog::normalize_preset(&mut self.chain_settings.room);
        self.master_power = self.chain_settings.enabled != 0;
        self.matched_treble = self.chain_settings.eq.matched != 0;
        self.eq_model_amount = self.chain_settings.eq.model_amount;
        self.revision += 1;
        self.commit_chain_and_linear_phase_kernel();
    }

    /// Encodes the current 11-stage `FeqChainSettings` into the flat `f64` wire array
    /// compatible with `feq_engine_prepare_config` and `feq_chain_settings_decode`.
    pub fn export_wire_parameters(&self) -> Vec<f64> {
        encode_chain_settings_wire(&self.chain_settings)
    }

    /// Decodes and applies a flat `f64` wire array into the store.
    pub fn import_wire_parameters(&mut self, values: &[f64]) -> bool {
        if let Some(decoded) = decode_chain_settings_wire(values) {
            self.update_chain_settings(decoded);
            true
        } else {
            false
        }
    }

    /// Atomically saves the current `AppStateSnapshot` to disk.
    pub fn save_to_file(&self, path: &Path) -> Result<(), String> {
        save_snapshot_atomic(path, &self.snapshot())
    }

    /// Loads and restores an `AppStateSnapshot` from disk, stripping transient isolate flags.
    pub fn load_from_file(&mut self, path: &Path) -> Result<AppStateSnapshot, String> {
        let snap = load_snapshot_or_recover(path)?;
        self.master_power = snap.master_power;
        self.auto_headroom = snap.auto_headroom;
        self.close_to_tray = snap.close_to_tray;
        self.preamp_db = snap.preamp_db;
        self.active_preset_name = snap.active_preset_name.clone();
        self.device_profiles = snap.device_profiles.clone();
        if self.device_profiles.active_device_guid.is_none() {
            // A file written before profiles were saved still names the output
            // that was active; keep that identity for the next switch.
            self.device_profiles.active_device_guid = snap.active_device_guid.clone();
            self.device_profiles.active_device_name = snap.active_device_name.clone();
        }
        self.eq_model = Self::parse_eq_model(&snap.eq_model);
        self.eq_model_amount = snap.eq_model_amount;
        self.matched_treble = snap.matched_treble;
        self.tone = snap.tone.clone();
        self.bands = snap.bands.clone();
        self.chain_settings = snap.chain_settings;
        crate::room_catalog::normalize_preset(&mut self.chain_settings.room);
        self.revision = self.revision.saturating_add(1);
        self.commit_chain_and_linear_phase_kernel();
        Ok(self.snapshot())
    }

    /// Stands the whole room on a built-in preset: shape, every speaker, bass
    /// management, the stereo fill and the renderer's space and tail. The
    /// head, `compare_original` and `source_already_spatial` are the
    /// listener's, so they stay as they are — and choosing a room still turns
    /// the stage on, because auditioning it is why the chip was pressed.
    pub fn set_room_preset(&mut self, preset_id: i32) -> AppStateSnapshot {
        if let Some(shape) = crate::room_catalog::shape_of(preset_id) {
            let room = &mut self.chain_settings.room;
            room.preset = preset_id;
            room.size_m = shape.size_m;
            room.walls = shape.walls;
            room.distance_m = shape.distance_m;
            room.centre_db = shape.centre_db;
            room.sub_db = shape.sub_db;
            room.angle_deg = shape.angles;
            room.level_db = shape.levels;
            room.speaker_distance_m = shape.distances;
            room.mute = shape.mutes;
            room.bass_management = i32::from(shape.bass_management);
            room.crossover_hz = shape.crossover_hz;
            room.music_upmix = i32::from(shape.music_upmix);
            room.upmix_amount = shape.upmix_amount;
            room.renderer_version = shape.renderer_version;
            room.early_reflection_db = shape.early_reflection_db;
            room.ambience_mix = shape.ambience_mix;
            room.ambience_decay_s = shape.ambience_decay_s;
            room.ambience_damping_hz = shape.ambience_damping_hz;
            room.preserve_position = i32::from(shape.preserve_position);
            room.enabled = 1;
        }
        self.revision += 1;
        self.commit_chain_and_linear_phase_kernel();
        self.snapshot()
    }

    pub fn set_room_head_choice(
        &mut self,
        head: i32,
        heads_dir: &Path,
    ) -> Result<AppStateSnapshot, String> {
        self.chain_settings.room.head = head.clamp(0, 2);
        self.chain_settings.room.enabled = 1;
        self.ensure_room_head_loaded(heads_dir);
        self.revision += 1;
        self.commit_chain_and_linear_phase_kernel();
        Ok(self.snapshot())
    }

    /// Loads the head named by the stored choice into the native convolution
    /// stage. The preset path only carries numbers, so the HRTF has to be
    /// restored after a reload instead of relying on the last session's memory.
    pub fn ensure_room_head_loaded(&mut self, heads_dir: &Path) -> bool {
        let head_name = match self.chain_settings.room.head.clamp(0, 2) {
            0 => "small",
            2 => "large",
            _ => "medium",
        };
        let file_path = heads_dir.join(format!("{head_name}.txt"));
        let Ok(text) = std::fs::read_to_string(&file_path) else {
            return false;
        };
        self.load_room_head_from_text(&text).is_ok()
    }

    pub fn set_room_bass_management(&mut self, enabled: bool) -> AppStateSnapshot {
        self.chain_settings.room.bass_management = if enabled { 1 } else { 0 };
        // Bass management is part of a room's shape, so moving it leaves the
        // preset it came from behind.
        self.chain_settings.room.preset = crate::room_catalog::CUSTOM_PRESET;
        self.revision += 1;
        self.commit_chain_and_linear_phase_kernel();
        self.snapshot()
    }

    /// Parses and loads a KEMAR room head text asset (`small.txt`, `medium.txt`, `large.txt`)
    /// into the native C++ `FeqChain` convolution stage.
    pub fn load_room_head_from_text(&mut self, text: &str) -> Result<String, String> {
        let bank = RoomHeadBank::parse_text(text)?;
        let head_id = bank.head_id.clone();
        if let Some((block, doubling)) = bank.select_for_stream_rate(self.sample_rate as u32) {
            let _ = self.native_chain.set_room_head(
                &block.left,
                &block.right,
                block.directions,
                block.taps,
                doubling,
            );
        }
        self.room_head_banks.insert(head_id.clone(), bank);
        self.residency
            .record_room_heads_cache(self.room_head_banks.len());
        self.revision += 1;
        Ok(head_id)
    }

    pub fn set_online_media_playing(&mut self, playing: bool) -> MemoryFootprintEstimate {
        self.residency.set_online_media_playing(playing);
        self.residency.estimate_memory()
    }

    pub fn enter_minimized_warm(&mut self) -> MemoryFootprintEstimate {
        self.native_meters.set_enabled(false);
        self.residency.enter_minimized_warm()
    }

    pub fn tick_minimized_idle(&mut self, delta_ms: u64) -> MemoryFootprintEstimate {
        let prev_mode = self.residency.mode();
        let est = self.residency.tick_minimized_idle(delta_ms);
        if prev_mode != WindowResidencyMode::ColdStandby
            && est.residency_mode == WindowResidencyMode::ColdStandby
        {
            self.evict_transient_caches();
        }
        self.residency.estimate_memory()
    }

    fn evict_transient_caches(&mut self) {
        self.opra_cache.clear();
        self.residency.record_opra_cache(0);
        if self.room_head_banks.len() > 1 {
            let active_key = match self.chain_settings.room.head {
                0 => "small",
                2 => "large",
                _ => "medium",
            };
            self.room_head_banks.retain(|k, _| k == active_key);
        }
        self.residency
            .record_room_heads_cache(self.room_head_banks.len());

        // If neural voice denoise is disabled when entering ColdStandby, unload its ORT session
        if self.chain_settings.denoise.voice.enabled == 0 && self.models.voice_model_loaded {
            self.unload_voice_denoise_model();
        }
    }

    /// Loads the DPDFNet neural voice denoise model into `NativeChainHandle`.
    pub fn load_voice_denoise_model(&mut self, model_path: &str, runtime_path: &str) -> bool {
        let ok = self.native_chain.load_voice_model(model_path, runtime_path);
        self.models.voice_model_loaded = ok;
        if ok {
            self.models.voice_model_path = Some(model_path.to_string());
            self.models.ort_runtime_path = Some(runtime_path.to_string());
            self.revision += 1;
        }
        ok
    }

    /// Unloads the DPDFNet neural voice denoise model and retires the worker thread.
    pub fn unload_voice_denoise_model(&mut self) {
        self.native_chain.unload_voice_model();
        self.models.voice_model_loaded = false;
        self.models.voice_model_path = None;
        self.models.ort_runtime_path = None;
        self.revision += 1;
    }

    /// Verifies the `/assets/models/swift-f0.onnx` model file on disk and records its metadata.
    pub fn verify_swift_f0_model(&mut self, path: &Path) -> Result<u64, String> {
        let size = verify_onnx_model_file(path)?;
        self.models.swift_f0_verified = true;
        self.models.swift_f0_size_bytes = size;
        self.revision += 1;
        Ok(size)
    }

    /// Computes a real-time vocal pitch estimate (`F0` in Hz, MIDI note, cents, confidence).
    pub fn estimate_vocal_pitch(&self, samples: &[f32]) -> PitchEstimate {
        estimate_vocal_pitch_yin(samples, self.sample_rate)
    }

    /// Synthesizes the 16,384-tap differential FIR kernel for a dynamic EQ band (`band_index`).
    pub fn build_dynamic_band_linear_phase_change_kernel(
        &self,
        band_index: usize,
        ear_slot: i32,
    ) -> Vec<f32> {
        synthesize_linear_phase_change_kernel(
            &self.chain_settings.eq,
            band_index,
            self.sample_rate,
            ear_slot,
        )
    }

    /// Saves the outgoing endpoint's EQ & rack profile and switches to `guid` (`device_name`),
    /// restoring the endpoint's saved profile if one exists.
    pub fn switch_active_output_device(&mut self, guid: &str, device_name: &str) -> AppStateSnapshot {
        if let Some(restored) = self.device_profiles.switch_device(DeviceEndpointProfile {
            guid: guid.to_string(),
            device_name: device_name.to_string(),
            active_preset_name: self.active_preset_name.clone(),
            preamp_db: self.preamp_db,
            matched_treble: self.matched_treble,
            eq_model: self.eq_model_name().to_string(),
            eq_model_amount: self.eq_model_amount,
            tone: self.tone.clone(),
            bands: self.bands.clone(),
            chain_settings: self.chain_settings,
        }) {
            self.active_preset_name = restored.active_preset_name;
            self.preamp_db = restored.preamp_db;
            self.matched_treble = restored.matched_treble;
            self.eq_model = Self::parse_eq_model(&restored.eq_model);
            self.eq_model_amount = restored.eq_model_amount;
            self.tone = restored.tone;
            self.bands = restored.bands;
            self.chain_settings = restored.chain_settings;
            self.sync_eq_and_tone_into_chain_settings();
            self.commit_chain_and_linear_phase_kernel();
        }
        self.revision += 1;
        self.snapshot()
    }

    pub fn enter_cold_standby(&mut self) -> MemoryFootprintEstimate {
        let mut hooks = residency::NativeResidencyHookBridge::new();
        self.apply_residency_transition_with_hooks(WindowResidencyMode::ColdStandby, &mut hooks)
    }

    pub fn wake_to_active(&mut self) -> AppStateSnapshot {
        let mut hooks = residency::NativeResidencyHookBridge::new();
        let _ = self.apply_residency_transition_with_hooks(WindowResidencyMode::Active, &mut hooks);
        self.snapshot()
    }

    pub fn active_device_guid(&self) -> Option<&str> {
        self.device_profiles.active_device_guid.as_deref()
    }

    pub fn residency_mode(&self) -> WindowResidencyMode {
        self.residency.mode()
    }

    pub fn close_to_tray(&self) -> bool {
        self.close_to_tray
    }

    pub fn set_close_to_tray(&mut self, enabled: bool) -> AppStateSnapshot {
        self.close_to_tray = enabled;
        self.revision += 1;
        self.snapshot()
    }

    pub fn set_audio_signal_active(&mut self, active: bool) -> MemoryFootprintEstimate {
        self.residency.set_audio_signal_active(active);
        let should_meter = self.residency.should_enable_native_meters();
        self.native_meters.set_enabled(should_meter);
        self.residency.estimate_memory()
    }

    /// Drives a full residency transition on `AppStateStore` while executing native OS working-set trim
    /// and Tauri/WRY WebView window destruction/recreation hooks.
    pub fn apply_residency_transition_with_hooks<H: residency::ResidencySystemHooks>(
        &mut self,
        requested_mode: WindowResidencyMode,
        hooks: &mut H,
    ) -> MemoryFootprintEstimate {
        let est = self
            .residency
            .apply_transition_with_hooks(requested_mode, hooks);
        self.native_meters.set_enabled(est.meters_fft_enabled);
        if est.residency_mode != WindowResidencyMode::Active {
            self.evict_transient_caches();
        }
        self.residency.estimate_memory()
    }

    fn eq_and_tone_coefficients(&self, include_cuts: bool) -> Vec<fluideq_dsp_sys::FeqBiquadCoefficients> {
        let mut coeffs: Vec<_> = self
            .bands
            .iter()
            .filter(|b| b.enabled)
            .map(|b| {
                biquad_coefficients_designed(
                    &FeqChainEqBand {
                        filter_type: b.filter_type.to_feq_type(),
                        frequency: b.frequency,
                        gain_db: b.gain_db,
                        quality: b.quality,
                        ..FeqChainEqBand::default()
                    },
                    self.sample_rate,
                    self.eq_model,
                    self.eq_model_amount,
                    self.matched_treble,
                )
            })
            .collect();
        if self.tone.bass_db.abs() > 1e-4 {
            coeffs.push(biquad_coefficients_designed(
                &FeqChainEqBand {
                    filter_type: FeqFilterType::FEQ_FILTER_LSC,
                    frequency: self.tone.bass_freq_hz,
                    gain_db: self.tone.bass_db,
                    quality: self.tone.bass_q,
                    ..FeqChainEqBand::default()
                },
                self.sample_rate,
                FeqEqModel::FEQ_EQ_MODEL_CLEAN,
                1.0,
                self.matched_treble,
            ));
        }
        if self.tone.treble_db.abs() > 1e-4 {
            coeffs.push(biquad_coefficients_designed(
                &FeqChainEqBand {
                    filter_type: FeqFilterType::FEQ_FILTER_HSC,
                    frequency: self.tone.treble_freq_hz,
                    gain_db: self.tone.treble_db,
                    quality: self.tone.treble_q,
                    ..FeqChainEqBand::default()
                },
                self.sample_rate,
                FeqEqModel::FEQ_EQ_MODEL_CLEAN,
                1.0,
                self.matched_treble,
            ));
        }
        if include_cuts {
            if self.tone.low_cut_hz > 20.5 {
                coeffs.push(biquad::biquad_coefficients_cookbook(
                    FeqFilterType::FEQ_FILTER_HPQ,
                    self.tone.low_cut_hz,
                    0.0,
                    0.707_106_781_186_547_5,
                    self.sample_rate,
                ));
            }
            if self.tone.high_cut_hz < 19_950.0 {
                coeffs.push(biquad::biquad_coefficients_cookbook(
                    FeqFilterType::FEQ_FILTER_LPQ,
                    self.tone.high_cut_hz,
                    0.0,
                    0.707_106_781_186_547_5,
                    self.sample_rate,
                ));
            }
        }
        coeffs
    }

    pub fn compute_current_auto_preamp_db(&self) -> f64 {
        let coeffs = self.eq_and_tone_coefficients(false);
        compute_auto_preamp_db(&coeffs, self.sample_rate)
    }

    pub fn evaluate_eq_curve_160_points(&self) -> Vec<f64> {
        let coeffs = self.eq_and_tone_coefficients(true);
        let min_log = 20.0_f64.log10();
        let max_log = 20000.0_f64.log10();
        (0..160)
            .map(|idx| {
                let t = idx as f64 / 159.0;
                let freq = 10.0_f64.powf(min_log + t * (max_log - min_log));
                let mut sum_db = 0.0_f64;
                for c in &coeffs {
                    sum_db += biquad::evaluate_biquad_magnitude_db(c, freq, self.sample_rate);
                }
                sum_db
            })
            .collect()
    }

    pub fn native_meters_mut(&mut self) -> &mut NativeMetersHandle {
        &mut self.native_meters
    }

    pub fn snapshot(&self) -> AppStateSnapshot {
        let auto_preamp_db = self.compute_current_auto_preamp_db();
        let eq_curve_db = self.evaluate_eq_curve_160_points();
        let native_working_set_bytes =
            residency::query_current_process_working_set_bytes().unwrap_or(0);
        AppStateSnapshot {
            revision: self.revision,
            master_power: self.master_power,
            auto_headroom: self.auto_headroom,
            close_to_tray: self.close_to_tray,
            preamp_db: self.preamp_db,
            auto_preamp_db,
            preamp_total_db: total_preamp_db(
                self.preamp_db,
                auto_preamp_db,
                self.auto_headroom,
            ),
            active_preset_name: self.active_preset_name.clone(),
            active_device_guid: self.device_profiles.active_device_guid.clone(),
            active_device_name: self.device_profiles.active_device_name.clone(),
            eq_model: self.eq_model_name().to_string(),
            eq_model_amount: self.eq_model_amount,
            matched_treble: self.matched_treble,
            tone: self.tone.clone(),
            bands: self.bands.clone(),
            eq_curve_db,
            native_abi_version: fluideq_dsp_sys::engine_abi_version().to_string(),
            native_active_stages: self.native_chain.active_stages(),
            native_latency_frames: self.native_chain.latency_frames(),
            native_working_set_bytes,
            chain_settings: self.chain_settings,
            models: self.models.clone(),
            device_profiles: self.device_profiles.clone(),
            residency: self.residency.estimate_memory(),
            impulse_response: self.impulse_response.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preamp_readout_is_the_guard_plus_the_trim() {
        // With headroom off the slider is the whole answer; with it on the
        // guard and the trim add up, which is what the engine applies.
        assert_eq!(total_preamp_db(0.0, -6.0, true), -6.0);
        assert_eq!(total_preamp_db(-3.0, -6.0, true), -9.0);
        assert_eq!(total_preamp_db(-3.0, -6.0, false), -3.0);
        // The default trim is zero, so a fresh store shows only the curve's
        // own headroom instead of an unexplained -1.5 dB.
        assert_eq!(AppStateStore::new(48_000.0).snapshot().preamp_db, 0.0);
    }

    #[test]
    fn close_to_tray_defaults_on_and_survives_a_reload() {
        let mut store = AppStateStore::new(48_000.0);
        assert!(store.snapshot().close_to_tray);
        assert!(!store.set_close_to_tray(false).close_to_tray);

        let path = std::env::temp_dir().join(format!(
            "fluideq-state-tray-{}.json",
            std::process::id()
        ));
        store.save_to_file(&path).unwrap();
        let mut reloaded = AppStateStore::new(48_000.0);
        let restored = reloaded.load_from_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(!restored.close_to_tray);

        // A state file written before the setting existed keeps the old habit.
        let mut old = serde_json::to_value(AppStateStore::new(48_000.0).snapshot()).unwrap();
        old.as_object_mut().unwrap().remove("close_to_tray");
        let parsed: AppStateSnapshot = serde_json::from_value(old).unwrap();
        assert!(parsed.close_to_tray);
    }

    #[test]
    fn endpoint_profiles_survive_a_reload_and_old_files_keep_the_active_name() {
        let mut store = AppStateStore::new(48_000.0);
        store.update_band_gain(2, 250.0, 4.0, 1.0).unwrap();
        store.switch_active_output_device("{device-a}", "Device A");
        store.update_band_gain(2, 250.0, -3.0, 1.0).unwrap();
        store.switch_active_output_device("{device-b}", "Device B");

        let saved = store
            .snapshot()
            .device_profiles
            .profiles
            .get("{device-a}")
            .cloned()
            .expect("the outgoing endpoint should be saved");
        let saved_band = saved.bands.iter().find(|band| band.id == 2).unwrap();
        assert!((saved_band.gain_db - -3.0).abs() < 0.001);

        let path = std::env::temp_dir().join(format!(
            "fluideq-state-profiles-{}.json",
            std::process::id()
        ));
        store.save_to_file(&path).unwrap();
        let mut reloaded = AppStateStore::new(48_000.0);
        let restored = reloaded.load_from_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(restored.active_device_guid.as_deref(), Some("{device-b}"));
        assert!(restored.device_profiles.profiles.contains_key("{device-a}"));

        // Files written before profiles were saved still name the active output.
        let mut legacy = serde_json::to_value(store.snapshot()).unwrap();
        legacy.as_object_mut().unwrap().remove("device_profiles");
        let legacy_path = std::env::temp_dir().join(format!(
            "fluideq-state-profiles-legacy-{}.json",
            std::process::id()
        ));
        std::fs::write(&legacy_path, serde_json::to_string(&legacy).unwrap()).unwrap();
        let mut legacy_store = AppStateStore::new(48_000.0);
        let adopted = legacy_store.load_from_file(&legacy_path).unwrap();
        let _ = std::fs::remove_file(&legacy_path);
        assert!(adopted.device_profiles.profiles.is_empty());
        assert_eq!(adopted.active_device_guid.as_deref(), Some("{device-b}"));
        assert_eq!(
            legacy_store
                .switch_active_output_device("{device-c}", "Device C")
                .active_device_guid
                .as_deref(),
            Some("{device-c}")
        );
    }

    #[test]
    fn applying_a_preset_restores_its_character_amount_and_survives_a_reload() {
        let mut store = AppStateStore::new(48_000.0);
        store.eq_model_amount = 0.2;
        let mut preset = presets::EqPreset::flat();
        preset.name = "Warm".to_string();
        preset.eq_model = "proportional".to_string();
        preset.eq_model_amount = 0.65;

        let applied = store.apply_preset(&preset);
        assert_eq!(applied.eq_model_amount, 0.65);
        assert_eq!(applied.eq_model, "proportional");
        assert_eq!(applied.chain_settings.eq.model_amount, 0.65);
        assert_eq!(applied.active_preset_name, "Warm");

        let path = std::env::temp_dir().join(format!(
            "fluideq-state-preset-{}.json",
            std::process::id()
        ));
        store.save_to_file(&path).unwrap();
        let mut reloaded = AppStateStore::new(48_000.0);
        let restored = reloaded.load_from_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(restored.eq_model, "proportional");
        assert_eq!(restored.eq_model_amount, 0.65);
    }

    #[test]
    fn a_loaded_impulse_response_owns_the_kernel_and_stays_out_of_the_state_file() {
        use crate::wav_ir::IrSummary;

        let mut store = AppStateStore::new(48_000.0);
        store.ir_kernel = Some((vec![1.0_f32, 0.0], vec![1.0_f32, 0.0]));
        store.impulse_response = Some(IrSummary {
            sample_rate: 48_000,
            channels: 2,
            frames: 2,
            seconds: 2.0 / 48_000.0,
            left_peak_db: 0.0,
            right_peak_db: 0.0,
        });

        // A band edit runs the EQ's own kernel refresh; the IR must survive it.
        store.update_band_gain(1, 40.0, 4.0, 1.0).unwrap();
        assert_eq!(store.native_chain.eq_kernel_len(), 2);
        let live = serde_json::to_value(store.snapshot()).unwrap();
        assert_eq!(live["impulse_response"]["frames"], 2);

        let path = std::env::temp_dir().join(format!(
            "fluideq-state-ir-{}.json",
            std::process::id()
        ));
        store.save_to_file(&path).unwrap();
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(saved["impulse_response"].is_null());

        store.clear_impulse_response();
        assert_eq!(store.native_chain.eq_kernel_len(), 0);
        assert!(store.snapshot().impulse_response.is_none());
    }

    #[test]
    fn cold_standby_preserves_eq_state_and_disables_meters() {
        let mut store = AppStateStore::new(48_000.0);
        store.update_band_gain(1, 32.0, 6.5, 0.85).unwrap();

        let cold_mem = store.enter_cold_standby();
        assert_eq!(cold_mem.residency_mode, WindowResidencyMode::ColdStandby);
        assert_eq!(cold_mem.webview_ui_and_canvas_mb, 0.0);
        assert!(!cold_mem.meters_fft_enabled);
        assert_eq!(cold_mem.spectrum_target_fps, 0);
        assert!(cold_mem.total_estimated_mb < 15.0);

        let restored = store.wake_to_active();
        assert_eq!(restored.residency.residency_mode, WindowResidencyMode::Active);
        assert!(restored.residency.meters_fft_enabled);
        assert_eq!(restored.bands[0].frequency, 32.0);
        assert_eq!(restored.bands[0].gain_db, 6.5);
    }

    #[test]
    fn silence_drops_spectrum_fps_to_zero() {
        let mut store = AppStateStore::new(48_000.0);
        let silent = store.set_audio_signal_active(false);
        assert_eq!(silent.spectrum_target_fps, 0);
        assert!(!silent.meters_fft_enabled);
    }

    #[test]
    fn a_restored_curve_keeps_its_name_and_character() {
        let mut store = AppStateStore::new(48_000.0);
        store.update_band_gain(3, 75.0, -4.2, 1.8).unwrap();
        store.set_active_preset_name("Saved curve");
        let saved = store.snapshot();

        store.apply_preset(&presets::EqPreset::modern_country());
        assert_ne!(store.snapshot().active_preset_name, saved.active_preset_name);

        let restored = store.restore_curve(&saved);
        assert_eq!(restored.active_preset_name, "Saved curve");
        assert_eq!(restored.eq_model_amount, saved.eq_model_amount);
        assert_eq!(restored.bands, saved.bands);
        assert_eq!(restored.preamp_db, saved.preamp_db);
    }

    #[test]
    fn wire_export_and_import_preserves_store_chain_settings() {
        let mut store = AppStateStore::new(48_000.0);
        store.update_band_gain(3, 75.0, -4.2, 1.8).unwrap();
        let wire = store.export_wire_parameters();

        let mut target_store = AppStateStore::new(48_000.0);
        assert!(target_store.import_wire_parameters(&wire));
        assert_eq!(
            target_store.snapshot().chain_settings.eq.bands[2].frequency,
            75.0
        );
        assert_eq!(
            target_store.snapshot().chain_settings.eq.bands[2].gain_db,
            -4.2
        );
    }

    #[test]
    fn set_tone_and_voicing_clamps_and_reflects_in_curve() {
        let mut store = AppStateStore::new(48_000.0);
        let tone = ToneSettings {
            bass_db: 18.0,      // should clamp to 12.0
            bass_freq_hz: 5.0,  // should fallback to 105.0
            bass_q: 0.01,       // should fallback to 0.71
            treble_db: -20.0,   // should clamp to -12.0
            treble_freq_hz: 30_000.0, // should clamp to 20_000.0
            treble_q: 15.0,     // should clamp to 10.0
            low_cut_hz: 20.0,
            high_cut_hz: 20_000.0,
        };
        let snap = store.set_tone_and_voicing(tone, "asymmetric", 0.8, false);
        assert_eq!(snap.tone.bass_db, 12.0);
        assert_eq!(snap.tone.bass_freq_hz, 105.0);
        assert_eq!(snap.tone.bass_q, 0.71);
        assert_eq!(snap.tone.treble_db, -12.0);
        assert_eq!(snap.tone.treble_freq_hz, 20_000.0);
        assert_eq!(snap.tone.treble_q, 10.0);
        assert_eq!(snap.eq_model, "asymmetric");
        assert_eq!(snap.eq_model_amount, 0.8);
        assert!(!snap.matched_treble);
        assert_eq!(snap.eq_curve_db.len(), 160);
        // Bass shelf of +12 dB at 105 Hz should make 20 Hz low frequencies boosted > 6 dB
        assert!(snap.eq_curve_db[0] > 6.0);
    }

    #[test]
    fn opra_vendor_listing_and_curve_application() {
        let mut store = AppStateStore::new(48_000.0);
        let curves_dir = std::path::Path::new("opra/curves");
        let vendors = store.list_opra_vendors(curves_dir);
        assert!(!vendors.is_empty());
        assert!(vendors.iter().any(|v| v == "sennheiser"));
        assert!(vendors.iter().any(|v| v == "sony"));

        let models = store.list_opra_models(curves_dir, "sennheiser").unwrap();
        assert!(models.contains(&"hd_600".to_string()));

        let resolved = store
            .apply_opra_headphone_curve(curves_dir, "sennheiser", "hd_600", None)
            .unwrap();
        assert!(resolved.contains("hd_600"));
        assert_eq!(store.snapshot().active_preset_name, resolved);
        assert!(store.snapshot().preamp_db < 0.0);
    }

    #[test]
    fn room_preset_and_head_selection() {
        let mut store = AppStateStore::new(48_000.0);
        let snap = store.set_room_preset(2); // classic Cinema
        assert_eq!(snap.chain_settings.room.preset, 2);
        assert_eq!(snap.chain_settings.room.size_m, 9.0);
        assert_eq!(snap.chain_settings.room.distance_m, 4.0);
        assert_eq!(snap.chain_settings.room.renderer_version, 1);
        // Picking a room turns the stage on so the preset is audible.
        assert_eq!(snap.chain_settings.room.enabled, 1);

        let featured = store.set_room_preset(12); // Reference (second renderer)
        assert_eq!(featured.chain_settings.room.preset, 12);
        assert_eq!(featured.chain_settings.room.renderer_version, 2);
        assert_eq!(featured.chain_settings.room.early_reflection_db, -16.0);
        assert!(featured.chain_settings.room.preserve_position != 0);

        // The head and the two source choices are the listener's, not a room's.
        let head_before = featured.chain_settings.room.head;
        let reshaped = store.set_room_preset(24); // Rear Guard
        assert_eq!(reshaped.chain_settings.room.head, head_before);

        let heads_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/room/heads");
        let heads_dir = heads_dir.as_path();
        let head_snap = store.set_room_head_choice(0, heads_dir).unwrap(); // Small
        assert_eq!(head_snap.chain_settings.room.head, 0);
        assert_eq!(head_snap.chain_settings.room.enabled, 1);
        assert!(store.ensure_room_head_loaded(heads_dir));

        let bass_snap = store.set_room_bass_management(true);
        assert_eq!(bass_snap.chain_settings.room.bass_management, 1);
        // Bass management belongs to a room's shape, so moving it leaves the
        // preset behind.
        assert_eq!(
            bass_snap.chain_settings.room.preset,
            crate::room_catalog::CUSTOM_PRESET
        );
    }

    #[test]
    fn band_shape_routing_and_bypass_round_trip() {
        let mut store = AppStateStore::new(48_000.0);
        let band_id = store.snapshot().bands[1].id;

        let shaped = store
            .update_band_config(band_id, false, "HSC", 2)
            .expect("shape");
        let band = shaped.bands.iter().find(|b| b.id == band_id).unwrap();
        assert!(!band.enabled);
        assert_eq!(band.filter_type, crate::biquad::BandFilterKind::HSC);
        assert_eq!(band.ear, 2);

        // The engine's own copy of the chain must agree with the snapshot.
        let engine_band = &shaped.chain_settings.eq.bands[(band_id as usize) - 1];
        assert_eq!(engine_band.enabled, 0);
        assert_eq!(engine_band.ear, 2);

        assert!(store.update_band_config(band_id, true, "nope", 0).is_err());
    }

    #[test]
    fn apo_text_import_and_export_round_trip() {
        let mut store = AppStateStore::new(48_000.0);
        let text = "Preamp: -6.5 dB\n\
                    Filter 1: ON PK Fc 100 Hz Gain 3.0 dB Q 1.41\n\
                    Filter 2: ON LSC Fc 105 Hz Gain 4.0 dB Q 0.70\n";
        let snap = store.import_apo_text(text).expect("import");
        assert_eq!(snap.bands.len(), 2);
        assert_eq!(snap.preamp_db, -6.5);
        assert_eq!(snap.bands[0].filter_type, crate::biquad::BandFilterKind::PK);
        assert_eq!(snap.bands[1].filter_type, crate::biquad::BandFilterKind::LSC);

        let exported = store.export_apo_text();
        assert!(exported.contains("Preamp: -6.5 dB"));
        assert!(exported.contains("ON PK Fc 100.0 Hz Gain 3.0 dB"));
        assert!(exported.contains("ON LSC Fc 105.0 Hz Gain 4.0 dB"));

        // Re-importing our own export must produce the same two bands.
        let again = store.import_apo_text(&exported).expect("re-import");
        assert_eq!(again.bands.len(), 2);
        assert!(store.import_apo_text("no filters here").is_err());
    }

    #[test]
    fn a_preset_carries_the_rack_and_room_it_was_saved_with() {
        let mut store = AppStateStore::new(48_000.0);
        let mut chain = store.snapshot().chain_settings;
        chain.room.enabled = 1;
        chain.room.size_m = 12.0;
        chain.exciter.enabled = 0;
        store.update_chain_settings(chain);

        let preset = store.current_preset("Room and rack");
        let mut target = AppStateStore::new(48_000.0);
        target.apply_preset(&preset);
        let restored = target.snapshot();
        assert_eq!(restored.chain_settings.room.enabled, 1);
        assert_eq!(restored.chain_settings.room.size_m, 12.0);
        assert_eq!(restored.chain_settings.exciter.enabled, 0);
        assert_eq!(restored.active_preset_name, "Room and rack");
    }

    #[test]
    fn a_factory_preset_stages_the_voicing_it_was_fitted_on() {
        let mut store = AppStateStore::new(48_000.0);
        let vinyl = presets::builtin_presets()
            .into_iter()
            .find(|preset| preset.name == "Vinyl")
            .expect("Vinyl");
        let applied = store.apply_preset(&vinyl);
        assert_eq!(applied.eq_model, "wide");
        assert_eq!(applied.chain_settings.eq.subsonic_hz, 30.0);
        assert_eq!(applied.chain_settings.eq.mono_below_hz, 150.0);
        assert_eq!(applied.chain_settings.eq.fuzz_amount, 0.15);

        let default = presets::builtin_presets()
            .into_iter()
            .find(|preset| preset.name == "Default")
            .expect("Default");
        let cleared = store.apply_preset(&default);
        assert_eq!(cleared.chain_settings.eq.subsonic_hz, 0.0);
        assert_eq!(cleared.chain_settings.eq.mono_below_hz, 0.0);
        assert_eq!(cleared.chain_settings.eq.fuzz_amount, 0.0);
    }

    #[test]
    fn an_old_preset_without_a_chain_leaves_the_rack_alone() {
        let mut store = AppStateStore::new(48_000.0);
        let mut chain = store.snapshot().chain_settings;
        chain.room.size_m = 5.5;
        chain.exciter.enabled = 1;
        store.update_chain_settings(chain);

        let old: presets::EqPreset =
            serde_json::from_str(r#"{"name":"Old","bands":[]}"#).expect("old file");
        assert!(old.chain_settings.is_none(), "a file written before the chain has no chain");

        let mut preset = presets::EqPreset::modern_country();
        preset.chain_settings = None;
        store.apply_preset(&preset);
        let applied = store.snapshot();
        assert_eq!(applied.chain_settings.room.size_m, 5.5);
        assert_eq!(applied.chain_settings.exciter.enabled, 1);
    }

    #[test]
    fn a_band_edit_refuses_an_unknown_id_and_keeps_a_sub_20hz_frequency() {
        let mut store = AppStateStore::new(48_000.0);
        assert!(store.update_band_gain(999, 1_000.0, 3.0, 1.0).is_err());
        store
            .update_band_gain(1, 12.0, 0.0, 1.0)
            .expect("band 1 exists");
        assert_eq!(store.snapshot().bands[0].frequency, 12.0);
    }
}
