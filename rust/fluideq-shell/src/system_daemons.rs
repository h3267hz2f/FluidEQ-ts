/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Native System Daemons & Windows Audio Protocol Bridge (`foreground-watch`, `output-watch`,
//! `volume-watch`, `media-watch`, `process-meter`, `remote-audio-capture`, `remote-audio-playback`).
//!
//! Implements the exact wire protocols defined in `/native/*`:
//! 1. `foreground-watch` (`/native/foreground-watch/src/main.cpp`):
//!    - Tab-separated lines (`front`, `open`, `listed`, `gone`) + `hold <pid>\n` process holding.
//! 2. `output-watch` (`/native/output-watch/src/main.cpp`):
//!    - Receives `outputs\n` whenever `IMMNotificationClient`, `DEVPKEY_Bluetooth_Battery`,
//!      or `Audiosrv` / `AudioEndpointBuilder` announces an endpoint change.
//! 3. `volume-watch` (`/native/volume-watch/src/main.cpp`):
//!    - Outputs: `ready\t2`, `volume\t<level 0..1>\t<muted 0|1>`, `applied\t<level>\t<muted>`, `none`.
//!    - Inputs: `set <level>\n`, `mute <0|1>\n`.
//! 4. `media-watch` (`/native/media-watch/src/media_line.h`):
//!    - Outputs: `"null"`, cover JSON line `{"cover":{"id":"<16-char hex>","type":"image/...","data":"<base64>"}}`,
//!      and reading JSON line `{"app":...,"title":...,"artist":...,"isPlaying":bool,"positionMs":i64,"durationMs":i64,"canNext":bool,"canPrevious":bool,"canSeek":bool,"playing":[...],"coverId":...}`.
//!    - Whole-second timeline deduplication (`reading_shape`) + magic-byte cover validation (`64..=3_145_728` bytes).
//! 5. `process-meter` (`/native/process-meter/src/main.cpp`):
//!    - Query: `<pid> c<pid> ...\n` (`c<pid>` expands child processes).
//!    - Reply: `<pid> <private_bytes|-> <working_bytes|-> <cpu_100ns_ticks|-> <exe_name>;...\n`.
//! 6. `remote-audio-capture` (`/native/remote-audio-capture/src/capture_queue.h`):
//!    - 24-byte binary `CaptureFrameHeader` (`magic = 0x314e414c` `'LAN1'`, `version = 2`).

use fluideq_dsp_sys::FeqChainSettings;
use fluideq_state::AppStateStore;
use serde::{Deserialize, Serialize};

pub const CAPTURE_FRAME_MAGIC: u32 = 0x314e_414c; // 'LAN1'
pub const CAPTURE_PIPE_VERSION: u32 = 2;
pub const CAPTURE_MAX_CHANNELS: u16 = 8;
pub const CAPTURE_MAX_FRAMES: u16 = 1920;
pub const CAPTURE_HEADER_BYTES: usize = 24;

pub const MEDIA_MIN_COVER_BYTES: usize = 64;
pub const MEDIA_MAX_COVER_BYTES: usize = 3 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowDesktopRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl WindowDesktopRect {
    pub fn parse_csv(text: &str) -> Option<Self> {
        if text.trim().is_empty() {
            return None;
        }
        let mut parts = text.split(',');
        let x = parts.next()?.parse().ok()?;
        let y = parts.next()?.parse().ok()?;
        let width = parts.next()?.parse().ok()?;
        let height = parts.next()?.parse().ok()?;
        Some(Self {
            x,
            y,
            width,
            height,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForegroundProgramRecord {
    pub pid: u32,
    pub rect: Option<WindowDesktopRect>,
    pub description: String,
    pub executable_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ForegroundWatchEvent {
    Front(ForegroundProgramRecord),
    Open(ForegroundProgramRecord),
    Listed,
    Gone { pid: u32 },
}

/// Sanitizes a single tab-delimited field by replacing `\t`, `\r`, and `\n` with `' '`,
/// matching `one_field` in `/native/foreground-watch/src/main.cpp` (lines 53–61).
pub fn sanitize_foreground_tab_field(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '\t' | '\r' | '\n' => ' ',
            other => other,
        })
        .collect()
}

/// Checks whether a process path is `ApplicationFrameHost.exe` (case-insensitive),
/// matching `is_frame_host` in `/native/foreground-watch/src/main.cpp` (lines 169–175).
pub fn is_uwp_application_frame_host(path: &str) -> bool {
    const SUFFIX: &str = "\\applicationframehost.exe";
    if path.len() < SUFFIX.len() {
        return false;
    }
    let tail = &path[path.len() - SUFFIX.len()..];
    tail.eq_ignore_ascii_case(SUFFIX)
}

/// Resolves the hosted UWP application PID when a top-level window is owned by `ApplicationFrameHost.exe`:
/// scans child windows for `Windows.UI.Core.CoreWindow` whose owning PID differs from `frame_host_pid`,
/// matching `window_program` in `/native/foreground-watch/src/main.cpp` (lines 202–221).
pub fn resolve_uwp_frame_host_pid(
    frame_host_pid: u32,
    frame_host_path: &str,
    child_windows: &[(&str, u32)],
) -> u32 {
    if !is_uwp_application_frame_host(frame_host_path) {
        return frame_host_pid;
    }
    for &(class_name, child_pid) in child_windows {
        if class_name == "Windows.UI.Core.CoreWindow"
            && child_pid != 0
            && child_pid != frame_host_pid
        {
            return child_pid;
        }
    }
    frame_host_pid
}

/// Parses a single line emitted by `/native/foreground-watch/src/main.cpp`.
pub fn parse_foreground_watch_line(line: &str) -> Option<ForegroundWatchEvent> {
    let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
    if trimmed == "listed" {
        return Some(ForegroundWatchEvent::Listed);
    }
    let mut fields = trimmed.split('\t');
    let kind = fields.next()?;
    match kind {
        "front" | "open" => {
            let pid: u32 = fields.next()?.parse().ok()?;
            let rect = WindowDesktopRect::parse_csv(fields.next().unwrap_or(""));
            let description = fields.next().unwrap_or("").to_string();
            let executable_path = fields.next().unwrap_or("").to_string();
            let record = ForegroundProgramRecord {
                pid,
                rect,
                description,
                executable_path,
            };
            if kind == "front" {
                Some(ForegroundWatchEvent::Front(record))
            } else {
                Some(ForegroundWatchEvent::Open(record))
            }
        }
        "gone" => {
            let pid: u32 = fields.next()?.parse().ok()?;
            Some(ForegroundWatchEvent::Gone { pid })
        }
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameProfileRule {
    pub profile_id: String,
    pub executable_match: String,
    pub footsteps_lift: f64,
    pub footsteps_calm: f64,
    pub enable_low_latency: bool,
}

/// Supervises `foreground-watch` events and automatically engages/restores Game Mode
/// while holding the active game process handle (`hold <pid>`) across Alt-Tab switches.
#[derive(Debug, Default)]
pub struct ForegroundGameSupervisor {
    rules: Vec<GameProfileRule>,
    held_game_pid: Option<u32>,
    active_profile_id: Option<String>,
    saved_settings_before_game: Option<FeqChainSettings>,
}

impl ForegroundGameSupervisor {
    pub fn new(rules: Vec<GameProfileRule>) -> Self {
        Self {
            rules,
            held_game_pid: None,
            active_profile_id: None,
            saved_settings_before_game: None,
        }
    }

    pub fn held_game_pid(&self) -> Option<u32> {
        self.held_game_pid
    }

    pub fn active_profile_id(&self) -> Option<&str> {
        self.active_profile_id.as_deref()
    }

    /// Processes a line from `foreground-watch`. If a command should be written back
    /// to `foreground-watch` stdin (e.g., `"hold 4812\n"` or `"hold 0\n"`), returns `Some(cmd)`.
    pub fn handle_line(&mut self, line: &str, store: &mut AppStateStore) -> Option<String> {
        let event = parse_foreground_watch_line(line)?;
        match event {
            ForegroundWatchEvent::Front(record) => {
                if self.held_game_pid.is_some() {
                    return None;
                }
                let path_lower = record.executable_path.to_ascii_lowercase();
                let desc_lower = record.description.to_ascii_lowercase();
                let matched_rule = self.rules.iter().find(|r| {
                    let needle = r.executable_match.to_ascii_lowercase();
                    path_lower.ends_with(&needle) || desc_lower.contains(&needle)
                })?;

                let mut current = store.snapshot().chain_settings;
                self.saved_settings_before_game = Some(current);
                if matched_rule.enable_low_latency {
                    current.low_latency = 1;
                }
                current.footsteps.enabled = 1;
                current.footsteps.lift = matched_rule.footsteps_lift.clamp(0.0, 1.0);
                current.footsteps.calm = matched_rule.footsteps_calm.clamp(0.0, 1.0);

                store.update_chain_settings(current);
                self.held_game_pid = Some(record.pid);
                self.active_profile_id = Some(matched_rule.profile_id.clone());
                Some(format!("hold {}\n", record.pid))
            }
            ForegroundWatchEvent::Gone { pid } => {
                if self.held_game_pid == Some(pid) {
                    self.held_game_pid = None;
                    self.active_profile_id = None;
                    if let Some(restored) = self.saved_settings_before_game.take() {
                        store.update_chain_settings(restored);
                    }
                    return Some("hold 0\n".to_string());
                }
                None
            }
            ForegroundWatchEvent::Open(_) | ForegroundWatchEvent::Listed => None,
        }
    }
}

// ============================================================================
// 3. `volume-watch` Protocol (`/native/volume-watch/src/main.cpp`)
// ============================================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum VolumeWatchEvent {
    Ready { protocol_version: u32 },
    Volume { level: f32, muted: bool },
    Applied { level: f32, muted: bool },
    None,
}

pub fn parse_volume_watch_line(line: &str) -> Option<VolumeWatchEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "none" {
        return Some(VolumeWatchEvent::None);
    }
    let mut parts = trimmed.split('\t');
    let tag = parts.next()?;
    match tag {
        "ready" => {
            let version: u32 = parts.next()?.parse().ok()?;
            Some(VolumeWatchEvent::Ready {
                protocol_version: version,
            })
        }
        "volume" | "applied" => {
            let raw_level: f32 = parts.next()?.parse().ok()?;
            if !raw_level.is_finite() {
                return None;
            }
            let level = raw_level.clamp(0.0, 1.0);
            let muted_flag = parts.next().unwrap_or("0");
            let muted = muted_flag == "1" || muted_flag.eq_ignore_ascii_case("true");
            if tag == "volume" {
                Some(VolumeWatchEvent::Volume { level, muted })
            } else {
                Some(VolumeWatchEvent::Applied { level, muted })
            }
        }
        _ => None,
    }
}

pub fn format_volume_set_command(level: f32) -> String {
    let clamped = if level.is_finite() {
        level.clamp(0.0, 1.0)
    } else {
        0.0
    };
    format!("set {:.4}\n", clamped)
}

pub fn format_volume_mute_command(muted: bool) -> String {
    format!("mute {}\n", if muted { 1 } else { 0 })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VolumeWatchCommand {
    Mute(bool),
    Set(f32),
}

/// Parses an inbound stdin command (`"mute 0"`, `"mute 1"`, or `"set <0..1>"`),
/// matching `take_command` in `/native/volume-watch/src/main.cpp` (lines 259–279).
pub fn take_volume_watch_command(raw_line: &str) -> Option<VolumeWatchCommand> {
    let line = raw_line.trim_end_matches(&['\r', '\n'][..]);
    if line == "mute 0" {
        return Some(VolumeWatchCommand::Mute(false));
    }
    if line == "mute 1" {
        return Some(VolumeWatchCommand::Mute(true));
    }
    if let Some(rest) = line.strip_prefix("set ") {
        if rest.is_empty() || rest.trim() != rest {
            return None;
        }
        let level: f32 = rest.parse().ok()?;
        if !level.is_finite() || !(0.0..=1.0).contains(&level) {
            return None;
        }
        return Some(VolumeWatchCommand::Set(level));
    }
    None
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumeWatchCoordinator {
    pub protocol_version: u32,
    pub endpoint_available: bool,
    pub level: f32,
    pub muted: bool,
}

impl Default for VolumeWatchCoordinator {
    fn default() -> Self {
        Self {
            protocol_version: 0,
            endpoint_available: false,
            level: 1.0,
            muted: false,
        }
    }
}

impl VolumeWatchCoordinator {
    pub fn handle_line(&mut self, line: &str) -> Option<VolumeWatchEvent> {
        let ev = parse_volume_watch_line(line)?;
        match ev {
            VolumeWatchEvent::Ready { protocol_version } => {
                self.protocol_version = protocol_version;
            }
            VolumeWatchEvent::Volume { level, muted }
            | VolumeWatchEvent::Applied { level, muted } => {
                self.endpoint_available = true;
                self.level = level;
                self.muted = muted;
            }
            VolumeWatchEvent::None => {
                self.endpoint_available = false;
            }
        }
        Some(ev)
    }
}

// ============================================================================
// 4. `media-watch` SMTC Protocol (`/native/media-watch/src/media_line.h`)
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemMediaReading {
    pub app: Option<String>,
    pub title: String,
    pub artist: Option<String>,
    pub is_playing: bool,
    pub position_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub can_next: bool,
    pub can_previous: bool,
    pub can_seek: bool,
    #[serde(default)]
    pub playing: Vec<String>,
    pub cover_id: Option<String>,
}

/// Converts 100ns `TimeSpan::Duration` ticks to milliseconds (`ticks / 10_000.0`) with ties-to-even rounding
/// and a 24-hour (`86_400_000` ms) sanity cap, matching `ticks_to_ms` in `/native/media-watch/src/media_line.h` (lines 95–107).
pub fn ticks_to_ms(ticks: i64) -> Option<i64> {
    if ticks < 0 {
        return None;
    }
    let ms = ((ticks as f64) / 10_000.0).round_ties_even() as i64;
    if !(0..=86_400_000).contains(&ms) {
        return None;
    }
    Some(ms)
}

/// Rounds a millisecond reading to the nearest whole second (`std::nearbyint(ms / 1000.0) * 1000`),
/// matching `whole_seconds` in `/native/media-watch/src/media_line.h` (lines 111–116).
pub fn whole_seconds_ms(ms: Option<i64>) -> Option<i64> {
    let v = ms?;
    Some((((v as f64) / 1000.0).round_ties_even() as i64) * 1000)
}

impl SystemMediaReading {
    /// Mirrors `whole_seconds(position_ms)` + `reading_shape` in `/native/media-watch/src/media_line.h` (lines 119–123):
    /// replaces `position_ms` with `whole_seconds_ms(position_ms)` while preserving `duration_ms` exact,
    /// so sub-second timeline drift does not trigger redundant state broadcasts.
    pub fn whole_second_shape(&self) -> SystemMediaReading {
        let mut shaped = self.clone();
        shaped.position_ms = whole_seconds_ms(self.position_ms);
        shaped
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemMediaCover {
    pub id: String,
    #[serde(rename = "type")]
    pub mime_type: String,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MediaCoverEnvelope {
    pub cover: SystemMediaCover,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaWatchEvent {
    Cleared,
    Cover(SystemMediaCover),
    Reading(SystemMediaReading),
}

/// Validates cover image size (`64..=3_145_728` bytes) and detects MIME type from magic bytes,
/// matching `cover_image_type` in `/native/media-watch/src/media_line.h`.
pub fn detect_cover_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() < MEDIA_MIN_COVER_BYTES || bytes.len() > MEDIA_MAX_COVER_BYTES {
        return None;
    }
    if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        return Some("image/jpeg");
    }
    if bytes.len() >= 8 && bytes[0..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
        return Some("image/png");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.len() >= 6 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a") {
        return Some("image/gif");
    }
    None
}

pub fn parse_media_watch_line(line: &str) -> Option<MediaWatchEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "null" {
        return Some(MediaWatchEvent::Cleared);
    }
    if let Ok(env) = serde_json::from_str::<MediaCoverEnvelope>(trimmed) {
        return Some(MediaWatchEvent::Cover(env.cover));
    }
    if let Ok(reading) = serde_json::from_str::<SystemMediaReading>(trimmed) {
        return Some(MediaWatchEvent::Reading(reading));
    }
    None
}

#[derive(Debug, Default)]
pub struct MediaWatchCoordinator {
    last_reading_shape: Option<SystemMediaReading>,
    latest_cover: Option<SystemMediaCover>,
}

impl MediaWatchCoordinator {
    pub fn latest_reading(&self) -> Option<&SystemMediaReading> {
        self.last_reading_shape.as_ref()
    }

    pub fn latest_cover(&self) -> Option<&SystemMediaCover> {
        self.latest_cover.as_ref()
    }

    /// Processes a line from `media-watch`, deduplicating whole-second timeline ticks
    /// and updating `AppStateStore::set_online_media_playing` so Cold-Residency never
    /// interrupts active online media sessions.
    pub fn handle_line(
        &mut self,
        line: &str,
        store: &mut AppStateStore,
    ) -> Option<MediaWatchEvent> {
        let ev = parse_media_watch_line(line)?;
        match &ev {
            MediaWatchEvent::Cleared => {
                self.last_reading_shape = None;
                store.set_online_media_playing(false);
                Some(ev)
            }
            MediaWatchEvent::Cover(cover) => {
                self.latest_cover = Some(cover.clone());
                Some(ev)
            }
            MediaWatchEvent::Reading(reading) => {
                let shaped = reading.whole_second_shape();
                let is_any_playing = reading.is_playing || !reading.playing.is_empty();
                store.set_online_media_playing(is_any_playing);
                if self.last_reading_shape.as_ref() == Some(&shaped) {
                    return None;
                }
                self.last_reading_shape = Some(shaped);
                Some(ev)
            }
        }
    }
}

// ============================================================================
// 5. `process-meter` Protocol (`/native/process-meter/src/main.cpp`)
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessMetricEntry {
    pub pid: u32,
    pub private_bytes: Option<u64>,
    pub working_set_bytes: Option<u64>,
    pub cpu_100ns_ticks: Option<u64>,
    pub executable_name: String,
}

/// Formats a `process-meter` query line (`<pid> c<tree_pid> ...\n`).
pub fn format_process_meter_query(direct_pids: &[u32], tree_root_pids: &[u32]) -> String {
    let mut tokens = Vec::with_capacity(direct_pids.len() + tree_root_pids.len());
    for pid in direct_pids {
        if *pid > 0 {
            tokens.push(pid.to_string());
        }
    }
    for pid in tree_root_pids {
        if *pid > 0 {
            tokens.push(format!("c{}", pid));
        }
    }
    let mut line = tokens.join(" ");
    line.push('\n');
    line
}

/// Parses a semicolon-separated response line from `/native/process-meter/src/main.cpp`:
/// `<pid> <private_bytes|-> <working_bytes|-> <cpu_100ns_ticks|-> <exe_name>;...`
pub fn parse_process_meter_reply(line: &str) -> Vec<ProcessMetricEntry> {
    let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut entries = Vec::new();
    for segment in trimmed.split(';') {
        if segment.trim().is_empty() {
            continue;
        }
        let mut fields = segment.splitn(5, ' ');
        let Some(pid_str) = fields.next() else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        let private_bytes = fields.next().and_then(|s| {
            if s == "-" {
                None
            } else {
                s.parse::<u64>().ok()
            }
        });
        let working_set_bytes = fields.next().and_then(|s| {
            if s == "-" {
                None
            } else {
                s.parse::<u64>().ok()
            }
        });
        let cpu_100ns_ticks = fields.next().and_then(|s| {
            if s == "-" {
                None
            } else {
                s.parse::<u64>().ok()
            }
        });
        let executable_name = fields.next().unwrap_or("").to_string();
        entries.push(ProcessMetricEntry {
            pid,
            private_bytes,
            working_set_bytes,
            cpu_100ns_ticks,
            executable_name,
        });
    }
    entries
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessMeterSummary {
    pub process_count: usize,
    pub total_private_bytes: u64,
    pub total_working_set_bytes: u64,
    pub entries: Vec<ProcessMetricEntry>,
}

pub fn summarize_process_meter_reply(line: &str) -> ProcessMeterSummary {
    let entries = parse_process_meter_reply(line);
    let total_private_bytes = entries.iter().filter_map(|e| e.private_bytes).sum();
    let total_working_set_bytes = entries.iter().filter_map(|e| e.working_set_bytes).sum();
    ProcessMeterSummary {
        process_count: entries.len(),
        total_private_bytes,
        total_working_set_bytes,
        entries,
    }
}

/// Sanitizes a process executable name for a semicolon-delimited reply line by replacing
/// `;`, `\n`, and `\r` with `'_'`, matching `reply_name` in `/native/process-meter/src/main.cpp` (lines 88–95).
pub fn sanitize_process_reply_name(name: &str) -> String {
    name.chars()
        .map(|ch| match ch {
            ';' | '\n' | '\r' => '_',
            other => other,
        })
        .collect()
}

/// Snapshot of a process entry from `CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)`
/// combined with `GetProcessTimes` creation timestamp (`100ns` ticks since 1601-01-01).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSnapshotItem {
    pub pid: u32,
    pub parent_pid: u32,
    pub created_100ns: u64,
    pub exe_name: String,
}

/// Expands all descendant PIDs of `root_pid` using BFS while guarding against Windows PID reuse
/// (`child_created >= parent_created` whenever both creation times are non-zero),
/// matching `children_of` in `/native/process-meter/src/main.cpp` (lines 97–137).
pub fn expand_process_tree_children(root_pid: u32, snapshot: &[ProcessSnapshotItem]) -> Vec<u32> {
    if root_pid == 0 {
        return Vec::new();
    }
    let created_of = |pid: u32| -> u64 {
        snapshot
            .iter()
            .find(|item| item.pid == pid)
            .map(|item| item.created_100ns)
            .unwrap_or(0)
    };
    let mut tree = vec![root_pid];
    let mut cursor = 0usize;
    while cursor < tree.len() {
        let parent = tree[cursor];
        cursor += 1;
        let parent_created = created_of(parent);
        for item in snapshot {
            if item.pid == 0 || item.parent_pid != parent {
                continue;
            }
            if tree.contains(&item.pid) {
                continue;
            }
            if parent_created != 0 && item.created_100ns != 0 && item.created_100ns < parent_created
            {
                continue;
            }
            tree.push(item.pid);
        }
    }
    tree
}

// ============================================================================
// 6. `remote-audio-capture` 24-Byte Frame Header (`/native/remote-audio-capture/src/capture_queue.h`)
// ============================================================================

/// Exact 24-byte `CaptureFrameHeader` from `/native/remote-audio-capture/src/capture_queue.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
            sample_rate: 48_000,
            channels: 2,
            frames: 0,
            payload_bytes: 0,
        }
    }
}

impl CaptureFrameHeader {
    /// Serializes the header into its exact 24-byte little-endian pipe representation.
    pub fn to_le_bytes(&self) -> [u8; CAPTURE_HEADER_BYTES] {
        let mut out = [0u8; CAPTURE_HEADER_BYTES];
        out[0..4].copy_from_slice(&self.magic.to_le_bytes());
        out[4..8].copy_from_slice(&self.kind.to_le_bytes());
        out[8..12].copy_from_slice(&self.sequence.to_le_bytes());
        out[12..16].copy_from_slice(&self.sample_rate.to_le_bytes());
        out[16..18].copy_from_slice(&self.channels.to_le_bytes());
        out[18..20].copy_from_slice(&self.frames.to_le_bytes());
        out[20..24].copy_from_slice(&self.payload_bytes.to_le_bytes());
        out
    }

    /// Parses and validates a 24-byte little-endian `CaptureFrameHeader`.
    pub fn from_le_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < CAPTURE_HEADER_BYTES {
            return None;
        }
        let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if magic != CAPTURE_FRAME_MAGIC {
            return None;
        }
        let kind = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let sequence = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        let sample_rate = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
        let channels = u16::from_le_bytes([bytes[16], bytes[17]]);
        let frames = u16::from_le_bytes([bytes[18], bytes[19]]);
        let payload_bytes = u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);

        if channels > CAPTURE_MAX_CHANNELS || frames > CAPTURE_MAX_FRAMES {
            return None;
        }
        Some(Self {
            magic,
            kind,
            sequence,
            sample_rate,
            channels,
            frames,
            payload_bytes,
        })
    }
}

// ============================================================================
// 7. `remote-audio-playback` 24-Byte Protocol (`/native/remote-audio-playback/src/replies.h`)
// ============================================================================

pub const PLAYBACK_HEADER_MAGIC: u32 = 0x3150_4c46; // 'FLP1'
pub const PLAYBACK_PROTOCOL_VERSION: u32 = 2;
pub const PLAYBACK_MAX_PEERS: u32 = 8;
pub const PLAYBACK_HEADER_BYTES: usize = 24;

/// Exact 24-byte `PlaybackHeader` from `/native/remote-audio-playback/src/replies.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    pub fn to_le_bytes(&self) -> [u8; PLAYBACK_HEADER_BYTES] {
        let mut out = [0u8; PLAYBACK_HEADER_BYTES];
        out[0..4].copy_from_slice(&self.magic.to_le_bytes());
        out[4..8].copy_from_slice(&self.kind.to_le_bytes());
        out[8..12].copy_from_slice(&self.id.to_le_bytes());
        out[12..16].copy_from_slice(&self.rate.to_le_bytes());
        out[16..18].copy_from_slice(&self.channels.to_le_bytes());
        out[18..20].copy_from_slice(&self.frames.to_le_bytes());
        out[20..24].copy_from_slice(&self.bytes.to_le_bytes());
        out
    }

    pub fn from_le_bytes(raw: &[u8]) -> Option<Self> {
        if raw.len() < PLAYBACK_HEADER_BYTES {
            return None;
        }
        let magic = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        if magic != PLAYBACK_HEADER_MAGIC {
            return None;
        }
        Some(Self {
            magic,
            kind: u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]),
            id: u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]),
            rate: u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]),
            channels: u16::from_le_bytes([raw[16], raw[17]]),
            frames: u16::from_le_bytes([raw[18], raw[19]]),
            bytes: u32::from_le_bytes([raw[20], raw[21], raw[22], raw[23]]),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PlaybackReplyEvent {
    Ready {
        protocol_version: u32,
    },
    CommandResult {
        id: u32,
        hresult: u32,
    },
    OutputFailedOrBadCommand {
        id: u32,
        hresult: u32,
    },
    PeerMeter {
        peer_id: u32,
        sample_rate: u32,
        buffered_ms: f64,
        peak: f64,
        rms: f64,
    },
    OutputFormat {
        sample_rate: u32,
        buffered_ms: f64,
    },
    CaptureExited {
        capture_id: u32,
        exit_code: u32,
    },
}

