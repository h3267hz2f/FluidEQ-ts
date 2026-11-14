/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! One output's whole chain as a single file somebody can send to somebody
//! else: the curve, the voicing, and every rack stage including Room.
//!
//! The file carries the profile, not the generated APO text and not the
//! convolution kernel. Those are rebuilt for whichever output it lands on;
//! the impulse response is a machine file and stays behind, exactly as it
//! does in a saved preset.
//!
//! An import arrives from anywhere, so it is refused whole rather than
//! applied in part: half a chain reaching the engine is worse than none.

use crate::biquad::EqBandSpec;
use crate::{AppStateSnapshot, ToneSettings};
use fluideq_dsp_sys::{FeqChainSettings, FEQ_CHAIN_MAX_EQ_BANDS};
use serde::{Deserialize, Serialize};

pub const CHAIN_BUNDLE_VERSION: u32 = 1;
pub const CHAIN_BUNDLE_APP: &str = "FluidEQ";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainBundle {
    pub app: String,
    pub version: u32,
    #[serde(default)]
    pub exported_from: Option<String>,
    #[serde(default)]
    pub exported_at_unix: Option<u64>,
    #[serde(default)]
    pub preset: String,
    pub auto_headroom: bool,
    pub preamp_db: f64,
    pub eq_model: String,
    pub eq_model_amount: f64,
    pub matched_treble: bool,
    #[serde(default)]
    pub tone: ToneSettings,
    pub bands: Vec<EqBandSpec>,
    pub chain_settings: FeqChainSettings,
}

fn known_eq_model(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "clean"
            | "proportional"
            | "wide"
            | "asymmetric"
            | "feq_eq_model_clean"
            | "feq_eq_model_proportional"
            | "feq_eq_model_wide"
            | "feq_eq_model_asymmetric"
    )
}

/// The bundle for the snapshot in hand. `exported_from` is for the person
/// reading the file; where it is imported is chosen at import time.
pub fn export_bundle(
    snapshot: &AppStateSnapshot,
    exported_from: Option<&str>,
    exported_at_unix: u64,
) -> Result<String, String> {
    let bundle = ChainBundle {
        app: CHAIN_BUNDLE_APP.to_string(),
        version: CHAIN_BUNDLE_VERSION,
        exported_from: exported_from.map(str::to_string),
        exported_at_unix: Some(exported_at_unix),
        preset: snapshot.active_preset_name.clone(),
        auto_headroom: snapshot.auto_headroom,
        preamp_db: snapshot.preamp_db,
        eq_model: snapshot.eq_model.clone(),
        eq_model_amount: snapshot.eq_model_amount,
        matched_treble: snapshot.matched_treble,
        tone: snapshot.tone.clone(),
        bands: snapshot.bands.clone(),
        chain_settings: snapshot.chain_settings,
    };
    serde_json::to_string_pretty(&bundle)
        .map_err(|error| format!("could not write the chain bundle: {error}"))
}

/// Parse a bundle from whatever was on disk, refusing anything that is not
/// one rather than mending it into something the sender never had.
pub fn parse_bundle(text: &str) -> Result<ChainBundle, String> {
    let bundle: ChainBundle = serde_json::from_str(text)
        .map_err(|error| format!("that is not a FluidEQ chain bundle: {error}"))?;
    if bundle.app != CHAIN_BUNDLE_APP {
        return Err(format!(
            "that file belongs to {}, not FluidEQ",
            bundle.app.trim()
        ));
    }
    if bundle.version != CHAIN_BUNDLE_VERSION {
        return Err(format!(
            "this bundle is version {}, and this FluidEQ reads version {CHAIN_BUNDLE_VERSION}",
            bundle.version
        ));
    }
    if bundle.bands.len() > FEQ_CHAIN_MAX_EQ_BANDS {
        return Err(format!(
            "the bundle carries {} bands, more than the chain's {FEQ_CHAIN_MAX_EQ_BANDS}",
            bundle.bands.len()
        ));
    }
    if !known_eq_model(&bundle.eq_model) {
        return Err(format!(
            "the bundle names an EQ model this build does not know: \"{}\"",
            bundle.eq_model
        ));
    }
    Ok(bundle)
}

/// A file name for the bundle that is the preset's name where it can be, and
/// a fixed one where the name has characters a file system would refuse.
pub fn export_file_name(preset: &str) -> String {
    let cleaned: String = preset
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim_matches(['.', ' ']).trim();
    let stem = if cleaned.is_empty() {
        "fluideq-chain"
    } else {
        cleaned
    };
    format!("{stem}.fluideq")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> AppStateSnapshot {
        crate::AppStateStore::new(48_000.0).snapshot()
    }

    #[test]
    fn a_bundle_round_trips_the_whole_chain() {
        let mut source = snapshot();
        source.active_preset_name = "Desk".into();
        source.preamp_db = -4.5;
        source.eq_model = "wide".into();
        source.eq_model_amount = 0.4;
        source.tone.bass_db = 3.0;
        source.chain_settings.room.size_m = 7.5;
        source.chain_settings.compander.attack_ms = 12.0;

        let text = export_bundle(&source, Some("Speakers"), 1_700_000_000).expect("export");
        let parsed = parse_bundle(&text).expect("parse");
        assert_eq!(parsed.preset, "Desk");
        assert_eq!(parsed.exported_from.as_deref(), Some("Speakers"));
        assert_eq!(parsed.preamp_db, -4.5);
        assert_eq!(parsed.chain_settings.room.size_m, 7.5);
        assert_eq!(parsed.chain_settings.compander.attack_ms, 12.0);
        assert_eq!(parsed.tone.bass_db, 3.0);
    }

    #[test]
    fn something_that_is_not_a_bundle_is_refused_whole() {
        assert!(parse_bundle("not json").is_err());
        assert!(parse_bundle("{\"app\":\"OtherEQ\",\"version\":1}").is_err());
        let mut value: serde_json::Value =
            serde_json::from_str(&export_bundle(&snapshot(), None, 0).unwrap()).unwrap();
        value["version"] = serde_json::json!(2);
        assert!(parse_bundle(&value.to_string()).is_err());
        value["version"] = serde_json::json!(1);
        value["eq_model"] = serde_json::json!("vinyl");
        assert!(parse_bundle(&value.to_string()).is_err());
    }

    #[test]
    fn a_file_name_is_the_preset_where_it_can_be() {
        assert_eq!(export_file_name("Desk"), "Desk.fluideq");
        assert_eq!(export_file_name("A/B: C?"), "A-B- C-.fluideq");
        assert_eq!(export_file_name("   "), "fluideq-chain.fluideq");
    }
}
