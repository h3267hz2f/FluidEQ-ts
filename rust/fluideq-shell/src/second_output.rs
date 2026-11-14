/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! The second output: `FluidEQ-LAN-Capture.exe` taps the process mix before
//! endpoint effects and plays the mirrors itself, so a machine with two sets
//! of speakers needs no virtual device. Its stdout is the wire (24-byte
//! frames, little endian) and one command a line on its stdin opens, moves or
//! stops a mirror. The helper leaves its own process tree out of the capture,
//! so neither the mirror nor this app's sound is heard twice.

use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

const CAPTURE_RESOURCE: &str = "engine-bin/FluidEQ-LAN-Capture.exe";
/// `kCaptureFrameMagic` in `capture_queue.h`.
const FRAME_MAGIC: u32 = 0x314e_414c;
const HEADER_BYTES: usize = 24;
/// The contract this app speaks; the helper puts it in the ready frame.
const PIPE_VERSION: u32 = 2;
const MAX_PAYLOAD_BYTES: usize = 1_920 * 8 * 4;
const KIND_READY: u32 = 1;
const KIND_AUDIO: u32 = 2;
const KIND_REPLY: u32 = 3;
const KIND_FAILURE: u32 = 4;
const KIND_DELAY: u32 = 5;
/// One mirror per helper; the id is the app's own name for it.
const MIRROR_ID: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecondOutputSnapshot {
    /// The helper is installed beside this build.
    pub available: bool,
    /// A mirror was asked for and is running.
    pub active: bool,
    pub endpoint: Option<String>,
    pub endpoint_name: Option<String>,
    pub volume: f64,
    pub delay_us: u32,
    pub sample_rate: u32,
    pub channels: u16,
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub kind: u32,
    pub sequence: u32,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u16,
    pub payload_bytes: u32,
}

/// `CaptureFrameHeader`, field for field. A frame from anything else is not
/// read: the helper is the only writer of this pipe.
pub fn parse_header(bytes: &[u8; HEADER_BYTES]) -> Option<FrameHeader> {
    let word =
        |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    if word(0) != FRAME_MAGIC {
        return None;
    }
    Some(FrameHeader {
        kind: word(4),
        sequence: word(8),
        sample_rate: word(12),
        channels: u16::from_le_bytes([bytes[16], bytes[17]]),
        frames: u16::from_le_bytes([bytes[18], bytes[19]]),
        payload_bytes: word(20),
    })
}

fn hresult_text(code: u32) -> String {
    format!("0x{code:08X}")
}

fn clamp_volume(volume: f64) -> f64 {
    if volume.is_finite() {
        volume.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Every command that waits for a reply carries the kind it belongs to, so a
/// refused start does not leave the switch reading "on".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingAction {
    Start,
    Volume,
}

struct SharedState {
    snapshot: SecondOutputSnapshot,
    pending: HashMap<u32, PendingAction>,
    next_request: u32,
}

impl SharedState {
    fn new(endpoint: String, name: String, volume: f64) -> Self {
        Self {
            snapshot: SecondOutputSnapshot {
                available: true,
                active: true,
                endpoint: Some(endpoint),
                endpoint_name: Some(name),
                volume,
                ..SecondOutputSnapshot::default()
            },
            pending: HashMap::new(),
            next_request: 1,
        }
    }

    fn take_request(&mut self, action: PendingAction) -> u32 {
        let request = self.next_request;
        self.next_request = request.wrapping_add(1).max(1);
        self.pending.insert(request, action);
        request
    }
}

/// One control frame in, what the window shows out. Returns whether the
/// snapshot changed, so the reader only wakes the view when it has news.
fn apply_frame(shared: &mut SharedState, header: &FrameHeader) -> bool {
    match header.kind {
        KIND_READY => {
            if header.sequence < PIPE_VERSION {
                shared.snapshot.active = false;
                shared.snapshot.problem = Some(format!(
                    "the capture helper speaks pipe version {}, this app needs {PIPE_VERSION}",
                    header.sequence
                ));
                return true;
            }
            shared.snapshot.sample_rate = header.sample_rate;
            shared.snapshot.channels = header.channels;
            true
        }
        KIND_REPLY => {
            if shared.pending.remove(&header.sequence).is_none() {
                return false;
            }
            let result = header.sample_rate as i32;
            if result < 0 {
                shared.snapshot.active = false;
                shared.snapshot.problem = Some(format!(
                    "the second output was refused ({})",
                    hresult_text(header.sample_rate)
                ));
            } else {
                shared.snapshot.problem = None;
            }
            true
        }
        KIND_FAILURE => {
            if header.sequence != MIRROR_ID {
                return false;
            }
            shared.snapshot.active = false;
            shared.snapshot.problem = Some(format!(
                "the second output stopped ({})",
                hresult_text(header.sample_rate)
            ));
            true
        }
        KIND_DELAY => {
            if header.sequence != MIRROR_ID {
                return false;
            }
            shared.snapshot.delay_us = header.sample_rate;
            true
        }
        _ => false,
    }
}

fn read_frames(
    mut stdout: impl Read,
    shared: Arc<Mutex<SharedState>>,
    closing: Arc<AtomicBool>,
    app: AppHandle,
) {
    let mut header_bytes = [0u8; HEADER_BYTES];
    let mut payload = Vec::new();
    'frames: loop {
        if stdout.read_exact(&mut header_bytes).is_err() {
            break;
        }
        let Some(header) = parse_header(&header_bytes) else {
            break;
        };
        if header.payload_bytes > 0 {
            let bytes = header.payload_bytes as usize;
            if bytes > MAX_PAYLOAD_BYTES {
                break;
            }
            // Audio carries the samples; control frames carry none, but any
            // payload is consumed whole so the stream cannot fall out of step.
            payload.resize(bytes, 0);
            if stdout.read_exact(&mut payload).is_err() {
                break;
            }
            if header.kind == KIND_AUDIO {
                continue;
            }
        }
        let changed = {
            let Ok(mut state) = shared.lock() else {
                break;
            };
            let changed = apply_frame(&mut state, &header);
            let stopped = !state.snapshot.active;
            drop(state);
            if stopped {
                notify(&app, &shared);
                break 'frames;
            }
            changed
        };
        if changed {
            notify(&app, &shared);
        }
    }
    if closing.load(Ordering::Acquire) {
        return;
    }
    if let Ok(mut state) = shared.lock() {
        if state.snapshot.active {
            state.snapshot.active = false;
            state.snapshot.problem = Some(String::from("the capture helper stopped"));
        }
    }
    notify(&app, &shared);
}

