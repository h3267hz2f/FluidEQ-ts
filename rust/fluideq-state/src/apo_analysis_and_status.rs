/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! FluidEQ System-APO Real-Time Source Analysis, Analysis Pipe Link, Bounded Engine Log,
//! Process-Wide Status Board, and Machine-Wide Path Resolver (`Slice-M1M8-9`).
//!
//! 1:1 faithful port of:
//! - `/native/system-apo/src/source_analysis.h` & `source_analysis.cpp` (`SourceAnalysis`, `parse_source_analysis`, `source_for_endpoint`)
//! - `/native/system-apo/src/analysis_link.h` & `analysis_link.cpp` (`AnalysisLink`, 44-byte handshake, commands 1/2/3)
//! - `/native/system-apo/src/analysis_snapshot.cpp` (`AnalysisLink::snapshot` 872-byte `FeqWireAnalysisFrame` + variable payload)
//! - `/native/system-apo/src/status_file.h` & `status_file.cpp` (`write_status`, `output_carried_flag`, `output_block_timing`, `StatusShare`)
//! - `/native/system-apo/src/log.h` & `log.cpp` (`Log`, `trace`, `trace_built`, `guid_text`, `to_utf8`, `file_name_of`, `decibels`, 1 MB cap)
//! - `/native/system-apo/src/paths.h` & `paths.cpp` (`engine_root`, `config_dir`, `log_path`, `owner_pipe_name`, `parent_of`, `deepest_existing`, `is_directory`)

use crate::apo_engine_runtime::{endpoint_config_name, format_engine_status_json, EngineStatusReport};
use crate::apo_graph_and_guard::BlockTiming;
use fluideq_dsp_sys::{
    feq_wire_deesser_report, feq_wire_dynamics_report, feq_wire_footsteps_report,
    feq_wire_room_report, FeqCompanderReport, FeqDeEsserReport,
    FeqDenoiseReport, FeqFootstepsReport, FeqMasterTelemetry, FeqNoiseProfile, FeqRoomReport,
    FeqWireAnalysisFrame, FEQ_BASS_FORGE_BANDS, FEQ_DENOISE_MAX_HUM_PARTIALS,
    FEQ_DENOISE_PROFILE_BANDS, FEQ_DENOISE_PROFILE_WIRE, FEQ_MAGIC_ANALYSIS, FEQ_METER_BINS,
    FEQ_METER_DETAIL_BINS, FEQ_METER_DETAIL_WINDOW, FEQ_METER_EXCITER_BANDS, FEQ_METER_MAX_BANDS,
    FEQ_METER_SCOPE_PAIRS,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

// ============================================================================
// 1. Source Analysis Sidecar Parser & Endpoint Router (`source_analysis.h/.cpp`)
// ============================================================================

/// Sidecar filename in the engine config directory (`kSourceAnalysisFile` in `source_analysis.h`).
pub const SOURCE_ANALYSIS_FILE: &str = "fluideq-source-analysis.txt";

/// Largest exact IEEE-754 integer accepted for `epoch` and `revision` (`kLargestExactInteger = 9007199254740991ULL`).
pub const LARGEST_EXACT_INTEGER: u64 = 9_007_199_254_740_991;

/// Maximum text byte length accepted by `parse_source_analysis` (`65536`).
pub const MAX_SOURCE_ANALYSIS_TEXT_BYTES: usize = 65_536;

/// Immutable source measurements (`SourceAnalysis` in `/native/system-apo/src/source_analysis.h`).
/// Holds source facts only, never an output's selected controls or gains.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceAnalysis {
    pub library: bool,
    pub engine_owner: bool,
    pub source: u64,
    pub epoch: u64,
    pub revision: u64,
    pub endpoint: String,
    pub has_level: bool,
    pub level: f64,
    pub peak: f64,
    pub noise: Option<FeqNoiseProfile>,
    pub voice_available: bool,
    pub voice_model: String,
    /// Never read from the writable sidecar: supplied by the trusted module's
    /// own protected installation path after the setup helper verifies it.
    pub voice_runtime: String,
    /// Pins verified data through model loading (`voice_model_guard` in C++).
    pub voice_model_verified: bool,
    pub identity: String,
}

impl Default for SourceAnalysis {
    fn default() -> Self {
        Self {
            library: false,
            engine_owner: true,
            source: 0,
            epoch: 0,
            revision: 0,
            endpoint: String::new(),
            has_level: false,
            level: 0.0,
            peak: 0.0,
            noise: None,
            voice_available: false,
            voice_model: String::new(),
            voice_runtime: String::new(),
            voice_model_verified: false,
            identity: String::new(),
        }
    }
}

/// Borrowed view equivalent to `FeqChainSourceAnalysis` returned by `SourceAnalysis::view()`.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainSourceAnalysisView<'a> {
    pub version: u32,
    pub library: i32,
    pub source_id: u64,
    pub epoch: u64,
    pub revision: u64,
    pub has_level: i32,
    pub integrated_lufs: f64,
    pub true_peak_dbtp: f64,
    pub noise: Option<&'a FeqNoiseProfile>,
    pub voice_model_path: Option<&'a str>,
    pub voice_runtime_path: Option<&'a str>,
}

impl SourceAnalysis {
    /// Exact port of `SourceAnalysis::view()` in `/native/system-apo/src/source_analysis.cpp`.
    pub fn view(&self) -> ChainSourceAnalysisView<'_> {
        let voice = self.voice_available
            && self.voice_model_verified
            && !self.voice_model.is_empty()
            && !self.voice_runtime.is_empty();
        ChainSourceAnalysisView {
            version: 1,
            library: if self.library { 1 } else { 0 },
            source_id: self.source,
            epoch: self.epoch,
            revision: self.revision,
            has_level: if self.has_level { 1 } else { 0 },
            integrated_lufs: self.level,
            true_peak_dbtp: self.peak,
            noise: self.noise.as_ref(),
            voice_model_path: if voice {
                Some(self.voice_model.as_str())
            } else {
                None
            },
            voice_runtime_path: if voice {
                Some(self.voice_runtime.as_str())
            } else {
                None
            },
        }
    }

    /// Exact port of `SourceAnalysis::processing_identity()` in `/native/system-apo/src/source_analysis.cpp`.
    /// Encodes DSP facts only: an ACK `revision` or route `endpoint` move must not rebuild a rack.
    pub fn processing_identity(&self) -> String {
        fn push_integer_part(out: &mut String, value: u64) {
            out.push('|');
            out.push_str(&value.to_string());
        }
        fn push_number_part(out: &mut String, value: f64) {
            out.push('|');
            if value == 0.0 {
                out.push('0');
            } else {
                out.push_str(&format!("{}", value));
            }
        }

        let mut out = String::from(if self.library { "library" } else { "live" });

        push_integer_part(&mut out, if self.engine_owner { 1 } else { 0 });
        push_integer_part(&mut out, self.source);
        push_integer_part(&mut out, self.epoch);
        push_integer_part(&mut out, if self.has_level { 1 } else { 0 });
        if self.has_level {
            push_number_part(&mut out, self.level);
            push_number_part(&mut out, self.peak);
        }
        push_integer_part(&mut out, if self.noise.is_some() { 1 } else { 0 });
        if let Some(noise) = &self.noise {
            for &band in &noise.bands_db {
                push_number_part(&mut out, band);
            }
            push_number_part(&mut out, noise.floor_dbfs);
            push_number_part(&mut out, noise.hum_hz);
            push_integer_part(&mut out, noise.hum_partial_count as u64);
            for &hz in &noise.hum_partial_hz {
                push_number_part(&mut out, hz);
            }
            for &excess in &noise.hum_partial_excess_db {
                push_number_part(&mut out, excess);
            }
        }
        push_integer_part(&mut out, if self.voice_available { 1 } else { 0 });
        push_integer_part(&mut out, if self.voice_model_verified { 1 } else { 0 });
        push_integer_part(&mut out, self.voice_model.len() as u64);
        out.push_str(&self.voice_model);
        push_integer_part(&mut out, self.voice_runtime.len() as u64);
        out.push_str(&self.voice_runtime);
        out
    }
}

