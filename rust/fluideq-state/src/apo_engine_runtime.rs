/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! FluidEQ System-APO Engine Runtime Contracts & Parsers (`Slice-M1M8-1`).
//!
//! 1:1 faithful port of:
//! - `/native/system-apo/src/endpoint_config.cpp` (`endpoint_config_name`)
//! - `/native/system-apo/src/programme.cpp` (`Programme`, `parse_programme`)
//! - `/native/system-apo/src/split_file.cpp` (`SplitRole`, `split_role_of`)
//! - `/native/system-apo/src/chain_signature.cpp` (`signature_of`)
//! - `/native/system-apo/src/status_json.cpp` (`EngineStatus`, `status_json`)
//! - `/native/system-apo/src/config_file.cpp` (`read_config_file` 4 MB cap + case-folded disk `Include:` tree loader)

pub use crate::apo::{
    resolve_apo_endpoint_chain, ApoEndpointChain, ChannelSelection, GraphicEqPoint,
    LOW_FREQUENCY_SPEAKER,
};
pub use crate::apo_watcher::GraphSourceReport as SourceAnalysisReport;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::Path;

/// Maximum size of a single Equalizer APO / FluidEQ config file (`4 MB`),
/// matching `kMaxConfigBytes` in `/native/system-apo/src/config_file.cpp`.
pub const MAX_APO_CONFIG_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Maximum lines parsed in `fluideq-split.txt` (`kMaxLines = 64` in `/native/system-apo/src/split_file.cpp`).
pub const MAX_SPLIT_FILE_LINES: usize = 64;

// ============================================================================
// 1. Endpoint Config Filename Sanitizer (`/native/system-apo/src/endpoint_config.cpp`)
// ============================================================================

/// Exact port of `endpoint_config_name` in `/native/system-apo/src/endpoint_config.cpp`:
/// Validates `endpoint_guid` (either `{8-4-4-4-12}` 38 chars or `8-4-4-4-12` 36 chars),
/// lowercases hex digits, and returns `<stem>-<canonical_guid>.txt`.
/// Rejects any malformed or path-traversal device string so an opaque ID can never name another file.
pub fn endpoint_config_name(stem: &str, endpoint_guid: &str) -> Option<String> {
    let mut guid = endpoint_guid;
    if guid.len() == 38 && guid.starts_with('{') && guid.ends_with('}') {
        guid = &guid[1..37];
    }
    if guid.len() != 36 {
        return None;
    }

    let mut canonical = String::with_capacity(36);
    for (at, b) in guid.bytes().enumerate() {
        if matches!(at, 8 | 13 | 18 | 23) {
            if b != b'-' {
                return None;
            }
            canonical.push('-');
        } else {
            let lower = b.to_ascii_lowercase();
            if !lower.is_ascii_hexdigit() {
                return None;
            }
            canonical.push(lower as char);
        }
    }

    Some(format!("{}-{}.txt", stem, canonical))
}

// ============================================================================
// 2. Song Programme Parser (`/native/system-apo/src/programme.cpp`)
// ============================================================================

/// Song programme descriptor read from `fluideq-programme.txt` (`Programme` in `programme.h`).
/// Kept outside the chain signature so a song change never rebuilds the DSP rack.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SongProgramme {
    pub song_id: u64,
    pub level_lufs: f64,
    pub peak_db: f64,
}

/// Alias matching `fluideq_engine::Programme` in `/native/system-apo/src/programme.h`.
pub type Programme = SongProgramme;

impl Default for SongProgramme {
    fn default() -> Self {
        Self {
            song_id: 0,
            level_lufs: -120.0,
            peak_db: -120.0,
        }
    }
}

fn trim_ws_cr(s: &str) -> &str {
    s.trim_matches(|c| c == ' ' || c == '\t' || c == '\r')
}

fn measured_value(token: &str, low: f64, high: f64, fallback: f64) -> f64 {
    let Ok(val) = token.parse::<f64>() else {
        return fallback;
    };
    if !val.is_finite() || val < low || val > high {
        return fallback;
    }
    val
}

