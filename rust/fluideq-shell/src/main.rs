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

pub mod apo_bridge;
pub mod audio_endpoint;
pub mod engine_host;
pub mod engine_setup;
pub mod host_runtime;
pub mod ipc;
pub mod spectrum_bridge;
pub mod system_daemons;

use fluideq_state::{AppStateSnapshot, AppStateStore};
use ipc::{dispatch_ipc_command, ShellIpcCommand, ShellIpcResponse};
use serde_json::Value;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use tauri::{Manager, State};

struct ShellState {
    store: Mutex<AppStateStore>,
    state_path: PathBuf,
    host: engine_host::EngineHost,
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

/// Every store mutation re-sends the whole chain snapshot, so the host never
/// holds a partial opinion about the sound.
fn push_chain(store: &AppStateStore, state: &ShellState, revision: u64) {
    state.host.apply_chain(&store.export_wire_parameters(), revision as u32);
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
    let current = store.snapshot();
    let snapshot = store.set_eq_bands_and_config(
        current.bands,
        &current.eq_model,
        current.matched_treble,
        enabled,
        current.auto_headroom,
        current.preamp_db,
        current.tone,
    );
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn set_preamp_db(state: State<'_, ShellState>, preamp_db: f64) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    let current = store.snapshot();
    let snapshot = store.set_eq_bands_and_config(
        current.bands,
        &current.eq_model,
        current.matched_treble,
        current.master_power,
        current.auto_headroom,
        preamp_db,
        current.tone,
    );
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn set_auto_headroom(
    state: State<'_, ShellState>,
    enabled: bool,
) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    let current = store.snapshot();
    let snapshot = store.set_eq_bands_and_config(
        current.bands,
        &current.eq_model,
        current.matched_treble,
        current.master_power,
        enabled,
        current.preamp_db,
        current.tone,
    );
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
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
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
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

#[tauri::command]
fn engine_status(app: tauri::AppHandle) -> Result<Value, String> {
    let mut status = engine_setup::status(&app)?;
    // The two ways the window can look right while the engine reads nothing:
    // the config file could not be written, or the owner pipe never came up.
    // Both are this process's failure, so they ride beside the helper's answer
    // rather than being guessed at from it.
    if let Some(error) = apo_bridge::last_error() {
        if let Some(object) = status.as_object_mut() {
            object.insert("bridgeError".to_string(), Value::String(error));
        }
    }
    Ok(status)
}

#[tauri::command]
fn install_engine(app: tauri::AppHandle) -> Result<Value, String> {
    engine_setup::install(&app)
}

#[tauri::command]
fn engine_host_status(state: State<'_, ShellState>) -> Result<engine_host::EngineHostStatus, String> {
    Ok(state.host.status())
}

/// The JSON-lines bridge the TypeScript preview server spawns with
/// `--ipc-stdio`: one command per line in, one reply per line out.
///
/// It runs no window and no host — the preview only needs the state centre,
/// and it spawns a debug build, which still owns the console the server reads.
fn run_stdio_ipc_loop() {
    let mut store = AppStateStore::new(48_000.0);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(raw) = line else { break };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<ShellIpcCommand>(trimmed) {
            Ok(command) => dispatch_ipc_command(&mut store, command),
            Err(error) => ShellIpcResponse::Error {
                message: format!("IPC decode error: {error}"),
            },
        };
        let encoded = match serde_json::to_string(&response) {
            Ok(encoded) => encoded,
            Err(_) => continue,
        };
        let mut out = stdout.lock();
        let _ = writeln!(out, "{encoded}");
        let _ = out.flush();
    }
}

fn main() {
    if std::env::args().any(|arg| arg == "--ipc-stdio") {
        run_stdio_ipc_loop();
        return;
    }
    let app = tauri::Builder::default()
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
            let host_exe = app
                .path()
                .resolve(
                    engine_host::ENGINE_HOST_RESOURCE,
                    tauri::path::BaseDirectory::Resource,
                )
                .unwrap_or_else(|_| PathBuf::from(engine_host::ENGINE_HOST_RESOURCE));
            let host = engine_host::EngineHost::new(host_exe);
            let snapshot = store.snapshot();
            host.apply_chain(&store.export_wire_parameters(), snapshot.revision as u32);
            // The system engine keeps its own copy of the chain, reads it from
            // its configuration directory, and passes audio through unless this
            // process holds the owner pipe open.
            apo_bridge::start_owner_link();
            let _ = apo_bridge::publish(&snapshot);
            // A dev build without the staged engine records the failure in the
            // host status; the window still opens.
            let _ = host.start();
            app.manage(ShellState {
                store: Mutex::new(store),
                state_path,
                host,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            set_master_power,
            set_preamp_db,
            set_auto_headroom,
            set_band_gain,
            get_output_volume,
            set_output_volume,
            engine_status,
            install_engine,
            engine_host_status,
        ])
        .build(tauri::generate_context!())
        .expect("FluidEQ failed to start");
    app.run(|app, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            app.state::<ShellState>().host.shutdown();
        }
    });
}
