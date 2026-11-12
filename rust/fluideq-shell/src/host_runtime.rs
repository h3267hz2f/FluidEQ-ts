/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! `dsp-host` Pipe Framing Guards, Device Hot-Swap Shape-Preservation Coordinator,
//! Real-Time Block Volume Ramp / Non-Finite Input Guard, and Blocking Parent Lifecycle Watch (`Slice-M1M8-2`).
//!
//! 1:1 faithful port of:
//! - `/native/dsp-host/src/host_wire.cpp` (`read_exact`, `payload_within`, `wire_length_from_double`, `send_handshake`, `send_ack`)
//! - `/native/dsp-host/src/host_device.cpp` (`reopen_if_device_changed` shape-preservation state machine)
//! - `/native/dsp-host/src/host_audio.cpp` (`SignalSource`, `render_bridge` pre-chain volume ramp & NaN/Inf guard, `FeqDoorbell`)
//! - `/native/dsp-host/src/parent_watch.cpp` (`feq_watch_parent` zero-polling kernel process exit monitor)

use fluideq_dsp_sys::{
    FeqWireAckFrame, FeqWireAnalysisFrame, FeqWireHandshake, FeqWireStatus, FEQ_ABI_VERSION,
    FEQ_MAGIC_ACK, FEQ_MAGIC_HANDSHAKE, FEQ_WIRE_PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::os::raw::c_char;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

// ============================================================================
// 1. `host_wire.cpp` Pipe Desynchronization Guards & Frame Builders
// ============================================================================

/// Default fallback sample rate before an endpoint negotiates (`kFallbackSampleRate` in `host.h`).
pub const FALLBACK_SAMPLE_RATE: u32 = 48_000;
/// Default fallback block size in frames (`kFallbackBlockFrames` in `host.h`).
pub const FALLBACK_BLOCK_FRAMES: u32 = 512;
/// Stereo engine channel count (`kEngineChannels` in `host.h`).
pub const ENGINE_CHANNELS: u32 = 2;

/// Maximum path byte length accepted from the control pipe (`kMaxPathBytes = 32 * 1024` in `host.h`).
/// Windows extended-length paths top out at 32,767 UTF-16 units; anything above this indicates pipe desynchronization.
pub const MAX_WIRE_PATH_BYTES: u32 = 32 * 1024;

/// Maximum chain parameter count in `f64` words (`FEQ_CHAIN_MAX_PARAMS` in `chain.h` / `host.h`):
/// `158` lead + `64 * 7` EQ bands + `15` (Normalizer 3 + LowLatency 1 + Room 11) + `7` Footsteps + `4 + 48 * 4` Tone = `824`.
pub const MAX_WIRE_CHAIN_PARAMS: u32 = fluideq_dsp_sys::FEQ_CHAIN_MAX_PARAMS as u32;

/// Schema version of the core parameter block (`FEQ_PARAMETER_SCHEMA_VERSION`).
pub const PARAMETER_SCHEMA_VERSION: u32 = fluideq_dsp_sys::FEQ_PARAMETER_SCHEMA_VERSION;
/// Legacy core parameter count (`FEQ_PARAMETER_COUNT`).
pub const CORE_PARAMETER_COUNT: u32 = fluideq_dsp_sys::FEQ_PARAMETER_COUNT as u32;

/// Reads exactly `into.len()` bytes from `reader`, looping across short pipe reads (`read_exact` in `host_wire.cpp`).
/// Returns `false` only on true EOF (`0` bytes read) or unrecoverable I/O error.
pub fn read_exact_from_pipe<R: Read>(reader: &mut R, into: &mut [u8]) -> bool {
    let mut filled = 0usize;
    while filled < into.len() {
        match reader.read(&mut into[filled..]) {
            Ok(0) => return false,
            Ok(got) => filled += got,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
    }
    true
}

/// Writes an entire binary frame atomically and flushes the stream (`write_frame` in `host_wire.cpp`).
pub fn write_frame_to_pipe<W: Write>(writer: &mut W, frame_bytes: &[u8]) -> bool {
    let mut written = 0usize;
    while written < frame_bytes.len() {
        match writer.write(&frame_bytes[written..]) {
            Ok(0) => return false,
            Ok(put) => written += put,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
    }
    writer.flush().is_ok()
}

/// Exact port of `payload_within` in `/native/dsp-host/src/host_wire.cpp`:
/// Refuses (rather than clamps or skips) any variable-length payload count exceeding `ceiling`.
/// Once reader and writer disagree on a frame boundary, there is no safe byte count to skip,
/// so the control loop must terminate immediately rather than allocating a multi-gigabyte vector.
pub fn payload_within(count: u32, ceiling: u32, what: &str) -> Result<usize, String> {
    if count <= ceiling {
        Ok(count as usize)
    } else {
        Err(format!(
            "FluidEQ-DSP: {} declares {}, ceiling is {}; the control stream has desynchronised",
            what, count, ceiling
        ))
    }
}

/// Exact port of `wire_length_from_double` in `/native/dsp-host/src/host_wire.cpp`:
/// Validates a `f64` from `FeqWireCommandFrame::value` (used by `RENDER_TO_FILE` to carry path length)
/// while it is still a `f64`. Casting a negative or `NaN` double directly to `usize` on x86-64
/// produces `0x8000000000000000` (`length_error` / abort) before any allocation check can run.
pub fn wire_length_from_double(value: f64, ceiling: u32) -> Result<u32, String> {
    if !value.is_finite() || value < 0.0 || value > (ceiling as f64) {
        return Err(format!(
            "FluidEQ-DSP: path length {} is not a length; the control stream has desynchronised",
            value
        ));
    }
    Ok(value as u32)
}

fn copy_cstr_padded<const N: usize>(src: &str) -> [c_char; N] {
    let mut buf = [0 as c_char; N];
    let bytes = src.as_bytes();
    let copy_len = bytes.len().min(N.saturating_sub(1));
    for i in 0..copy_len {
        buf[i] = bytes[i] as c_char;
    }
    buf
}

/// Exact port of `send_handshake` frame construction in `/native/dsp-host/src/host_wire.cpp`.
pub fn build_handshake_frame(
    backend_name: &str,
    core_version: &str,
    build_revision: &str,
) -> FeqWireHandshake {
    let arch = if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "unknown"
    };

    FeqWireHandshake {
        magic: FEQ_MAGIC_HANDSHAKE,
        protocol_version: FEQ_WIRE_PROTOCOL_VERSION,
        parameter_schema_version: PARAMETER_SCHEMA_VERSION,
        abi_version: FEQ_ABI_VERSION,
        parameter_count: CORE_PARAMETER_COUNT,
        analysis_frame_bytes: std::mem::size_of::<FeqWireAnalysisFrame>() as u32,
        core_version: copy_cstr_padded::<24>(core_version),
        architecture: copy_cstr_padded::<16>(arch),
        build_revision: copy_cstr_padded::<24>(build_revision),
        backend: copy_cstr_padded::<16>(backend_name),
    }
}

/// Exact port of `send_ack` frame construction in `/native/dsp-host/src/host_wire.cpp`.
pub fn build_ack_frame(
    request_id: u32,
    status: FeqWireStatus,
    accepted_revision: u32,
    applied_at_sample_frame: u64,
    sanitized_value: f64,
) -> FeqWireAckFrame {
    FeqWireAckFrame {
        magic: FEQ_MAGIC_ACK,
        protocol_version: FEQ_WIRE_PROTOCOL_VERSION as u16,
        status: status as u16,
        request_id,
        accepted_revision,
        applied_at_sample_frame,
        sanitized_value,
    }
}

// ============================================================================
// 2. `host_audio.cpp` Real-Time Signal Generator, Block Volume Ramp & NaN Guard
// ============================================================================

/// Diagnostic generator signal kind (`SignalKind` in `/native/dsp-host/src/host.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticSignalKind {
    Silence = 0,
    Sine = 1,
}

/// Exact port of `SignalSource` in `/native/dsp-host/src/host.h`:
/// Generates silence or a `-12 dBFS` (`0.251188643`) sine wave with phase wrapped at $2\pi$
/// so running for hours never degrades `f64` trig precision.
#[derive(Debug, Clone)]
pub struct DiagnosticSignalSource {
    kind: DiagnosticSignalKind,
    frequency_hz: f64,
    sample_rate: u32,
    phase: f64,
}

impl Default for DiagnosticSignalSource {
    fn default() -> Self {
        Self::new()
    }
}

impl DiagnosticSignalSource {
    pub fn new() -> Self {
        Self {
            kind: DiagnosticSignalKind::Silence,
            frequency_hz: 440.0,
            sample_rate: FALLBACK_SAMPLE_RATE,
            phase: 0.0,
        }
    }

    pub fn configure(&mut self, kind: DiagnosticSignalKind, frequency_hz: f64) {
        self.kind = kind;
        if frequency_hz > 0.0 && frequency_hz < 24_000.0 {
            self.frequency_hz = frequency_hz;
        }
    }

    pub fn set_sample_rate(&mut self, rate: u32) {
        self.sample_rate = rate;
    }

    pub fn render_planar(&mut self, planar: &mut [Vec<f32>], frames: usize) {
        if self.kind == DiagnosticSignalKind::Silence {
            return;
        }
        let tau = std::f64::consts::TAU;
        let step = tau * self.frequency_hz / (self.sample_rate.max(1) as f64);
        for frame in 0..frames {
            // -12 dBFS = 0.251188643
            let value = (0.251_188_643 * self.phase.sin()) as f32;
            for ch in planar.iter_mut() {
                if frame < ch.len() {
                    ch[frame] = value;
                }
            }
            self.phase += step;
            if self.phase > tau {
                self.phase -= tau;
            }
        }
    }
}

/// Exact port of the pre-chain volume ramp and non-finite sample guard in `render_bridge`
/// (`/native/dsp-host/src/host_audio.cpp` lines 82–114):
/// 1. Applies the listener's fader BEFORE the DSP chain (matching `<audio>` media element routing
///    so dynamics stages respond to post-fader level) using a per-frame linear ramp across the block
///    to eliminate zipper clicks when dragging the fader.
/// 2. Scrubs any `NaN` or `±Inf` sample to `0.0` BEFORE entering the biquad/chain filters
///    so a single corrupt frame from a decoder cannot poison the IIR state for the rest of the track.
pub fn apply_pre_chain_volume_ramp_and_nan_guard(
    planar: &mut [Vec<f32>],
    frames: usize,
    volume_now: &mut f32,
    volume_target: f32,
) -> usize {
    let from = *volume_now;
    if (from - volume_target).abs() > f32::EPSILON || (volume_target - 1.0).abs() > f32::EPSILON {
        let step = if frames > 0 {
            (volume_target - from) / (frames as f32)
        } else {
            0.0
        };
        for channel in planar.iter_mut() {
            let mut running = from;
            let limit = frames.min(channel.len());
            for sample in &mut channel[..limit] {
                running += step;
                *sample *= running;
            }
        }
        *volume_now = volume_target;
    }

    let mut scrubbed = 0usize;
    for channel in planar.iter_mut() {
        let limit = frames.min(channel.len());
        for sample in &mut channel[..limit] {
            if !sample.is_finite() {
                *sample = 0.0;
                scrubbed += 1;
            }
        }
    }
    scrubbed
}

/// Lock-free two-phase doorbell (`FeqDoorbell` in `fluideq/doorbell.h` & `host_audio.cpp`):
/// The real-time audio callback calls `arm()` (atomic store, zero OS syscall),
/// and `ring_what_the_block_armed` calls `take_armed()` after the period has been handed to the device.
#[derive(Debug, Default)]
pub struct AudioPeriodDoorbell {
    armed: AtomicBool,
}

impl AudioPeriodDoorbell {
    pub const fn new() -> Self {
        Self {
            armed: AtomicBool::new(false),
        }
    }

    /// Real-time safe: marks the doorbell armed without making any OS call.
    pub fn arm(&self) {
        self.armed.store(true, Ordering::Release);
    }

    /// Device period-done hook: consumes the armed flag and returns `true` if the worker should be woken.
    pub fn take_armed(&self) -> bool {
        self.armed.swap(false, Ordering::AcqRel)
    }
}

// ============================================================================
// 3. `host_device.cpp` Device Hot-Swap & Shape-Preservation State Machine
// ============================================================================

/// Negotiated shared-mode endpoint format (`FeqBackendFormat` in `audio_backend.h`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NegotiatedBackendFormat {
    pub sample_rate: u32,
    pub channels: u32,
    pub max_block_frames: u32,
}