/// Exact port of `parse_programme` in `/native/system-apo/src/programme.cpp`:
/// - `song` must be an exact 16-character non-zero hexadecimal hash (`0` is rejected).
/// - `level` is clamped/validated in `[-70.0, 0.0]` LUFS (fallback `-120.0`).
/// - `peak` is validated in `[-70.0, 12.0]` dB (fallback `-120.0`), and a peak without a valid
///   level (`level <= -100.0`) is discarded to `-120.0`.
pub fn parse_programme(text: &str) -> SongProgramme {
    let mut programme = SongProgramme::default();
    let mut named = false;
    let mut level = -120.0;
    let mut peak = -120.0;

    for raw_line in text.split('\n') {
        let line = trim_ws_cr(raw_line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(eq_pos) = line.find('=') else {
            continue;
        };
        let key = trim_ws_cr(&line[..eq_pos]);
        let value = trim_ws_cr(&line[eq_pos + 1..]);
        if key == "song" {
            if value.len() == 16 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
                if let Ok(id) = u64::from_str_radix(value, 16) {
                    if id != 0 {
                        named = true;
                        programme.song_id = id;
                    } else {
                        named = false;
                    }
                }
            } else {
                named = false;
            }
        } else if key == "level" {
            level = measured_value(value, -70.0, 0.0, -120.0);
        } else if key == "peak" {
            peak = measured_value(value, -70.0, 12.0, -120.0);
        }
    }

    if !named {
        return SongProgramme::default();
    }
    programme.level_lufs = level;
    programme.peak_db = if level > -100.0 { peak } else { -120.0 };
    programme
}

// ============================================================================
// 3. Secondary Output Split Routing Parser (`/native/system-apo/src/split_file.cpp`)
// ============================================================================

/// Role of an endpoint in `fluideq-split.txt` (`SplitRole` in `split_file.h`).
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct SplitRole {
    /// True if this endpoint is the main output (either via `# main <guid>` or as the single source).
    pub primary: bool,
    /// True if this endpoint is actively feeding at least one secondary receiver.
    pub source: bool,
    /// Non-empty source GUID when this endpoint is a secondary receiver.
    pub from: String,
    /// Receiver playback gain in `[0.0, 1.0]`.
    pub volume: f32,
}

impl SplitRole {
    pub fn is_target(&self) -> bool {
        !self.from.is_empty()
    }
}

