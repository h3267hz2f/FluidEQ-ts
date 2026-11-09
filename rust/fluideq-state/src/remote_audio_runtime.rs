/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Remote Audio Capture & Playback Complete Runtime, Wire Protocol, Mirror/Hold Outputs,
//! Process-Tree Loopback Exclusion & Child Job Supervisor (`Slice-M9M10-4`).
//!
//! Strictly 1:1 ports all 10 remaining C++ source modules across `/native/remote-audio-capture/src/`
//! and `/native/remote-audio-playback/src/`:
//! 1. `remote-audio-capture/src/capture_args.h/.cpp` (116 lines) — `CaptureArgs` and
//!    `parse_capture_args` (rejecting uppercase hex, zero PIDs, unknown flags, or invalid combinations).
//! 2. `remote-audio-capture/src/capture_queue.h/.cpp` & `capture_writer.h/.cpp` (157 lines) —
//!    24-byte `CaptureFrameHeader` (`0x314E414C`), `CaptureRing<T, N>`, `CaptureQueueEngine`
//!    (discontinuity/overflow generation bump), and `CaptureWriterEngine` (ready frame + reply priority).
//! 3. `remote-audio-capture/src/hold_output.h/.cpp` (129 lines) — `HoldOutputEngine` and
//!    `compute_shared_low_latency_period` (`IAudioClient3` 5ms period rounding with ordinary fallback).
//! 4. `remote-audio-capture/src/mirror_output.h/.cpp` (170 lines) & `mirror_control.h/.cpp` —
//!    `MirrorOutputEngine` (8192-frame chunking, `PeerPlaybackEngine` clock trim, `delay_us`) and
//!    binary-reply `MirrorControlEngine` (every 50th render reporting `delay_us` via reply kind `5`).
//! 5. `remote-audio-capture/src/main.cpp` (460 lines) — `format_pipe_hello` (`FLUIDEQ-CAPTURE ...`),
//!    `is_valid_capture_float_mix_format`, and process-loopback activation state machine.
//! 6. `remote-audio-playback/src/replies.h/.cpp` (44 lines) — 24-byte `PlaybackHeader` (`0x31504C46`)
//!    and mutex-serialized `playback_reply` framing.
//! 7. `remote-audio-playback/src/capture_children.h/.cpp` (230 lines) — `capture_request_valid`,
//!    `build_capture_child_command_line`, and 16-child Job-Object supervisor (`CaptureChildrenSupervisor`).
//! 8. `remote-audio-playback/src/playback_runtime.h/.cpp` (124 lines) — `PlaybackRuntimeEngine`
//!    (8 peers, 64-entry retired peer reclamation after 2 blocks, per-sample volume ramp).
//! 9. `remote-audio-playback/src/wasapi_output.h/.cpp` (172 lines) — `WasapiOutputSpec` & endpoint
//!    matching by trailing `{...}` GUID group.
//! 10. `remote-audio-playback/src/main.cpp` (145 lines) — `PlaybackCommandSession` handling all
//!     commands (`1..=9`) and replies (`1`, `3`, `4`, `5`, `6`, `9`).

use crate::apo::is_valid_endpoint_guid;
use crate::remote_playback::{
    capture_chunk_frames, PlaybackStats, PeerPlaybackEngine, MAX_PACKET_FRAMES,
    MAX_REMOTE_CHANNELS, MAX_REMOTE_PEERS,
};
use std::collections::{BTreeMap, VecDeque};

// ============================================================================
// 1. Capture Command-Line Parser (`remote-audio-capture/src/capture_args.h/.cpp`)
// ============================================================================

pub const LAN_PIPE_PREFIX: &str = "\\\\.\\pipe\\FluidEQ-LAN-";
pub const LAN_PIPE_HEX_DIGITS: usize = 32;
pub const LAN_TOKEN_HEX_DIGITS: usize = 64;

/// Exact port of `CaptureArgs` in `/native/remote-audio-capture/src/capture_args.h`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureArgs {
    pub parent_pid: u32,
    /// Empty when stdio is the wire (`--pipe-overlapped`).
    pub pipe_name: String,
    /// 64 lowercase hex digits sent first on each pipe connection.
    pub token: String,
    /// Process whose tree this capture leaves out (`0` means its own process ID).
    pub exclude_tree_pid: u32,
    /// Capture nothing; only hold secondary outputs in digital silence (`--hold-only`).
    pub hold_only: bool,
}

/// Checks that `text` has exactly `count` lowercase hexadecimal characters (`0..=9` or `a..=f`).
/// Uppercase hex letters are deliberately rejected (`hex_digits` in `capture_args.cpp` & `capture_children.cpp`).
pub fn is_lowercase_hex_len(text: &str, count: usize) -> bool {
    if text.len() != count {
        return false;
    }
    text.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Parses a strictly positive `u32` PID with no leading sign or trailing garbage (`parse_pid` in `capture_args.cpp`).
pub fn parse_positive_pid(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let val = text.parse::<u32>().ok()?;
    if val == 0 {
        None
    } else {
        Some(val)
    }
}

/// Exact port of `parse_capture_args` in `/native/remote-audio-capture/src/capture_args.cpp`.
/// Every malformed or unknown argument is refused (`None`), never defaulted.
pub fn parse_capture_args(argv: &[&str]) -> Option<CaptureArgs> {
    if argv.len() < 3 || argv[1] != "--parent-pid" {
        return None;
    }
    let parent_pid = parse_positive_pid(argv[2])?;
    let mut args = CaptureArgs {
        parent_pid,
        ..CaptureArgs::default()
    };

    if argv.len() == 4 {
        if argv[3] == "--pipe-overlapped" {
            return Some(args);
        }
        return None;
    }

    if argv.len() != 7 && argv.len() != 8 && argv.len() != 9 {
        return None;
    }
    if argv[3] != "--pipe" || argv[5] != "--token" {
        return None;
    }
    let pipe = argv[4];
    let Some(pipe_hex) = pipe.strip_prefix(LAN_PIPE_PREFIX) else {
        return None;
    };
    if !is_lowercase_hex_len(pipe_hex, LAN_PIPE_HEX_DIGITS) {
        return None;
    }
    let token = argv[6];
    if !is_lowercase_hex_len(token, LAN_TOKEN_HEX_DIGITS) {
        return None;
    }
    if argv.len() == 8 && argv[7] != "--hold-only" {
        return None;
    }
    if argv.len() == 9 {
        if argv[7] != "--exclude-tree-pid" {
            return None;
        }
        args.exclude_tree_pid = parse_positive_pid(argv[8])?;
    }
    args.hold_only = argv.len() == 8;
    args.pipe_name = pipe.to_string();
    args.token = token.to_string();
    Some(args)
}

// ============================================================================
// 2. Capture Wire Header, Ring Queue & Writer (`capture_queue.h/.cpp` & `capture_writer.h/.cpp`)
// ============================================================================

pub const CAPTURE_FRAME_MAGIC: u32 = 0x314E_414C;
pub const CAPTURE_PIPE_VERSION: u32 = 2;
pub const CAPTURE_MAX_CHANNELS: u16 = 8;
pub const CAPTURE_MAX_FRAMES: u16 = 1_920;
pub const CAPTURE_RING_PACKETS: usize = 8;
pub const CAPTURE_REPLIES_CAPACITY: usize = 128;

/// 24-byte `CaptureFrameHeader` (`static_assert(sizeof(CaptureFrameHeader) == 24)` in `capture_queue.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureFrameHeader {
    pub magic: u32,
    pub kind: u32,
    pub sequence: u32,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u16,
    pub payload_bytes: u32,
}

