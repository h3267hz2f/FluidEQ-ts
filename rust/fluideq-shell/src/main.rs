/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! The desktop shell.
//!
//! A Tauri window hosts the view; the authoritative state and the audio work
//! stay in Rust. The web view is disposable — closing it does not lose the
//! chain, the bands, or the device selection.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod audio_endpoint;
pub mod host_runtime;
pub mod ipc;
pub mod spectrum_bridge;
pub mod system_daemons;

use fluideq_state::{AppStateSnapshot, AppStateStore};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use tauri::{Manager, State};

struct ShellState {
    store: Mutex<AppStateStore>,
    state_path: PathBuf,
}

fn store_of(state: &ShellState) -> Result<MutexGuard<'_, AppStateStore>, String> {
    state
        .store
        .lock()
        .map_err(|_| "the state store was poisoned by an earlier failure".to_string())
}

fn save(store: &AppStateStore, state: &ShellState) {
    // Persistence is best-effort: a read-only profile must not make a slider
    // stop working. The next successful write repairs the file.
    let _ = store.save_to_file(&state.state_path);
}

#[tauri::command]
fn get_snapshot(state: State<'_, ShellState>) -> Result<AppStateSnapshot, String> {
    Ok(store_of(&state)?.snapshot())
}

#[tauri::command]
fn set_master_power(
    state: State<'_, ShellState>,
    enabled: bool,
) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    store.set_master_power(enabled);
    let snapshot = store.snapshot();
    save(&store, &state);
    Ok(snapshot)
}

#[tauri::command]
fn set_preamp_db(state: State<'_, ShellState>, preamp_db: f64) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    store.set_preamp_db(preamp_db);
    let snapshot = store.snapshot();
    save(&store, &state);
    Ok(snapshot)
}

#[tauri::command]
fn set_band_gain(
    state: State<'_, ShellState>,
    band_id: u32,
    frequency: f64,
    gain_db: f64,
    quality: f64,
) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    store.update_band_gain(band_id, frequency, gain_db, quality);
    let snapshot = store.snapshot();
    save(&store, &state);
    Ok(snapshot)
}

#[tauri::command]
fn get_output_volume() -> Result<f64, String> {
    audio_endpoint::get_master_volume()
}

#[tauri::command]
fn set_output_volume(volume: f64) -> Result<f64, String> {
    audio_endpoint::set_master_volume(volume)
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let state_path = data_dir.join("state.json");
            let mut store = AppStateStore::new(48_000.0);
            if state_path.exists() {
                // A corrupt file leaves the defaults in place; the first
                // successful save replaces it.
                let _ = store.load_from_file(&state_path);
            }
            app.manage(ShellState {
                store: Mutex::new(store),
                state_path,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            set_master_power,
            set_preamp_db,
            set_band_gain,
            get_output_volume,
            set_output_volume,
        ])
        .run(tauri::generate_context!())
        .expect("FluidEQ failed to start");
}
