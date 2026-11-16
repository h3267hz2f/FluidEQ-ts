/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! `FluidEQ-LAN-Playback.exe`, kept running for the life of the app.
//!
//! It plays what other computers share with this one, and it is the parent of
//! every capture that needs its tree left out: the microphone of the network
//! share and the second output's mirror. A capture for the network must not
//! hear the sound arriving from the network, or it would travel back where it
//! came from, and Windows can only leave one process tree out of a loopback —
//! so the receiving side owns the tree that all the rendering lives in.
//!
//! One 24-byte header and its body are both command and reply, field for
//! field with `replies.h`: magic, kind, id, rate, channels, frames, bytes.

use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

use crate::capture_pipe::{self, CaptureFrame, CaptureSession};

pub const PLAYBACK_RESOURCE: &str = "engine-bin/FluidEQ-LAN-Playback.exe";
const MAGIC: u32 = 0x3150_4c46;
const HEADER_BYTES: usize = 24;
const MAX_BODY_BYTES: usize = 1_920 * 8 * 4 + 1;

pub const CMD_OPEN_OUTPUT: u32 = 1;
pub const CMD_AUDIO: u32 = 2;
pub const CMD_REMOVE_PEER: u32 = 3;
pub const CMD_VOLUME: u32 = 4;
pub const CMD_CLOSE_OUTPUT: u32 = 7;
pub const CMD_START_CAPTURE: u32 = 8;
pub const CMD_SMOOTH: u32 = 9;

const REPLY_READY: u32 = 1;
const REPLY_RESULT: u32 = 3;
const REPLY_FAILURE: u32 = 4;
const REPLY_METER: u32 = 5;
const REPLY_FORMAT: u32 = 6;
const REPLY_CHILD_ENDED: u32 = 9;