fn notify(app: &AppHandle, shared: &Arc<Mutex<SharedState>>) {
    let Ok(state) = shared.lock() else {
        return;
    };
    let _ = app.emit("second-output", state.snapshot.clone());
}

/// The helper beside this build, when the engine was staged.
pub fn capture_exe(app: &AppHandle) -> Option<PathBuf> {
    let path = app
        .path()
        .resolve(CAPTURE_RESOURCE, tauri::path::BaseDirectory::Resource)
        .ok()?;
    path.is_file().then_some(path)
}

struct Running {
    child: Child,
    stdin: ChildStdin,
    shared: Arc<Mutex<SharedState>>,
    closing: Arc<AtomicBool>,
}

impl Running {
    fn send(&mut self, line: &str) -> Result<(), String> {
        writeln!(self.stdin, "{line}")
            .and_then(|()| self.stdin.flush())
            .map_err(|error| format!("the capture helper is not listening: {error}"))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // The helper watches this process and would exit on its own; the
        // kill is for a helper that answered ready and then wedged.
        self.closing.store(true, Ordering::Release);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct SecondOutputState(Mutex<Option<Running>>);

impl Default for SecondOutputState {
    fn default() -> Self {
        Self::new()
    }
}

impl SecondOutputState {
    pub fn new() -> Self {
        Self(Mutex::new(None))
    }

    pub fn snapshot(&self) -> SecondOutputSnapshot {
        let Ok(guard) = self.0.lock() else {
            return SecondOutputSnapshot::default();
        };
        guard
            .as_ref()
            .and_then(|running| {
                running
                    .shared
                    .lock()
                    .ok()
                    .map(|state| state.snapshot.clone())
            })
            .unwrap_or_default()
    }

    pub fn start(
        &self,
        app: &AppHandle,
        endpoint: &str,
        name: &str,
        volume: f64,
    ) -> Result<SecondOutputSnapshot, String> {
        let guid = endpoint.trim();
        if !fluideq_state::apo::is_valid_endpoint_guid(guid) {
            return Err(String::from("that output is not a Windows endpoint"));
        }
        let volume = clamp_volume(volume);
        let exe = capture_exe(app)
            .ok_or_else(|| format!("{CAPTURE_RESOURCE} is not installed beside this app"))?;
        self.stop()?;

        let mut command = Command::new(&exe);
        command
            .arg("--parent-pid")
            .arg(std::process::id().to_string())
            .arg("--pipe-overlapped")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("the capture helper did not start: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| String::from("the capture helper gave no input"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| String::from("the capture helper gave no output"))?;

        let shared = Arc::new(Mutex::new(SharedState::new(
            guid.to_string(),
            name.to_string(),
            volume,
        )));
        let closing = Arc::new(AtomicBool::new(false));
        {
            let reader = Arc::clone(&shared);
            let reader_closing = Arc::clone(&closing);
            let reader_app = app.clone();
            std::thread::spawn(move || read_frames(stdout, reader, reader_closing, reader_app));
        }

        let request = {
            let mut state = shared
                .lock()
                .map_err(|_| String::from("the second output could not be reached"))?;
            state.take_request(PendingAction::Start)
        };
        let mut running = Running {
            child,
            stdin,
            shared: Arc::clone(&shared),
            closing,
        };
        running.send(&format!("start {request} {MIRROR_ID} {guid} {volume:.3}"))?;
        let snapshot = shared
            .lock()
            .map(|state| state.snapshot.clone())
            .unwrap_or_default();
        let mut guard = self
            .0
            .lock()
            .map_err(|_| String::from("the second output could not be reached"))?;
        *guard = Some(running);
        Ok(snapshot)
    }

    pub fn stop(&self) -> Result<SecondOutputSnapshot, String> {
        let mut guard = self
            .0
            .lock()
            .map_err(|_| String::from("the second output could not be reached"))?;
        guard.take();
        Ok(SecondOutputSnapshot::default())
    }

    pub fn set_volume(&self, volume: f64) -> Result<SecondOutputSnapshot, String> {
        let volume = clamp_volume(volume);
        let mut guard = self
            .0
            .lock()
            .map_err(|_| String::from("the second output could not be reached"))?;
        let running = guard
            .as_mut()
            .ok_or_else(|| String::from("the second output is not running"))?;
        let request = {
            let mut state = running
                .shared
                .lock()
                .map_err(|_| String::from("the second output could not be reached"))?;
            state.take_request(PendingAction::Volume)
        };
        running.send(&format!("volume {request} {MIRROR_ID} {volume:.3}"))?;
        let mut state = running
            .shared
            .lock()
            .map_err(|_| String::from("the second output could not be reached"))?;
        state.snapshot.volume = volume;
        Ok(state.snapshot.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_bytes(kind: u32, sequence: u32, sample_rate: u32, payload: u32) -> [u8; 24] {
        let mut bytes = [0u8; 24];
        bytes[0..4].copy_from_slice(&FRAME_MAGIC.to_le_bytes());
        bytes[4..8].copy_from_slice(&kind.to_le_bytes());
        bytes[8..12].copy_from_slice(&sequence.to_le_bytes());
        bytes[12..16].copy_from_slice(&sample_rate.to_le_bytes());
        bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&480u16.to_le_bytes());
        bytes[20..24].copy_from_slice(&payload.to_le_bytes());
        bytes
    }

    fn started() -> SharedState {
        SharedState::new(
            String::from("{0.0.0.00000000}.board"),
            String::from("Board"),
            0.8,
        )
    }

    #[test]
    fn the_helpers_ready_frame_names_the_stream() {
        let header = parse_header(&header_bytes(KIND_READY, PIPE_VERSION, 48_000, 0)).unwrap();
        assert_eq!(header.kind, KIND_READY);
        assert_eq!(header.sequence, PIPE_VERSION);
        assert_eq!(header.sample_rate, 48_000);
        assert_eq!(header.channels, 2);
    }

    #[test]
    fn a_frame_from_anything_else_is_not_read() {
        let mut bytes = header_bytes(KIND_AUDIO, 9, 48_000, 480 * 2 * 4);
        bytes[0..4].copy_from_slice(&0x314E_4148u32.to_le_bytes());
        assert!(parse_header(&bytes).is_none());
    }

    #[test]
    fn a_refused_start_leaves_the_switch_off() {
        let mut state = started();
        let request = state.take_request(PendingAction::Start);
        let reply = parse_header(&header_bytes(KIND_REPLY, request, 0x8007_0005u32, 0)).unwrap();
        assert!(apply_frame(&mut state, &reply));
        assert!(!state.snapshot.active);
        assert!(
            state
                .snapshot
                .problem
                .as_deref()
                .unwrap_or_default()
                .contains("0x80070005")
        );
    }

    #[test]
    fn an_accepted_start_clears_the_problem() {
        let mut state = started();
        let request = state.take_request(PendingAction::Start);
        let reply = parse_header(&header_bytes(KIND_REPLY, request, 0, 0)).unwrap();
        apply_frame(&mut state, &reply);
        assert!(state.snapshot.problem.is_none());
        assert!(state.snapshot.active);
    }

    #[test]
    fn a_failed_mirror_stops_the_output_and_keeps_the_delay() {
        let mut state = started();
        let delay = parse_header(&header_bytes(KIND_DELAY, MIRROR_ID, 42_000, 0)).unwrap();
        apply_frame(&mut state, &delay);
        assert_eq!(state.snapshot.delay_us, 42_000);
        let failure =
            parse_header(&header_bytes(KIND_FAILURE, MIRROR_ID, 0x8889_0004u32, 0)).unwrap();
        apply_frame(&mut state, &failure);
        assert!(!state.snapshot.active);
        assert_eq!(state.snapshot.delay_us, 42_000);
        assert!(
            state
                .snapshot
                .problem
                .as_deref()
                .unwrap_or_default()
                .contains("0x88890004")
        );
    }

    #[test]
    fn volume_is_held_between_zero_and_one() {
        assert_eq!(clamp_volume(2.0), 1.0);
        assert_eq!(clamp_volume(-0.5), 0.0);
        assert_eq!(clamp_volume(f64::NAN), 0.0);
        assert_eq!(clamp_volume(0.25), 0.25);
    }
}