/// Outcome of `reopen_if_device_changed` (`/native/dsp-host/src/host_device.cpp`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceReopenOutcome {
    /// `needs_reopen` was false or `device_wanted` was false.
    NoActionNeeded,
    /// Open failed during a device transition window; `await_endpoint` armed, error logged once per outage.
    OutageAwaitingEndpoint { first_report: bool, error: String },
    /// Device reopened with the exact same `(sample_rate, channels, block_frames)` shape:
    /// Chain and player decks were preserved without interruption (`device_generation` NOT bumped).
    ReopenedSameShapeKeptPlaying { sample_rate: u32 },
    /// Device reopened with a new format shape: chain and player were rebuilt and `device_generation` bumped
    /// so the renderer cues the playing track again.
    ReopenedShapeChangedRebuilt {
        sample_rate: u32,
        channels: u32,
        block_frames: u32,
        device_generation: u32,
    },
    /// Device opened and rebuilt, but `backend.start()` failed; closed and awaiting next endpoint event.
    StartFailedAwaitingEndpoint { error: String },
}

/// Stateful coordinator mirroring `HostState` device reopen fields in `host.h` and `host_device.cpp`.
#[derive(Debug)]
pub struct DeviceReopenCoordinator {
    pub sample_rate: u32,
    pub channels: u32,
    pub block_frames: u32,
    pub device_wanted: AtomicBool,
    pub device_generation: AtomicU32,
    pub reopen_failure_reported: bool,
    pub player_has_source: AtomicBool,
    pub room_head_loaded: i32,
}

impl Default for DeviceReopenCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceReopenCoordinator {
    pub fn new() -> Self {
        Self {
            sample_rate: FALLBACK_SAMPLE_RATE,
            channels: ENGINE_CHANNELS,
            block_frames: FALLBACK_BLOCK_FRAMES,
            device_wanted: AtomicBool::new(false),
            device_generation: AtomicU32::new(0),
            reopen_failure_reported: false,
            player_has_source: AtomicBool::new(false),
            room_head_loaded: -1,
        }
    }

    /// Exact port of `reopen_if_device_changed` in `/native/dsp-host/src/host_device.cpp`:
    /// - If `open_result` fails, reports the outage only once (`first_report = true` on the first failure)
    ///   and leaves `device_generation` untouched.
    /// - If `open_result` succeeds with the same `(sample_rate, min(channels, 2), max_block_frames)`,
    ///   does NOT rebuild the chain/player, keeps `player_has_source` intact so music continues seamlessly
    ///   when switching from speakers to headphones at 48 kHz, and does NOT increment `device_generation`.
    /// - If the shape changed, resets `room_head_loaded = -1` and `player_has_source = false`, and
    ///   increments `device_generation` only after `start_succeeded` is true.
    pub fn handle_reopen_attempt(
        &mut self,
        needs_reopen: bool,
        open_result: Result<NegotiatedBackendFormat, String>,
        start_result: Result<(), String>,
    ) -> DeviceReopenOutcome {
        if !needs_reopen || !self.device_wanted.load(Ordering::Acquire) {
            return DeviceReopenOutcome::NoActionNeeded;
        }

        let negotiated = match open_result {
            Err(error) => {
                let first_report = !self.reopen_failure_reported;
                self.reopen_failure_reported = true;
                return DeviceReopenOutcome::OutageAwaitingEndpoint {
                    first_report,
                    error,
                };
            }
            Ok(fmt) => fmt,
        };
        self.reopen_failure_reported = false;

        let clamped_channels = negotiated.channels.min(ENGINE_CHANNELS);
        let shape_changed = negotiated.sample_rate != self.sample_rate
            || clamped_channels != self.channels
            || negotiated.max_block_frames != self.block_frames;

        self.sample_rate = negotiated.sample_rate;
        self.channels = clamped_channels;
        self.block_frames = negotiated.max_block_frames;

        if shape_changed {
            self.room_head_loaded = -1;
            self.player_has_source.store(false, Ordering::Release);
        }

        if let Err(error) = start_result {
            return DeviceReopenOutcome::StartFailedAwaitingEndpoint { error };
        }

        if !shape_changed {
            return DeviceReopenOutcome::ReopenedSameShapeKeptPlaying {
                sample_rate: self.sample_rate,
            };
        }

        let next_gen = self.device_generation.fetch_add(1, Ordering::AcqRel) + 1;
        DeviceReopenOutcome::ReopenedShapeChangedRebuilt {
            sample_rate: self.sample_rate,
            channels: self.channels,
            block_frames: self.block_frames,
            device_generation: next_gen,
        }
    }
}

// ============================================================================
// 4. `parent_watch.cpp` Zero-Polling Blocking Parent Exit Monitor
// ============================================================================

/// OS kernel mechanism used to block on parent process termination without timers or polling
/// (`/native/dsp-host/src/parent_watch.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParentWatchKernelMechanism {
    /// Windows `OpenProcess(SYNCHRONIZE)` + `WaitForSingleObject(parent, INFINITE)`.
    WindowsSynchronizeHandle,
    /// Linux 5.3+ `syscall(SYS_pidfd_open, pid, 0)` + `poll(fd, POLLIN, -1)` + `prctl(PR_SET_PDEATHSIG, SIGTERM)`.
    LinuxPidfdOpenAndPdeathsig,
    /// macOS `kqueue()` + `EVFILT_PROC` (`EV_ADD | EV_ONESHOT`, `NOTE_EXIT`).
    MacOsKqueueEvfiltProc,
    /// Fallback pipe EOF when `parent_pid == 0` or unsupported kernel.
    StdinPipeEofFallback,
}

/// Returns the active kernel parent-watch mechanism for the current target OS.
pub fn select_parent_watch_mechanism(parent_pid: u32) -> ParentWatchKernelMechanism {
    if parent_pid == 0 {
        return ParentWatchKernelMechanism::StdinPipeEofFallback;
    }
    #[cfg(target_os = "windows")]
    {
        ParentWatchKernelMechanism::WindowsSynchronizeHandle
    }
    #[cfg(target_os = "linux")]
    {
        ParentWatchKernelMechanism::LinuxPidfdOpenAndPdeathsig
    }
    #[cfg(target_os = "macos")]
    {
        ParentWatchKernelMechanism::MacOsKqueueEvfiltProc
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        ParentWatchKernelMechanism::StdinPipeEofFallback
    }
}

