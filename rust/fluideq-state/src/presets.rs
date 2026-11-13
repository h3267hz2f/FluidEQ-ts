/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! User EQ presets: the whole curve, its headroom and its voicing, in one
//! named file under the app's data directory.
//!
//! A preset is deliberately not a snapshot: the engine's load state and the
//! live meters are transient and must never come back with a curve the user
//! saved. Built-in presets are read-only data in this module and are never
//! written to disk, so an app update can change them and the user's own
//! files stay untouched.

use crate::biquad::{BandFilterKind, EqBandSpec};
use crate::preset_catalog::{EqPresetGroup, PresetSpec, PRESET_CATALOG};
use crate::ToneSettings;
use fluideq_dsp_sys::FeqChainSettings;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const ISO_FREQUENCIES: [f64; 15] = [
    32.0, 50.0, 80.0, 125.0, 200.0, 315.0, 500.0, 800.0, 1250.0, 2000.0, 3150.0, 5000.0, 8000.0,
    12500.0, 16000.0,
];

pub const MAX_PRESET_NAME_CHARS: usize = 64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqPreset {
    pub name: String,
    /// The picker heading a factory curve files under. A preset the user
    /// saved is `custom`, which is how the picker tells the two apart.
    #[serde(default = "default_group")]
    pub group: String,
    #[serde(default)]
    pub preamp_db: f64,
    #[serde(default = "default_auto_headroom")]
    pub auto_headroom: bool,
    #[serde(default = "default_eq_model")]
    pub eq_model: String,
    #[serde(default = "default_eq_model_amount")]
    pub eq_model_amount: f64,
    #[serde(default = "default_matched_treble")]
    pub matched_treble: bool,
    #[serde(default)]
    pub subsonic_hz: f64,
    #[serde(default)]
    pub mono_below_hz: f64,
    #[serde(default)]
    pub fuzz_amount: f64,
    #[serde(default)]
    pub tone: ToneSettings,
    pub bands: Vec<EqBandSpec>,
    /// The rack, Room and chain switches as they were when the preset was
    /// saved. Absent from files written before presets carried the chain, so
    /// an old preset still applies its curve without resetting the machine.
    #[serde(default)]
    pub chain_settings: Option<FeqChainSettings>,
}

fn default_group() -> String {
    "custom".to_string()
}

fn default_auto_headroom() -> bool {
    true
}

fn default_eq_model() -> String {
    "clean".to_string()
}

fn default_eq_model_amount() -> f64 {
    1.0
}

fn default_matched_treble() -> bool {
    true
}

impl EqPreset {
    /// The curve the app starts with: flat, with the 15 ISO handles in place.
    pub fn flat() -> Self {
        Self::from_gains("Flat", &[0.0; 15])
    }

    /// The everyday curve the store starts on and the picker marks as first.
    pub fn balanced() -> Self {
        builtin_presets()
            .into_iter()
            .find(|preset| preset.name == "Balanced")
            .unwrap_or_else(Self::flat)
    }

    /// The country curve the old default state carried, kept for tests and
    /// for callers that want a named non-flat curve without a directory.
    pub fn modern_country() -> Self {
        builtin_presets()
            .into_iter()
            .find(|preset| preset.name == "Modern Country")
            .unwrap_or_else(Self::flat)
    }

    fn from_spec(spec: &PresetSpec) -> Self {
        let mut preset = Self::from_gains(spec.name, &spec.gains);
        preset.group = spec.group.as_str().to_string();
        preset.eq_model = spec.model.to_string();
        preset.eq_model_amount = spec.model_amount;
        preset.subsonic_hz = spec.subsonic_hz;
        preset.mono_below_hz = spec.mono_below_hz;
        preset.fuzz_amount = spec.fuzz_amount;
        if let Some(dynamic) = spec.dynamic {
            for (band, threshold) in preset.bands.iter_mut().zip(dynamic) {
                band.dynamic = threshold.is_some();
                band.threshold_db = threshold.unwrap_or(-18.0);
            }
        }
        preset
    }

    fn from_gains(name: &str, gains: &[f64; 15]) -> Self {
        let bands = ISO_FREQUENCIES
            .iter()
            .zip(gains.iter())
            .enumerate()
            .map(|(idx, (&frequency, &gain_db))| EqBandSpec {
                id: (idx + 1) as u32,
                enabled: true,
                filter_type: if idx == 0 {
                    BandFilterKind::LSC
                } else if idx == ISO_FREQUENCIES.len() - 1 {
                    BandFilterKind::HSC
                } else {
                    BandFilterKind::PK
                },
                frequency,
                gain_db,
                quality: if idx == 0 || idx == ISO_FREQUENCIES.len() - 1 {
                    0.71
                } else {
                    1.41
                },
                dynamic: false,
                threshold_db: -18.0,
                ear: 0,
            })
            .collect();
        Self {
            name: name.to_string(),
            group: default_group(),
            preamp_db: 0.0,
            auto_headroom: true,
            eq_model: default_eq_model(),
            eq_model_amount: default_eq_model_amount(),
            matched_treble: default_matched_treble(),
            subsonic_hz: 0.0,
            mono_below_hz: 0.0,
            fuzz_amount: 0.0,
            tone: ToneSettings::default(),
            bands,
            chain_settings: None,
        }
    }
}