/// Decodes a reply packet (`PlaybackHeader` + optional payload) emitted by `remote-audio-playback`.
pub fn parse_playback_reply_packet(
    header: &PlaybackHeader,
    payload: &[u8],
) -> Option<PlaybackReplyEvent> {
    if header.magic != PLAYBACK_HEADER_MAGIC || payload.len() < (header.bytes as usize) {
        return None;
    }
    match header.kind {
        1 => Some(PlaybackReplyEvent::Ready {
            protocol_version: header.rate,
        }),
        3 => Some(PlaybackReplyEvent::CommandResult {
            id: header.id,
            hresult: header.rate,
        }),
        4 => Some(PlaybackReplyEvent::OutputFailedOrBadCommand {
            id: header.id,
            hresult: header.rate,
        }),
        5 => {
            if header.bytes != 24 {
                return None;
            }
            let buffered_ms = f64::from_le_bytes(payload[0..8].try_into().ok()?);
            let peak = f64::from_le_bytes(payload[8..16].try_into().ok()?);
            let rms = f64::from_le_bytes(payload[16..24].try_into().ok()?);
            Some(PlaybackReplyEvent::PeerMeter {
                peer_id: header.id,
                sample_rate: header.rate,
                buffered_ms,
                peak,
                rms,
            })
        }
        6 => {
            if header.bytes != 8 {
                return None;
            }
            let buffered_ms = f64::from_le_bytes(payload[0..8].try_into().ok()?);
            Some(PlaybackReplyEvent::OutputFormat {
                sample_rate: header.rate,
                buffered_ms,
            })
        }
        9 => Some(PlaybackReplyEvent::CaptureExited {
            capture_id: header.id,
            exit_code: header.rate,
        }),
        _ => None,
    }
}