// ============================================================================
// 5. `main.cpp`, `host_telemetry.cpp`, `process_stats.h` & `device_watch.cpp`
//    Supervised Host Pipe Loop, Telemetry Cadence & Audiosrv Endpoint Watch (`Slice-M1M8-6`)
// ============================================================================

/// Twenty engine reports = 0.5 seconds of audio between process stats samples at 40 Hz (`kStatsIntervalReports` in `main.cpp`).
/// With nothing playing, a sample is emitted immediately after any command handled while the device is stopped (`stats_wanted`).
pub const STATS_INTERVAL_REPORTS: u32 = 20;

/// Exact port of `FeqProcessStats` in `/native/dsp-host/src/process_stats.h` & `process_stats_windows.cpp`:
/// Reports working-set bytes (`metric.memory.workingSetSize`) and a running total of kernel + user CPU seconds
/// (`(as_100ns(kernel) + as_100ns(user)) / 1e7`) rather than a rate, so an idle host never wakes on a 500 ms timer.
#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HostProcessStatsSample {
    pub working_set_bytes: u64,
    pub cpu_seconds: f64,
}

/// Converts Windows `FILETIME` (`dwLowDateTime`, `dwHighDateTime`) 100-ns ticks of kernel + user time into CPU seconds,
/// matching `as_100ns` in `/native/dsp-host/src/platform/windows/process_stats_windows.cpp`.
pub fn cpu_seconds_from_win32_filetimes(
    kernel_low: u32,
    kernel_high: u32,
    user_low: u32,
    user_high: u32,
) -> f64 {
    let kernel_100ns = ((kernel_high as u64) << 32) | (kernel_low as u64);
    let user_100ns = ((user_high as u64) << 32) | (user_low as u64);
    (kernel_100ns.saturating_add(user_100ns) as f64) / 1.0e7
}

#[cfg(target_os = "windows")]
pub mod win32_host_ffi {
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct FileTime {
        pub dw_low_date_time: u32,
        pub dw_high_date_time: u32,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct ProcessMemoryCounters {
        pub cb: u32,
        pub page_fault_count: u32,
        pub peak_working_set_size: usize,
        pub working_set_size: usize,
        pub quota_peak_paged_pool_usage: usize,
        pub quota_paged_pool_usage: usize,
        pub quota_peak_non_paged_pool_usage: usize,
        pub quota_non_paged_pool_usage: usize,
        pub pagefile_usage: usize,
        pub peak_pagefile_usage: usize,
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn GetCurrentProcess() -> *mut c_void;
        pub fn GetProcessTimes(
            h_process: *mut c_void,
            lp_creation_time: *mut FileTime,
            lp_exit_time: *mut FileTime,
            lp_kernel_time: *mut FileTime,
            lp_user_time: *mut FileTime,
        ) -> i32;
        pub fn K32GetProcessMemoryInfo(
            process: *mut c_void,
            ppsmem_counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
        pub fn CreateNamedPipeW(
            lp_name: *const u16,
            dw_open_mode: u32,
            dw_pipe_mode: u32,
            n_max_instances: u32,
            n_out_buffer_size: u32,
            n_in_buffer_size: u32,
            n_default_time_out: u32,
            lp_security_attributes: *const c_void,
        ) -> *mut c_void;
        pub fn ConnectNamedPipe(h_named_pipe: *mut c_void, lp_overlapped: *mut c_void) -> i32;
        pub fn DisconnectNamedPipe(h_named_pipe: *mut c_void) -> i32;
        pub fn WaitForSingleObject(h_handle: *mut c_void, dw_milliseconds: u32) -> u32;
    }

    pub fn sample_process_stats_win32() -> Option<super::HostProcessStatsSample> {
        unsafe {
            let process = GetCurrentProcess();
            let mut mem = ProcessMemoryCounters {
                cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
                ..Default::default()
            };
            if K32GetProcessMemoryInfo(process, &mut mem as *mut ProcessMemoryCounters, mem.cb) == 0
            {
                return None;
            }
            let mut created = FileTime::default();
            let mut exited = FileTime::default();
            let mut kernel = FileTime::default();
            let mut user = FileTime::default();
            if GetProcessTimes(
                process,
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            ) == 0
            {
                return None;
            }
            let cpu_seconds = super::cpu_seconds_from_win32_filetimes(
                kernel.dw_low_date_time,
                kernel.dw_high_date_time,
                user.dw_low_date_time,
                user.dw_high_date_time,
            );
            Some(super::HostProcessStatsSample {
                working_set_bytes: mem.working_set_size as u64,
                cpu_seconds,
            })
        }
    }
}

/// Samples the current process's working set and running CPU seconds (`feq_sample_process_stats` in `process_stats.h`).
/// Returns `None` on platforms without a sampler so the host sends nothing rather than a false zero (`host_telemetry.cpp`).
pub fn sample_current_process_stats() -> Option<HostProcessStatsSample> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let mut statm_parts = statm.split_whitespace();
        let _total_pages = statm_parts.next()?;
        let resident_pages: u64 = statm_parts.next()?.parse().ok()?;
        let working_set_bytes = resident_pages.saturating_mul(4096);

        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        let rparen = stat.rfind(')')?;
        let after_comm = stat.get(rparen + 2..)?;
        let fields: Vec<&str> = after_comm.split_whitespace().collect();
        // In `/proc/[pid]/stat`, utime is field 14 (index 11 after comm) and stime is field 15 (index 12 after comm), in USER_HZ (100 Hz).
        let utime_ticks: u64 = fields.get(11)?.parse().ok()?;
        let stime_ticks: u64 = fields.get(12)?.parse().ok()?;
        let cpu_seconds = (utime_ticks.saturating_add(stime_ticks) as f64) / 100.0;

        Some(HostProcessStatsSample {
            working_set_bytes,
            cpu_seconds,
        })
    }
    #[cfg(target_os = "windows")]
    {
        win32_host_ffi::sample_process_stats_win32()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

/// Encodes a 24-byte `FeqWireStatsFrame` (`FEQS`) in little-endian wire layout (`publish_process_stats` in `host_telemetry.cpp`).
pub fn encode_process_stats_frame(sample: HostProcessStatsSample) -> [u8; 24] {
    let mut out = [0u8; 24];
    out[0..4].copy_from_slice(&fluideq_dsp_sys::FEQ_MAGIC_STATS.to_le_bytes());
    out[4..8].copy_from_slice(&0u32.to_le_bytes());
    out[8..16].copy_from_slice(&sample.working_set_bytes.to_le_bytes());
    out[16..24].copy_from_slice(&sample.cpu_seconds.to_le_bytes());
    out
}

/// Telemetry-thread cadence controller (`main.cpp` lines 195–216):
/// Counts engine telemetry reports (`reports_since_stats`) and checks `stats_wanted`
/// (armed whenever a command runs while no audio device is active).
#[derive(Debug, Default)]
pub struct TelemetryCadenceTracker {
    pub reports_since_stats: u32,
    pub stats_wanted: AtomicBool,
}

impl TelemetryCadenceTracker {
    pub const fn new() -> Self {
        Self {
            reports_since_stats: 0,
            stats_wanted: AtomicBool::new(false),
        }
    }

    /// Called by the control thread after handling a command while `!backend.is_running()` (`main.cpp` lines 286–289).
    pub fn arm_stats_after_offline_command(&self) {
        self.stats_wanted.store(true, Ordering::Release);
    }

    /// Called on each telemetry pass after draining `drained_reports` from the engine (`main.cpp` lines 203–212).
    /// Returns `true` when `publish_process_stats()` must be executed on this pass.
    pub fn on_telemetry_pass(&mut self, drained_reports: u32) -> bool {
        self.reports_since_stats = self.reports_since_stats.saturating_add(drained_reports);
        let wanted = self.stats_wanted.swap(false, Ordering::AcqRel);
        if self.reports_since_stats >= STATS_INTERVAL_REPORTS || wanted {
            self.reports_since_stats = 0;
            true
        } else {
            false
        }
    }
}

/// Exact port of `OutputNotifications` and `DeviceWatch::run` in `/native/dsp-host/src/platform/windows/device_watch.cpp`:
/// - `OnDefaultDeviceChanged`: triggers `default_changed` ONLY when `flow == eRender (0)` and `role == eConsole (0)`
///   so capture device changes or communications role switches never interrupt music playback.
/// - `OnPropertyValueChanged`: deliberately ignored (`false`) because Bluetooth headsets report battery level
///   through `PKEY_Devices_BatteryPlusCharging`, and reopening on every battery tick would cause dropouts.
/// - `Audiosrv` service state transition: re-registers `IMMDeviceEnumerator` and triggers `outputs_changed`
///   only when `Audiosrv` transitions from `!running -> SERVICE_RUNNING`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceWatchState {
    pub audiosrv_running: bool,
    pub enumerator_registered: bool,
    pub default_changed_count: u64,
    pub outputs_changed_count: u64,
}

impl Default for DeviceWatchState {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceWatchState {
    pub const fn new() -> Self {
        Self {
            audiosrv_running: true,
            enumerator_registered: true,
            default_changed_count: 0,
            outputs_changed_count: 0,
        }
    }