fn parse_exact_uint64(text: &str, radix: u32) -> Option<u64> {
    if text.is_empty() || text.starts_with('+') {
        return None;
    }
    u64::from_str_radix(text, radix).ok()
}

fn parse_exact_finite_f64(text: &str) -> Option<f64> {
    if text.is_empty() || text != text.trim() {
        return None;
    }
    let val = text.parse::<f64>().ok()?;
    if val.is_finite() {
        Some(val)
    } else {
        None
    }
}

/// Exact port of `absolute_model` in `/native/system-apo/src/source_analysis.cpp`.
/// A data file only (`C:\...` or `C:/...`, length `4..=32767`, no `\r`, `\n`, or `\0`).
pub fn is_valid_absolute_voice_model_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() > 3
        && bytes.len() <= 32_767
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
        && !bytes.iter().any(|&b| b == b'\r' || b == b'\n' || b == b'\0')
}

/// Exact port of `noise_profile` in `/native/system-apo/src/source_analysis.cpp`.
/// Parses `FEQ_DENOISE_PROFILE_WIRE` (63) whitespace-separated finite numbers.
pub fn parse_source_noise_profile(mut text: &str) -> Option<FeqNoiseProfile> {
    let mut values = [0.0f64; FEQ_DENOISE_PROFILE_WIRE];
    for value in &mut values {
        let trimmed = text.trim_start_matches([' ', '\t']);
        if trimmed.is_empty() {
            return None;
        }
        let end = trimmed.find([' ', '\t']).unwrap_or(trimmed.len());
        *value = parse_exact_finite_f64(&trimmed[..end])?;
        text = &trimmed[end..];
    }
    if !text.trim_start_matches([' ', '\t']).is_empty() {
        return None;
    }

    let mut out = FeqNoiseProfile::default();
    let mut at = 0usize;
    for band in &mut out.bands_db {
        *band = values[at];
        at += 1;
    }
    out.floor_dbfs = values[at];
    at += 1;
    out.hum_hz = values[at];
    at += 1;
    let count = values[at];
    at += 1;
    if !(0.0..=(FEQ_DENOISE_MAX_HUM_PARTIALS as f64)).contains(&count) || count.floor() != count {
        return None;
    }
    out.hum_partial_count = count as u32;
    for hz in &mut out.hum_partial_hz {
        *hz = values[at];
        at += 1;
    }
    for excess in &mut out.hum_partial_excess_db {
        *excess = values[at];
        at += 1;
    }
    Some(out)
}

fn same_endpoint(left: &str, right: &str) -> bool {
    let a = endpoint_config_name("id", left);
    let b = endpoint_config_name("id", right);
    matches!((a, b), (Some(lhs), Some(rhs)) if lhs == rhs)
}

/// Exact port of `parse_source_analysis` in `/native/system-apo/src/source_analysis.cpp`.
/// Refuses duplicate/unknown fields, invalid numbers, and incomplete facts.
pub fn parse_source_analysis(text: &str) -> Option<SourceAnalysis> {
    if text.len() > MAX_SOURCE_ANALYSIS_TEXT_BYTES || text.as_bytes().contains(&0) {
        return None;
    }
    let mut out = SourceAnalysis {
        identity: text.to_string(),
        ..Default::default()
    };
    let mut seen = BTreeSet::new();
    let mut has_peak = false;
    let mut cursor = text;

    while !cursor.is_empty() {
        let (raw_line, rest) = match cursor.find('\n') {
            Some(end) => (&cursor[..end], &cursor[end + 1..]),
            None => (cursor, ""),
        };
        cursor = rest;
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let equals = line.find('=')?;
        let key = &line[..equals];
        let value = &line[equals + 1..];
        if !seen.insert(key) {
            return None;
        }
        match key {
            "version" => {
                if value != "1" {
                    return None;
                }
            }
            "kind" => {
                if value != "library" && value != "live" {
                    return None;
                }
                out.library = value == "library";
            }
            "owner" => {
                if value != "host" && value != "engine" {
                    return None;
                }
                out.engine_owner = value == "engine";
            }
            "source" => {
                if value.len() != 16 {
                    return None;
                }
                out.source = parse_exact_uint64(value, 16)?;
            }
            "epoch" | "revision" => {
                let parsed = parse_exact_uint64(value, 10)?;
                if parsed > LARGEST_EXACT_INTEGER {
                    return None;
                }
                if key == "epoch" {
                    out.epoch = parsed;
                } else {
                    out.revision = parsed;
                }
            }
            "endpoint" => {
                endpoint_config_name("id", value)?;
                out.endpoint = value.to_string();
            }
            "level" | "peak" => {
                let parsed = parse_exact_finite_f64(value)?;
                if !(-120.0..=24.0).contains(&parsed) {
                    return None;
                }
                if key == "level" {
                    out.level = parsed;
                    out.has_level = true;
                } else {
                    out.peak = parsed;
                    has_peak = true;
                }
            }
            "noise" => {
                out.noise = Some(parse_source_noise_profile(value)?);
            }
            "voice" => {
                if value != "available" && value != "unavailable" {
                    return None;
                }
                out.voice_available = value == "available";
            }
            "voiceModel" => {
                if !is_valid_absolute_voice_model_path(value) {
                    return None;
                }
                out.voice_model = value.to_string();
            }
            _ => return None,
        }
    }

    for required in ["version", "kind", "source", "epoch", "revision"] {
        if !seen.contains(required) {
            return None;
        }
    }

    if out.has_level != has_peak
        || (out.voice_available && out.voice_model.is_empty())
        || (!out.voice_available && !out.voice_model.is_empty())
        || (out.library
            && (out.source == 0 || out.endpoint.is_empty() || !seen.contains("owner")))
    {
        return None;
    }

    Some(out)
}

