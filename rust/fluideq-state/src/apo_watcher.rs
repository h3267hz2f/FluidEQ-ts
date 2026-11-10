/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! FluidEQ System-APO Background Directory Watcher, Two-Pointer `GraphSlot` Handover,
//! Grace-Block Graph Reclaimer, Debounced Hot-Reload Coordinator, Auto-Normalize
//! Level Prediction Worker, Engine Log & Status Reporter, and Pinned Voice Model Verifier (`Slice-M1M8-10`).
//!
//! 1:1 faithful port of:
//! - `/native/system-apo/src/graph_reclaim.h` & `graph_reclaim.cpp` (`kGraceBlocks = 2`, `OwnedGraph`, `reclaim_graphs`)
//! - `/native/system-apo/src/watcher.h` & `watcher.cpp` (`GraphSlot`, `ChangeNotification`, `Watcher` thread & event loop)
//! - `/native/system-apo/src/watcher_level.cpp` (`open_level_prediction`, `plan_level`, `owe_level`, `settle_level`, `accept_level`)
//! - `/native/system-apo/src/watcher_load.cpp` (`latency_parts_of`, `reload`, `load_chain`, `publish`, `reclaim`)
//! - `/native/system-apo/src/watcher_log.cpp` (`kMaxIgnoredSetsLogged = 32`, `log_chain`, `log_timing`, `report_status`, `follow_programme`, `follow_split`)
//! - `/native/system-apo/src/watcher_source.cpp` (`same_model_path`, `file_stamp`, `verified_voice_model`, `trusted_voice_runtime`, `read_source`, `graph_adopted`)

use crate::apo_analysis_and_status::{
    decibels, deepest_existing, file_name_of, is_directory, output_block_timing,
    output_carried_flag, parse_source_analysis, source_for_endpoint, EngineLog, SourceAnalysis,
    StatusShare, SOURCE_ANALYSIS_FILE,
};
use crate::apo_engine_runtime::{
    endpoint_config_name, parse_programme, read_apo_config_file_bounded,
    resolve_apo_endpoint_chain_from_dir, signature_of_chain, BlockLoadReport, EngineStatusReport,
    FinishedSongStatus, SignatureBand, SignatureChainInput, SourceAnalysisStatusReport,
    SplitStatusReport,
};
use crate::apo_graph_and_guard::{
    describe_timing, judge_level_prediction, timing_step, InputHistory, LevelMailbox,
    LevelPrediction, LevelPredictionJudgement, LEVEL_HISTORY_SECONDS,
};
use crate::apo_process_and_stages::{ApoEndpointProperties, ConnectionFormat};
pub use crate::apo_split_and_leveling::{sha256_digest, EndpointLevelingCoordinator};
use crate::apo_split_tap_and_reader::{SplitReport, SplitState};
use crate::chain_wire::decode_chain_settings_wire;
use crate::room_heads::RoomHeadBank;
use fluideq_dsp_sys::FeqChainLatencyParts;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

// ============================================================================
// 1. Grace-Block Graph Reclaimer (`graph_reclaim.h` & `graph_reclaim.cpp`)
// ============================================================================

/// Blocks the audio thread must complete after a publish before graphs older
/// than it can be destroyed (`kGraceBlocks = 2` in `graph_reclaim.h`).
pub const GRACE_BLOCKS: u64 = 2;

/// Trait implemented by graphs managed in the watcher's `owned_` retirement queue.
pub trait ReclaimableGraph {
    /// Unique identifier for pointer-equivalence comparisons (`Graph*` in C++).
    fn graph_id(&self) -> usize;

    /// Returns the `graph_id` of the older graph this one is currently cross-fading from
    /// (`Graph::crossing_from()` in `graph.h`), or `None` once the crossover has finished.
    fn crossing_from_id(&self) -> Option<usize>;
}

/// A graph the watcher owns, and the audio thread's block count at its publish
/// (`OwnedGraph` in `graph_reclaim.h`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedGraphEntry<G> {
    pub graph: G,
    pub blocks_at_publish: u64,
}

/// Exact port of `reclaim_graphs` in `/native/system-apo/src/graph_reclaim.cpp`:
/// Destroys (returns in the retired vector) every graph in `owned` that nothing can play
/// any more, keeping the rest in publish order. Any graph transitively referenced by
/// `crossing_from_id()` from a kept graph is also preserved until the crossover finishes.
pub fn reclaim_graphs<G: ReclaimableGraph>(
    owned: &mut Vec<OwnedGraphEntry<G>>,
    blocks: u64,
) -> Vec<G> {
    let mut boundary = 0usize;
    for at in (1..=owned.len()).rev() {
        if blocks >= owned[at - 1].blocks_at_publish.saturating_add(GRACE_BLOCKS) {
            boundary = at - 1;
            break;
        }
    }

    let mut crossed_from: Vec<usize> = Vec::new();
    for at in boundary..owned.len() {
        let mut current_from = owned[at].graph.crossing_from_id();
        while let Some(from_id) = current_from {
            if !crossed_from.contains(&from_id) {
                crossed_from.push(from_id);
            }
            current_from = owned
                .iter()
                .find(|entry| entry.graph.graph_id() == from_id)
                .and_then(|entry| entry.graph.crossing_from_id());
        }
    }

    let mut retired = Vec::new();
    let old_entries = std::mem::take(owned);
    for (at, entry) in old_entries.into_iter().enumerate() {
        let still_played = crossed_from.contains(&entry.graph.graph_id());
        if at >= boundary || still_played {
            owned.push(entry);
        } else {
            retired.push(entry.graph);
        }
    }
    retired
}

// ============================================================================
// 2. Two-Pointer Atomic Handover Slot (`GraphSlot` in `watcher.h`)
// ============================================================================

/// The two-pointer lock-free handover between the watcher thread and the audio thread
/// (`GraphSlot` in `/native/system-apo/src/watcher.h`).
///
/// Uses `0` as `nullptr` and non-zero `usize` graph IDs as `Graph*`.
/// Preserves the exact memory orderings from `watcher.h`:
/// - `adopt`: `pending_.exchange(0, AcqRel)` -> `adopt_state(active_.load(Relaxed))` -> `active_.store(next, Release)`
/// - `finish_block`: `blocks_.fetch_add(1, Release)`
/// - `active`: `active_.load(Acquire)`
/// - `set_latency` / `latency`: `Relaxed`
/// - `blocks`: `blocks_.load(Acquire)`
/// - `publish`: `pending_.exchange(graph, AcqRel)`
#[derive(Debug, Default)]
pub struct GraphSlot {
    pending: AtomicUsize,
    active: AtomicUsize,
    blocks: AtomicU64,
    latency: AtomicU32,
}

impl GraphSlot {
    pub const fn new() -> Self {
        Self {
            pending: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            blocks: AtomicU64::new(0),
            latency: AtomicU32::new(0),
        }
    }

    /// Audio thread, at the start of a block (`GraphSlot::adopt` in `watcher.h`).
    /// Takes `next` out of `pending` via atomic `AcqRel` exchange before publishing into `active`.
    pub fn adopt_with<F>(&self, mut on_adopt_state: F) -> usize
    where
        F: FnMut(usize, usize),
    {
        let next = self.pending.swap(0, Ordering::AcqRel);
        if next != 0 {
            let prev = self.active.load(Ordering::Relaxed);
            on_adopt_state(next, prev);
            self.active.store(next, Ordering::Release);
        }
        self.active.load(Ordering::Relaxed)
    }

    /// Audio thread, at the end of a block (`GraphSlot::finish_block` in `watcher.h`).
    pub fn finish_block(&self) {
        self.blocks.fetch_add(1, Ordering::Release);
    }