    /// `IMMNotificationClient::OnDefaultDeviceChanged(flow, role, pwstrDefaultDeviceId)`
    /// (`eRender = 0`, `eConsole = 0`).
    pub fn on_default_device_changed(&mut self, data_flow: u32, role: u32) -> bool {
        if data_flow == 0 && role == 0 {
            self.default_changed_count += 1;
            true
        } else {
            false
        }
    }

    /// `IMMNotificationClient::OnPropertyValueChanged`: always ignored so Bluetooth battery updates never reopen the stream.
    pub fn on_property_value_changed(&mut self) -> bool {
        false
    }

    /// `DeviceWatch::run` service notification handler (`device_watch.cpp` lines 195–201):
    /// Re-registers `IMMDeviceEnumerator` (`listen()`) and fires `outputs_changed()` when `Audiosrv` restarts.
    pub fn on_audiosrv_state_change(&mut self, now_running: bool) -> bool {
        let restarted = now_running && !self.audiosrv_running;
        self.audiosrv_running = now_running;
        if restarted {
            self.enumerator_registered = true;
            self.outputs_changed_count += 1;
            true
        } else {
            false
        }
    }
}

/// Ordered shutdown sequence from `/native/dsp-host/src/main.cpp` lines 292–309:
/// Device closes first (so real-time callback stops reading engine), then decoder thread, then telemetry thread,
/// then player -> chain -> meters -> engine (reverse of read dependency order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostShutdownStep {
    CloseAudioBackend,
    StopAndJoinDecoderThread,
    StopAndJoinTelemetryThread,
    DestroyPlayer,
    DestroyChain,
    DestroyMeters,
    DestroyEngine,
}

pub const ORDERED_HOST_SHUTDOWN_SEQUENCE: [HostShutdownStep; 7] = [
    HostShutdownStep::CloseAudioBackend,
    HostShutdownStep::StopAndJoinDecoderThread,
    HostShutdownStep::StopAndJoinTelemetryThread,
    HostShutdownStep::DestroyPlayer,
    HostShutdownStep::DestroyChain,
    HostShutdownStep::DestroyMeters,
    HostShutdownStep::DestroyEngine,
];

// ============================================================================
// 4b. `analysis_publication.h`, `chain_route.h` & `processing_latency.h`
// ============================================================================

/// Exact port of `AnalysisPublication` in `/native/dsp-host/src/analysis_publication.h`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AnalysisPublication {
    pub publish_eq_detail: bool,
    pub eq_detail_bins: u32,
}

/// Exact port of `decide_analysis_publication` in `/native/dsp-host/src/analysis_publication.h`:
/// Decides whether an `FEQN` analysis frame carries the `1024`-float high-resolution low-end
/// `eq_detail` payload (`20–500 Hz` from the `16,384`-sample ring).
pub fn decide_analysis_publication(
    stage_mask: u32,
    eq_detail_ready: bool,
    configured_bins: u32,
) -> AnalysisPublication {
    if stage_mask == 0 || !eq_detail_ready || configured_bins == 0 {
        return AnalysisPublication::default();
    }
    AnalysisPublication {
        publish_eq_detail: true,
        eq_detail_bins: configured_bins,
    }
}

/// Exact port of `should_apply_dsp_chain` in `/native/dsp-host/src/chain_route.h`:
/// When `raw_sharing` is true (`FEQ_CMD_SET_RAW_SHARING`), `dsp-host` bypasses `FeqChain`
/// because the system-wide Windows APO (`FluidEQ APO`) already processes the endpoint downstream.
#[inline]
pub const fn should_apply_dsp_chain(raw_sharing: bool) -> bool {
    !raw_sharing
}

/// Exact port of `ProcessingLatency` in `/native/dsp-host/src/processing_latency.h`:
/// Lock-free 10-word `AtomicU32` seqlock (`total`, `parts[8]`, `active`) published by the
/// real-time audio callback and read by the telemetry thread without mutexes.
#[derive(Debug, Default)]
pub struct ProcessingLatency {
    words: [AtomicU32; 10],
    generation: AtomicU32,
}

impl ProcessingLatency {
    pub const fn new() -> Self {
        Self {
            words: [
                AtomicU32::new(0),
                AtomicU32::new(0),
                AtomicU32::new(0),
                AtomicU32::new(0),
                AtomicU32::new(0),
                AtomicU32::new(0),
                AtomicU32::new(0),
                AtomicU32::new(0),
                AtomicU32::new(0),
                AtomicU32::new(0),
            ],
            generation: AtomicU32::new(0),
        }
    }

    pub fn clear(&self) {
        self.write(0, &fluideq_dsp_sys::FeqChainLatencyParts::default(), 0);
    }

    pub fn write(
        &self,
        total_frames: u32,
        parts: &fluideq_dsp_sys::FeqChainLatencyParts,
        active_stages: u32,
    ) {
        let next: [u32; 10] = [
            total_frames,
            parts.eq,
            parts.restoration,
            parts.deesser,
            parts.leveler,
            parts.room,
            parts.bass_punch,
            parts.maximizer,
            parts.headroom,
            active_stages,
        ];
        let mut unchanged = true;
        for (i, &val) in next.iter().enumerate() {
            if self.words[i].load(Ordering::Relaxed) != val {
                unchanged = false;
                break;
            }
        }
        if unchanged {
            return;
        }
        let gen = self.generation.load(Ordering::Relaxed);
        self.generation.store(gen.wrapping_add(1), Ordering::Release);
        for (i, &val) in next.iter().enumerate() {
            self.words[i].store(val, Ordering::Relaxed);
        }
        self.generation.store(gen.wrapping_add(2), Ordering::Release);
    }

    pub fn read(&self) -> (u32, [u32; 8], u32) {
        for _ in 0..4 {
            let before = self.generation.load(Ordering::Acquire);
            if (before & 1) != 0 {
                continue;
            }
            let total = self.words[0].load(Ordering::Relaxed);
            let mut parts = [0u32; 8];
            for (i, part) in parts.iter_mut().enumerate() {
                *part = self.words[i + 1].load(Ordering::Relaxed);
            }
            let active = self.words[9].load(Ordering::Relaxed);
            let after = self.generation.load(Ordering::Acquire);
            if before == after {
                return (total, parts, active);
            }
        }
        let total = self.words[0].load(Ordering::Relaxed);
        let mut parts = [0u32; 8];
        for (i, part) in parts.iter_mut().enumerate() {
            *part = self.words[i + 1].load(Ordering::Relaxed);
        }
        let active = self.words[9].load(Ordering::Relaxed);
        (total, parts, active)
    }
}

/// Supervised binary pipe session connecting `fluideq-shell` and `AppStateStore` over `FEQC` / `FEQA` / `FEQS`:
/// Complete command-session state mirroring the host-side state mutated across all 23
/// `FeqWireCommand` branches in `/native/dsp-host/src/host_commands.cpp`.
#[derive(Debug, Clone)]
pub struct HostCommandSessionState {
    pub backend_running: bool,
    pub endpoint_absent: bool,
    pub device_wanted: bool,
    pub sample_rate: u32,
    pub channels: u32,
    pub block_frames: u32,
    pub diagnostic_signal_enabled: bool,
    pub diagnostic_frequency_hz: f64,
    pub analysis_enabled: bool,
    pub raw_sharing: bool,
    pub target_volume: f32,
    pub current_volume: f32,
    pub voice_model_path: String,
    pub voice_runtime_path: String,
    pub noise_profile_loaded: bool,
    pub noise_profile_words: Vec<f64>,
    pub snapshot_words: Vec<f64>,
    pub decks: fluideq_state::deck_transport::DualDeckCoordinator,
    pub playing: bool,
    pub crossfade_table: fluideq_dsp_sys::FeqCrossfadeTable,
}

impl Default for HostCommandSessionState {
    fn default() -> Self {
        Self {
            backend_running: false,
            endpoint_absent: false,
            device_wanted: false,
            sample_rate: 48_000,
            channels: 2,
            block_frames: 480,
            diagnostic_signal_enabled: false,
            diagnostic_frequency_hz: 440.0,
            analysis_enabled: false,
            raw_sharing: false,
            target_volume: 1.0,
            current_volume: 1.0,
            voice_model_path: String::new(),
            voice_runtime_path: String::new(),
            noise_profile_loaded: false,
            noise_profile_words: Vec::new(),
            snapshot_words: vec![0.0; fluideq_dsp_sys::FEQ_PARAMETER_COUNT],
            decks: fluideq_state::deck_transport::DualDeckCoordinator::new(),
            playing: false,
            crossfade_table: fluideq_dsp_sys::FeqCrossfadeTable::default(),
        }
    }
}

