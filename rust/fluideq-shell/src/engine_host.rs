/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! The supervised `FluidEQ-DSP` host: process launch, handshake, chain
//! snapshots, crash replacement, and orderly shutdown.
//!
//! The host owns the output endpoint and the real-time thread. This module owns
//! nothing but its lifetime: it starts the bundled executable with
//! `--parent-pid`, reads the one `FEQH` handshake, writes whole `FEQC` command
//! frames, and drains the host's `FEQA` / `FEQT` / `FEQS` / `FEQN` replies so a
//! full pipe never blocks the audio process.
//!
//! A chain snapshot is the shell's only opinion about sound. Every command that
//! changes the store sends the whole `export_wire_parameters()` buffer again,
//! and the last buffer is re-sent to a replacement host, so a process that came
//! back flat cannot look like an engine ignoring the panel.

use fluideq_dsp_sys::{
    FeqWireAckFrame, FeqWireHandshake, FeqWireStatsFrame, FEQ_ABI_VERSION, FEQ_MAGIC_ACK,
    FEQ_MAGIC_ANALYSIS, FEQ_MAGIC_HANDSHAKE, FEQ_MAGIC_STATS, FEQ_MAGIC_TELEMETRY,
    FEQ_METER_BINS, FEQ_METER_DETAIL_BINS, FEQ_METER_MAX_BANDS, FEQ_METER_SCOPE_PAIRS,
    FEQ_PARAMETER_COUNT, FEQ_PARAMETER_SCHEMA_VERSION, FEQ_WIRE_PROTOCOL_VERSION,
};
use serde::Serialize;
use std::io::Read;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::host_runtime::{read_exact_from_pipe, write_frame_to_pipe, MAX_WIRE_CHAIN_PARAMS};
use crate::system_daemons::{
    compute_analysis_frame_payload_bytes, encode_dsp_host_command_frame, parse_dsp_host_ack_frame,
    parse_dsp_host_stats_frame,
};

pub const HANDSHAKE_BYTES: usize = 104;
pub const COMMAND_BYTES: usize = 32;
pub const ACK_BYTES: usize = 32;
pub const TELEMETRY_BYTES: usize = 192;
pub const STATS_BYTES: usize = 24;
pub const ANALYSIS_HEADER_BYTES: usize = 872;

/// The most restarts allowed inside [`RESTART_WINDOW`] before supervision gives up.
pub const RESTART_BUDGET: usize = 3;
/// Restarts older than this do not count against the budget.
pub const RESTART_WINDOW: Duration = Duration::from_secs(60);

/// One frame read from the host's stdout.
#[derive(Debug, Clone)]
pub enum HostFrame {
    Handshake(FeqWireHandshake),
    Ack(FeqWireAckFrame),
    Telemetry(Vec<u8>),
    Stats(FeqWireStatsFrame),
    Analysis(Vec<u8>),
}

/// Reads and validates the 104-byte `FEQH` handshake.
pub fn parse_handshake(bytes: &[u8]) -> Result<FeqWireHandshake, String> {
    if bytes.len() < HANDSHAKE_BYTES {
        return Err(format!(
            "FluidEQ-DSP: handshake is {} bytes, expected {HANDSHAKE_BYTES}",
            bytes.len()
        ));
    }
    let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    if magic != FEQ_MAGIC_HANDSHAKE {
        return Err(format!(
            "FluidEQ-DSP: handshake magic is 0x{magic:08x}, expected 0x{FEQ_MAGIC_HANDSHAKE:08x}"
        ));
    }
    let protocol_version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    if protocol_version != FEQ_WIRE_PROTOCOL_VERSION {
        return Err(format!(
            "FluidEQ-DSP: wire protocol {protocol_version} does not match {FEQ_WIRE_PROTOCOL_VERSION}"
        ));
    }
    let parameter_schema_version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    if parameter_schema_version != FEQ_PARAMETER_SCHEMA_VERSION {
        return Err(format!(
            "FluidEQ-DSP: parameter schema {parameter_schema_version} does not match {FEQ_PARAMETER_SCHEMA_VERSION}"
        ));
    }
    let abi_version = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    if abi_version != FEQ_ABI_VERSION {
        return Err(format!(
            "FluidEQ-DSP: ABI {abi_version} does not match {FEQ_ABI_VERSION}"
        ));
    }
    let parameter_count = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
    if parameter_count != FEQ_PARAMETER_COUNT as u32 {
        return Err(format!(
            "FluidEQ-DSP: the host reports {parameter_count} parameters, the shell has {FEQ_PARAMETER_COUNT}"
        ));
    }
    let analysis_frame_bytes = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    if analysis_frame_bytes as usize != ANALYSIS_HEADER_BYTES {
        return Err(format!(
            "FluidEQ-DSP: analysis frame is {analysis_frame_bytes} bytes, expected {ANALYSIS_HEADER_BYTES}"
        ));
    }
    Ok(FeqWireHandshake {
        magic,
        protocol_version,
        parameter_schema_version,
        abi_version,
        parameter_count,
        analysis_frame_bytes,
        core_version: std::array::from_fn(|index| bytes[24 + index] as c_char),
        architecture: std::array::from_fn(|index| bytes[48 + index] as c_char),
        build_revision: std::array::from_fn(|index| bytes[64 + index] as c_char),
        backend: std::array::from_fn(|index| bytes[88 + index] as c_char),
    })
}