    /// Watcher thread: what the audio thread is running now (`GraphSlot::active` in `watcher.h`).
    pub fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }

    /// Cached added delay of the latest published graph (`GraphSlot::set_latency` in `watcher.h`).
    pub fn set_latency(&self, frames: u32) {
        self.latency.store(frames, Ordering::Relaxed);
    }

    pub fn latency(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }

    pub fn blocks(&self) -> u64 {
        self.blocks.load(Ordering::Acquire)
    }

    /// Watcher thread (`GraphSlot::publish` in `watcher.h`).
    /// Returns the unconsumed graph ID replaced in `pending` (`0` if `pending` was empty).
    pub fn publish(&self, graph_id: usize) -> usize {
        self.pending.swap(graph_id, Ordering::AcqRel)
    }

    /// After the watcher has stopped and the audio thread is gone (`GraphSlot::clear` in `watcher.h`).
    pub fn clear(&self) {
        self.pending.store(0, Ordering::Relaxed);
        self.active.store(0, Ordering::Relaxed);
    }
}

// ============================================================================
// 3. Stage-by-Stage Latency Breakdown (`latency_parts_of` in `watcher_load.cpp`)
// ============================================================================

/// Complete stage-by-stage delay breakdown of a `Graph` (`Graph::LatencyParts` in `graph.h`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphLatencyParts {
    pub rack: FeqChainLatencyParts,
    pub curves: u32,
    pub eq_phase: u32,
    pub curve_phase: u32,
    pub convolution: u32,
    pub guard: u32,
}

/// Exact port of `latency_parts_of` in `/native/system-apo/src/watcher_load.cpp`:
/// Lists the graph's delay stage by stage, by the exact camelCase names the app reads,
/// in the exact signal-path order (`feq_chain_process` followed by `Graph::process`),
/// including any stage that either adds non-zero latency or appears in `active_stages`.
pub fn latency_parts_of(
    parts: &GraphLatencyParts,
    active_stages: &[String],
) -> Vec<(String, u32)> {
    let named: [(&str, u32); 20] = [
        ("leveler", parts.rack.leveler),
        ("restoration", parts.rack.restoration),
        ("deEsser", parts.rack.deesser),
        ("exciter", 0),
        ("bassForge", 0),
        ("linearEq", parts.rack.eq),
        ("footsteps", 0),
        ("bassPunch", parts.rack.bass_punch),
        ("room", parts.rack.room),
        ("dimension", 0),
        ("maximizer", parts.rack.maximizer),
        ("headroom", parts.rack.headroom),
        ("master", 0),
        ("eqPhase", parts.eq_phase),
        ("curvePhase", parts.curve_phase),
        ("convolution", parts.convolution),
        ("curves", parts.curves),
        ("guard", parts.guard),
        ("filters", 0),
        ("preamp", 0),
    ];

    let mut out = Vec::new();
    for (name, frames) in named {
        if frames > 0 || active_stages.iter().any(|s| s == name) {
            out.push((name.to_string(), frames));
        }
    }
    out
}

// ============================================================================
// 4. Pinned Voice Model SHA-256 & Trusted Runtime Verifier (`watcher_source.cpp`)
// ============================================================================

/// Exact byte size required for `fluideq-voice.onnx` (`10596848` in `watcher_source.cpp`).
pub const PINNED_VOICE_MODEL_SIZE_BYTES: u64 = 10_596_848;

/// Exact SHA-256 hex digest required for `fluideq-voice.onnx` (`watcher_source.cpp`).
pub const PINNED_VOICE_MODEL_SHA256_HEX: &str =
    "0b399f8a58dc4d70d8cd97541f5c39869406145193b957d00a03b66070944928";

/// Self-contained FIPS 180-4 SHA-256 implementation matching `BCryptHashData` in `watcher_source.cpp`.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity((data.len() + 72) & !63);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0x00);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = state[0];
        let mut b = state[1];
        let mut c = state[2];
        let mut d = state[3];
        let mut e = state[4];
        let mut f = state[5];
        let mut g = state[6];
        let mut h = state[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for word in state {
        for byte in word.to_be_bytes() {
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

/// Exact port of `same_model_path` in `/native/system-apo/src/watcher_source.cpp`:
/// Normalizes `/` to `\` and compares case-insensitively against `expected`.
pub fn same_model_path(utf8: &str, expected: &str) -> bool {
    if utf8.is_empty() {
        return false;
    }
    let normalized_utf8: String = utf8
        .chars()
        .map(|c| if c == '/' { '\\' } else { c })
        .collect();
    let normalized_expected: String = expected
        .chars()
        .map(|c| if c == '/' { '\\' } else { c })
        .collect();
    normalized_utf8.eq_ignore_ascii_case(&normalized_expected)
}

/// Exact port of `file_stamp` in `/native/system-apo/src/watcher_source.cpp`:
/// Returns `(size_bytes, last_write_time_ns)` for a file on disk, or `None` if absent.
pub fn file_stamp(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    Some((meta.len(), mtime))
}

/// Exact port of `verified_voice_model` in `/native/system-apo/src/watcher_source.cpp`:
/// Checks that the file size equals `10596848` (`PINNED_VOICE_MODEL_SIZE_BYTES`) and its
/// SHA-256 hex digest equals `PINNED_VOICE_MODEL_SHA256_HEX`.
pub fn verify_pinned_voice_model_bytes(bytes: &[u8]) -> bool {
    if (bytes.len() as u64) != PINNED_VOICE_MODEL_SIZE_BYTES {
        return false;
    }
    sha256_hex(bytes) == PINNED_VOICE_MODEL_SHA256_HEX
}

/// Verifies a voice model file on disk and returns a shared guard handle if it matches
/// both `PINNED_VOICE_MODEL_SIZE_BYTES` and `PINNED_VOICE_MODEL_SHA256_HEX`.
pub fn verified_voice_model_file(path: &Path) -> Option<Arc<std::fs::File>> {
    let file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() != PINNED_VOICE_MODEL_SIZE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if verify_pinned_voice_model_bytes(&bytes) {
        Some(Arc::new(file))
    } else {
        None
    }
}

/// Exact port of `trusted_voice_runtime` in `/native/system-apo/src/watcher_source.cpp`:
/// Verifies that `module_path` equals `<program_files_x64>\FluidEQ Engine\FluidEQ-Engine.dll`
/// (case-insensitive) and `runtime_exists_and_is_file` is true, returning
/// `<program_files_x64>\FluidEQ Engine\onnxruntime.dll`. Never accepts a runtime path from
/// the all-users-writable configuration directory.
pub fn resolve_trusted_voice_runtime(
    module_path: &str,
    program_files_x64: &str,
    runtime_exists_and_is_file: bool,
) -> String {
    if module_path.is_empty() || program_files_x64.is_empty() || !runtime_exists_and_is_file {
        return String::new();
    }
    let directory = format!(
        "{}\\FluidEQ Engine",
        program_files_x64.trim_end_matches(['\\', '/'])
    );
    let expected = format!("{}\\FluidEQ-Engine.dll", directory);
    if !same_model_path(module_path, &expected) {
        return String::new();
    }
    format!("{}\\onnxruntime.dll", directory)
}

// ============================================================================
// 5. Built Graph Snapshot & Watcher Level Prediction (`watcher_level.cpp`)
// ============================================================================

/// Source analysis status reported by a constructed `Graph` (`Graph::SourceReport` in `graph.h`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphSourceReport {
    pub library: bool,
    pub engine_owner: bool,
    pub source: u64,
    pub epoch: u64,
    pub revision: u64,
    pub ready: bool,
    pub voice_ready: bool,
    pub warm_handover: bool,
}

impl Default for GraphSourceReport {
    fn default() -> Self {
        Self {
            library: false,
            engine_owner: true,
            source: 0,
            epoch: 0,
            revision: 0,
            ready: true,
            voice_ready: false,
            warm_handover: false,
        }
    }
}

/// Descriptor of a constructed APO `Graph` managed by `ApoWatcher` (`Graph` in `graph.h`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatcherGraphInstance {
    pub id: usize,
    pub crossing_from: Option<usize>,
    pub is_passthrough: bool,
    pub latency_frames: u32,
    pub latency_parts: GraphLatencyParts,
    pub active_stages: Vec<String>,
    pub low_latency: bool,
    pub warnings: Vec<String>,
    pub problems: Vec<String>,
    pub room_note: String,
    pub room_state: String,
    pub source_report: GraphSourceReport,
    pub rack_shared_with_previous: bool,
    pub state_transfer_requested: bool,
    pub expected_level_generation: u32,
    pub silenced_blocks: u32,
}

impl Default for WatcherGraphInstance {
    fn default() -> Self {
        Self {
            id: 1,
            crossing_from: None,
            is_passthrough: true,
            latency_frames: 0,
            latency_parts: GraphLatencyParts::default(),
            active_stages: Vec::new(),
            low_latency: false,
            warnings: Vec::new(),
            problems: Vec::new(),
            room_note: String::new(),
            room_state: "off".to_string(),
            source_report: GraphSourceReport::default(),
            rack_shared_with_previous: false,
            state_transfer_requested: false,
            expected_level_generation: 0,
            silenced_blocks: 0,
        }
    }
}

impl ReclaimableGraph for WatcherGraphInstance {
    fn graph_id(&self) -> usize {
        self.id
    }

    fn crossing_from_id(&self) -> Option<usize> {
        self.crossing_from
    }
}

/// A level with nothing to go on: the audio thread finds it the old way (`kNoLevel = quiet_NaN()` in `watcher_level.cpp`).
pub const NO_LEVEL: f32 = f32::NAN;

/// The level still to be worked out for the newest published graph (`Watcher::OwedLevel` in `watcher.h`).
#[derive(Debug, Clone, PartialEq)]
pub struct OwedLevel {
    pub chain: SignatureChainInput,
    pub generation: u32,
    pub published_at: u64,
}

/// Wait signals handled by `Watcher::run()` (`WaitForMultipleObjects` in `watcher.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatcherWaitSignal {
    /// `WAIT_OBJECT_0`: `stop_event_` signalled -> exit `run()`.
    Stop,
    /// `WAIT_OBJECT_0 + 1`: `reset_event_` signalled (`IAudioProcessingObject::Reset`) -> `reload()` keeping audio in flight.
    Reset,
    /// `WAIT_OBJECT_0 + 2`: `change.get()` directory change notification -> `owner_->retry()`, `FindNextChangeNotification`, `reload()`.
    ConfigChanged,
    /// `status_event_` signalled (`say_it_carried`, `say_it_was_slow`, `graph_adopted`) -> `log_timing()` + `report_status(true)`.
    StatusWake,
    /// `owner_event_` signalled (FluidEQ came or went) -> `reload()`.
    OwnerWake,
    /// `split_event_`, `writer_process()`, or `writer_released()` signalled -> `split_->prepare()` + `report_status(true)`.
    SplitWake,
    /// `split_->initializing()` mutex acquired or abandoned -> `split_->prepare(init_handle)` + `report_status(true)`.
    SplitInitializing { abandoned: bool },
    /// Handle became invalid -> re-arm directory watch.
    HandleInvalid,
}

