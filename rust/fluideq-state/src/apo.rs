/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Equalizer APO Config Tree Resolver (`fluideq_engine::config`) & Windows `FxProperties`
//! 8-Rung Slot Registration Planner (`fluideq_engine::setup::fx_list`).
//!
//! Strictly ports two critical modules from `/native/system-apo/`:
//! 1. `/native/system-apo/include/fluideq_engine/config.h`:
//!    - `format_bands_as_apo_lines`, `parse_apo_filter_line` (supporting `PK`/`PEQ`/`MODAL`,
//!      `LSC`/`LS`/`LSQ`, `HSC`/`HS`/`HSQ`, `NO`, `LPQ`/`LP`, `HPQ`/`HP`, `BP`, `Q` and `BW Oct`),
//!      `parse_apo_graphic_eq`, `parse_apo_channel_selection` (`L`, `R`, `C`, `LFE`, `RL`, `RR`, `SL`, `SR`, `all`),
//!      `device_matches_endpoint`, and 8-level cycle-safe `Include:` tree resolver.
//! 2. `/native/system-apo/setup/fx_list.cpp`:
//!    - Pure, side-effect-free `FxValues` planner (`plan_attach`, `plan_detach`, `default_slot_for`,
//!      `plan_suspend_apo`) covering all three Windows `FxProperties` generations (`composite` pids 13/14/15,
//!      `single` pids 5/6/7, `legacy` pids 1/2 LFX/GFX) so vendor speaker-protection APOs are mirrored
//!      forward and never overwritten, while Windows' 4 inbox default APO CLSIDs (`WM LFX APO` / `WM GFX APO`)
//!      can be cleanly replaced on single-value Bluetooth/USB endpoints.