/// The total on-wire length of a frame whose first four bytes are `magic`.
///
/// `header` must hold at least 92 bytes for `FEQN`; the other frames need only
/// the magic.
pub fn frame_bytes_needed(magic: u32, header: &[u8]) -> Result<usize, String> {
    match magic {
        FEQ_MAGIC_HANDSHAKE => Ok(HANDSHAKE_BYTES),
        FEQ_MAGIC_ACK => Ok(ACK_BYTES),
        FEQ_MAGIC_STATS => Ok(STATS_BYTES),
        FEQ_MAGIC_TELEMETRY => Ok(TELEMETRY_BYTES),
        FEQ_MAGIC_ANALYSIS => {
            if header.len() < 92 {
                return Err(format!(
                    "FluidEQ-DSP: analysis header is {} bytes, expected at least 92",
                    header.len()
                ));
            }
            let stage_mask = u32::from_le_bytes(header[8..12].try_into().unwrap());
            let bins = u32::from_le_bytes(header[12..16].try_into().unwrap());
            let pairs = u32::from_le_bytes(header[16..20].try_into().unwrap());
            let bands = u32::from_le_bytes(header[20..24].try_into().unwrap());
            let detail_bins = u32::from_le_bytes(header[88..92].try_into().unwrap());
            if bins as usize > FEQ_METER_BINS
                || pairs as usize > FEQ_METER_SCOPE_PAIRS
                || bands as usize > FEQ_METER_MAX_BANDS
                || detail_bins as usize > FEQ_METER_DETAIL_BINS
                || stage_mask.count_ones() as usize > 8
            {
                return Err(
                    "FluidEQ-DSP: analysis frame declares more bins than the engine can publish; the stream has desynchronised"
                        .to_string(),
                );
            }
            Ok(ANALYSIS_HEADER_BYTES
                + compute_analysis_frame_payload_bytes(stage_mask, bins, pairs, bands, detail_bins))
        }
        other => Err(format!(
            "FluidEQ-DSP: unknown frame magic 0x{other:08x}; the control stream has desynchronised"
        )),
    }
}