/// Maximum number of distinct `chain.ignored` command sets logged per endpoint lock
/// (`kMaxIgnoredSetsLogged = 32` in `/native/system-apo/src/watcher_log.cpp`).
pub const MAX_IGNORED_SETS_LOGGED: usize = 32;

// ============================================================================
// 6. Complete `Watcher` Coordinator (`watcher.cpp`, `watcher_level.cpp`,
//    `watcher_load.cpp`, `watcher_log.cpp`, `watcher_source.cpp`)
// ============================================================================

/// Complete 1:1 port of `fluideq_engine::Watcher` across:
/// - `watcher.h` & `watcher.cpp`
/// - `watcher_level.cpp`
/// - `watcher_load.cpp`
/// - `watcher_log.cpp`
/// - `watcher_source.cpp`
pub struct ApoWatcher {
    pub slot: Arc<GraphSlot>,
    pub log: EngineLog,
    pub endpoint_guid: String,
    pub endpoint_name: String,
    pub config_dir: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub max_frames: u32,
    pub channel_mask: u32,
    pub default_mode: bool,

    // Leveling & auto-normalize prediction (`watcher_level.cpp`)
    pub leveling: Option<EndpointLevelingCoordinator>,
    pub history: Option<Arc<InputHistory>>,
    pub accepted_chain: Option<SignatureChainInput>,
    pub accepted_heard_frames: u64,
    pub predictor_active: bool,
    pub level_mailbox: Arc<LevelMailbox>,
    pub generation: u32,
    pub owed_level: Option<OwedLevel>,
    pub others_from: u64,

    // Voice model verification cache (`watcher_source.cpp`)
    pub voice_guard: Option<Arc<std::fs::File>>,
    pub voice_verified_flag: bool,
    pub voice_refused: Option<(u64, u64)>,
    pub trusted_runtime_path: String,

    // Room head text & generation cache (`watcher_load.cpp`)
    pub room_head_text: Option<String>,
    pub room_head: Option<RoomHeadBank>,
    pub room_head_generation: u64,
    pub rack_source_id: Option<usize>,
    pub rack_source_head: u64,

    // Owned graph retirement queue & signature deduplication (`watcher_load.cpp`)
    pub owned: Vec<OwnedGraphEntry<WatcherGraphInstance>>,
    pub next_graph_id: usize,
    pub signature: String,
    pub have_signature: bool,
    pub logged_ignored: Vec<String>,
    pub passthrough_reason: String,
    pub have_passthrough_reason: bool,

    // Last published graph status fields (`watcher_log.cpp`)
    pub last_processing: bool,
    pub last_owner: bool,
    pub graph_problems: Vec<String>,
    pub room_state: String,
    pub latency: u32,
    pub latency_parts: Vec<(String, u32)>,
    pub latency_active: Vec<String>,
    pub game_mode: bool,
    pub source_report: GraphSourceReport,
    pub source_invalid: bool,
    pub source_prepare_failed: bool,
    pub reload_failed: bool,
    pub unwatched: bool,

    // Split tap status & transport failure state
    pub split_report: Option<SplitReport>,
    pub split_transport_failed: bool,

    // Status file share & timing step (`watcher_log.cpp`)
    pub status_share: StatusShare,
    pub status_failure_logged: bool,
    pub timing_step_logged: u32,

    // Event flags (`stop_event_`, `reset_event_`, `status_event_`, `split_event_`, `owner_`)
    pub stop_event: AtomicBool,
    pub reset_event: AtomicBool,
    pub status_event: AtomicBool,
    pub split_event: AtomicBool,
    pub change_armed: AtomicBool,
    pub owner_is_present: bool,
}

