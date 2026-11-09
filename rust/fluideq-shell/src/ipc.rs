/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Lightweight IPC Command Router between the Cold-Residency WebView UI and the Rust State Store.

use fluideq_dsp_sys::FeqChainSettings;
use fluideq_state::{
    apo::{
        default_slot_for, plan_attach, plan_detach, plan_mode_in_place, ApoSlot, EndpointFxPlan,
        EndpointFxValues, FLUID_ENGINE_APO_CLSID,
    },
    biquad::EqBandSpec,
    deck_transport::DeckLoadOutcome,
    models::PitchEstimate,
    residency::MemoryFootprintEstimate,
    wav_ir::DecodedImpulseResponse,
    AppStateSnapshot, AppStateStore, ToneSettings,
};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", content = "payload")]
pub enum ShellIpcCommand {
    GetSnapshot,
    UpdateBand {
        band_id: u32,
        frequency: f64,
        gain_db: f64,
        quality: f64,
    },
    SetEqBandsAndConfig {
        bands: Vec<EqBandSpec>,
        eq_model: String,
        matched_treble: bool,
        master_power: bool,
        auto_headroom: bool,
        manual_preamp_db: f64,
        tone: ToneSettings,
    },
    UpdateChainSettings(FeqChainSettings),
    ExportWireParameters,
    ImportWireParameters {
        values: Vec<f64>,
    },
    LoadRoomHeadText {
        text: String,
    },
    ApplyOpraHeadphoneCurve {
        opra_dir: String,
        vendor: String,
        product: String,
        curve_id: Option<String>,
    },
    LoadWavImpulseResponse {
        bytes: Vec<u8>,
    },
    LoadPlayerDeck {
        asked_deck: u32,
        path: String,
        cue_seconds: f64,
    },
    ApplyNoiseProfileWire {
        values: Vec<f64>,
    },
    PlanApoEndpointAttach {
        current: EndpointFxValues,
        requested_slot: Option<ApoSlot>,
        pre_win81: bool,
    },
    PlanApoModeInPlace {
        current: EndpointFxValues,
    },
    PlanApoEndpointDetach {
        current: EndpointFxValues,
        backup: EndpointFxValues,
    },
    LoadVoiceDenoiseModel {
        model_path: String,
        runtime_path: String,
    },
    UnloadVoiceDenoiseModel,
    VerifySwiftF0Model {
        path: String,
    },
    EstimateVocalPitch {
        samples: Vec<f32>,
    },
    SwitchOutputDevice {
        guid: String,
        name: String,
    },
    SaveStateToFile {
        path: String,
    },
    LoadStateFromFile {
        path: String,
    },
    SetOnlineMediaPlaying {
        playing: bool,
    },
    EnterMinimizedWarm,
    TickMinimizedIdle {
        delta_ms: u64,
    },
    EnterColdStandby,
    WakeFromColdStandby,
    SetAudioSignalActive {
        active: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum ShellIpcResponse {
    Snapshot(AppStateSnapshot),
    Residency(MemoryFootprintEstimate),
    WireParameters(Vec<f64>),
    RoomHeadLoaded {
        head_id: String,
    },
    OpraCurveApplied {
        curve_id: String,
        snapshot: AppStateSnapshot,
    },
    WavImpulseLoaded {
        channels: u16,
        sample_rate: u32,
        frames: usize,
    },
    DeckLoaded(DeckLoadOutcome),
    NoiseProfileApplied {
        active: bool,
    },
    ApoFxPlanned(EndpointFxPlan),
    VoiceModelStatus {
        loaded: bool,
    },
    SwiftF0Verified {
        size_bytes: u64,
    },
    PitchEstimated(PitchEstimate),
    Saved {
        path: String,
    },
    Error {
        message: String,
    },
}

pub fn dispatch_ipc_command(
    store: &mut AppStateStore,
    command: ShellIpcCommand,
) -> ShellIpcResponse {
    match command {
        ShellIpcCommand::GetSnapshot => ShellIpcResponse::Snapshot(store.snapshot()),
        ShellIpcCommand::UpdateBand {
            band_id,
            frequency,
            gain_db,
            quality,
        } => {
            store.update_band_gain(band_id, frequency, gain_db, quality);
            ShellIpcResponse::Snapshot(store.snapshot())
        }
        ShellIpcCommand::SetEqBandsAndConfig {
            bands,
            eq_model,
            matched_treble,
            master_power,
            auto_headroom,
            manual_preamp_db,
            tone,
        } => ShellIpcResponse::Snapshot(store.set_eq_bands_and_config(
            bands,
            &eq_model,
            matched_treble,
            master_power,
            auto_headroom,
            manual_preamp_db,
            tone,
        )),
        ShellIpcCommand::UpdateChainSettings(settings) => {
            store.update_chain_settings(settings);
            ShellIpcResponse::Snapshot(store.snapshot())
        }
        ShellIpcCommand::ExportWireParameters => {
            ShellIpcResponse::WireParameters(store.export_wire_parameters())
        }
        ShellIpcCommand::ImportWireParameters { values } => {
            if store.import_wire_parameters(&values) {
                ShellIpcResponse::Snapshot(store.snapshot())
            } else {
                ShellIpcResponse::Error {
                    message: "Invalid FeqChainSettings wire parameter buffer".to_string(),
                }
            }
        }
        ShellIpcCommand::LoadRoomHeadText { text } => match store.load_room_head_from_text(&text) {
            Ok(head_id) => ShellIpcResponse::RoomHeadLoaded { head_id },
            Err(message) => ShellIpcResponse::Error { message },
        },
        ShellIpcCommand::ApplyOpraHeadphoneCurve {
            opra_dir,
            vendor,
            product,
            curve_id,
        } => match store.apply_opra_headphone_curve(
            Path::new(&opra_dir),
            &vendor,
            &product,
            curve_id.as_deref(),
        ) {
            Ok(applied_id) => ShellIpcResponse::OpraCurveApplied {
                curve_id: applied_id,
                snapshot: store.snapshot(),
            },
            Err(message) => ShellIpcResponse::Error { message },
        },
        ShellIpcCommand::LoadWavImpulseResponse { bytes } => {
            match store.load_wav_impulse_response(&bytes) {
                Ok(DecodedImpulseResponse {
                    channels,
                    sample_rate,
                    frames,
                    ..
                }) => ShellIpcResponse::WavImpulseLoaded {
                    channels,
                    sample_rate,
                    frames,
                },
                Err(message) => ShellIpcResponse::Error { message },
            }
        }
        ShellIpcCommand::LoadPlayerDeck {
            asked_deck,
            path,
            cue_seconds,
        } => ShellIpcResponse::DeckLoaded(store.load_player_deck(asked_deck, &path, cue_seconds)),
        ShellIpcCommand::ApplyNoiseProfileWire { values } => {
            match store.apply_noise_profile_wire(&values) {
                Ok(active) => ShellIpcResponse::NoiseProfileApplied { active },
                Err(message) => ShellIpcResponse::Error { message },
            }
        }
        ShellIpcCommand::PlanApoEndpointAttach {
            current,
            requested_slot,
            pre_win81,
        } => {
            let slot = requested_slot.unwrap_or_else(|| default_slot_for(&current, pre_win81));
            ShellIpcResponse::ApoFxPlanned(plan_attach(&current, FLUID_ENGINE_APO_CLSID, slot))
        }
        ShellIpcCommand::PlanApoEndpointDetach { current, backup } => {
            ShellIpcResponse::ApoFxPlanned(plan_detach(&current, &backup, FLUID_ENGINE_APO_CLSID))
        }
        ShellIpcCommand::PlanApoModeInPlace { current } => {
            ShellIpcResponse::ApoFxPlanned(plan_mode_in_place(&current, FLUID_ENGINE_APO_CLSID))
        }
        ShellIpcCommand::LoadVoiceDenoiseModel {
            model_path,
            runtime_path,
        } => {
            let loaded = store.load_voice_denoise_model(&model_path, &runtime_path);
            ShellIpcResponse::VoiceModelStatus { loaded }
        }
        ShellIpcCommand::UnloadVoiceDenoiseModel => {
            store.unload_voice_denoise_model();
            ShellIpcResponse::VoiceModelStatus { loaded: false }
        }
        ShellIpcCommand::VerifySwiftF0Model { path } => {
            match store.verify_swift_f0_model(Path::new(&path)) {
                Ok(size_bytes) => ShellIpcResponse::SwiftF0Verified { size_bytes },
                Err(message) => ShellIpcResponse::Error { message },
            }
        }
        ShellIpcCommand::EstimateVocalPitch { samples } => {
            ShellIpcResponse::PitchEstimated(store.estimate_vocal_pitch(&samples))
        }
        ShellIpcCommand::SwitchOutputDevice { guid, name } => {
            ShellIpcResponse::Snapshot(store.switch_active_output_device(&guid, &name))
        }
        ShellIpcCommand::SaveStateToFile { path } => {
            match store.save_to_file(Path::new(&path)) {
                Ok(()) => ShellIpcResponse::Saved { path },
                Err(message) => ShellIpcResponse::Error { message },
            }
        }
        ShellIpcCommand::LoadStateFromFile { path } => {
            match store.load_from_file(Path::new(&path)) {
                Ok(snap) => ShellIpcResponse::Snapshot(snap),
                Err(message) => ShellIpcResponse::Error { message },
            }
        }
        ShellIpcCommand::SetOnlineMediaPlaying { playing } => {
            ShellIpcResponse::Residency(store.set_online_media_playing(playing))
        }
        ShellIpcCommand::EnterMinimizedWarm => {
            ShellIpcResponse::Residency(store.enter_minimized_warm())
        }
        ShellIpcCommand::TickMinimizedIdle { delta_ms } => {
            ShellIpcResponse::Residency(store.tick_minimized_idle(delta_ms))
        }
        ShellIpcCommand::EnterColdStandby => {
            store.enter_cold_standby();
            ShellIpcResponse::Snapshot(store.snapshot())
        }
        ShellIpcCommand::WakeFromColdStandby => {
            ShellIpcResponse::Snapshot(store.wake_to_active())
        }
        ShellIpcCommand::SetAudioSignalActive { active } => {
            ShellIpcResponse::Residency(store.set_audio_signal_active(active))
        }
    }
}