/// Reads exactly one whole frame from the host's stdout stream.
pub fn read_frame<R: Read>(reader: &mut R) -> Result<HostFrame, String> {
    let mut magic_bytes = [0u8; 4];
    if !read_exact_from_pipe(reader, &mut magic_bytes) {
        return Err(String::from("FluidEQ-DSP: the host closed its output"));
    }
    let magic = u32::from_le_bytes(magic_bytes);
    match magic {
        FEQ_MAGIC_HANDSHAKE => {
            let mut bytes = [0u8; HANDSHAKE_BYTES];
            bytes[0..4].copy_from_slice(&magic_bytes);
            if !read_exact_from_pipe(reader, &mut bytes[4..]) {
                return Err(String::from("FluidEQ-DSP: the pipe closed mid handshake"));
            }
            Ok(HostFrame::Handshake(parse_handshake(&bytes)?))
        }
        FEQ_MAGIC_ACK => {
            let mut bytes = [0u8; ACK_BYTES];
            bytes[0..4].copy_from_slice(&magic_bytes);
            if !read_exact_from_pipe(reader, &mut bytes[4..]) {
                return Err(String::from("FluidEQ-DSP: the pipe closed mid ack"));
            }
            let ack = parse_dsp_host_ack_frame(&bytes)
                .ok_or_else(|| String::from("FluidEQ-DSP: the ack frame is malformed"))?;
            Ok(HostFrame::Ack(ack))
        }
        FEQ_MAGIC_STATS => {
            let mut bytes = [0u8; STATS_BYTES];
            bytes[0..4].copy_from_slice(&magic_bytes);
            if !read_exact_from_pipe(reader, &mut bytes[4..]) {
                return Err(String::from("FluidEQ-DSP: the pipe closed mid stats"));
            }
            let stats = parse_dsp_host_stats_frame(&bytes)
                .ok_or_else(|| String::from("FluidEQ-DSP: the stats frame is malformed"))?;
            Ok(HostFrame::Stats(stats))
        }
        FEQ_MAGIC_TELEMETRY => {
            let mut bytes = vec![0u8; TELEMETRY_BYTES];
            bytes[0..4].copy_from_slice(&magic_bytes);
            if !read_exact_from_pipe(reader, &mut bytes[4..]) {
                return Err(String::from("FluidEQ-DSP: the pipe closed mid telemetry"));
            }
            Ok(HostFrame::Telemetry(bytes))
        }
        FEQ_MAGIC_ANALYSIS => {
            let mut header = [0u8; 92];
            header[0..4].copy_from_slice(&magic_bytes);
            if !read_exact_from_pipe(reader, &mut header[4..]) {
                return Err(String::from("FluidEQ-DSP: the pipe closed mid analysis header"));
            }
            let total = frame_bytes_needed(magic, &header)?;
            let mut packet = vec![0u8; total];
            packet[0..92].copy_from_slice(&header);
            if total > 92 && !read_exact_from_pipe(reader, &mut packet[92..]) {
                return Err(String::from("FluidEQ-DSP: the pipe closed mid analysis"));
            }
            Ok(HostFrame::Analysis(packet))
        }
        other => Err(format!(
            "FluidEQ-DSP: unknown frame magic 0x{other:08x}; the control stream has desynchronised"
        )),
    }
}

/// Encodes `FEQ_CMD_APPLY_CHAIN` plus its whole `f64` payload.
pub fn build_apply_chain(
    values: &[f64],
    revision: u32,
    request_id: u32,
) -> Result<Vec<u8>, String> {
    if values.is_empty() {
        return Err(String::from(
            "FluidEQ-DSP: an empty chain snapshot has nothing to apply",
        ));
    }
    if values.len() > MAX_WIRE_CHAIN_PARAMS as usize {
        return Err(format!(
            "FluidEQ-DSP: chain snapshot has {} parameters, the wire ceiling is {MAX_WIRE_CHAIN_PARAMS}",
            values.len()
        ));
    }
    if values.iter().any(|value| !value.is_finite()) {
        return Err(String::from(
            "FluidEQ-DSP: chain snapshot contains a non-finite value",
        ));
    }
    let mut frame = Vec::with_capacity(COMMAND_BYTES + values.len() * 8);
    frame.extend_from_slice(&encode_dsp_host_command_frame(
        fluideq_dsp_sys::FeqWireCommand::FEQ_CMD_APPLY_CHAIN,
        request_id,
        revision,
        values.len() as u32,
        0,
        0.0,
    ));
    for value in values {
        frame.extend_from_slice(&value.to_le_bytes());
    }
    Ok(frame)
}

/// Encodes `FEQ_CMD_START`, which opens the endpoint.
pub fn build_start(revision: u32, request_id: u32) -> [u8; COMMAND_BYTES] {
    encode_dsp_host_command_frame(
        fluideq_dsp_sys::FeqWireCommand::FEQ_CMD_START,
        request_id,
        revision,
        0,
        0,
        0.0,
    )
}

/// Encodes `FEQ_CMD_SHUTDOWN`, which stops the host without closing its pipe first.
pub fn build_shutdown(revision: u32, request_id: u32) -> [u8; COMMAND_BYTES] {
    encode_dsp_host_command_frame(
        fluideq_dsp_sys::FeqWireCommand::FEQ_CMD_SHUTDOWN,
        request_id,
        revision,
        0,
        0,
        0.0,
    )
}

