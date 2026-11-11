/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Atomic Disk Persistence & Crash-Safe State Recovery for `AppStateStore`.
//!
//! Ensures user presets, 11-stage DSP rack configurations, and EQ bands survive
//! application restarts and unexpected terminations:
//! - Writes serialized JSON to a `.tmp` sibling file first, flushes, and atomically
//!   renames over the destination file so a crash mid-write never corrupts the saved state.
//! - Automatically strips transient solo/isolate monitoring flags (`isolate = 0`,
//!   `compare_original = 0`) when loading from disk, matching the contract in
//!   `bass_forge.h` (`readStored` drops `isolate` on load so a saved monitoring mode
//!   never mutes the programme on restart).

use crate::AppStateSnapshot;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Clears all transient monitoring / isolate flags on a loaded snapshot so the user
/// never boots into a soloed delta monitor or muted comparison path.
pub fn sanitize_loaded_snapshot(snapshot: &mut AppStateSnapshot) {
    snapshot.chain_settings.denoise.isolate = 0;
    snapshot.chain_settings.deesser.isolate = 0;
    snapshot.chain_settings.footsteps.isolate = 0;
    snapshot.chain_settings.exciter.isolate = 0;
    snapshot.chain_settings.eq.isolate = 0;
    snapshot.chain_settings.bass_forge.isolate = 0;
    snapshot.chain_settings.bass_punch.isolate = 0;
    snapshot.chain_settings.room.compare_original = 0;
}

fn temp_path_for(target: &Path) -> PathBuf {
    let mut tmp = target.as_os_str().to_os_string();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

/// Atomically persists `AppStateSnapshot` to `path` via write-temp + sync + rename.
pub fn save_snapshot_atomic(path: &Path, snapshot: &AppStateSnapshot) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {}", e))?;
        }
    }

    // The impulse response is a live verdict from this session's engine, not
    // something a restart may come back claiming.
    let mut persistable = snapshot.clone();
    persistable.impulse_response = None;
    let json = serde_json::to_vec_pretty(&persistable)
        .map_err(|e| format!("Failed to serialize state snapshot: {}", e))?;

    let tmp_path = temp_path_for(path);
    {
        let mut file = File::create(&tmp_path)
            .map_err(|e| format!("Failed to create temp state file: {}", e))?;
        file.write_all(&json)
            .map_err(|e| format!("Failed to write temp state file: {}", e))?;
        file.sync_all()
            .map_err(|e| format!("Failed to sync temp state file: {}", e))?;
    }

    fs::rename(&tmp_path, path)
        .map_err(|e| format!("Failed to atomically rename state file: {}", e))?;
    Ok(())
}

/// Loads `AppStateSnapshot` from `path` (falling back to `.tmp` if interrupted during rename)
/// and sanitizes transient monitoring flags.
pub fn load_snapshot_or_recover(path: &Path) -> Result<AppStateSnapshot, String> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(_) => {
            let tmp_path = temp_path_for(path);
            fs::read(&tmp_path)
                .map_err(|e| format!("Failed to read state file or recovery temp file: {}", e))?
        }
    };

    let mut snapshot: AppStateSnapshot = serde_json::from_slice(&bytes)
        .map_err(|e| format!("Failed to parse saved state JSON: {}", e))?;
    sanitize_loaded_snapshot(&mut snapshot);
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppStateStore;

    #[test]
    fn sanitize_clears_all_transient_isolate_monitors_on_load() {
        let store = AppStateStore::new(48_000.0);
        let mut snap = store.snapshot();
        snap.chain_settings.denoise.isolate = 1;
        snap.chain_settings.deesser.isolate = 1;
        snap.chain_settings.footsteps.isolate = 1;
        snap.chain_settings.exciter.isolate = 1;
        snap.chain_settings.eq.isolate = 1;
        snap.chain_settings.bass_forge.isolate = 1;
        snap.chain_settings.bass_punch.isolate = 1;
        snap.chain_settings.room.compare_original = 1;

        sanitize_loaded_snapshot(&mut snap);

        assert_eq!(snap.chain_settings.denoise.isolate, 0);
        assert_eq!(snap.chain_settings.deesser.isolate, 0);
        assert_eq!(snap.chain_settings.footsteps.isolate, 0);
        assert_eq!(snap.chain_settings.exciter.isolate, 0);
        assert_eq!(snap.chain_settings.eq.isolate, 0);
        assert_eq!(snap.chain_settings.bass_forge.isolate, 0);
        assert_eq!(snap.chain_settings.bass_punch.isolate, 0);
        assert_eq!(snap.chain_settings.room.compare_original, 0);
    }
}