fn is_strict_braced_guid(word: &str) -> bool {
    let bytes = word.as_bytes();
    if bytes.len() != 38 || bytes[0] != b'{' || bytes[37] != b'}' {
        return false;
    }
    for (at, &byte) in bytes.iter().enumerate().take(37).skip(1) {
        let dash = matches!(at, 9 | 14 | 19 | 24);
        if dash {
            if byte != b'-' {
                return false;
            }
        } else if !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

fn same_guid_ci(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn next_ws_word<'a>(line: &mut &'a str) -> &'a str {
    let trimmed = line.trim_start_matches([' ', '\t']);
    if trimmed.is_empty() {
        *line = "";
        return "";
    }
    if let Some(stop) = trimmed.find([' ', '\t']) {
        let word = &trimmed[..stop];
        *line = &trimmed[stop..];
        word
    } else {
        *line = "";
        trimmed
    }
}

/// Exact port of `split_role_of` in `/native/system-apo/src/split_file.cpp`:
/// Parses up to 64 lines of `fluideq-split.txt`, enforcing single-source topology
/// (a conflicting line can never turn the main output into a receiver or chain a secondary through another secondary).
pub fn split_role_of(text: &str, endpoint: &str) -> SplitRole {
    let mut role = SplitRole::default();
    if endpoint.is_empty() {
        return role;
    }

    let mut lines = 0usize;
    let mut root = String::new();
    let mut remaining = text;

    while !remaining.is_empty() && lines < MAX_SPLIT_FILE_LINES {
        let (raw_line, rest) = match remaining.find('\n') {
            Some(stop) => (&remaining[..stop], &remaining[stop + 1..]),
            None => (remaining, ""),
        };
        remaining = rest;
        let mut line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        lines += 1;

        if let Some(after_main) = line.strip_prefix("# main ") {
            let mut cursor = after_main;
            let main = next_ws_word(&mut cursor);
            if is_strict_braced_guid(main)
                && next_ws_word(&mut cursor).is_empty()
                && (root.is_empty() || same_guid_ci(main, &root))
            {
                root = main.to_string();
                role.primary = same_guid_ci(main, endpoint);
            }
            continue;
        }

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let from = next_ws_word(&mut line);
        let to = next_ws_word(&mut line);
        let level = next_ws_word(&mut line);
        if !is_strict_braced_guid(from)
            || !is_strict_braced_guid(to)
            || !next_ws_word(&mut line).is_empty()
        {
            continue;
        }
        let Ok(volume) = level.parse::<f32>() else {
            continue;
        };
        if !volume.is_finite() || !(0.0..=1.0).contains(&volume) {
            continue;
        }
        if same_guid_ci(to, from) {
            continue;
        }

        // Enforce single source across the entire file
        if root.is_empty() {
            root = from.to_string();
        } else if !same_guid_ci(from, &root) {
            continue;
        }

        if same_guid_ci(from, endpoint) {
            role.source = true;
            role.primary = true;
        }
        if !role.is_target() && same_guid_ci(to, endpoint) {
            role.from = from.to_string();
            role.volume = volume;
        }
    }

    role
}

// ============================================================================
// 4. Deterministic Chain Signature (`/native/system-apo/src/chain_signature.cpp`)
// ============================================================================

/// Band descriptor for full APO graph signature calculation (`fluideq_engine::Band` in `config.h`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignatureBand {
    pub filter_type: i32,
    pub frequency: f64,
    pub gain_db: f64,
    pub quality: f64,
    pub user_eq: bool,
    pub curve_layer: bool,
    pub matched: bool,
    pub channels_all: bool,
    pub channels_speakers: u32,
    pub channels_indices: u32,
}

/// Complete resolved APO chain state input for `signature_of` (`/native/system-apo/src/chain_signature.cpp`).
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignatureChainInput {
    pub matched: bool,
    pub convolution_path: String,
    pub convolution_passes: i32,
    /// Pre-formatted file stamp `high_size:low_size:high_mtime:low_mtime` or `"-"`.
    pub convolution_stamp: String,
    pub preamp_db: f64,
    pub output_guard: bool,
    pub auto_preamp: bool,
    pub auto_preamp_start_db: f64,
    pub stable_graphic: bool,
    pub low_latency: bool,
    pub minimum_curve_phase: bool,
    pub minimum_eq_phase: bool,
    pub eq_graphic_curves: Vec<Vec<GraphicEqPoint>>,
    pub comparison_curves: Vec<Vec<GraphicEqPoint>>,
    pub bands: Vec<SignatureBand>,
    pub graphic_curves: Vec<Vec<GraphicEqPoint>>,
    pub files_read: Vec<String>,
    pub ignored: Vec<String>,
    pub dsp_values: Vec<f64>,
}

/// Alias matching `fluideq_engine::Chain` in `/native/system-apo/src/config.h`.
pub type ApoEngineChain = SignatureChainInput;

fn format_number_g17(value: f64) -> String {
    if !value.is_finite() {
        return "?".to_string();
    }
    if value == 0.0 {
        return "0".to_string();
    }
    // Format round-trippable float matching C++ `%.17g` compact representation for typical values
    let s = format!("{}", value);
    s
}

