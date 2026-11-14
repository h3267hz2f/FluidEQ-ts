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
pub mod game_presets;
pub mod host_runtime;
pub mod media_watch;
pub mod hotkeys;
pub mod output_watch;
pub mod second_output;
pub mod spectrum_bridge;
pub mod startup;
pub mod system_daemons;

use fluideq_state::residency::WindowResidencyMode;
use fluideq_state::{AppStateSnapshot, AppStateStore};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use tauri::menu::MenuBuilder;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, State, WindowEvent};

struct ShellState {
    store: Mutex<AppStateStore>,
    state_path: PathBuf,
    data_dir: PathBuf,
    host: engine_host::EngineHost,
    game_watch: Mutex<Option<game_presets::Watch>>,
    /// The curve a game interrupted, put back when its process ends.
    game_restore: Mutex<Option<AppStateSnapshot>>,
    /// Adaptive spectrum and multi-stage telemetry, polled by the event loop.
    spectrum: Mutex<spectrum_bridge::SpectrumBridge>,
}

fn presets_dir(state: &ShellState) -> PathBuf {
    state.data_dir.join("presets")
}

fn opra_curves_dir(app: &tauri::AppHandle) -> PathBuf {
    if let Ok(resource_dir) = app.path().resolve("opra/curves", tauri::path::BaseDirectory::Resource) {
        if resource_dir.exists() {
            return resource_dir;
        }
    }
    let local = PathBuf::from("opra/curves");
    if local.exists() {
        return local;
    }
    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join("opra").join("curves");
        if candidate.exists() {
            return candidate;
        }
    }
    local
}

fn room_heads_dir(app: &tauri::AppHandle) -> PathBuf {
    if let Ok(resource_dir) = app.path().resolve("assets/room/heads", tauri::path::BaseDirectory::Resource) {
        if resource_dir.exists() {
            return resource_dir;
        }
    }
    let local = PathBuf::from("assets/room/heads");
    if local.exists() {
        return local;
    }
    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join("assets").join("room").join("heads");
        if candidate.exists() {
            return candidate;
        }
    }
    local
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

/// A mapped game came to the front: remember the curve it interrupted, apply
/// its preset, and tell the window. Returns false when the preset is gone, so
/// the watcher does not hold a game it cannot switch.
fn engage_game(app: &tauri::AppHandle, preset_name: &str) -> bool {
    let Some(state) = app.try_state::<ShellState>() else {
        return false;
    };
    let Ok(preset) = fluideq_state::presets::load(&presets_dir(&state), preset_name) else {
        return false;
    };
    let Ok(mut store) = store_of(&state) else {
        return false;
    };
    if let Ok(mut slot) = state.game_restore.lock() {
        slot.get_or_insert(store.snapshot());
    }
    store.apply_preset(&preset);
    store.ensure_room_head_loaded(&room_heads_dir(app));
    let snapshot = store.snapshot();
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    let _ = app.emit("snapshot", snapshot);
    true
}

/// The held game ended: put back the curve it interrupted.
fn release_game(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<ShellState>() else {
        return;
    };
    let Some(saved) = state
        .game_restore
        .lock()
        .ok()
        .and_then(|mut slot| slot.take())
    else {
        return;
    };
    let Ok(mut store) = store_of(&state) else {
        return;
    };
    store.restore_curve(&saved);
    store.ensure_room_head_loaded(&room_heads_dir(app));
    let snapshot = store.snapshot();
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    let _ = app.emit("snapshot", snapshot);
}