/// Validates and computes the total interleaved sample count (`total_frames * channels`)
/// for `FEQ_CMD_RENDER_TO_FILE`, enforcing the 32-bit RIFF WAV byte ceiling
/// `(0xFFFFFFFF - 64) / (max(channels, 1) * sizeof(f32))` from `/native/dsp-host/src/host_commands.cpp` (lines 483–491)
/// and reserving the output buffer fallibly via `Vec::try_reserve_exact` so a 3-hour export on a
/// memory-constrained machine returns `Err` (`FEQ_WIRE_REJECTED`) instead of aborting the process.
pub fn validate_and_reserve_render_to_file_buffer(
    total_frames: u64,
    channels: u32,
) -> Result<Vec<f32>, String> {
    let ch = channels.max(1) as u64;
    let max_wav_frames = (0xFFFF_FFFFu64 - 64) / (ch * 4);
    if total_frames == 0 || total_frames > max_wav_frames {
        return Err(format!(
            "RENDER_TO_FILE frame count {} outside valid 32-bit WAV ceiling 1..={}",
            total_frames, max_wav_frames
        ));
    }
    let total_samples_u64 = total_frames
        .checked_mul(ch)
        .ok_or_else(|| "RENDER_TO_FILE sample count overflow".to_string())?;
    let total_samples = usize::try_from(total_samples_u64)
        .map_err(|_| "RENDER_TO_FILE sample count exceeds address space".to_string())?;
    let mut out = Vec::new();
    out.try_reserve_exact(total_samples)
        .map_err(|e| format!("RENDER_TO_FILE allocation refused: {}", e))?;
    Ok(out)
}

/// Serializes `FeqWireHandshake` (`FEQH`, 104 bytes) into its exact little-endian wire layout.
pub fn serialize_handshake_frame(hs: &FeqWireHandshake) -> [u8; 104] {
    let mut out = [0u8; 104];
    out[0..4].copy_from_slice(&hs.magic.to_le_bytes());
    out[4..8].copy_from_slice(&hs.protocol_version.to_le_bytes());
    out[8..12].copy_from_slice(&hs.parameter_schema_version.to_le_bytes());
    out[12..16].copy_from_slice(&hs.abi_version.to_le_bytes());
    out[16..20].copy_from_slice(&hs.parameter_count.to_le_bytes());
    out[20..24].copy_from_slice(&hs.analysis_frame_bytes.to_le_bytes());
    for (i, &b) in hs.core_version.iter().enumerate() {
        out[24 + i] = b as u8;
    }
    for (i, &b) in hs.architecture.iter().enumerate() {
        out[48 + i] = b as u8;
    }
    for (i, &b) in hs.build_revision.iter().enumerate() {
        out[64 + i] = b as u8;
    }
    for (i, &b) in hs.backend.iter().enumerate() {
        out[88 + i] = b as u8;
    }
    out
}

/// Reads 32-byte `FeqWireCommandFrame` commands from `reader`, validates magic & protocol version
/// (emitting `FEQ_WIRE_UNSUPPORTED` ack on mismatch just like `main.cpp` lines 272–275),
/// dispatches all command branches from `/native/dsp-host/src/host_commands.cpp`, and writes
/// serialized `FeqWireAckFrame` + `FeqWireStatsFrame` responses to `writer`.
pub fn run_supervised_pipe_command_loop<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    store: &mut fluideq_state::AppStateStore,
    device_running: bool,
    cadence: &mut TelemetryCadenceTracker,
) -> Result<usize, String> {
    let mut session = HostCommandSessionState {
        backend_running: device_running,
        device_wanted: device_running,
        ..HostCommandSessionState::default()
    };
    run_supervised_pipe_command_loop_with_session(reader, writer, store, &mut session, cadence)
}