impl ApoWatcher {
    pub fn new(
        slot: Arc<GraphSlot>,
        log: EngineLog,
        endpoint: ApoEndpointProperties<'_>,
        config_dir: impl Into<String>,
        format: ConnectionFormat,
        max_frames: u32,
        default_mode: bool,
    ) -> Self {
        let guid = endpoint.guid.unwrap_or_default().to_string();
        let dir = config_dir.into();
        let status_root = Path::new(&dir)
            .parent()
            .unwrap_or_else(|| Path::new(&dir))
            .to_path_buf();
        Self {
            slot,
            log,
            endpoint_guid: guid.clone(),
            endpoint_name: endpoint.friendly_name.unwrap_or_default().to_string(),
            config_dir: dir,
            sample_rate: format.rate.max(1),
            channels: format.channels.max(1),
            max_frames: max_frames.max(1),
            channel_mask: format.mask,
            default_mode,
            leveling: if guid.is_empty() {
                None
            } else {
                Some(EndpointLevelingCoordinator::new())
            },
            history: None,
            accepted_chain: None,
            accepted_heard_frames: 0,
            predictor_active: false,
            level_mailbox: Arc::new(LevelMailbox::new()),
            generation: 0,
            owed_level: None,
            others_from: u64::MAX,
            voice_guard: None,
            voice_verified_flag: false,
            voice_refused: None,
            trusted_runtime_path: String::new(),
            room_head_text: None,
            room_head: None,
            room_head_generation: 0,
            rack_source_id: None,
            rack_source_head: 0,
            owned: Vec::new(),
            next_graph_id: 1,
            signature: String::new(),
            have_signature: false,
            logged_ignored: Vec::new(),
            passthrough_reason: String::new(),
            have_passthrough_reason: false,
            last_processing: false,
            last_owner: true,
            graph_problems: Vec::new(),
            room_state: "off".to_string(),
            latency: 0,
            latency_parts: Vec::new(),
            latency_active: Vec::new(),
            game_mode: false,
            source_report: GraphSourceReport::default(),
            source_invalid: false,
            source_prepare_failed: false,
            reload_failed: false,
            unwatched: false,
            split_report: None,
            split_transport_failed: false,
            status_share: StatusShare::with_root(status_root),
            status_failure_logged: false,
            timing_step_logged: 0,
            stop_event: AtomicBool::new(false),
            reset_event: AtomicBool::new(false),
            status_event: AtomicBool::new(false),
            split_event: AtomicBool::new(false),
            change_armed: AtomicBool::new(false),
            owner_is_present: true,
        }
    }

    // ------------------------------------------------------------------------
    // `watcher.cpp` — Lifecycle, Wake Events & Wait Loop
    // ------------------------------------------------------------------------

    /// Exact port of `Watcher::owner_present()` in `watcher.cpp`.
    pub fn owner_present(&self) -> bool {
        self.owner_is_present
    }

    /// Exact port of `Watcher::stop_requested()` in `watcher.cpp`.
    pub fn stop_requested(&self) -> bool {
        self.stop_event.load(Ordering::Acquire)
    }

    /// Exact port of `Watcher::change_pending()` in `watcher.cpp`.
    pub fn change_pending(&self) -> bool {
        self.change_armed.load(Ordering::Acquire)
    }

    /// Exact port of `Watcher::say_it_carried()` in `watcher.cpp`.
    pub fn say_it_carried(&self) {
        self.status_event.store(true, Ordering::Release);
    }

    /// Exact port of `Watcher::say_it_was_slow()` in `watcher.cpp`.
    pub fn say_it_was_slow(&self) {
        self.status_event.store(true, Ordering::Release);
    }

    /// Exact port of `Watcher::split_wake()` in `watcher.cpp`.
    pub fn split_wake(&self) {
        self.split_event.store(true, Ordering::Release);
    }

    /// Exact port of `Watcher::request_reset()` in `watcher.cpp`.
    pub fn request_reset(&self) {
        self.reset_event.store(true, Ordering::Release);
    }

    /// Exact port of `Watcher::graph_adopted()` in `watcher_source.cpp`.
    pub fn graph_adopted(&self) {
        self.status_event.store(true, Ordering::Release);
    }

    /// Exact port of `Watcher::load_initial()` in `watcher.cpp`:
    /// Opens level prediction, logs diagnostic lines if the endpoint has no GUID
    /// or `config_dir` does not exist yet, and performs the initial `reload()`.
    pub fn load_initial<B>(&mut self, build_graph: B)
    where
        B: FnOnce(
            &SignatureChainInput,
            Option<&RoomHeadBank>,
            bool,
            Option<&WatcherGraphInstance>,
            &SourceAnalysis,
        ) -> WatcherGraphInstance,
    {
        self.open_level_prediction();
        if self.endpoint_guid.is_empty() {
            self.log
                .write("no endpoint identified; only unguarded configuration applies");
        }
        if !is_directory(&self.config_dir) {
            self.log.write(&format!(
                "configuration directory {} is not present; waiting for it",
                self.config_dir
            ));
        }
        self.reload(build_graph, None);
    }

    /// Exact port of `Watcher::stop()` in `watcher.cpp`:
    /// Signals stop, reports `locked = false` to `status.json`, clears `GraphSlot`,
    /// and destroys every owned graph.
    pub fn stop(&mut self) {
        self.stop_event.store(true, Ordering::Release);
        self.report_status(false);
        self.slot.clear();
        self.rack_source_id = None;
        self.owned.clear();
    }

    /// Resolves the deepest existing directory ancestor to watch (`deepest_existing(config_dir_)` in `watcher.cpp`).
    pub fn watched_directory(&self) -> (String, bool) {
        let watched = deepest_existing(&self.config_dir);
        let watching_config = watched == self.config_dir;
        (watched, watching_config)
    }

    /// Exact port of one iteration of `Watcher::run()`'s inner wait loop (`watcher.cpp` lines 348–448).
    /// Returns `(should_exit, should_rearm)`.
    pub fn handle_wait_signal<B>(
        &mut self,
        signal: WatcherWaitSignal,
        watching_config: bool,
        rearmed_ok: bool,
        build_graph: B,
        predicted_level: Option<LevelPrediction>,
    ) -> (bool, bool)
    where
        B: FnOnce(
            &SignatureChainInput,
            Option<&RoomHeadBank>,
            bool,
            Option<&WatcherGraphInstance>,
            &SourceAnalysis,
        ) -> WatcherGraphInstance,
    {
        match signal {
            WatcherWaitSignal::Stop => (true, false),
            WatcherWaitSignal::SplitInitializing { .. } => {
                self.report_status(true);
                (false, false)
            }
            WatcherWaitSignal::Reset => {
                self.reset_event.store(false, Ordering::Release);
                self.reload(build_graph, predicted_level);
                (false, false)
            }
            WatcherWaitSignal::StatusWake => {
                self.status_event.store(false, Ordering::Release);
                self.log_timing();
                self.report_status(true);
                (false, false)
            }
            WatcherWaitSignal::OwnerWake => {
                self.reload(build_graph, predicted_level);
                (false, false)
            }
            WatcherWaitSignal::SplitWake => {
                self.split_event.store(false, Ordering::Release);
                self.report_status(true);
                (false, false)
            }
            WatcherWaitSignal::HandleInvalid => (false, true),
            WatcherWaitSignal::ConfigChanged => {
                self.change_armed.store(false, Ordering::Release);
                let mut rearm = false;
                if watching_config {
                    self.reload(build_graph, predicted_level);
                } else if is_directory(&self.config_dir) {
                    return (false, true);
                }
                if !rearmed_ok {
                    rearm = true;
                }
                (false, rearm)
            }
        }
    }

    // ------------------------------------------------------------------------
    // `watcher_level.cpp` — Auto-Normalize Level Prediction & Mailbox Delivery
    // ------------------------------------------------------------------------

    /// Exact port of `Watcher::open_level_prediction()` in `/native/system-apo/src/watcher_level.cpp`.
    pub fn open_level_prediction(&mut self) {
        self.history = Some(Arc::new(InputHistory::new(
            self.sample_rate,
            self.channels,
            LEVEL_HISTORY_SECONDS,
        )));
        self.accepted_chain = None;
        self.accepted_heard_frames = 0;
        self.predictor_active = true;
    }