/// Exact port of `source_for_endpoint` in `/native/system-apo/src/source_analysis.cpp`.
/// Only the Library source and split receivers of that source in `default_mode` may use its scan;
/// unrelated outputs get their own `live` source so a Library seek/scan cannot reset their leveling
/// or force their graph to rebuild.
pub fn source_for_endpoint(
    source: Option<&SourceAnalysis>,
    endpoint_guid: &str,
    from_guid: &str,
    default_mode: bool,
) -> SourceAnalysis {
    if let Some(src) = source {
        if src.library
            && default_mode
            && (same_endpoint(endpoint_guid, &src.endpoint)
                || same_endpoint(from_guid, &src.endpoint))
        {
            return src.clone();
        }
    }

    let mut live = SourceAnalysis::default();
    if let Some(src) = source {
        live.voice_available = src.voice_available;
        live.voice_model = src.voice_model.clone();
        live.voice_runtime = src.voice_runtime.clone();
        live.voice_model_verified = src.voice_model_verified;
    }
    live.identity = format!(
        "live|model={}|runtime={}|verified={}",
        live.voice_model,
        live.voice_runtime,
        if live.voice_model_verified { 1 } else { 0 }
    );
    live
}

// ============================================================================
// 2. Real-Time Analysis Pipe Link & Snapshot Builder (`analysis_link.cpp` & `analysis_snapshot.cpp`)
// ============================================================================

/// Named pipe used by the APO engine to serve display spectra, preamp state, and `audiodg` process stats.
pub const ENGINE_ANALYSIS_PIPE_NAME: &str = r"\\.\pipe\FluidEQ-Engine-Analysis";

/// Magic word `"FEAP"` (`0x50414546`) sent in response to command `2` (preamp request).
pub const ANALYSIS_PREAMP_MAGIC: u32 = 0x5041_4546;

/// Magic word `"FEQS"` (`0x53514546`) sent in response to command `3` (`audiodg.exe` process stats request).
pub const ANALYSIS_PROCESS_STATS_MAGIC: u32 = 0x5351_4546;

/// Capability bits advertised at `hello[39]` in `AnalysisLink::run()`.
pub const ANALYSIS_HELLO_CAP_PREAMP: u8 = 1;
pub const ANALYSIS_HELLO_CAP_PROCESS_STATS: u8 = 2;

/// Stage spectrum readings and meter snapshots used to construct a variable-length `FEQN` packet
/// in `AnalysisLink::snapshot()` (`/native/system-apo/src/analysis_snapshot.cpp`).
#[derive(Debug, Clone)]
pub struct AnalysisSnapshotInputs {
    /// Four optional stage spectra (`EXCITER = 0`, `EQ = 1`, `MASTER = 2`, `DENOISE = 3`), each `FEQ_METER_BINS` (1024) floats.
    pub stage_spectra: [Option<Vec<f32>>; 4],
    /// Optional 16,384-window EQ detail spectrum (`FEQ_METER_DETAIL_BINS` = 2048 floats), sent only when stage 1 (`EQ`) is fresh.
    pub eq_detail: Option<Vec<f32>>,
    /// Optional goniometer scope interleaved L/R pairs (`FEQ_METER_SCOPE_PAIRS * 2` = 512 floats).
    pub scope_pairs: Option<Vec<f32>>,
    pub correlation: f64,
    pub peaks: [f32; 2],
    /// Dynamic EQ band amounts and levels (up to `FEQ_METER_MAX_BANDS` = 64 each).
    pub band_amounts: Vec<f32>,
    pub band_levels: Vec<f32>,
    pub exciter_bands: [f32; FEQ_METER_EXCITER_BANDS],
    pub exciter_organic: f32,
    pub maximizer_reduction_db: f32,
    pub dimension_guard: f32,
    pub room: FeqRoomReport,
    pub master: FeqMasterTelemetry,
    pub normalizer_input_peaks: [f32; 2],
    pub normalizer_output_peaks: [f32; 2],
    pub normalizer_applied_gain_db: f32,
    /// BS.1770 `[momentary_lufs, short_term_lufs, integrated_lufs, range_lu]`.
    pub loudness: [f32; 4],
    pub denoise: FeqDenoiseReport,
    pub bass_forge_input_db: [f32; FEQ_BASS_FORGE_BANDS],
    pub bass_forge_output_db: [f32; FEQ_BASS_FORGE_BANDS],
    pub bass_punch_transient_db: f32,
    pub bass_punch_sustain_db: f32,
    pub bass_punch_duck_db: f32,
    pub deesser: FeqDeEsserReport,
    pub footsteps: FeqFootstepsReport,
    pub dynamics: FeqCompanderReport,
    /// Trailing 5-float live input meter tail (`[-120, -120, -120, 0, -120]` by default).
    pub live_input: [f32; 5],
}

impl Default for AnalysisSnapshotInputs {
    fn default() -> Self {
        Self {
            stage_spectra: [None, None, None, None],
            eq_detail: None,
            scope_pairs: None,
            correlation: 1.0,
            peaks: [0.0, 0.0],
            band_amounts: Vec::new(),
            band_levels: Vec::new(),
            exciter_bands: [0.0; FEQ_METER_EXCITER_BANDS],
            exciter_organic: 0.0,
            maximizer_reduction_db: 0.0,
            dimension_guard: 1.0,
            room: FeqRoomReport::default(),
            master: FeqMasterTelemetry::default(),
            normalizer_input_peaks: [0.0, 0.0],
            normalizer_output_peaks: [0.0, 0.0],
            normalizer_applied_gain_db: 0.0,
            loudness: [-120.0, -120.0, -120.0, 0.0],
            denoise: FeqDenoiseReport::default(),
            bass_forge_input_db: [-120.0; FEQ_BASS_FORGE_BANDS],
            bass_forge_output_db: [-120.0; FEQ_BASS_FORGE_BANDS],
            bass_punch_transient_db: 0.0,
            bass_punch_sustain_db: 0.0,
            bass_punch_duck_db: 0.0,
            deesser: FeqDeEsserReport::default(),
            footsteps: FeqFootstepsReport::default(),
            dynamics: FeqCompanderReport::default(),
            live_input: [-120.0, -120.0, -120.0, 0.0, -120.0],
        }
    }
}

/// Exact port of `AnalysisLink` in `/native/system-apo/src/analysis_link.h/.cpp` and `analysis_snapshot.cpp`.
/// One producer (the endpoint's audio thread), one reader (the pipe worker).
/// The meters outlive every graph, including graphs retired during a reload.
pub struct AnalysisLink {
    endpoint: String,
    rate: u32,
    channels: u32,
    sequence: u32,
    meters_enabled: AtomicBool,
    pub active: AtomicBool,
    output_gain_bits: AtomicU32,
    pub output_enabled: AtomicBool,
    pub output_active: AtomicBool,
}