/// The presets that ship with the app. They are never files on disk, so an
/// uninstall takes nothing away and a rename cannot collide with them.
pub fn builtin_presets() -> Vec<EqPreset> {
    PRESET_CATALOG.iter().map(EqPreset::from_spec).collect()
}

/// One factory preset's name and heading, for a picker that draws sections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PresetListing {
    pub name: String,
    pub group: String,
}

/// Every factory preset in catalogue order, headings included.
pub fn catalog() -> Vec<PresetListing> {
    PRESET_CATALOG
        .iter()
        .map(|spec| PresetListing {
            name: spec.name.to_string(),
            group: spec.group.as_str().to_string(),
        })
        .collect()
}

/// The heading of a factory preset by name, for a saved preset that wants to
/// show where its curve came from. User files are `custom`.
pub fn group_of(name: &str) -> Option<EqPresetGroup> {
    crate::preset_catalog::group_of(name)
}

/// A name is a file stem, so it is trimmed and kept to what Windows will
/// accept in one component. `..` and separators are refused by name rather
/// than silently rewritten, because the caller has to tell the user.
pub fn is_valid_preset_name(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty()
        && trimmed.chars().count() <= MAX_PRESET_NAME_CHARS
        && !trimmed.starts_with('.')
        && !trimmed.ends_with('.')
        && !trimmed.ends_with(' ')
        && !trimmed.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
}

fn preset_path(directory: &Path, name: &str) -> Option<PathBuf> {
    if !is_valid_preset_name(name) {
        return None;
    }
    Some(directory.join(format!("{}.json", name.trim())))
}

fn user_preset_names(directory: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()?.to_str()? != "json" {
                return None;
            }
            path.file_stem()?.to_str().map(str::to_string)
        })
        .collect()
}

/// Every preset the dropdown shows: built-ins first, then the user's files,
/// sorted case-insensitively so a rename cannot split one name in two.
pub fn list(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = builtin_presets()
        .into_iter()
        .map(|preset| preset.name)
        .collect();
    names.extend(user_preset_names(directory));
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup_by(|left, right| left.eq_ignore_ascii_case(right.as_str()));
    names
}

fn user_preset_file(directory: &Path, name: &str) -> Option<PathBuf> {
    // Windows' filesystem is case-insensitive and the other two are not; the
    // name is matched first so "my curve" finds "My Curve" on every platform.
    let trimmed = name.trim();
    user_preset_names(directory)
        .into_iter()
        .find(|existing| existing.eq_ignore_ascii_case(trimmed))
        .and_then(|existing| preset_path(directory, &existing))
}

/// Loads a preset by name, preferring the user's file over a built-in of the
/// same name: saving over a built-in is how it is customised.
pub fn load(directory: &Path, name: &str) -> Result<EqPreset, String> {
    if !is_valid_preset_name(name) {
        return Err(format!("\"{}\" is not a valid preset name", name.trim()));
    }
    if let Some(path) = user_preset_file(directory, name) {
        let bytes = fs::read(&path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        return serde_json::from_slice(&bytes)
            .map_err(|error| format!("{} is not a readable preset: {error}", path.display()));
    }
    builtin_presets()
        .into_iter()
        .find(|preset| preset.name.eq_ignore_ascii_case(name.trim()))
        .ok_or_else(|| format!("there is no preset named \"{}\"", name.trim()))
}

/// Writes a preset whole through a temporary file, so a crash mid-write leaves
/// the previous preset rather than half of a new one.
pub fn save(directory: &Path, preset: &EqPreset) -> Result<(), String> {
    let Some(path) = preset_path(directory, &preset.name) else {
        return Err(format!(
            "\"{}\" is not a valid preset name",
            preset.name.trim()
        ));
    };
    fs::create_dir_all(directory)
        .map_err(|error| format!("could not create {}: {error}", directory.display()))?;
    let json = serde_json::to_vec_pretty(preset)
        .map_err(|error| format!("could not encode the preset: {error}"))?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, &json)
        .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, &path)
        .map_err(|error| format!("could not replace {}: {error}", path.display()))
}