use crate::biquad::{BandFilterKind, EqBandSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const DEFAULT_PROCESSING_MODE_CLSID: &str = "{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}";
pub const FLUID_ENGINE_APO_CLSID: &str = "{B7E2C4D1-5A8F-4C3E-9D2B-6F1A0C8E7D34}";

pub const EQUALIZER_APO_CLSIDS: [&str; 2] = [
    "{EACD2258-FCAC-4FF4-B36D-419E924A6D79}",
    "{EC1CC9CE-FAED-4822-828A-82A81A6F018F}",
];

/// Four inbox Windows APO CLSIDs (`WM LFX APO` and `WM GFX APO` from `wdmaudio.inf`).
pub const WINDOWS_DEFAULT_APO_CLSIDS: [&str; 4] = [
    "{62DC1A93-AE24-464C-A43E-452F824C4250}",
    "{637C490D-EEE3-4C0A-973F-371958802DA2}",
    "{C9453E73-8C5C-4463-9984-AF8BAB2F5447}",
    "{13AB3EBD-137E-4903-9D89-60BE8277FD17}",
];

pub const APO_MAX_INCLUDE_DEPTH: usize = 8;
pub const APO_MAX_CHAIN_BANDS: usize = 256;

pub fn format_bands_as_apo_lines(preamp_db: f64, bands: &[EqBandSpec]) -> String {
    let mut lines = Vec::with_capacity(bands.len() + 1);
    // No leading `+`: the engine parses numbers with `std::from_chars`, which
    // rejects it, so a positive gain written that way loses its whole filter.
    lines.push(format!("Preamp: {:.1} dB", preamp_db));
    for (idx, band) in bands.iter().enumerate() {
        let state = if band.enabled { "ON" } else { "OFF" };
        let kind = match band.filter_type {
            BandFilterKind::PK => "PK",
            BandFilterKind::NO => "NO",
            BandFilterKind::LSC => "LSC",
            BandFilterKind::HSC => "HSC",
            BandFilterKind::LPQ => "LPQ",
            BandFilterKind::HPQ => "HPQ",
            BandFilterKind::BP => "BP",
        };
        if matches!(
            band.filter_type,
            BandFilterKind::PK | BandFilterKind::LSC | BandFilterKind::HSC
        ) {
            lines.push(format!(
                "Filter {}: {} {} Fc {:.1} Hz Gain {:.1} dB Q {:.2}",
                idx + 1,
                state,
                kind,
                band.frequency,
                band.gain_db,
                band.quality
            ));
        } else {
            lines.push(format!(
                "Filter {}: {} {} Fc {:.1} Hz Q {:.2}",
                idx + 1,
                state,
                kind,
                band.frequency,
                band.quality
            ));
        }
    }
    lines.join("\n")
}

pub const LOW_FREQUENCY_SPEAKER: u32 = 7;
pub const APO_MAX_GRAPHIC_CURVES: usize = 32;
pub const APO_MAX_GRAPHIC_POINTS: usize = 16_384;
pub const APO_MAX_DSP_VALUES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ApoGraphicPoint {
    pub frequency: f64,
    pub gain_db: f64,
}

pub type GraphicEqPoint = ApoGraphicPoint;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApoChannelSelection {
    pub all: bool,
    pub speakers: u32,
    pub indices: u32,
    pub speakers_mask: u32,
    pub indices_mask: u32,
}

pub type ChannelSelection = ApoChannelSelection;

impl Default for ApoChannelSelection {
    fn default() -> Self {
        Self {
            all: true,
            speakers: 0,
            indices: 0,
            speakers_mask: 0,
            indices_mask: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApoTokenizedLine {
    pub command: String,
    pub body: String,
}

/// Strips a UTF-16LE (`0xFF 0xFE`) or UTF-8 (`0xEF 0xBB 0xBF`) byte-order mark,
/// transcoding UTF-16LE (including surrogate pairs) to UTF-8.
///
/// Mirrors `strip_bom_and_decode` in `/native/system-apo/src/config_grammar.cpp`.
pub fn strip_bom_and_decode_bytes(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let mut out = String::new();
        let mut i = 2usize;
        while i + 1 < bytes.len() {
            let mut unit = u32::from(bytes[i]) | (u32::from(bytes[i + 1]) << 8);
            i += 2;
            if (0xD800..=0xDBFF).contains(&unit) && i + 1 < bytes.len() {
                let unit2 = u32::from(bytes[i]) | (u32::from(bytes[i + 1]) << 8);
                if (0xDC00..=0xDFFF).contains(&unit2) {
                    unit = 0x10000 + ((unit - 0xD800) << 10) + (unit2 - 0xDC00);
                    i += 2;
                }
            }
            if let Some(ch) = char::from_u32(unit) {
                out.push(ch);
            }
        }
        return out;
    }
    if bytes.len() >= 3 && bytes[0] == 0xEF && bytes[1] == 0xBB && bytes[2] == 0xBF {
        return String::from_utf8_lossy(&bytes[3..]).into_owned();
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn trim_apo_line_ws(s: &str) -> &str {
    s.trim_matches(|c| c == ' ' || c == '\t' || c == '\r')
}

/// Tokenizes an Equalizer APO / FluidEQ config text into `Command: Body` lines,
/// promoting the 6 `# FluidEQ*:` metadata comments into first-class commands before
/// stripping `#` comments, and stripping trailing `<space><digits>` from `Filter <N>`.
///
/// Mirrors `fluideq_engine::tokenize` in `/native/system-apo/src/config_grammar.cpp`.
pub fn tokenize_apo_config(text: &str) -> Vec<ApoTokenizedLine> {
    let decoded = strip_bom_and_decode_bytes(text.as_bytes());
    let mut lines = Vec::new();
    for raw_part in decoded.split('\n') {
        let mut raw = trim_apo_line_ws(raw_part);
        if raw.starts_with("# FluidEQAutoPreamp: ")
            || raw.starts_with("# FluidEQCurveStage: ")
            || raw.starts_with("# FluidEQCurveLayer: ")
            || raw.starts_with("# FluidEQEqLayer: ")
            || raw.starts_with("# FluidEQFilterDesign: ")
            || raw.starts_with("# FluidEQLowLatency: ")
        {
            raw = &raw[2..];
        }
        if let Some(hash_idx) = raw.find('#') {
            raw = &raw[..hash_idx];
        }
        raw = trim_apo_line_ws(raw);
        if let Some(colon_idx) = raw.find(':') {
            let mut command = trim_apo_line_ws(&raw[..colon_idx]);
            let body = trim_apo_line_ws(&raw[colon_idx + 1..]);
            let bytes = command.as_bytes();
            let mut digit_start = bytes.len();
            while digit_start > 0 && bytes[digit_start - 1].is_ascii_digit() {
                digit_start -= 1;
            }
            if digit_start > 0 && digit_start < bytes.len() && bytes[digit_start - 1] == b' ' {
                command = &command[..digit_start - 1];
            }
            if !command.is_empty() {
                lines.push(ApoTokenizedLine {
                    command: command.to_string(),
                    body: body.to_string(),
                });
            }
        }
    }
    lines
}

/// Parses an Equalizer APO `Channel:` body (`all`, `L`, `R`, `C`, `LFE`/`SUB`, `SL`, `SR`, `RL`, `RR`, or 1-based channel indices `1..=32`).
///
/// Mirrors `fluideq_engine::parse_channels` in `/native/system-apo/src/config_grammar.cpp`.
pub fn parse_apo_channel_selection(body: &str) -> ApoChannelSelection {
    let mut sel = ApoChannelSelection {
        all: false,
        speakers: 0,
        indices: 0,
        speakers_mask: 0,
        indices_mask: 0,
    };
    for raw_token in body.split([' ', '\t']) {
        let token = raw_token.trim_matches(',');
        if token.is_empty() {
            continue;
        }
        let upper = token.to_ascii_uppercase();
        match upper.as_str() {
            "ALL" => return ApoChannelSelection::default(),
            "L" | "FL" => sel.speakers |= 1 << 0,
            "R" | "FR" => sel.speakers |= 1 << 1,
            "C" => sel.speakers |= 1 << 2,
            "SL" => sel.speakers |= 1 << 3,
            "SR" => sel.speakers |= 1 << 4,
            "RL" | "BL" => sel.speakers |= 1 << 5,
            "RR" | "BR" => sel.speakers |= 1 << 6,
            "LFE" | "SUB" => sel.speakers |= 1 << LOW_FREQUENCY_SPEAKER,
            _ => {
                if let Ok(ch_num) = token.parse::<u32>() {
                    if (1..=32).contains(&ch_num) {
                        sel.indices |= 1 << (ch_num - 1);
                    }
                }
            }
        }
    }
    if sel.speakers == 0 && sel.indices == 0 {
        return ApoChannelSelection::default();
    }
    sel.speakers_mask = sel.speakers;
    sel.indices_mask = sel.indices;
    sel
}

/// Converts bandwidth in octaves (`BW Oct <o>`) to Q factor (`sqrt(2^oct) / (2^oct - 1)`),
/// matching `parse_filter` in `/native/system-apo/src/config_grammar.cpp` (lines 324–325).
fn bandwidth_octaves_to_q(bw_oct: f64) -> Option<f64> {
    if !bw_oct.is_finite() || bw_oct <= 0.0 {
        return None;
    }
    let factor = 2.0_f64.powf(bw_oct);
    let denom = factor - 1.0;
    if denom > 0.0 && factor.is_finite() {
        let q = factor.sqrt() / denom;
        if q > 0.0 && q.is_finite() {
            return Some(q);
        }
    }
    None
}

/// Parses an Equalizer APO `Filter:` body:
/// `ON <type> Fc <f> Hz [Gain <g> dB] (Q <q> | BW Oct <o>)`.
///
/// Mirrors `fluideq_engine::parse_filter` in `/native/system-apo/src/config_grammar.cpp`.
pub fn parse_apo_filter_line(body: &str, id: u32, ear: u8) -> Option<EqBandSpec> {
    let tokens: Vec<&str> = body
        .split([' ', '\t'])
        .filter(|s| !s.is_empty())
        .collect();
    let mut i = 0usize;
    if i >= tokens.len() || !tokens[i].eq_ignore_ascii_case("ON") {
        return None;
    }
    i += 1;
    if i >= tokens.len() {
        return None;
    }
    let filter_type = match tokens[i].to_ascii_uppercase().as_str() {
        "PK" | "PEQ" | "MODAL" => BandFilterKind::PK,
        "NO" | "NOTCH" => BandFilterKind::NO,
        "LSC" | "LS" | "LSQ" => BandFilterKind::LSC,
        "HSC" | "HS" | "HSQ" => BandFilterKind::HSC,
        "LPQ" | "LP" => BandFilterKind::LPQ,
        "HPQ" | "HP" => BandFilterKind::HPQ,
        "BP" => BandFilterKind::BP,
        _ => return None,
    };
    i += 1;

    if i + 1 >= tokens.len() || !tokens[i].eq_ignore_ascii_case("Fc") {
        return None;
    }
    i += 1;
    let frequency: f64 = tokens[i].parse().ok()?;
    if !(frequency > 0.0 && frequency.is_finite()) {
        return None;
    }
    i += 1;
    if i >= tokens.len() || !tokens[i].eq_ignore_ascii_case("Hz") {
        return None;
    }
    i += 1;

    let mut gain_db: f64 = 0.0;
    if i + 2 < tokens.len() && tokens[i].eq_ignore_ascii_case("Gain") {
        gain_db = tokens[i + 1].parse().ok()?;
        if !gain_db.is_finite() || !tokens[i + 2].eq_ignore_ascii_case("dB") {
            return None;
        }
        i += 3;
    }

    let quality: f64;
    if i + 1 < tokens.len() && tokens[i].eq_ignore_ascii_case("Q") {
        quality = tokens[i + 1].parse().ok()?;
        i += 2;
    } else if i + 2 < tokens.len()
        && tokens[i].eq_ignore_ascii_case("BW")
        && tokens[i + 1].eq_ignore_ascii_case("Oct")
    {
        let octaves: f64 = tokens[i + 2].parse().ok()?;
        quality = bandwidth_octaves_to_q(octaves)?;
        i += 3;
    } else {
        return None;
    }

    if !(quality > 0.0 && quality.is_finite()) || i != tokens.len() {
        return None;
    }

    Some(EqBandSpec {
        id,
        enabled: true,
        filter_type,
        frequency,
        gain_db,
        quality,
        dynamic: false,
        threshold_db: -18.0,
        ear,
    })
}

/// Parses an Equalizer APO / Squiglink text into its preamp and bands.
///
/// Every well-formed `Filter:` line becomes a band; `Preamp:` sets the trim.
/// Anything else is skipped, which is how the system engine treats a line it
/// does not recognise.
pub fn parse_apo_curve(text: &str) -> (Option<f64>, Vec<EqBandSpec>) {
    let mut preamp_db: Option<f64> = None;
    let mut bands: Vec<EqBandSpec> = Vec::new();
    for line in tokenize_apo_config(text) {
        if line.command.eq_ignore_ascii_case("Preamp") {
            let value = line
                .body
                .split([' ', '\t'])
                .find_map(|token| token.trim().parse::<f64>().ok());
            if let Some(db) = value {
                if db.is_finite() {
                    preamp_db = Some(db.clamp(-24.0, 24.0));
                }
            }
        } else if line.command.eq_ignore_ascii_case("Filter") {
            let id = (bands.len() + 1) as u32;
            let trimmed = line.body.trim_start();
            let (body, enabled) = if trimmed
                .get(..3)
                .is_some_and(|head| head.eq_ignore_ascii_case("OFF"))
            {
                (format!("ON{}", &trimmed[3..]), false)
            } else {
                (line.body.clone(), true)
            };
            if let Some(mut band) = parse_apo_filter_line(&body, id, 0) {
                band.enabled = enabled;
                bands.push(band);
            }
        }
    }
    (preamp_db, bands)
}

/// Parses an Equalizer APO `GraphicEQ:` body (`<freq> <gain>; <freq> <gain>; ...`) all-or-nothing,
/// capped at `APO_MAX_GRAPHIC_POINTS` (`16384`) points.
///
/// Mirrors `fluideq_engine::parse_graphic` in `/native/system-apo/src/config_grammar.cpp`.
pub fn parse_apo_graphic_eq(body: &str) -> Vec<ApoGraphicPoint> {
    let mut points = Vec::new();
    for segment in body.split(';') {
        let trimmed = trim_apo_line_ws(segment);
        let tokens: Vec<&str> = trimmed
            .split([' ', '\t'])
            .filter(|s| !s.is_empty())
            .collect();
        if tokens.len() != 2 {
            return Vec::new();
        }
        let (Ok(freq), Ok(gain)) = (tokens[0].parse::<f64>(), tokens[1].parse::<f64>()) else {
            return Vec::new();
        };
        if !(freq >= 0.0 && freq.is_finite() && gain.is_finite())
            || points.len() >= APO_MAX_GRAPHIC_POINTS
        {
            return Vec::new();
        }
        points.push(ApoGraphicPoint {
            frequency: freq,
            gain_db: gain,
        });
    }
    points
}

/// Parses `fluideq-dsp-<guid>.txt` into up to `APO_MAX_DSP_VALUES` (`128`) finite `f64` numbers.
/// Any non-finite or unparseable token invalidates the entire vector (returns empty).
///
/// Mirrors `fluideq_engine::parse_dsp_values` in `/native/system-apo/src/config_grammar.cpp`.
pub fn parse_apo_dsp_values(text: &str) -> Vec<f64> {
    let decoded = strip_bom_and_decode_bytes(text.as_bytes());
    let mut values = Vec::new();
    for raw_line in decoded.split('\n') {
        let line = trim_apo_line_ws(raw_line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        for tok in line.split([' ', '\t']).filter(|s| !s.is_empty()) {
            let Ok(v) = tok.parse::<f64>() else {
                return Vec::new();
            };
            if !v.is_finite() || values.len() >= APO_MAX_DSP_VALUES {
                return Vec::new();
            }
            values.push(v);
        }
    }
    values
}

/// Checks whether an Equalizer APO `Device:` pattern matches the given endpoint `(guid, friendly_name)`.
///
/// Mirrors `fluideq_engine::device_matches` in `/native/system-apo/src/config.cpp`.
pub fn device_matches_endpoint(pattern: &str, endpoint_guid: &str, friendly_name: &str) -> bool {
    let pat = trim_apo_line_ws(pattern);
    if pat.is_empty() || pat.eq_ignore_ascii_case("all") {
        return true;
    }
    let pat_lower = pat.to_ascii_lowercase();
    let guid_lower = trim_apo_line_ws(endpoint_guid).to_ascii_lowercase();
    let name_lower = trim_apo_line_ws(friendly_name).to_ascii_lowercase();

    if !guid_lower.is_empty() && pat_lower.contains(&guid_lower) {
        return true;
    }

    let mut checked_any = false;
    for clause in pat_lower.split(';') {
        let token = trim_apo_line_ws(clause);
        if token.is_empty() {
            continue;
        }
        if !guid_lower.is_empty() && token == guid_lower {
            return true;
        }
        checked_any = true;
        if name_lower.is_empty() || !name_lower.contains(token) {
            return false;
        }
    }
    checked_any
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApoResolvedBand {
    pub spec: EqBandSpec,
    pub user_eq: bool,
    pub curve_layer: bool,
    pub matched: bool,
    pub channels: ApoChannelSelection,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedApoChain {
    pub matched: bool,
    pub convolution_path: String,
    pub convolution_passes: u32,
    pub preamp_db: f64,
    pub output_guard: bool,
    pub auto_preamp: bool,
    pub auto_preamp_start_db: f64,
    pub stable_graphic: bool,
    pub low_latency: bool,
    pub minimum_curve_phase: bool,
    pub minimum_eq_phase: bool,
    pub classic_eq_treble: bool,
    pub classic_curve_treble: bool,
    pub bands: Vec<EqBandSpec>,
    pub apo_bands: Vec<ApoResolvedBand>,
    pub graphic_curves: Vec<Vec<ApoGraphicPoint>>,
    pub comparison_curves: Vec<Vec<ApoGraphicPoint>>,
    pub eq_graphic_curves: Vec<Vec<ApoGraphicPoint>>,
    pub files_read: Vec<String>,
    pub ignored: Vec<String>,
    pub dsp_values: Vec<f64>,
}

pub type ApoEndpointChain = ResolvedApoChain;

impl Default for ResolvedApoChain {
    fn default() -> Self {
        Self {
            matched: false,
            convolution_path: String::new(),
            convolution_passes: 1,
            preamp_db: 0.0,
            output_guard: false,
            auto_preamp: false,
            auto_preamp_start_db: 0.0,
            stable_graphic: false,
            low_latency: false,
            minimum_curve_phase: true,
            minimum_eq_phase: true,
            classic_eq_treble: false,
            classic_curve_treble: false,
            bands: Vec::new(),
            apo_bands: Vec::new(),
            graphic_curves: Vec::new(),
            comparison_curves: Vec::new(),
            eq_graphic_curves: Vec::new(),
            files_read: Vec::new(),
            ignored: Vec::new(),
            dsp_values: Vec::new(),
        }
    }
}

fn note_ignored(ignored: &mut Vec<String>, command: &str) {
    if !ignored.iter().any(|existing| existing == command) {
        ignored.push(command.to_string());
    }
}

fn has_dotdot_segment(path: &str) -> bool {
    path.split(['\\', '/']).any(|seg| seg == "..")
}

fn has_drive_or_unc_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 2 && bytes[1] == b':')
        || (!bytes.is_empty() && (bytes[0] == b'\\' || bytes[0] == b'/'))
}

fn canonical_bare_guid(endpoint_guid: &str) -> Option<String> {
    let mut g = endpoint_guid.trim();
    if g.len() == 38 && g.starts_with('{') && g.ends_with('}') {
        g = &g[1..37];
    }
    if g.len() != 36 {
        return None;
    }
    let mut out = String::with_capacity(36);
    for (idx, b) in g.bytes().enumerate() {
        if matches!(idx, 8 | 13 | 18 | 23) {
            if b != b'-' {
                return None;
            }
            out.push('-');
        } else if b.is_ascii_hexdigit() {
            out.push(b.to_ascii_lowercase() as char);
        } else {
            return None;
        }
    }
    Some(out)
}

fn lookup_config_file<'a>(
    files: &'a BTreeMap<String, String>,
    name: &str,
) -> Option<(String, &'a String)> {
    let trimmed = name.trim_matches('"');
    if has_dotdot_segment(trimmed) || has_drive_or_unc_prefix(trimmed) {
        return None;
    }
    if let Some((k, v)) = files.get_key_value(trimmed) {
        return Some((k.clone(), v));
    }
    let lower = trimmed.to_ascii_lowercase();
    for (k, v) in files {
        if k.eq_ignore_ascii_case(&lower) {
            return Some((k.clone(), v));
        }
    }
    None
}

#[derive(Clone)]
struct IncludeFrame {
    path_key: String,
    lines: Vec<ApoTokenizedLine>,
    next_line: usize,
    matching: bool,
    curve_layer: bool,
    eq_layer: bool,
    matched_design: bool,
    channels: ApoChannelSelection,
}

/// Resolves an Equalizer APO configuration tree starting at `root_file` (capped at 8 nested `Include:` frames),
/// matching `fluideq_engine::resolve_chain` in `/native/system-apo/src/config.cpp`.
pub fn resolve_apo_endpoint_chain(
    root_file: &str,
    endpoint_guid: &str,
    friendly_name: &str,
    files: &BTreeMap<String, String>,
) -> ResolvedApoChain {
    let mut chain = ResolvedApoChain::default();
    let no_endpoint = endpoint_guid.trim().is_empty() && friendly_name.trim().is_empty();

    // 1. Read per-endpoint phase, treble, and DSP sidecar files when an endpoint GUID is present (`config.cpp` lines 237–285)
    if let Some(bare_guid) = canonical_bare_guid(endpoint_guid) {
        let curve_phase_name = format!("fluideq-curve-phase-{}.txt", bare_guid);
        if let Some((key, text)) = lookup_config_file(files, &curve_phase_name) {
            chain.files_read.push(key);
            chain.minimum_curve_phase = !text.starts_with('A');
        }
        let eq_phase_name = format!("fluideq-eq-phase-{}.txt", bare_guid);
        if let Some((key, text)) = lookup_config_file(files, &eq_phase_name) {
            chain.files_read.push(key);
            chain.minimum_eq_phase = !text.starts_with('A');
        }
        let read_treble_classic = |stem: &str, chain_files: &mut Vec<String>| -> bool {
            let file_name = format!("{}-{}.txt", stem, bare_guid);
            let Some((key, text)) = lookup_config_file(files, &file_name) else {
                return false;
            };
            chain_files.push(key);
            let mut end = text.len();
            let bytes = text.as_bytes();
            while end > 0 && matches!(bytes[end - 1], b' ' | b'\t' | b'\r' | b'\n') {
                end -= 1;
            }
            text[..end].eq_ignore_ascii_case("classic")
        };
        chain.classic_eq_treble = read_treble_classic("fluideq-eq-treble", &mut chain.files_read);
        chain.classic_curve_treble =
            read_treble_classic("fluideq-curve-treble", &mut chain.files_read);

        let dsp_name = format!("fluideq-dsp-{}.txt", bare_guid);
        if let Some((key, text)) = lookup_config_file(files, &dsp_name) {
            chain.files_read.push(key);
            chain.dsp_values = parse_apo_dsp_values(text);
            for line in tokenize_apo_config(text) {
                if line.command.eq_ignore_ascii_case("FluidEQLowLatency")
                    && line.body.eq_ignore_ascii_case("ON")
                {
                    chain.low_latency = true;
                }
            }
        }
    }

    let Some((root_key, root_text)) = lookup_config_file(files, root_file) else {
        return chain;
    };
    chain.files_read.push(root_key.clone());
    chain.matched = true;

    let mut stack: Vec<IncludeFrame> = Vec::with_capacity(APO_MAX_INCLUDE_DEPTH);
    stack.push(IncludeFrame {
        path_key: root_key.to_ascii_lowercase(),
        lines: tokenize_apo_config(root_text),
        next_line: 0,
        matching: true,
        curve_layer: false,
        eq_layer: false,
        matched_design: false,
        channels: ApoChannelSelection::default(),
    });

    while let Some(top_idx) = stack.len().checked_sub(1) {
        if stack[top_idx].next_line >= stack[top_idx].lines.len() {
            stack.pop();
            continue;
        }
        let line = stack[top_idx].lines[stack[top_idx].next_line].clone();
        stack[top_idx].next_line += 1;

        if line.command.eq_ignore_ascii_case("Device") {
            let matches = if no_endpoint {
                let b = trim_apo_line_ws(&line.body);
                b.is_empty() || b.eq_ignore_ascii_case("all")
            } else {
                device_matches_endpoint(&line.body, endpoint_guid, friendly_name)
            };
            stack[top_idx].matching = matches;
            if matches {
                chain.matched = true;
            }
            continue;
        }

        if line.command.eq_ignore_ascii_case("Channel") {
            if stack[top_idx].matching {
                stack[top_idx].channels = parse_apo_channel_selection(&line.body);
            }
            continue;
        }

        if !stack[top_idx].matching {
            continue;
        }

        if line.command.eq_ignore_ascii_case("Include") {
            if stack.len() >= APO_MAX_INCLUDE_DEPTH || line.body.is_empty() {
                continue;
            }
            let Some((child_key, child_text)) = lookup_config_file(files, &line.body) else {
                continue;
            };
            let child_key_lower = child_key.to_ascii_lowercase();
            if stack.iter().any(|f| f.path_key == child_key_lower) {
                continue;
            }
            if !chain
                .files_read
                .iter()
                .any(|f| f.eq_ignore_ascii_case(&child_key))
            {
                chain.files_read.push(child_key);
            }
            let parent = &stack[top_idx];
            stack.push(IncludeFrame {
                path_key: child_key_lower,
                lines: tokenize_apo_config(child_text),
                next_line: 0,
                matching: parent.matching,
                curve_layer: parent.curve_layer,
                eq_layer: parent.eq_layer,
                matched_design: parent.matched_design,
                channels: parent.channels,
            });
            continue;
        }

        if line.command.eq_ignore_ascii_case("Convolution") {
            let raw_conv = line.body.trim_matches('"');
            if !raw_conv.is_empty()
                && !has_dotdot_segment(raw_conv)
                && !has_drive_or_unc_prefix(raw_conv)
            {
                if !chain.convolution_path.is_empty()
                    && chain.convolution_path.eq_ignore_ascii_case(raw_conv)
                {
                    if chain.convolution_passes < 8 {
                        chain.convolution_passes += 1;
                    }
                } else {
                    chain.convolution_path = raw_conv.to_string();
                    chain.convolution_passes = 1;
                }
            }
            continue;
        }

        if line.command.eq_ignore_ascii_case("FluidEQAutoPreamp") {
            if line.body.eq_ignore_ascii_case("ON") {
                if !chain.auto_preamp {
                    chain.output_guard = true;
                    chain.auto_preamp = true;
                    chain.auto_preamp_start_db = chain.preamp_db.min(0.0);
                    chain.preamp_db = 0.0;
                }
            } else if line.body.eq_ignore_ascii_case("OFF") {
                chain.output_guard = false;
                chain.auto_preamp = false;
                chain.auto_preamp_start_db = 0.0;
            }
            continue;
        }

        if line.command.eq_ignore_ascii_case("FluidEQCurveStage") {
            chain.stable_graphic = line.body.eq_ignore_ascii_case("ON");
            continue;
        }

        if line.command.eq_ignore_ascii_case("FluidEQCurveLayer") {
            stack[top_idx].curve_layer = line.body.eq_ignore_ascii_case("ON");
            continue;
        }

        if line.command.eq_ignore_ascii_case("FluidEQEqLayer") {
            stack[top_idx].eq_layer = line.body.eq_ignore_ascii_case("ON");
            continue;
        }

        if line.command.eq_ignore_ascii_case("FluidEQFilterDesign") {
            if line.body.eq_ignore_ascii_case("MATCHED") {
                stack[top_idx].matched_design = true;
            } else if line.body.eq_ignore_ascii_case("COOKBOOK") {
                stack[top_idx].matched_design = false;
            }
            continue;
        }

        if line.command.eq_ignore_ascii_case("FluidEQLowLatency") {
            chain.low_latency = line.body.eq_ignore_ascii_case("ON");
            continue;
        }

        if line.command.eq_ignore_ascii_case("Preamp") {
            let tokens: Vec<&str> = line
                .body
                .split([' ', '\t'])
                .filter(|s| !s.is_empty())
                .collect();
            if !tokens.is_empty()
                && (tokens.len() == 1
                    || (tokens.len() == 2 && tokens[1].eq_ignore_ascii_case("dB")))
            {
                if let Ok(val) = tokens[0].parse::<f64>() {
                    if val.is_finite() {
                        // `config.cpp` reads every Preamp into the same field:
                        // one above the guard directive is its starting point,
                        // one after it is the user's own fixed trim.
                        chain.preamp_db = val;
                        chain.matched = true;
                    }
                }
            }
            continue;
        }

        if line.command.eq_ignore_ascii_case("Filter") {
            if chain.bands.len() >= APO_MAX_CHAIN_BANDS {
                note_ignored(&mut chain.ignored, "Filter beyond the band limit");
                continue;
            }
            let channels = stack[top_idx].channels;
            let ear = if channels.all {
                0
            } else if channels.speakers == (1 << 0) {
                1
            } else if channels.speakers == (1 << 1) {
                2
            } else {
                0
            };
            let next_id = (chain.bands.len() + 1) as u32;
            if let Some(band_spec) = parse_apo_filter_line(&line.body, next_id, ear) {
                chain.apo_bands.push(ApoResolvedBand {
                    spec: band_spec,
                    user_eq: stack[top_idx].eq_layer,
                    curve_layer: stack[top_idx].curve_layer,
                    matched: stack[top_idx].matched_design,
                    channels,
                });
                chain.bands.push(band_spec);
            }
            continue;
        }

        if line.command.eq_ignore_ascii_case("GraphicEQ") {
            if chain.graphic_curves.len() >= APO_MAX_GRAPHIC_CURVES {
                note_ignored(&mut chain.ignored, "GraphicEQ beyond the curve limit");
                continue;
            }
            let pts = parse_apo_graphic_eq(&line.body);
            if !pts.is_empty() {
                if stack[top_idx].curve_layer {
                    chain.comparison_curves.push(pts.clone());
                }
                if stack[top_idx].eq_layer {
                    chain.eq_graphic_curves.push(pts.clone());
                }
                chain.graphic_curves.push(pts);
            }
            continue;
        }

        note_ignored(&mut chain.ignored, &line.command);
    }

    chain
}

// ============================================================================
// 2. Windows `FxProperties` 8-Rung APO Slot Ladder (`/native/system-apo/setup/fx_list.cpp`)
// ============================================================================

pub const FX_SLOT_SFX: usize = 0;
pub const FX_SLOT_MFX: usize = 1;
pub const FX_SLOT_EFX: usize = 2;
pub const FX_SLOT_COUNT: usize = 3;

pub const FX_LEGACY_LFX: usize = 0;
pub const FX_LEGACY_GFX: usize = 1;
pub const FX_LEGACY_COUNT: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApoSlot {
    Efx,
    Mfx,
    Sfx,
    EfxSingle,
    MfxSingle,
    SfxSingle,
    Gfx,
    Lfx,
}

impl ApoSlot {
    pub fn is_legacy(self) -> bool {
        matches!(self, ApoSlot::Gfx | ApoSlot::Lfx)
    }

    pub fn is_single(self) -> bool {
        matches!(
            self,
            ApoSlot::EfxSingle | ApoSlot::MfxSingle | ApoSlot::SfxSingle
        )
    }

    pub fn one_value_name(self) -> &'static str {
        match self {
            ApoSlot::Lfx => "LFX",
            ApoSlot::Gfx => "GFX",
            ApoSlot::SfxSingle => "the SFX value",
            ApoSlot::MfxSingle => "the MFX value",
            _ => "the EFX value",
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointFxValues {
    pub single: [Option<String>; FX_SLOT_COUNT],
    pub composite: [Option<Vec<String>>; FX_SLOT_COUNT],
    pub legacy: [Option<String>; FX_LEGACY_COUNT],
    pub modes: [Option<Vec<String>>; FX_SLOT_COUNT],
    pub composite_was_sz: [bool; FX_SLOT_COUNT],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointFxPlan {
    pub after: EndpointFxValues,
    pub changed: bool,
    pub refused: Option<String>,
}

pub fn equal_clsid_ci(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

pub fn is_windows_default_apo_clsid(clsid: &str) -> bool {
    WINDOWS_DEFAULT_APO_CLSIDS
        .iter()
        .any(|&w| equal_clsid_ci(clsid, w))
}

pub fn is_equalizer_apo_clsid(clsid: &str) -> bool {
    EQUALIZER_APO_CLSIDS
        .iter()
        .any(|&a| equal_clsid_ci(clsid, a))
}

fn holds_effect(val: &Option<String>) -> bool {
    val.as_ref().map(|s| !s.is_empty()).unwrap_or(false)
}

fn has_modern_values(values: &EndpointFxValues) -> bool {
    (0..FX_SLOT_COUNT).any(|i| values.single[i].is_some() || values.composite[i].is_some())
}

pub fn is_legacy_only(values: &EndpointFxValues) -> bool {
    !has_modern_values(values) && (0..FX_LEGACY_COUNT).any(|i| holds_effect(&values.legacy[i]))
}

pub fn is_single_only(values: &EndpointFxValues) -> bool {
    let mut any = false;
    for i in 0..FX_SLOT_COUNT {
        if values.composite[i].is_some() {
            return false;
        }
        any = any || holds_effect(&values.single[i]);
    }
    any
}

/// Determines the default attachment slot for an endpoint (`default_slot_for` in `fx_list.cpp`).
pub fn default_slot_for(original: &EndpointFxValues, combined: bool) -> ApoSlot {
    if is_legacy_only(original) {
        let gfx = &original.legacy[FX_LEGACY_GFX];
        if !holds_effect(gfx)
            || gfx
                .as_deref()
                .map(is_windows_default_apo_clsid)
                .unwrap_or(false)
        {
            return ApoSlot::Gfx;
        }
    }
    let efx = &original.single[FX_SLOT_EFX];
    if is_single_only(original) {
        for (slot, idx) in [
            (ApoSlot::EfxSingle, FX_SLOT_EFX),
            (ApoSlot::MfxSingle, FX_SLOT_MFX),
            (ApoSlot::SfxSingle, FX_SLOT_SFX),
        ] {
            let held = &original.single[idx];
            if !holds_effect(held)
                || held
                    .as_deref()
                    .map(is_windows_default_apo_clsid)
                    .unwrap_or(false)
            {
                return slot;
            }
        }
    } else if (0..FX_SLOT_COUNT).any(|i| original.composite[i].is_some()) {
        // The driver registered its effects as composite lists. Windows falls
        // back to the endpoint value (pid 7) when it cannot build that list
        // with ours on the end — a Realtek output attached beside its own
        // APOs was registered and never once loaded. Where that value is
        // free, Windows' own, or Equalizer APO's, ours belongs there; EAPO is
        // another EQ and the detach backup puts it back.
        let efx_free = !holds_effect(efx)
            || efx
                .as_deref()
                .map(is_windows_default_apo_clsid)
                .unwrap_or(false)
            || efx.as_deref().map(is_equalizer_apo_clsid).unwrap_or(false);
        if efx_free {
            return ApoSlot::EfxSingle;
        }
    }
    if combined {
        ApoSlot::Mfx
    } else {
        ApoSlot::Efx
    }
}

/// Pure function computing the `FxProperties` values after attaching `clsid` to `slot`
/// (`plan_attach` in `/native/system-apo/setup/fx_list.cpp`).
pub fn plan_attach(before: &EndpointFxValues, clsid: &str, slot: ApoSlot) -> EndpointFxPlan {
    let mut after = before.clone();

    if slot.is_legacy() || slot.is_single() {
        let legacy = slot.is_legacy();
        let at = match slot {
            ApoSlot::Lfx => FX_LEGACY_LFX,
            ApoSlot::Gfx => FX_LEGACY_GFX,
            ApoSlot::SfxSingle => FX_SLOT_SFX,
            ApoSlot::MfxSingle => FX_SLOT_MFX,
            _ => FX_SLOT_EFX,
        };
        let held = if legacy {
            &before.legacy[at]
        } else {
            &before.single[at]
        };
        if let Some(existing) = held.as_deref() {
            if !existing.is_empty()
                && !equal_clsid_ci(existing, clsid)
                && !is_windows_default_apo_clsid(existing)
                && !is_equalizer_apo_clsid(existing)
            {
                return EndpointFxPlan {
                    after: before.clone(),
                    changed: false,
                    refused: Some(format!(
                        "{} already holds another effect, {}",
                        slot.one_value_name(),
                        existing
                    )),
                };
            }
        }
        if legacy {
            after.legacy[at] = Some(clsid.to_string());
        } else {
            after.single[at] = Some(clsid.to_string());
            if after.modes[at].is_none() {
                after.modes[at] = Some(modes_for_empty_slot(&after, at));
            }
        }
        let changed = &after != before;
        return EndpointFxPlan {
            after,
            changed,
            refused: None,
        };
    }

    let index = match slot {
        ApoSlot::Sfx => FX_SLOT_SFX,
        ApoSlot::Mfx => FX_SLOT_MFX,
        _ => FX_SLOT_EFX,
    };

    // 1. Mirror singles into composites so vendor APOs remain active
    for at in 0..FX_SLOT_COUNT {
        if after.composite[at].is_none() {
            if let Some(single_val) = before.single[at].as_deref() {
                if !single_val.is_empty() {
                    after.composite[at] = Some(vec![single_val.to_string()]);
                }
            }
        }
    }

    // 2. Mirror legacy LFX/GFX if no modern values exist
    if !has_modern_values(before) {
        for at in 0..FX_LEGACY_COUNT {
            if let Some(leg_val) = before.legacy[at].as_deref() {
                if !leg_val.is_empty() {
                    after.composite[at] = Some(vec![leg_val.to_string()]);
                }
            }
        }
    }

    // 3. Append our CLSID once
    let list = after.composite[index].get_or_insert_with(Vec::new);
    if !list.iter().any(|e| equal_clsid_ci(e, clsid)) {
        list.push(clsid.to_string());
        after.composite_was_sz[index] = false;
    }

    // 4. Populate processing modes for empty slot (`modes_for_empty_slot` from `fx_modes.cpp`)
    if after.modes[index].is_none() {
        after.modes[index] = Some(modes_for_empty_slot(&after, index));
    }

    let changed = &after != before;
    EndpointFxPlan {
        after,
        changed,
        refused: None,
    }
}

/// Pure function computing `FxProperties` values after detaching `clsid` against `backup`
/// (`plan_detach` in `/native/system-apo/setup/fx_list.cpp`).
pub fn plan_detach(
    current: &EndpointFxValues,
    backup: &EndpointFxValues,
    clsid: &str,
) -> EndpointFxPlan {
    let mut after = current.clone();

    for at in 0..FX_SLOT_COUNT {
        let Some(entries) = after.composite[at].as_mut() else {
            continue;
        };
        let was = entries.len();
        entries.retain(|e| !equal_clsid_ci(e, clsid));
        if entries.len() == was {
            continue;
        }
        if entries.is_empty() && backup.composite[at].is_none() {
            after.composite[at] = None;
        }
        if backup.modes[at].is_none() {
            after.modes[at] = None;
        }
        if after.composite[at].is_some()
            && backup.composite_was_sz[at]
            && after.composite[at] == backup.composite[at]
        {
            after.composite_was_sz[at] = true;
        }
    }

    for at in 0..FX_LEGACY_COUNT {
        if after.legacy[at]
            .as_deref()
            .map(|e| equal_clsid_ci(e, clsid))
            .unwrap_or(false)
        {
            after.legacy[at] = backup.legacy[at].clone();
        }
    }
    for at in 0..FX_SLOT_COUNT {
        if after.single[at]
            .as_deref()
            .map(|e| equal_clsid_ci(e, clsid))
            .unwrap_or(false)
        {
            after.single[at] = backup.single[at].clone();
            if backup.modes[at].is_none() {
                after.modes[at] = None;
            }
        }
    }

    let changed = &after != current;
    EndpointFxPlan {
        after,
        changed,
        refused: None,
    }
}

/// Exact port of `slot_of` in `/native/system-apo/setup/fx_list.cpp`:
/// Checks composite lists first (`Efx`, `Mfx`, `Sfx`), then single-value slots (`EfxSingle`, `MfxSingle`, `SfxSingle`),
/// and finally pre-Windows 8.1 legacy slots (`Gfx`, `Lfx`).
pub fn slot_of(values: &EndpointFxValues, clsid: &str) -> Option<ApoSlot> {
    for (slot, at) in [
        (ApoSlot::Efx, FX_SLOT_EFX),
        (ApoSlot::Mfx, FX_SLOT_MFX),
        (ApoSlot::Sfx, FX_SLOT_SFX),
    ] {
        if let Some(list) = values.composite[at].as_ref() {
            if list.iter().any(|entry| equal_clsid_ci(entry, clsid)) {
                return Some(slot);
            }
        }
    }
    for (slot, at) in [
        (ApoSlot::EfxSingle, FX_SLOT_EFX),
        (ApoSlot::MfxSingle, FX_SLOT_MFX),
        (ApoSlot::SfxSingle, FX_SLOT_SFX),
    ] {
        if holds_effect(&values.single[at])
            && values.single[at]
                .as_deref()
                .map(|s| equal_clsid_ci(s, clsid))
                .unwrap_or(false)
        {
            return Some(slot);
        }
    }
    for (slot, at) in [
        (ApoSlot::Gfx, FX_LEGACY_GFX),
        (ApoSlot::Lfx, FX_LEGACY_LFX),
    ] {
        if holds_effect(&values.legacy[at])
            && values.legacy[at]
                .as_deref()
                .map(|s| equal_clsid_ci(s, clsid))
                .unwrap_or(false)
        {
            return Some(slot);
        }
    }
    None
}

/// Exact port of `is_attached` in `/native/system-apo/setup/fx_list.cpp`.
pub fn is_attached(values: &EndpointFxValues, clsid: &str) -> bool {
    slot_of(values, clsid).is_some()
}

/// Exact port of `plan_suspend_apo` (`plan_disable_apo`) in `/native/system-apo/setup/fx_list.cpp`:
/// Mirrors singles/legacies into composite lists first, then strips Equalizer APO's CLSIDs (`EQUALIZER_APO_CLSIDS`)
/// from composite, single, and legacy slots. If nothing of Equalizer APO's was present, drops the mirroring
/// and returns `before` unchanged.
pub fn plan_disable_apo(before: &EndpointFxValues) -> EndpointFxPlan {
    let mut after = before.clone();

    for at in 0..FX_SLOT_COUNT {
        if after.composite[at].is_none() {
            if let Some(s) = before.single[at].as_deref() {
                if !s.is_empty() {
                    after.composite[at] = Some(vec![s.to_string()]);
                }
            }
        }
    }
    if !has_modern_values(before) {
        for at in 0..FX_LEGACY_COUNT {
            if let Some(leg) = before.legacy[at].as_deref() {
                if !leg.is_empty() {
                    after.composite[at] = Some(vec![leg.to_string()]);
                }
            }
        }
    }

    let mut removed = false;
    for at in 0..FX_SLOT_COUNT {
        if let Some(entries) = after.composite[at].as_mut() {
            let was = entries.len();
            entries.retain(|e| !is_equalizer_apo_clsid(e));
            if entries.len() != was {
                removed = true;
                after.composite_was_sz[at] = false;
            }
        }
    }

    for at in 0..FX_SLOT_COUNT {
        if after.single[at]
            .as_deref()
            .map(is_equalizer_apo_clsid)
            .unwrap_or(false)
        {
            after.single[at] = None;
            removed = true;
        }
    }

    for at in 0..FX_LEGACY_COUNT {
        if after.legacy[at]
            .as_deref()
            .map(is_equalizer_apo_clsid)
            .unwrap_or(false)
        {
            after.legacy[at] = None;
            removed = true;
        }
    }

    if !removed {
        return EndpointFxPlan {
            after: before.clone(),
            changed: false,
            refused: None,
        };
    }

    let changed = &after != before;
    EndpointFxPlan {
        after,
        changed,
        refused: None,
    }
}

/// Alias matching the exact C++ function name `plan_suspend_apo` in `fx_list.cpp`.
pub fn plan_suspend_apo(before: &EndpointFxValues) -> EndpointFxPlan {
    plan_disable_apo(before)
}

/// Exact port of `plan_restore_apo` in `/native/system-apo/setup/fx_list.cpp`:
/// Restores `saved` (the exact state before Equalizer APO was suspended, including value types),
/// and then re-attaches `keep` at whichever slot it currently occupies in `current` (if any).
pub fn plan_restore_apo_with_keep(
    current: &EndpointFxValues,
    saved: &EndpointFxValues,
    keep: &str,
) -> EndpointFxPlan {
    let mut after = saved.clone();
    if let Some(ours) = slot_of(current, keep) {
        let again = plan_attach(&after, keep, ours);
        if again.refused.is_none() {
            after = again.after;
        }
    }
    let changed = &after != current;
    EndpointFxPlan {
        after,
        changed,
        refused: None,
    }
}

/// Convenience wrapper for `plan_restore_apo_with_keep` using `FLUID_ENGINE_APO_CLSID`.
pub fn plan_restore_apo(current: &EndpointFxValues, saved: &EndpointFxValues) -> EndpointFxPlan {
    plan_restore_apo_with_keep(current, saved, FLUID_ENGINE_APO_CLSID)
}

/// Exact port of `plan_move` in `/native/system-apo/setup/fx_list.cpp`:
/// Moves `clsid` from its current slot to `to` by detaching against `backup` and re-attaching at `to`.
/// - If `clsid` is not attached or is already at `to`, delegates directly to `plan_attach` so vendor list
///   ordering is never disturbed.
/// - When moving to a legacy or single-value slot, strips any mirrored `composite` / `modes` lists that were
///   not present in `backup` so the driver reads the older generation.
pub fn plan_move(
    before: &EndpointFxValues,
    backup: &EndpointFxValues,
    clsid: &str,
    to: ApoSlot,
) -> EndpointFxPlan {
    let current = slot_of(before, clsid);
    if current.is_none() || current == Some(to) {
        return plan_attach(before, clsid, to);
    }
    let mut cleared = plan_detach(before, backup, clsid).after;
    if to.is_legacy() || to.is_single() {
        for at in 0..FX_SLOT_COUNT {
            if backup.composite[at].is_none() {
                cleared.composite[at] = None;
                cleared.composite_was_sz[at] = false;
            }
            if backup.modes[at].is_none() {
                cleared.modes[at] = None;
            }
        }
    }
    let attached = plan_attach(&cleared, clsid, to);
    if let Some(refused) = attached.refused {
        return EndpointFxPlan {
            after: before.clone(),
            changed: false,
            refused: Some(refused),
        };
    }
    let changed = &attached.after != before;
    EndpointFxPlan {
        after: attached.after,
        changed,
        refused: None,
    }
}

// ============================================================================
// 4. Processing Modes & Per-Endpoint Slot Memory (`/native/system-apo/setup/fx_modes.cpp` & `slot_memory.cpp`)
// ============================================================================

/// `{9E90EA20-B493-4FD1-A1A8-7E1361A956CF}` — Windows RAW processing mode.
pub const RAW_PROCESSING_MODE_CLSID: &str = "{9E90EA20-B493-4FD1-A1A8-7E1361A956CF}";

/// Returns the `modes[]` index (`0 = SFX`, `1 = MFX`, `2 = EFX`) of a composite or single slot,
/// or `None` for the pre-Windows 8.1 `Gfx`/`Lfx` pair.
pub fn modes_index_of_slot(slot: ApoSlot) -> Option<usize> {
    match slot {
        ApoSlot::Sfx | ApoSlot::SfxSingle => Some(FX_SLOT_SFX),
        ApoSlot::Mfx | ApoSlot::MfxSingle => Some(FX_SLOT_MFX),
        ApoSlot::Efx | ApoSlot::EfxSingle => Some(FX_SLOT_EFX),
        ApoSlot::Gfx | ApoSlot::Lfx => None,
    }
}

/// Exact port of `modes_for_empty_slot` in `/native/system-apo/setup/fx_modes.cpp`:
/// - `EFX` gets `DEFAULT` alone.
/// - `SFX` or `MFX` gets `DEFAULT` plus every non-`RAW` processing mode declared by the
///   other per-stream slot (`MFX` or `SFX`), preserving driver-declared film/media modes.
pub fn modes_for_empty_slot(values: &EndpointFxValues, at: usize) -> Vec<String> {
    let mut modes = vec![DEFAULT_PROCESSING_MODE_CLSID.to_string()];
    if at == FX_SLOT_EFX {
        return modes;
    }
    let other = if at == FX_SLOT_SFX {
        FX_SLOT_MFX
    } else {
        FX_SLOT_SFX
    };
    if let Some(other_modes) = values.modes[other].as_ref() {
        for mode in other_modes {
            if !mode.is_empty()
                && !equal_clsid_ci(mode, RAW_PROCESSING_MODE_CLSID)
                && !modes.iter().any(|existing| equal_clsid_ci(existing, mode))
            {
                modes.push(mode.clone());
            }
        }
    }
    modes
}

/// Exact port of `mode_missing` in `/native/system-apo/setup/fx_modes.cpp`:
/// Returns `true` when `clsid` sits in a composite or single slot whose `modes` property is missing.
pub fn mode_missing(values: &EndpointFxValues, clsid: &str) -> bool {
    slot_of(values, clsid)
        .and_then(modes_index_of_slot)
        .map(|at| values.modes[at].is_none())
        .unwrap_or(false)
}

/// Exact port of `plan_mode_in_place` in `/native/system-apo/setup/fx_modes.cpp`:
/// Populates the missing `modes[at]` entry for `clsid`'s current slot without changing its slot or vendor APOs.
pub fn plan_mode_in_place(before: &EndpointFxValues, clsid: &str) -> EndpointFxPlan {
    let mut after = before.clone();
    if let Some(at) = slot_of(before, clsid).and_then(modes_index_of_slot) {
        if before.modes[at].is_none() {
            after.modes[at] = Some(modes_for_empty_slot(before, at));
        }
    }
    let changed = &after != before;
    EndpointFxPlan {
        after,
        changed,
        refused: None,
    }
}

/// Canonical 8-rung ladder order from newest/highest-priority to oldest (`efx` -> `lfx`).
pub const APO_SLOT_LADDER: [ApoSlot; 8] = [
    ApoSlot::Efx,
    ApoSlot::Mfx,
    ApoSlot::Sfx,
    ApoSlot::EfxSingle,
    ApoSlot::MfxSingle,
    ApoSlot::SfxSingle,
    ApoSlot::Gfx,
    ApoSlot::Lfx,
];

impl ApoSlot {
    /// Exact port of `slot_name` in `/native/system-apo/setup/slot_memory.cpp`.
    pub fn slot_name(self) -> &'static str {
        match self {
            ApoSlot::Efx => "efx",
            ApoSlot::Mfx => "mfx",
            ApoSlot::Sfx => "sfx",
            ApoSlot::EfxSingle => "efx-single",
            ApoSlot::MfxSingle => "mfx-single",
            ApoSlot::SfxSingle => "sfx-single",
            ApoSlot::Gfx => "gfx",
            ApoSlot::Lfx => "lfx",
        }
    }

    /// Exact port of `slot_from_name` in `/native/system-apo/setup/slot_memory.cpp`.
    pub fn from_slot_name(name: &str) -> Option<Self> {
        APO_SLOT_LADDER
            .iter()
            .copied()
            .find(|s| s.slot_name() == name.trim())
    }
}

/// Validates that `guid` is a strict `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` 38-character hex GUID
/// so it is safe to join onto `<engine root>\slots\<guid>.txt` (`is_valid_endpoint_guid`).
pub fn is_valid_endpoint_guid(guid: &str) -> bool {
    let bytes = guid.as_bytes();
    if bytes.len() != 38 || bytes[0] != b'{' || bytes[37] != b'}' {
        return false;
    }
    for (idx, &b) in bytes[1..37].iter().enumerate() {
        if matches!(idx, 8 | 13 | 18 | 23) {
            if b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

/// Parses the contents of `<engine root>\slots\<guid>.txt` (oldest slot first, current slot last).
/// Exact port of `remembered_slots` in `/native/system-apo/setup/slot_memory.cpp`.
pub fn parse_remembered_slots(text: &str) -> Vec<ApoSlot> {
    text.lines()
        .filter_map(|line| ApoSlot::from_slot_name(line.trim()))
        .collect()
}

/// Appends `slot` to an endpoint's slot history unless it is already the last entry,
/// and serializes it as newline-separated slot names (`remember_slot` in `slot_memory.cpp`).
pub fn append_remembered_slot(history: &mut Vec<ApoSlot>, slot: ApoSlot) -> String {
    if history.last().copied() != Some(slot) {
        history.push(slot);
    }
    history
        .iter()
        .map(|s| s.slot_name())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Selects the first rung on `APO_SLOT_LADDER` that has not yet been tried in `history`.
pub fn first_untried_ladder_slot(history: &[ApoSlot]) -> Option<ApoSlot> {
    APO_SLOT_LADDER
        .iter()
        .copied()
        .find(|candidate| !history.contains(candidate))
}

// ============================================================================
// 5. Slot Report, Attach Outcome & Write-Once Backup Store
//    (`/native/system-apo/setup/slot_report.cpp`, `attach_outcome.cpp`, `backup.cpp`)
// ============================================================================

/// Human-readable description of a single effect CLSID sitting in one of the 8 `FxProperties` slots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescribedSlotEffect {
    pub slot: ApoSlot,
    pub clsid: String,
    pub label: String,
    pub is_ours: bool,
    pub is_equalizer_apo: bool,
    pub is_windows_default: bool,
}

/// Exact port of `describe_slots` in `/native/system-apo/setup/slot_report.cpp`:
/// Walks all 8 slots in ladder order (`EFX`, `MFX`, `SFX`, `efx-single`, `mfx-single`, `sfx-single`, `gfx`, `lfx`)
/// and classifies each non-empty CLSID as `FluidEQ Engine`, `Equalizer APO`, `Windows default`, or vendor `other`.
pub fn describe_slots(values: &EndpointFxValues, our_clsid: &str) -> Vec<DescribedSlotEffect> {
    let mut out = Vec::new();
    let mut push_entry = |slot: ApoSlot, clsid: &str| {
        if clsid.is_empty() {
            return;
        }
        let is_ours = equal_clsid_ci(clsid, our_clsid);
        let is_equalizer_apo = !is_ours && is_equalizer_apo_clsid(clsid);
        let is_windows_default =
            !is_ours && !is_equalizer_apo && is_windows_default_apo_clsid(clsid);
        let label = if is_ours {
            "FluidEQ Engine".to_string()
        } else if is_equalizer_apo {
            "Equalizer APO".to_string()
        } else if is_windows_default {
            "Windows default".to_string()
        } else {
            "other".to_string()
        };
        out.push(DescribedSlotEffect {
            slot,
            clsid: clsid.to_string(),
            label,
            is_ours,
            is_equalizer_apo,
            is_windows_default,
        });
    };

    for (slot, idx) in [
        (ApoSlot::Efx, FX_SLOT_EFX),
        (ApoSlot::Mfx, FX_SLOT_MFX),
        (ApoSlot::Sfx, FX_SLOT_SFX),
    ] {
        if let Some(entries) = values.composite[idx].as_ref() {
            for e in entries {
                push_entry(slot, e);
            }
        }
    }

    for (slot, idx) in [
        (ApoSlot::EfxSingle, FX_SLOT_EFX),
        (ApoSlot::MfxSingle, FX_SLOT_MFX),
        (ApoSlot::SfxSingle, FX_SLOT_SFX),
    ] {
        if let Some(s) = values.single[idx].as_deref() {
            push_entry(slot, s);
        }
    }

    if let Some(gfx) = values.legacy[FX_LEGACY_GFX].as_deref() {
        push_entry(ApoSlot::Gfx, gfx);
    }
    if let Some(lfx) = values.legacy[FX_LEGACY_LFX].as_deref() {
        push_entry(ApoSlot::Lfx, lfx);
    }

    out
}

/// Exact port of `held_slots` in `/native/system-apo/setup/slot_report.cpp`:
/// Returns the comma-separated list of slot names currently holding `clsid` (e.g., `"efx"` or `"mfx, efx"`).
pub fn held_slots(values: &EndpointFxValues, clsid: &str) -> String {
    let mut names = Vec::new();
    for (slot, idx) in [
        (ApoSlot::Sfx, FX_SLOT_SFX),
        (ApoSlot::Mfx, FX_SLOT_MFX),
        (ApoSlot::Efx, FX_SLOT_EFX),
    ] {
        if let Some(entries) = values.composite[idx].as_ref() {
            if entries.iter().any(|e| equal_clsid_ci(e, clsid)) {
                names.push(slot.slot_name());
            }
        }
    }
    for (slot, idx) in [
        (ApoSlot::SfxSingle, FX_SLOT_SFX),
        (ApoSlot::MfxSingle, FX_SLOT_MFX),
        (ApoSlot::EfxSingle, FX_SLOT_EFX),
    ] {
        if values.single[idx]
            .as_deref()
            .map(|e| equal_clsid_ci(e, clsid))
            .unwrap_or(false)
        {
            names.push(slot.slot_name());
        }
    }
    if values.legacy[FX_LEGACY_LFX]
        .as_deref()
        .map(|e| equal_clsid_ci(e, clsid))
        .unwrap_or(false)
    {
        names.push(ApoSlot::Lfx.slot_name());
    }
    if values.legacy[FX_LEGACY_GFX]
        .as_deref()
        .map(|e| equal_clsid_ci(e, clsid))
        .unwrap_or(false)
    {
        names.push(ApoSlot::Gfx.slot_name());
    }
    names.join(", ")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointAttachResult {
    pub guid: String,
    pub ok: bool,
    pub changed: bool,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachAllOutcome {
    pub attached_count: usize,
    pub changed_count: usize,
    pub first_error: String,
}

/// Exact port of `summarise_attach_all` in `/native/system-apo/setup/attach_outcome.cpp`.
pub fn summarise_attach_all(results: &[EndpointAttachResult]) -> AttachAllOutcome {
    let mut out = AttachAllOutcome {
        attached_count: 0,
        changed_count: 0,
        first_error: String::new(),
    };
    for r in results {
        if r.ok {
            out.attached_count += 1;
            if r.changed {
                out.changed_count += 1;
            }
        } else if out.first_error.is_empty() && !r.error.is_empty() {
            out.first_error = r.error.clone();
        }
    }
    out
}

/// Write-once endpoint backup ledger (`<engine root>\backups\<guid>.json` and `<engine root>\apo_off\<guid>.json`).
/// Exact port of `save_backup_once` and `save_apo_off_once` in `/native/system-apo/setup/backup.cpp`:
/// - `save_backup_once` records the vendor `FxProperties` snapshot ONLY if no backup exists yet for `guid`,
///   so attaching a second time never clobbers the true pre-FluidEQ vendor state.
/// - `save_apo_off_once` records the pre-strip Equalizer APO state ONLY if `apo_off` does not exist yet for `guid`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointBackupStore {
    backups: BTreeMap<String, EndpointFxValues>,
    apo_off: BTreeMap<String, EndpointFxValues>,
}

impl EndpointBackupStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Saves `values` as the original vendor backup for `guid` only if no backup is stored yet.
    /// Returns `Ok(true)` if newly written, `Ok(false)` if preserved existing backup, or `Err` on invalid GUID.
    pub fn save_backup_once(
        &mut self,
        guid: &str,
        values: &EndpointFxValues,
    ) -> Result<bool, String> {
        if !is_valid_endpoint_guid(guid) {
            return Err(format!("invalid endpoint GUID: {}", guid));
        }
        let key = guid.to_ascii_uppercase();
        if self.backups.contains_key(&key) {
            return Ok(false);
        }
        self.backups.insert(key, values.clone());
        Ok(true)
    }

    pub fn load_backup(&self, guid: &str) -> Option<&EndpointFxValues> {
        self.backups.get(&guid.to_ascii_uppercase())
    }

    /// Saves `values` before stripping Equalizer APO only if no `apo_off` snapshot exists yet for `guid`.
    pub fn save_apo_off_once(
        &mut self,
        guid: &str,
        values: &EndpointFxValues,
    ) -> Result<bool, String> {
        if !is_valid_endpoint_guid(guid) {
            return Err(format!("invalid endpoint GUID: {}", guid));
        }
        let key = guid.to_ascii_uppercase();
        if self.apo_off.contains_key(&key) {
            return Ok(false);
        }
        self.apo_off.insert(key, values.clone());
        Ok(true)
    }

    pub fn take_apo_off(&mut self, guid: &str) -> Option<EndpointFxValues> {
        self.apo_off.remove(&guid.to_ascii_uppercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_nested_apo_include_tree_and_device_guards() {
        let mut files = BTreeMap::new();
        files.insert(
            "config.txt".to_string(),
            "Preamp: -2.5 dB\nInclude: fluideq.txt\n".to_string(),
        );
        files.insert(
            "fluideq.txt".to_string(),
            "# FluidEQLowLatency: ON\nDevice: Headphones (USB DAC); {11111111-2222-3333-4444-555555555555}\nChannel: L\nFilter 1: ON PK Fc 1000 Hz Gain 3.5 dB Q 1.41\nChannel: all\nGraphicEQ: 20 -1.0; 1000 2.0; 20000 0.0\nDevice: Speakers Only\nFilter 2: ON PK Fc 500 Hz Gain -6.0 dB Q 2.0\n".to_string(),
        );

        let resolved = resolve_apo_endpoint_chain(
            "config.txt",
            "{11111111-2222-3333-4444-555555555555}",
            "Headphones (USB DAC)",
            &files,
        );
        assert!(resolved.matched);
        assert!(resolved.low_latency);
        assert_eq!(resolved.preamp_db, -2.5);
        assert_eq!(resolved.bands.len(), 1);
        assert_eq!(resolved.bands[0].ear, 1);
        assert_eq!(resolved.bands[0].gain_db, 3.5);
        assert_eq!(resolved.graphic_curves.len(), 1);
        assert_eq!(resolved.graphic_curves[0].len(), 3);
    }

    #[test]
    fn fx_list_plan_attach_mirrors_vendor_singles_and_detaches_cleanly() {
        let mut vendor = EndpointFxValues::default();
        vendor.single[FX_SLOT_SFX] = Some("{AA000000-0000-0000-0000-000000000001}".to_string());

        let attached = plan_attach(&vendor, FLUID_ENGINE_APO_CLSID, ApoSlot::Efx);
        assert!(attached.changed);
        assert!(attached.refused.is_none());
        // Vendor's SFX single was mirrored into composite[SFX] so vendor effect stays alive
        assert_eq!(
            attached.after.composite[FX_SLOT_SFX],
            Some(vec!["{AA000000-0000-0000-0000-000000000001}".to_string()])
        );
        assert_eq!(
            attached.after.composite[FX_SLOT_EFX],
            Some(vec![FLUID_ENGINE_APO_CLSID.to_string()])
        );

        let detached = plan_detach(&attached.after, &vendor, FLUID_ENGINE_APO_CLSID);
        assert!(detached.changed);
        assert!(detached.after.composite[FX_SLOT_EFX].is_none());
    }

    #[test]
    fn bluetooth_single_only_endpoint_selects_single_slot_over_windows_default_apo() {
        let mut bt = EndpointFxValues::default();
        bt.single[FX_SLOT_SFX] = Some(WINDOWS_DEFAULT_APO_CLSIDS[0].to_string());
        bt.single[FX_SLOT_MFX] = Some(WINDOWS_DEFAULT_APO_CLSIDS[1].to_string());

        let slot = default_slot_for(&bt, false);
        assert_eq!(slot, ApoSlot::EfxSingle);
        let plan = plan_attach(&bt, FLUID_ENGINE_APO_CLSID, ApoSlot::MfxSingle);
        assert!(plan.changed);
        assert!(plan.refused.is_none());
        assert_eq!(
            plan.after.single[FX_SLOT_MFX].as_deref(),
            Some(FLUID_ENGINE_APO_CLSID)
        );
    }

    #[test]
    fn every_windows_default_effect_is_takeable_in_both_generations() {
        // Windows ships four inbox effects under two names, and which pair an
        // endpoint carries varies by machine. Every one of them, in the mixed
        // case the audio stack actually writes, is ours to replace; a vendor's
        // class id in the same value is not.
        let vendor = "{5C8DC6DB-1A99-46FE-90E8-8229A41DD3EF}";
        for &windows in &WINDOWS_DEFAULT_APO_CLSIDS {
            for spelling in [
                windows.to_string(),
                windows.to_ascii_lowercase(),
                windows.to_ascii_uppercase(),
            ] {
                assert!(is_windows_default_apo_clsid(&spelling));

                let mut single = EndpointFxValues::default();
                single.single[FX_SLOT_EFX] = Some(spelling.clone());
                assert!(is_single_only(&single));
                assert_eq!(default_slot_for(&single, false), ApoSlot::EfxSingle);
                let plan = plan_attach(&single, FLUID_ENGINE_APO_CLSID, ApoSlot::EfxSingle);
                assert!(plan.refused.is_none(), "{spelling}: {:?}", plan.refused);

                let mut legacy = EndpointFxValues::default();
                legacy.legacy[FX_LEGACY_GFX] = Some(spelling.clone());
                assert!(is_legacy_only(&legacy));
                assert_eq!(default_slot_for(&legacy, false), ApoSlot::Gfx);
                let plan = plan_attach(&legacy, FLUID_ENGINE_APO_CLSID, ApoSlot::Gfx);
                assert!(plan.refused.is_none(), "{spelling}: {:?}", plan.refused);
            }
        }

        // The positive control: the same shape with somebody else's effect in
        // it is still refused, so this test cannot pass by taking everything.
        let mut vendor_value = EndpointFxValues::default();
        vendor_value.single[FX_SLOT_EFX] = Some(vendor.to_string());
        assert!(!is_windows_default_apo_clsid(vendor));
        assert!(!plan_attach(&vendor_value, FLUID_ENGINE_APO_CLSID, ApoSlot::EfxSingle)
            .refused
            .is_none());

        let mut vendor_legacy = EndpointFxValues::default();
        vendor_legacy.legacy[FX_LEGACY_GFX] = Some(vendor.to_string());
        assert!(!plan_attach(&vendor_legacy, FLUID_ENGINE_APO_CLSID, ApoSlot::Gfx)
            .refused
            .is_none());
    }

    #[test]
    fn slot_matrix_falls_back_to_composite_and_never_takes_a_vendor_value() {
        // A vendor's composite lists with Equalizer APO in the value Windows
        // actually creates: the endpoint value is ours to take because EAPO is
        // another EQ and the detach backup puts it back.
        let mut composite = EndpointFxValues::default();
        composite.single[FX_SLOT_EFX] = Some(EQUALIZER_APO_CLSIDS[0].to_string());
        composite.composite[FX_SLOT_SFX] =
            Some(vec!["{DA2C9ECE-7418-4906-B4FA-0A00B3EB88AA}".to_string()]);
        composite.composite[FX_SLOT_EFX] =
            Some(vec!["{A29EB043-6CE2-4ee2-B38C-F58719E0D88F}".to_string()]);
        assert_eq!(default_slot_for(&composite, false), ApoSlot::EfxSingle);

        // The same composite endpoint with Windows' own effect in the value.
        let mut windows_value = composite.clone();
        windows_value.single[FX_SLOT_EFX] = Some(WINDOWS_DEFAULT_APO_CLSIDS[3].to_string());
        assert_eq!(default_slot_for(&windows_value, false), ApoSlot::EfxSingle);

        // Composite endpoint with a vendor in the value: the value is not ours
        // to take, so the ladder falls back to the generic composite rungs.
        let mut vendor_value = composite.clone();
        vendor_value.single[FX_SLOT_EFX] = Some("{5C8DC6DB-1A99-46FE-90E8-8229A41DD3EF}".to_string());
        assert_eq!(default_slot_for(&vendor_value, false), ApoSlot::Efx);
        assert_eq!(default_slot_for(&vendor_value, true), ApoSlot::Mfx);

        // Every one-value rung held by a vendor and no composite list at all:
        // nothing may be written there, so the ladder's own top rung stands.
        let mut full = EndpointFxValues::default();
        for at in 0..FX_SLOT_COUNT {
            full.single[at] = Some("{62DC1A93-CE3E-4B2C-9B3B-9F1B0E2A0001}".to_string());
        }
        assert!(is_single_only(&full));
        assert_eq!(default_slot_for(&full, false), ApoSlot::Efx);

        // Bare endpoint: the newest rung, or MFX on a combined endpoint.
        let bare = EndpointFxValues::default();
        assert!(!is_legacy_only(&bare));
        assert_eq!(default_slot_for(&bare, false), ApoSlot::Efx);
        assert_eq!(default_slot_for(&bare, true), ApoSlot::Mfx);
    }

    #[test]
    fn vendor_composite_with_equalizer_apo_in_efx_takes_the_endpoint_value() {
        let mut values = EndpointFxValues::default();
        // Realtek-shaped: vendor APOs in the lists, Equalizer APO in the
        // values Windows actually creates, and a free MFX value it never
        // creates for a shared-mode stream.
        values.single[FX_SLOT_SFX] = Some(EQUALIZER_APO_CLSIDS[0].to_string());
        values.single[FX_SLOT_EFX] = Some(EQUALIZER_APO_CLSIDS[1].to_string());
        values.composite[FX_SLOT_SFX] =
            Some(vec!["{DA2C9ECE-7418-4906-B4FA-0A00B3EB88AA}".to_string()]);
        values.composite[FX_SLOT_MFX] =
            Some(vec!["{A296D363-EE83-4af9-9BE7-729C1296150A}".to_string()]);
        values.composite[FX_SLOT_EFX] =
            Some(vec!["{A29EB043-6CE2-4ee2-B38C-F58719E0D88F}".to_string()]);

        assert_eq!(default_slot_for(&values, false), ApoSlot::EfxSingle);
        let plan = plan_attach(&values, FLUID_ENGINE_APO_CLSID, ApoSlot::EfxSingle);
        assert!(plan.refused.is_none(), "{:?}", plan.refused);
        assert_eq!(
            plan.after.single[FX_SLOT_EFX].as_deref(),
            Some(FLUID_ENGINE_APO_CLSID)
        );
        let detached = plan_detach(&plan.after, &values, FLUID_ENGINE_APO_CLSID);
        assert_eq!(
            detached.after.single[FX_SLOT_EFX].as_deref(),
            Some(EQUALIZER_APO_CLSIDS[1])
        );
    }

    #[test]
    fn fx_modes_copies_non_raw_sibling_modes_and_slot_memory_tracks_ladder() {
        let mut values = EndpointFxValues::default();
        let movie_mode = "{B26FEB0D-EC94-477C-9494-D1AB8E753F6E}";
        values.single[FX_SLOT_MFX] = Some(FLUID_ENGINE_APO_CLSID.to_string());
        values.modes[FX_SLOT_SFX] = Some(vec![
            DEFAULT_PROCESSING_MODE_CLSID.to_string(),
            movie_mode.to_string(),
            RAW_PROCESSING_MODE_CLSID.to_string(),
        ]);

        assert!(mode_missing(&values, FLUID_ENGINE_APO_CLSID));
        let fixed = plan_mode_in_place(&values, FLUID_ENGINE_APO_CLSID);
        assert!(fixed.changed);
        assert_eq!(
            fixed.after.modes[FX_SLOT_MFX],
            Some(vec![
                DEFAULT_PROCESSING_MODE_CLSID.to_string(),
                movie_mode.to_string()
            ])
        );

        assert!(is_valid_endpoint_guid(
            "{11111111-2222-3333-4444-555555555555}"
        ));
        let mut history = parse_remembered_slots("efx\nmfx\n");
        assert_eq!(history, vec![ApoSlot::Efx, ApoSlot::Mfx]);
        assert_eq!(first_untried_ladder_slot(&history), Some(ApoSlot::Sfx));
        let serialized = append_remembered_slot(&mut history, ApoSlot::Sfx);
        assert_eq!(serialized, "efx\nmfx\nsfx");
    }

    #[test]
    fn disable_restore_equalizer_apo_move_slot_and_write_once_backup() {
        let guid = "{11111111-2222-3333-4444-555555555555}";
        let vendor_clsid = "{AA000000-0000-0000-0000-000000000001}";
        let eq_apo_clsid = EQUALIZER_APO_CLSIDS[0];

        let mut initial = EndpointFxValues::default();
        initial.composite[FX_SLOT_EFX] = Some(vec![
            vendor_clsid.to_string(),
            eq_apo_clsid.to_string(),
            FLUID_ENGINE_APO_CLSID.to_string(),
        ]);

        let mut store = EndpointBackupStore::new();
        assert_eq!(store.save_backup_once(guid, &initial), Ok(true));
        // Second call must NOT overwrite the original vendor backup
        let modified = EndpointFxValues::default();
        assert_eq!(store.save_backup_once(guid, &modified), Ok(false));
        assert_eq!(store.load_backup(guid), Some(&initial));

        // Strip Equalizer APO while preserving vendor + FluidEQ
        assert_eq!(store.save_apo_off_once(guid, &initial), Ok(true));
        let disabled = plan_disable_apo(&initial);
        assert!(disabled.changed);
        assert_eq!(
            disabled.after.composite[FX_SLOT_EFX],
            Some(vec![
                vendor_clsid.to_string(),
                FLUID_ENGINE_APO_CLSID.to_string()
            ])
        );

        // Restore Equalizer APO in its original relative position
        let saved_apo = store.take_apo_off(guid).unwrap();
        let restored = plan_restore_apo(&disabled.after, &saved_apo);
        assert!(restored.changed);
        assert_eq!(restored.after.composite[FX_SLOT_EFX], initial.composite[FX_SLOT_EFX]);

        // Move FluidEQ from EFX to MFX and verify held_slots & describe_slots
        let moved = plan_move(&disabled.after, &disabled.after, FLUID_ENGINE_APO_CLSID, ApoSlot::Mfx);
        assert!(moved.changed);
        assert_eq!(held_slots(&moved.after, FLUID_ENGINE_APO_CLSID), "mfx");
        let report = describe_slots(&moved.after, FLUID_ENGINE_APO_CLSID);
        assert!(report.iter().any(|r| r.slot == ApoSlot::Mfx && r.is_ours));
    }
}
