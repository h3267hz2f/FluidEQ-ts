/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! The curve a track remembers.
//!
//! A whole `AppStateSnapshot` per remembered track, beside the state rather
//! than inside it: the tuning belongs to the file, and losing one must not
//! cost the machine its EQ. Only the curve is restored on play (the same
//! `restore_curve` a game preset uses), so device routing and the window's
//! habits stay where they are.

use fluideq_state::persistence::sanitize_loaded_snapshot;
use fluideq_state::AppStateSnapshot;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const TUNINGS_FILE: &str = "track-tunings.json";
const MAX_TUNINGS: usize = 2_000;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackTuningInfo {
    pub remembered: bool,
    /// The preset name the curve carried when it was remembered, if any.
    pub preset: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct TuningFile {
    #[serde(default)]
    tracks: BTreeMap<String, AppStateSnapshot>,
}

struct Inner {
    data_dir: PathBuf,
    tracks: Mutex<BTreeMap<String, AppStateSnapshot>>,
}

pub struct TrackTuningState(Arc<Inner>);

impl TrackTuningState {
    pub fn new(data_dir: PathBuf) -> Self {
        let tracks = load(&data_dir);
        Self(Arc::new(Inner {
            data_dir,
            tracks: Mutex::new(tracks),
        }))
    }

    pub fn get(&self, path: &str) -> Option<AppStateSnapshot> {
        self.0.tracks.lock().ok()?.get(path).cloned()
    }

    pub fn info(&self, path: &str) -> TrackTuningInfo {
        let Ok(tracks) = self.0.tracks.lock() else {
            return TrackTuningInfo::default();
        };
        info_of(tracks.get(path))
    }

    pub fn remember(
        &self,
        path: &str,
        snapshot: AppStateSnapshot,
    ) -> Result<TrackTuningInfo, String> {
        let mut tracks = self
            .0
            .tracks
            .lock()
            .map_err(|_| String::from("the track tunings are not reachable"))?;
        if !tracks.contains_key(path) && tracks.len() >= MAX_TUNINGS {
            return Err(format!(
                "there are already {MAX_TUNINGS} remembered tracks; forget one first"
            ));
        }
        tracks.insert(path.to_string(), snapshot);
        save(&self.0.data_dir, &tracks)?;
        Ok(info_of(tracks.get(path)))
    }

    pub fn forget(&self, path: &str) -> Result<TrackTuningInfo, String> {
        let mut tracks = self
            .0
            .tracks
            .lock()
            .map_err(|_| String::from("the track tunings are not reachable"))?;
        if tracks.remove(path).is_some() {
            save(&self.0.data_dir, &tracks)?;
        }
        Ok(TrackTuningInfo::default())
    }
}

fn info_of(snapshot: Option<&AppStateSnapshot>) -> TrackTuningInfo {
    match snapshot {
        Some(snapshot) => {
            let preset = snapshot.active_preset_name.trim();
            TrackTuningInfo {
                remembered: true,
                preset: (!preset.is_empty()).then(|| preset.to_string()),
            }
        }
        None => TrackTuningInfo::default(),
    }
}

fn load(data_dir: &Path) -> BTreeMap<String, AppStateSnapshot> {
    let Ok(bytes) = std::fs::read(data_dir.join(TUNINGS_FILE)) else {
        return BTreeMap::new();
    };
    let file: TuningFile = serde_json::from_slice(&bytes).unwrap_or_default();
    file.tracks
        .into_iter()
        .filter_map(|(path, mut snapshot)| {
            if path.trim().is_empty() {
                return None;
            }
            sanitize_loaded_snapshot(&mut snapshot);
            Some((path, snapshot))
        })
        .collect()
}

fn save(data_dir: &Path, tracks: &BTreeMap<String, AppStateSnapshot>) -> Result<(), String> {
    let file = TuningFile {
        tracks: tracks.clone(),
    };
    let json = serde_json::to_vec_pretty(&file)
        .map_err(|error| format!("the track tunings could not be written: {error}"))?;
    std::fs::create_dir_all(data_dir)
        .map_err(|error| format!("the track tuning folder could not be made: {error}"))?;
    let temporary = data_dir.join(format!("{TUNINGS_FILE}.tmp"));
    std::fs::write(&temporary, json)
        .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, data_dir.join(TUNINGS_FILE))
        .map_err(|error| format!("could not replace {TUNINGS_FILE}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluideq_state::AppStateStore;

    #[test]
    fn a_remembered_curve_survives_a_reload_and_can_be_forgotten() {
        let dir = std::env::temp_dir().join(format!("fluideq-tunings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = AppStateStore::new(48_000.0);
        let mut snapshot = store.snapshot();
        snapshot.active_preset_name = String::from("Warm");
        let path = r"C:\Music\a.flac";

        let state = TrackTuningState::new(dir.clone());
        let info = state
            .remember(path, snapshot.clone())
            .expect("a writable data folder");
        assert!(info.remembered);
        assert_eq!(info.preset.as_deref(), Some("Warm"));

        let reloaded = TrackTuningState::new(dir.clone());
        assert_eq!(reloaded.info(path).preset.as_deref(), Some("Warm"));
        assert!(reloaded.get(path).is_some());

        let gone = reloaded.forget(path).expect("a writable data folder");
        assert!(!gone.remembered);
        assert!(TrackTuningState::new(dir.clone()).get(path).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_track_remembers_nothing() {
        let dir = std::env::temp_dir().join(format!("fluideq-tunings-none-{}", std::process::id()));
        let state = TrackTuningState::new(dir);
        assert!(!state.info(r"C:\Music\none.flac").remembered);
        assert!(state.get(r"C:\Music\none.flac").is_none());
    }
}