/// Exact port of `signature_of` in `/native/system-apo/src/chain_signature.cpp`:
/// Produces the deterministic pipe-delimited signature string used by the APO watcher
/// to decide whether a config notification requires rebuilding the audio graph.
/// Note that `fluideq-programme.txt` is intentionally excluded so song changes never rebuild the rack.
pub fn signature_of_chain(chain: &SignatureChainInput) -> String {
    let mut out = String::with_capacity(512);
    out.push_str(if chain.matched { "m1" } else { "m0" });
    out.push_str("|c=");
    out.push_str(&chain.convolution_path);
    out.push_str("|cp=");
    out.push_str(&chain.convolution_passes.to_string());
    out.push_str("|s=");
    if chain.convolution_stamp.is_empty() {
        out.push('-');
    } else {
        out.push_str(&chain.convolution_stamp);
    }
    out.push_str("|p=");
    out.push_str(&format_number_g17(chain.preamp_db));
    out.push_str(if chain.output_guard { "|og1" } else { "|og0" });
    out.push_str(if chain.auto_preamp { "|ap1" } else { "|ap0" });
    out.push_str("|as=");
    out.push_str(&format_number_g17(chain.auto_preamp_start_db));
    out.push_str(if chain.stable_graphic { "|sg1" } else { "|sg0" });
    out.push_str(if chain.low_latency { "|ll1" } else { "|ll0" });
    out.push_str(if chain.minimum_curve_phase {
        "|cmp1"
    } else {
        "|cmp0"
    });
    out.push_str(if chain.minimum_eq_phase {
        "|eqp1"
    } else {
        "|eqp0"
    });

    out.push_str("|eqg=");
    for curve in &chain.eq_graphic_curves {
        for point in curve {
            out.push_str(&format_number_g17(point.frequency));
            out.push(',');
            out.push_str(&format_number_g17(point.gain_db));
            out.push(';');
        }
        out.push('/');
    }

    out.push_str("|cg=");
    for curve in &chain.comparison_curves {
        for point in curve {
            out.push_str(&format_number_g17(point.frequency));
            out.push(',');
            out.push_str(&format_number_g17(point.gain_db));
            out.push(';');
        }
        out.push('/');
    }

    out.push_str("|b=");
    for band in &chain.bands {
        out.push_str(&band.filter_type.to_string());
        out.push(',');
        out.push_str(&format_number_g17(band.frequency));
        out.push(',');
        out.push_str(&format_number_g17(band.gain_db));
        out.push(',');
        out.push_str(&format_number_g17(band.quality));
        out.push_str(if band.user_eq { ",eq" } else { ",other" });
        out.push_str(if band.curve_layer {
            ",curve"
        } else {
            ",untagged"
        });
        out.push_str(if band.matched {
            ",matched"
        } else {
            ",cookbook"
        });
        if !band.channels_all {
            out.push_str(",ch");
            out.push_str(&band.channels_speakers.to_string());
            out.push('.');
            out.push_str(&band.channels_indices.to_string());
        }
        out.push(';');
    }

    out.push_str("|g=");
    for curve in &chain.graphic_curves {
        for point in curve {
            out.push_str(&format_number_g17(point.frequency));
            out.push(',');
            out.push_str(&format_number_g17(point.gain_db));
            out.push(';');
        }
        out.push('/');
    }

    out.push_str("|f=");
    for file in &chain.files_read {
        out.push_str(file);
        out.push(';');
    }

    out.push_str("|i=");
    for cmd in &chain.ignored {
        out.push_str(cmd);
        out.push(';');
    }

    out.push_str("|d=");
    for &val in &chain.dsp_values {
        out.push_str(&format_number_g17(val));
        out.push(';');
    }

    out
}