/// Restarts the watcher against the rules on disk: an edit is rare, and one
/// helper for the newest rules is simpler than teaching a running one to
/// replace its list. A game it was holding is released first, so editing the
/// rules cannot strand a game's curve on the machine.
fn restart_game_watch(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<ShellState>() else {
        return;
    };
    let Ok(mut slot) = state.game_watch.lock() else {
        return;
    };
    if let Some(mut watch) = slot.take() {
        if watch.holds_game() {
            release_game(app);
        }
        watch.stop();
    }
    let rules = game_presets::load(&state.data_dir);
    if rules.is_empty() {
        return;
    }
    if let Some(watch) = game_presets::spawn(app.clone(), rules, engage_game, release_game) {
        *slot = Some(watch);
    }
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

#[allow(clippy::too_many_arguments)]
#[tauri::command]
fn set_tone_and_voicing(
    state: State<'_, ShellState>,
    bass_db: f64,
    treble_db: f64,
    low_cut_hz: f64,
    high_cut_hz: f64,
    eq_model: String,
    eq_model_amount: f64,
    matched_treble: bool,
) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    let mut tone = store.snapshot().tone;
    tone.bass_db = bass_db;
    tone.treble_db = treble_db;
    tone.low_cut_hz = low_cut_hz;
    tone.high_cut_hz = high_cut_hz;
    let snapshot = store.set_tone_and_voicing(tone, &eq_model, eq_model_amount, matched_treble);
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn set_stage_enabled(
    state: State<'_, ShellState>,
    stage: String,
    enabled: bool,
) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    let mut settings = store.snapshot().chain_settings;
    fluideq_state::rack_stages::set_stage_enabled(&mut settings, &stage, enabled)?;
    store.update_chain_settings(settings);
    let snapshot = store.snapshot();
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn set_stage_parameter(
    state: State<'_, ShellState>,
    stage: String,
    parameter: String,
    value: f64,
) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    let mut settings = store.snapshot().chain_settings;
    fluideq_state::rack_stages::set_stage_parameter(&mut settings, &stage, &parameter, value)?;
    store.update_chain_settings(settings);
    let snapshot = store.snapshot();
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn set_stage_isolate(
    state: State<'_, ShellState>,
    stage: String,
    enabled: bool,
) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    let mut settings = store.snapshot().chain_settings;
    fluideq_state::rack_stages::set_stage_isolate(&mut settings, &stage, enabled)?;
    store.update_chain_settings(settings);
    let snapshot = store.snapshot();
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn set_normalizer_mode(
    state: State<'_, ShellState>,
    mode: String,
) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    let mut settings = store.snapshot().chain_settings;
    fluideq_state::rack_stages::set_normalizer_mode(&mut settings, &mode)?;
    store.update_chain_settings(settings);
    let snapshot = store.snapshot();
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn set_master_target(
    state: State<'_, ShellState>,
    target: String,
) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    let mut settings = store.snapshot().chain_settings;
    fluideq_state::rack_stages::set_master_target(&mut settings, &target)?;
    store.update_chain_settings(settings);
    let snapshot = store.snapshot();
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn set_dynamics_preset(
    state: State<'_, ShellState>,
    preset: String,
) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    let mut settings = store.snapshot().chain_settings;
    fluideq_state::rack_stages::set_dynamics_preset(&mut settings, &preset)?;
    store.update_chain_settings(settings);
    let snapshot = store.snapshot();
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
    store.update_band_gain(band_id, frequency, gain_db, quality)?;
    let snapshot = store.snapshot();
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn set_band_config(
    state: State<'_, ShellState>,
    band_id: u32,
    enabled: bool,
    filter_type: String,
    ear: u8,
) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    let snapshot = store.update_band_config(band_id, enabled, &filter_type, ear)?;
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn import_apo_curve(
    state: State<'_, ShellState>,
    text: String,
) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    let snapshot = store.import_apo_text(&text)?;
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

/// Writes the current curve as Equalizer APO text into the app data folder and
/// returns the path. The window has no file picker, so the export has a
/// predictable home the user can open.
#[tauri::command]
fn export_apo_curve(state: State<'_, ShellState>) -> Result<String, String> {
    let store = store_of(&state)?;
    let text = store.export_apo_text();
    let folder = state.data_dir.join("exports");
    std::fs::create_dir_all(&folder)
        .map_err(|error| format!("could not create {}: {error}", folder.display()))?;
    let path = folder.join("fluideq-apo.txt");
    std::fs::write(&path, text.as_bytes())
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Writes the whole chain as a `.fluideq` bundle into the app data folder and
/// returns the path, the same predictable home the APO text export has.
#[tauri::command]
fn export_chain_bundle(state: State<'_, ShellState>) -> Result<String, String> {
    let store = store_of(&state)?;
    let snapshot = store.snapshot();
    let text = store.export_chain_bundle(snapshot.active_device_name.as_deref())?;
    let folder = state.data_dir.join("exports");
    std::fs::create_dir_all(&folder)
        .map_err(|error| format!("could not create {}: {error}", folder.display()))?;
    let name = fluideq_state::chain_bundle::export_file_name(&snapshot.active_preset_name);
    let path = folder.join(name);
    std::fs::write(&path, text.as_bytes())
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
fn import_chain_bundle(
    state: State<'_, ShellState>,
    text: String,
) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    let snapshot = store.import_chain_bundle(&text)?;
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn list_presets(state: State<'_, ShellState>) -> Result<Vec<String>, String> {
    Ok(fluideq_state::presets::list(&presets_dir(&state)))
}

#[tauri::command]
fn list_preset_catalog() -> Vec<fluideq_state::presets::PresetListing> {
    fluideq_state::presets::catalog()
}

#[tauri::command]
fn list_room_presets() -> Vec<fluideq_state::room_catalog::RoomListing> {
    fluideq_state::room_catalog::catalog()
}

#[tauri::command]
fn apply_preset(
    app: tauri::AppHandle,
    state: State<'_, ShellState>,
    name: String,
) -> Result<AppStateSnapshot, String> {
    let preset = fluideq_state::presets::load(&presets_dir(&state), &name)?;
    let mut store = store_of(&state)?;
    store.apply_preset(&preset);
    // The preset carries the Room head index but not the HRTF taps, so the
    // kernel has to be staged again or the chain keeps the previous head.
    store.ensure_room_head_loaded(&room_heads_dir(&app));
    let snapshot = store.snapshot();
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn save_preset(state: State<'_, ShellState>, name: String) -> Result<Vec<String>, String> {
    let mut store = store_of(&state)?;
    let preset = store.current_preset(&name);
    fluideq_state::presets::save(&presets_dir(&state), &preset)?;
    let snapshot = store.set_active_preset_name(&preset.name);
    save(&store, &state);
    let _ = apo_bridge::publish(&snapshot);
    Ok(fluideq_state::presets::list(&presets_dir(&state)))
}

#[tauri::command]
fn delete_preset(state: State<'_, ShellState>, name: String) -> Result<Vec<String>, String> {
    fluideq_state::presets::delete(&presets_dir(&state), &name)?;
    let mut store = store_of(&state)?;
    if store
        .snapshot()
        .active_preset_name
        .eq_ignore_ascii_case(name.trim())
    {
        // The curve stays as it is; only its claim to a name that no longer
        // exists goes away.
        let snapshot = store.set_active_preset_name("Custom curve");
        save(&store, &state);
        let _ = apo_bridge::publish(&snapshot);
    }
    Ok(fluideq_state::presets::list(&presets_dir(&state)))
}

#[tauri::command]
fn list_game_presets(state: State<'_, ShellState>) -> Vec<game_presets::GameRule> {
    game_presets::load(&state.data_dir)
}

#[tauri::command]
fn set_game_preset(
    app: tauri::AppHandle,
    state: State<'_, ShellState>,
    executable: String,
    preset: String,
) -> Result<Vec<game_presets::GameRule>, String> {
    let rules = game_presets::upsert(&state.data_dir, &executable, &preset)?;
    restart_game_watch(&app);
    Ok(rules)
}

#[tauri::command]
fn remove_game_preset(
    app: tauri::AppHandle,
    state: State<'_, ShellState>,
    executable: String,
) -> Result<Vec<game_presets::GameRule>, String> {
    let rules = game_presets::remove(&state.data_dir, &executable)?;
    restart_game_watch(&app);
    Ok(rules)
}

#[tauri::command]
fn game_watch_available(app: tauri::AppHandle) -> bool {
    game_presets::watcher_available(&app)
}

#[tauri::command]
fn load_ir(state: State<'_, ShellState>, bytes: Vec<u8>) -> Result<AppStateSnapshot, String> {
    let folder = PathBuf::from(fluideq_state::apo_analysis_and_status::config_dir());
    if folder.as_os_str().is_empty() {
        return Err("the engine root could not be resolved".to_string());
    }
    // The window accepts AIFF as well as WAV, but the engine's own
    // Convolution reader is RIFF/WAVE-only. Hand both halves the same
    // canonical float WAV so an AIFF cannot load in the chain and do nothing
    // system-wide.
    let canonical = fluideq_state::wav_ir::canonical_engine_wav(&bytes)?;
    let mut store = store_of(&state)?;
    store.load_wav_impulse_response(&canonical)?;
    if let Err(error) = apo_bridge::write_convolution(&folder, &canonical) {
        store.clear_impulse_response();
        return Err(format!(
            "could not write {}: {error}",
            folder.join(apo_bridge::CONVOLUTION_FILE).display()
        ));
    }
    let snapshot = store.snapshot();
    save(&store, &state);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn clear_ir(state: State<'_, ShellState>) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    store.clear_impulse_response();
    let snapshot = store.snapshot();
    save(&store, &state);
    let folder = PathBuf::from(fluideq_state::apo_analysis_and_status::config_dir());
    if !folder.as_os_str().is_empty() {
        apo_bridge::remove_convolution(&folder);
        let _ = apo_bridge::publish(&snapshot);
    }
    Ok(snapshot)
}

#[tauri::command]
fn list_opra_vendors(app: tauri::AppHandle, state: State<'_, ShellState>) -> Vec<String> {
    let dir = opra_curves_dir(&app);
    let store = match store_of(&state) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    store.list_opra_vendors(&dir)
}

#[tauri::command]
fn list_opra_models(
    app: tauri::AppHandle,
    state: State<'_, ShellState>,
    vendor: String,
) -> Result<Vec<String>, String> {
    let dir = opra_curves_dir(&app);
    let mut store = store_of(&state)?;
    store.list_opra_models(&dir, &vendor)
}

#[tauri::command]
fn apply_opra_curve(
    app: tauri::AppHandle,
    state: State<'_, ShellState>,
    vendor: String,
    product: String,
    curve_id: Option<String>,
) -> Result<AppStateSnapshot, String> {
    let dir = opra_curves_dir(&app);
    let mut store = store_of(&state)?;
    store.apply_opra_headphone_curve(&dir, &vendor, &product, curve_id.as_deref())?;
    let snapshot = store.snapshot();
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn set_room_preset(
    state: State<'_, ShellState>,
    preset: i32,
) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    let snapshot = store.set_room_preset(preset);
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn room_reset(state: State<'_, ShellState>) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    let snapshot = store.reset_room();
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn room_migrate_renderer(state: State<'_, ShellState>) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    let snapshot = store.migrate_room_renderer();
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn set_room_speaker(
    state: State<'_, ShellState>,
    speaker: u32,
    parameter: String,
    value: f64,
) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    let snapshot = store.set_room_speaker(speaker, &parameter, value)?;
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn set_room_head(
    app: tauri::AppHandle,
    state: State<'_, ShellState>,
    head: i32,
) -> Result<AppStateSnapshot, String> {
    let dir = room_heads_dir(&app);
    let mut store = store_of(&state)?;
    let snapshot = store.set_room_head_choice(head, &dir)?;
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn set_room_bass_management(
    state: State<'_, ShellState>,
    enabled: bool,
) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    let snapshot = store.set_room_bass_management(enabled);
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
fn set_output_volume(app: tauri::AppHandle, volume: f64) -> Result<f64, String> {
    let applied = audio_endpoint::set_master_volume(volume)?;
    // Every window follows the one number, whichever one moved it.
    let _ = app.emit("volume", applied);
    Ok(applied)
}

/// The compact player is a second, frameless window that lives off the same
/// media events; showing it never takes focus from what is playing.
#[tauri::command]
fn show_compact_player(app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("player")
        .ok_or_else(|| String::from("the compact player is not configured"))?;
    window
        .show()
        .map_err(|error| format!("could not show the compact player: {error}"))?;
    let _ = window.set_always_on_top(true);
    Ok(())
}

#[tauri::command]
fn hide_compact_player(app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("player")
        .ok_or_else(|| String::from("the compact player is not configured"))?;
    window
        .hide()
        .map_err(|error| format!("could not hide the compact player: {error}"))
}

/// The helper's answer plus this process's own failures: a config file that
/// could not be written, or an owner pipe that never came up. Both ride beside
/// the helper's answer rather than being guessed at from it, and so do the
/// engine's last verdicts, where an output that was attached but never
/// instantiated says why.
fn enrich_engine_status(mut status: Value) -> Value {
    if let Some(object) = status.as_object_mut() {
        if let Some(error) = apo_bridge::last_error() {
            object.insert("bridgeError".to_string(), Value::String(error));
        }
        // And the engine's own last verdicts, which is where an output that
        // was attached but never instantiated says why.
        object.insert(
            "engineLog".to_string(),
            Value::Array(
                engine_setup::log_tail()
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    status
}

#[tauri::command]
fn engine_status(app: tauri::AppHandle) -> Result<Value, String> {
    engine_setup::status(&app).map(enrich_engine_status)
}

#[tauri::command]
fn install_engine(app: tauri::AppHandle) -> Result<Value, String> {
    engine_setup::install(&app).map(enrich_engine_status)
}

#[tauri::command]
fn restart_audio(app: tauri::AppHandle) -> Result<Value, String> {
    engine_setup::restart_audio(&app).map(enrich_engine_status)
}

#[tauri::command]
fn engine_attach(app: tauri::AppHandle, guids: Vec<String>) -> Result<Value, String> {
    engine_setup::attach(&app, &guids).map(enrich_engine_status)
}

#[tauri::command]
fn engine_detach(app: tauri::AppHandle, guids: Vec<String>) -> Result<Value, String> {
    engine_setup::detach(&app, &guids).map(enrich_engine_status)
}

#[tauri::command]
fn engine_settle(app: tauri::AppHandle) -> Result<Value, String> {
    engine_setup::settle(&app).map(enrich_engine_status)
}

/// What the rest of the machine is playing, as the bar last heard it.
#[tauri::command]
fn media_status(state: State<'_, media_watch::MediaWatchState>) -> media_watch::MediaSnapshot {
    state.snapshot()
}

/// One transport move, passed to the player the bar names.
#[tauri::command]
fn media_command(
    state: State<'_, media_watch::MediaWatchState>,
    command: String,
) -> Result<(), String> {
    if !matches!(command.as_str(), "play" | "pause" | "next" | "previous") {
        return Err(format!("there is no media command named \"{command}\""));
    }
    state.send(&command)
}

/// The second output as it stands, plus whether this build carries the
/// helper that plays it.
#[tauri::command]
fn second_output_status(
    app: tauri::AppHandle,
    state: State<'_, second_output::SecondOutputState>,
) -> second_output::SecondOutputSnapshot {
    let mut snapshot = state.snapshot();
    snapshot.available = second_output::capture_exe(&app).is_some();
    snapshot
}

#[tauri::command]
fn second_output_start(
    app: tauri::AppHandle,
    state: State<'_, second_output::SecondOutputState>,
    endpoint: String,
    name: Option<String>,
    volume: Option<f64>,
) -> Result<second_output::SecondOutputSnapshot, String> {
    state.start(
        &app,
        &endpoint,
        name.as_deref().unwrap_or("Second output"),
        volume.unwrap_or(0.8),
    )
}

#[tauri::command]
fn second_output_stop(
    app: tauri::AppHandle,
    state: State<'_, second_output::SecondOutputState>,
) -> Result<second_output::SecondOutputSnapshot, String> {
    let mut snapshot = state.stop()?;
    snapshot.available = second_output::capture_exe(&app).is_some();
    Ok(snapshot)
}

#[tauri::command]
fn second_output_volume(
    state: State<'_, second_output::SecondOutputState>,
    volume: f64,
) -> Result<second_output::SecondOutputSnapshot, String> {
    state.set_volume(volume)
}

#[tauri::command]
fn engine_host_status(state: State<'_, ShellState>) -> Result<engine_host::EngineHostStatus, String> {
    Ok(state.host.status())
}

#[tauri::command]
fn set_close_to_tray(
    state: State<'_, ShellState>,
    enabled: bool,
) -> Result<AppStateSnapshot, String> {
    let mut store = store_of(&state)?;
    let snapshot = store.set_close_to_tray(enabled);
    save(&store, &state);
    Ok(snapshot)
}

#[tauri::command]
fn hotkey_failures() -> Vec<String> {
    hotkeys::failures()
}

fn apply_default_output(
    state: &ShellState,
    guid: &str,
    name: &str,
) -> Result<Option<AppStateSnapshot>, String> {
    let normalized = fluideq_state::device_profiles::normalize_endpoint_guid(guid);
    if normalized.is_empty() {
        return Ok(None);
    }
    let mut store = store_of(state)?;
    if store.active_device_guid() == Some(normalized.as_str()) {
        return Ok(None);
    }
    let snapshot = store.switch_active_output_device(guid, name);
    save(&store, state);
    state
        .host
        .apply_chain(&store.export_wire_parameters(), snapshot.revision as u32);
    let _ = apo_bridge::publish(&snapshot);
    Ok(Some(snapshot))
}

#[tauri::command]
fn follow_output_device(
    state: State<'_, ShellState>,
    guid: String,
    name: String,
) -> Result<Option<AppStateSnapshot>, String> {
    apply_default_output(state.inner(), &guid, &name)
}

/// The watcher's half: ask the engine helper which output is default now and
/// move the chain there. Runs off the UI thread; the window is told when the
/// profile actually changed.
fn follow_default_output_now(app: &tauri::AppHandle) {
    let Ok(status) = engine_setup::status(app) else {
        return;
    };
    let Some(device) = status.get("defaultDevice") else {
        return;
    };
    let guid = device
        .get("guid")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let name = device
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Default Output");
    if guid.is_empty() {
        return;
    }
    let Some(state) = app.try_state::<ShellState>() else {
        return;
    };
    if let Ok(Some(snapshot)) = apply_default_output(state.inner(), guid, name) {
        let _ = app.emit("snapshot", snapshot);
    }
}

#[tauri::command]
fn get_startup_enabled() -> Result<bool, String> {
    startup::enabled()
}

#[tauri::command]
fn set_startup_enabled(enabled: bool) -> Result<bool, String> {
    startup::set(enabled)?;
    startup::enabled()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct WindowPlacement {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    maximized: bool,
}

/// A restored window has to leave a usable piece on one monitor: a titlebar
/// corner is not enough to drag it back if the monitor it was saved on is gone.
fn placement_is_visible(placement: &WindowPlacement, monitors: &[(i32, i32, u32, u32)]) -> bool {
    const MIN_VISIBLE: i64 = 96;
    monitors.iter().any(|&(mx, my, mw, mh)| {
        let monitor_right = i64::from(mx) + i64::from(mw);
        let monitor_bottom = i64::from(my) + i64::from(mh);
        let window_right = i64::from(placement.x) + i64::from(placement.width);
        let window_bottom = i64::from(placement.y) + i64::from(placement.height);
        let overlap_width =
            window_right.min(monitor_right) - i64::from(placement.x).max(i64::from(mx));
        let overlap_height =
            window_bottom.min(monitor_bottom) - i64::from(placement.y).max(i64::from(my));
        overlap_width >= MIN_VISIBLE && overlap_height >= MIN_VISIBLE
    })
}

fn load_window_placement(path: &Path) -> Option<WindowPlacement> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn save_window_placement(path: &Path, window: &tauri::WebviewWindow) {
    let (Ok(position), Ok(size)) = (window.outer_position(), window.inner_size()) else {
        return;
    };
    let placement = WindowPlacement {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
        maximized: window.is_maximized().unwrap_or(false),
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(&placement) {
        let _ = std::fs::write(path, json);
    }
}

fn restore_window_placement(window: &tauri::WebviewWindow, path: &Path) {
    let Some(saved) = load_window_placement(path) else {
        return;
    };
    let monitors: Vec<(i32, i32, u32, u32)> = window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            (position.x, position.y, size.width, size.height)
        })
        .collect();
    if monitors.is_empty() || !placement_is_visible(&saved, &monitors) {
        return;
    }
    if saved.width > 0 && saved.height > 0 {
        let _ = window.set_size(tauri::PhysicalSize::new(saved.width, saved.height));
    }
    let _ = window.set_position(tauri::PhysicalPosition::new(saved.x, saved.y));
    if saved.maximized {
        let _ = window.maximize();
    }
}

/// A minimized window stops the meters; the curve and the chain keep running.
fn minimize_to_warm(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<ShellState>() else {
        return;
    };
    let Ok(mut store) = state.store.lock() else {
        return;
    };
    if store.residency_mode() == WindowResidencyMode::MinimizedWarm {
        return;
    }
    let memory = store.enter_minimized_warm();
    drop(store);
    let _ = app.emit("residency", memory);
}

/// Coming back to the window restores the live meters and the full snapshot.
fn wake_from_residency(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<ShellState>() else {
        return;
    };
    let Ok(mut store) = state.store.lock() else {
        return;
    };
    if store.residency_mode() == WindowResidencyMode::Active {
        return;
    }
    let snapshot = store.wake_to_active();
    drop(store);
    let _ = app.emit("snapshot", snapshot);
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    // Showing the window is waking it: restore the meters and the snapshot
    // here as well, so a tray restore does not wait for a focus event that
    // Windows may not send.
    wake_from_residency(app);
}

/// A hidden window is a warm one: no meters, no spectrum frames, no redraws,
/// while the chain and the WebView stay resident for an instant return.
fn hide_to_warm(app: &tauri::AppHandle, window: &tauri::WebviewWindow) {
    let _ = window.hide();
    minimize_to_warm(app);
}

/// Pushes spectrum bins and multi-stage telemetry to the window on a timer.
///
/// The bridge owns the cadence: sixty frames a second while audio is playing,
/// fifteen while a silent tail decays, and no frames at all once it settles or
/// the window enters `MinimizedWarm` / `ColdStandby`. That is what keeps the
/// canvas animation frame count tied to actual sound rather than to a clock.
fn spawn_spectrum_loop(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        let Some(state) = app.try_state::<ShellState>() else {
            std::thread::sleep(std::time::Duration::from_millis(250));
            continue;
        };
        let packet = {
            let Ok(mut bridge) = state.spectrum.lock() else {
                break;
            };
            let Ok(mut store) = state.store.lock() else {
                break;
            };
            // Warm and cold states emit nothing at all. A sleeping packet every
            // fifth of a second was still a timer waking the WebView for a
            // canvas that is not on screen.
            if !spectrum_bridge::should_emit_frame(store.residency_mode()) {
                drop(store);
                drop(bridge);
                std::thread::sleep(std::time::Duration::from_millis(250));
                continue;
            }
            let (momentary_lufs, true_peak_db) = {
                let meters = store.native_meters_mut();
                let loudness = meters.read_loudness();
                let telemetry = meters.read_master_telemetry();
                (loudness[0], telemetry.auto_headroom_true_peak_db as f32)
            };
            // `FeqMasterTelemetry::default()` is 0 dBTP at -14.2 LUFS; that
            // pair means the engine has published nothing yet, so an unlinked
            // dev build settles into sleep instead of animating a flat floor.
            let meter_has_data = true_peak_db != 0.0 || (momentary_lufs + 14.2).abs() > 0.05;
            let peak_db = if true_peak_db.is_finite() {
                true_peak_db
            } else {
                spectrum_bridge::SPECTRUM_SILENCE_FLOOR_DB
            };
            let audio_playing =
                meter_has_data && peak_db > spectrum_bridge::SPECTRUM_SILENCE_FLOOR_DB;
            bridge.poll_from_store(&mut store, audio_playing, peak_db)
        };
        let _ = app.emit("spectrum", &packet);
        let sleep_ms = if packet.sleeping {
            200
        } else {
            (1_000 / packet.target_fps.max(1)).clamp(8, 1_000)
        };
        std::thread::sleep(std::time::Duration::from_millis(sleep_ms as u64));
    });
}

/// The tray's bypass entry flips the same master switch as the power button,
/// so the chain, the config file and the window cannot disagree.
fn toggle_bypass(app: &tauri::AppHandle) -> Option<AppStateSnapshot> {
    let state = app.try_state::<ShellState>()?;
    let mut store = store_of(&state).ok()?;
    let current = store.snapshot();
    let snapshot = store.set_eq_bands_and_config(
        current.bands,
        &current.eq_model,
        current.matched_treble,
        !current.master_power,
        current.auto_headroom,
        current.preamp_db,
        current.tone,
    );
    save(&store, &state);
    push_chain(&store, &state, snapshot.revision);
    let _ = apo_bridge::publish(&snapshot);
    Some(snapshot)
}

fn toggle_bypass_hotkey(app: &tauri::AppHandle) {
    if let Some(snapshot) = toggle_bypass(app) {
        let _ = app.emit("snapshot", snapshot);
    }
}

fn setup_tray(app: &mut tauri::App) -> tauri::Result<()> {
    // No separate tray asset: the window icon is already embedded by the
    // bundle, and the tray is not a second place to keep one.
    let Some(icon) = app.default_window_icon().cloned() else {
        return Ok(());
    };
    let menu = MenuBuilder::new(app)
        .text("show", "Show FluidEQ")
        .text("bypass", "Bypass / resume")
        .separator()
        .text("quit", "Quit FluidEQ")
        .build()?;
    TrayIconBuilder::with_id("main")
        .icon(icon)
        .tooltip("FluidEQ")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main_window(app),
            "bypass" => {
                if let Some(snapshot) = toggle_bypass(app) {
                    let _ = app.emit("snapshot", snapshot);
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(window) = app.get_webview_window("main") {
                    if window.is_visible().unwrap_or(false) {
                        hide_to_warm(app, &window);
                    } else {
                        show_main_window(app);
                    }
                }
            }
        })
        .build(app)?;
    Ok(())
}

/// Windows keeps a named mutex per session. A second launch finds it and
/// stops before it can serve a second owner pipe or write the same config.
#[cfg(target_os = "windows")]
fn another_instance_is_running() -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{BOOL, CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = "Local\\FluidEQ-Desktop-Owner\0".encode_utf16().collect();
    // SAFETY: `name` is a NUL-terminated UTF-16 string alive for the call.
    match unsafe { CreateMutexW(None, BOOL(1), PCWSTR(name.as_ptr())) } {
        Ok(handle) => {
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                // The first process owns the mutex; this handle is only the
                // probe that discovered it.
                unsafe {
                    let _ = CloseHandle(handle);
                }
                true
            } else {
                // The raw handle stays open until process exit.
                false
            }
        }
        Err(_) => false,
    }
}

#[cfg(target_os = "windows")]
fn focus_existing_window() {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE,
    };

    let title: Vec<u16> = "FluidEQ\0".encode_utf16().collect();
    // SAFETY: the title is NUL-terminated and alive for the call.
    if let Ok(window) = unsafe { FindWindowW(PCWSTR(std::ptr::null()), PCWSTR(title.as_ptr())) } {
        unsafe {
            let _ = ShowWindow(window, SW_RESTORE);
            let _ = SetForegroundWindow(window);
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn another_instance_is_running() -> bool {
    false
}

#[cfg(not(target_os = "windows"))]
fn focus_existing_window() {}

fn main() {
    if another_instance_is_running() {
        focus_existing_window();
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
            // The KEMAR head is a convolution kernel, not a number in the
            // state file, so it has to be staged into the native chain again
            // on every launch or Room would render with no HRTF.
            let heads_dir = room_heads_dir(app.handle());
            store.ensure_room_head_loaded(&heads_dir);
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
            let placement_path = data_dir.join("window.json");
            app.manage(ShellState {
                store: Mutex::new(store),
                state_path,
                data_dir,
                host,
                game_watch: Mutex::new(None),
                game_restore: Mutex::new(None),
                spectrum: Mutex::new(spectrum_bridge::SpectrumBridge::new(96)),
            });
            app.manage(media_watch::MediaWatchState::start(app.handle()));
            app.manage(second_output::SecondOutputState::new());
            setup_tray(app)?;
            hotkeys::spawn(app.handle().clone(), toggle_bypass_hotkey);
            output_watch::spawn(app.handle().clone(), follow_default_output_now);
            spawn_spectrum_loop(app.handle().clone());
            restart_game_watch(app.handle());
            // Closing the window puts FluidEQ in the tray: the engine keeps
            // processing and the curve is one click away, and quit is an
            // explicit tray entry rather than an accident of the X button.
            if let Some(window) = app.get_webview_window("main") {
                restore_window_placement(&window, &placement_path);
                let window_for_event = window.clone();
                let placement_path_for_event = placement_path.clone();
                let app_for_event = app.handle().clone();
                window.on_window_event(move |event| match event {
                    WindowEvent::CloseRequested { api, .. } => {
                        save_window_placement(&placement_path_for_event, &window_for_event);
                        let close_to_tray =
                            match app_for_event.try_state::<ShellState>() {
                                Some(state) => match state.store.lock() {
                                    Ok(store) => store.close_to_tray(),
                                    Err(_) => true,
                                },
                                None => true,
                            };
                        if close_to_tray {
                            api.prevent_close();
                            hide_to_warm(&app_for_event, &window_for_event);
                        }
                    }
                    WindowEvent::Resized(_)
                        if window_for_event.is_minimized().unwrap_or(false) =>
                    {
                        minimize_to_warm(&app_for_event);
                    }
                    WindowEvent::Focused(true) => wake_from_residency(&app_for_event),
                    _ => {}
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            set_master_power,
            set_preamp_db,
            set_auto_headroom,
            set_tone_and_voicing,
            set_stage_enabled,
            set_stage_parameter,
            set_stage_isolate,
            set_normalizer_mode,
            set_master_target,
            set_dynamics_preset,
            set_band_gain,
            set_band_config,
            import_apo_curve,
            export_apo_curve,
            export_chain_bundle,
            import_chain_bundle,
            list_presets,
            list_preset_catalog,
            list_room_presets,
            apply_preset,
            save_preset,
            delete_preset,
            list_game_presets,
            set_game_preset,
            remove_game_preset,
            game_watch_available,
            load_ir,
            clear_ir,
            list_opra_vendors,
            list_opra_models,
            apply_opra_curve,
            set_room_preset,
            room_reset,
            room_migrate_renderer,
            set_room_speaker,
            set_room_head,
            set_room_bass_management,
            get_output_volume,
            set_output_volume,
            show_compact_player,
            hide_compact_player,
            engine_status,
            install_engine,
            restart_audio,
            engine_attach,
            engine_detach,
            engine_settle,
            media_status,
            media_command,
            second_output_status,
            second_output_start,
            second_output_stop,
            second_output_volume,
            engine_host_status,
            get_startup_enabled,
            set_startup_enabled,
            set_close_to_tray,
            hotkey_failures,
            follow_output_device,
        ])
        .build(tauri::generate_context!())
        .expect("FluidEQ failed to start");
    app.run(|app, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            if let Some(window) = app.get_webview_window("main") {
                let path = app.state::<ShellState>().data_dir.join("window.json");
                save_window_placement(&path, &window);
            }
            app.state::<ShellState>().host.shutdown();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITORS: [(i32, i32, u32, u32); 2] =
        [(0, 0, 1920, 1080), (-2560, 0, 2560, 1440)];

    #[test]
    fn window_placements_lost_off_screen_are_not_restored() {
        let inside = WindowPlacement {
            x: 100,
            y: 100,
            width: 1280,
            height: 720,
            maximized: false,
        };
        assert!(placement_is_visible(&inside, &MONITORS));

        let on_left_monitor = WindowPlacement {
            x: -2000,
            y: 200,
            ..inside
        };
        assert!(placement_is_visible(&on_left_monitor, &MONITORS));

        let off_screen = WindowPlacement {
            x: 5000,
            y: 100,
            ..inside
        };
        assert!(!placement_is_visible(&off_screen, &MONITORS));

        // A 20px sliver is not enough to grab the titlebar and drag it back.
        let sliver = WindowPlacement {
            x: 1900,
            y: 100,
            ..inside
        };
        assert!(!placement_is_visible(&sliver, &MONITORS));
        assert!(!placement_is_visible(&inside, &[]));
    }

    #[test]
    fn a_window_placement_round_trips_through_its_file() {
        let directory = std::env::temp_dir().join(format!(
            "fluideq-window-{}-{}",
            std::process::id(),
            line!()
        ));
        let path = directory.join("window.json");
        let placement = WindowPlacement {
            x: -12,
            y: 40,
            width: 1440,
            height: 900,
            maximized: true,
        };
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&path, serde_json::to_string(&placement).unwrap()).unwrap();
        assert_eq!(load_window_placement(&path), Some(placement));
        let _ = std::fs::remove_dir_all(&directory);
    }
}