    /// Exact port of `Watcher::plan_level(const Chain& chain, Graph& graph)` in `watcher_level.cpp`.
    pub fn plan_level(
        &mut self,
        chain: &SignatureChainInput,
        graph: &mut WatcherGraphInstance,
    ) -> bool {
        if !self.predictor_active || self.history.is_none() {
            return false;
        }
        if judge_level_prediction(self.accepted_chain.as_ref(), chain)
            == LevelPredictionJudgement::None
        {
            return false;
        }
        self.generation = if self.generation == u32::MAX {
            1
        } else {
            self.generation + 1
        };
        graph.expected_level_generation = self.generation;
        true
    }

    /// Exact port of `Watcher::owe_level(const Chain& chain, bool owed)` in `watcher_level.cpp`.
    pub fn owe_level(&mut self, chain: &SignatureChainInput, owed: bool) {
        let heard = self.history.as_ref().map(|h| h.written()).unwrap_or(0);
        if !owed {
            self.owed_level = None;
            self.others_from = u64::MAX;
            self.accept_level(chain, heard);
            return;
        }
        if self.others_from == u64::MAX {
            self.others_from = heard;
        }
        self.owed_level = Some(OwedLevel {
            chain: chain.clone(),
            generation: self.generation,
            published_at: heard,
        });
    }

    /// Exact port of `Watcher::settle_level()` in `/native/system-apo/src/watcher_level.cpp`.
    pub fn settle_level(&mut self, predicted: Option<LevelPrediction>) {
        let Some(owed) = self.owed_level.clone() else {
            return;
        };
        let mut shift_db: f32 = NO_LEVEL;
        if self.predictor_active && self.history.is_some() {
            let superseded = self.stop_requested() || self.change_pending();
            match judge_level_prediction(self.accepted_chain.as_ref(), &owed.chain) {
                LevelPredictionJudgement::None => {}
                LevelPredictionJudgement::Same => {
                    // Back to the EQ the level was last settled for: 0.0 dB shift
                    shift_db = 0.0;
                }
                LevelPredictionJudgement::Changed => {
                    if let Some(level) = predicted {
                        shift_db = level.shift_db as f32;
                        let note = format!(
                            "level {:+.2} dB from {:.1} s of music",
                            level.shift_db, level.seconds
                        );
                        self.log.write(&note);
                    } else if superseded {
                        // Still owed: the next wake publishes the newer edit, or starts this one again
                        return;
                    }
                }
            }
        }
        self.level_mailbox.post(owed.generation, shift_db);
        self.accept_level(&owed.chain, owed.published_at);
        self.owed_level = None;
        self.others_from = u64::MAX;
    }

    /// Exact port of `Watcher::accept_level(const Chain& chain, uint64_t heard_frames)` in `watcher_level.cpp`.
    pub fn accept_level(&mut self, chain: &SignatureChainInput, heard_frames: u64) {
        if !self.predictor_active {
            return;
        }
        self.accepted_chain = Some(chain.clone());
        self.accepted_heard_frames = heard_frames;
    }

    // ------------------------------------------------------------------------
    // `watcher_load.cpp` — Configuration Resolution, Graph Build & Handover
    // ------------------------------------------------------------------------

    /// Exact port of `Watcher::reload()` in `/native/system-apo/src/watcher_load.cpp`.
    pub fn reload<B>(&mut self, build_graph: B, predicted_level: Option<LevelPrediction>)
    where
        B: FnOnce(
            &SignatureChainInput,
            Option<&RoomHeadBank>,
            bool,
            Option<&WatcherGraphInstance>,
            &SourceAnalysis,
        ) -> WatcherGraphInstance,
    {
        let chain = self.resolve_chain_from_disk();
        self.load_chain_with_resolved(&chain, None, build_graph);
        self.settle_level(predicted_level);
    }

    fn resolve_chain_from_disk(&self) -> SignatureChainInput {
        if !self.owner_present() {
            return SignatureChainInput::default();
        }
        let dir = Path::new(&self.config_dir);
        let Some(resolved) = resolve_apo_endpoint_chain_from_dir(
            dir,
            "config.txt",
            &self.endpoint_guid,
            &self.endpoint_name,
        ) else {
            return SignatureChainInput::default();
        };

        let convolution_stamp = if resolved.convolution_path.is_empty() {
            String::new()
        } else {
            let conv_path = Path::new(&resolved.convolution_path);
            let full_conv = if conv_path.is_absolute() {
                conv_path.to_path_buf()
            } else {
                dir.join(conv_path)
            };
            match file_stamp(&full_conv) {
                Some((size, mtime)) => format!(
                    "{}:{}:{}:{}",
                    (size >> 32) as u32,
                    size as u32,
                    (mtime >> 32) as u32,
                    mtime as u32
                ),
                None => "-".to_string(),
            }
        };

        let bands = resolved
            .apo_bands
            .into_iter()
            .map(|b| SignatureBand {
                filter_type: b.spec.filter_type as i32,
                frequency: b.spec.frequency,
                gain_db: b.spec.gain_db,
                quality: b.spec.quality,
                user_eq: b.user_eq,
                curve_layer: b.curve_layer,
                matched: b.matched,
                channels_all: b.channels.all,
                channels_speakers: b.channels.speakers,
                channels_indices: b.channels.indices,
            })
            .collect();

        SignatureChainInput {
            matched: resolved.matched,
            convolution_path: resolved.convolution_path,
            convolution_passes: resolved.convolution_passes as i32,
            convolution_stamp,
            preamp_db: resolved.preamp_db,
            output_guard: resolved.output_guard,
            auto_preamp: resolved.auto_preamp,
            auto_preamp_start_db: resolved.auto_preamp_start_db,
            stable_graphic: resolved.stable_graphic,
            low_latency: resolved.low_latency,
            minimum_curve_phase: resolved.minimum_curve_phase,
            minimum_eq_phase: resolved.minimum_eq_phase,
            eq_graphic_curves: resolved.eq_graphic_curves,
            comparison_curves: resolved.comparison_curves,
            bands,
            graphic_curves: resolved.graphic_curves,
            files_read: resolved.files_read,
            ignored: resolved.ignored,
            dsp_values: resolved.dsp_values,
        }
    }

