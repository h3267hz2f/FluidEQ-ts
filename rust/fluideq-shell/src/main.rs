/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

pub mod host_runtime;
pub mod ipc;
pub mod spectrum_bridge;
pub mod system_daemons;

use fluideq_state::AppStateStore;
use ipc::{dispatch_ipc_command, ShellIpcCommand, ShellIpcResponse};
use spectrum_bridge::SpectrumBridge;
use std::io::{BufRead, Write};
use system_daemons::{
    desktop_cover_of, encode_lighting_colours_frame, encode_playback_open_output,
    summarize_process_meter_reply, DesktopBoxRect, ForegroundGameSupervisor, GameProfileRule,
    LightingFrameReader, MediaWatchCoordinator, OutputWatchCoordinator, VolumeWatchCoordinator,
};

fn run_stdio_ipc_loop(store: &mut AppStateStore) {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(raw) = line else {
            break;
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<ShellIpcCommand>(trimmed) {
            Ok(cmd) => dispatch_ipc_command(store, cmd),
            Err(err) => ShellIpcResponse::Error {
                message: format!("IPC decode error: {}", err),
            },
        };
        if let Ok(encoded) = serde_json::to_string(&response) {
            let mut out = stdout.lock();
            let _ = writeln!(out, "{}", encoded);
            let _ = out.flush();
        }
    }
}

fn main() {
    let mut store = AppStateStore::new(48_000.0);
    if std::env::args().any(|arg| arg == "--ipc-stdio") {
        run_stdio_ipc_loop(&mut store);
        return;
    }
    let mut spectrum = SpectrumBridge::new(64);
    let mut game_supervisor = ForegroundGameSupervisor::new(vec![GameProfileRule {
        profile_id: "esports-footsteps".to_string(),
        executable_match: "valorant.exe".to_string(),
        footsteps_lift: 0.70,
        footsteps_calm: 0.55,
        enable_low_latency: true,
    }]);
    let mut volume_watch = VolumeWatchCoordinator::default();
    let mut media_watch = MediaWatchCoordinator::default();
    let mut output_watch = OutputWatchCoordinator::default();
    let mut lighting_reader = LightingFrameReader::default();

    let _ = volume_watch.handle_line("ready\t2\n");
    let _ = volume_watch.handle_line("volume\t0.8000\t0\n");
    let _ = media_watch.handle_line("null\n", &mut store);
    let _ = output_watch.handle_line("outputs\n");
    let _ = output_watch.update_devices_and_switch_store(
        r#"[{"id":"{0.0.0.00000000}.{11111111-2222-3333-4444-555555555555}","name":"Headphones (USB DAC)","guid":"{11111111-2222-3333-4444-555555555555}","isDefault":true,"isActive":true,"isEqualizerApoAttached":false,"isFluidEngineAttached":true,"canHostEffects":true,"effectsEnabled":true,"sampleRate":48000,"channels":2,"batteryPercent":90}]"#,
        &mut store,
    );
    if let Some(lighting_pkt) = encode_lighting_colours_frame(0, &[[0, 180, 255]]) {
        lighting_reader.feed(&lighting_pkt);
        let _ = lighting_reader.next_frame();
    }
    let _cover = desktop_cover_of(
        DesktopBoxRect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        },
        &[],
        &[],
    );
    let _open_pkt = encode_playback_open_output(1, "{11111111-2222-3333-4444-555555555555}");
    let _meter = summarize_process_meter_reply(
        "4120 18350080 29360128 950000 fluideq-shell.exe\n",
    );

    let initial = dispatch_ipc_command(&mut store, ShellIpcCommand::GetSnapshot);
    let _opra = dispatch_ipc_command(
        &mut store,
        ShellIpcCommand::ApplyOpraHeadphoneCurve {
            opra_dir: "opra/curves".to_string(),
            vendor: "sennheiser".to_string(),
            product: "hd_600".to_string(),
            curve_id: None,
        },
    );
    let _swift = dispatch_ipc_command(
        &mut store,
        ShellIpcCommand::VerifySwiftF0Model {
            path: "assets/models/swift-f0.onnx".to_string(),
        },
    );
    let _wire = dispatch_ipc_command(&mut store, ShellIpcCommand::ExportWireParameters);
    let _hold = game_supervisor.handle_line(
        "front\t7712\t0,0,1920,1080\tVALORANT\tC:\\Riot Games\\VALORANT\\valorant.exe\n",
        &mut store,
    );
    let _release = game_supervisor.handle_line("gone\t7712\n", &mut store);

    let _active_frame = spectrum.poll_frame(
        store.snapshot().residency.residency_mode,
        true,
        -14.0,
    );
    let mut residency_hooks = fluideq_state::residency::NativeResidencyHookBridge::new();
    let _standby = store.apply_residency_transition_with_hooks(
        fluideq_state::residency::WindowResidencyMode::ColdStandby,
        &mut residency_hooks,
    );
    let _restored = store.apply_residency_transition_with_hooks(
        fluideq_state::residency::WindowResidencyMode::Active,
        &mut residency_hooks,
    );

    let mut cadence = host_runtime::TelemetryCadenceTracker::new();
    let shutdown_cmd = system_daemons::encode_dsp_host_command_frame(
        fluideq_dsp_sys::FeqWireCommand::FEQ_CMD_SHUTDOWN,
        1,
        1,
        0,
        0,
        0.0,
    );
    let mut pipe_in = std::io::Cursor::new(shutdown_cmd);
    let mut pipe_out = Vec::new();
    let _ = host_runtime::run_supervised_pipe_command_loop(
        &mut pipe_in,
        &mut pipe_out,
        &mut store,
        false,
        &mut cadence,
    );

    if let Ok(json) = serde_json::to_string(&initial) {
        println!("{}", json);
    }
}