impl AnalysisLink {
    pub fn new(endpoint: &str, rate: u32, channels: u32) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            rate,
            channels: channels.min(2),
            sequence: 0,
            meters_enabled: AtomicBool::new(false),
            active: AtomicBool::new(false),
            output_gain_bits: AtomicU32::new(0.0f32.to_bits()),
            output_enabled: AtomicBool::new(false),
            output_active: AtomicBool::new(false),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn channels(&self) -> u32 {
        self.channels
    }

    pub fn set_output_gain(&self, gain_db: f32) {
        self.output_gain_bits
            .store(gain_db.to_bits(), Ordering::Relaxed);
    }

    pub fn output_gain(&self) -> f32 {
        f32::from_bits(self.output_gain_bits.load(Ordering::Relaxed))
    }

    pub fn meters_enabled(&self) -> bool {
        self.meters_enabled.load(Ordering::Acquire)
    }

    /// Builds the fixed 44-byte handshake sent on connection to `\\.\pipe\FluidEQ-Engine-Analysis`
    /// (`AnalysisLink::run()` in `/native/system-apo/src/analysis_link.cpp`):
    /// - `hello[0..39]`: ASCII endpoint GUID, zero-padded (max 39 bytes)
    /// - `hello[39]`: capability mask `1 | 2` (preamp request `2` + process-stats request `3`)
    /// - `hello[40..44]`: little-endian `u32` sample rate
    pub fn build_hello_packet(&self) -> [u8; 44] {
        let mut hello = [0u8; 44];
        let endpoint_bytes = self.endpoint.as_bytes();
        let copy_len = endpoint_bytes.len().min(39);
        hello[..copy_len].copy_from_slice(&endpoint_bytes[..copy_len]);
        hello[39] = ANALYSIS_HELLO_CAP_PREAMP | ANALYSIS_HELLO_CAP_PROCESS_STATS;
        hello[40..44].copy_from_slice(&self.rate.to_le_bytes());
        hello
    }

    /// Encodes the 16-byte response (4-byte size `12` + 12-byte packet) for command `2` (preamp state).
    pub fn encode_preamp_response(&self) -> [u8; 16] {
        let gain = self.output_gain();
        let flags = (if self.output_enabled.load(Ordering::Relaxed) {
            1u32
        } else {
            0u32
        }) | (if self.output_active.load(Ordering::Relaxed) {
            2u32
        } else {
            0u32
        });
        let size = 12u32;
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&size.to_le_bytes());
        out[4..8].copy_from_slice(&ANALYSIS_PREAMP_MAGIC.to_le_bytes());
        out[8..12].copy_from_slice(&gain.to_le_bytes());
        out[12..16].copy_from_slice(&flags.to_le_bytes());
        out
    }

    /// Encodes the 28-byte response (4-byte size `24` + 24-byte packet) for command `3` (`audiodg.exe` process stats).
    pub fn encode_process_stats_response(
        pid: u32,
        working_set_bytes: u64,
        cpu_hundred_ns: u64,
    ) -> [u8; 28] {
        let size = 24u32;
        let mut out = [0u8; 28];
        out[0..4].copy_from_slice(&size.to_le_bytes());
        out[4..8].copy_from_slice(&ANALYSIS_PROCESS_STATS_MAGIC.to_le_bytes());
        out[8..12].copy_from_slice(&pid.to_le_bytes());
        out[12..20].copy_from_slice(&working_set_bytes.to_le_bytes());
        out[20..28].copy_from_slice(&cpu_hundred_ns.to_le_bytes());
        out
    }

    /// Exact port of `AnalysisLink::snapshot()` in `/native/system-apo/src/analysis_snapshot.cpp`.
    /// Assembles the 872-byte `FeqWireAnalysisFrame` header + variable-length spectra, scope,
    /// dynamic band amounts/levels, EQ detail long window, and 5-float `live_input` tail.
    pub fn snapshot(&mut self, inputs: &AnalysisSnapshotInputs) -> Vec<u8> {
        let mut frame = FeqWireAnalysisFrame::default();
        let mut present = 0u32;
        let mut spectra_concat = Vec::new();

        for (stage, maybe_spec) in inputs.stage_spectra.iter().enumerate() {
            if let Some(spec) = maybe_spec {
                if spec.len() >= FEQ_METER_BINS {
                    frame.stage_mask |= 1u32 << (stage as u32);
                    present += 1;
                    spectra_concat.extend_from_slice(&spec[..FEQ_METER_BINS]);
                }
            }
        }

        // The EQ tap's long window travels only beside a fresh short one (stage 1 = FEQ_METER_STAGE_EQ).
        let eq_fresh = (frame.stage_mask & (1u32 << 1)) != 0;
        let detail_slice: &[f32] = match (eq_fresh, inputs.eq_detail.as_deref()) {
            (true, Some(d)) if d.len() >= FEQ_METER_DETAIL_BINS => &d[..FEQ_METER_DETAIL_BINS],
            _ => &[],
        };
        let detail_bins = detail_slice.len() as u32;

        let scope_slice: &[f32] = match inputs.scope_pairs.as_deref() {
            Some(s) if s.len() >= FEQ_METER_SCOPE_PAIRS * 2 => &s[..FEQ_METER_SCOPE_PAIRS * 2],
            _ => &[],
        };
        let has_scope = !scope_slice.is_empty();

        // No fresh audio window means no new display frame, even if static band telemetry exists.
        if present == 0 && !has_scope {
            return Vec::new();
        }

        self.sequence = self.sequence.wrapping_add(1);
        let band_count = inputs
            .band_amounts
            .len()
            .min(inputs.band_levels.len())
            .min(FEQ_METER_MAX_BANDS);

        frame.magic = FEQ_MAGIC_ANALYSIS;
        frame.sequence = self.sequence;
        frame.bins = FEQ_METER_BINS as u32;
        frame.pairs = if has_scope {
            FEQ_METER_SCOPE_PAIRS as u32
        } else {
            0
        };
        frame.bands = band_count as u32;
        frame.correlation = inputs.correlation;
        frame.peak_left = inputs.peaks[0];
        frame.peak_right = inputs.peaks[1];
        frame.exciter_bands = inputs.exciter_bands;
        frame.exciter_organic = inputs.exciter_organic;
        frame.maximizer_reduction_db = inputs.maximizer_reduction_db;
        frame.dimension_guard = inputs.dimension_guard;
        feq_wire_room_report(&mut frame, &inputs.room);
        frame.auto_headroom_reduction_db = inputs.master.auto_headroom_reduction_db as f32;
        frame.auto_headroom_true_peak_db = inputs.master.auto_headroom_true_peak_db as f32;
        frame.safety_reduction_db = 0.0;
        frame.safety_true_peak_db = 0.0;
        frame.dc_correction_db = 0.0;
        frame.eq_detail_window = FEQ_METER_DETAIL_WINDOW as u32;
        frame.eq_detail_bins = detail_bins;
        frame.safety_enabled = 0;
        frame.normalizer_input_peaks = inputs.normalizer_input_peaks;
        frame.normalizer_output_peaks = inputs.normalizer_output_peaks;
        frame.normalizer_applied_gain_db = inputs.normalizer_applied_gain_db;
        frame.loudness_momentary_lufs = inputs.loudness[0];
        frame.loudness_short_term_lufs = inputs.loudness[1];
        frame.loudness_integrated_lufs = inputs.loudness[2];
        frame.loudness_range_lu = inputs.loudness[3];

        frame.denoise_reduction_db = inputs.denoise.reduction_db as f32;
        frame.denoise_noise_floor_db = inputs.denoise.noise_floor_db as f32;
        frame.denoise_clicks_repaired = inputs.denoise.clicks_repaired;
        frame.denoise_voice_underruns = inputs.denoise.voice_underruns;
        frame.denoise_profile_ready = if inputs.denoise.profile_ready != 0 {
            1
        } else {
            0
        };
        frame.denoise_voice_model_loaded = if inputs.denoise.voice_model_loaded != 0 {
            1
        } else {
            0
        };
        for band in 0..FEQ_DENOISE_PROFILE_BANDS {
            frame.denoise_floor_bands[band] = inputs.denoise.floor_bands_db[band] as f32;
            frame.denoise_hiss_reduction_bands[band] =
                inputs.denoise.hiss_reduction_bands_db[band] as f32;
        }
        frame.bass_forge_input_db = inputs.bass_forge_input_db;
        frame.bass_forge_output_db = inputs.bass_forge_output_db;
        frame.bass_punch_transient_db = inputs.bass_punch_transient_db;
        frame.bass_punch_sustain_db = inputs.bass_punch_sustain_db;
        frame.bass_punch_duck_db = inputs.bass_punch_duck_db;
        feq_wire_deesser_report(&mut frame, &inputs.deesser);
        feq_wire_footsteps_report(&mut frame, &inputs.footsteps);
        feq_wire_dynamics_report(&mut frame, &inputs.dynamics);

        let header_bytes = std::mem::size_of::<FeqWireAnalysisFrame>();
        let spectrum_bytes = std::mem::size_of::<f32>() * spectra_concat.len();
        let scope_bytes = std::mem::size_of_val(scope_slice);
        let band_bytes = std::mem::size_of::<f32>() * band_count;
        let detail_bytes = std::mem::size_of_val(detail_slice);
        let live_input_bytes = std::mem::size_of::<f32>() * 5;

        let mut packet = Vec::with_capacity(
            header_bytes
                + spectrum_bytes
                + scope_bytes
                + band_bytes * 2
                + detail_bytes
                + live_input_bytes,
        );

        let header_slice = unsafe {
            std::slice::from_raw_parts(
                (&frame as *const FeqWireAnalysisFrame).cast::<u8>(),
                header_bytes,
            )
        };
        packet.extend_from_slice(header_slice);

        for &val in &spectra_concat {
            packet.extend_from_slice(&val.to_le_bytes());
        }
        for &val in scope_slice {
            packet.extend_from_slice(&val.to_le_bytes());
        }
        for &val in &inputs.band_amounts[..band_count] {
            packet.extend_from_slice(&val.to_le_bytes());
        }
        for &val in &inputs.band_levels[..band_count] {
            packet.extend_from_slice(&val.to_le_bytes());
        }
        for &val in detail_slice {
            packet.extend_from_slice(&val.to_le_bytes());
        }
        for &val in &inputs.live_input {
            packet.extend_from_slice(&val.to_le_bytes());
        }

        packet
    }

    /// Processes a single command byte (`1`, `2`, `3`, or `0`/other) received on the analysis pipe,
    /// returning the exact wire bytes to send back (`AnalysisLink::run()` in `analysis_link.cpp`).
    pub fn handle_command(
        &mut self,
        command: u8,
        pid: u32,
        working_set_bytes: u64,
        cpu_hundred_ns: u64,
        inputs: Option<&AnalysisSnapshotInputs>,
    ) -> Vec<u8> {
        if command == 2 {
            return self.encode_preamp_response().to_vec();
        }
        if command == 3 {
            return Self::encode_process_stats_response(pid, working_set_bytes, cpu_hundred_ns)
                .to_vec();
        }
        self.meters_enabled
            .store(command == 1, Ordering::Release);
        if command != 1 {
            return Vec::new();
        }

        let active = self.active.load(Ordering::Acquire);
        let packet = match (active, inputs) {
            (true, Some(inp)) => self.snapshot(inp),
            _ => Vec::new(),
        };
        // UINT32_MAX means the callback explicitly reported no valid audio;
        // zero only means a complete FFT window has not arrived yet.
        let size: u32 = if active {
            packet.len() as u32
        } else {
            u32::MAX
        };
        let mut out = Vec::with_capacity(4 + packet.len());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&packet);
        out
    }
}