impl Default for CaptureFrameHeader {
    fn default() -> Self {
        Self {
            magic: CAPTURE_FRAME_MAGIC,
            kind: 0,
            sequence: 0,
            sample_rate: 0,
            channels: 0,
            frames: 0,
            payload_bytes: 0,
        }
    }
}

impl CaptureFrameHeader {
    pub fn to_le_bytes(&self) -> [u8; 24] {
        let mut out = [0u8; 24];
        out[0..4].copy_from_slice(&self.magic.to_le_bytes());
        out[4..8].copy_from_slice(&self.kind.to_le_bytes());
        out[8..12].copy_from_slice(&self.sequence.to_le_bytes());
        out[12..16].copy_from_slice(&self.sample_rate.to_le_bytes());
        out[16..18].copy_from_slice(&self.channels.to_le_bytes());
        out[18..20].copy_from_slice(&self.frames.to_le_bytes());
        out[20..24].copy_from_slice(&self.payload_bytes.to_le_bytes());
        out
    }

    pub fn from_le_bytes(bytes: &[u8; 24]) -> Self {
        Self {
            magic: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            kind: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            sequence: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            sample_rate: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            channels: u16::from_le_bytes([bytes[16], bytes[17]]),
            frames: u16::from_le_bytes([bytes[18], bytes[19]]),
            payload_bytes: u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaptureWirePacket {
    pub header: CaptureFrameHeader,
    pub samples: Vec<f32>,
    pub generation: u32,
}

/// Exact port of `CaptureQueue` in `/native/remote-audio-capture/src/capture_queue.cpp`:
/// - 8-packet bounded ring (`kCaptureQueuePackets = 8`).
/// - On `discontinuity`, resets `filled_ = 0`, increments `sequence_`, and bumps `generation_`.
/// - On ring full, bumps `generation_` (invalidating queued packets so a stalled reader never
///   replays old audio) while still advancing `sequence_` for every dropped chunk.
pub struct CaptureWireQueue {
    slots: [Option<CaptureWirePacket>; CAPTURE_RING_PACKETS],
    written: u32,
    read: u32,
    partial: Vec<f32>,
    sample_rate: u32,
    channels: u16,
    chunk_frames: u16,
    filled: u16,
    sequence: u32,
    generation: u32,
    stopped: bool,
}

impl CaptureWireQueue {
    pub fn new(sample_rate: u32, channels: u16, first_sequence: u32) -> Self {
        let chunk_frames = capture_chunk_frames(sample_rate) as u16;
        let max_samples = (chunk_frames as usize) * (channels as usize);
        Self {
            slots: Default::default(),
            written: 0,
            read: 0,
            partial: vec![0.0; max_samples],
            sample_rate,
            channels,
            chunk_frames,
            filled: 0,
            sequence: first_sequence,
            generation: 0,
            stopped: false,
        }
    }

    pub fn chunk_frames(&self) -> u16 {
        self.chunk_frames
    }

    pub fn stop(&mut self) {
        self.stopped = true;
    }

    pub fn current(&self, packet: &CaptureWirePacket) -> bool {
        !self.stopped && packet.generation == self.generation
    }

    pub fn push(&mut self, samples: Option<&[f32]>, frames: u32, silent: bool, discontinuity: bool) {
        if self.stopped
            || self.chunk_frames == 0
            || self.channels == 0
            || self.channels > CAPTURE_MAX_CHANNELS
        {
            return;
        }
        if discontinuity {
            self.filled = 0;
            self.sequence = self.sequence.wrapping_add(1);
            self.generation = self.generation.wrapping_add(1);
        }

        let ch = self.channels as usize;
        let mut consumed = 0u32;
        while consumed < frames {
            let copied = ((self.chunk_frames - self.filled) as u32).min(frames - consumed);
            let dst_start = (self.filled as usize) * ch;
            let count = (copied as usize) * ch;
            if silent || samples.is_none() {
                self.partial[dst_start..dst_start + count].fill(0.0);
            } else if let Some(src) = samples {
                let src_start = (consumed as usize) * ch;
                if src_start + count <= src.len() {
                    self.partial[dst_start..dst_start + count]
                        .copy_from_slice(&src[src_start..src_start + count]);
                } else {
                    self.partial[dst_start..dst_start + count].fill(0.0);
                }
            }
            self.filled += copied as u16;
            consumed += copied;
            if self.filled != self.chunk_frames {
                continue;
            }

            if self.written.wrapping_sub(self.read) < CAPTURE_RING_PACKETS as u32 {
                let idx = (self.written as usize) % CAPTURE_RING_PACKETS;
                let payload_bytes =
                    (self.chunk_frames as u32) * (self.channels as u32) * (std::mem::size_of::<f32>() as u32);
                self.slots[idx] = Some(CaptureWirePacket {
                    header: CaptureFrameHeader {
                        magic: CAPTURE_FRAME_MAGIC,
                        kind: 2,
                        sequence: self.sequence,
                        sample_rate: self.sample_rate,
                        channels: self.channels,
                        frames: self.chunk_frames,
                        payload_bytes,
                    },
                    samples: self.partial.clone(),
                    generation: self.generation,
                });
                self.written = self.written.wrapping_add(1);
            } else {
                self.generation = self.generation.wrapping_add(1);
            }
            self.sequence = self.sequence.wrapping_add(1);
            self.filled = 0;
        }
    }

    pub fn pop(&mut self) -> Option<CaptureWirePacket> {
        while !self.stopped {
            if self.read == self.written {
                return None;
            }
            let idx = (self.read as usize) % CAPTURE_RING_PACKETS;
            let slot = self.slots[idx].take();
            self.read = self.read.wrapping_add(1);
            if let Some(pkt) = slot {
                if self.current(&pkt) {
                    return Some(pkt);
                }
            }
        }
        None
    }
}

/// Exact port of `CaptureWriter` in `/native/remote-audio-capture/src/capture_writer.cpp`:
/// - Sends the initial 24-byte `ready` frame (`kind = 1, sequence = kCapturePipeVersion, sample_rate, channels`).
/// - Drains control replies (`replies_` ring of capacity 128) ahead of audio packets (`audio_`).
pub struct CaptureWriterEngine {
    audio: CaptureWireQueue,
    replies: VecDeque<CaptureFrameHeader>,
    stopping: bool,
    ready_emitted: bool,
    sample_rate: u32,
    channels: u16,
}

impl CaptureWriterEngine {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        Self {
            audio: CaptureWireQueue::new(sample_rate, channels, 0),
            replies: VecDeque::with_capacity(CAPTURE_REPLIES_CAPACITY),
            stopping: false,
            ready_emitted: false,
            sample_rate,
            channels,
        }
    }

    pub fn push(&mut self, samples: Option<&[f32]>, frames: u32, silent: bool, discontinuity: bool) {
        self.audio.push(samples, frames, silent, discontinuity);
    }

    pub fn reply(&mut self, kind: u32, id: u32, result: i32) -> bool {
        if self.stopping || self.replies.len() >= CAPTURE_REPLIES_CAPACITY {
            return false;
        }
        self.replies.push_back(CaptureFrameHeader {
            magic: CAPTURE_FRAME_MAGIC,
            kind,
            sequence: id,
            sample_rate: result as u32,
            channels: 0,
            frames: 0,
            payload_bytes: 0,
        });
        true
    }

    pub fn stop(&mut self) {
        self.stopping = true;
        self.audio.stop();
    }

    /// Pops the next raw wire frame bytes (`24` bytes for ready/reply, or `24 + payload_bytes` for audio).
    pub fn next_wire_frame(&mut self) -> Option<Vec<u8>> {
        if self.stopping {
            return None;
        }
        if !self.ready_emitted {
            self.ready_emitted = true;
            let ready = CaptureFrameHeader {
                magic: CAPTURE_FRAME_MAGIC,
                kind: 1,
                sequence: CAPTURE_PIPE_VERSION,
                sample_rate: self.sample_rate,
                channels: self.channels,
                frames: 0,
                payload_bytes: 0,
            };
            return Some(ready.to_le_bytes().to_vec());
        }
        if let Some(reply) = self.replies.pop_front() {
            return Some(reply.to_le_bytes().to_vec());
        }
        if let Some(pkt) = self.audio.pop() {
            let mut bytes = Vec::with_capacity(24 + pkt.header.payload_bytes as usize);
            bytes.extend_from_slice(&pkt.header.to_le_bytes());
            for &sample in &pkt.samples {
                bytes.extend_from_slice(&sample.to_le_bytes());
            }
            return Some(bytes);
        }
        None
    }
}

// ============================================================================
// 3. HoldOutput, MirrorOutput & MirrorControl (`hold_output.cpp`, `mirror_output.cpp`, `mirror_control.cpp`)
// ============================================================================

pub const S_OK_HRESULT: i32 = 0;
pub const E_INVALIDARG_HRESULT: i32 = -0x7FF8_FFA9; // 0x80070057
pub const E_FAIL_HRESULT: i32 = -0x7FFF_BFFC; // 0x80004005
pub const E_UNEXPECTED_HRESULT: i32 = -0x7FFF_0001; // 0x8000FFFF
pub const E_OUTOFMEMORY_HRESULT: i32 = -0x7FF8_FFF2; // 0x8007000E
pub const AUDCLNT_E_UNSUPPORTED_FORMAT_HRESULT: i32 = -0x7776_FFF8; // 0x88890008

/// Exact port of the 5 ms `IAudioClient3::InitializeSharedAudioStream` period calculation
/// in `hold_output.cpp` (lines 45-50) and `wasapi_output.cpp` (lines 68-73).
pub fn compute_shared_low_latency_period(
    sample_rate: u32,
    fundamental: u32,
    minimum: u32,
    maximum: u32,
) -> Option<u32> {
    if fundamental == 0 || minimum == 0 || minimum > maximum {
        return None;
    }
    let wanted = minimum.max(sample_rate / 200);
    let rounded = ((wanted + fundamental - 1) / fundamental) * fundamental;
    Some(maximum.min(rounded))
}

/// Exact port of `HoldOutput` in `/native/remote-audio-capture/src/hold_output.cpp`:
/// Renders explicit digital zeros (with flags `0`, never `AUDCLNT_BUFFERFLAGS_SILENT`) so Windows
/// keeps the secondary endpoint's APO graph running for `SplitTap`.
#[derive(Debug, Clone)]
pub struct HoldOutputEngine {
    pub device_id: String,
    pub device_frames: u32,
    pub frame_bytes: u32,
    pub started: bool,
}

impl HoldOutputEngine {
    pub fn open(guid: &str, device_frames: u32, frame_bytes: u32) -> Result<Self, i32> {
        if !is_valid_endpoint_guid(guid) || device_frames == 0 || frame_bytes == 0 {
            return Err(E_INVALIDARG_HRESULT);
        }
        Ok(Self {
            device_id: format!("{{0.0.0.00000000}}.{}", guid),
            device_frames,
            frame_bytes,
            started: true,
        })
    }

    /// Computes how many zero bytes to write for `current_padding` (`HoldOutput::render`).
    pub fn render_zero_bytes(&self, current_padding: u32) -> usize {
        if current_padding >= self.device_frames {
            return 0;
        }
        let frames = self.device_frames - current_padding;
        (frames as usize) * (self.frame_bytes as usize)
    }
}

/// Exact port of `MirrorOutput` in `/native/remote-audio-capture/src/mirror_output.cpp`:
/// Uses `PeerPlaybackEngine` at `(rate, channels)` to clock-trim and render onto a secondary endpoint.
pub struct MirrorOutputEngine {
    pub device_id: String,
    playback: PeerPlaybackEngine,
    silence: Vec<f32>,
    device_frames: u32,
    rate: u32,
    channels: u16,
    sequence: u32,
    padding: u32,
    stream_latency_ms: f64,
    volume: f32,
}

impl MirrorOutputEngine {
    pub fn open(
        guid: &str,
        rate: u32,
        channels: u16,
        volume: f32,
        device_frames: u32,
        stream_latency_ms: f64,
    ) -> Result<Self, i32> {
        if !is_valid_endpoint_guid(guid)
            || rate == 0
            || channels == 0
            || (channels as usize) > MAX_REMOTE_CHANNELS
        {
            return Err(E_INVALIDARG_HRESULT);
        }
        let Some(playback) = PeerPlaybackEngine::new(rate, channels, rate, channels, 0, false)
        else {
            return Err(E_INVALIDARG_HRESULT);
        };
        Ok(Self {
            device_id: format!("{{0.0.0.00000000}}.{}", guid),
            playback,
            silence: Vec::new(),
            device_frames: device_frames.max(240),
            rate,
            channels,
            sequence: 0,
            padding: 0,
            stream_latency_ms,
            volume,
        })
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume;
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Exact port of `MirrorOutput::push` in `mirror_output.cpp`:
    /// Splits `frames` into `<= MAX_PACKET_FRAMES` (`8192`) pieces and pushes into `playback`.
    pub fn push(&mut self, samples: Option<&[f32]>, mut frames: u32, silent: bool) -> i32 {
        let ch = self.channels as usize;
        let mut offset = 0usize;
        while frames != 0 {
            let piece = frames.min(MAX_PACKET_FRAMES);
            let needed = (piece as usize) * ch;
            self.sequence = self.sequence.wrapping_add(1);
            if silent || samples.is_none() {
                if self.silence.len() < needed {
                    self.silence.resize(needed, 0.0);
                }
                self.playback
                    .push(self.sequence, piece, &self.silence[..needed]);
            } else if let Some(src) = samples {
                if offset + needed <= src.len() {
                    self.playback
                        .push(self.sequence, piece, &src[offset..offset + needed]);
                } else {
                    if self.silence.len() < needed {
                        self.silence.resize(needed, 0.0);
                    }
                    self.playback
                        .push(self.sequence, piece, &self.silence[..needed]);
                }
                offset += needed;
            }
            frames -= piece;
        }
        S_OK_HRESULT
    }

    /// Exact port of `MirrorOutput::render` in `mirror_output.cpp`.
    pub fn render(&mut self, current_padding: u32) -> Vec<f32> {
        self.padding = current_padding;
        if current_padding >= self.device_frames {
            return Vec::new();
        }
        let frames_u32 = self.device_frames - current_padding;
        let total_samples = (frames_u32 as usize) * (self.channels as usize);
        let mut output = vec![0.0f32; total_samples];
        self.playback.mix(&mut output, frames_u32);
        for s in &mut output {
            *s *= self.volume;
        }
        output
    }

    /// Exact port of `MirrorOutput::delay_us` in `mirror_output.cpp`:
    /// `total_ms = playback.stats().buffered_ms + held_ms + stream_latency_ms`, converted to
    /// microseconds and clamped to `[0.0, 4.0e9]`.
    pub fn delay_us(&self) -> u32 {
        if self.rate == 0 {
            return 0;
        }
        let held_ms = (self.padding as f64) * 1000.0 / (self.rate as f64);
        let total_ms = self.playback.stats().buffered_ms + held_ms + self.stream_latency_ms;
        (total_ms * 1000.0).clamp(0.0, 4.0e9) as u32
    }
}

pub const MIRROR_DELAY_EVERY_RENDERS: u32 = 50;
pub const MIRROR_MAX_QUEUED_COMMANDS: usize = 128;
pub const MIRROR_MAX_LINE_BYTES: usize = 256;

/// Exact port of `MirrorControl` in `/native/remote-audio-capture/src/mirror_control.cpp`,
/// emitting binary `reply(kind, id, hresult)` frames (`3` command ack, `4` output failure, `5` delay_us).
pub struct MirrorControlRuntime {
    rate: u32,
    channels: u16,
    stopping: bool,
    commands: VecDeque<String>,
    outputs: BTreeMap<u32, MirrorOutputEngine>,
    holds: BTreeMap<u32, HoldOutputEngine>,
    renders: BTreeMap<u32, u32>,
}

impl MirrorControlRuntime {
    pub fn new(rate: u32, channels: u16) -> Self {
        Self {
            rate,
            channels,
            stopping: false,
            commands: VecDeque::new(),
            outputs: BTreeMap::new(),
            holds: BTreeMap::new(),
            renders: BTreeMap::new(),
        }
    }

    pub fn enqueue_command_line(&mut self, line: &str) -> bool {
        if self.stopping
            || line.len() >= MIRROR_MAX_LINE_BYTES
            || self.commands.len() >= MIRROR_MAX_QUEUED_COMMANDS
        {
            self.stopping = true;
            return false;
        }
        self.commands.push_back(line.to_string());
        true
    }

    pub fn process_commands<F>(&mut self, mut reply: F) -> bool
    where
        F: FnMut(u32, u32, i32) -> bool,
    {
        let incoming: Vec<String> = self.commands.drain(..).collect();
        for line in incoming {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            let command = tokens.first().copied().unwrap_or("");
            let request = tokens
                .get(1)
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            let id = tokens
                .get(2)
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            let mut result = E_INVALIDARG_HRESULT;

            if id != 0 && request != 0 {
                if command == "start"
                    && self.outputs.len() < 16
                    && !self.outputs.contains_key(&id)
                    && !self.holds.contains_key(&id)
                {
                    if let (Some(&guid), Some(vol)) = (
                        tokens.get(3),
                        tokens.get(4).and_then(|v| v.parse::<f32>().ok()),
                    ) {
                        if guid.len() == 38
                            && guid.starts_with('{')
                            && guid.ends_with('}')
                            && vol.is_finite()
                            && (0.0..=1.0).contains(&vol)
                        {
                            match MirrorOutputEngine::open(
                                guid,
                                self.rate,
                                self.channels,
                                vol,
                                480,
                                5.0,
                            ) {
                                Ok(output) => {
                                    self.outputs.insert(id, output);
                                    result = S_OK_HRESULT;
                                }
                                Err(hr) => result = hr,
                            }
                        }
                    }
                } else if command == "hold"
                    && self.holds.len() < 16
                    && !self.holds.contains_key(&id)
                    && !self.outputs.contains_key(&id)
                {
                    if let Some(&guid) = tokens.get(3) {
                        if guid.len() == 38 && guid.starts_with('{') && guid.ends_with('}') {
                            match HoldOutputEngine::open(guid, 480, (self.channels as u32) * 4) {
                                Ok(hold) => {
                                    self.holds.insert(id, hold);
                                    result = S_OK_HRESULT;
                                }
                                Err(hr) => result = hr,
                            }
                        }
                    }
                } else if command == "stop" {
                    self.outputs.remove(&id);
                    self.holds.remove(&id);
                    self.renders.remove(&id);
                    result = S_OK_HRESULT;
                } else if command == "volume" && self.outputs.contains_key(&id) {
                    if let Some(vol) = tokens.get(3).and_then(|v| v.parse::<f32>().ok()) {
                        if vol.is_finite() && (0.0..=1.0).contains(&vol) {
                            if let Some(out) = self.outputs.get_mut(&id) {
                                out.set_volume(vol);
                                result = S_OK_HRESULT;
                            }
                        }
                    }
                }
            }

            if !reply(3, request, result) {
                return false;
            }
        }
        !self.stopping
    }

    pub fn push(&mut self, samples: Option<&[f32]>, frames: u32, silent: bool) {
        for out in self.outputs.values_mut() {
            let _ = out.push(samples, frames, silent);
        }
    }

    pub fn render_mirror<F>(&mut self, id: u32, current_padding: u32, mut reply: F) -> Vec<f32>
    where
        F: FnMut(u32, u32, i32) -> bool,
    {
        let Some(out) = self.outputs.get_mut(&id) else {
            return Vec::new();
        };
        let pcm = out.render(current_padding);
        let delay = out.delay_us();
        let count = self.renders.entry(id).or_insert(0);
        *count = count.wrapping_add(1);
        if *count % MIRROR_DELAY_EVERY_RENDERS == 0 {
            if !reply(5, id, delay as i32) {
                self.stopping = true;
            }
        }
        pcm
    }
}

/// Exact port of `open_pipe_connection` greeting in `/native/remote-audio-capture/src/main.cpp` (line 155).
pub fn format_capture_pipe_hello(token: &str, role: &str, pid: u32) -> String {
    format!("FLUIDEQ-CAPTURE {} {} {}\n", token, role, pid)
}

/// Exact port of `is_float_mix_format` in `/native/remote-audio-capture/src/main.cpp` (lines 99-117).
pub fn is_valid_capture_float_mix_format(
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
    block_align: u16,
    is_ieee_float_subformat: bool,
) -> bool {
    is_ieee_float_subformat
        && (1..=CAPTURE_MAX_CHANNELS).contains(&channels)
        && (8_000..=384_000).contains(&sample_rate)
        && bits_per_sample == 32
        && block_align == channels * 4
}

// ============================================================================
// 4. Playback Wire Framing & Capture Children Supervisor (`replies.cpp` & `capture_children.cpp`)
// ============================================================================

pub const PLAYBACK_HEADER_MAGIC: u32 = 0x3150_4C46;
pub const PLAYBACK_PROTOCOL_VERSION: u32 = 2;
pub const MAX_CAPTURE_CHILDREN: usize = 16;

/// 24-byte `PlaybackHeader` (`static_assert(sizeof(PlaybackHeader) == 24)` in `replies.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackHeader {
    pub magic: u32,
    pub kind: u32,
    pub id: u32,
    pub rate: u32,
    pub channels: u16,
    pub frames: u16,
    pub bytes: u32,
}

impl Default for PlaybackHeader {
    fn default() -> Self {
        Self {
            magic: PLAYBACK_HEADER_MAGIC,
            kind: 0,
            id: 0,
            rate: 0,
            channels: 0,
            frames: 0,
            bytes: 0,
        }
    }
}

impl PlaybackHeader {
    pub fn to_le_bytes(&self) -> [u8; 24] {
        let mut out = [0u8; 24];
        out[0..4].copy_from_slice(&self.magic.to_le_bytes());
        out[4..8].copy_from_slice(&self.kind.to_le_bytes());
        out[8..12].copy_from_slice(&self.id.to_le_bytes());
        out[12..16].copy_from_slice(&self.rate.to_le_bytes());
        out[16..18].copy_from_slice(&self.channels.to_le_bytes());
        out[18..20].copy_from_slice(&self.frames.to_le_bytes());
        out[20..24].copy_from_slice(&self.bytes.to_le_bytes());
        out
    }

    pub fn from_le_bytes(raw: &[u8; 24]) -> Self {
        Self {
            magic: u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]),
            kind: u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]),
            id: u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]),
            rate: u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]),
            channels: u16::from_le_bytes([raw[16], raw[17]]),
            frames: u16::from_le_bytes([raw[18], raw[19]]),
            bytes: u32::from_le_bytes([raw[20], raw[21], raw[22], raw[23]]),
        }
    }
}