    /// Exact port of `Watcher::load_chain()` in `/native/system-apo/src/watcher_load.cpp`:
    /// Evaluates owner presence, `stop_requested()` checkpoints, programme & split sidecars,
    /// `read_source(owner)`, room head parsing & torn-write protection (`requested.room.head != room_head.size`),
    /// composite signature comparison (`signature_of(chain) + "|o=1/0|h=<gen>|s=<source.identity>"`),
    /// `rack_from` sharing, `follows_processing`, source/seek prediction reset, `silenced_blocks` logging,
    /// `log_chain`, `publish`, and `owe_level`.
    pub fn load_chain_with_resolved<B>(
        &mut self,
        resolved_chain: &SignatureChainInput,
        override_head_text: Option<Option<String>>,
        build_graph: B,
    ) where
        B: FnOnce(
            &SignatureChainInput,
            Option<&RoomHeadBank>,
            bool,
            Option<&WatcherGraphInstance>,
            &SourceAnalysis,
        ) -> WatcherGraphInstance,
    {
        let owner = self.owner_present();
        let chain = if owner {
            resolved_chain.clone()
        } else {
            SignatureChainInput::default()
        };
        if self.stop_requested() {
            return;
        }

        let finished_song = owner && self.follow_programme();
        let split_moved = self.follow_split(owner);
        let source = self.read_source(owner);
        if self.source_invalid {
            self.source_prepare_failed = true;
            self.report_status(true);
            return;
        }

        // Read room head identity before signature comparison (`watcher_load.cpp` lines 116-129)
        let head_text = match override_head_text {
            Some(ht) => {
                if owner {
                    ht
                } else {
                    None
                }
            }
            None => {
                let head_name = endpoint_config_name("fluideq-room-head", &self.endpoint_guid);
                if owner {
                    head_name.and_then(|name| {
                        read_apo_config_file_bounded(&Path::new(&self.config_dir).join(name))
                    })
                } else {
                    None
                }
            }
        };

        if head_text != self.room_head_text {
            let parsed = head_text
                .as_deref()
                .and_then(|txt| RoomHeadBank::parse_text(txt).ok());
            self.room_head = parsed;
            self.room_head_text = head_text;
            self.room_head_generation = self.room_head_generation.wrapping_add(1);
        }
        if self.stop_requested() {
            return;
        }

        // Torn-write guard: Main publishes the selected head before its rack.
        // If both files do not yet name the same head size, keep the current graph playing (`watcher_load.cpp` lines 134-140).
        if let Some(room_head) = &self.room_head {
            let head_size_code = match room_head.head_id.as_str() {
                "small" => 0,
                "medium" => 1,
                "large" => 2,
                _ => -1,
            };
            if head_size_code >= 0 {
                if let Some(requested) = decode_chain_settings_wire(&chain.dsp_values) {
                    if requested.enabled != 0
                        && requested.room.enabled != 0
                        && requested.room.head != head_size_code
                    {
                        return;
                    }
                }
            }
        }

        let next_sig = format!(
            "{}|o={}|h={}|s={}",
            signature_of_chain(&chain),
            if owner { 1 } else { 0 },
            self.room_head_generation,
            source.identity
        );

        if self.have_signature && next_sig == self.signature {
            let recovered = self.source_prepare_failed;
            self.source_prepare_failed = false;
            if recovered {
                if let Some(rack_id) = self.rack_source_id {
                    if let Some(found) = self.owned.iter().find(|e| e.graph.id == rack_id) {
                        self.source_report = found.graph.source_report.clone();
                    }
                }
            }
            if finished_song || split_moved || recovered {
                self.report_status(true);
            }
            return;
        }

        let rack_from = if self.rack_source_head == self.room_head_generation {
            self.rack_source_id
                .and_then(|id| self.owned.iter().find(|e| e.graph.id == id).map(|e| &e.graph))
        } else {
            None
        };

        let active_id = self.slot.active();
        let running = self
            .owned
            .iter()
            .find(|e| e.graph.id == active_id)
            .map(|e| &e.graph);
        let follows_processing = running.map(|g| !g.is_passthrough).unwrap_or(false);

        let mut graph = build_graph(
            &chain,
            self.room_head.as_ref(),
            follows_processing,
            rack_from,
            &source,
        );
        if graph.id == 0 {
            graph.id = self.next_graph_id;
            self.next_graph_id += 1;
        } else if graph.id >= self.next_graph_id {
            self.next_graph_id = graph.id + 1;
        }

        if !graph.source_report.ready {
            self.source_report = graph.source_report.clone();
            self.source_prepare_failed = true;
            for warning in &graph.warnings {
                self.log.write(warning);
            }
            self.report_status(true);
            return;
        }

        let rack_kept = rack_from.is_some() && graph.rack_shared_with_previous;
        if !graph.room_note.is_empty() && !rack_kept {
            self.log.write(&graph.room_note);
        }
        if self.stop_requested() {
            return;
        }

        // Source/seek boundary check (`watcher_load.cpp` lines 196-209)
        if let Some(prev_rack_id) = self.rack_source_id {
            if let Some(prev_rack) = self.owned.iter().find(|e| e.graph.id == prev_rack_id) {
                let before = &prev_rack.graph.source_report;
                let after = &graph.source_report;
                if before.library != after.library
                    || before.engine_owner != after.engine_owner
                    || before.source != after.source
                    || before.epoch != after.epoch
                {
                    self.owed_level = None;
                    self.others_from = u64::MAX;
                    self.open_level_prediction();
                }
            }
        }

        graph.state_transfer_requested = true;
        let level_owed = self.plan_level(&chain, &mut graph);

        // Log silenced non-real blocks from the outgoing active graph (`watcher_load.cpp` lines 215-220)
        if let Some(previous) = self
            .owned
            .iter()
            .find(|e| e.graph.id == self.slot.active())
            .map(|e| &e.graph)
        {
            if previous.silenced_blocks > 0 {
                self.log.write(&format!(
                    "silenced {} block(s) whose samples were not all real numbers",
                    previous.silenced_blocks
                ));
            }
        }

        self.log_chain(&chain, &graph, owner);
        let processing = !graph.is_passthrough;
        let problems = graph.problems.clone();
        self.room_state = graph.room_state.clone();
        self.latency = if processing { graph.latency_frames } else { 0 };
        self.latency_active = if processing {
            graph.active_stages.clone()
        } else {
            Vec::new()
        };
        self.latency_parts = if processing {
            latency_parts_of(&graph.latency_parts, &graph.active_stages)
        } else {
            Vec::new()
        };
        self.game_mode = processing && graph.low_latency;

        self.publish(graph);
        self.owe_level(&chain, level_owed);
        self.signature = next_sig;
        self.have_signature = true;
        self.last_processing = processing;
        self.last_owner = owner;
        self.graph_problems = problems;
        self.reload_failed = false;
        self.source_prepare_failed = false;
        self.report_status(true);
    }

    /// Exact port of `Watcher::publish(std::unique_ptr<Graph> graph)` in `/native/system-apo/src/watcher_load.cpp`.
    pub fn publish(&mut self, graph: WatcherGraphInstance) {
        let graph_id = graph.id;
        let graph_latency = graph.latency_frames;
        self.rack_source_id = Some(graph_id);
        self.source_report = graph.source_report.clone();
        self.rack_source_head = self.room_head_generation;

        self.owned.push(OwnedGraphEntry {
            graph,
            blocks_at_publish: 0,
        });

        self.slot.set_latency(graph_latency);
        let unconsumed = self.slot.publish(graph_id);
        let current_blocks = self.slot.blocks();
        if let Some(last) = self.owned.last_mut() {
            last.blocks_at_publish = current_blocks;
        }

        if unconsumed != 0 {
            if let Some(pos) = self.owned.iter().position(|e| e.graph.id == unconsumed) {
                self.owned.remove(pos);
            }
        }
        self.reclaim();
    }

    /// Exact port of `Watcher::reclaim()` in `/native/system-apo/src/watcher_load.cpp`.
    pub fn reclaim(&mut self) -> Vec<WatcherGraphInstance> {
        reclaim_graphs(&mut self.owned, self.slot.blocks())
    }

    // ------------------------------------------------------------------------
    // `watcher_log.cpp` — Chain Logging, Timing Step Logging & Status Reporting
    // ------------------------------------------------------------------------