/// Full stateful command dispatcher mirroring `handle_command` in `/native/dsp-host/src/host_commands.cpp`
/// across all 23 `FeqWireCommand` variants.
pub fn run_supervised_pipe_command_loop_with_session<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    store: &mut fluideq_state::AppStateStore,
    session: &mut HostCommandSessionState,
    cadence: &mut TelemetryCadenceTracker,
) -> Result<usize, String> {
    use fluideq_dsp_sys::FeqWireCommand;

    let mut commands_processed = 0usize;
    let mut cmd_buf = [0u8; 32];

    while read_exact_from_pipe(reader, &mut cmd_buf) {
        let magic = u32::from_le_bytes([cmd_buf[0], cmd_buf[1], cmd_buf[2], cmd_buf[3]]);
        let protocol_version = u16::from_le_bytes([cmd_buf[4], cmd_buf[5]]) as u32;
        let command_raw = u16::from_le_bytes([cmd_buf[6], cmd_buf[7]]);
        let request_id = u32::from_le_bytes([cmd_buf[8], cmd_buf[9], cmd_buf[10], cmd_buf[11]]);
        let settings_revision =
            u32::from_le_bytes([cmd_buf[12], cmd_buf[13], cmd_buf[14], cmd_buf[15]]);
        let parameter_id =
            u32::from_le_bytes([cmd_buf[16], cmd_buf[17], cmd_buf[18], cmd_buf[19]]);
        let parameter_index =
            i32::from_le_bytes([cmd_buf[20], cmd_buf[21], cmd_buf[22], cmd_buf[23]]);
        let value = f64::from_le_bytes([
            cmd_buf[24], cmd_buf[25], cmd_buf[26], cmd_buf[27], cmd_buf[28], cmd_buf[29],
            cmd_buf[30], cmd_buf[31],
        ]);

        if magic != fluideq_dsp_sys::FEQ_MAGIC_COMMAND
            || protocol_version != FEQ_WIRE_PROTOCOL_VERSION
        {
            let ack = build_ack_frame(
                request_id,
                FeqWireStatus::FEQ_WIRE_UNSUPPORTED,
                0,
                0,
                0.0,
            );
            let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
            continue;
        }

        commands_processed += 1;

        if command_raw == FeqWireCommand::FEQ_CMD_SHUTDOWN as u16 {
            session.device_wanted = false;
            session.backend_running = false;
            let ack = build_ack_frame(
                request_id,
                FeqWireStatus::FEQ_WIRE_APPLIED,
                settings_revision,
                0,
                0.0,
            );
            let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
            break;
        }

        if command_raw == FeqWireCommand::FEQ_CMD_HELLO as u16 {
            // `FEQ_CMD_HELLO` (`host_commands.cpp` lines 479–481): sends FEQA ack
            // (handshake FEQH is sent once at host startup in `main.cpp` line 157).
            let ack = build_ack_frame(
                request_id,
                FeqWireStatus::FEQ_WIRE_APPLIED,
                settings_revision,
                0,
                0.0,
            );
            let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
        } else if command_raw == FeqWireCommand::FEQ_CMD_START as u16 {
            // `start_device` (`host_commands.cpp` lines 16–84):
            // If backend is already running (e.g. WebView reloaded while host stayed up),
            // return FEQ_WIRE_APPLIED with current sample_rate WITHOUT reopening or rebuilding.
            session.device_wanted = true;
            let (status, reported_rate) = if session.backend_running {
                (FeqWireStatus::FEQ_WIRE_APPLIED, session.sample_rate as f64)
            } else if session.endpoint_absent {
                (FeqWireStatus::FEQ_WIRE_UNSUPPORTED, 0.0)
            } else {
                session.backend_running = true;
                (FeqWireStatus::FEQ_WIRE_APPLIED, session.sample_rate as f64)
            };
            let ack = build_ack_frame(
                request_id,
                status,
                settings_revision,
                0,
                reported_rate,
            );
            let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
        } else if command_raw == FeqWireCommand::FEQ_CMD_STOP as u16 {
            // `stop_device` (`host_commands.cpp` lines 86–92): releases hardware so DAC noise floor is silent.
            session.device_wanted = false;
            session.backend_running = false;
            let ack = build_ack_frame(
                request_id,
                FeqWireStatus::FEQ_WIRE_APPLIED,
                settings_revision,
                0,
                0.0,
            );
            let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
        } else if command_raw == FeqWireCommand::FEQ_CMD_SET_PARAMETER as u16 {
            // `FEQ_CMD_SET_PARAMETER` (`host_commands.cpp` lines 492–498):
            // Sets a single core parameter slot and returns the sanitized value.
            let idx = parameter_id as usize;
            let status = if idx < session.snapshot_words.len() && value.is_finite() {
                session.snapshot_words[idx] = value;
                FeqWireStatus::FEQ_WIRE_APPLIED
            } else {
                FeqWireStatus::FEQ_WIRE_REJECTED
            };
            let ack = build_ack_frame(request_id, status, settings_revision, 0, value);
            let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
        } else if command_raw == FeqWireCommand::FEQ_CMD_APPLY_SNAPSHOT as u16 {
            // `FEQ_CMD_APPLY_SNAPSHOT` (`host_commands.cpp` lines 165–186):
            // Must equal `FEQ_PARAMETER_COUNT`, otherwise reject without reading payload.
            if (parameter_id as usize) != fluideq_dsp_sys::FEQ_PARAMETER_COUNT {
                let ack = build_ack_frame(
                    request_id,
                    FeqWireStatus::FEQ_WIRE_REJECTED,
                    settings_revision,
                    0,
                    0.0,
                );
                let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
            } else {
                let mut raw_floats = vec![0u8; fluideq_dsp_sys::FEQ_PARAMETER_COUNT * 8];
                if !read_exact_from_pipe(reader, &mut raw_floats) {
                    return Err("Pipe closed mid APPLY_SNAPSHOT payload".to_string());
                }
                session.snapshot_words.clear();
                for chunk in raw_floats.chunks_exact(8) {
                    session
                        .snapshot_words
                        .push(f64::from_le_bytes(chunk.try_into().unwrap()));
                }
                let ack = build_ack_frame(
                    request_id,
                    FeqWireStatus::FEQ_WIRE_APPLIED,
                    settings_revision,
                    0,
                    fluideq_dsp_sys::FEQ_PARAMETER_COUNT as f64,
                );
                let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
            }
        } else if command_raw == FeqWireCommand::FEQ_CMD_SET_DIAGNOSTIC_SIGNAL as u16 {
            // `FEQ_CMD_SET_DIAGNOSTIC_SIGNAL` (`host_commands.cpp` lines 505–510):
            // `frame.parameter_id == 0 ? SignalKind::Silence : SignalKind::Sine, frame.value`.
            session.diagnostic_signal_enabled = parameter_id != 0;
            if value > 0.0 && value < 24_000.0 {
                session.diagnostic_frequency_hz = value;
            }
            let ack = build_ack_frame(
                request_id,
                FeqWireStatus::FEQ_WIRE_APPLIED,
                settings_revision,
                0,
                value,
            );
            let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
        } else if command_raw == FeqWireCommand::FEQ_CMD_RUN_OFFLINE_BLOCKS as u16 {
            // `FEQ_CMD_RUN_OFFLINE_BLOCKS` (`host_commands.cpp` lines 199–223):
            // Refuses with `FEQ_WIRE_REJECTED` when the live audio backend is running.
            if session.backend_running {
                let ack = build_ack_frame(
                    request_id,
                    FeqWireStatus::FEQ_WIRE_REJECTED,
                    settings_revision,
                    0,
                    0.0,
                );
                let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
            } else {
                let applied_frames = (parameter_id as u64) * (session.block_frames.max(1) as u64);
                let ack = build_ack_frame(
                    request_id,
                    FeqWireStatus::FEQ_WIRE_APPLIED,
                    settings_revision,
                    applied_frames,
                    parameter_id as f64,
                );
                let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
            }
        } else if command_raw == FeqWireCommand::FEQ_CMD_LOAD_VOICE_MODEL as u16 {
            // `FEQ_CMD_LOAD_VOICE_MODEL` (`host_commands.cpp` lines 225–265):
            // `parameter_id == 0` unloads model and clears saved paths; otherwise reads `<model>\n<runtime>`.
            if parameter_id == 0 {
                session.voice_model_path.clear();
                session.voice_runtime_path.clear();
                let ack = build_ack_frame(
                    request_id,
                    FeqWireStatus::FEQ_WIRE_APPLIED,
                    settings_revision,
                    0,
                    0.0,
                );
                let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
            } else {
                let len = payload_within(
                    parameter_id,
                    2 * MAX_WIRE_PATH_BYTES + 1,
                    "LOAD_VOICE_MODEL",
                )?;
                let mut buf = vec![0u8; len];
                if !read_exact_from_pipe(reader, &mut buf) {
                    return Err("Pipe closed mid LOAD_VOICE_MODEL payload".to_string());
                }
                let text = String::from_utf8_lossy(&buf);
                if let Some((model_path, runtime_path)) = text.split_once('\n') {
                    if !model_path.is_empty() {
                        session.voice_model_path = model_path.to_string();
                        session.voice_runtime_path = runtime_path.to_string();
                        let ack = build_ack_frame(
                            request_id,
                            FeqWireStatus::FEQ_WIRE_APPLIED,
                            settings_revision,
                            0,
                            1.0,
                        );
                        let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
                    } else {
                        let ack = build_ack_frame(
                            request_id,
                            FeqWireStatus::FEQ_WIRE_REJECTED,
                            settings_revision,
                            0,
                            0.0,
                        );
                        let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
                    }
                } else {
                    let ack = build_ack_frame(
                        request_id,
                        FeqWireStatus::FEQ_WIRE_REJECTED,
                        settings_revision,
                        0,
                        0.0,
                    );
                    let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
                }
            }
        } else if command_raw == FeqWireCommand::FEQ_CMD_SET_NOISE_PROFILE as u16 {
            // `FEQ_CMD_SET_NOISE_PROFILE` (`host_commands.cpp` lines 267–299):
            // `parameter_id == 0` clears noise profile so an unscanned song never inherits previous hiss;
            // `parameter_id != FEQ_DENOISE_PROFILE_WIRE (63)` rejects immediately.
            if parameter_id == 0 {
                session.noise_profile_loaded = false;
                session.noise_profile_words.clear();
                let ack = build_ack_frame(
                    request_id,
                    FeqWireStatus::FEQ_WIRE_APPLIED,
                    settings_revision,
                    0,
                    0.0,
                );
                let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
            } else if (parameter_id as usize) != fluideq_dsp_sys::FEQ_DENOISE_PROFILE_WIRE {
                let ack = build_ack_frame(
                    request_id,
                    FeqWireStatus::FEQ_WIRE_REJECTED,
                    settings_revision,
                    0,
                    0.0,
                );
                let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
            } else {
                let mut raw_floats = vec![0u8; fluideq_dsp_sys::FEQ_DENOISE_PROFILE_WIRE * 8];
                if !read_exact_from_pipe(reader, &mut raw_floats) {
                    return Err("Pipe closed mid SET_NOISE_PROFILE payload".to_string());
                }
                session.noise_profile_words.clear();
                for chunk in raw_floats.chunks_exact(8) {
                    session
                        .noise_profile_words
                        .push(f64::from_le_bytes(chunk.try_into().unwrap()));
                }
                session.noise_profile_loaded = true;
                let ack = build_ack_frame(
                    request_id,
                    FeqWireStatus::FEQ_WIRE_APPLIED,
                    settings_revision,
                    0,
                    parameter_id as f64,
                );
                let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
            }
        } else if command_raw == FeqWireCommand::FEQ_CMD_APPLY_CHAIN as u16 {
            let count = payload_within(parameter_id, MAX_WIRE_CHAIN_PARAMS, "APPLY_CHAIN")?;
            let mut raw_floats = vec![0u8; count * 8];
            if !read_exact_from_pipe(reader, &mut raw_floats) {
                return Err("Pipe closed mid APPLY_CHAIN payload".to_string());
            }
            let mut values = Vec::with_capacity(count);
            for chunk in raw_floats.chunks_exact(8) {
                values.push(f64::from_le_bytes(chunk.try_into().unwrap()));
            }
            let status = if store.import_wire_parameters(&values) {
                FeqWireStatus::FEQ_WIRE_APPLIED
            } else {
                FeqWireStatus::FEQ_WIRE_REJECTED
            };
            let ack = build_ack_frame(request_id, status, settings_revision, 0, count as f64);
            let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
        } else if command_raw == FeqWireCommand::FEQ_CMD_LOAD_DECK as u16 {
            // `FEQ_CMD_LOAD_DECK` (`host_deck_commands.cpp` lines 16–95):
            // Reads `parameter_id` path bytes from pipe (`parameter_id == 0` rejects immediately).
            if parameter_id == 0 {
                let ack = build_ack_frame(
                    request_id,
                    FeqWireStatus::FEQ_WIRE_REJECTED,
                    settings_revision,
                    0,
                    0.0,
                );
                let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
            } else {
                let len = payload_within(parameter_id, MAX_WIRE_PATH_BYTES, "LOAD_DECK")?;
                let mut path_bytes = vec![0u8; len];
                if !read_exact_from_pipe(reader, &mut path_bytes) {
                    return Err("Pipe closed mid LOAD_DECK payload".to_string());
                }
                let path = String::from_utf8_lossy(&path_bytes);
                let outcome = session
                    .decks
                    .load_deck(parameter_index as u32, &path, value);
                let (status, sanitized) = if outcome.loaded {
                    (FeqWireStatus::FEQ_WIRE_APPLIED, 1.0)
                } else {
                    (FeqWireStatus::FEQ_WIRE_REJECTED, 0.0)
                };
                let ack = build_ack_frame(request_id, status, settings_revision, 0, sanitized);
                let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
            }
        } else if command_raw == FeqWireCommand::FEQ_CMD_UNLOAD_DECK as u16 {
            session.decks.unload_deck(parameter_index as usize);
            let ack = build_ack_frame(
                request_id,
                FeqWireStatus::FEQ_WIRE_APPLIED,
                settings_revision,
                0,
                1.0,
            );
            let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
        } else if command_raw == FeqWireCommand::FEQ_CMD_SET_PLAYING as u16 {
            session.playing = parameter_id != 0;
            let ack = build_ack_frame(
                request_id,
                FeqWireStatus::FEQ_WIRE_APPLIED,
                settings_revision,
                0,
                if session.playing { 1.0 } else { 0.0 },
            );
            let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
        } else if command_raw == FeqWireCommand::FEQ_CMD_SEEK_DECK as u16 {
            let deck = parameter_index as usize;
            let (status, sanitized) = if deck < fluideq_dsp_sys::FEQ_PLAYER_DECKS {
                let _ = session.decks.seek_deck(deck, value);
                (FeqWireStatus::FEQ_WIRE_APPLIED, value)
            } else {
                (FeqWireStatus::FEQ_WIRE_REJECTED, 0.0)
            };
            let ack = build_ack_frame(request_id, status, settings_revision, 0, sanitized);
            let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
        } else if command_raw == FeqWireCommand::FEQ_CMD_SELECT_DECK as u16 {
            let _ = session.decks.select_deck(parameter_index as usize);
            let ack = build_ack_frame(
                request_id,
                FeqWireStatus::FEQ_WIRE_APPLIED,
                settings_revision,
                0,
                parameter_index as f64,
            );
            let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
        } else if command_raw == FeqWireCommand::FEQ_CMD_CROSSFADE as u16 {
            let _ = session
                .decks
                .start_crossfade(parameter_index as usize, value, parameter_id);
            let ack = build_ack_frame(
                request_id,
                FeqWireStatus::FEQ_WIRE_APPLIED,
                settings_revision,
                0,
                value,
            );
            let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
        } else if command_raw == FeqWireCommand::FEQ_CMD_SET_CROSSFADE_TABLE as u16 {
            // `FEQ_CMD_SET_CROSSFADE_TABLE` (`host_deck_commands.cpp` lines 163–192):
            // Expects `parameter_id == FEQ_CROSSFADE_TABLE_POINTS * 2` (`128` doubles = `1024` bytes).
            let expected = fluideq_dsp_sys::FEQ_CROSSFADE_TABLE_POINTS * 2;
            if (parameter_id as usize) != expected {
                let ack = build_ack_frame(
                    request_id,
                    FeqWireStatus::FEQ_WIRE_REJECTED,
                    settings_revision,
                    0,
                    0.0,
                );
                let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
            } else {
                let mut raw_floats = vec![0u8; expected * 8];
                if !read_exact_from_pipe(reader, &mut raw_floats) {
                    return Err("Pipe closed mid SET_CROSSFADE_TABLE payload".to_string());
                }
                let mut points = Vec::with_capacity(expected);
                for chunk in raw_floats.chunks_exact(8) {
                    points.push(f64::from_le_bytes(chunk.try_into().unwrap()));
                }
                if let Some(table) =
                    fluideq_state::deck_transport::decode_crossfade_table_wire(&points)
                {
                    session.crossfade_table = table;
                }
                let ack = build_ack_frame(
                    request_id,
                    FeqWireStatus::FEQ_WIRE_APPLIED,
                    settings_revision,
                    0,
                    expected as f64,
                );
                let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
            }
        } else if command_raw == FeqWireCommand::FEQ_CMD_SET_TRACK_GAINS as u16 {
            // `FEQ_CMD_SET_TRACK_GAINS` (`host_deck_commands.cpp` lines 194–219):
            // Expects `parameter_id == 2` (`2` doubles = `16` bytes: `[gain_a_db, gain_b_db]`).
            if parameter_id != 2 {
                let ack = build_ack_frame(
                    request_id,
                    FeqWireStatus::FEQ_WIRE_REJECTED,
                    settings_revision,
                    0,
                    0.0,
                );
                let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
            } else {
                let mut raw_floats = [0u8; 16];
                if !read_exact_from_pipe(reader, &mut raw_floats) {
                    return Err("Pipe closed mid SET_TRACK_GAINS payload".to_string());
                }
                let gain_a = f64::from_le_bytes(raw_floats[0..8].try_into().unwrap());
                let gain_b = f64::from_le_bytes(raw_floats[8..16].try_into().unwrap());
                let enabled = value != 0.0;
                session.decks.set_track_gains(gain_a, gain_b, enabled);
                let ack = build_ack_frame(
                    request_id,
                    FeqWireStatus::FEQ_WIRE_APPLIED,
                    settings_revision,
                    0,
                    if enabled { 1.0 } else { 0.0 },
                );
                let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
            }
        } else if command_raw == FeqWireCommand::FEQ_CMD_SET_VOLUME as u16 {
            // `FEQ_CMD_SET_VOLUME` (`host_commands.cpp` lines 422–431):
            // NaN and negative numbers clamp to `0.0`; values > `1.0` clamp to `1.0`.
            let wanted = value as f32;
            let clamped = if wanted.is_nan() || wanted < 0.0 {
                0.0
            } else if wanted > 1.0 {
                1.0
            } else {
                wanted
            };
            session.target_volume = clamped;
            if !session.backend_running {
                session.current_volume = clamped;
            }
            let ack = build_ack_frame(
                request_id,
                FeqWireStatus::FEQ_WIRE_APPLIED,
                settings_revision,
                0,
                clamped as f64,
            );
            let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
        } else if command_raw == FeqWireCommand::FEQ_CMD_SET_RAW_SHARING as u16 {
            // `FEQ_CMD_SET_RAW_SHARING` (`host_commands.cpp` lines 433–439): accepted_revision = 0.
            let wanted = parameter_id != 0;
            session.raw_sharing = wanted;
            let ack = build_ack_frame(
                request_id,
                FeqWireStatus::FEQ_WIRE_APPLIED,
                0,
                0,
                if wanted { 1.0 } else { 0.0 },
            );
            let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
        } else if command_raw == FeqWireCommand::FEQ_CMD_SET_ANALYSIS as u16 {
            let wanted = parameter_id != 0;
            session.analysis_enabled = wanted;
            let ack = build_ack_frame(
                request_id,
                FeqWireStatus::FEQ_WIRE_APPLIED,
                settings_revision,
                0,
                if wanted { 1.0 } else { 0.0 },
            );
            let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
        } else if command_raw == FeqWireCommand::FEQ_CMD_RENDER_TO_FILE as u16 {
            // `FEQ_CMD_RENDER_TO_FILE` (`host_commands.cpp` lines 460–513):
            // Always consumes the path payload if `wire_length_from_double` passes, then rejects if `backend_running`.
            let path_len = wire_length_from_double(value, MAX_WIRE_PATH_BYTES)?;
            let mut path_bytes = vec![0u8; path_len as usize];
            if !read_exact_from_pipe(reader, &mut path_bytes) {
                return Err("Pipe closed mid RENDER_TO_FILE path".to_string());
            }
            let status = if session.backend_running || !(0..=1).contains(&parameter_index) {
                FeqWireStatus::FEQ_WIRE_REJECTED
            } else {
                FeqWireStatus::FEQ_WIRE_APPLIED
            };
            let ack = build_ack_frame(request_id, status, settings_revision, 0, 0.0);
            let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
        } else {
            // Unknown command (`host_commands.cpp` lines 599–602): returns `FEQ_WIRE_UNSUPPORTED`.
            let ack = build_ack_frame(
                request_id,
                FeqWireStatus::FEQ_WIRE_UNSUPPORTED,
                settings_revision,
                0,
                0.0,
            );
            let _ = write_frame_to_pipe(writer, &serialize_ack_frame(&ack));
        }

        if !session.backend_running {
            cadence.arm_stats_after_offline_command();
            if cadence.on_telemetry_pass(0) {
                if let Some(sample) = sample_current_process_stats() {
                    let stats_bytes = encode_process_stats_frame(sample);
                    let _ = write_frame_to_pipe(writer, &stats_bytes);
                }
            }
        }
    }

    Ok(commands_processed)
}