/// Encodes Command 1 (`open output <guid>`) for `remote-audio-playback`.
pub fn encode_playback_open_output(request_id: u32, guid_braced: &str) -> Option<Vec<u8>> {
    let bytes = guid_braced.as_bytes();
    if !bytes.is_empty() && bytes.len() != 38 {
        return None;
    }
    let header = PlaybackHeader {
        magic: PLAYBACK_HEADER_MAGIC,
        kind: 1,
        id: request_id,
        rate: 0,
        channels: 0,
        frames: 0,
        bytes: bytes.len() as u32,
    };
    let mut packet = Vec::with_capacity(PLAYBACK_HEADER_BYTES + bytes.len());
    packet.extend_from_slice(&header.to_le_bytes());
    packet.extend_from_slice(bytes);
    Some(packet)
}

// ============================================================================
// 8. `output-watch` & Windows Render Endpoint Coordinator (`/native/output-watch/src/main.cpp`)
// ============================================================================

pub const FLUID_ENGINE_CLSID: &str = "{B7E2C4D1-5A8F-4C3E-9D2B-6F1A0C8E7D34}";
pub const EQUALIZER_APO_CLSIDS: [&str; 2] = [
    "{EACD2258-FCAC-4FF4-B36D-419E924A6D79}",
    "{EC1CC9CE-FAED-4822-828A-82A81A6F018F}",
];
pub const ENGINE_FX_SLOT_VALUES: [&str; 8] = [
    "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},15",
    "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},14",
    "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},13",
    "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},7",
    "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},6",
    "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},5",
    "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},2",
    "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},1",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsRenderDevice {
    pub id: String,
    pub name: String,
    pub guid: String,
    pub is_default: bool,
    pub is_active: bool,
    pub is_equalizer_apo_attached: Option<bool>,
    pub is_fluid_engine_attached: Option<bool>,
    pub can_host_effects: bool,
    pub effects_enabled: Option<bool>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub battery_percent: Option<i32>,
}

#[derive(Debug, Default)]
pub struct OutputWatchCoordinator {
    pub refresh_generation: u64,
    pub devices: Vec<WindowsRenderDevice>,
    pub default_device_guid: Option<String>,
}

impl OutputWatchCoordinator {
    /// Handles a line from `/native/output-watch/src/main.cpp` (`"outputs\n"`).
    /// Returns `true` when the endpoint list must be re-queried.
    pub fn handle_line(&mut self, line: &str) -> bool {
        if line.trim() == "outputs" {
            self.refresh_generation += 1;
            true
        } else {
            false
        }
    }

    /// Updates the known render endpoints from JSON (`windows-audio-devices.ps1` or native COM enumeration)
    /// and returns `Some(new_default_guid)` if the default playback device switched.
    pub fn update_devices_json(&mut self, json_text: &str) -> Result<Option<String>, String> {
        let trimmed = json_text.trim();
        let parsed: Vec<WindowsRenderDevice> = if trimmed.starts_with('[') {
            serde_json::from_str(trimmed)
                .map_err(|e| format!("Failed to parse device array JSON: {}", e))?
        } else if trimmed.starts_with('{') {
            let single: WindowsRenderDevice = serde_json::from_str(trimmed)
                .map_err(|e| format!("Failed to parse single device JSON: {}", e))?;
            vec![single]
        } else {
            Vec::new()
        };

        let new_default = parsed.iter().find(|d| d.is_default).map(|d| d.guid.clone());
        let switched = if new_default != self.default_device_guid {
            self.default_device_guid = new_default.clone();
            new_default
        } else {
            None
        };
        self.devices = parsed;
        Ok(switched)
    }

    /// Updates the known render endpoints and automatically switches `AppStateStore`
    /// onto the new default endpoint's saved EQ & rack profile.
    pub fn update_devices_and_switch_store(
        &mut self,
        json_text: &str,
        store: &mut AppStateStore,
    ) -> Result<Option<String>, String> {
        let switched = self.update_devices_json(json_text)?;
        if let Some(ref guid) = switched {
            let name = self
                .devices
                .iter()
                .find(|d| &d.guid == guid)
                .map(|d| d.name.as_str())
                .unwrap_or("Default Output");
            let _ = store.switch_active_output_device(guid, name);
        }
        Ok(switched)
    }
}

// ============================================================================
// 9. `lighting-host` Binary Wire Protocol (`/native/lighting-host/src/wire.h`)
// ============================================================================

pub const LIGHTING_WIRE_MAGIC: u32 = 0x3148_4C46; // 'FLH1'
pub const LIGHTING_PROTOCOL_VERSION: u32 = 1;
pub const LIGHTING_COLOURS_FRAME_KIND: u32 = 1;
pub const LIGHTING_HEADER_BYTES: usize = 12;
pub const LIGHTING_MAX_LAMPS: u32 = 4096;
pub const LIGHTING_MAX_PAYLOAD_BYTES: u32 = 8 + LIGHTING_MAX_LAMPS * 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LightingColoursFrame {
    pub device_index: u32,
    pub lamp_count: u32,
    pub rgb_bytes: Vec<u8>,
}

/// Encodes a `ColoursFrame` (`FLH1`, 12-byte header + `u32 device` + `u32 lamp_count` + `R,G,B` triplets)
/// matching `/native/lighting-host/src/wire.h`.
pub fn encode_lighting_colours_frame(
    device_index: u32,
    rgb_triplets: &[[u8; 3]],
) -> Option<Vec<u8>> {
    let lamp_count = rgb_triplets.len() as u32;
    if lamp_count > LIGHTING_MAX_LAMPS {
        return None;
    }
    let payload_bytes = 8u32 + lamp_count * 3;
    let mut out = Vec::with_capacity(LIGHTING_HEADER_BYTES + (payload_bytes as usize));
    out.extend_from_slice(&LIGHTING_WIRE_MAGIC.to_le_bytes());
    out.extend_from_slice(&LIGHTING_COLOURS_FRAME_KIND.to_le_bytes());
    out.extend_from_slice(&payload_bytes.to_le_bytes());
    out.extend_from_slice(&device_index.to_le_bytes());
    out.extend_from_slice(&lamp_count.to_le_bytes());
    for rgb in rgb_triplets {
        out.extend_from_slice(rgb);
    }
    Some(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LightingReadResult {
    NeedMore,
    Colours(LightingColoursFrame),
    Broken,
}

/// Streaming frame reader matching `fluideq_lighting::FrameReader` in `/native/lighting-host/src/wire.h`.
#[derive(Debug, Default)]
pub struct LightingFrameReader {
    buffer: Vec<u8>,
    consumed: usize,
    broken: bool,
}

impl LightingFrameReader {
    pub fn feed(&mut self, bytes: &[u8]) {
        if self.consumed > 0 {
            self.buffer.drain(0..self.consumed);
            self.consumed = 0;
        }
        self.buffer.extend_from_slice(bytes);
    }

    pub fn next_frame(&mut self) -> LightingReadResult {
        if self.broken {
            return LightingReadResult::Broken;
        }
        let available = self.buffer.len().saturating_sub(self.consumed);
        if available < LIGHTING_HEADER_BYTES {
            return LightingReadResult::NeedMore;
        }
        let head = &self.buffer[self.consumed..];
        let magic = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
        let kind = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
        let payload = u32::from_le_bytes([head[8], head[9], head[10], head[11]]);
        if magic != LIGHTING_WIRE_MAGIC
            || kind != LIGHTING_COLOURS_FRAME_KIND
            || payload > LIGHTING_MAX_PAYLOAD_BYTES
            || payload < 8
        {
            self.broken = true;
            return LightingReadResult::Broken;
        }
        let total_frame_bytes = LIGHTING_HEADER_BYTES + (payload as usize);
        if available < total_frame_bytes {
            return LightingReadResult::NeedMore;
        }
        let body = &head[LIGHTING_HEADER_BYTES..total_frame_bytes];
        let device_index = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
        let lamp_count = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
        if lamp_count > LIGHTING_MAX_LAMPS || 8 + (lamp_count as usize) * 3 != (payload as usize) {
            self.broken = true;
            return LightingReadResult::Broken;
        }
        let rgb_bytes = body[8..].to_vec();
        self.consumed += total_frame_bytes;
        LightingReadResult::Colours(LightingColoursFrame {
            device_index,
            lamp_count,
            rgb_bytes,
        })
    }
}

// ============================================================================
// 10. `wallpaper` 48-Piece Desktop Occlusion Engine (`/native/wallpaper/desktop.h`)
// ============================================================================

pub const WALLPAPER_MAX_SHOWING_PIECES: usize = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopBoxRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl DesktopBoxRect {
    pub fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WallpaperCoverState {
    None,
    Owner,
    Others,
}

impl WallpaperCoverState {
    pub fn as_protocol_line(&self) -> &'static str {
        match self {
            WallpaperCoverState::None => "active\n",
            WallpaperCoverState::Owner => "paused-owner\n",
            WallpaperCoverState::Others => "paused\n",
        }
    }
}

/// Exact port of `wallpaper::covered` in `/native/wallpaper/desktop.h`:
/// Subtracts each opaque window rectangle `windows` from `area` (up to `48` visible fragments).
/// Returns `true` when zero desktop fragments remain visible.
pub fn desktop_area_covered(area: DesktopBoxRect, windows: &[DesktopBoxRect]) -> bool {
    if area.is_empty() {
        return false;
    }
    let mut showing = Vec::with_capacity(WALLPAPER_MAX_SHOWING_PIECES);
    let mut kept = Vec::with_capacity(WALLPAPER_MAX_SHOWING_PIECES);
    showing.push(area);

    for cut in windows {
        if showing.is_empty() {
            break;
        }
        kept.clear();
        for showing_piece in &showing {
            let overlap = DesktopBoxRect {
                left: cut.left.max(showing_piece.left),
                top: cut.top.max(showing_piece.top),
                right: cut.right.min(showing_piece.right),
                bottom: cut.bottom.min(showing_piece.bottom),
            };
            if overlap.is_empty() {
                if kept.len() == WALLPAPER_MAX_SHOWING_PIECES {
                    return false;
                }
                kept.push(*showing_piece);
            } else {
                let around = [
                    DesktopBoxRect {
                        left: showing_piece.left,
                        top: showing_piece.top,
                        right: showing_piece.right,
                        bottom: overlap.top,
                    },
                    DesktopBoxRect {
                        left: showing_piece.left,
                        top: overlap.bottom,
                        right: showing_piece.right,
                        bottom: showing_piece.bottom,
                    },
                    DesktopBoxRect {
                        left: showing_piece.left,
                        top: overlap.top,
                        right: overlap.left,
                        bottom: overlap.bottom,
                    },
                    DesktopBoxRect {
                        left: overlap.right,
                        top: overlap.top,
                        right: showing_piece.right,
                        bottom: overlap.bottom,
                    },
                ];
                for part in around {
                    if part.is_empty() {
                        continue;
                    }
                    if kept.len() == WALLPAPER_MAX_SHOWING_PIECES {
                        return false;
                    }
                    kept.push(part);
                }
            }
        }
        std::mem::swap(&mut showing, &mut kept);
    }
    showing.is_empty()
}

/// Exact port of `wallpaper::cover_of` in `/native/wallpaper/desktop.h`.
pub fn desktop_cover_of(
    area: DesktopBoxRect,
    all_windows: &[DesktopBoxRect],
    other_windows: &[DesktopBoxRect],
) -> WallpaperCoverState {
    if !desktop_area_covered(area, all_windows) {
        return WallpaperCoverState::None;
    }
    if desktop_area_covered(area, other_windows) {
        WallpaperCoverState::Others
    } else {
        WallpaperCoverState::Owner
    }
}

/// Exact port of `wallpaper::stacked_between` in `/native/wallpaper/desktop.h` (lines 14–37):
/// Returns the z-order slice strictly between `top` and `bottom` in `order`.
/// Returns an empty `Vec` if `top == 0`, `bottom == 0`, `top == bottom`, or if `top` does not precede `bottom`.
pub fn stacked_between<T: Copy + PartialEq + Default>(top: T, bottom: T, order: &[T]) -> Vec<T> {
    let zero = T::default();
    if top == zero || bottom == zero || top == bottom {
        return Vec::new();
    }
    let Some(first_idx) = order.iter().position(|&item| item == top) else {
        return Vec::new();
    };
    let tail = &order[first_idx + 1..];
    let Some(last_rel_idx) = tail.iter().position(|&item| item == bottom) else {
        return Vec::new();
    };
    tail[..last_rel_idx].to_vec()
}

/// Evaluates whether a top-level window is an opaque, uncloaked rectangular window that hides the desktop,
/// matching `hides_desktop` in `/native/wallpaper/main.cpp` (lines 226–259).
pub fn window_hides_desktop(
    visible: bool,
    iconic: bool,
    cloaked: bool,
    is_tool_window: bool,
    is_no_activate: bool,
    layered_alpha: Option<u8>,
    has_complex_region: bool,
) -> bool {
    if !visible || iconic || cloaked {
        return false;
    }
    if is_tool_window || is_no_activate {
        return false;
    }
    if let Some(alpha) = layered_alpha {
        if alpha < 240 {
            return false;
        }
    }
    if has_complex_region {
        return false;
    }
    true
}

/// Desktop window tree state machine (`Progman` / `SHELLDLL_DefView` / `WorkerW`) supporting both
/// legacy Windows 10/11 `WorkerW` and Windows 11 24H2 (`26100+`) raised `Progman` layered child layouts,
/// matching `Desktop` in `/native/wallpaper/main.cpp` (lines 66–224).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WallpaperDesktopHost {
    pub progman: usize,
    pub def_view: usize,
    pub worker: usize,
    pub raised_layered: bool,
}

impl WallpaperDesktopHost {
    /// Validates that the discovered desktop host handles are still intact (`Desktop::valid` in `main.cpp` lines 169–196).
    pub fn is_valid(
        &self,
        def_view_parent: usize,
        worker_parent: usize,
        wallpaper_parent: usize,
    ) -> bool {
        if self.progman == 0 || self.def_view == 0 || self.worker == 0 {
            return false;
        }
        if self.raised_layered {
            if self.worker != self.progman {
                return false;
            }
            if def_view_parent != self.progman || wallpaper_parent != self.progman {
                return false;
            }
            return true;
        }
        if def_view_parent == self.progman {
            return worker_parent == 0 && wallpaper_parent == self.worker;
        }
        if def_view_parent == self.worker {
            return worker_parent == self.progman && wallpaper_parent == self.worker;
        }
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallpaperStdinCommand {
    Show,
    Hide,
}

/// Parses a 4-byte command token (`"show"` or `"hide"`) from `wallpaper` stdin,
/// matching `read_input` in `/native/wallpaper/main.cpp` (lines 422–450).
pub fn parse_wallpaper_stdin_command(token: &str) -> Option<WallpaperStdinCommand> {
    match token.trim() {
        "show" => Some(WallpaperStdinCommand::Show),
        "hide" => Some(WallpaperStdinCommand::Hide),
        _ => None,
    }
}

/// Strictly parses decimal CLI arguments `<wallpaper_hwnd> [owner_pid]` for `wallpaper.exe`,
/// matching `decimal` and `wmain` in `/native/wallpaper/main.cpp` (lines 382–486).
pub fn parse_wallpaper_cli_args(args: &[&str]) -> Option<(u64, u32)> {
    if args.is_empty() || args.len() > 2 {
        return None;
    }
    let parse_strict_u64 = |text: &str| -> Option<u64> {
        if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let val = text.parse::<u64>().ok()?;
        if val == 0 {
            None
        } else {
            Some(val)
        }
    };
    let hwnd = parse_strict_u64(args[0])?;
    let owner_pid = if args.len() == 2 {
        u32::try_from(parse_strict_u64(args[1])?).ok()?
    } else {
        0
    };
    Some((hwnd, owner_pid))
}

// ============================================================================
// 11. `dsp-host` Binary Command & Telemetry Frame Codec (`/native/dsp-host/src/wire.h`)
// ============================================================================

use fluideq_dsp_sys::{
    FeqChainLatencyParts, FeqRoomReport, FeqWireAckFrame, FeqWireCommand, FeqWireCommandFrame,
    FeqWireStatsFrame, FEQ_MAGIC_ACK, FEQ_MAGIC_ANALYSIS, FEQ_MAGIC_COMMAND, FEQ_MAGIC_STATS,
    FEQ_ROOM_REPORT_TAG, FEQ_WIRE_PROTOCOL_VERSION,
};
use std::sync::atomic::{AtomicU32, Ordering};

/// Bounded, lock-free 10-word seqlock snapshot from the audio callback.
/// Exact port of `ProcessingLatency` in `/native/dsp-host/src/processing_latency.h`.
pub struct ProcessingLatencySnapshot {
    sequence: AtomicU32,
    values: [AtomicU32; 10],
}

impl Default for ProcessingLatencySnapshot {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessingLatencySnapshot {
    pub const fn new() -> Self {
        Self {
            sequence: AtomicU32::new(0),
            values: [
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
        }
    }

    pub fn clear(&self) {
        self.sequence.store(0, Ordering::Release);
    }

    /// Publishes `[total_frames, eq, restoration, leveler, room, bass_punch, maximizer, headroom, deesser, processed_stages]`.
    pub fn publish(&self, total_frames: u32, parts: &FeqChainLatencyParts, processed_stages: u32) {
        let words: [u32; 10] = [
            total_frames,
            parts.eq,
            parts.restoration,
            parts.leveler,
            parts.room,
            parts.bass_punch,
            parts.maximizer,
            parts.headroom,
            parts.deesser,
            processed_stages,
        ];
        self.sequence.fetch_add(1, Ordering::AcqRel);
        for (slot, &w) in self.values.iter().zip(words.iter()) {
            slot.store(w, Ordering::Relaxed);
        }
        self.sequence.fetch_add(1, Ordering::Release);
    }

    /// Publishes an all-zero latency snapshot (used when `raw` bypass is active).
    pub fn publish_raw_zero(&self) {
        let zero_parts = FeqChainLatencyParts::default();
        self.publish(0, &zero_parts, 0);
    }

    /// Lock-free seqlock read: returns `Some([u32; 10])` if a consistent even generation was read.
    pub fn read(&self) -> Option<[u32; 10]> {
        let before = self.sequence.load(Ordering::Acquire);
        if before == 0 || (before & 1) != 0 {
            return None;
        }
        let mut out = [0u32; 10];
        for (i, slot) in self.values.iter().enumerate() {
            out[i] = slot.load(Ordering::Relaxed);
        }
        let after = self.sequence.load(Ordering::Acquire);
        if before == after {
            Some(out)
        } else {
            None
        }
    }
}

/// Audio-owned route boundary tracker (`raw` bypass vs processed chain).
/// Exact port of `ChainRoute` in `/native/dsp-host/src/chain_route.h`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ChainRouteGuard {
    raw: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteTransitionAction {
    /// True when `raw != previous_raw`, requiring `feq_chain_reset_room` (never full chain reset).
    pub reset_room: bool,
    /// True when the audio callback should publish an inactive `FeqRoomReport` and skip `feq_chain_process`.
    pub bypass_chain: bool,
}

impl ChainRouteGuard {
    pub fn step(&mut self, raw: bool) -> RouteTransitionAction {
        let reset_room = raw != self.raw;
        self.raw = raw;
        RouteTransitionAction {
            reset_room,
            bypass_chain: raw,
        }
    }

    pub fn inactive_room_report() -> FeqRoomReport {
        FeqRoomReport {
            flags: FEQ_ROOM_REPORT_TAG,
            reference_gain_db: 0.0,
        }
    }
}

/// Telemetry-thread publication gate that suppresses redundant 12 KB `FEQN` packets
/// when no spectrum stage, scope, or band reading is fresh unless the Room report changed.
/// Exact port of `AnalysisPublication` in `/native/dsp-host/src/analysis_publication.h`.
#[derive(Debug, Default, Clone)]
pub struct AnalysisPublicationGate {
    last_room_flags: u32,
    last_room_reference_gain_db: f64,
}

impl AnalysisPublicationGate {
    pub fn clear(&mut self) {
        self.last_room_flags = 0;
        self.last_room_reference_gain_db = 0.0;
    }

    pub fn take(
        &mut self,
        stage_mask: u32,
        has_scope: bool,
        bands: u32,
        room_flags: u32,
        room_reference_gain_db: f64,
    ) -> bool {
        let changed = room_flags != self.last_room_flags
            || (room_reference_gain_db - self.last_room_reference_gain_db).abs() > f64::EPSILON;
        if stage_mask == 0 && !has_scope && bands == 0 && !changed {
            return false;
        }
        self.last_room_flags = room_flags;
        self.last_room_reference_gain_db = room_reference_gain_db;
        true
    }
}

/// Computes the exact trailing float payload byte count of a variable-length `FeqWireAnalysisFrame` (`FEQN`)
/// packet as assembled in `/native/dsp-host/src/host_telemetry.cpp`.
pub fn compute_analysis_frame_payload_bytes(
    stage_mask: u32,
    bins: u32,
    scope_pairs: u32,
    bands: u32,
    eq_detail_bins: u32,
) -> usize {
    let present_stages = stage_mask.count_ones() as usize;
    let spectrum_bytes = present_stages * (bins as usize) * 4;
    let scope_bytes = (scope_pairs as usize) * 2 * 4;
    let band_bytes = (bands as usize) * 2 * 4;
    let detail_bytes = (eq_detail_bins as usize) * 4;
    spectrum_bytes + scope_bytes + band_bytes + detail_bytes
}

/// Validates the header magic and total byte size of a single-write `FEQN` analysis packet
/// (`816` header bytes + trailing spectrum, scope, band amounts/levels, and EQ long-window floats).
pub fn validate_analysis_packet_layout(packet: &[u8]) -> Option<(u32, u32, usize)> {
    const HEADER_BYTES: usize = 816;
    if packet.len() < HEADER_BYTES {
        return None;
    }
    let magic = u32::from_le_bytes([packet[0], packet[1], packet[2], packet[3]]);
    if magic != FEQ_MAGIC_ANALYSIS {
        return None;
    }
    let sequence = u32::from_le_bytes([packet[4], packet[5], packet[6], packet[7]]);
    let stage_mask = u32::from_le_bytes([packet[8], packet[9], packet[10], packet[11]]);
    let bins = u32::from_le_bytes([packet[12], packet[13], packet[14], packet[15]]);
    let pairs = u32::from_le_bytes([packet[16], packet[17], packet[18], packet[19]]);
    let bands = u32::from_le_bytes([packet[20], packet[21], packet[22], packet[23]]);
    // `eq_detail_bins` sits at byte offset 88 in `FeqWireAnalysisFrame` (`wire.h` line 659)
    let eq_detail_bins = u32::from_le_bytes([packet[88], packet[89], packet[90], packet[91]]);

    let payload_bytes =
        compute_analysis_frame_payload_bytes(stage_mask, bins, pairs, bands, eq_detail_bins);
    if packet.len() != HEADER_BYTES + payload_bytes {
        return None;
    }
    Some((sequence, stage_mask, payload_bytes))
}

/// Encodes a 32-byte `FeqWireCommandFrame` (`FEQC`) in little-endian wire layout.
pub fn encode_dsp_host_command_frame(
    command: FeqWireCommand,
    request_id: u32,
    settings_revision: u32,
    parameter_id: u32,
    parameter_index: i32,
    value: f64,
) -> [u8; 32] {
    let frame = FeqWireCommandFrame {
        magic: FEQ_MAGIC_COMMAND,
        protocol_version: FEQ_WIRE_PROTOCOL_VERSION as u16,
        command: command as u16,
        request_id,
        settings_revision,
        parameter_id,
        parameter_index,
        value,
    };
    let mut out = [0u8; 32];
    out[0..4].copy_from_slice(&frame.magic.to_le_bytes());
    out[4..6].copy_from_slice(&frame.protocol_version.to_le_bytes());
    out[6..8].copy_from_slice(&frame.command.to_le_bytes());
    out[8..12].copy_from_slice(&frame.request_id.to_le_bytes());
    out[12..16].copy_from_slice(&frame.settings_revision.to_le_bytes());
    out[16..20].copy_from_slice(&frame.parameter_id.to_le_bytes());
    out[20..24].copy_from_slice(&frame.parameter_index.to_le_bytes());
    out[24..32].copy_from_slice(&frame.value.to_le_bytes());
    out
}

/// Decodes a 32-byte `FeqWireAckFrame` (`FEQA`) emitted by `dsp-host`.
pub fn parse_dsp_host_ack_frame(bytes: &[u8]) -> Option<FeqWireAckFrame> {
    if bytes.len() < 32 {
        return None;
    }
    let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if magic != FEQ_MAGIC_ACK {
        return None;
    }
    let protocol_version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if (protocol_version as u32) != FEQ_WIRE_PROTOCOL_VERSION {
        return None;
    }
    Some(FeqWireAckFrame {
        magic,
        protocol_version,
        status: u16::from_le_bytes([bytes[6], bytes[7]]),
        request_id: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
        accepted_revision: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
        applied_at_sample_frame: u64::from_le_bytes(bytes[16..24].try_into().ok()?),
        sanitized_value: f64::from_le_bytes(bytes[24..32].try_into().ok()?),
    })
}

/// Decodes a 24-byte `FeqWireStatsFrame` (`FEQS`) emitted by `dsp-host`.
pub fn parse_dsp_host_stats_frame(bytes: &[u8]) -> Option<FeqWireStatsFrame> {
    if bytes.len() < 24 {
        return None;
    }
    let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if magic != FEQ_MAGIC_STATS {
        return None;
    }
    Some(FeqWireStatsFrame {
        magic,
        reserved: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        working_set_bytes: u64::from_le_bytes(bytes[8..16].try_into().ok()?),
        cpu_seconds: f64::from_le_bytes(bytes[16..24].try_into().ok()?),
    })
}

// ============================================================================
// 12. `lighting-host` Identity, Registry Settings, Razer Container & LampArray
//     State Machines (`/native/lighting-host/src/{json_text.h,identity.cpp,lighting_settings.cpp,razer_devices.cpp,lamp_arrays.cpp,main.cpp}`)
// ============================================================================

use std::collections::{BTreeMap, BTreeSet};

pub const FLUIDEQ_LIGHTING_PACKAGE_NAME: &str = "FluidEQ.Lighting";
pub const FLUIDEQ_LIGHTING_MAIN_PACKAGE_NAME: &str = "FluidEQ.Lighting.Main";
pub const FLUIDEQ_LIGHTING_PUBLISHER_ID: &str = "r0s8b6jr3efv6";
pub const RAZER_USB_VENDOR_ID: u16 = 0x1532; // 5426

/// Exact 1:1 port of `fluideq_lighting::JsonLine` in `/native/lighting-host/src/json_text.h`.
/// Escapes control characters `< 0x20` as `\u00XX` and clamps non-finite or `> 1e9` floats to `0`.
#[derive(Debug, Default, Clone)]
pub struct LightingJsonLine {
    text: String,
}

impl LightingJsonLine {
    pub fn new() -> Self {
        Self {
            text: String::with_capacity(256),
        }
    }

    pub fn raw(&mut self, value: &str) -> &mut Self {
        self.text.push_str(value);
        self
    }

    pub fn ch(&mut self, value: char) -> &mut Self {
        self.text.push(value);
        self
    }

    pub fn string(&mut self, value: &str) -> &mut Self {
        self.text.push('"');
        for ch in value.chars() {
            match ch {
                '"' => self.text.push_str("\\\""),
                '\\' => self.text.push_str("\\\\"),
                '\n' => self.text.push_str("\\n"),
                '\r' => self.text.push_str("\\r"),
                '\t' => self.text.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    let n = c as u32;
                    let hex = b"0123456789abcdef";
                    self.text.push_str("\\u00");
                    self.text.push(hex[((n >> 4) & 0x0F) as usize] as char);
                    self.text.push(hex[(n & 0x0F) as usize] as char);
                }
                c => self.text.push(c),
            }
        }
        self.text.push('"');
        self
    }

    pub fn key(&mut self, name: &str) -> &mut Self {
        self.string(name).ch(':')
    }

    pub fn number_i64(&mut self, value: i64) -> &mut Self {
        self.text.push_str(&value.to_string());
        self
    }

    pub fn number_u32(&mut self, value: u32) -> &mut Self {
        self.text.push_str(&value.to_string());
        self
    }

    pub fn number_f64(&mut self, value: f64) -> &mut Self {
        if !value.is_finite() || value.abs() > 1e9 {
            self.text.push('0');
        } else {
            let formatted = format!("{:.3}", value);
            let trimmed = formatted.trim_end_matches('0').trim_end_matches('.');
            self.text
                .push_str(if trimmed.is_empty() || trimmed == "-0" {
                    "0"
                } else {
                    trimmed
                });
        }
        self
    }

    pub fn boolean(&mut self, value: bool) -> &mut Self {
        self.text.push_str(if value { "true" } else { "false" });
        self
    }

    pub fn finish_line(mut self) -> String {
        self.text.push('\n');
        self.text
    }
}

/// CLI subcommands handled by `/native/lighting-host/src/main.cpp` and `identity.cpp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LightingHostCliCommand {
    RunDaemon,
    Status,
    Register {
        external_dir: String,
        sparse_msix: String,
    },
    RegisterDev {
        manifest_path: String,
    },
    Remove,
    InvalidArgs,
}

/// Parses command-line arguments identically to `wmain` in `/native/lighting-host/src/main.cpp`.
pub fn parse_lighting_host_cli_args(args: &[String]) -> LightingHostCliCommand {
    match args.len() {
        0 => LightingHostCliCommand::RunDaemon,
        1 if args[0] == "status" => LightingHostCliCommand::Status,
        1 if args[0] == "remove" => LightingHostCliCommand::Remove,
        2 if args[0] == "register-dev" => LightingHostCliCommand::RegisterDev {
            manifest_path: args[1].clone(),
        },
        3 if args[0] == "register" => LightingHostCliCommand::Register {
            external_dir: args[1].clone(),
            sparse_msix: args[2].clone(),
        },
        _ => LightingHostCliCommand::InvalidArgs,
    }
}

/// Formats `{"type":"identity","active":...,"registered":...,"packageFullName":"..."}\n`
/// matching `write_identity_line` in `/native/lighting-host/src/identity.cpp`.
pub fn format_lighting_identity_line(
    active: bool,
    registered: bool,
    package_full_name: &str,
) -> String {
    let mut line = LightingJsonLine::new();
    line.ch('{')
        .key("type")
        .string("identity")
        .ch(',')
        .key("active")
        .boolean(active)
        .ch(',')
        .key("registered")
        .boolean(registered)
        .ch(',')
        .key("packageFullName")
        .string(package_full_name)
        .ch('}');
    line.finish_line()
}

/// Evaluates `register-dev` prerequisites from `/native/lighting-host/src/identity.cpp` (lines 147–172):
/// Windows `RegisterPackageByFullName` refuses loose `AppxManifest.xml` files unless Developer Mode
/// (`HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock\AllowDevelopmentWithoutDevLicense == 1`) is enabled.
pub fn validate_lighting_register_dev(
    developer_mode_enabled: bool,
    manifest_exists: bool,
) -> Result<(), &'static str> {
    if !developer_mode_enabled {
        return Err("Developer Mode is off; turn it on in Windows Settings > System > For developers, or install lighting.cer to Local Machine > Trusted People.");
    }
    if !manifest_exists {
        return Err("AppxManifest.xml not found.");
    }
    Ok(())
}

/// Per-device override under `HKCU\Software\Microsoft\Lighting\Devices\<device_id>` (`lighting_settings.cpp`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LightingDeviceOverride {
    pub device_id: String,
    pub controlled: Option<bool>,
    /// Numeric priority slots (`"1"`, `"2"`, ...) -> provider executable or package name.
    pub providers: BTreeMap<u32, String>,
}

/// Snapshot of `HKCU\Software\Microsoft\Lighting` read by `/native/lighting-host/src/lighting_settings.cpp`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LightingSettingsSnapshot {
    pub ambient_lighting_enabled: Option<bool>,
    pub controlled_by_foreground: Option<bool>,
    pub global_providers: BTreeMap<u32, String>,
    pub device_overrides: Vec<LightingDeviceOverride>,
}

impl LightingSettingsSnapshot {
    /// Formats `{"type":"lighting-settings","ambientLightingEnabled":...,"controlledByForeground":...,"providers":[...],"devices":[...]}\n`
    /// with providers ordered by numeric value name (`1, 2, 3...`), matching `read_lighting_settings_line` in `lighting_settings.cpp`.
    pub fn to_json_line(&self) -> String {
        let mut line = LightingJsonLine::new();
        line.ch('{')
            .key("type")
            .string("lighting-settings")
            .ch(',')
            .key("ambientLightingEnabled");
        match self.ambient_lighting_enabled {
            Some(v) => line.boolean(v),
            None => line.raw("null"),
        };
        line.ch(',').key("controlledByForeground");
        match self.controlled_by_foreground {
            Some(v) => line.boolean(v),
            None => line.raw("null"),
        };
        line.ch(',').key("providers").ch('[');
        for (idx, provider) in self.global_providers.values().enumerate() {
            if idx > 0 {
                line.ch(',');
            }
            line.string(provider);
        }
        line.ch(']').ch(',').key("devices").ch('[');
        for (d_idx, dev) in self.device_overrides.iter().enumerate() {
            if d_idx > 0 {
                line.ch(',');
            }
            line.ch('{')
                .key("id")
                .string(&dev.device_id)
                .ch(',')
                .key("controlled");
            match dev.controlled {
                Some(v) => line.boolean(v),
                None => line.raw("null"),
            };
            line.ch(',').key("providers").ch('[');
            for (p_idx, provider) in dev.providers.values().enumerate() {
                if p_idx > 0 {
                    line.ch(',');
                }
                line.string(provider);
            }
            line.ch(']').ch('}');
        }
        line.ch(']').ch('}');
        line.finish_line()
    }
}

/// Deduplicating registry watcher state machine matching `report_lighting_settings_if_changed`
/// in `/native/lighting-host/src/lighting_settings.cpp`.
#[derive(Debug, Default)]
pub struct LightingSettingsReporter {
    last_line: String,
}

impl LightingSettingsReporter {
    pub fn report_if_changed(&mut self, snapshot: &LightingSettingsSnapshot) -> Option<String> {
        let line = snapshot.to_json_line();
        if line == self.last_line {
            return None;
        }
        self.last_line = line.clone();
        Some(line)
    }
}

/// Parses `VID_1532&PID_xxxx` from a Windows PnP device or interface ID (`parse_vid_pid` in `razer_devices.cpp`).
pub fn parse_usb_vid_pid(text: &str) -> (u16, u16) {
    let upper = text.to_ascii_uppercase();
    let parse_after = |tag: &str| -> u16 {
        let Some(pos) = upper.find(tag) else {
            return 0;
        };
        let rest = &upper[pos + tag.len()..];
        let hex_prefix: String = rest
            .chars()
            .take(4)
            .take_while(|c| c.is_ascii_hexdigit())
            .collect();
        if hex_prefix.is_empty() {
            0
        } else {
            u16::from_str_radix(&hex_prefix, 16).unwrap_or(0)
        }
    };
    (parse_after("VID_"), parse_after("PID_"))
}

/// Chooses the best marketing name across PnP nodes of a Razer container (`better_razer_name` in `razer_devices.cpp`):
/// - Ignores generic `"USB Input Device"` / `"HID-compliant ..."` strings unless nothing else is set;
/// - Prefers names starting with `"Razer "` over short USB descriptors like `"DSV2Pro TKL"`;
/// - Among two `"Razer ..."` names, prefers the longer specific name (e.g. `"Razer BlackWidow V4"` over `"Razer Control"`).
pub fn is_better_razer_name(candidate: &str, current: &str) -> bool {
    if candidate.is_empty() {
        return false;
    }
    if candidate.starts_with("USB ") || candidate.starts_with("HID") {
        return current.is_empty();
    }
    if current.is_empty() || current.starts_with("USB ") || current.starts_with("HID") {
        return true;
    }
    let cand_razer = candidate.len() > 6 && candidate[..6].eq_ignore_ascii_case("Razer ");
    let curr_razer = current.len() > 6 && current[..6].eq_ignore_ascii_case("Razer ");
    if cand_razer != curr_razer {
        return cand_razer;
    }
    candidate.len() > current.len()
}

#[derive(Debug, Clone, Default)]
pub struct RazerContainerEntry {
    pub container_id: String,
    pub name: String,
    pub product_ids: BTreeSet<u16>,
    pub interface_ids: BTreeSet<String>,
    pub reported_name: String,
    pub reported_pids: BTreeSet<u16>,
}

/// Tracks physical Razer USB containers (`VID_1532`) across multiple HID interfaces and PIDs
/// (such as dual-PID wireless dongles `0x0F20` + `0x48F0`), matching `/native/lighting-host/src/razer_devices.cpp`.
#[derive(Debug, Default)]
pub struct RazerContainerTracker {
    containers: BTreeMap<String, RazerContainerEntry>,
}

impl RazerContainerTracker {
    /// Processes a PnP interface addition/update; returns `Some(json_line)` if a `"razer-added"` line should be emitted.
    pub fn upsert_interface(
        &mut self,
        interface_id: &str,
        container_id: &str,
        candidate_names: &[&str],
        instance_id_fallback: Option<&str>,
    ) -> Option<String> {
        let (mut vid, mut pid) = parse_usb_vid_pid(interface_id);
        if vid == 0 || pid == 0 {
            if let Some(inst) = instance_id_fallback {
                let (iv, ip) = parse_usb_vid_pid(inst);
                if vid == 0 {
                    vid = iv;
                }
                if pid == 0 {
                    pid = ip;
                }
            }
        }
        if vid != RAZER_USB_VENDOR_ID || pid == 0 || container_id.is_empty() {
            return None;
        }

        let key = container_id.to_ascii_lowercase();
        let entry = self
            .containers
            .entry(key)
            .or_insert_with(|| RazerContainerEntry {
                container_id: container_id.to_string(),
                ..Default::default()
            });
        entry.interface_ids.insert(interface_id.to_string());
        entry.product_ids.insert(pid);
        for &cand in candidate_names {
            if is_better_razer_name(cand, &entry.name) {
                entry.name = cand.to_string();
            }
        }
        if entry.name.is_empty() {
            entry.name = "Razer Device".to_string();
        }
        if entry.name == entry.reported_name && entry.product_ids == entry.reported_pids {
            return None;
        }
        entry.reported_name = entry.name.clone();
        entry.reported_pids = entry.product_ids.clone();

        let first_pid = *entry.product_ids.iter().next().unwrap_or(&pid);
        let mut line = LightingJsonLine::new();
        line.ch('{')
            .key("type")
            .string("razer-added")
            .ch(',')
            .key("id")
            .string(&entry.container_id)
            .ch(',')
            .key("name")
            .string(&entry.name)
            .ch(',')
            .key("vendorId")
            .number_u32(RAZER_USB_VENDOR_ID as u32)
            .ch(',')
            .key("productId")
            .number_u32(first_pid as u32)
            .ch(',')
            .key("productIds")
            .ch('[');
        for (idx, &p) in entry.product_ids.iter().enumerate() {
            if idx > 0 {
                line.ch(',');
            }
            line.number_u32(p as u32);
        }
        line.ch(']').ch('}');
        Some(line.finish_line())
    }

    /// Removes a PnP interface; emits `{"type":"razer-removed","id":"..."}\n` only when the last
    /// interface belonging to that physical container leaves (`razer_devices.cpp` lines 253–269).
    pub fn remove_interface(&mut self, interface_id: &str) -> Option<String> {
        let mut emptied_key = None;
        let mut removed_container_id = String::new();
        for (key, entry) in &mut self.containers {
            if entry.interface_ids.remove(interface_id) {
                if entry.interface_ids.is_empty() {
                    emptied_key = Some(key.clone());
                    removed_container_id = entry.container_id.clone();
                }
                break;
            }
        }
        if let Some(key) = emptied_key {
            self.containers.remove(&key);
            if !removed_container_id.is_empty() {
                let mut line = LightingJsonLine::new();
                line.ch('{')
                    .key("type")
                    .string("razer-removed")
                    .ch(',')
                    .key("id")
                    .string(&removed_container_id)
                    .ch('}');
                return Some(line.finish_line());
            }
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LampPositionMeters {
    pub index: i32,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub purposes: u32,
}

#[derive(Debug, Clone)]
pub struct OpenedLampArrayDevice {
    pub index: u32,
    pub id: String,
    pub name: String,
    pub kind: i32,
    pub vendor_id: u16,
    pub product_id: u16,
    pub lamp_count: i32,
    pub min_update_ms: f64,
    pub width_m: f64,
    pub height_m: f64,
    pub depth_m: f64,
    pub available: bool,
    pub supports_set: bool,
    pub lamps: Vec<LampPositionMeters>,
    pub last_send_us: Option<u64>,
}

/// Async `LampArray` discovery, `FromIdAsync` in-flight cancellation guard, and `MinUpdateInterval`
/// frame rate limiter matching `/native/lighting-host/src/lamp_arrays.cpp`.
#[derive(Debug, Default)]
pub struct LampArrayRegistry {
    next_index: u32,
    opening_ids: BTreeSet<String>,
    failed_ids: BTreeMap<String, String>,
    devices: Vec<OpenedLampArrayDevice>,
}

impl LampArrayRegistry {
    /// Marks `id` as `opening` when `DeviceWatcher::Added` fires; returns `false` if already opened or opening.
    pub fn begin_open(&mut self, id: &str) -> bool {
        if self.devices.iter().any(|d| d.id == id) || self.opening_ids.contains(id) {
            return false;
        }
        self.opening_ids.insert(id.to_string());
        true
    }

    /// Completes an async `LampArray::FromIdAsync` open; if `remove_device` was called while the
    /// async open was in flight (`!opening_ids.remove(id)`), discards the opened handle immediately
    /// (`lamp_arrays.cpp` lines 166–170).
    pub fn complete_open(&mut self, mut dev: OpenedLampArrayDevice) -> Option<String> {
        if !self.opening_ids.remove(&dev.id) {
            return None;
        }
        self.failed_ids.remove(&dev.id);
        let index = self.next_index;
        self.next_index += 1;
        dev.index = index;
        dev.last_send_us = None;

        let mut line = LightingJsonLine::new();
        line.ch('{')
            .key("type")
            .string("added")
            .ch(',')
            .key("index")
            .number_u32(dev.index)
            .ch(',')
            .key("id")
            .string(&dev.id)
            .ch(',')
            .key("name")
            .string(&dev.name)
            .ch(',')
            .key("kind")
            .number_i64(dev.kind as i64)
            .ch(',')
            .key("vendorId")
            .number_u32(dev.vendor_id as u32)
            .ch(',')
            .key("productId")
            .number_u32(dev.product_id as u32)
            .ch(',')
            .key("lampCount")
            .number_i64(dev.lamp_count as i64)
            .ch(',')
            .key("minIntervalMs")
            .number_f64(dev.min_update_ms)
            .ch(',')
            .key("available")
            .boolean(dev.available)
            .ch(',')
            .key("supportsSet")
            .boolean(dev.supports_set)
            .ch(',')
            .key("bounds")
            .ch('{')
            .key("width")
            .number_f64(dev.width_m)
            .ch(',')
            .key("height")
            .number_f64(dev.height_m)
            .ch(',')
            .key("depth")
            .number_f64(dev.depth_m)
            .ch('}')
            .ch(',')
            .key("lamps")
            .ch('[');
        for (i, lamp) in dev.lamps.iter().enumerate() {
            if i > 0 {
                line.ch(',');
            }
            line.ch('{')
                .key("i")
                .number_i64(lamp.index as i64)
                .ch(',')
                .key("x")
                .number_f64(lamp.x)
                .ch(',')
                .key("y")
                .number_f64(lamp.y)
                .ch(',')
                .key("z")
                .number_f64(lamp.z)
                .ch(',')
                .key("purposes")
                .number_u32(lamp.purposes)
                .ch('}');
        }
        line.ch(']').ch('}');
        self.devices.push(dev);
        Some(line.finish_line())
    }

    /// Records a failed `FromIdAsync` open, deduplicating identical failure reasons per device ID.
    pub fn record_open_failure(&mut self, id: &str, name: &str, reason: &str) -> Option<String> {
        self.opening_ids.remove(id);
        if self.failed_ids.get(id).map(|s| s.as_str()) == Some(reason) {
            return None;
        }
        self.failed_ids.insert(id.to_string(), reason.to_string());
        let mut line = LightingJsonLine::new();
        line.ch('{')
            .key("type")
            .string("open-failed")
            .ch(',')
            .key("id")
            .string(id)
            .ch(',')
            .key("name")
            .string(name)
            .ch(',')
            .key("reason")
            .string(reason)
            .ch('}');
        Some(line.finish_line())
    }

    /// Removes a device when `DeviceWatcher::Removed` fires, cancelling any in-flight `opening_ids` entry.
    pub fn remove_device(&mut self, id: &str) -> Option<String> {
        self.opening_ids.remove(id);
        self.failed_ids.remove(id);
        let pos = self.devices.iter().position(|d| d.id == id)?;
        let removed = self.devices.remove(pos);
        let mut line = LightingJsonLine::new();
        line.ch('{')
            .key("type")
            .string("removed")
            .ch(',')
            .key("index")
            .number_u32(removed.index)
            .ch(',')
            .key("id")
            .string(&removed.id)
            .ch('}');
        Some(line.finish_line())
    }

    /// Updates `IsAvailable` on `AvailabilityChanged` and emits `{"type":"available",...}\n` only when changed.
    pub fn update_availability(&mut self, index: u32, available: bool) -> Option<String> {
        let dev = self.devices.iter_mut().find(|d| d.index == index)?;
        if dev.available == available {
            return None;
        }
        dev.available = available;
        let mut line = LightingJsonLine::new();
        line.ch('{')
            .key("type")
            .string("available")
            .ch(',')
            .key("index")
            .number_u32(index)
            .ch(',')
            .key("available")
            .boolean(available)
            .ch('}');
        Some(line.finish_line())
    }

    /// Evaluates whether a `ColoursFrame` should be applied to hardware at `now_us` (`apply_colours` in `lamp_arrays.cpp`):
    /// drops frames if `!supports_set` or if `now_us - last_send_us < min_update_ms * 1000`.
    pub fn should_apply_colours_frame(
        &mut self,
        frame: &LightingColoursFrame,
        now_us: u64,
    ) -> Option<usize> {
        let dev = self
            .devices
            .iter_mut()
            .find(|d| d.index == frame.device_index)?;
        if !dev.supports_set || dev.lamp_count <= 0 {
            return None;
        }
        let min_interval_us = (dev.min_update_ms.max(0.0) * 1000.0) as u64;
        if let Some(last_us) = dev.last_send_us {
            if min_interval_us > 0 && now_us.saturating_sub(last_us) < min_interval_us {
                return None;
            }
        }
        let count = (frame.lamp_count as usize).min(dev.lamp_count as usize);
        if count == 0 || frame.rgb_bytes.len() < count * 3 {
            return None;
        }
        dev.last_send_us = Some(now_us);
        Some(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_watch_engages_and_releases_game_profile_across_alt_tab() {
        let mut store = AppStateStore::new(48_000.0);
        let rules = vec![GameProfileRule {
            profile_id: "fps-competitive".to_string(),
            executable_match: "cs2.exe".to_string(),
            footsteps_lift: 0.75,
            footsteps_calm: 0.60,
            enable_low_latency: true,
        }];
        let mut sup = ForegroundGameSupervisor::new(rules);

        let hold_cmd = sup.handle_line(
            "front\t4242\t0,0,2560,1440\tCounter-Strike 2\tC:\\Games\\cs2.exe\n",
            &mut store,
        );
        assert_eq!(hold_cmd.as_deref(), Some("hold 4242\n"));
        assert_eq!(store.snapshot().chain_settings.low_latency, 1);
        assert_eq!(store.snapshot().chain_settings.footsteps.enabled, 1);
        assert_eq!(store.snapshot().chain_settings.footsteps.lift, 0.75);

        // User Alt-Tabs to browser while game is still running: game profile stays held
        let alt_tab = sup.handle_line(
            "front\t1088\t100,100,1280,800\tBrowser\tC:\\Program Files\\browser.exe\n",
            &mut store,
        );
        assert!(alt_tab.is_none());
        assert_eq!(store.snapshot().chain_settings.low_latency, 1);

        // Game process exits -> `gone\t4242` restores original settings
        let release_cmd = sup.handle_line("gone\t4242\n", &mut store);
        assert_eq!(release_cmd.as_deref(), Some("hold 0\n"));
        assert_eq!(store.snapshot().chain_settings.low_latency, 0);
        assert_eq!(store.snapshot().chain_settings.footsteps.enabled, 0);
    }

    #[test]
    fn volume_watch_parses_events_and_formats_commands() {
        let mut coord = VolumeWatchCoordinator::default();
        assert_eq!(
            coord.handle_line("ready\t2\n"),
            Some(VolumeWatchEvent::Ready {
                protocol_version: 2
            })
        );
        assert_eq!(
            coord.handle_line("volume\t0.6250\t0\n"),
            Some(VolumeWatchEvent::Volume {
                level: 0.625,
                muted: false
            })
        );
        assert!(coord.endpoint_available);
        assert_eq!(format_volume_set_command(0.75), "set 0.7500\n");
        assert_eq!(format_volume_mute_command(true), "mute 1\n");
    }

    #[test]
    fn media_watch_deduplicates_whole_seconds_and_protects_cold_standby() {
        let mut store = AppStateStore::new(48_000.0);
        let mut media = MediaWatchCoordinator::default();
        let line_1 = r#"{"app":"Spotify.exe","title":"Holocene","artist":"Bon Iver","isPlaying":true,"positionMs":12150,"durationMs":336000,"canNext":true,"canPrevious":true,"canSeek":true,"playing":["Spotify.exe"],"coverId":"0123456789abcdef"}"#;
        let line_2 = r#"{"app":"Spotify.exe","title":"Holocene","artist":"Bon Iver","isPlaying":true,"positionMs":12300,"durationMs":336000,"canNext":true,"canPrevious":true,"canSeek":true,"playing":["Spotify.exe"],"coverId":"0123456789abcdef"}"#;

        assert!(media.handle_line(line_1, &mut store).is_some());
        assert!(store.snapshot().residency.online_media_playing);
        // Same whole second (12s) -> deduplicated
        assert!(media.handle_line(line_2, &mut store).is_none());

        assert_eq!(
            media.handle_line("null\n", &mut store),
            Some(MediaWatchEvent::Cleared)
        );
        assert!(!store.snapshot().residency.online_media_playing);
    }

    #[test]
    fn process_meter_parses_private_and_working_set_bytes() {
        let reply = "4120 18350080 29360128 950000 fluideq-shell.exe;5188 12582912 19922944 410000 fluideq-audio.exe\n";
        let summary = summarize_process_meter_reply(reply);
        assert_eq!(summary.process_count, 2);
        assert_eq!(summary.total_private_bytes, 18_350_080 + 12_582_912);
        assert_eq!(summary.entries[0].executable_name, "fluideq-shell.exe");
    }

    #[test]
    fn capture_frame_header_24_byte_roundtrip() {
        let header = CaptureFrameHeader {
            magic: CAPTURE_FRAME_MAGIC,
            kind: 1,
            sequence: 99,
            sample_rate: 48_000,
            channels: 2,
            frames: 480,
            payload_bytes: 480 * 2 * 4,
        };
        let bytes = header.to_le_bytes();
        let decoded = CaptureFrameHeader::from_le_bytes(&bytes).expect("valid 24-byte header");
        assert_eq!(decoded, header);
    }

    #[test]
    fn playback_header_and_peer_meter_reply_roundtrip() {
        let header = PlaybackHeader {
            magic: PLAYBACK_HEADER_MAGIC,
            kind: 5,
            id: 2,
            rate: 48_000,
            channels: 0,
            frames: 0,
            bytes: 24,
        };
        let raw_hdr = header.to_le_bytes();
        let decoded_hdr = PlaybackHeader::from_le_bytes(&raw_hdr).expect("valid FLP1 header");
        assert_eq!(decoded_hdr, header);

        let mut payload = Vec::with_capacity(24);
        payload.extend_from_slice(&18.5_f64.to_le_bytes());
        payload.extend_from_slice(&0.72_f64.to_le_bytes());
        payload.extend_from_slice(&0.25_f64.to_le_bytes());

        let ev = parse_playback_reply_packet(&decoded_hdr, &payload).expect("valid meter reply");
        assert_eq!(
            ev,
            PlaybackReplyEvent::PeerMeter {
                peer_id: 2,
                sample_rate: 48_000,
                buffered_ms: 18.5,
                peak: 0.72,
                rms: 0.25,
            }
        );
    }

    #[test]
    fn output_watch_detects_default_device_switch() {
        let mut coord = OutputWatchCoordinator::default();
        assert!(coord.handle_line("outputs\n"));
        let json = r#"[{"id":"{0.0.0.00000000}.{11111111-2222-3333-4444-555555555555}","name":"Headphones (USB DAC)","guid":"{11111111-2222-3333-4444-555555555555}","isDefault":true,"isActive":true,"isEqualizerApoAttached":false,"isFluidEngineAttached":true,"canHostEffects":true,"effectsEnabled":true,"sampleRate":48000,"channels":2,"batteryPercent":85}]"#;
        let switched = coord.update_devices_json(json).expect("valid devices json");
        assert_eq!(
            switched.as_deref(),
            Some("{11111111-2222-3333-4444-555555555555}")
        );
        assert_eq!(coord.devices[0].battery_percent, Some(85));
    }

    #[test]
    fn lighting_frame_reader_decodes_flh1_rgb_packet() {
        let packet = encode_lighting_colours_frame(0, &[[255, 0, 128], [0, 255, 64]])
            .expect("valid FLH1 frame");
        let mut reader = LightingFrameReader::default();
        reader.feed(&packet);
        assert_eq!(
            reader.next_frame(),
            LightingReadResult::Colours(LightingColoursFrame {
                device_index: 0,
                lamp_count: 2,
                rgb_bytes: vec![255, 0, 128, 0, 255, 64],
            })
        );
    }

    #[test]
    fn wallpaper_occlusion_detects_two_side_by_side_windows_covering_monitor() {
        let monitor = DesktopBoxRect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let left_half = DesktopBoxRect {
            left: 0,
            top: 0,
            right: 960,
            bottom: 1080,
        };
        let right_half = DesktopBoxRect {
            left: 960,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        assert_eq!(
            desktop_cover_of(monitor, &[left_half, right_half], &[left_half]),
            WallpaperCoverState::Owner
        );
        assert_eq!(
            desktop_cover_of(monitor, &[left_half, right_half], &[left_half, right_half]),
            WallpaperCoverState::Others
        );
    }

    #[test]
    fn dsp_host_command_and_stats_frame_roundtrip() {
        let cmd_bytes =
            encode_dsp_host_command_frame(FeqWireCommand::FEQ_CMD_SET_VOLUME, 7, 42, 0, 0, 0.85);
        assert_eq!(
            u32::from_le_bytes([cmd_bytes[0], cmd_bytes[1], cmd_bytes[2], cmd_bytes[3]]),
            FEQ_MAGIC_COMMAND
        );

        let mut stats_bytes = [0u8; 24];
        stats_bytes[0..4].copy_from_slice(&FEQ_MAGIC_STATS.to_le_bytes());
        stats_bytes[8..16].copy_from_slice(&14_680_064_u64.to_le_bytes());
        stats_bytes[16..24].copy_from_slice(&3.25_f64.to_le_bytes());
        let parsed = parse_dsp_host_stats_frame(&stats_bytes).expect("valid FEQS frame");
        assert_eq!(parsed.working_set_bytes, 14_680_064);
        assert_eq!(parsed.cpu_seconds, 3.25);
    }

    #[test]
    fn processing_latency_seqlock_and_analysis_gate_behave_deterministically() {
        let seqlock = ProcessingLatencySnapshot::new();
        assert!(seqlock.read().is_none());

        let parts = FeqChainLatencyParts {
            eq: 8192,
            restoration: 480,
            deesser: 96,
            ..Default::default()
        };
        seqlock.publish(8768, &parts, 0x0B);
        let words = seqlock.read().expect("published even generation");
        assert_eq!(words[0], 8768);
        assert_eq!(words[1], 8192);
        assert_eq!(words[2], 480);
        assert_eq!(words[8], 96);
        assert_eq!(words[9], 0x0B);

        let mut gate = AnalysisPublicationGate::default();
        assert!(!gate.take(0, false, 0, 0, 0.0));
        // Room report change triggers exactly one frame even when stage_mask == 0
        assert!(gate.take(0, false, 0, 1, -3.5));
        assert!(!gate.take(0, false, 0, 1, -3.5));
    }

    #[test]
    fn lighting_host_razer_container_and_lamp_array_state_machines_match_native() {
        // 1. Razer container groups dual-PID interfaces and prefers specific "Razer ..." name over "Razer Control" / "DSV2Pro TKL"
        let mut razer = RazerContainerTracker::default();
        let line1 = razer
            .upsert_interface(
                "\\\\?\\HID#VID_1532&PID_0F20&MI_00#1",
                "{aaaa-bbbb}",
                &["USB Input Device", "DSV2Pro TKL"],
                None,
            )
            .expect("initial container add");
        assert!(line1.contains("\"name\":\"DSV2Pro TKL\""));
        assert!(line1.contains("\"productIds\":[3872]"));

        let line2 = razer
            .upsert_interface(
                "\\\\?\\HID#VID_1532&PID_48F0&MI_01#1",
                "{AAAA-BBBB}",
                &["Razer Control", "Razer DeathStalker V2 Pro TKL"],
                None,
            )
            .expect("second PID and better Razer name update");
        assert!(line2.contains("\"name\":\"Razer DeathStalker V2 Pro TKL\""));
        assert!(line2.contains("\"productIds\":[3872,18672]"));

        // Removing first interface does NOT emit razer-removed while second interface remains
        assert!(razer
            .remove_interface("\\\\?\\HID#VID_1532&PID_0F20&MI_00#1")
            .is_none());
        let removed = razer
            .remove_interface("\\\\?\\HID#VID_1532&PID_48F0&MI_01#1")
            .expect("last interface removed");
        assert_eq!(
            removed,
            "{\"type\":\"razer-removed\",\"id\":\"{aaaa-bbbb}\"}\n"
        );

        // 2. LampArrayRegistry cancels in-flight open if device is removed before FromIdAsync finishes,
        //    and enforces min_update_ms frame rate limiting.
        let mut lamps = LampArrayRegistry::default();
        assert!(lamps.begin_open("\\\\?\\LAMP#1"));
        assert!(lamps.remove_device("\\\\?\\LAMP#1").is_none());
        let discarded = lamps.complete_open(OpenedLampArrayDevice {
            index: 0,
            id: "\\\\?\\LAMP#1".to_string(),
            name: "Keyboard".to_string(),
            kind: 1,
            vendor_id: 0x1532,
            product_id: 0x0290,
            lamp_count: 2,
            min_update_ms: 16.0,
            width_m: 0.45,
            height_m: 0.15,
            depth_m: 0.03,
            available: true,
            supports_set: true,
            lamps: vec![],
            last_send_us: None,
        });
        assert!(discarded.is_none());

        assert!(lamps.begin_open("\\\\?\\LAMP#2"));
        let added = lamps
            .complete_open(OpenedLampArrayDevice {
                index: 0,
                id: "\\\\?\\LAMP#2".to_string(),
                name: "Mousepad".to_string(),
                kind: 3,
                vendor_id: 0x1532,
                product_id: 0x0C05,
                lamp_count: 2,
                min_update_ms: 10.0,
                width_m: 0.35,
                height_m: 0.25,
                depth_m: 0.01,
                available: true,
                supports_set: true,
                lamps: vec![],
                last_send_us: None,
            })
            .expect("completed open");
        assert!(added.contains("\"type\":\"added\""));

        let frame = LightingColoursFrame {
            device_index: 0,
            lamp_count: 2,
            rgb_bytes: vec![255, 0, 0, 0, 255, 0],
        };
        assert_eq!(lamps.should_apply_colours_frame(&frame, 100_000), Some(2));
        // 5 ms later (< 10 ms min_update_ms) -> rate-limited
        assert_eq!(lamps.should_apply_colours_frame(&frame, 105_000), None);
        // 11 ms later -> applied
        assert_eq!(lamps.should_apply_colours_frame(&frame, 111_000), Some(2));
    }

    #[test]
    fn analysis_packet_layout_reads_eq_detail_bins_at_offset_88_and_helpers_match_native() {
        // 1. Verify `validate_analysis_packet_layout` reads `eq_detail_bins` at byte offset 88 (`wire.h` line 659)
        let mut packet = vec![0u8; 816 + 2048 * 4];
        packet[0..4].copy_from_slice(&FEQ_MAGIC_ANALYSIS.to_le_bytes());
        packet[4..8].copy_from_slice(&17u32.to_le_bytes()); // sequence = 17
        packet[88..92].copy_from_slice(&2048u32.to_le_bytes()); // eq_detail_bins = 2048 at offset 88
        let (seq, mask, payload_bytes) =
            validate_analysis_packet_layout(&packet).expect("valid 816-byte header + detail bins");
        assert_eq!(seq, 17);
        assert_eq!(mask, 0);
        assert_eq!(payload_bytes, 2048 * 4);

        // 2. Verify `stacked_between` matches `/native/wallpaper/tests/desktop_test.cpp`
        assert_eq!(
            stacked_between(20usize, 50usize, &[10, 20, 30, 40, 50, 60]),
            vec![30, 40]
        );
        assert!(stacked_between(50usize, 20usize, &[10, 20, 30, 40, 50, 60]).is_empty());

        // 3. Verify `expand_process_tree_children` rejects recycled PIDs where child_created < parent_created
        let snap = vec![
            ProcessSnapshotItem {
                pid: 100,
                parent_pid: 4,
                created_100ns: 5_000,
                exe_name: "parent.exe".to_string(),
            },
            ProcessSnapshotItem {
                pid: 200,
                parent_pid: 100,
                created_100ns: 6_000,
                exe_name: "valid_child.exe".to_string(),
            },
            ProcessSnapshotItem {
                pid: 300,
                parent_pid: 100,
                created_100ns: 2_000, // older than parent -> PID reuse false positive!
                exe_name: "recycled_orphan.exe".to_string(),
            },
        ];
        assert_eq!(expand_process_tree_children(100, &snap), vec![100, 200]);

        // 4. Verify UWP ApplicationFrameHost resolution and strict volume/wallpaper command parsers
        assert_eq!(
            resolve_uwp_frame_host_pid(
                1100,
                "C:\\Windows\\System32\\ApplicationFrameHost.exe",
                &[
                    ("ApplicationFrameInputSinkWindow", 1100),
                    ("Windows.UI.Core.CoreWindow", 8420)
                ]
            ),
            8420
        );
        assert_eq!(
            take_volume_watch_command("set 0.5\n"),
            Some(VolumeWatchCommand::Set(0.5))
        );
        assert_eq!(take_volume_watch_command("set 1.5\n"), None);
        assert_eq!(
            parse_wallpaper_cli_args(&["65552", "4200"]),
            Some((65552, 4200))
        );
        assert_eq!(parse_wallpaper_cli_args(&["0x100"]), None);
    }
}