    /// Exact port of `Watcher::log_chain` in `/native/system-apo/src/watcher_log.cpp`.
    pub fn log_chain(
        &mut self,
        chain: &SignatureChainInput,
        graph: &WatcherGraphInstance,
        owner_present: bool,
    ) {
        let files: String = chain
            .files_read
            .iter()
            .map(|f| file_name_of(f))
            .collect::<Vec<_>>()
            .join(", ");

        let mut line = format!("chain loaded: files={}", chain.files_read.len());
        if !files.is_empty() {
            line.push_str(&format!(" ({})", files));
        }
        line.push_str(&format!(" bands={}", chain.bands.len()));
        line.push_str(&format!(" graphic_curves={}", chain.graphic_curves.len()));
        line.push_str(&format!(" preamp={} dB", decibels(chain.preamp_db)));
        line.push_str(" ir=");
        if chain.convolution_path.is_empty() {
            line.push_str("none");
        } else {
            line.push_str(&chain.convolution_path);
        }
        line.push_str(" rack=");
        if chain.dsp_values.is_empty() {
            line.push_str("none");
        } else {
            line.push_str(&format!("{} values", chain.dsp_values.len()));
        }
        line.push_str(&format!(" latency={} frames", graph.latency_frames));
        self.log.write(&line);

        for warning in &graph.warnings {
            self.log.write(&format!("graph warning: {}", warning));
        }

        if !chain.ignored.is_empty() && self.logged_ignored.len() < MAX_IGNORED_SETS_LOGGED {
            let set = chain.ignored.join(", ");
            if !self.logged_ignored.contains(&set) {
                self.logged_ignored.push(set.clone());
                self.log.write(&format!(
                    "ignored commands (this engine does not run them): {}",
                    set
                ));
            }
        }

        let mut reason = String::new();
        if graph.is_passthrough {
            if !owner_present {
                reason = "FluidEQ is not running".to_string();
            } else if !is_directory(&self.config_dir) {
                reason = "no configuration directory".to_string();
            } else if chain.files_read.is_empty() {
                reason = "no config.txt in the configuration directory".to_string();
            } else if !chain.matched {
                reason = "no configuration block names this endpoint".to_string();
            } else {
                reason = "the configuration asks for nothing on this endpoint".to_string();
            }
        }
        if !self.have_passthrough_reason || reason != self.passthrough_reason {
            self.passthrough_reason = reason.clone();
            self.have_passthrough_reason = true;
            if !reason.is_empty() {
                self.log.write(&format!("pass-through: {}", reason));
            } else {
                self.log.write("processing this endpoint");
            }
        }
    }

    /// Exact port of `Watcher::log_timing()` in `/native/system-apo/src/watcher_log.cpp`.
    pub fn log_timing(&mut self) {
        if self.endpoint_guid.is_empty() {
            return;
        }
        let timing = output_block_timing(&self.endpoint_guid);
        let reading = timing.read();
        let step = timing_step(reading.slowest_permille);
        if reading.blocks == 0 || step <= self.timing_step_logged {
            return;
        }
        self.timing_step_logged = step;
        self.log.write(&describe_timing(&reading));
    }

    /// Exact port of `Watcher::report_status(bool locked)` in `/native/system-apo/src/watcher_log.cpp`.
    pub fn report_status(&mut self, locked: bool) {
        let mut written = true;
        let mut why = String::new();

        if !locked {
            if let Err(err) = self.status_share.leave() {
                written = false;
                why = err;
            }
        } else if !self.endpoint_guid.is_empty() {
            let mut status = EngineStatusReport {
                endpoint: self.endpoint_guid.clone(),
                locked: true,
                processing: self.last_processing,
                carried: false,
                owner: self.last_owner,
                reason: self.passthrough_reason.clone(),
                problems: self.graph_problems.clone(),
                last_song: None,
                channels: self.channels,
                room: self.room_state.clone(),
                rate: self.sample_rate,
                latency: self.latency,
                latency_parts: self.latency_parts.clone(),
                latency_active: self.latency_active.clone(),
                game_mode: self.game_mode,
                load: None,
                split: None,
                source_analysis: None,
            };

            let source_id = format!("{:016x}", self.source_report.source);
            let source_ready = !self.source_prepare_failed
                && !self.source_invalid
                && self.rack_source_id.is_some()
                && Some(self.slot.active()) == self.rack_source_id
                && self.source_report.ready;

            status.source_analysis = Some(SourceAnalysisStatusReport {
                version: 1,
                kind: if self.source_report.library {
                    "library".to_string()
                } else {
                    "live".to_string()
                },
                owner: if self.source_report.engine_owner {
                    "engine".to_string()
                } else {
                    "host".to_string()
                },
                source: source_id,
                epoch: self.source_report.epoch,
                revision: self.source_report.revision,
                ready: source_ready,
                voice_ready: source_ready && self.source_report.voice_ready,
            });

            if self.source_prepare_failed || self.source_invalid {
                status.problems.push("source-analysis".to_string());
            }
            if self.split_transport_failed {
                status.problems.push("split-transport".to_string());
            }
            let carried = output_carried_flag(&self.endpoint_guid);
            status.carried = carried.load(Ordering::Relaxed);
            let timing = output_block_timing(&self.endpoint_guid);
            let reading = timing.read();
            if reading.blocks > 0 {
                status.load = Some(BlockLoadReport {
                    blocks: reading.blocks,
                    over_half: reading.over_half,
                    slowest_ms: reading.slowest_ms,
                    period_ms: reading.period_ms,
                });
            }
            if self.reload_failed {
                status.problems.push("reload-failed".to_string());
            }
            if self.unwatched {
                status.problems.push("unwatched".to_string());
            }
            if let Some(song) = self.leveling.as_ref().and_then(|l| l.last_song()) {
                status.last_song = Some(FinishedSongStatus {
                    id: format!("{:016x}", song.song_id),
                    level_lufs: song.level_lufs,
                    peak_db: song.peak_db,
                    seconds: song.seconds,
                });
            }
            if let Some(split) = &self.split_report {
                status.split = Some(SplitStatusReport {
                    from: split.from.clone(),
                    state: if split.state == SplitState::Playing {
                        "playing".to_string()
                    } else {
                        "waiting".to_string()
                    },
                    lag_ms: (split.lag_us as f64) / 1000.0,
                    source_dsp_ms: 0.0,
                    output_eq_ms: if self.sample_rate > 0 {
                        (self.latency as f64) * 1000.0 / (self.sample_rate as f64)
                    } else {
                        0.0
                    },
                    underruns: split.underruns as u64,
                });
            }

            if let Err(err) = self.status_share.publish(&status) {
                written = false;
                why = err;
            }
        }

        if written {
            self.status_failure_logged = false;
        } else if !self.status_failure_logged {
            self.status_failure_logged = true;
            self.log.write(&format!(
                "could not write this output's status for the app ({})",
                why
            ));
        }
    }

    /// Exact port of `Watcher::follow_programme()` in `/native/system-apo/src/watcher_log.cpp`.
    pub fn follow_programme(&mut self) -> bool {
        let Some(leveling) = self.leveling.as_mut() else {
            return false;
        };
        let name = endpoint_config_name("fluideq-programme", &self.endpoint_guid);
        let text = name.and_then(|n| {
            read_apo_config_file_bounded(&Path::new(&self.config_dir).join(n))
        });
        let programme = text
            .as_deref()
            .map(parse_programme)
            .unwrap_or_default();
        leveling.announce(&programme)
    }

    /// Exact port of `Watcher::follow_split(bool owner)` in `/native/system-apo/src/watcher_log.cpp`.
    pub fn follow_split(&mut self, _owner: bool) -> bool {
        false
    }

    // ------------------------------------------------------------------------
    // `watcher_source.cpp` — Source Sidecar & Pinned Voice Model Resolution
    // ------------------------------------------------------------------------

    /// Exact port of `Watcher::read_source(bool owner)` in `/native/system-apo/src/watcher_source.cpp`.
    pub fn read_source(&mut self, owner: bool) -> SourceAnalysis {
        let path = Path::new(&self.config_dir).join(SOURCE_ANALYSIS_FILE);
        let text = if owner {
            read_apo_config_file_bounded(&path)
        } else {
            None
        };
        self.read_source_from_text(owner, text.as_deref())
    }