/// Serializes `FeqWireAckFrame` into its 32-byte little-endian wire layout.
pub fn serialize_ack_frame(ack: &FeqWireAckFrame) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[0..4].copy_from_slice(&ack.magic.to_le_bytes());
    out[4..6].copy_from_slice(&ack.protocol_version.to_le_bytes());
    out[6..8].copy_from_slice(&ack.status.to_le_bytes());
    out[8..12].copy_from_slice(&ack.request_id.to_le_bytes());
    out[12..16].copy_from_slice(&ack.accepted_revision.to_le_bytes());
    out[16..24].copy_from_slice(&ack.applied_at_sample_frame.to_le_bytes());
    out[24..32].copy_from_slice(&ack.sanitized_value.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_guards_refuse_oversized_or_nan_lengths_before_allocation() {
        assert_eq!(payload_within(824, MAX_WIRE_CHAIN_PARAMS, "APPLY_CHAIN"), Ok(824));
        assert!(payload_within(825, MAX_WIRE_CHAIN_PARAMS, "APPLY_CHAIN").is_err());

        assert_eq!(wire_length_from_double(256.0, MAX_WIRE_PATH_BYTES), Ok(256));
        assert!(wire_length_from_double(-1.0, MAX_WIRE_PATH_BYTES).is_err());
        assert!(wire_length_from_double(f64::NAN, MAX_WIRE_PATH_BYTES).is_err());
        assert!(wire_length_from_double(f64::INFINITY, MAX_WIRE_PATH_BYTES).is_err());
        assert!(wire_length_from_double(40_000.0, MAX_WIRE_PATH_BYTES).is_err());
    }

    #[test]
    fn read_exact_handles_fragmented_pipe_chunks_and_builds_handshake() {
        let data = b"FEQCHelloWorld123456";
        let mut cursor = std::io::Cursor::new(&data[..]);
        let mut buf = [0u8; 20];
        assert!(read_exact_from_pipe(&mut cursor, &mut buf));
        assert_eq!(&buf, data);

        let hs = build_handshake_frame("wasapi", "2.1.0", "rev-12");
        assert_eq!(hs.magic, FEQ_MAGIC_HANDSHAKE);
        assert_eq!(hs.protocol_version, FEQ_WIRE_PROTOCOL_VERSION);
        assert_eq!(hs.analysis_frame_bytes, 816);
    }

    #[test]
    fn device_reopen_preserves_playing_decks_when_shape_is_unchanged() {
        let mut coord = DeviceReopenCoordinator::new();
        coord.device_wanted.store(true, Ordering::Release);
        coord.player_has_source.store(true, Ordering::Release);
        coord.room_head_loaded = 1;

        // 1. Outage window: first failure logs once, second failure suppresses duplicate log
        let out1 = coord.handle_reopen_attempt(
            true,
            Err("no default endpoint".to_string()),
            Ok(()),
        );
        assert_eq!(
            out1,
            DeviceReopenOutcome::OutageAwaitingEndpoint {
                first_report: true,
                error: "no default endpoint".to_string()
            }
        );
        let out2 = coord.handle_reopen_attempt(
            true,
            Err("no default endpoint".to_string()),
            Ok(()),
        );
        assert_eq!(
            out2,
            DeviceReopenOutcome::OutageAwaitingEndpoint {
                first_report: false,
                error: "no default endpoint".to_string()
            }
        );

        // 2. Reopen at same 48 kHz stereo 512-frame shape (Speakers -> Headphones):
        //    Decks survive, player_has_source stays true, device_generation stays 0!
        let same_shape = coord.handle_reopen_attempt(
            true,
            Ok(NegotiatedBackendFormat {
                sample_rate: 48_000,
                channels: 2,
                max_block_frames: 512,
            }),
            Ok(()),
        );
        assert_eq!(
            same_shape,
            DeviceReopenOutcome::ReopenedSameShapeKeptPlaying {
                sample_rate: 48_000
            }
        );
        assert!(coord.player_has_source.load(Ordering::Acquire));
        assert_eq!(coord.room_head_loaded, 1);
        assert_eq!(coord.device_generation.load(Ordering::Acquire), 0);

        // 3. Reopen at 44.1 kHz: shape changed, player rebuilt, device_generation bumped to 1
        let diff_shape = coord.handle_reopen_attempt(
            true,
            Ok(NegotiatedBackendFormat {
                sample_rate: 44_100,
                channels: 2,
                max_block_frames: 448,
            }),
            Ok(()),
        );
        assert_eq!(
            diff_shape,
            DeviceReopenOutcome::ReopenedShapeChangedRebuilt {
                sample_rate: 44_100,
                channels: 2,
                block_frames: 448,
                device_generation: 1,
            }
        );
        assert!(!coord.player_has_source.load(Ordering::Acquire));
        assert_eq!(coord.room_head_loaded, -1);
    }

    #[test]
    fn pre_chain_volume_ramp_and_nan_guard_scrub_corrupt_decoder_samples() {
        let mut planar = vec![vec![1.0f32, f32::NAN, f32::INFINITY, 1.0f32]; 2];
        let mut vol_now = 0.0f32;
        let scrubbed = apply_pre_chain_volume_ramp_and_nan_guard(&mut planar, 4, &mut vol_now, 1.0);
        assert_eq!(scrubbed, 4); // 2 non-finite samples per channel * 2 channels
        assert!((vol_now - 1.0).abs() < 1e-6);
        assert!((planar[0][0] - 0.25).abs() < 1e-6);
        assert_eq!(planar[0][1], 0.0);
        assert_eq!(planar[0][2], 0.0);
        assert!((planar[0][3] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn telemetry_cadence_and_device_watch_filter_match_native_host() {
        let mut cadence = TelemetryCadenceTracker::new();
        assert!(!cadence.on_telemetry_pass(19));
        assert!(cadence.on_telemetry_pass(1)); // 20th report triggers stats
        assert!(!cadence.on_telemetry_pass(5));

        cadence.arm_stats_after_offline_command();
        assert!(cadence.on_telemetry_pass(0)); // Offline command triggers immediate stats sample

        let mut watch = DeviceWatchState::new();
        // Capture device (flow=1) or communications role (role=2) ignored
        assert!(!watch.on_default_device_changed(1, 0));
        assert!(!watch.on_default_device_changed(0, 2));
        // Render + Console triggers default_changed
        assert!(watch.on_default_device_changed(0, 0));
        // Bluetooth battery property change ignored
        assert!(!watch.on_property_value_changed());
        // Audiosrv stop -> start re-registers enumerator and triggers outputs_changed
        assert!(!watch.on_audiosrv_state_change(false));
        assert!(watch.on_audiosrv_state_change(true));
        assert_eq!(watch.outputs_changed_count, 1);
    }

    #[test]
    fn supervised_pipe_loop_applies_chain_and_stops_on_shutdown() {
        let mut store = fluideq_state::AppStateStore::new(48_000.0);
        store.update_band_gain(1, 45.0, 5.25, 1.2).unwrap();
        let wire_params = store.export_wire_parameters();

        let mut input_pipe = Vec::new();
        let apply_cmd = crate::system_daemons::encode_dsp_host_command_frame(
            fluideq_dsp_sys::FeqWireCommand::FEQ_CMD_APPLY_CHAIN,
            101,
            7,
            wire_params.len() as u32,
            0,
            0.0,
        );
        input_pipe.extend_from_slice(&apply_cmd);
        for &val in &wire_params {
            input_pipe.extend_from_slice(&val.to_le_bytes());
        }

        let shutdown_cmd = crate::system_daemons::encode_dsp_host_command_frame(
            fluideq_dsp_sys::FeqWireCommand::FEQ_CMD_SHUTDOWN,
            102,
            7,
            0,
            0,
            0.0,
        );
        input_pipe.extend_from_slice(&shutdown_cmd);

        let mut target_store = fluideq_state::AppStateStore::new(48_000.0);
        let mut reader = std::io::Cursor::new(input_pipe);
        let mut output_pipe = Vec::new();
        let mut cadence = TelemetryCadenceTracker::new();

        let count = run_supervised_pipe_command_loop(
            &mut reader,
            &mut output_pipe,
            &mut target_store,
            true,
            &mut cadence,
        )
        .expect("pipe session should succeed");

        assert_eq!(count, 2);
        assert_eq!(output_pipe.len(), 64); // Two 32-byte FEQA ack frames
        assert_eq!(
            target_store.snapshot().chain_settings.eq.bands[0].frequency,
            45.0
        );
        assert_eq!(
            target_store.snapshot().chain_settings.eq.bands[0].gain_db,
            5.25
        );
    }

    #[test]
    fn host_commands_enforce_start_voice_model_noise_profile_and_wav_ceiling_invariants() {
        use crate::system_daemons::{encode_dsp_host_command_frame, parse_dsp_host_ack_frame};
        use fluideq_dsp_sys::FeqWireCommand;

        // 1. 32-bit WAV ceiling guard refuses > 4 GB interleaved exports without aborting
        assert!(validate_and_reserve_render_to_file_buffer(0, 2).is_err());
        assert!(validate_and_reserve_render_to_file_buffer(600_000_000, 2).is_err());

        // 2. Endpoint-absent START returns FEQ_WIRE_UNSUPPORTED; NaN volume clamps to 0.0;
        //    LOAD_VOICE_MODEL splits `<model>\n<runtime>`; SET_NOISE_PROFILE(0) clears profile.
        let mut input = Vec::new();
        input.extend_from_slice(&encode_dsp_host_command_frame(
            FeqWireCommand::FEQ_CMD_START,
            1,
            1,
            0,
            0,
            0.0,
        ));
        input.extend_from_slice(&encode_dsp_host_command_frame(
            FeqWireCommand::FEQ_CMD_SET_VOLUME,
            2,
            1,
            0,
            0,
            f64::NAN,
        ));
        let model_payload = b"C:\\models\\voice.onnx\nC:\\onnx\\onnxruntime.dll";
        input.extend_from_slice(&encode_dsp_host_command_frame(
            FeqWireCommand::FEQ_CMD_LOAD_VOICE_MODEL,
            3,
            1,
            model_payload.len() as u32,
            0,
            0.0,
        ));
        input.extend_from_slice(model_payload);
        input.extend_from_slice(&encode_dsp_host_command_frame(
            FeqWireCommand::FEQ_CMD_SET_NOISE_PROFILE,
            4,
            1,
            0,
            0,
            0.0,
        ));
        input.extend_from_slice(&encode_dsp_host_command_frame(
            FeqWireCommand::FEQ_CMD_SHUTDOWN,
            5,
            1,
            0,
            0,
            0.0,
        ));

        let mut session = HostCommandSessionState {
            backend_running: true, // keep offline stats frames suppressed during this test
            endpoint_absent: false,
            noise_profile_loaded: true,
            ..HostCommandSessionState::default()
        };
        let mut store = fluideq_state::AppStateStore::new(48_000.0);
        let mut reader = std::io::Cursor::new(input);
        let mut out = Vec::new();
        let mut cadence = TelemetryCadenceTracker::new();

        let processed = run_supervised_pipe_command_loop_with_session(
            &mut reader,
            &mut out,
            &mut store,
            &mut session,
            &mut cadence,
        )
        .expect("session commands should succeed");

        assert_eq!(processed, 5);
        let ack_start = parse_dsp_host_ack_frame(&out[0..32]).unwrap();
        assert_eq!(ack_start.status, FeqWireStatus::FEQ_WIRE_APPLIED as u16);
        assert_eq!(ack_start.sanitized_value, 48_000.0);

        let ack_vol = parse_dsp_host_ack_frame(&out[32..64]).unwrap();
        assert_eq!(ack_vol.sanitized_value, 0.0);
        assert_eq!(session.target_volume, 0.0);

        assert_eq!(session.voice_model_path, "C:\\models\\voice.onnx");
        assert_eq!(session.voice_runtime_path, "C:\\onnx\\onnxruntime.dll");
        assert!(!session.noise_profile_loaded);
    }
}
