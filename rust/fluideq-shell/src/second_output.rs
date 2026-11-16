/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! The second output: a mirror played by a capture helper under the playback
//! helper's tree. The helper taps the process mix before endpoint effects and
//! renders the mirror itself, so a machine with two sets of speakers needs no
//! virtual device — and, because it lives in the receiving side's tree, the
//! network share's own capture never hears it.
//!
//! The frames (24-byte header, little endian) arrive on one pipe connection
//! and one command a line goes out on the other: `start`, `volume`, `stop`.
//! Both pipes belong to this app; the playback helper only starts the child.

use crate::capture_pipe::{CaptureFrame, CaptureHeader, CaptureSession};
use crate::playback_host::{CaptureMode, PlaybackHost};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

const CAPTURE_RESOURCE: &str = "engine-bin/FluidEQ-LAN-Capture.exe";
/// The contract this app speaks; the helper puts it in the ready frame.
const PIPE_VERSION: u32 = 2;
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
fn apply_frame(shared: &mut SharedState, header: &CaptureHeader) -> bool {
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
    session: CaptureSession,
    shared: Arc<Mutex<SharedState>>,
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
        host: &PlaybackHost,
        endpoint: &str,
        name: &str,
        volume: f64,
    ) -> Result<SecondOutputSnapshot, String> {
        let guid = endpoint.trim();
        if !fluideq_state::apo::is_valid_endpoint_guid(guid) {
            return Err(String::from("that output is not a Windows endpoint"));
        }
        let volume = clamp_volume(volume);
        if capture_exe(app).is_none() {
            return Err(format!(
                "{CAPTURE_RESOURCE} is not installed beside this app"
            ));
        }
        self.stop()?;

        let shared = Arc::new(Mutex::new(SharedState::new(
            guid.to_string(),
            name.to_string(),
            volume,
        )));

        let reader = Arc::clone(&shared);
        let reader_app = app.clone();
        let end_shared = Arc::clone(&shared);
        let end_app = app.clone();
        let session = host.start_capture(
            CaptureMode::Local,
            move |frame: &CaptureFrame| {
                if frame.header.kind == KIND_AUDIO {
                    return;
                }
                let Ok(mut state) = reader.lock() else {
                    return;
                };
                let changed = apply_frame(&mut state, &frame.header);
                drop(state);
                if changed {
                    notify(&reader_app, &reader);
                }
            },
            move || {
                if let Ok(mut state) = end_shared.lock() {
                    if state.snapshot.active {
                        state.snapshot.active = false;
                        state.snapshot.problem = Some(String::from("the capture helper stopped"));
                    }
                }
                notify(&end_app, &end_shared);
            },
        )?;

        let request = {
            let mut state = shared
                .lock()
                .map_err(|_| String::from("the second output could not be reached"))?;
            state.take_request(PendingAction::Start)
        };
        if let Err(problem) = session.send(&format!(
            "start {request} {MIRROR_ID} {guid} {volume:.3}"
        )) {
            session.shutdown();
            return Err(problem);
        }

        let snapshot = shared
            .lock()
            .map(|state| state.snapshot.clone())
            .unwrap_or_default();
        let mut guard = self
            .0
            .lock()
            .map_err(|_| String::from("the second output could not be reached"))?;
        *guard = Some(Running { session, shared });
        Ok(snapshot)
    }

    pub fn stop(&self) -> Result<SecondOutputSnapshot, String> {
        let mut guard = self
            .0
            .lock()
            .map_err(|_| String::from("the second output could not be reached"))?;
        if let Some(running) = guard.take() {
            running.session.shutdown();
        }
        Ok(SecondOutputSnapshot::default())
    }

    pub fn set_volume(&self, volume: f64) -> Result<SecondOutputSnapshot, String> {
        let volume = clamp_volume(volume);
        let guard = self
            .0
            .lock()
            .map_err(|_| String::from("the second output could not be reached"))?;
        let running = guard
            .as_ref()
            .ok_or_else(|| String::from("the second output is not running"))?;
        let request = {
            let mut state = running
                .shared
                .lock()
                .map_err(|_| String::from("the second output could not be reached"))?;
            state.take_request(PendingAction::Volume)
        };
        running
            .session
            .send(&format!("volume {request} {MIRROR_ID} {volume:.3}"))?;
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

    fn header(kind: u32, sequence: u32, sample_rate: u32, channels: u16, frames: u16) -> CaptureHeader {
        CaptureHeader {
            kind,
            sequence,
            sample_rate,
            channels,
            frames,
            payload_bytes: 0,
        }
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
        let ready = header(KIND_READY, PIPE_VERSION, 48_000, 2, 0);
        let mut state = started();
        assert!(apply_frame(&mut state, &ready));
        assert_eq!(state.snapshot.sample_rate, 48_000);
        assert_eq!(state.snapshot.channels, 2);
    }

    #[test]
    fn an_older_pipe_is_reported_and_stops_the_switch() {
        let mut state = started();
        let ready = header(KIND_READY, PIPE_VERSION - 1, 48_000, 2, 0);
        assert!(apply_frame(&mut state, &ready));
        assert!(!state.snapshot.active);
        assert!(
            state
                .snapshot
                .problem
                .as_deref()
                .unwrap_or_default()
                .contains("pipe version 1")
        );
    }

    #[test]
    fn a_refused_start_leaves_the_switch_off() {
        let mut state = started();
        let request = state.take_request(PendingAction::Start);
        let reply = header(KIND_REPLY, request, 0x8007_0005u32, 2, 0);
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
        let reply = header(KIND_REPLY, request, 0, 2, 0);
        apply_frame(&mut state, &reply);
        assert!(state.snapshot.problem.is_none());
        assert!(state.snapshot.active);
    }

    #[test]
    fn a_failed_mirror_stops_the_output_and_keeps_the_delay() {
        let mut state = started();
        let delay = header(KIND_DELAY, MIRROR_ID, 42_000, 2, 0);
        apply_frame(&mut state, &delay);
        assert_eq!(state.snapshot.delay_us, 42_000);
        let failure = header(KIND_FAILURE, MIRROR_ID, 0x8889_0004u32, 2, 0);
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