// ============================================================================
// 3. Machine-Wide Engine Paths Resolver (`paths.h` & `paths.cpp`)
// ============================================================================

/// Default owner pipe name (`\\.\pipe\FluidEQ-Engine-Owner`).
pub const DEFAULT_OWNER_PIPE_NAME: &str = r"\\.\pipe\FluidEQ-Engine-Owner";

/// Default ProgramData fallback root on Windows (`C:\ProgramData\FluidEQ\engine`).
pub const DEFAULT_PROGRAM_DATA_ENGINE_ROOT: &str = r"C:\ProgramData\FluidEQ\engine";

/// Exact port of `trim_separators` in `/native/system-apo/src/paths.cpp`:
/// Removes trailing `\` or `/` characters while keeping a 3-character drive root (`C:\`) intact.
pub fn trim_separators(path: &str) -> String {
    let mut out = path.to_string();
    while out.len() > 3 && (out.ends_with('\\') || out.ends_with('/')) {
        out.pop();
    }
    out
}

/// Exact port of `parent_of` in `/native/system-apo/src/paths.cpp`:
/// - `C:\ProgramData`'s parent is `C:\`, not `C:` (which names the per-process current directory on that drive).
/// - Paths with `cut < 3` that are not drive-root children return `""`.
pub fn parent_of(path: &str) -> String {
    let Some(cut) = path.rfind(['\\', '/']) else {
        return String::new();
    };
    let bytes = path.as_bytes();
    if cut == 2 && bytes.len() > 3 && bytes[1] == b':' {
        return path[..3].to_string();
    }
    if cut < 3 {
        return String::new();
    }
    path[..cut].to_string()
}

/// Exact port of `is_directory` in `/native/system-apo/src/paths.cpp`.
pub fn is_directory(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    Path::new(path).is_dir()
}

/// Testable core of `deepest_existing` in `/native/system-apo/src/paths.cpp`:
/// Walks upward from `trim_separators(path)` via `parent_of` until `check_dir` returns `true`.
pub fn deepest_existing_with<F>(path: &str, mut check_dir: F) -> String
where
    F: FnMut(&str) -> bool,
{
    let mut at = trim_separators(path);
    while !at.is_empty() {
        if check_dir(&at) {
            return at;
        }
        at = parent_of(&at);
    }
    String::new()
}

