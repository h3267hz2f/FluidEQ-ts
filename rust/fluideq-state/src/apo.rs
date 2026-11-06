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
    lines.push(format!("Preamp: {:+.1} dB", preamp_db));
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
                "Filter {}: {} {} Fc {:.1} Hz Gain {:+.1} dB Q {:.2}",
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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ApoGraphicPoint {
    pub frequency: f64,
    pub gain_db: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApoChannelSelection {
    pub all: bool,
    pub speakers_mask: u32,
    pub indices_mask: u32,
}

impl Default for ApoChannelSelection {
    fn default() -> Self {
        Self {
            all: true,
            speakers_mask: 0,
            indices_mask: 0,
        }
    }
}

/// Parses an Equalizer APO `Channel:` body (`all`, `L`, `R`, `C`, `LFE`/`SUB`, `SL`, `SR`, `RL`, `RR`, or 1-based channel indices).
pub fn parse_apo_channel_selection(body: &str) -> ApoChannelSelection {
    let mut sel = ApoChannelSelection {
        all: false,
        speakers_mask: 0,
        indices_mask: 0,
    };
    for raw_token in body.split_whitespace() {
        let token = raw_token.trim_matches(',').to_ascii_uppercase();
        if token.is_empty() {
            continue;
        }
        match token.as_str() {
            "ALL" => return ApoChannelSelection::default(),
            "L" | "FL" => sel.speakers_mask |= 1 << 0,
            "R" | "FR" => sel.speakers_mask |= 1 << 1,
            "C" => sel.speakers_mask |= 1 << 2,
            "SL" => sel.speakers_mask |= 1 << 3,
            "SR" => sel.speakers_mask |= 1 << 4,
            "RL" | "BL" => sel.speakers_mask |= 1 << 5,
            "RR" | "BR" => sel.speakers_mask |= 1 << 6,
            "LFE" | "SUB" => sel.speakers_mask |= 1 << 7,
            _ => {
                if let Ok(ch_num) = token.parse::<u32>() {
                    if (1..=32).contains(&ch_num) {
                        sel.indices_mask |= 1 << (ch_num - 1);
                    }
                }
            }
        }
    }
    if sel.speakers_mask == 0 && sel.indices_mask == 0 {
        sel.all = true;
    }
    sel
}

/// Converts bandwidth in octaves (`BW Oct <o>`) to Q factor (`1 / (2 * sinh(ln(2)/2 * bw))`).
fn bandwidth_octaves_to_q(bw_oct: f64) -> Option<f64> {
    if !(bw_oct > 0.0) || !bw_oct.is_finite() {
        return None;
    }
    let arg = 0.5 * std::f64::consts::LN_2 * bw_oct;
    let denom = 2.0 * arg.sinh();
    if denom > 1e-9 {
        Some(1.0 / denom)
    } else {
        None
    }
}

/// Parses an Equalizer APO `Filter:` body:
/// `ON <type> Fc <f> Hz [Gain <g> dB] (Q <q> | BW Oct <o>)`.
pub fn parse_apo_filter_line(body: &str, id: u32, ear: u8) -> Option<EqBandSpec> {
    let tokens: Vec<&str> = body.split_whitespace().collect();
    if tokens.len() < 4 || !tokens[0].eq_ignore_ascii_case("ON") {
        return None;
    }
    let filter_type = match tokens[1].to_ascii_uppercase().as_str() {
        "PK" | "PEQ" | "MODAL" => BandFilterKind::PK,
        "NO" => BandFilterKind::NO,
        "LSC" | "LS" | "LSQ" => BandFilterKind::LSC,
        "HSC" | "HS" | "HSQ" => BandFilterKind::HSC,
        "LPQ" | "LP" => BandFilterKind::LPQ,
        "HPQ" | "HP" => BandFilterKind::HPQ,
        "BP" => BandFilterKind::BP,
        _ => return None,
    };

    let mut idx = 2;
    let mut frequency: Option<f64> = None;
    let mut gain_db: f64 = 0.0;
    let mut quality: f64 = 0.7071;

    while idx < tokens.len() {
        let key = tokens[idx].to_ascii_uppercase();
        match key.as_str() {
            "FC" => {
                let val_str = tokens.get(idx + 1)?.replace(',', ".");
                frequency = val_str.parse::<f64>().ok();
                idx += 2;
                if idx < tokens.len() && tokens[idx].eq_ignore_ascii_case("HZ") {
                    idx += 1;
                }
            }
            "GAIN" => {
                let val_str = tokens.get(idx + 1)?.replace(',', ".");
                gain_db = val_str.parse::<f64>().ok()?;
                idx += 2;
                if idx < tokens.len() && tokens[idx].eq_ignore_ascii_case("DB") {
                    idx += 1;
                }
            }
            "Q" => {
                let val_str = tokens.get(idx + 1)?.replace(',', ".");
                quality = val_str.parse::<f64>().ok()?;
                idx += 2;
            }
            "BW" => {
                if idx + 2 < tokens.len() && tokens[idx + 1].eq_ignore_ascii_case("OCT") {
                    let bw_val = tokens[idx + 2].replace(',', ".").parse::<f64>().ok()?;
                    quality = bandwidth_octaves_to_q(bw_val)?;
                    idx += 3;
                } else {
                    return None;
                }
            }
            _ => idx += 1,
        }
    }

    let freq = frequency?;
    if !(freq > 0.0 && freq.is_finite() && quality > 0.0 && quality.is_finite() && gain_db.is_finite()) {
        return None;
    }

    Some(EqBandSpec {
        id,
        enabled: true,
        filter_type,
        frequency: freq,
        gain_db,
        quality,
        dynamic: false,
        threshold_db: -18.0,
        ear,
    })
}

/// Parses an Equalizer APO `GraphicEQ:` body (`<freq> <gain>; <freq> <gain>; ...`) all-or-nothing.
pub fn parse_apo_graphic_eq(body: &str) -> Vec<ApoGraphicPoint> {
    let mut points = Vec::new();
    for segment in body.split(';') {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let Some(f_str) = parts.next() else {
            return Vec::new();
        };
        let Some(g_str) = parts.next() else {
            return Vec::new();
        };
        if parts.next().is_some() {
            return Vec::new();
        }
        let (Ok(freq), Ok(gain)) = (f_str.parse::<f64>(), g_str.parse::<f64>()) else {
            return Vec::new();
        };
        if !(freq > 0.0 && freq.is_finite() && gain.is_finite()) {
            return Vec::new();
        }
        points.push(ApoGraphicPoint {
            frequency: freq,
            gain_db: gain,
        });
    }
    points
}

/// Checks whether an Equalizer APO `Device:` pattern matches the given endpoint `(guid, friendly_name)`.
pub fn device_matches_endpoint(pattern: &str, endpoint_guid: &str, friendly_name: &str) -> bool {
    let pat = pattern.trim();
    if pat.is_empty() || pat.eq_ignore_ascii_case("all") {
        return true;
    }
    let pat_lower = pat.to_ascii_lowercase();
    let guid_lower = endpoint_guid.trim().to_ascii_lowercase();
    let name_lower = friendly_name.trim().to_ascii_lowercase();

    for clause in pat_lower.split(';') {
        let token = clause.trim();
        if token.is_empty() {
            continue;
        }
        if token == guid_lower || name_lower.contains(token) || token.contains(&guid_lower) {
            return true;
        }
    }
    false
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedApoChain {
    pub preamp_db: f64,
    pub low_latency: bool,
    pub bands: Vec<EqBandSpec>,
    pub graphic_curves: Vec<Vec<ApoGraphicPoint>>,
    pub files_read: Vec<String>,
    pub matched: bool,
}

/// Resolves an Equalizer APO configuration tree starting at `root_file` (capped at 8 nested `Include:` frames),
/// matching `fluideq_engine::resolve_chain` in `/native/system-apo/include/fluideq_engine/config.h`.
pub fn resolve_apo_endpoint_chain(
    root_file: &str,
    endpoint_guid: &str,
    friendly_name: &str,
    files: &BTreeMap<String, String>,
) -> ResolvedApoChain {
    let mut chain = ResolvedApoChain::default();
    let mut stack = Vec::with_capacity(APO_MAX_INCLUDE_DEPTH);
    resolve_file_recursive(
        root_file,
        endpoint_guid,
        friendly_name,
        files,
        &mut stack,
        &mut chain,
    );
    chain
}

fn resolve_file_recursive(
    file_name: &str,
    endpoint_guid: &str,
    friendly_name: &str,
    files: &BTreeMap<String, String>,
    open_stack: &mut Vec<String>,
    chain: &mut ResolvedApoChain,
) {
    if open_stack.len() >= APO_MAX_INCLUDE_DEPTH || open_stack.iter().any(|f| f == file_name) {
        return;
    }
    let Some(content) = files.get(file_name) else {
        return;
    };
    open_stack.push(file_name.to_string());
    chain.files_read.push(file_name.to_string());

    let mut device_active = true;
    let mut current_ear: u8 = 0;

    for raw_line in content.lines() {
        let trimmed = raw_line.trim().trim_start_matches('\u{feff}');
        if trimmed.is_empty() {
            continue;
        }
        if let Some(comment) = trimmed.strip_prefix('#') {
            if device_active
                && comment
                    .trim()
                    .eq_ignore_ascii_case("FluidEQLowLatency: ON")
            {
                chain.low_latency = true;
            }
            continue;
        }
        let Some((cmd_raw, body_raw)) = trimmed.split_once(':') else {
            continue;
        };
        let cmd = cmd_raw.trim();
        let body = body_raw.trim();

        if cmd.eq_ignore_ascii_case("Device") {
            device_active = device_matches_endpoint(body, endpoint_guid, friendly_name);
            continue;
        }
        if !device_active {
            continue;
        }

        if cmd.eq_ignore_ascii_case("Include") {
            resolve_file_recursive(body, endpoint_guid, friendly_name, files, open_stack, chain);
        } else if cmd.eq_ignore_ascii_case("Preamp") {
            let num_str = body
                .split_whitespace()
                .next()
                .unwrap_or("0")
                .replace(',', ".");
            if let Ok(db) = num_str.parse::<f64>() {
                if db.is_finite() {
                    chain.preamp_db += db;
                    chain.matched = true;
                }
            }
        } else if cmd.eq_ignore_ascii_case("Channel") {
            let sel = parse_apo_channel_selection(body);
            current_ear = if sel.all {
                0
            } else if sel.speakers_mask == (1 << 0) {
                1
            } else if sel.speakers_mask == (1 << 1) {
                2
            } else {
                0
            };
        } else if cmd.eq_ignore_ascii_case("GraphicEQ") {
            let pts = parse_apo_graphic_eq(body);
            if !pts.is_empty() {
                chain.graphic_curves.push(pts);
                chain.matched = true;
            }
        } else if cmd
            .get(0..6)
            .map(|p| p.eq_ignore_ascii_case("Filter"))
            .unwrap_or(false)
            && chain.bands.len() < APO_MAX_CHAIN_BANDS
        {
            let next_id = (chain.bands.len() + 1) as u32;
            if let Some(band) = parse_apo_filter_line(body, next_id, current_ear) {
                chain.bands.push(band);
                chain.matched = true;
            }
        }
    }

    open_stack.pop();
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
        if !holds_effect(gfx) || gfx.as_deref().map(is_windows_default_apo_clsid).unwrap_or(false)
        {
            return ApoSlot::Gfx;
        }
    }
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