/// Records a restart at `now` when the budget allows it.
///
/// Restarts older than [`RESTART_WINDOW`] are forgotten first, so a process
/// that crashes once an hour is replaced forever and a process that crashes
/// three times in a minute is left alone.
pub fn restart_allowed(restarts: &mut Vec<Instant>, now: Instant) -> bool {
    restarts.retain(|restart| now.duration_since(*restart) < RESTART_WINDOW);
    if restarts.len() >= RESTART_BUDGET {
        return false;
    }
    restarts.push(now);
    true
}

/// The supervisor's public view of the host.
#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct EngineHostStatus {
    pub state: String,
    pub pid: Option<u32>,
    pub backend: Option<String>,
    pub core_version: Option<String>,
    pub parameter_count: Option<u32>,
    pub restarts: u32,
    pub last_error: Option<String>,
}

/// The bundled DSP host executable, under the app's resource directory.
pub const ENGINE_HOST_RESOURCE: &str = "engine-bin/FluidEQ-DSP.exe";

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// How long [`EngineHost::shutdown`] waits for the orderly exit before killing.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// The last whole chain snapshot, kept for the next host that starts.
struct ChainSnapshot {
    values: Vec<f64>,
    revision: u32,
}

struct HostInner {
    exe: PathBuf,
    child: Option<Child>,
    writer: Option<mpsc::Sender<Vec<u8>>>,
    status: EngineHostStatus,
    last_chain: Option<ChainSnapshot>,
    restarts: Vec<Instant>,
    stopping: bool,
    next_request_id: u32,
}

/// Owns the supervised `FluidEQ-DSP` process.
///
/// Three threads touch a host: the caller queues command frames, a writer
/// thread owns stdin, and a reader thread owns stdout. The split is the
/// deadlock rule: the reader must keep draining while a write blocks, because
/// a host blocked on a full stdout pipe stops reading stdin.
pub struct EngineHost {
    inner: Arc<Mutex<HostInner>>,
}

impl EngineHost {
    pub fn new(exe: PathBuf) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HostInner {
                exe,
                child: None,
                writer: None,
                status: EngineHostStatus {
                    state: String::from("stopped"),
                    ..EngineHostStatus::default()
                },
                last_chain: None,
                restarts: Vec::new(),
                stopping: false,
                next_request_id: 1,
            })),
        }
    }

    pub fn status(&self) -> EngineHostStatus {
        lock(&self.inner).status.clone()
    }

    /// Starts the host unless it is already running. The last chain snapshot
    /// and `FEQ_CMD_START` are queued before the caller can send anything else,
    /// so a replacement host opens the endpoint with the chain already applied.
    pub fn start(&self) -> Result<(), String> {
        let mut inner = lock(&self.inner);
        if inner.stopping {
            return Err(String::from("the shell is shutting down"));
        }
        if inner.child.is_some() {
            return Ok(());
        }
        let mut request_ids = inner.next_request_id;
        let frames = startup_frames(inner.last_chain.as_ref(), &mut request_ids)?;
        inner.next_request_id = request_ids;
        let (child, writer, stdout) = match spawn_host(&inner.exe) {
            Ok(parts) => parts,
            Err(error) => {
                inner.status.last_error = Some(error.clone());
                return Err(error);
            }
        };
        inner.child = Some(child);
        inner.writer = Some(writer.clone());
        inner.status.state = String::from("starting");
        inner.status.last_error = None;
        for frame in frames {
            let _ = writer.send(frame);
        }
        let inner = Arc::clone(&self.inner);
        std::thread::spawn(move || drain_host(inner, stdout));
        Ok(())
    }

    /// Queues the whole chain snapshot. Safe before [`Self::start`]: the buffer
    /// is kept and becomes the first frame a new host receives.
    pub fn apply_chain(&self, values: &[f64], revision: u32) {
        let mut inner = lock(&self.inner);
        match build_apply_chain(values, revision, next_request_id(&mut inner.next_request_id)) {
            Ok(frame) => {
                inner.last_chain = Some(ChainSnapshot {
                    values: values.to_vec(),
                    revision,
                });
                if let Some(writer) = &inner.writer {
                    if writer.send(frame).is_err() {
                        inner.status.last_error = Some(String::from(
                            "the DSP host writer stopped; the reader will replace the host",
                        ));
                    }
                }
            }
            Err(error) => inner.status.last_error = Some(error),
        }
    }

    /// Orderly stop: `FEQ_CMD_SHUTDOWN`, then stdin EOF, then the kill only if
    /// the host refused both. The reader thread stands down instead of restarting.
    pub fn shutdown(&self) {
        let child = {
            let mut inner = lock(&self.inner);
            inner.stopping = true;
            if let Some(writer) = inner.writer.take() {
                let _ = writer.send(
                    build_shutdown(0, next_request_id(&mut inner.next_request_id)).to_vec(),
                );
            }
            inner.child.take()
        };
        if let Some(mut child) = child {
            let deadline = Instant::now() + SHUTDOWN_GRACE;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) => {
                        if Instant::now() >= deadline {
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                }
            }
        }
        let mut inner = lock(&self.inner);
        inner.status.state = String::from("stopped");
        inner.status.pid = None;
    }
}