/// Exact port of `deepest_existing` in `/native/system-apo/src/paths.cpp`.
pub fn deepest_existing(path: &str) -> String {
    deepest_existing_with(path, is_directory)
}

/// Exact port of `engine_root` in `/native/system-apo/src/paths.cpp`:
/// Checks `FLUIDEQ_ENGINE_ROOT` override first, then `%ProgramData%\FluidEQ\engine`.
pub fn engine_root() -> String {
    if let Ok(override_root) = std::env::var("FLUIDEQ_ENGINE_ROOT") {
        if !override_root.is_empty() {
            return trim_separators(&override_root);
        }
    }
    if let Ok(program_data) = std::env::var("ProgramData") {
        if !program_data.is_empty() {
            return format!("{}\\FluidEQ\\engine", trim_separators(&program_data));
        }
    }
    DEFAULT_PROGRAM_DATA_ENGINE_ROOT.to_string()
}

/// Exact port of `config_dir` (`<engine root>\config`) in `/native/system-apo/src/paths.cpp`.
pub fn config_dir() -> String {
    let root = engine_root();
    if root.is_empty() {
        String::new()
    } else {
        format!("{}\\config", root)
    }
}

/// Exact port of `log_path` (`<engine root>\engine.log`) in `/native/system-apo/src/paths.cpp`.
pub fn log_path() -> String {
    let root = engine_root();
    if root.is_empty() {
        String::new()
    } else {
        format!("{}\\engine.log", root)
    }
}

/// Exact port of `owner_pipe_name` in `/native/system-apo/src/paths.cpp`.
pub fn owner_pipe_name() -> String {
    if let Ok(override_name) = std::env::var("FLUIDEQ_ENGINE_OWNER_PIPE") {
        if !override_name.is_empty() {
            return override_name;
        }
    }
    DEFAULT_OWNER_PIPE_NAME.to_string()
}

// ============================================================================
// 4. Bounded 1 MB Support Log Writer (`log.h` & `log.cpp`)
// ============================================================================

/// 1 MB log file ceiling (`kMaxLogBytes = 1024 * 1024` in `/native/system-apo/src/log.cpp`).
/// Prevents a user dragging a band for a minute from filling the system drive.
pub const MAX_ENGINE_LOG_BYTES: u64 = 1024 * 1024;

/// Empty endpoint constant (`no_endpoint()` in `log.h`), formatted as `{no endpoint}` by `ApoEngineLog`.
pub const fn no_endpoint() -> &'static str {
    ""
}

/// Exact port of `file_name_of` in `/native/system-apo/src/log.cpp`:
/// Returns the last component after `\` or `/`, or the whole path when no separator is present.
pub fn file_name_of(path: &str) -> &str {
    match path.rfind(['\\', '/']) {
        Some(cut) => &path[cut + 1..],
        None => path,
    }
}

/// Exact port of `decibels` in `/native/system-apo/src/log.cpp`:
/// Formats a double with one decimal (`%.1f`) without locale-sensitive streams.
pub fn decibels(value: f64) -> String {
    if !value.is_finite() {
        return "?".to_string();
    }
    format!("{:.1}", value)
}

/// Formats a 16-byte GUID `(data1, data2, data3, data4)` as `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`
/// matching `StringFromGUID2` in `guid_text` (`log.cpp`).
pub fn guid_text(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        data1,
        data2,
        data3,
        data4[0],
        data4[1],
        data4[2],
        data4[3],
        data4[4],
        data4[5],
        data4[6],
        data4[7]
    )
}

/// Exact port of `Log` in `/native/system-apo/src/log.h/.cpp`.
/// Appends timestamped, PID-tagged, endpoint-tagged lines (`\r\n`) and truncates the file
/// whenever its existing size exceeds `MAX_ENGINE_LOG_BYTES` (1 MB).
#[derive(Debug)]
pub struct ApoEngineLog {
    mutex: Mutex<()>,
    path: String,
    tag: String,
}

/// Alias matching `fluideq_engine::Log` in `/native/system-apo/src/log.h`.
pub type EngineLog = ApoEngineLog;

impl ApoEngineLog {
    pub fn new(path: impl AsRef<Path>, endpoint_guid: &str) -> Self {
        Self::with_path(path.as_ref().to_string_lossy().into_owned(), endpoint_guid)
    }

    pub fn for_default_path(endpoint_guid: &str) -> Self {
        Self::with_path(log_path(), endpoint_guid)
    }

    pub fn with_path(path: String, endpoint_guid: &str) -> Self {
        let tag = if endpoint_guid.is_empty() {
            "{no endpoint} ".to_string()
        } else {
            format!("{} ", endpoint_guid)
        };
        Self {
            mutex: Mutex::new(()),
            path,
            tag,
        }
    }

    /// Formats a single log line: `<utc_timestamp> pid=<pid> <tag><message>\r\n`.
    pub fn format_line(&self, utc_timestamp: &str, pid: u32, message: &str) -> String {
        let sep = if utc_timestamp.ends_with(' ') { "" } else { " " };
        format!("{}{}pid={} {}{}\r\n", utc_timestamp, sep, pid, self.tag, message)
    }

    /// Writes `message` to `self.path`, truncating the file first if its size exceeds `1 MB` (`kMaxLogBytes`).
    /// Never panics or propagates errors (`noexcept` contract).
    pub fn write_with_metadata(&self, utc_timestamp: &str, pid: u32, message: &str) {
        if self.path.is_empty() {
            return;
        }
        let line = self.format_line(utc_timestamp, pid, message);
        let Ok(_guard) = self.mutex.lock() else {
            return;
        };
        let path_obj = Path::new(&self.path);
        if let Ok(meta) = fs::metadata(path_obj) {
            if meta.len() > MAX_ENGINE_LOG_BYTES {
                let _ = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(path_obj);
            }
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path_obj) {
            let _ = file.write_all(line.as_bytes());
        }
    }

    pub fn write(&self, message: &str) {
        self.write_with_metadata("2026-11-05T00:00:00.000Z ", std::process::id(), message);
    }
}

/// Exact port of `trace(endpoint, message)` in `/native/system-apo/src/log.cpp`.
pub fn trace(endpoint_guid: &str, message: &str) {
    ApoEngineLog::for_default_path(endpoint_guid).write(message);
}

/// Exact port of `trace` in `/native/system-apo/src/log.cpp` with explicit log file path.
pub fn trace_in_path(log_file_path: &str, endpoint_guid: &str, message: &str) {
    ApoEngineLog::with_path(log_file_path.to_string(), endpoint_guid).write(message);
}

/// Exact port of `trace_built` in `/native/system-apo/src/log.h`:
/// Executes `build` inside the same no-throw boundary as the log write.
pub fn trace_built_in_path<F>(log_file_path: &str, endpoint_guid: &str, build: F)
where
    F: FnOnce() -> String + std::panic::UnwindSafe,
{
    if let Ok(msg) = std::panic::catch_unwind(build) {
        trace_in_path(log_file_path, endpoint_guid, &msg);
    }
}

