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
pub mod deck_transport;
pub mod device_profiles;
pub mod linear_phase;
pub mod models;
pub mod opra;
pub mod persistence;
pub mod remote_audio_runtime;
pub mod remote_playback;
pub mod residency;
pub mod room_heads;
pub mod wav_ir;

use biquad::{biquad_coefficients_designed, compute_auto_preamp_db, BandFilterKind, EqBandSpec};
use chain_wire::{decode_chain_settings_wire, encode_chain_settings_wire};
use deck_transport::{decode_noise_profile_wire, DeckLoadOutcome, DualDeckCoordinator};
use device_profiles::DeviceProfileRegistry;
use fluideq_dsp_sys::{
    FeqChainSettings, FeqEqModel, FeqFilterType, FeqPhaseMode, NativeChainHandle,
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
use wav_ir::{decode_pcm_impulse_response, DecodedImpulseResponse};

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
    pub preamp_db: f64,
    pub auto_preamp_db: f64,
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
    pub residency: MemoryFootprintEstimate,
}

pub struct AppStateStore {
    revision: u64,
    sample_rate: f64,
    master_power: bool,
    auto_headroom: bool,
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
}

impl Default for AppStateStore {
    fn default() -> Self {
        Self::new(48_000.0)
    }
}

impl AppStateStore {
    pub fn new(sample_rate: f64) -> Self {
        let iso_freqs = [
            25.0, 40.0, 63.0, 100.0, 160.0, 250.0, 400.0, 630.0, 1000.0, 1600.0, 2500.0, 4000.0,
            6300.0, 10000.0, 16000.0,
        ];
        let default_gains = [
            0.0, 0.0, 0.0, 0.0, 1.4, 0.0, 0.0, -1.1, 0.0, 0.0, 0.0, 1.8, 0.0, 0.0, 0.0,
        ];
        let bands: Vec<EqBandSpec> = iso_freqs
            .iter()
            .zip(default_gains.iter())
            .enumerate()
            .map(|(idx, (&freq, &gain))| EqBandSpec {
                id: (idx + 1) as u32,
                enabled: true,
                filter_type: if idx == 0 {
                    BandFilterKind::LSC
                } else if idx == iso_freqs.len() - 1 {
                    BandFilterKind::HSC
                } else {
                    BandFilterKind::PK
                },
                frequency: freq,
                gain_db: gain,
                quality: if idx == 0 || idx == iso_freqs.len() - 1 {
                    0.71
                } else {
                    1.41
                },
                dynamic: false,
                threshold_db: -18.0,
                ear: 0,
            })
            .collect();

        let mut store = Self {
            revision: 1,
            sample_rate,
            master_power: true,
            auto_headroom: true,
            preamp_db: -1.5,
            active_preset_name: "Modern Country".to_string(),
            eq_model: FeqEqModel::FEQ_EQ_MODEL_CLEAN,
            eq_model_amount: 1.0,
            matched_treble: true,
            tone: ToneSettings::default(),
            bands,
            chain_settings: FeqChainSettings::default(),
            device_profiles: DeviceProfileRegistry::new(),
            deck_transport: DualDeckCoordinator::new(),
            room_head_banks: BTreeMap::new(),
            opra_cache: OpraShardCache::new(),
            models: NeuralModelStatus::default(),
            residency: ColdResidencyController::new(),
            native_chain: NativeChainHandle::new(sample_rate as u32, 2, 2048),
            native_meters: NativeMetersHandle::new(2, sample_rate),
        };
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
            "proportional" => FeqEqModel::FEQ_EQ_MODEL_PROPORTIONAL,
            "wide" => FeqEqModel::FEQ_EQ_MODEL_WIDE,
            "asymmetric" => FeqEqModel::FEQ_EQ_MODEL_ASYMMETRIC,
            _ => FeqEqModel::FEQ_EQ_MODEL_CLEAN,
        }
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
        if self.chain_settings.eq.phase == FeqPhaseMode::FEQ_PHASE_LINEAR as i32 {
            let (left, right) =
                synthesize_stereo_linear_phase_kernels(&self.chain_settings.eq, self.sample_rate);
            let _ = self.native_chain.set_eq_kernel(&left, &right);
        } else if self.native_chain.eq_kernel_len() != 0 {
            let _ = self.native_chain.set_eq_kernel(&[], &[]);
        }
        let _ = self.native_chain.configure(&self.chain_settings);
    }

    pub fn update_band_gain(&mut self, band_id: u32, frequency: f64, gain_db: f64, quality: f64) {
        if let Some(band) = self.bands.iter_mut().find(|b| b.id == band_id) {
            band.frequency = frequency.clamp(20.0, 20000.0);
            band.gain_db = gain_db.clamp(-24.0, 24.0);
            band.quality = quality.clamp(0.1, 18.0);
            self.revision += 1;
            self.sync_eq_and_tone_into_chain_settings();
            self.commit_chain_and_linear_phase_kernel();
        }
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
        self.tone = tone;
        self.revision += 1;
        self.sync_eq_and_tone_into_chain_settings();
        self.commit_chain_and_linear_phase_kernel();
        self.snapshot()
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
        let _ = self.native_chain.set_eq_kernel(&decoded.left, &decoded.right);
        self.revision += 1;
        Ok(decoded)
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
        self.preamp_db = snap.preamp_db;
        self.active_preset_name = snap.active_preset_name.clone();
        self.eq_model_amount = snap.eq_model_amount;
        self.matched_treble = snap.matched_treble;
        self.tone = snap.tone.clone();
        self.bands = snap.bands.clone();
        self.chain_settings = snap.chain_settings;
        self.revision = self.revision.saturating_add(1);
        self.commit_chain_and_linear_phase_kernel();
        Ok(self.snapshot())
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
        if let Some(restored) = self.device_profiles.switch_device(
            guid,
            device_name,
            &self.active_preset_name,
            self.preamp_db,
            self.matched_treble,
            self.eq_model_amount,
            &self.tone,
            &self.bands,
            &self.chain_settings,
        ) {
            self.active_preset_name = restored.active_preset_name;
            self.preamp_db = restored.preamp_db;
            self.matched_treble = restored.matched_treble;
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

    pub fn compute_current_auto_preamp_db(&self) -> f64 {
        let coeffs: Vec<_> = self
            .bands
            .iter()
            .filter(|b| b.enabled)
            .map(|b| {
                biquad_coefficients_designed(
                    b.filter_type.to_feq_type(),
                    b.frequency,
                    b.gain_db,
                    b.quality,
                    self.sample_rate,
                    self.eq_model,
                    self.eq_model_amount,
                    self.matched_treble,
                )
            })
            .collect();
        compute_auto_preamp_db(&coeffs, self.sample_rate)
    }

    pub fn evaluate_eq_curve_160_points(&self) -> Vec<f64> {
        let coeffs: Vec<_> = self
            .bands
            .iter()
            .filter(|b| b.enabled)
            .map(|b| {
                biquad_coefficients_designed(
                    b.filter_type.to_feq_type(),
                    b.frequency,
                    b.gain_db,
                    b.quality,
                    self.sample_rate,
                    self.eq_model,
                    self.eq_model_amount,
                    self.matched_treble,
                )
            })
            .collect();
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
            preamp_db: self.preamp_db,
            auto_preamp_db,
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
            residency: self.residency.estimate_memory(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_standby_preserves_eq_state_and_disables_meters() {
        let mut store = AppStateStore::new(48_000.0);
        store.update_band_gain(1, 32.0, 6.5, 0.85);

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
    fn wire_export_and_import_preserves_store_chain_settings() {
        let mut store = AppStateStore::new(48_000.0);
        store.update_band_gain(3, 75.0, -4.2, 1.8);
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
}