fn lock(inner: &Mutex<HostInner>) -> MutexGuard<'_, HostInner> {
    inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn next_request_id(counter: &mut u32) -> u32 {
    let id = *counter;
    *counter = counter.wrapping_add(1).max(1);
    id
}

fn c_string(bytes: &[c_char]) -> String {
    let bytes: Vec<u8> = bytes
        .iter()
        .take_while(|&&byte| byte != 0)
        .map(|&byte| byte as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The frames a fresh host receives: the last chain snapshot, then `START`.
fn startup_frames(
    last_chain: Option<&ChainSnapshot>,
    request_ids: &mut u32,
) -> Result<Vec<Vec<u8>>, String> {
    let mut frames = Vec::new();
    if let Some(snapshot) = last_chain {
        frames.push(build_apply_chain(
            &snapshot.values,
            snapshot.revision,
            next_request_id(request_ids),
        )?);
        frames.push(build_start(snapshot.revision, next_request_id(request_ids)).to_vec());
    } else {
        frames.push(build_start(0, next_request_id(request_ids)).to_vec());
    }
    Ok(frames)
}

fn spawn_host(exe: &Path) -> Result<(Child, mpsc::Sender<Vec<u8>>, ChildStdout), String> {
    let mut command = Command::new(exe);
    command
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start {}: {error}", exe.display()))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| String::from("FluidEQ-DSP: stdin was not piped"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| String::from("FluidEQ-DSP: stdout was not piped"))?;
    let (sender, receiver) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        while let Ok(frame) = receiver.recv() {
            if !write_frame_to_pipe(&mut stdin, &frame) {
                break;
            }
        }
        // stdin drops here: EOF is the host's second orderly-shutdown path.
    });
    Ok((child, sender, stdout))
}

/// The reader thread: handshake once, then drain every reply so a full pipe
/// never blocks the host, and replace a dead host within the restart budget.
fn drain_host(inner: Arc<Mutex<HostInner>>, mut stdout: ChildStdout) {
    let mut handshaken = false;
    loop {
        match read_frame(&mut stdout) {
            Ok(HostFrame::Handshake(handshake)) if !handshaken => {
                handshaken = true;
                let mut guard = lock(&inner);
                guard.status.state = String::from("running");
                guard.status.pid = guard.child.as_ref().map(Child::id);
                guard.status.backend = Some(c_string(&handshake.backend));
                guard.status.core_version = Some(c_string(&handshake.core_version));
                guard.status.parameter_count = Some(handshake.parameter_count);
                guard.status.last_error = None;
            }
            Ok(HostFrame::Handshake(_)) => {
                match restart_host(
                    &inner,
                    "FluidEQ-DSP: a second handshake desynchronised the control stream",
                ) {
                    Some(replacement) => {
                        stdout = replacement;
                        handshaken = false;
                    }
                    None => return,
                }
            }
            Ok(HostFrame::Ack(ack)) => {
                if ack.status != fluideq_dsp_sys::FeqWireStatus::FEQ_WIRE_APPLIED as u16 {
                    let mut guard = lock(&inner);
                    guard.status.last_error = Some(format!(
                        "FluidEQ-DSP: request {} was not applied (wire status {})",
                        ack.request_id, ack.status
                    ));
                }
            }
            // Telemetry, stats and analysis are drained, not interpreted: the
            // shell's meters arrive over their own bridge.
            Ok(_) => {}
            Err(error) => match restart_host(&inner, &error) {
                Some(replacement) => {
                    stdout = replacement;
                    handshaken = false;
                }
                None => return,
            },
        }
    }
}

