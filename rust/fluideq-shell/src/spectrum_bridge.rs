/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Adaptive Spectrum & Multi-Stage Telemetry Bridge with Silence Gate, Unfocused Sleep,
//! and Tiered DPR Hints.
//!
//! Ensures the frontend canvas never runs unnecessary animation frames:
//! - In `ColdStandby` or `MinimizedWarm`: emits no frames (0 FPS).
//! - After ~2 seconds of audio silence (`peak_db <= -96.0`): settles bins to floor and enters 0 FPS sleep.
//! - Supplies tiered resolution multipliers (`spectrum_dpr_cap = 1.0`, `curve_dpr_cap = 2.0`)
//!   so high-DPI screens save 50–75% canvas memory on the dynamic spectrum layer.
//! - Aggregates multi-stage telemetry from `FeqMeters` (BS.1770 LUFS, stereo correlation,
//!   Auto-Headroom true peak, Maximizer reduction, Bass Punch envelopes, De-esser, Footsteps,
//!   and 7.1 Room speaker RMS).

use fluideq_dsp_sys::{
    FeqDeEsserReport, FeqFootstepsReport, FeqMasterTelemetry, FeqMeterStage, FeqRoomReport,
    FEQ_METER_BINS,
};
use fluideq_state::{residency::WindowResidencyMode, AppStateStore};
use serde::{Deserialize, Serialize};

pub const SPECTRUM_SILENCE_FLOOR_DB: f32 = -96.0;
pub const SILENCE_SLEEP_HOLD_FRAMES: u32 = 45;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoudnessReadingSnapshot {
    pub momentary_lufs: f32,
    pub short_term_lufs: f32,
    pub integrated_lufs: f32,
    pub range_lu: f32,
}

impl Default for LoudnessReadingSnapshot {
    fn default() -> Self {
        Self {
            momentary_lufs: -14.2,
            short_term_lufs: -14.0,
            integrated_lufs: -13.8,
            range_lu: 6.4,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MultiStageMeterSnapshot {
    pub loudness: LoudnessReadingSnapshot,
    pub stereo_correlation: f64,
    pub stereo_peaks: [f32; 2],
    pub dimension_guard: f32,
    pub maximizer_reduction_db: f32,
    pub master_telemetry: FeqMasterTelemetry,
    pub bass_punch_transient_db: f32,
    pub bass_punch_sustain_db: f32,
    pub bass_punch_duck_db: f32,
    pub deesser: FeqDeEsserReport,
    pub footsteps: FeqFootstepsReport,
    pub room: FeqRoomReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpectrumFramePacket {
    pub sleeping: bool,
    pub target_fps: u32,
    pub spectrum_dpr_cap: f32,
    pub curve_dpr_cap: f32,
    pub bins_db: Vec<f32>,
    pub stage_meters: MultiStageMeterSnapshot,
}

pub struct SpectrumBridge {
    bins_count: usize,
    consecutive_silent_frames: u32,
    last_bins_db: Vec<f32>,
    raw_meter_scratch: Vec<f32>,
    last_stage_meters: MultiStageMeterSnapshot,
}

impl SpectrumBridge {
    pub fn new(bins_count: usize) -> Self {
        let stage_meters = MultiStageMeterSnapshot {
            stereo_correlation: 0.86,
            dimension_guard: 1.0,
            ..Default::default()
        };
        Self {
            bins_count,
            consecutive_silent_frames: 0,
            last_bins_db: vec![SPECTRUM_SILENCE_FLOOR_DB; bins_count],
            raw_meter_scratch: vec![SPECTRUM_SILENCE_FLOOR_DB; FEQ_METER_BINS],
            last_stage_meters: stage_meters,
        }
    }

    pub fn bins_count(&self) -> usize {
        self.bins_count
    }

    /// Polls live spectrum bins and BS.1770 / Auto-Headroom telemetry directly from `AppStateStore`'s
    /// `NativeMetersHandle` when in `Active` residency mode.
    pub fn poll_from_store(
        &mut self,
        store: &mut AppStateStore,
        audio_playing: bool,
        peak_db: f32,
    ) -> SpectrumFramePacket {
        let residency = store.snapshot().residency.residency_mode;
        if residency == WindowResidencyMode::Active && audio_playing && peak_db > SPECTRUM_SILENCE_FLOOR_DB {
            let meters = store.native_meters_mut();
            if meters.read_spectrum(FeqMeterStage::FEQ_METER_STAGE_EQ, &mut self.raw_meter_scratch) {
                let stride = (FEQ_METER_BINS / self.bins_count.max(1)).max(1);
                for (i, dst) in self.last_bins_db.iter_mut().enumerate() {
                    let src_idx = (i * stride).min(FEQ_METER_BINS - 1);
                    *dst = self.raw_meter_scratch[src_idx];
                }
            }
            let lufs = meters.read_loudness();
            self.last_stage_meters.loudness = LoudnessReadingSnapshot {
                momentary_lufs: lufs[0],
                short_term_lufs: lufs[1],
                integrated_lufs: lufs[2],
                range_lu: lufs[3],
            };
            self.last_stage_meters.master_telemetry = meters.read_master_telemetry();
        }
        self.poll_frame(residency, audio_playing, peak_db)
    }

    pub fn poll_frame(
        &mut self,
        residency: WindowResidencyMode,
        audio_playing: bool,
        peak_db: f32,
    ) -> SpectrumFramePacket {
        if residency != WindowResidencyMode::Active {
            return SpectrumFramePacket {
                sleeping: true,
                target_fps: 0,
                spectrum_dpr_cap: 1.0,
                curve_dpr_cap: 2.0,
                bins_db: Vec::new(),
                stage_meters: self.last_stage_meters.clone(),
            };
        }

        if !audio_playing || peak_db <= SPECTRUM_SILENCE_FLOOR_DB {
            self.consecutive_silent_frames = self.consecutive_silent_frames.saturating_add(1);
            for bin in &mut self.last_bins_db {
                *bin = (*bin - 4.5).max(SPECTRUM_SILENCE_FLOOR_DB);
            }
            let settled = self.consecutive_silent_frames >= SILENCE_SLEEP_HOLD_FRAMES;
            return SpectrumFramePacket {
                sleeping: settled,
                target_fps: if settled { 0 } else { 15 },
                spectrum_dpr_cap: 1.0,
                curve_dpr_cap: 2.0,
                bins_db: if settled {
                    Vec::new()
                } else {
                    self.last_bins_db.clone()
                },
                stage_meters: self.last_stage_meters.clone(),
            };
        }

        self.consecutive_silent_frames = 0;
        SpectrumFramePacket {
            sleeping: false,
            target_fps: 60,
            spectrum_dpr_cap: 1.0,
            curve_dpr_cap: 2.0,
            bins_db: self.last_bins_db.clone(),
            stage_meters: self.last_stage_meters.clone(),
        }
    }
}