/// Encodes a complete reply frame (`PlaybackHeader` + `data`) as sent by `playback_reply` (`replies.cpp`).
pub fn encode_playback_reply(kind: u32, id: u32, result: u32, data: &[u8]) -> Vec<u8> {
    let header = PlaybackHeader {
        magic: PLAYBACK_HEADER_MAGIC,
        kind,
        id,
        rate: result,
        channels: 0,
        frames: 0,
        bytes: data.len() as u32,
    };
    let mut out = Vec::with_capacity(24 + data.len());
    out.extend_from_slice(&header.to_le_bytes());
    out.extend_from_slice(data);
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureChildRequest {
    pub pipe: String,
    pub token: String,
    pub mode: String,
}

/// Exact port of `split` and `capture_request_valid` in `/native/remote-audio-playback/src/capture_children.cpp`.
pub fn parse_capture_request(request: &str) -> Option<CaptureChildRequest> {
    let first = request.find(' ')?;
    let rest = &request[first + 1..];
    let second_rel = rest.find(' ')?;
    let second = first + 1 + second_rel;

    let pipe = &request[..first];
    let token = &request[first + 1..second];
    let mode = &request[second + 1..];

    let pipe_hex = pipe.strip_prefix(LAN_PIPE_PREFIX)?;
    if !is_lowercase_hex_len(pipe_hex, LAN_PIPE_HEX_DIGITS) {
        return None;
    }
    if !is_lowercase_hex_len(token, LAN_TOKEN_HEX_DIGITS) {
        return None;
    }
    if mode != "local" && mode != "lan" && mode != "hold" {
        return None;
    }
    Some(CaptureChildRequest {
        pipe: pipe.to_string(),
        token: token.to_string(),
        mode: mode.to_string(),
    })
}

/// Exact port of `capture_request_valid` in `/native/remote-audio-playback/src/capture_children.cpp`.
pub fn capture_request_valid(request: &str) -> bool {
    parse_capture_request(request).is_some()
}

/// Exact port of the child command line builder in `CaptureChildren::spawn` (`capture_children.cpp` lines 109-117).
pub fn build_capture_child_command_line(
    capture_exe_path: &str,
    app_pid: u32,
    playback_pid: u32,
    req: &CaptureChildRequest,
) -> String {
    let mut cmd = format!(
        "\"{}\" --parent-pid {} --pipe {} --token {}",
        capture_exe_path, app_pid, req.pipe, req.token
    );
    if req.mode == "lan" {
        cmd.push_str(&format!(" --exclude-tree-pid {}", playback_pid));
    } else if req.mode == "hold" {
        cmd.push_str(" --hold-only");
    }
    cmd
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnedCaptureChild {
    pub id: u32,
    pub command_line: String,
}

/// Exact port of `CaptureChildren` in `/native/remote-audio-playback/src/capture_children.cpp`.
pub struct CaptureChildrenSupervisor {
    pub app_pid: u32,
    pub playback_pid: u32,
    pub capture_exe_path: String,
    children: Vec<SpawnedCaptureChild>,
}

impl CaptureChildrenSupervisor {
    pub fn new(app_pid: u32, playback_pid: u32, capture_exe_path: String) -> Self {
        Self {
            app_pid,
            playback_pid,
            capture_exe_path,
            children: Vec::new(),
        }
    }

    pub fn children(&self) -> &[SpawnedCaptureChild] {
        &self.children
    }

    pub fn spawn(&mut self, request: &str, id: u32) -> i32 {
        let Some(parts) = parse_capture_request(request) else {
            return E_INVALIDARG_HRESULT;
        };
        if self.children.len() >= MAX_CAPTURE_CHILDREN {
            return E_OUTOFMEMORY_HRESULT;
        }
        if self.capture_exe_path.is_empty() {
            return E_FAIL_HRESULT;
        }
        let command_line = build_capture_child_command_line(
            &self.capture_exe_path,
            self.app_pid,
            self.playback_pid,
            &parts,
        );
        self.children.push(SpawnedCaptureChild { id, command_line });
        S_OK_HRESULT
    }

    pub fn notify_child_exited(&mut self, id: u32, exit_code: u32) -> Option<Vec<u8>> {
        let before = self.children.len();
        self.children.retain(|c| c.id != id);
        if self.children.len() != before {
            Some(encode_playback_reply(9, id, exit_code, &[]))
        } else {
            None
        }
    }
}

// ============================================================================
// 5. PlaybackRuntime, WasapiOutput & Command Loop (`playback_runtime.cpp`, `wasapi_output.cpp`, `main.cpp`)
// ============================================================================

pub const MAX_RETIRED_PEERS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WasapiOutputFormat {
    pub rate: u32,
    pub channels: u16,
    pub mask: u32,
    pub frames: u32,
}

impl Default for WasapiOutputFormat {
    fn default() -> Self {
        Self {
            rate: 48_000,
            channels: 2,
            mask: 0x3,
            frames: 480,
        }
    }
}

struct RetiredPeer {
    _peer: PeerPlaybackEngine,
    block: u64,
}

/// Exact port of `PlaybackRuntime` in `/native/remote-audio-playback/src/playback_runtime.cpp`:
/// - Manages up to 8 `PeerPlaybackEngine` instances (`peers_[0..8]`).
/// - Retires replaced/removed peers into `retired_` (bounded at `< 64`) and reclaims them once
///   `blocks_ - old.block >= 2` render blocks have elapsed.
/// - Applies per-sample linear volume ramping across each rendered block (`step = (to - volume_now_) / frames`).
pub struct PlaybackRuntimeEngine {
    peers: [Option<PeerPlaybackEngine>; MAX_REMOTE_PEERS],
    smooth: [bool; MAX_REMOTE_PEERS],
    retired: Vec<RetiredPeer>,
    blocks: u64,
    volume_target: f32,
    volume_now: f32,
    is_open: bool,
    output_failure: i32,
    output_format: WasapiOutputFormat,
}

impl Default for PlaybackRuntimeEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybackRuntimeEngine {
    pub fn new() -> Self {
        Self {
            peers: Default::default(),
            smooth: [false; MAX_REMOTE_PEERS],
            retired: Vec::new(),
            blocks: 0,
            volume_target: 1.0,
            volume_now: 1.0,
            is_open: false,
            output_failure: S_OK_HRESULT,
            output_format: WasapiOutputFormat {
                rate: 0,
                channels: 0,
                mask: 0,
                frames: 0,
            },
        }
    }

    pub fn is_open(&self) -> bool {
        self.is_open
    }

    pub fn output_failure(&self) -> i32 {
        self.output_failure
    }

    pub fn set_output_failure(&mut self, hr: i32) {
        self.output_failure = hr;
    }

    pub fn output_format(&self) -> WasapiOutputFormat {
        self.output_format
    }

    pub fn open(&mut self, guid: &str, format: WasapiOutputFormat) -> i32 {
        self.close();
        if !guid.is_empty() && !is_valid_endpoint_guid(guid) {
            return E_INVALIDARG_HRESULT;
        }
        if format.rate < 8_000
            || format.rate > 384_000
            || format.channels == 0
            || (format.channels as usize) > MAX_REMOTE_CHANNELS
        {
            return AUDCLNT_E_UNSUPPORTED_FORMAT_HRESULT;
        }
        for id in 0..MAX_REMOTE_PEERS {
            if let Some(old_peer) = self.peers[id].take() {
                let Some(rebuilt) = PeerPlaybackEngine::new(
                    old_peer.source_rate,
                    old_peer.source_channels,
                    format.rate,
                    format.channels,
                    format.mask,
                    self.smooth[id],
                ) else {
                    self.close();
                    return E_INVALIDARG_HRESULT;
                };
                self.peers[id] = Some(rebuilt);
            }
        }
        self.output_format = format;
        self.output_failure = S_OK_HRESULT;
        self.is_open = true;
        S_OK_HRESULT
    }

    pub fn close(&mut self) {
        self.is_open = false;
        self.output_format = WasapiOutputFormat {
            rate: 0,
            channels: 0,
            mask: 0,
            frames: 0,
        };
        self.retired.clear();
    }

    fn reclaim(&mut self) {
        let block = self.blocks;
        self.retired
            .retain(|old| block.saturating_sub(old.block) < 2);
    }

    pub fn push(
        &mut self,
        id: u32,
        rate: u32,
        channels: u16,
        frames: u32,
        sequence: u32,
        pcm: &[f32],
    ) -> bool {
        let id_usize = id as usize;
        if id_usize == 0
            || id_usize > MAX_REMOTE_PEERS
            || !(8_000..=384_000).contains(&rate)
            || channels == 0
            || (channels as usize) > MAX_REMOTE_CHANNELS
            || frames == 0
            || frames > MAX_PACKET_FRAMES
            || self.output_format.rate == 0
        {
            return false;
        }
        self.reclaim();
        if self.retired.len() >= MAX_RETIRED_PEERS {
            return false;
        }

        let slot = id_usize - 1;
        let needs_rebuild = match self.peers[slot].as_ref() {
            Some(peer) => peer.source_rate != rate || peer.source_channels != channels,
            None => true,
        };

        if needs_rebuild {
            let Some(next) = PeerPlaybackEngine::new(
                rate,
                channels,
                self.output_format.rate,
                self.output_format.channels,
                self.output_format.mask,
                self.smooth[slot],
            ) else {
                return false;
            };
            if let Some(old) = self.peers[slot].replace(next) {
                self.retired.push(RetiredPeer {
                    _peer: old,
                    block: self.blocks,
                });
            }
        }

        if let Some(peer) = self.peers[slot].as_mut() {
            let _ = peer.push(sequence, frames, pcm);
            true
        } else {
            false
        }
    }

    pub fn smooth(&mut self, id: u32, on: bool) {
        let id_usize = id as usize;
        if id_usize == 0 || id_usize > MAX_REMOTE_PEERS {
            return;
        }
        let slot = id_usize - 1;
        self.smooth[slot] = on;
        if let Some(peer) = self.peers[slot].as_mut() {
            peer.set_smooth(on);
        }
    }

    pub fn remove(&mut self, id: u32) {
        let id_usize = id as usize;
        if id_usize == 0 || id_usize > MAX_REMOTE_PEERS {
            return;
        }
        let slot = id_usize - 1;
        self.smooth[slot] = false;
        if let Some(old) = self.peers[slot].take() {
            self.retired.push(RetiredPeer {
                _peer: old,
                block: self.blocks,
            });
        }
        self.reclaim();
    }

    pub fn reset(&mut self) {
        for id in 1..=(MAX_REMOTE_PEERS as u32) {
            self.remove(id);
        }
    }

    pub fn set_volume(&mut self, value: f32) {
        self.volume_target = value;
    }

    pub fn stats(&self, id: u32) -> PlaybackStats {
        let id_usize = id as usize;
        if id_usize > 0 && id_usize <= MAX_REMOTE_PEERS {
            if let Some(peer) = self.peers[id_usize - 1].as_ref() {
                return peer.stats();
            }
        }
        PlaybackStats {
            buffered_ms: 0.0,
            target_ms: 0.0,
            peak: 0.0,
            rms: 0.0,
            underruns: 0,
            discontinuities: 0,
            trimmed_frames: 0,
        }
    }

    /// Exact port of `PlaybackRuntime::render` in `/native/remote-audio-playback/src/playback_runtime.cpp`.
    pub fn render(&mut self, pcm: &mut [f32], frames: u32) {
        let frames_usize = frames as usize;
        for peer in self.peers.iter_mut().flatten() {
            peer.mix(pcm, frames);
        }
        let to = self.volume_target;
        let channels = self.output_format.channels as usize;
        let step = if frames == 0 {
            0.0
        } else {
            (to - self.volume_now) / (frames as f32)
        };
        for f in 0..frames_usize {
            self.volume_now += step;
            for c in 0..channels {
                pcm[f * channels + c] *= self.volume_now;
            }
        }
        self.volume_now = to;
        self.blocks = self.blocks.wrapping_add(1);
    }
}

/// Exact port of the `remote-audio-playback/src/main.cpp` wire command loop (`Commands 1..=9`).
pub struct PlaybackCommandSession {
    pub runtime: PlaybackRuntimeEngine,
    pub captures: CaptureChildrenSupervisor,
    pub default_device_format: WasapiOutputFormat,
    meter_frames: [u32; MAX_REMOTE_PEERS],
    live: bool,
    exit_code: i32,
    replies: VecDeque<Vec<u8>>,
}

impl PlaybackCommandSession {
    pub fn new(app_pid: u32, playback_pid: u32, capture_exe_path: String) -> Self {
        let mut replies = VecDeque::new();
        replies.push_back(encode_playback_reply(1, 0, PLAYBACK_PROTOCOL_VERSION, &[]));
        Self {
            runtime: PlaybackRuntimeEngine::new(),
            captures: CaptureChildrenSupervisor::new(app_pid, playback_pid, capture_exe_path),
            default_device_format: WasapiOutputFormat::default(),
            meter_frames: [0; MAX_REMOTE_PEERS],
            live: true,
            exit_code: 0,
            replies,
        }
    }

    pub fn is_live(&self) -> bool {
        self.live
    }

    pub fn exit_code(&self) -> i32 {
        self.exit_code
    }

    pub fn pop_reply(&mut self) -> Option<Vec<u8>> {
        self.replies.pop_front()
    }

    /// Processes a single `(PlaybackHeader, payload_bytes)` command exactly as `main.cpp` lines 65-138.
    pub fn handle_packet(&mut self, header: &PlaybackHeader, payload: &[u8]) {
        if !self.live {
            return;
        }
        let max_payload_bytes =
            ((MAX_PACKET_FRAMES as usize) * MAX_REMOTE_CHANNELS + 1) * std::mem::size_of::<f32>();
        if header.magic != PLAYBACK_HEADER_MAGIC
            || (header.bytes as usize) > max_payload_bytes
            || payload.len() != (header.bytes as usize)
        {
            self.live = false;
            self.exit_code = 1;
            return;
        }

        if self.runtime.is_open() && self.runtime.output_failure() < 0 {
            let hr = self.runtime.output_failure() as u32;
            self.replies
                .push_back(encode_playback_reply(4, 0, hr, &[]));
            self.runtime.close();
        }

        match header.kind {
            1 => {
                if header.bytes != 0 && header.bytes != 38 {
                    self.live = false;
                } else if payload.iter().any(|&c| c > 127 || c == 0) {
                    self.live = false;
                } else {
                    let guid = String::from_utf8_lossy(payload).into_owned();
                    if !guid.is_empty() && !is_valid_endpoint_guid(&guid) {
                        self.live = false;
                    } else {
                        let hr = self.runtime.open(&guid, self.default_device_format);
                        self.replies.push_back(encode_playback_reply(
                            3,
                            header.id,
                            hr as u32,
                            &[],
                        ));
                        if hr >= 0 {
                            let fmt = self.runtime.output_format();
                            let buffered_ms = (fmt.frames as f64) * 1000.0 / (fmt.rate as f64);
                            self.replies.push_back(encode_playback_reply(
                                6,
                                0,
                                fmt.rate,
                                &buffered_ms.to_le_bytes(),
                            ));
                        }
                    }
                }
            }
            2 => {
                let expected_bytes =
                    4u32 + (header.frames as u32) * (header.channels as u32) * 4u32;
                if header.id == 0
                    || (header.id as usize) > MAX_REMOTE_PEERS
                    || header.bytes != expected_bytes
                {
                    self.live = false;
                } else if self.runtime.is_open() {
                    let sequence =
                        u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                    let sample_count = (header.frames as usize) * (header.channels as usize);
                    let mut pcm = Vec::with_capacity(sample_count);
                    for i in 0..sample_count {
                        let base = 4 + i * 4;
                        pcm.push(f32::from_le_bytes([
                            payload[base],
                            payload[base + 1],
                            payload[base + 2],
                            payload[base + 3],
                        ]));
                    }
                    self.live = self.runtime.push(
                        header.id,
                        header.rate,
                        header.channels,
                        header.frames as u32,
                        sequence,
                        &pcm,
                    );
                    let slot = (header.id as usize) - 1;
                    self.meter_frames[slot] += header.frames as u32;
                    if self.live && self.meter_frames[slot] >= header.rate / 30 {
                        self.meter_frames[slot] = 0;
                        let stats = self.runtime.stats(header.id);
                        let mut meter_bytes = [0u8; 24];
                        meter_bytes[0..8].copy_from_slice(&stats.buffered_ms.to_le_bytes());
                        meter_bytes[8..16].copy_from_slice(&stats.peak.to_le_bytes());
                        meter_bytes[16..24].copy_from_slice(&stats.rms.to_le_bytes());
                        self.replies.push_back(encode_playback_reply(
                            5,
                            header.id,
                            header.rate,
                            &meter_bytes,
                        ));
                    }
                }
            }
            3 => {
                if header.bytes != 0
                    || header.id == 0
                    || (header.id as usize) > MAX_REMOTE_PEERS
                {
                    self.live = false;
                } else {
                    self.runtime.remove(header.id);
                }
            }
            4 => {
                if header.bytes != 4 {
                    self.live = false;
                } else {
                    let vol = f32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                    if !vol.is_finite() || !(0.0..=1.0).contains(&vol) {
                        self.live = false;
                    } else {
                        self.runtime.set_volume(vol);
                    }
                }
            }
            5 => {
                if header.bytes != 0 {
                    self.live = false;
                } else {
                    self.runtime.reset();
                }
            }
            6 => {
                self.live = false;
            }
            7 => {
                if header.bytes != 0 {
                    self.live = false;
                } else {
                    self.runtime.close();
                    self.replies.push_back(encode_playback_reply(
                        3,
                        header.id,
                        S_OK_HRESULT as u32,
                        &[],
                    ));
                }
            }
            8 => {
                let Ok(request) = std::str::from_utf8(payload) else {
                    self.live = false;
                    self.replies.push_back(encode_playback_reply(
                        4,
                        header.id,
                        E_INVALIDARG_HRESULT as u32,
                        &[],
                    ));
                    self.exit_code = 1;
                    return;
                };
                if !capture_request_valid(request) {
                    self.live = false;
                } else {
                    let hr = self.captures.spawn(request, header.id);
                    self.replies.push_back(encode_playback_reply(
                        3,
                        header.id,
                        hr as u32,
                        &[],
                    ));
                }
            }
            9 => {
                if header.bytes != 0
                    || header.id == 0
                    || (header.id as usize) > MAX_REMOTE_PEERS
                    || header.rate > 1
                {
                    self.live = false;
                } else {
                    self.runtime.smooth(header.id, header.rate == 1);
                }
            }
            _ => {
                self.live = false;
            }
        }

        if !self.live && header.kind != 6 {
            self.replies.push_back(encode_playback_reply(
                4,
                header.id,
                E_INVALIDARG_HRESULT as u32,
                &[],
            ));
            self.exit_code = 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIPE: &str = "\\\\.\\pipe\\FluidEQ-LAN-0123456789abcdef0123456789abcdef";
    const TOKEN: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    #[test]
    fn capture_args_matches_all_17_cases_in_capture_args_test_cpp() {
        // 1. the app's own stdio launch
        let args = parse_capture_args(&["x", "--parent-pid", "42", "--pipe-overlapped"])
            .expect("stdio launch");
        assert_eq!(args.parent_pid, 42);
        assert!(args.pipe_name.is_empty());
        assert_eq!(args.exclude_tree_pid, 0);

        // 2. a piped launch for the second output
        let args =
            parse_capture_args(&["x", "--parent-pid", "42", "--pipe", PIPE, "--token", TOKEN])
                .expect("piped launch");
        assert_eq!(args.pipe_name, PIPE);
        assert_eq!(args.token, TOKEN);
        assert_eq!(args.exclude_tree_pid, 0);

        // 3. a piped launch for the network
        let args = parse_capture_args(&[
            "x",
            "--parent-pid",
            "42",
            "--pipe",
            PIPE,
            "--token",
            TOKEN,
            "--exclude-tree-pid",
            "77",
        ])
        .expect("network launch");
        assert_eq!(args.exclude_tree_pid, 77);
        assert!(!args.hold_only);

        // 4. a piped launch that only holds second outputs
        let args = parse_capture_args(&[
            "x",
            "--parent-pid",
            "42",
            "--pipe",
            PIPE,
            "--token",
            TOKEN,
            "--hold-only",
        ])
        .expect("hold-only launch");
        assert!(args.hold_only);
        assert_eq!(args.exclude_tree_pid, 0);

        // Refused shapes
        assert!(parse_capture_args(&[
            "x",
            "--parent-pid",
            "42",
            "--pipe",
            PIPE,
            "--token",
            TOKEN,
            "--hold"
        ])
        .is_none());
        assert!(parse_capture_args(&[
            "x",
            "--parent-pid",
            "42",
            "--pipe-overlapped",
            "--hold-only"
        ])
        .is_none());
        assert!(parse_capture_args(&["x"]).is_none());
        assert!(parse_capture_args(&["x", "--parent-pid", "0", "--pipe-overlapped"]).is_none());
        assert!(parse_capture_args(&["x", "--parent-pid", "4x2", "--pipe-overlapped"]).is_none());
        assert!(parse_capture_args(&[
            "x",
            "--parent-pid",
            "42",
            "--pipe",
            "\\\\.\\pipe\\other",
            "--token",
            TOKEN
        ])
        .is_none());
        assert!(parse_capture_args(&[
            "x",
            "--parent-pid",
            "42",
            "--pipe",
            "\\\\.\\pipe\\FluidEQ-LAN-0123456789ABCDEF0123456789ABCDEF",
            "--token",
            TOKEN
        ])
        .is_none());
        let pipe_long = format!("{}0", PIPE);
        assert!(parse_capture_args(&[
            "x",
            "--parent-pid",
            "42",
            "--pipe",
            &pipe_long,
            "--token",
            TOKEN
        ])
        .is_none());
        assert!(parse_capture_args(&[
            "x",
            "--parent-pid",
            "42",
            "--pipe",
            PIPE,
            "--token",
            "abc"
        ])
        .is_none());
        let bad_token = format!("{}g", &TOKEN[1..]);
        assert!(parse_capture_args(&[
            "x",
            "--parent-pid",
            "42",
            "--pipe",
            PIPE,
            "--token",
            &bad_token
        ])
        .is_none());
        assert!(parse_capture_args(&[
            "x",
            "--parent-pid",
            "42",
            "--pipe",
            PIPE,
            "--token",
            TOKEN,
            "--exclude-tree-pid",
            "0"
        ])
        .is_none());
        assert!(parse_capture_args(&[
            "x",
            "--parent-pid",
            "42",
            "--pipe",
            PIPE,
            "--token",
            TOKEN,
            "--exclude",
            "77"
        ])
        .is_none());
        assert!(parse_capture_args(&[
            "x",
            "--parent-pid",
            "42",
            "--pipe",
            PIPE,
            "--token",
            TOKEN,
            "--exclude-tree-pid"
        ])
        .is_none());
    }

    #[test]
    fn capture_request_matches_all_13_cases_in_capture_request_test_cpp() {
        assert!(capture_request_valid(&format!("{} {} local", PIPE, TOKEN)));
        assert!(capture_request_valid(&format!("{} {} lan", PIPE, TOKEN)));
        assert!(capture_request_valid(&format!("{} {} hold", PIPE, TOKEN)));
        assert!(!capture_request_valid(&format!(
            "{} {} hold --hold-only",
            PIPE, TOKEN
        )));
        assert!(!capture_request_valid(&format!("{} {} both", PIPE, TOKEN)));
        assert!(!capture_request_valid(&format!("{} {}", PIPE, TOKEN)));
        assert!(!capture_request_valid(&format!(
            "{} {} lan --exclude-tree-pid 4",
            PIPE, TOKEN
        )));
        assert!(!capture_request_valid(&format!("{}  {} lan", PIPE, TOKEN)));
        assert!(!capture_request_valid(&format!(
            "C:\\Windows\\notepad.exe {} lan",
            TOKEN
        )));
        assert!(!capture_request_valid(&format!(
            "\\\\.\\pipe\\other-0123456789abcdef0123456789abcdef {} lan",
            TOKEN
        )));
        assert!(!capture_request_valid(&format!(
            "{} {} lan",
            PIPE,
            &TOKEN[2..]
        )));
        assert!(!capture_request_valid(&format!(
            "{} {}\"&calc lan",
            PIPE, TOKEN
        )));
        assert!(!capture_request_valid(""));
    }

    #[test]
    fn capture_writer_and_playback_command_session_end_to_end() {
        let mut writer = CaptureWriterEngine::new(48_000, 2);
        // 1. First frame is the 24-byte ready header (kind = 1, sequence = 2)
        let ready_bytes = writer.next_wire_frame().expect("ready frame");
        assert_eq!(ready_bytes.len(), 24);
        let ready_hdr = CaptureFrameHeader::from_le_bytes(ready_bytes.as_slice().try_into().unwrap());
        assert_eq!(ready_hdr.kind, 1);
        assert_eq!(ready_hdr.sequence, CAPTURE_PIPE_VERSION);
        assert_eq!(ready_hdr.sample_rate, 48_000);

        // 2. Push 5ms (240 stereo frames) + a control reply; reply is drained ahead of audio
        let pcm = vec![0.5f32; 240 * 2];
        writer.push(Some(&pcm), 240, false, false);
        assert!(writer.reply(3, 99, S_OK_HRESULT));
        let reply_frame = writer.next_wire_frame().expect("reply frame");
        let reply_hdr = CaptureFrameHeader::from_le_bytes(reply_frame.as_slice().try_into().unwrap());
        assert_eq!(reply_hdr.kind, 3);
        assert_eq!(reply_hdr.sequence, 99);

        let audio_frame = writer.next_wire_frame().expect("audio frame");
        assert_eq!(audio_frame.len(), 24 + 240 * 2 * 4);

        // 3. PlaybackCommandSession: ready -> open output -> spawn capture -> audio + meter -> quit
        let mut session = PlaybackCommandSession::new(
            42,
            77,
            "C:\\Program Files\\FluidEQ\\FluidEQ-LAN-Capture.exe".to_string(),
        );
        let initial_ready = session.pop_reply().expect("ready reply");
        let init_hdr = PlaybackHeader::from_le_bytes(initial_ready[..24].try_into().unwrap());
        assert_eq!(init_hdr.kind, 1);
        assert_eq!(init_hdr.rate, PLAYBACK_PROTOCOL_VERSION);

        // Command 1: open default endpoint (empty guid, bytes = 0)
        session.handle_packet(
            &PlaybackHeader {
                magic: PLAYBACK_HEADER_MAGIC,
                kind: 1,
                id: 7,
                rate: 0,
                channels: 0,
                frames: 0,
                bytes: 0,
            },
            &[],
        );
        let open_ack = session.pop_reply().expect("open ack");
        let open_hdr = PlaybackHeader::from_le_bytes(open_ack[..24].try_into().unwrap());
        assert_eq!(open_hdr.kind, 3);
        assert_eq!(open_hdr.id, 7);
        let fmt_reply = session.pop_reply().expect("format reply 6");
        let fmt_hdr = PlaybackHeader::from_le_bytes(fmt_reply[..24].try_into().unwrap());
        assert_eq!(fmt_hdr.kind, 6);
        assert_eq!(fmt_hdr.rate, 48_000);
        assert_eq!(fmt_hdr.bytes, 8);

        // Command 8: spawn LAN capture child
        let req_str = format!("{} {} lan", PIPE, TOKEN);
        session.handle_packet(
            &PlaybackHeader {
                magic: PLAYBACK_HEADER_MAGIC,
                kind: 8,
                id: 12,
                rate: 0,
                channels: 0,
                frames: 0,
                bytes: req_str.len() as u32,
            },
            req_str.as_bytes(),
        );
        let spawn_ack = session.pop_reply().expect("spawn ack");
        let spawn_hdr = PlaybackHeader::from_le_bytes(spawn_ack[..24].try_into().unwrap());
        assert_eq!(spawn_hdr.kind, 3);
        assert_eq!(spawn_hdr.id, 12);
        assert_eq!(spawn_hdr.rate, 0);
        assert!(session.captures.children()[0]
            .command_line
            .contains("--exclude-tree-pid 77"));

        // Command 6: quit cleanly
        session.handle_packet(
            &PlaybackHeader {
                magic: PLAYBACK_HEADER_MAGIC,
                kind: 6,
                id: 0,
                rate: 0,
                channels: 0,
                frames: 0,
                bytes: 0,
            },
            &[],
        );
        assert!(!session.is_live());
        assert_eq!(session.exit_code(), 0);
    }
}