/// Called on reader EOF/error. Returns the replacement host's stdout, or None
/// when supervision has ended (orderly stop, budget exhausted, spawn failure).
fn restart_host(inner: &Arc<Mutex<HostInner>>, error: &str) -> Option<ChildStdout> {
    let mut guard = lock(inner);
    guard.writer = None;
    if let Some(mut child) = guard.child.take() {
        let _ = child.wait();
    }
    if guard.stopping {
        guard.status.state = String::from("stopped");
        guard.status.pid = None;
        return None;
    }
    if !restart_allowed(&mut guard.restarts, Instant::now()) {
        guard.status.state = String::from("failed");
        guard.status.pid = None;
        guard.status.last_error = Some(format!("{error}; the restart budget is exhausted"));
        return None;
    }
    let mut request_ids = guard.next_request_id;
    let frames = match startup_frames(guard.last_chain.as_ref(), &mut request_ids) {
        Ok(frames) => {
            guard.next_request_id = request_ids;
            frames
        }
        Err(build_error) => {
            guard.status.state = String::from("failed");
            guard.status.last_error = Some(build_error);
            return None;
        }
    };
    match spawn_host(&guard.exe) {
        Ok((child, writer, stdout)) => {
            guard.child = Some(child);
            guard.writer = Some(writer.clone());
            guard.status.state = String::from("starting");
            guard.status.restarts += 1;
            guard.status.last_error = Some(error.to_string());
            for frame in frames {
                let _ = writer.send(frame);
            }
            Some(stdout)
        }
        Err(spawn_error) => {
            guard.status.state = String::from("failed");
            guard.status.pid = None;
            guard.status.last_error = Some(spawn_error);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_runtime::{build_handshake_frame, serialize_handshake_frame};

    fn handshake_bytes(parameter_count: u32) -> [u8; HANDSHAKE_BYTES] {
        let mut handshake = build_handshake_frame("wasapi-shared", "2.1.0", "test");
        handshake.parameter_count = parameter_count;
        serialize_handshake_frame(&handshake)
    }

    #[test]
    fn handshake_accepts_the_host_contract() {
        let parsed = parse_handshake(&handshake_bytes(FEQ_PARAMETER_COUNT as u32))
            .expect("the handshake should parse");
        assert_eq!(parsed.magic, FEQ_MAGIC_HANDSHAKE);
        assert_eq!(parsed.protocol_version, FEQ_WIRE_PROTOCOL_VERSION);
        assert_eq!(parsed.parameter_schema_version, FEQ_PARAMETER_SCHEMA_VERSION);
        assert_eq!(parsed.abi_version, FEQ_ABI_VERSION);
        assert_eq!(parsed.parameter_count, FEQ_PARAMETER_COUNT as u32);
        assert_eq!(parsed.analysis_frame_bytes as usize, ANALYSIS_HEADER_BYTES);
    }

    #[test]
    fn handshake_rejects_a_parameter_count_mismatch() {
        let error = parse_handshake(&handshake_bytes(FEQ_PARAMETER_COUNT as u32 + 1))
            .expect_err("a different parameter table is a mismatch");
        assert!(error.contains("parameter"), "unexpected error: {error}");
    }

    #[test]
    fn handshake_rejects_a_wrong_magic() {
        let mut bytes = handshake_bytes(FEQ_PARAMETER_COUNT as u32);
        bytes[0..4].copy_from_slice(&FEQ_MAGIC_ACK.to_le_bytes());
        assert!(parse_handshake(&bytes).is_err());
    }

    #[test]
    fn apply_chain_carries_the_whole_payload() {
        let values = vec![0.0, 1.5, -2.25];
        let frame = build_apply_chain(&values, 7, 41).expect("a finite payload is accepted");
        assert_eq!(frame.len(), COMMAND_BYTES + values.len() * 8);
        assert_eq!(
            u32::from_le_bytes(frame[0..4].try_into().unwrap()),
            fluideq_dsp_sys::FEQ_MAGIC_COMMAND
        );
        assert_eq!(
            u16::from_le_bytes(frame[6..8].try_into().unwrap()),
            fluideq_dsp_sys::FeqWireCommand::FEQ_CMD_APPLY_CHAIN as u16
        );
        assert_eq!(
            u32::from_le_bytes(frame[8..12].try_into().unwrap()),
            41
        );
        assert_eq!(
            u32::from_le_bytes(frame[16..20].try_into().unwrap()),
            values.len() as u32
        );
        assert_eq!(
            f64::from_le_bytes(frame[32..40].try_into().unwrap()),
            0.0
        );
        assert_eq!(
            f64::from_le_bytes(frame[40..48].try_into().unwrap()),
            1.5
        );
    }

    #[test]
    fn apply_chain_refuses_empty_and_non_finite_payloads() {
        assert!(build_apply_chain(&[], 1, 1).is_err());
        assert!(build_apply_chain(&[f64::NAN], 1, 1).is_err());
        assert!(build_apply_chain(&[f64::INFINITY], 1, 1).is_err());
        let too_many = vec![0.0; MAX_WIRE_CHAIN_PARAMS as usize + 1];
        assert!(build_apply_chain(&too_many, 1, 1).is_err());
    }

    #[test]
    fn frame_lengths_cover_every_reply_the_host_writes() {
        assert_eq!(frame_bytes_needed(FEQ_MAGIC_ACK, &[]).unwrap(), ACK_BYTES);
        assert_eq!(frame_bytes_needed(FEQ_MAGIC_STATS, &[]).unwrap(), STATS_BYTES);
        assert_eq!(
            frame_bytes_needed(FEQ_MAGIC_TELEMETRY, &[]).unwrap(),
            TELEMETRY_BYTES
        );

        let mut header = [0u8; 92];
        header[0..4].copy_from_slice(&FEQ_MAGIC_ANALYSIS.to_le_bytes());
        header[8..12].copy_from_slice(&1u32.to_le_bytes());
        header[12..16].copy_from_slice(&(FEQ_METER_BINS as u32).to_le_bytes());
        header[16..20].copy_from_slice(&(FEQ_METER_SCOPE_PAIRS as u32).to_le_bytes());
        header[20..24].copy_from_slice(&(FEQ_METER_MAX_BANDS as u32).to_le_bytes());
        header[88..92].copy_from_slice(&(FEQ_METER_DETAIL_BINS as u32).to_le_bytes());
        let expected = ANALYSIS_HEADER_BYTES
            + compute_analysis_frame_payload_bytes(
                1,
                FEQ_METER_BINS as u32,
                FEQ_METER_SCOPE_PAIRS as u32,
                FEQ_METER_MAX_BANDS as u32,
                FEQ_METER_DETAIL_BINS as u32,
            );
        assert_eq!(
            frame_bytes_needed(FEQ_MAGIC_ANALYSIS, &header).unwrap(),
            expected
        );
    }

    #[test]
    fn frame_lengths_reject_an_unknown_magic() {
        assert!(frame_bytes_needed(0xDEAD_BEEF, &[]).is_err());
    }

    #[test]
    fn read_frame_reads_an_ack_from_a_stream() {
        let ack = crate::host_runtime::build_ack_frame(
            9,
            fluideq_dsp_sys::FeqWireStatus::FEQ_WIRE_APPLIED,
            3,
            0,
            0.0,
        );
        let bytes = crate::host_runtime::serialize_ack_frame(&ack);
        let mut reader = std::io::Cursor::new(bytes.to_vec());
        match read_frame(&mut reader).expect("ack frame") {
            HostFrame::Ack(parsed) => {
                assert_eq!(parsed.request_id, 9);
                assert_eq!(parsed.accepted_revision, 3);
            }
            other => panic!("expected an ack, got {other:?}"),
        }
    }

    #[test]
    fn read_frame_reads_a_stats_frame() {
        let mut bytes = [0u8; STATS_BYTES];
        bytes[0..4].copy_from_slice(&FEQ_MAGIC_STATS.to_le_bytes());
        bytes[8..16].copy_from_slice(&123_456u64.to_le_bytes());
        let mut reader = std::io::Cursor::new(bytes.to_vec());
        match read_frame(&mut reader).expect("stats frame") {
            HostFrame::Stats(parsed) => assert_eq!(parsed.working_set_bytes, 123_456),
            other => panic!("expected stats, got {other:?}"),
        }
    }

    #[test]
    fn read_frame_refuses_a_truncated_stream() {
        let mut reader = std::io::Cursor::new(FEQ_MAGIC_ACK.to_le_bytes().to_vec());
        assert!(read_frame(&mut reader).is_err());
    }

    #[test]
    fn restart_budget_allows_three_restarts_per_window() {
        let now = Instant::now();
        let mut restarts = Vec::new();
        assert!(restart_allowed(&mut restarts, now));
        assert!(restart_allowed(&mut restarts, now));
        assert!(restart_allowed(&mut restarts, now));
        assert!(!restart_allowed(
            &mut restarts,
            now + RESTART_WINDOW - Duration::from_millis(1)
        ));
        assert!(restart_allowed(
            &mut restarts,
            now + RESTART_WINDOW + Duration::from_millis(1)
        ));
    }

    #[test]
    fn startup_frames_apply_the_chain_before_start() {
        let snapshot = ChainSnapshot {
            values: vec![0.0, 1.0, -1.0],
            revision: 9,
        };
        let mut request_ids = 1;
        let frames = startup_frames(Some(&snapshot), &mut request_ids).expect("frames");
        assert_eq!(frames.len(), 2);
        assert_eq!(
            u16::from_le_bytes(frames[0][6..8].try_into().unwrap()),
            fluideq_dsp_sys::FeqWireCommand::FEQ_CMD_APPLY_CHAIN as u16
        );
        assert_eq!(
            u16::from_le_bytes(frames[1][6..8].try_into().unwrap()),
            fluideq_dsp_sys::FeqWireCommand::FEQ_CMD_START as u16
        );
        assert_eq!(
            u32::from_le_bytes(frames[0][12..16].try_into().unwrap()),
            9
        );
        assert_ne!(
            u32::from_le_bytes(frames[0][8..12].try_into().unwrap()),
            u32::from_le_bytes(frames[1][8..12].try_into().unwrap()),
            "each command carries its own request id"
        );
    }

    #[test]
    fn startup_frames_without_a_chain_only_start() {
        let mut request_ids = 1;
        let frames = startup_frames(None, &mut request_ids).expect("frames");
        assert_eq!(frames.len(), 1);
        assert_eq!(
            u16::from_le_bytes(frames[0][6..8].try_into().unwrap()),
            fluideq_dsp_sys::FeqWireCommand::FEQ_CMD_START as u16
        );
    }

    #[test]
    fn apply_chain_before_start_keeps_the_buffer_for_the_first_host() {
        let host = EngineHost::new(PathBuf::from("no-such-host.exe"));
        host.apply_chain(&[0.0, 1.5], 4);
        let inner = lock(&host.inner);
        let kept = inner.last_chain.as_ref().expect("the snapshot is kept");
        assert_eq!(kept.values, vec![0.0, 1.5]);
        assert_eq!(kept.revision, 4);
        assert_eq!(inner.status.state, "stopped");
    }

    #[test]
    fn apply_chain_refuses_a_bad_buffer_without_losing_the_last_good_one() {
        let host = EngineHost::new(PathBuf::from("no-such-host.exe"));
        host.apply_chain(&[0.0], 1);
        host.apply_chain(&[f64::NAN], 2);
        let inner = lock(&host.inner);
        assert_eq!(inner.last_chain.as_ref().unwrap().values, vec![0.0]);
        assert_eq!(inner.last_chain.as_ref().unwrap().revision, 1);
        assert!(inner.status.last_error.is_some());
    }

    #[test]
    fn shutdown_of_a_never_started_host_is_harmless() {
        let host = EngineHost::new(PathBuf::from("no-such-host.exe"));
        host.shutdown();
        assert_eq!(host.status().state, "stopped");
        assert!(host.start().is_err(), "a stopped shell does not start hosts");
    }

    #[test]
    fn start_of_a_missing_executable_fails_without_a_panic() {
        let host = EngineHost::new(PathBuf::from("no-such-host.exe"));
        assert!(host.start().is_err());
        assert_eq!(host.status().state, "stopped");
    }

    #[test]
    fn c_string_stops_at_the_nul() {
        let mut bytes = [0 as c_char; 16];
        bytes[0] = b'w' as c_char;
        bytes[1] = b'a' as c_char;
        bytes[2] = b's' as c_char;
        bytes[3] = b'a' as c_char;
        bytes[5] = b'x' as c_char;
        assert_eq!(c_string(&bytes), "wasa");
    }
}