// ============================================================================
// 5. Locale-Free Engine Status JSON Serializer (`/native/system-apo/src/status_json.cpp`)
// ============================================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FinishedSongStatus {
    pub id: String,
    pub level_lufs: f64,
    pub peak_db: f64,
    pub seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SplitStatusReport {
    pub from: String,
    pub state: String,
    pub lag_ms: f64,
    pub source_dsp_ms: f64,
    pub output_eq_ms: f64,
    pub underruns: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockLoadReport {
    pub blocks: u64,
    pub over_half: u64,
    pub slowest_ms: f64,
    pub period_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceAnalysisStatusReport {
    pub version: u32,
    pub kind: String,
    pub owner: String,
    pub source: String,
    pub epoch: u64,
    pub revision: u64,
    pub ready: bool,
    pub voice_ready: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineStatusReport {
    pub endpoint: String,
    pub locked: bool,
    pub processing: bool,
    pub carried: bool,
    pub owner: bool,
    pub reason: String,
    pub problems: Vec<String>,
    pub last_song: Option<FinishedSongStatus>,
    pub channels: u32,
    pub room: String,
    pub rate: u32,
    pub latency: u32,
    pub latency_parts: Vec<(String, u32)>,
    pub latency_active: Vec<String>,
    pub game_mode: bool,
    pub load: Option<BlockLoadReport>,
    pub split: Option<SplitStatusReport>,
    pub source_analysis: Option<SourceAnalysisStatusReport>,
}

fn json_quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        let code = ch as u32;
        if ch == '"' || ch == '\\' {
            out.push('\\');
            out.push(ch);
        } else if code < 0x20 {
            out.push_str(&format!("\\u{:04x}", code));
        } else {
            out.push(ch);
        }
    }
    out.push('"');
    out
}

fn narrow_ascii_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            let code = c as u32;
            if (0x20..0x7f).contains(&code) {
                c
            } else {
                '?'
            }
        })
        .collect()
}

fn format_decimal_2(value: f64) -> String {
    if !value.is_finite() {
        return "0".to_string();
    }
    format!("{:.2}", value)
}

/// Exact port of `status_json` in `/native/system-apo/src/status_json.cpp`:
/// Formats the single-line `\r\n`-terminated JSON status report with locale-free `.2f` decimals
/// and conditional `lastSong`, `load`, `split`, and `sourceAnalysis` objects.
pub fn format_engine_status_json(status: &EngineStatusReport, pid: u32, at: &str) -> String {
    let problems = format!(
        "[{}]",
        status
            .problems
            .iter()
            .map(|p| json_quoted(p))
            .collect::<Vec<_>>()
            .join(",")
    );

    let last_song = match &status.last_song {
        Some(song) => format!(
            ",\"lastSong\":{{\"id\":{},\"level\":{},\"peak\":{},\"seconds\":{}}}",
            json_quoted(&song.id),
            format_decimal_2(song.level_lufs),
            format_decimal_2(song.peak_db),
            format_decimal_2(song.seconds)
        ),
        None => String::new(),
    };

    let parts = format!(
        "{{{}}}",
        status
            .latency_parts
            .iter()
            .map(|(k, v)| format!("{}:{}", json_quoted(k), v))
            .collect::<Vec<_>>()
            .join(",")
    );

    let active = format!(
        "[{}]",
        status
            .latency_active
            .iter()
            .map(|a| json_quoted(a))
            .collect::<Vec<_>>()
            .join(",")
    );

    let split = match &status.split {
        Some(s) => format!(
            ",\"split\":{{\"from\":{},\"state\":{},\"lagMs\":{},\"sourceDspMs\":{},\"outputEqMs\":{},\"underruns\":{}}}",
            json_quoted(&narrow_ascii_id(&s.from)),
            json_quoted(&s.state),
            format_decimal_2(s.lag_ms),
            format_decimal_2(s.source_dsp_ms),
            format_decimal_2(s.output_eq_ms),
            s.underruns
        ),
        None => String::new(),
    };

    let load = match &status.load {
        Some(l) => format!(
            ",\"load\":{{\"blocks\":{},\"overHalf\":{},\"slowestMs\":{},\"periodMs\":{}}}",
            l.blocks,
            l.over_half,
            format_decimal_2(l.slowest_ms),
            format_decimal_2(l.period_ms)
        ),
        None => String::new(),
    };

    let source_analysis = match &status.source_analysis {
        Some(sa) => format!(
            ",\"sourceAnalysis\":{{\"version\":{},\"kind\":{},\"owner\":{},\"source\":{},\"epoch\":{},\"revision\":{},\"ready\":{},\"voiceReady\":{}}}",
            sa.version,
            json_quoted(&sa.kind),
            json_quoted(&sa.owner),
            json_quoted(&sa.source),
            sa.epoch,
            sa.revision,
            if sa.ready { "true" } else { "false" },
            if sa.voice_ready { "true" } else { "false" }
        ),
        None => String::new(),
    };

    format!(
        "{{\"version\":1,\"endpoint\":{},\"pid\":{},\"locked\":{},\"processing\":{},\"carried\":{},\"owner\":{},\"reason\":{},\"problems\":{}{},\"channels\":{},\"room\":{},\"rate\":{},\"latency\":{},\"latencyParts\":{},\"latencyActive\":{},\"gameMode\":{}{}{}{},\"at\":{}}}\r\n",
        json_quoted(&narrow_ascii_id(&status.endpoint)),
        pid,
        if status.locked { "true" } else { "false" },
        if status.processing { "true" } else { "false" },
        if status.carried { "true" } else { "false" },
        if status.owner { "true" } else { "false" },
        json_quoted(&status.reason),
        problems,
        last_song,
        status.channels,
        json_quoted(&status.room),
        status.rate,
        status.latency,
        parts,
        active,
        if status.game_mode { "true" } else { "false" },
        load,
        split,
        source_analysis,
        json_quoted(at)
    )
}