pub const EVENT: &str = "lan-playback";

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerMeter {
    pub buffered_ms: f64,
    pub peak: f64,
    pub rms: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackStatus {
    pub available: bool,
    pub ready: bool,
    pub version: u32,
    pub output_rate: u32,
    pub buffered_ms: f64,
    pub problem: Option<String>,
}

#[derive(Default)]
struct HostState {
    ready: bool,
    version: u32,
    output_rate: u32,
    buffered_ms: f64,
    meters: HashMap<u32, PeerMeter>,
    problem: Option<String>,
}

impl HostState {
    fn snapshot(&self) -> PlaybackStatus {
        PlaybackStatus {
            available: true,
            ready: self.ready,
            version: self.version,
            output_rate: self.output_rate,
            buffered_ms: self.buffered_ms,
            problem: self.problem.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    /// A mirror of the second output: rendered by the capture helper itself.
    Local,
    /// The network share: leaves the playback helper's whole tree out.
    Lan,
}

impl CaptureMode {
    fn word(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Lan => "lan",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReplyHeader {
    kind: u32,
    id: u32,
    rate: u32,
    channels: u16,
    frames: u16,
    bytes: u32,
}

impl ReplyHeader {
    fn parse(bytes: &[u8; HEADER_BYTES]) -> Option<Self> {
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        if word(0) != MAGIC {
            return None;
        }
        Some(Self {
            kind: word(4),
            id: word(8),
            rate: word(12),
            channels: u16::from_le_bytes([bytes[16], bytes[17]]),
            frames: u16::from_le_bytes([bytes[18], bytes[19]]),
            bytes: word(20),
        })
    }
}

pub fn playback_exe(app: &AppHandle) -> Option<PathBuf> {
    let path = app
        .path()
        .resolve(PLAYBACK_RESOURCE, tauri::path::BaseDirectory::Resource)
        .ok()?;
    path.is_file().then_some(path)
}

pub struct PlaybackHost {
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    state: Arc<Mutex<HostState>>,
    captures: Arc<Mutex<HashMap<u32, CaptureSession>>>,
    next_id: AtomicU32,
}

impl PlaybackHost {
    fn spawn(app: &AppHandle) -> Result<Self, String> {
        let exe = playback_exe(app).ok_or_else(|| {
            format!("{PLAYBACK_RESOURCE} is not installed beside this app")
        })?;
        let mut command = Command::new(&exe);
        command
            .arg("--parent-pid")
            .arg(std::process::id().to_string())
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
            .map_err(|error| format!("the audio helper did not start: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| String::from("the audio helper gave no input"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| String::from("the audio helper gave no output"))?;

        let state = Arc::new(Mutex::new(HostState::default()));
        let captures: Arc<Mutex<HashMap<u32, CaptureSession>>> =
            Arc::new(Mutex::new(HashMap::new()));
        {
            let state = Arc::clone(&state);
            let captures = Arc::clone(&captures);
            let app = app.clone();
            std::thread::Builder::new()
                .name(String::from("fluideq-playback-replies"))
                .spawn(move || read_replies(stdout, state, captures, app))
                .map_err(|error| format!("the audio helper could not be watched: {error}"))?;
        }

        Ok(Self {
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
            state,
            captures,
            next_id: AtomicU32::new(1),
        })
    }

    fn request_id(&self) -> u32 {
        loop {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            if id != 0 {
                return id;
            }
        }
    }

    fn command(
        &self,
        kind: u32,
        id: u32,
        rate: u32,
        channels: u16,
        frames: u16,
        payload: &[u8],
    ) -> Result<(), String> {
        let mut header = [0u8; HEADER_BYTES];
        header[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        header[4..8].copy_from_slice(&kind.to_le_bytes());
        header[8..12].copy_from_slice(&id.to_le_bytes());
        header[12..16].copy_from_slice(&rate.to_le_bytes());
        header[16..18].copy_from_slice(&channels.to_le_bytes());
        header[18..20].copy_from_slice(&frames.to_le_bytes());
        header[20..24].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|_| String::from("the audio helper is not reachable"))?;
        stdin
            .write_all(&header)
            .and_then(|()| stdin.write_all(payload))
            .and_then(|()| stdin.flush())
            .map_err(|error| format!("the audio helper is not listening: {error}"))
    }

    pub fn open_output(&self, endpoint: &str) -> Result<(), String> {
        let id = self.request_id();
        self.command(CMD_OPEN_OUTPUT, id, 0, 0, 0, endpoint.as_bytes())
    }

    pub fn close_output(&self) -> Result<(), String> {
        let id = self.request_id();
        self.command(CMD_CLOSE_OUTPUT, id, 0, 0, 0, &[])
    }

    pub fn volume(&self, volume: f64) -> Result<(), String> {
        let value = volume.clamp(0.0, 1.0) as f32;
        self.command(CMD_VOLUME, 0, 0, 0, 0, &value.to_le_bytes())
    }

    pub fn remove_peer(&self, peer: u32) -> Result<(), String> {
        self.command(CMD_REMOVE_PEER, peer, 0, 0, 0, &[])
    }

    pub fn smooth(&self, peer: u32, smooth: bool) -> Result<(), String> {
        self.command(
            CMD_SMOOTH,
            peer,
            u32::from(smooth),
            0,
            0,
            &[],
        )
    }

    /// One packet of received audio, exactly as the playback helper reads it:
    /// the packet's own sequence as a leading u32, then the Float32 samples.
    pub fn audio(
        &self,
        peer: u32,
        sequence: u32,
        sample_rate: u32,
        channels: u8,
        frames: u16,
        samples: &[f32],
    ) -> Result<(), String> {
        let expected = frames as usize * channels as usize;
        if samples.len() != expected {
            return Err(String::from("the audio packet is not the size it claims"));
        }
        let mut payload = Vec::with_capacity(4 + samples.len() * 4);
        payload.extend_from_slice(&sequence.to_le_bytes());
        for sample in samples {
            payload.extend_from_slice(&sample.to_le_bytes());
        }
        self.command(
            CMD_AUDIO,
            peer,
            sample_rate,
            u16::from(channels),
            frames,
            &payload,
        )
    }

    /// A capture helper under this host's tree. The pipe exists before the
    /// request goes out, so the helper can connect the instant it runs.
    pub fn start_capture<F, E>(
        &self,
        mode: CaptureMode,
        on_frame: F,
        on_end: E,
    ) -> Result<CaptureSession, String>
    where
        F: FnMut(&CaptureFrame) + Send + 'static,
        E: FnOnce() + Send + 'static,
    {
        let request = self.request_id();
        let captures = Arc::clone(&self.captures);
        capture_pipe::open_capture(
            on_frame,
            on_end,
            |name, token, session| {
                if let Ok(mut running) = captures.lock() {
                    running.insert(request, session.clone());
                }
                self.command(
                    CMD_START_CAPTURE,
                    request,
                    0,
                    0,
                    0,
                    format!("{name} {token} {}", mode.word()).as_bytes(),
                )
            },
        )
    }

    pub fn status(&self) -> PlaybackStatus {
        match self.state.lock() {
            Ok(state) => state.snapshot(),
            Err(_) => PlaybackStatus::default(),
        }
    }

    pub fn meter(&self, peer: u32) -> PeerMeter {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.meters.get(&peer).copied())
            .unwrap_or_default()
    }
}

impl Drop for PlaybackHost {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub struct PlaybackHostState(Mutex<Option<Arc<PlaybackHost>>>);

impl Default for PlaybackHostState {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybackHostState {
    pub fn new() -> Self {
        Self(Mutex::new(None))
    }

    pub fn ensure(&self, app: &AppHandle) -> Result<Arc<PlaybackHost>, String> {
        let mut guard = self
            .0
            .lock()
            .map_err(|_| String::from("the audio helper is not reachable"))?;
        if let Some(host) = guard.as_ref() {
            let status = host.status();
            if status.ready || status.problem.is_none() {
                return Ok(Arc::clone(host));
            }
        }
        let host = Arc::new(PlaybackHost::spawn(app)?);
        *guard = Some(Arc::clone(&host));
        Ok(host)
    }

    pub fn existing(&self) -> Option<Arc<PlaybackHost>> {
        self.0.lock().ok().and_then(|guard| guard.clone())
    }
}

fn read_replies(
    mut stdout: impl Read,
    state: Arc<Mutex<HostState>>,
    captures: Arc<Mutex<HashMap<u32, CaptureSession>>>,
    app: AppHandle,
) {
    let mut header_bytes = [0u8; HEADER_BYTES];
    let mut body = Vec::new();
    loop {
        if stdout.read_exact(&mut header_bytes).is_err() {
            break;
        }
        let Some(header) = ReplyHeader::parse(&header_bytes) else {
            break;
        };
        if header.bytes as usize > MAX_BODY_BYTES {
            break;
        }
        body.clear();
        if header.bytes > 0 {
            body.resize(header.bytes as usize, 0);
            if stdout.read_exact(&mut body).is_err() {
                break;
            }
        }
        apply_reply(&state, &captures, &header, &body);
        if let Ok(state) = state.lock() {
            let _ = app.emit(EVENT, state.snapshot());
        }
    }
    if let Ok(mut state) = state.lock() {
        state.ready = false;
        if state.problem.is_none() {
            state.problem = Some(String::from("the audio helper stopped"));
        }
        let _ = app.emit(EVENT, state.snapshot());
    }
}

fn apply_reply(
    state: &Arc<Mutex<HostState>>,
    captures: &Arc<Mutex<HashMap<u32, CaptureSession>>>,
    header: &ReplyHeader,
    body: &[u8],
) {
    let failed = (header.rate as i32) < 0;
    match header.kind {
        REPLY_READY => {
            if let Ok(mut state) = state.lock() {
                state.ready = true;
                state.version = header.rate;
                state.problem = None;
            }
        }
        REPLY_RESULT => {
            if failed {
                let session = captures
                    .lock()
                    .ok()
                    .and_then(|mut running| running.remove(&header.id));
                match session {
                    Some(session) => {
                        session.shutdown();
                        if let Ok(mut state) = state.lock() {
                            state.problem = Some(format!(
                                "the audio capture was refused (0x{:08X})",
                                header.rate
                            ));
                        }
                    }
                    None => {
                        if let Ok(mut state) = state.lock() {
                            state.problem = Some(format!(
                                "the audio output was refused (0x{:08X})",
                                header.rate
                            ));
                        }
                    }
                }
            }
        }
        REPLY_FAILURE => {
            if header.id == 0 || failed {
                if let Ok(mut state) = state.lock() {
                    state.problem = Some(format!(
                        "the audio output stopped (0x{:08X})",
                        header.rate
                    ));
                }
            }
        }
        REPLY_METER => {
            if body.len() == 24 {
                let read = |at: usize| {
                    f64::from_le_bytes(body[at..at + 8].try_into().unwrap())
                };
                if let Ok(mut state) = state.lock() {
                    state.meters.insert(
                        header.id,
                        PeerMeter {
                            buffered_ms: read(0),
                            peak: read(8),
                            rms: read(16),
                        },
                    );
                }
            }
        }
        REPLY_FORMAT => {
            let buffered = if body.len() == 8 {
                f64::from_le_bytes(body[..8].try_into().unwrap())
            } else {
                0.0
            };
            if let Ok(mut state) = state.lock() {
                state.output_rate = header.rate;
                state.buffered_ms = buffered;
            }
        }
        REPLY_CHILD_ENDED => {
            let session = captures
                .lock()
                .ok()
                .and_then(|mut running| running.remove(&header.id));
            if let Some(session) = session {
                session.shutdown();
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(kind: u32, id: u32, rate: u32, bytes: u32) -> [u8; HEADER_BYTES] {
        let mut out = [0u8; HEADER_BYTES];
        out[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        out[4..8].copy_from_slice(&kind.to_le_bytes());
        out[8..12].copy_from_slice(&id.to_le_bytes());
        out[12..16].copy_from_slice(&rate.to_le_bytes());
        out[16..18].copy_from_slice(&2u16.to_le_bytes());
        out[18..20].copy_from_slice(&480u16.to_le_bytes());
        out[20..24].copy_from_slice(&bytes.to_le_bytes());
        out
    }

    #[test]
    fn the_helper_header_is_read_field_for_field() {
        let parsed = ReplyHeader::parse(&header(REPLY_METER, 3, 48_000, 24)).unwrap();
        assert_eq!(parsed.kind, REPLY_METER);
        assert_eq!(parsed.id, 3);
        assert_eq!(parsed.rate, 48_000);
        assert_eq!(parsed.channels, 2);
        assert_eq!(parsed.frames, 480);
        assert_eq!(parsed.bytes, 24);

        let mut wrong = header(REPLY_READY, 0, 2, 0);
        wrong[0] ^= 0xff;
        assert!(ReplyHeader::parse(&wrong).is_none());
    }

    #[test]
    fn a_meter_reply_is_three_doubles() {
        let state = Arc::new(Mutex::new(HostState::default()));
        let captures = Arc::new(Mutex::new(HashMap::new()));
        let mut body = Vec::new();
        body.extend_from_slice(&12.5f64.to_le_bytes());
        body.extend_from_slice(&0.75f64.to_le_bytes());
        body.extend_from_slice(&0.25f64.to_le_bytes());
        apply_reply(
            &state,
            &captures,
            &ReplyHeader {
                kind: REPLY_METER,
                id: 2,
                rate: 48_000,
                channels: 0,
                frames: 0,
                bytes: 24,
            },
            &body,
        );
        let meter = state.lock().unwrap().meters.get(&2).copied().unwrap();
        assert_eq!(meter.buffered_ms, 12.5);
        assert_eq!(meter.peak, 0.75);
        assert_eq!(meter.rms, 0.25);
    }

    #[test]
    fn the_output_format_is_remembered() {
        let state = Arc::new(Mutex::new(HostState::default()));
        let captures = Arc::new(Mutex::new(HashMap::new()));
        apply_reply(
            &state,
            &captures,
            &ReplyHeader {
                kind: REPLY_FORMAT,
                id: 0,
                rate: 44_100,
                channels: 0,
                frames: 0,
                bytes: 8,
            },
            &6.5f64.to_le_bytes(),
        );
        let status = state.lock().unwrap().snapshot();
        assert_eq!(status.output_rate, 44_100);
        assert_eq!(status.buffered_ms, 6.5);
    }
}