/// Removes one of the user's presets. A built-in has no file and is refused
/// with the reason, not with a silent no-op.
pub fn delete(directory: &Path, name: &str) -> Result<(), String> {
    if !is_valid_preset_name(name) {
        return Err(format!("\"{}\" is not a valid preset name", name.trim()));
    }
    let Some(path) = user_preset_file(directory, name) else {
        let builtin = builtin_presets()
            .iter()
            .any(|preset| preset.name.eq_ignore_ascii_case(name.trim()));
        return Err(if builtin {
            format!(
                "\"{}\" is a built-in preset and cannot be deleted",
                name.trim()
            )
        } else {
            format!("there is no preset named \"{}\"", name.trim())
        });
    };
    fs::remove_file(&path).map_err(|error| format!("could not delete {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "fluideq-presets-{}-{}-{}",
            label,
            std::process::id(),
            line!()
        ));
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn a_saved_preset_round_trips_and_lists_beside_the_builtins() {
        let directory = temp_dir("round-trip");
        let mut preset = EqPreset::flat();
        preset.name = "My Curve".to_string();
        preset.bands[4].gain_db = 3.5;
        preset.preamp_db = -2.0;
        preset.auto_headroom = false;
        save(&directory, &preset).unwrap();

        let names = list(&directory);
        assert!(names.iter().any(|name| name == "My Curve"));
        assert!(names.iter().any(|name| name == "Flat"));
        // Built-ins are data, not files, so the folder holds only the user's.
        assert_eq!(user_preset_names(&directory), vec!["My Curve".to_string()]);

        let loaded = load(&directory, "my curve").unwrap();
        assert_eq!(loaded, preset, "the name is matched case-insensitively");

        delete(&directory, "My Curve").unwrap();
        assert!(!list(&directory).iter().any(|name| name == "My Curve"));
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn the_factory_catalogue_is_complete_and_has_no_duplicate_names() {
        let presets = builtin_presets();
        assert_eq!(presets.len(), PRESET_CATALOG.len());
        assert!(presets.len() >= 100, "the catalogue is the reference set");

        let mut names: Vec<&str> = presets.iter().map(|p| p.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), presets.len(), "two presets share a name");

        for preset in &presets {
            assert_eq!(preset.bands.len(), ISO_FREQUENCIES.len(), "{}", preset.name);
            assert!(
                EqPresetGroup::parse(&preset.group).is_some(),
                "{} has no heading",
                preset.name
            );
        }
        assert!(catalog().iter().any(|entry| entry.name == "Rock"));
        assert_eq!(group_of("Rock"), Some(EqPresetGroup::Genre));
        assert_eq!(group_of("a curve nobody saved"), None);
    }

    #[test]
    fn a_factory_preset_carries_its_voicing_and_dynamic_bands() {
        let vinyl = builtin_presets()
            .into_iter()
            .find(|preset| preset.name == "Vinyl")
            .expect("Vinyl");
        assert_eq!(vinyl.eq_model, "wide");
        assert_eq!(vinyl.subsonic_hz, 30.0);
        assert_eq!(vinyl.mono_below_hz, 150.0);
        assert_eq!(vinyl.fuzz_amount, 0.15);

        let de_esser = builtin_presets()
            .into_iter()
            .find(|preset| preset.name == "De-esser")
            .expect("De-esser");
        let reacting: Vec<usize> = de_esser
            .bands
            .iter()
            .enumerate()
            .filter(|(_, band)| band.dynamic)
            .map(|(index, _)| index)
            .collect();
        assert_eq!(reacting, vec![11, 12]);
        assert!(de_esser.bands[11].threshold_db < 0.0);
        assert!(!de_esser.bands[10].dynamic);
    }

    #[test]
    fn builtins_cannot_be_deleted_and_unsafe_names_are_refused() {
        let directory = temp_dir("guards");
        assert!(delete(&directory, "Flat").is_err());
        assert!(load(&directory, "Flat").is_ok());

        for name in ["", "  ", "..", ".hidden", "a/b", "a\\b", "C:name", "bad*name"] {
            assert!(!is_valid_preset_name(name), "{name:?} must be refused");
            let mut preset = EqPreset::flat();
            preset.name = name.to_string();
            assert!(save(&directory, &preset).is_err(), "{name:?} must not save");
        }

        // The positive control: a plain name with spaces and punctuation the
        // filesystem accepts still saves.
        let mut preset = EqPreset::flat();
        preset.name = "Rock (Live) 2026".to_string();
        save(&directory, &preset).unwrap();
        assert!(load(&directory, "Rock (Live) 2026").is_ok());
        let _ = fs::remove_dir_all(&directory);
    }
}