// ============================================================================
// 5. Process-Wide Status Board & Atomic Status Writer (`status_file.h` & `status_file.cpp`)
// ============================================================================

struct StatusHolder {
    instances: u32,
    carried: Arc<AtomicBool>,
    timing: Arc<BlockTiming>,
}

impl Default for StatusHolder {
    fn default() -> Self {
        Self {
            instances: 0,
            carried: Arc::new(AtomicBool::new(false)),
            timing: Arc::new(BlockTiming::new()),
        }
    }
}

static STATUS_BOARD: OnceLock<Mutex<BTreeMap<String, StatusHolder>>> = OnceLock::new();

fn status_board() -> &'static Mutex<BTreeMap<String, StatusHolder>> {
    STATUS_BOARD.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Exact port of `output_carried_flag` in `/native/system-apo/src/status_file.cpp`.
/// Shared by every APO instance running on `endpoint` in this process; resets when the last
/// instance on the output leaves.
pub fn output_carried_flag(endpoint: &str) -> Arc<AtomicBool> {
    let mut board = status_board().lock().unwrap_or_else(|e| e.into_inner());
    board
        .entry(endpoint.to_string())
        .or_default()
        .carried
        .clone()
}

/// Exact port of `output_block_timing` in `/native/system-apo/src/status_file.cpp`.
/// Shared `BlockTiming` across every APO instance on `endpoint` in this process.
pub fn output_block_timing(endpoint: &str) -> Arc<BlockTiming> {
    let mut board = status_board().lock().unwrap_or_else(|e| e.into_inner());
    board
        .entry(endpoint.to_string())
        .or_default()
        .timing
        .clone()
}

/// Exact port of `write_status` in `/native/system-apo/src/status_file.cpp`:
/// Atomically writes `<root>/status-<endpoint>.json` via a PID-scoped temporary file
/// (`status-<endpoint>.json.<pid>.tmp`) and renames it over the target so the app never reads
/// a half-written JSON status file.
pub fn write_status_in_root(
    root: &Path,
    status: &EngineStatusReport,
    pid: u32,
    utc_iso: &str,
) -> Result<(), String> {
    if root.as_os_str().is_empty() || status.endpoint.is_empty() {
        return Err("no engine root or endpoint: error 0".to_string());
    }
    if !root.is_dir() {
        return Err("engine root is not a directory: error 3".to_string());
    }

    let file_name = format!("status-{}.json", status.endpoint);
    let path = root.join(&file_name);
    let aside = root.join(format!("{}.{}.tmp", file_name, pid));
    let text = format_engine_status_json(status, pid, utc_iso);

    if let Err(err) = fs::write(&aside, text.as_bytes()) {
        let _ = fs::remove_file(&aside);
        return Err(format!(
            "write the temporary: error {}",
            err.raw_os_error().unwrap_or(0)
        ));
    }

    if let Err(err) = fs::rename(&aside, &path) {
        let _ = fs::remove_file(&aside);
        return Err(format!(
            "rename into place: error {}",
            err.raw_os_error().unwrap_or(0)
        ));
    }

    Ok(())
}

/// Convenience wrapper for `write_status` using `engine_root()` and current process ID.
pub fn write_status(status: &EngineStatusReport) -> Result<(), String> {
    let root = engine_root();
    write_status_in_root(
        Path::new(&root),
        status,
        std::process::id(),
        "2026-11-05T00:00:00.000Z",
    )
}

/// Exact port of `StatusShare` in `/native/system-apo/src/status_file.h/.cpp`.
/// Counts instances running each output process-wide so only the last instance out
/// writes `"locked":false` and clears the endpoint's `carried` and `BlockTiming` state.
pub struct StatusShare {
    endpoint: String,
    root_override: Option<std::path::PathBuf>,
}

impl Default for StatusShare {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusShare {
    pub fn new() -> Self {
        Self {
            endpoint: String::new(),
            root_override: None,
        }
    }

    pub fn with_root(root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            endpoint: String::new(),
            root_override: Some(root.into()),
        }
    }

    fn effective_root(&self) -> std::path::PathBuf {
        match &self.root_override {
            Some(r) => r.clone(),
            None => std::path::PathBuf::from(engine_root()),
        }
    }

    /// Exact port of `StatusShare::publish`:
    /// Counts this instance in on the first call (even if the disk write fails, because the
    /// instance is running the output whether or not the app has been told) and writes `status`.
    pub fn publish(&mut self, status: &EngineStatusReport) -> Result<(), String> {
        let mut board = status_board().lock().unwrap_or_else(|e| e.into_inner());
        if self.endpoint.is_empty() && !status.endpoint.is_empty() {
            let endpoint = status.endpoint.clone();
            let holder = board.entry(endpoint.clone()).or_default();
            holder.instances += 1;
            self.endpoint = endpoint;
        }
        write_status_in_root(
            &self.effective_root(),
            status,
            std::process::id(),
            "2026-11-05T00:00:00.000Z",
        )
    }

    /// Exact port of `StatusShare::leave`:
    /// Decrements this endpoint's instance count; only the last instance out removes the holder
    /// (resetting `carried` and `BlockTiming`) and writes an unlocked status file.
    pub fn leave(&mut self) -> Result<(), String> {
        if self.endpoint.is_empty() {
            return Ok(());
        }
        let mut board = status_board().lock().unwrap_or_else(|e| e.into_inner());
        let endpoint = std::mem::take(&mut self.endpoint);
        if let Some(holder) = board.get_mut(&endpoint) {
            holder.instances = holder.instances.saturating_sub(1);
            if holder.instances > 0 {
                return Ok(());
            }
        } else {
            return Ok(());
        }

        // Last instance out removes the entry and writes "locked": false
        board.remove(&endpoint);
        let released = EngineStatusReport {
            endpoint,
            locked: false,
            processing: false,
            carried: false,
            owner: true,
            ..Default::default()
        };
        write_status_in_root(
            &self.effective_root(),
            &released,
            std::process::id(),
            "2026-11-05T00:00:00.000Z",
        )
    }
}