// ============================================================================
// 6. Bounded Config File Reader & Case-Folded Disk `Include:` Tree Loader
//    (`/native/system-apo/src/config_file.cpp` + M5 L3 Disk Reader Closure)
// ============================================================================

/// Reads a single APO config file from disk with a strict `4 MB` (`MAX_APO_CONFIG_FILE_BYTES`)
/// size ceiling, matching `read_config_file` in `/native/system-apo/src/config_file.cpp`.
pub fn read_apo_config_file_bounded(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_APO_CONFIG_FILE_BYTES {
        return None;
    }
    let file = fs::File::open(path).ok()?;
    let mut buf = Vec::with_capacity(meta.len() as usize);
    file.take(MAX_APO_CONFIG_FILE_BYTES + 1)
        .read_to_end(&mut buf)
        .ok()?;
    if (buf.len() as u64) > MAX_APO_CONFIG_FILE_BYTES {
        return None;
    }
    String::from_utf8(buf).ok()
}

/// Resolves an Equalizer APO / FluidEQ config tree directly from `config_dir` on disk,
/// applying Windows-compatible ASCII case-folding on filenames and rejecting any `..` or rooted path
/// in `Include:` directives so an included file cannot escape `config_dir`.
pub fn resolve_apo_endpoint_chain_from_dir(
    config_dir: &Path,
    root_filename: &str,
    endpoint_guid: &str,
    device_name: &str,
) -> Option<ApoEndpointChain> {
    let entries = fs::read_dir(config_dir).ok()?;
    let mut files = BTreeMap::new();
    let mut canonical_names = BTreeMap::new();

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.to_ascii_lowercase().ends_with(".txt") {
            continue;
        }
        if let Some(content) = read_apo_config_file_bounded(&path) {
            canonical_names.insert(name.to_ascii_lowercase(), name.to_string());
            files.insert(name.to_string(), content.clone());
            files.insert(name.to_ascii_lowercase(), content);
        }
    }

    let root_key = canonical_names
        .get(&root_filename.to_ascii_lowercase())
        .cloned()
        .unwrap_or_else(|| root_filename.to_string());

    if !files.contains_key(&root_key) {
        return None;
    }

    Some(resolve_apo_endpoint_chain(
        &root_key,
        endpoint_guid,
        device_name,
        &files,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_config_name_normalizes_guid_and_rejects_traversal() {
        assert_eq!(
            endpoint_config_name("fluideq-dsp", "{11111111-2222-3333-AAAA-555555555555}"),
            Some("fluideq-dsp-11111111-2222-3333-aaaa-555555555555.txt".to_string())
        );
        assert_eq!(endpoint_config_name("fluideq-dsp", "../evil"), None);
        assert_eq!(
            endpoint_config_name("fluideq-dsp", "{11111111-2222-3333-GGGG-555555555555}"),
            None
        );
    }

    #[test]
    fn programme_parser_requires_16_hex_song_id_and_guards_peak_without_level() {
        let valid = parse_programme("song = 0123456789abcdef\nlevel = -14.25\npeak = -1.10\n");
        assert_eq!(valid.song_id, 0x0123_4567_89ab_cdef);
        assert!((valid.level_lufs - (-14.25)).abs() < 1e-9);
        assert!((valid.peak_db - (-1.10)).abs() < 1e-9);

        // Peak without valid level must be dropped to -120.0
        let peak_only = parse_programme("song = 0123456789abcdef\npeak = -1.10\n");
        assert_eq!(peak_only.song_id, 0x0123_4567_89ab_cdef);
        assert_eq!(peak_only.level_lufs, -120.0);
        assert_eq!(peak_only.peak_db, -120.0);

        // Zero song ID is invalid
        let zero_id = parse_programme("song = 0000000000000000\nlevel = -14.0\n");
        assert_eq!(zero_id.song_id, 0);
    }

    #[test]
    fn split_role_enforces_single_primary_source_and_prevents_chaining() {
        let main_guid = "{11111111-2222-3333-4444-555555555555}";
        let sec_guid = "{AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE}";
        let third_guid = "{99999999-8888-7777-6666-555555555555}";
        let text = format!(
            "# main {}\n{} {} 0.75\n{} {} 0.50\n",
            main_guid, main_guid, sec_guid, sec_guid, third_guid
        );

        let main_role = split_role_of(&text, main_guid);
        assert!(main_role.primary);
        assert!(main_role.source);
        assert!(!main_role.is_target());

        let sec_role = split_role_of(&text, sec_guid);
        assert!(!sec_role.primary);
        assert!(sec_role.is_target());
        assert_eq!(sec_role.from, main_guid);
        assert!((sec_role.volume - 0.75).abs() < 1e-6);

        // Chaining sec -> third was rejected because only one source (`main_guid`) is permitted
        let third_role = split_role_of(&text, third_guid);
        assert!(!third_role.is_target());
    }

    #[test]
    fn chain_signature_and_status_json_match_native_format() {
        let mut input = SignatureChainInput::default();
        input.matched = true;
        input.preamp_db = -3.5;
        input.low_latency = true;
        input.dsp_values = vec![1.0, 0.0, -14.0];
        let sig = signature_of_chain(&input);
        assert!(sig.starts_with("m1|c=|cp=0|s=-|p=-3.5|og0|ap0|as=0|sg0|ll1|cmp0|eqp0|"));
        assert!(sig.ends_with("|d=1;0;-14;"));

        let mut status = EngineStatusReport::default();
        status.endpoint = "{11111111-2222-3333-4444-555555555555}".to_string();
        status.locked = true;
        status.processing = true;
        status.owner = true;
        status.channels = 2;
        status.rate = 48_000;
        status.last_song = Some(FinishedSongStatus {
            id: "0123456789abcdef".to_string(),
            level_lufs: -11.84,
            peak_db: -0.92,
            seconds: 184.5,
        });
        let json = format_engine_status_json(&status, 4242, "2026-11-04T18:00:00Z");
        assert!(json.ends_with("\r\n"));
        assert!(json.contains("\"lastSong\":{\"id\":\"0123456789abcdef\",\"level\":-11.84,\"peak\":-0.92,\"seconds\":184.50}"));
    }
}
