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
use tauri::menu::MenuBuilder;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, State, WindowEvent};

struct ShellState {
    store: Mutex<AppStateStore>,
    state_path: PathBuf,
    data_dir: PathBuf,
    host: engine_host::EngineHost,
}

fn presets_dir(state: &ShellState) -> PathBuf {
    state.data_dir.join("presets")
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
fn list_presets(state: State<'_, ShellState>) -> Result<Vec<String>, String> {
    Ok(fluideq_state::presets::list(&presets_dir(&state)))
}

#[tauri::command]
fn apply_preset(state: State<'_, ShellState>, name: String) -> Result<AppStateSnapshot, String> {
    let preset = fluideq_state::presets::load(&presets_dir(&state), &name)?;
    let mut store = store_of(&state)?;
    let snapshot = store.apply_preset(&preset);
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
fn load_ir(state: State<'_, ShellState>, bytes: Vec<u8>) -> Result<AppStateSnapshot, String> {
    let folder = PathBuf::from(fluideq_state::apo_analysis_and_status::config_dir());
    if folder.as_os_str().is_empty() {
        return Err("the engine root could not be resolved".to_string());
    }
    let mut store = store_of(&state)?;
    store.load_wav_impulse_response(&bytes)?;
    if let Err(error) = apo_bridge::write_convolution(&folder, &bytes) {
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
    if let Some(object) = status.as_object_mut() {
        if let Some(error) = apo_bridge::last_error() {
            object.insert("bridgeError".to_string(), Value::String(error));
        }
        // And the engine's own last verdicts, which are where an output that
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

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
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
                        let _ = window.hide();
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
    if std::env::args().any(|arg| arg == "--ipc-stdio") {
        run_stdio_ipc_loop();
        return;
    }
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
                data_dir,
                host,
            });
            setup_tray(app)?;
            // Closing the window puts FluidEQ in the tray: the engine keeps
            // processing and the curve is one click away, and quit is an
            // explicit tray entry rather than an accident of the X button.
            if let Some(window) = app.get_webview_window("main") {
                let window_for_event = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = window_for_event.hide();
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            set_master_power,
            set_preamp_db,
            set_auto_headroom,
            set_band_gain,
            list_presets,
            apply_preset,
            save_preset,
            delete_preset,
            load_ir,
            clear_ir,
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