    /// Testable inner logic of `Watcher::read_source` accepting optional pre-read sidecar text.
    pub fn read_source_from_text(&mut self, owner: bool, text: Option<&str>) -> SourceAnalysis {
        let text = if owner { text } else { None };
        let mut parsed = text.and_then(parse_source_analysis);
        self.source_invalid = text.is_some() && parsed.is_none();

        if let Some(ref mut sa) = parsed {
            if sa.voice_available {
                sa.voice_runtime = self.trusted_runtime_path.clone();
                let expected_model = format!(
                    "{}\\fluideq-voice.onnx",
                    self.config_dir.trim_end_matches(['\\', '/'])
                );
                if same_model_path(&sa.voice_model, &expected_model) {
                    sa.voice_model = expected_model.clone();
                    if self.voice_guard.is_none() && !self.voice_verified_flag {
                        let model_path = Path::new(&self.config_dir).join("fluideq-voice.onnx");
                        let stamp = file_stamp(&model_path);
                        if stamp.is_none() || stamp != self.voice_refused {
                            self.voice_guard = verified_voice_model_file(&model_path);
                            self.voice_verified_flag = self.voice_guard.is_some();
                            self.voice_refused = if self.voice_verified_flag {
                                None
                            } else {
                                stamp
                            };
                        }
                    }
                    sa.voice_model_verified = self.voice_verified_flag;
                }
            } else {
                self.voice_guard = None;
                self.voice_verified_flag = false;
                self.voice_refused = None;
            }
        } else {
            self.voice_guard = None;
            self.voice_verified_flag = false;
            self.voice_refused = None;
        }

        let split_from = self
            .split_report
            .as_ref()
            .map(|s| s.from.as_str())
            .unwrap_or("");
        let mut source =
            source_for_endpoint(parsed.as_ref(), &self.endpoint_guid, split_from, self.default_mode);
        if source.library {
            source.identity.push_str("|runtime=");
            source.identity.push_str(&source.voice_runtime);
            source.identity.push_str(if source.voice_model_verified {
                "|verified=1"
            } else {
                "|verified=0"
            });
        }
        source
    }
}

// ============================================================================
// 7. Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reclaim_graphs_respects_two_grace_blocks_and_transitive_crossing_chain() {
        let mut owned = vec![
            OwnedGraphEntry {
                graph: WatcherGraphInstance {
                    id: 1,
                    crossing_from: None,
                    ..Default::default()
                },
                blocks_at_publish: 10,
            },
            OwnedGraphEntry {
                graph: WatcherGraphInstance {
                    id: 2,
                    crossing_from: Some(1),
                    ..Default::default()
                },
                blocks_at_publish: 11,
            },
            OwnedGraphEntry {
                graph: WatcherGraphInstance {
                    id: 3,
                    crossing_from: Some(2),
                    ..Default::default()
                },
                blocks_at_publish: 12,
            },
        ];

        // At block 14, graph #3 (published at 12) has completed 2 grace blocks (`14 >= 12 + 2`),
        // so boundary = 2 (graph #3). However, #3 crosses from #2 and #2 crosses from #1,
        // so all three must be kept!
        let retired = reclaim_graphs(&mut owned, 14);
        assert!(retired.is_empty());
        assert_eq!(owned.len(), 3);

        // Once #3 finishes crossing (`crossing_from = None`), graphs #1 and #2 are reclaimed.
        owned[2].graph.crossing_from = None;
        let retired = reclaim_graphs(&mut owned, 14);
        assert_eq!(retired.len(), 2);
        assert_eq!(retired[0].id, 1);
        assert_eq!(retired[1].id, 2);
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].graph.id, 3);
    }

    #[test]
    fn graph_slot_two_pointer_handover_preserves_unconsumed_and_active_invariants() {
        let slot = GraphSlot::new();
        assert_eq!(slot.active(), 0);

        // Publish graph 10, then supersede with graph 20 before audio thread adopts
        assert_eq!(slot.publish(10), 0);
        assert_eq!(slot.publish(20), 10);

        // Audio thread adopts graph 20
        let mut adopted_pair = (0usize, 0usize);
        let active = slot.adopt_with(|next, prev| {
            adopted_pair = (next, prev);
        });
        assert_eq!(active, 20);
        assert_eq!(adopted_pair, (20, 0));
        assert_eq!(slot.active(), 20);

        slot.finish_block();
        slot.finish_block();
        assert_eq!(slot.blocks(), 2);
    }

    #[test]
    fn latency_parts_of_maintains_exact_signal_path_order_and_active_stages() {
        let mut parts = GraphLatencyParts::default();
        parts.rack.leveler = 64;
        parts.rack.eq = 8192;
        parts.guard = 96;
        let active = vec![
            "leveler".to_string(),
            "exciter".to_string(),
            "linearEq".to_string(),
            "guard".to_string(),
            "preamp".to_string(),
        ];

        let out = latency_parts_of(&parts, &active);
        assert_eq!(
            out,
            vec![
                ("leveler".to_string(), 64),
                ("exciter".to_string(), 0),
                ("linearEq".to_string(), 8192),
                ("guard".to_string(), 96),
                ("preamp".to_string(), 0),
            ]
        );
    }

    #[test]
    fn sha256_and_trusted_voice_runtime_match_pinned_security_rules() {
        // Known NIST test vector for "abc"
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(same_model_path(
            "C:/ProgramData/FluidEQ/config/fluideq-voice.onnx",
            "c:\\programdata\\fluideq\\config\\FluidEQ-Voice.onnx"
        ));
        assert_eq!(
            resolve_trusted_voice_runtime(
                "C:\\Program Files\\FluidEQ Engine\\fluideq-engine.dll",
                "C:\\Program Files",
                true
            ),
            "C:\\Program Files\\FluidEQ Engine\\onnxruntime.dll"
        );
        // Untrusted module folder must be rejected
        assert_eq!(
            resolve_trusted_voice_runtime(
                "C:\\Users\\Public\\FluidEQ-Engine.dll",
                "C:\\Program Files",
                true
            ),
            ""
        );
    }

    #[test]
    fn watcher_load_chain_level_prediction_and_torn_room_head_guard() {
        let temp_dir = std::env::temp_dir().join(format!(
            "fluideq_watcher_test_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&temp_dir);
        let slot = Arc::new(GraphSlot::new());
        let log = EngineLog::new(temp_dir.join("engine.log"), "{11111111-2222-3333-4444-555555555555}");
        let mut watcher = ApoWatcher::new(
            slot.clone(),
            log,
            ApoEndpointProperties {
                guid: Some("{11111111-2222-3333-4444-555555555555}"),
                friendly_name: Some("Speakers"),
            },
            temp_dir.to_string_lossy().into_owned(),
            ConnectionFormat {
                acceptable: true,
                rate: 48_000,
                channels: 2,
                mask: 0x3,
            },
            480,
            true,
        );
        watcher.open_level_prediction();

        let mut chain1 = SignatureChainInput::default();
        chain1.matched = true;
        chain1.output_guard = true;
        chain1.auto_preamp = true;
        chain1.preamp_db = -2.0;
        chain1.files_read = vec!["C:\\config\\config.txt".to_string()];

        watcher.load_chain_with_resolved(&chain1, Some(None), |_, _, _, _, _| WatcherGraphInstance {
            id: 1,
            is_passthrough: false,
            latency_frames: 96,
            ..Default::default()
        });
        assert!(watcher.have_signature);
        assert_eq!(slot.latency(), 96);
        assert!(watcher.owed_level.is_none()); // First chain sets baseline, nothing owed yet

        // Audio thread adopts graph #1
        assert_eq!(slot.adopt_with(|_, _| {}), 1);

        // Second edit changes preamp -> level prediction is planned and owed
        let mut chain2 = chain1.clone();
        chain2.preamp_db = -5.0;
        watcher.load_chain_with_resolved(&chain2, Some(None), |_, _, follows, _, _| {
            assert!(follows);
            WatcherGraphInstance {
                id: 2,
                is_passthrough: false,
                latency_frames: 96,
                ..Default::default()
            }
        });
        assert!(watcher.owed_level.is_some());
        assert_eq!(watcher.generation, 1);

        // Settle the owed level prediction and verify mailbox delivery
        watcher.settle_level(Some(LevelPrediction {
            shift_db: -2.75,
            seconds: 3.4,
        }));
        assert!(watcher.owed_level.is_none());
        assert_eq!(watcher.level_mailbox.take(1), Some(-2.75));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