impl Drop for StatusShare {
    fn drop(&mut self) {
        let _ = self.leave();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_source_analysis_and_isolates_unrelated_endpoints() {
        let mut noise_tokens = Vec::new();
        for i in 0..FEQ_DENOISE_PROFILE_BANDS {
            noise_tokens.push(format!("{}", -75.0 + (i as f64) * 0.25));
        }
        noise_tokens.push("-68.5".to_string()); // floor_dbfs
        noise_tokens.push("60".to_string()); // hum_hz
        noise_tokens.push("2".to_string()); // hum_partial_count
        for i in 0..FEQ_DENOISE_MAX_HUM_PARTIALS {
            noise_tokens.push(format!("{}", 60.0 * ((i + 1) as f64)));
        }
        for _ in 0..FEQ_DENOISE_MAX_HUM_PARTIALS {
            noise_tokens.push("6.5".to_string());
        }

        let sidecar = format!(
            "# FluidEQ source analysis\r\n\
             version=1\r\n\
             kind=library\r\n\
             owner=engine\r\n\
             source=0123456789abcdef\r\n\
             epoch=42\r\n\
             revision=108\r\n\
             endpoint={{11111111-2222-3333-4444-555555555555}}\r\n\
             level=-14.2\r\n\
             peak=-1.1\r\n\
             noise={}\r\n\
             voice=available\r\n\
             voiceModel=C:\\Program Files\\FluidEQ\\models\\dpdfnet.onnx\r\n",
            noise_tokens.join(" ")
        );

        let mut parsed = parse_source_analysis(&sidecar).expect("valid library source analysis");
        assert!(parsed.library);
        assert!(parsed.engine_owner);
        assert_eq!(parsed.source, 0x0123_4567_89ab_cdef);
        assert_eq!(parsed.epoch, 42);
        assert_eq!(parsed.revision, 108);
        assert!(parsed.has_level);
        assert_eq!(parsed.level, -14.2);
        assert_eq!(parsed.peak, -1.1);
        assert!(parsed.noise.is_some());
        assert_eq!(parsed.noise.unwrap().hum_partial_count, 2);

        // Revision changes must not change processing_identity()
        let id_rev_108 = parsed.processing_identity();
        parsed.revision = 999;
        assert_eq!(parsed.processing_identity(), id_rev_108);

        // Library endpoint and its secondary split receiver get the library analysis
        let for_primary = source_for_endpoint(
            Some(&parsed),
            "{11111111-2222-3333-4444-555555555555}",
            "",
            true,
        );
        assert!(for_primary.library);

        let for_split_receiver = source_for_endpoint(
            Some(&parsed),
            "{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}",
            "{11111111-2222-3333-4444-555555555555}",
            true,
        );
        assert!(for_split_receiver.library);

        // Unrelated endpoint gets isolated live analysis
        let for_unrelated = source_for_endpoint(
            Some(&parsed),
            "{99999999-8888-7777-6666-555555555555}",
            "",
            true,
        );
        assert!(!for_unrelated.library);
        assert!(!for_unrelated.has_level);
        assert!(for_unrelated.identity.starts_with("live|model="));
    }

    #[test]
    fn analysis_link_builds_hello_preamp_stats_and_872_byte_snapshot_frames() {
        let mut link = AnalysisLink::new("{11111111-2222-3333-4444-555555555555}", 48_000, 2);
        let hello = link.build_hello_packet();
        assert_eq!(&hello[..38], b"{11111111-2222-3333-4444-555555555555}");
        assert_eq!(
            hello[39],
            ANALYSIS_HELLO_CAP_PREAMP | ANALYSIS_HELLO_CAP_PROCESS_STATS
        );
        assert_eq!(u32::from_le_bytes(hello[40..44].try_into().unwrap()), 48_000);

        link.set_output_gain(-4.5);
        link.output_enabled.store(true, Ordering::Relaxed);
        link.output_active.store(true, Ordering::Relaxed);
        let preamp_pkt = link.handle_command(2, 1234, 0, 0, None);
        assert_eq!(preamp_pkt.len(), 16);
        assert_eq!(u32::from_le_bytes(preamp_pkt[0..4].try_into().unwrap()), 12);
        assert_eq!(
            u32::from_le_bytes(preamp_pkt[4..8].try_into().unwrap()),
            ANALYSIS_PREAMP_MAGIC
        );
        assert_eq!(
            f32::from_le_bytes(preamp_pkt[8..12].try_into().unwrap()),
            -4.5f32
        );
        assert_eq!(u32::from_le_bytes(preamp_pkt[12..16].try_into().unwrap()), 3);

        // Inactive callback returns UINT32_MAX size marker on command 1
        let inactive_resp = link.handle_command(1, 1234, 0, 0, None);
        assert_eq!(inactive_resp.len(), 4);
        assert_eq!(
            u32::from_le_bytes(inactive_resp[0..4].try_into().unwrap()),
            u32::MAX
        );
        assert!(link.meters_enabled());

        // Active callback with EQ spectrum and detail window returns 872-byte header + payload
        link.active.store(true, Ordering::Release);
        let mut inputs = AnalysisSnapshotInputs::default();
        inputs.stage_spectra[1] = Some(vec![-30.0f32; FEQ_METER_BINS]);
        inputs.eq_detail = Some(vec![-32.0f32; FEQ_METER_DETAIL_BINS]);
        let active_resp = link.handle_command(1, 1234, 0, 0, Some(&inputs));
        let expected_payload = 872
            + FEQ_METER_BINS * 4
            + FEQ_METER_DETAIL_BINS * 4
            + 5 * 4;
        assert_eq!(
            u32::from_le_bytes(active_resp[0..4].try_into().unwrap()) as usize,
            expected_payload
        );
        assert_eq!(active_resp.len(), 4 + expected_payload);
    }

    #[test]
    fn parent_of_preserves_drive_root_and_status_share_counts_instances() {
        assert_eq!(parent_of(r"C:\ProgramData\FluidEQ\engine"), r"C:\ProgramData\FluidEQ");
        assert_eq!(parent_of(r"C:\ProgramData"), r"C:\");
        assert_eq!(parent_of(r"C:\"), "");
        assert_eq!(file_name_of(r"C:\ProgramData\FluidEQ\engine.log"), "engine.log");
        assert_eq!(decibels(-3.456), "-3.5");

        let temp_dir = std::env::temp_dir().join(format!("fluideq_status_test_{}", std::process::id()));
        let _ = fs::create_dir_all(&temp_dir);

        let ep = "{22222222-3333-4444-5555-666666666666}".to_string();
        let carried = output_carried_flag(&ep);
        carried.store(true, Ordering::Release);

        let mut share1 = StatusShare::with_root(&temp_dir);
        let mut share2 = StatusShare::with_root(&temp_dir);

        let locked_status = EngineStatusReport {
            endpoint: ep.clone(),
            locked: true,
            processing: true,
            carried: true,
            owner: true,
            room: "off".to_string(),
            ..Default::default()
        };
        share1.publish(&locked_status).expect("share1 publish");
        share2.publish(&locked_status).expect("share2 publish");

        // Leaving first instance keeps the output locked and preserves carried flag
        share1.leave().expect("share1 leave");
        let status_path = temp_dir.join(format!("status-{}.json", ep));
        let after_first = fs::read_to_string(&status_path).expect("status file exists");
        assert!(after_first.contains("\"locked\":true"));
        assert!(output_carried_flag(&ep).load(Ordering::Acquire));

        // Leaving last instance writes "locked":false and resets carried flag for next chain
        share2.leave().expect("share2 leave");
        let after_last = fs::read_to_string(&status_path).expect("status file after last leave");
        assert!(after_last.contains("\"locked\":false"));
        assert!(!output_carried_flag(&ep).load(Ordering::Acquire));

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
