/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! System APO Setup Console Borrowing, UAC Elevation Relaunch, Render Endpoint Enumerator,
//! Slot Report & Full Command Dispatcher (`Slice-M9M10-3`).
//!
//! Strictly 1:1 ports the remaining 5 C++ modules (plus exact `attach_outcome`, `slot_memory`,
//! and `slot_report` helpers) from `/native/system-apo/setup/`:
//! 1. `console.h/.cpp` (155 lines) — `borrow_caller_console` (re-binding CRT stdout/stderr to
//!    inherited pipe handles via `DuplicateHandle` + `_open_osfhandle(_O_TEXT)` + `_dup2`, or
//!    attaching to parent console `CONOUT$` only for streams with no inherited handle).
//! 2. `elevate.h/.cpp` (190 lines) — `quote_command_line_arg` (`CommandLineToArgvW`-compatible
//!    backslash/quote escaping), `is_elevated`, and `relaunch_elevated` (handling `ERROR_CANCELLED`
//!    as exit code `2` without setting an error string).
//! 3. `endpoints.h/.cpp` (199 lines) — `describe_hresult` (`0x88890008`), `guid_from_device_id`
//!    (extracting and uppercasing the trailing `{...}` group), and `list_render_endpoints`.
//! 4. `commands.h/.cpp` (766 lines) — `SetupOptions`, `ensure_engine_tree`, `run_command`
//!    (`install`, `uninstall`, `attach`, `detach`, `suspend-apo`, `restore-apo`, `restart-audio`),
//!    `complete_modes`, and `result_json`.
//! 5. `main.cpp` (628 lines) — `USAGE_TEXT`, `parse_command_line`, `runtime_beside`,
//!    `engine_has_run`, `build_status_json`, `note_run`, `run_elevated`, `run_through_elevation`,
//!    and `run_setup_main`.

use crate::apo::{
    append_remembered_slot, default_slot_for, is_attached, is_valid_endpoint_guid, mode_missing,
    parse_remembered_slots, plan_detach, plan_mode_in_place, plan_move, plan_restore_apo_with_keep,
    plan_suspend_apo, slot_of, ApoSlot, EndpointAttachResult, EndpointFxValues, APO_SLOT_LADDER,
    FX_LEGACY_COUNT, FX_SLOT_COUNT,
};
use crate::apo_setup_registry::{
    apo_off_endpoints_on_disk, apo_record_present, backup_exists_on_disk, enable_unsigned_effects,
    endpoint_is_combined, endpoint_key_exists, json_escape, load_apo_off_from_disk,
    load_backup_from_disk, read_fx_values, register_engine, registered_dll_path,
    remove_apo_off_on_disk, remove_backup_on_disk, save_apo_off_once_on_disk,
    save_backup_once_on_disk, unregister_engine, unsigned_effects_enabled, write_fx_values,
    decode_sz, RegistryBackend, ENGINE_CLSID_UPPER, REG_SZ_TYPE,
};
use crate::apo_setup_services_and_fs::{
    apply_config_acl, apply_engine_acl, apply_log_acl, apply_read_only_acl, describe_error,
    restart_audio, run_with_retries, service_can_write, sha256_hex, shipped_digest, take_ownership,
    wait_audio_settled, SecurityBackend, ServiceControl, ServiceStateWaiter, SetupCommandResult,
    SetupPaths, ShippedDllEntry, COMMAND_TRIES,
};
use std::path::{Path, PathBuf};

// ============================================================================
// 1. Caller Console Borrowing (`console.h/.cpp`)
// ============================================================================

/// Action taken for one standard stream (`stdout` fd 1 or `stderr` fd 2) by `borrow_caller_console`
/// (`/native/system-apo/setup/console.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleStreamBindAction {
    /// `_get_osfhandle(descriptor) == handle`: CRT stream is already wired to the inherited handle.
    AlreadyBoundToInherited,
    /// Duplicates `handle` (`DUPLICATE_SAME_ACCESS`), opens with `_O_TEXT`, and `_dup2`s onto `descriptor`.
    RebindToInherited { handle: u64, descriptor: i32 },
    /// Reopens the stream on `"CONOUT$"` after `AttachConsole(ATTACH_PARENT_PROCESS)` succeeded.
    ReopenOnParentConsole,
    /// No inherited handle and no parent console; output is discarded as before.
    Discard,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleBorrowPlan {
    pub attached_parent_console: bool,
    pub stdout_action: ConsoleStreamBindAction,
    pub stderr_action: ConsoleStreamBindAction,
}

/// Normalizes a raw Win32 `GetStdHandle` return value (`NULL = 0` or `INVALID_HANDLE_VALUE = u64::MAX`)
/// into `Option<u64>` (`inherited` in `console.cpp`).
pub fn normalize_inherited_std_handle(raw_handle: u64) -> Option<u64> {
    if raw_handle == 0 || raw_handle == u64::MAX {
        None
    } else {
        Some(raw_handle)
    }
}

/// Exact port of `borrow_caller_console` and `bind_to_inherited` in `/native/system-apo/setup/console.cpp`:
/// - An inherited handle (`stdout` or `stderr`) ALWAYS wins over `CONOUT$` so piped `execFile` JSON output
///   is never diverted to an interactive console.
/// - `AttachConsole(ATTACH_PARENT_PROCESS)` is only attempted when at least one of `stdout` or `stderr`
///   was not handed down (`out.is_none() || error.is_none()`).
/// - Each stream is decided independently so `FluidEQ-Engine-Setup status > out.txt` binds `stdout` to the
///   redirected file while `stderr` attaches to the parent shell's console.
pub fn plan_borrow_caller_console(
    raw_stdout_handle: u64,
    raw_stderr_handle: u64,
    crt_stdout_osfhandle: Option<u64>,
    crt_stderr_osfhandle: Option<u64>,
    can_attach_parent_console: bool,
) -> ConsoleBorrowPlan {
    let out = normalize_inherited_std_handle(raw_stdout_handle);
    let error = normalize_inherited_std_handle(raw_stderr_handle);

    let attached = (out.is_none() || error.is_none()) && can_attach_parent_console;

    let stdout_action = match out {
        Some(handle) => {
            if crt_stdout_osfhandle == Some(handle) {
                ConsoleStreamBindAction::AlreadyBoundToInherited
            } else {
                ConsoleStreamBindAction::RebindToInherited {
                    handle,
                    descriptor: 1,
                }
            }
        }
        None if attached => ConsoleStreamBindAction::ReopenOnParentConsole,
        None => ConsoleStreamBindAction::Discard,
    };

    let stderr_action = match error {
        Some(handle) => {
            if crt_stderr_osfhandle == Some(handle) {
                ConsoleStreamBindAction::AlreadyBoundToInherited
            } else {
                ConsoleStreamBindAction::RebindToInherited {
                    handle,
                    descriptor: 2,
                }
            }
        }
        None if attached => ConsoleStreamBindAction::ReopenOnParentConsole,
        None => ConsoleStreamBindAction::Discard,
    };

    ConsoleBorrowPlan {
        attached_parent_console: attached,
        stdout_action,
        stderr_action,
    }
}

// ============================================================================
// 2. Administrator Elevation & `CommandLineToArgvW` Quoting (`elevate.h/.cpp`)
// ============================================================================

/// Win32 `ERROR_CANCELLED` (`1223`) returned by `ShellExecuteExW` when the UAC prompt is declined.
pub const ERROR_CANCELLED_WIN32: u32 = 1223;

/// Exit code `0`: command completed.
pub const SETUP_EXIT_OK: i32 = 0;
/// Exit code `1`: command line could not be parsed.
pub const SETUP_EXIT_USAGE: i32 = 1;
/// Exit code `2`: UAC administrator consent prompt was declined.
pub const SETUP_EXIT_DECLINED: i32 = 2;
/// Exit code `3`: command ran and failed.
pub const SETUP_EXIT_FAILED: i32 = 3;

/// Exact port of `quote` in `/native/system-apo/setup/elevate.cpp`:
/// Quotes a single command-line argument the way `CommandLineToArgvW` will parse it back:
/// - Non-empty arguments containing none of `' '`, `'\t'`, `'\n'`, `'\x0b'` (`\v`), or `'"'`
///   are returned unchanged.
/// - Empty arguments or arguments with whitespace/quotes are wrapped in `"..."`, doubling
///   backslashes only immediately before `'"'` or the closing `'"'`.
pub fn quote_command_line_arg(argument: &str) -> String {
    if !argument.is_empty()
        && !argument
            .chars()
            .any(|c| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '"'))
    {
        return argument.to_string();
    }

    let chars: Vec<char> = argument.chars().collect();
    let mut quoted = String::from("\"");
    let mut at = 0usize;
    while at < chars.len() {
        let mut backslashes = 0usize;
        while at < chars.len() && chars[at] == '\\' {
            at += 1;
            backslashes += 1;
        }
        if at == chars.len() {
            for _ in 0..(backslashes * 2) {
                quoted.push('\\');
            }
            break;
        }
        if chars[at] == '"' {
            for _ in 0..(backslashes * 2 + 1) {
                quoted.push('\\');
            }
        } else {
            for _ in 0..backslashes {
                quoted.push('\\');
            }
        }
        quoted.push(chars[at]);
        at += 1;
    }
    quoted.push('"');
    quoted
}

/// Joins `arguments` into the `lpParameters` string passed to `ShellExecuteExW` (`elevate.cpp`).
pub fn format_elevated_parameters(arguments: &[String]) -> Option<String> {
    if arguments.is_empty() {
        return None;
    }
    let mut line = String::new();
    for arg in arguments {
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&quote_command_line_arg(arg));
    }
    Some(line)
}

/// Outcome of `ShellExecuteExW` (`runas` verb, `SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC`, `SW_HIDE`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellExecuteRunasOutcome {
    /// `ShellExecuteExW` succeeded and the child exited with `Some(exit_code)` (or `None` if `GetExitCodeProcess` failed).
    Started { exit_code: Option<u32> },
    /// `ShellExecuteExW` returned `TRUE`, but `hProcess` was `nullptr`.
    NullProcessHandle,
    /// `ShellExecuteExW` returned `FALSE` with `GetLastError() == win32_error`.
    Failed { win32_error: u32 },
}

pub trait ElevationBackend {
    fn is_elevated(&self) -> bool;
    fn module_path(&self) -> PathBuf;
    fn shell_execute_runas(
        &mut self,
        exe: &Path,
        parameters: Option<&str>,
    ) -> ShellExecuteRunasOutcome;
}

/// Exact port of `relaunch_elevated` in `/native/system-apo/setup/elevate.cpp`:
/// - Returns `(SETUP_EXIT_FAILED, "could not find this program's own path")` if `module_path()` is empty.
/// - Returns `(SETUP_EXIT_DECLINED, "")` on `ERROR_CANCELLED` (`1223`) without setting an error message.
/// - Returns `(SETUP_EXIT_FAILED, "could not ask for administrator rights: ...")` on other `ShellExecuteExW` errors.
/// - Returns `(SETUP_EXIT_FAILED, "the elevated helper did not start")` if `hProcess` is null.
/// - Otherwise waits for the child process and returns its exit code (defaulting to `3` if `GetExitCodeProcess` failed).
pub fn relaunch_elevated<E: ElevationBackend>(
    backend: &mut E,
    arguments: &[String],
) -> (i32, String) {
    let exe = backend.module_path();
    if exe.as_os_str().is_empty() {
        return (
            SETUP_EXIT_FAILED,
            "could not find this program's own path".to_string(),
        );
    }
    let line = format_elevated_parameters(arguments);
    match backend.shell_execute_runas(&exe, line.as_deref()) {
        ShellExecuteRunasOutcome::Failed { win32_error } => {
            if win32_error == ERROR_CANCELLED_WIN32 {
                (SETUP_EXIT_DECLINED, String::new())
            } else {
                (
                    SETUP_EXIT_FAILED,
                    format!(
                        "could not ask for administrator rights: {}",
                        describe_error(win32_error)
                    ),
                )
            }
        }
        ShellExecuteRunasOutcome::NullProcessHandle => (
            SETUP_EXIT_FAILED,
            "the elevated helper did not start".to_string(),
        ),
        ShellExecuteRunasOutcome::Started { exit_code } => {
            let code = exit_code.unwrap_or(SETUP_EXIT_FAILED as u32) as i32;
            (code, String::new())
        }
    }
}

// ============================================================================
// 3. Render Endpoint Enumerator (`endpoints.h/.cpp`)
// ============================================================================

/// Active render endpoint returned by `list_render_endpoints` (`Endpoint` in `endpoints.h`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupEndpoint {
    /// `{XXXXXXXX-...}` uppercase — the last brace group of the device ID.
    pub guid: String,
    /// `PKEY_Device_FriendlyName` from the endpoint's property store, or empty.
    pub name: String,
}

/// Exact port of `describe_hresult` in `/native/system-apo/setup/endpoints.cpp`:
/// Formats an `HRESULT` as `0x88890008` uppercase hex rather than passing it to `FormatMessage`.
pub fn describe_hresult(code: u32) -> String {
    format!("0x{:08X}", code)
}

/// Exact port of `guid_from_device_id` in `/native/system-apo/setup/endpoints.cpp`:
/// Extracts the last `{...}` brace group from an `IMMDevice::GetId` string (e.g.
/// `"{0.0.0.00000000}.{a1b2c3d4-...}"`) and uppercases ASCII hex letters.
pub fn guid_from_device_id(id: &str) -> String {
    if !id.ends_with('}') {
        return String::new();
    }
    let Some(open) = id.rfind('{') else {
        return String::new();
    };
    id[open..].to_ascii_uppercase()
}

/// Abstraction over `IMMDeviceEnumerator` (`eRender`, `DEVICE_STATE_ACTIVE`) in `endpoints.cpp`.
pub trait EndpointEnumeratorBackend {
    /// Simulates `CoCreateInstance(MMDeviceEnumerator)` -> `Err(hresult)` on failure.
    fn open_enumerator(&self) -> Result<(), u32>;
    /// Simulates `EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)` -> `Err(hresult)` on failure.
    fn open_active_render_collection(&self) -> Result<(), u32>;
    /// Simulates `IMMDeviceCollection::GetCount` -> `Err(hresult)` on failure.
    fn device_count(&self) -> Result<usize, u32>;
    /// Simulates `Item(at)` + `GetId` + `OpenPropertyStore(PKEY_Device_FriendlyName)`.
    /// Returns `None` if `Item(at)` or `GetId` fails, or `Some((raw_device_id, friendly_name))`.
    fn device_at(&self, index: usize) -> Option<(String, String)>;
}

/// Exact port of `list_render_endpoints` in `/native/system-apo/setup/endpoints.cpp`.
pub fn list_render_endpoints<B: EndpointEnumeratorBackend>(
    backend: &B,
) -> Result<Vec<SetupEndpoint>, String> {
    if let Err(hr) = backend.open_enumerator() {
        return Err(format!(
            "could not reach the audio device list: {}",
            describe_hresult(hr)
        ));
    }
    if let Err(hr) = backend.open_active_render_collection() {
        return Err(format!(
            "could not list the outputs: {}",
            describe_hresult(hr)
        ));
    }
    let count = match backend.device_count() {
        Ok(c) => c,
        Err(hr) => {
            return Err(format!(
                "could not count the outputs: {}",
                describe_hresult(hr)
            ));
        }
    };

    let mut out = Vec::new();
    for at in 0..count {
        let Some((raw_id, friendly_name)) = backend.device_at(at) else {
            continue;
        };
        let guid = guid_from_device_id(&raw_id);
        if guid.is_empty() {
            continue;
        }
        out.push(SetupEndpoint {
            guid,
            name: friendly_name,
        });
    }
    Ok(out)
}

// ============================================================================
// 4. Exact `attach_outcome`, `slot_memory`, and `slot_report` Helpers
//    (`attach_outcome.cpp`, `slot_memory.cpp`, `slot_report.cpp`)
// ============================================================================

/// Exact port of `AttachOutcome` in `/native/system-apo/setup/attach_outcome.h`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupAttachOutcome {
    pub ok: bool,
    pub error: String,
}

impl Default for SetupAttachOutcome {
    fn default() -> Self {
        Self {
            ok: true,
            error: String::new(),
        }
    }
}

/// Exact port of `summarise_attach_all` in `/native/system-apo/setup/attach_outcome.cpp`:
/// - One attached endpoint makes the whole `--attach-all` (or `suspend-apo`) run succeed;
///   any per-endpoint failures ride along in `endpoints`.
/// - An empty endpoint list succeeds (`!any_failed || any_attached`).
/// - Only when at least one endpoint failed and zero endpoints attached does the command fail
///   with `"no output could be attached — <guid>: <error>; ..."`.
pub fn summarise_attach_all_endpoints(endpoints: &[EndpointAttachResult]) -> SetupAttachOutcome {
    let mut outcome = SetupAttachOutcome::default();
    let mut any_attached = false;
    let mut any_failed = false;
    let mut collected = String::new();

    for one in endpoints {
        if one.ok {
            any_attached = true;
        }
        if one.error.is_empty() {
            continue;
        }
        any_failed = true;
        if !collected.is_empty() {
            collected.push_str("; ");
        }
        collected.push_str(&one.guid);
        collected.push_str(": ");
        collected.push_str(&one.error);
    }

    if !any_failed || any_attached {
        return outcome;
    }
    outcome.ok = false;
    outcome.error = format!("no output could be attached — {}", collected);
    outcome
}

/// `<engine_root>\slots\<guid>.txt`, or `None` when `guid` is not a valid 38-char GUID
/// (`slot_memory_path` in `/native/system-apo/setup/slot_memory.cpp`).
pub fn slot_memory_path(paths: &SetupPaths, guid: &str) -> Option<PathBuf> {
    if !is_valid_endpoint_guid(guid) {
        return None;
    }
    paths.slots_dir().map(|dir| dir.join(format!("{}.txt", guid)))
}

/// Exact port of `remembered_slots` in `/native/system-apo/setup/slot_memory.cpp`.
pub fn remembered_slots_on_disk(paths: &SetupPaths, guid: &str) -> Vec<ApoSlot> {
    let Some(path) = slot_memory_path(paths, guid) else {
        return Vec::new();
    };
    let Some(text) = paths.read_utf8(&path) else {
        return Vec::new();
    };
    parse_remembered_slots(&text)
}

/// Exact port of `remembered_slot` in `/native/system-apo/setup/slot_memory.cpp`.
pub fn remembered_slot_on_disk(paths: &SetupPaths, guid: &str) -> Option<ApoSlot> {
    remembered_slots_on_disk(paths, guid).pop()
}

/// Exact port of `remember_slot` in `/native/system-apo/setup/slot_memory.cpp`:
/// Appends `slot` to `<engine_root>\slots\<guid>.txt` unless it is already the last line.
pub fn remember_slot_on_disk(paths: &SetupPaths, guid: &str, slot: ApoSlot) {
    let Some(path) = slot_memory_path(paths, guid) else {
        return;
    };
    let Some(slots_dir) = paths.slots_dir() else {
        return;
    };
    if !paths.ensure_directory(&slots_dir) {
        return;
    }
    let mut all = remembered_slots_on_disk(paths, guid);
    if all.last().copied() == Some(slot) {
        return;
    }
    let text = append_remembered_slot(&mut all, slot);
    let _ = paths.write_utf8(&path, &text);
}

/// Exact port of `describe_slots` in `/native/system-apo/setup/slot_report.cpp`:
/// Produces the two JSON members (without surrounding braces):
/// `"effects":[{"slot":"sfx"|"mfx"|"efx"|"lfx"|"gfx","from":"list"|"single"|"legacy","clsid":"...","name":"..."},...],"emptySlots":N`
pub fn describe_slots_json<F>(values: &EndpointFxValues, name_lookup: F) -> String
where
    F: Fn(&str) -> String,
{
    const SLOT_NAMES: [&str; FX_SLOT_COUNT] = ["sfx", "mfx", "efx"];
    const LEGACY_NAMES: [&str; FX_LEGACY_COUNT] = ["lfx", "gfx"];

    let mut out = String::from("\"effects\":[");
    let mut first = true;
    let mut empty = 0usize;

    let mut append_effect = |slot: &str, from: &str, clsid: &str| {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str("{\"slot\":\"");
        out.push_str(slot);
        out.push_str("\",\"from\":\"");
        out.push_str(from);
        out.push_str("\",\"clsid\":\"");
        out.push_str(&json_escape(clsid));
        out.push_str("\",\"name\":\"");
        out.push_str(&json_escape(&name_lookup(clsid)));
        out.push_str("\"}");
    };

    for slot in 0..FX_SLOT_COUNT {
        let mut held = false;
        if let Some(list) = values.composite[slot].as_ref() {
            for clsid in list {
                if clsid.is_empty() {
                    continue;
                }
                held = true;
                append_effect(SLOT_NAMES[slot], "list", clsid);
            }
        }
        if let Some(single) = values.single[slot].as_deref() {
            if !single.is_empty() {
                held = true;
                append_effect(SLOT_NAMES[slot], "single", single);
            }
        }
        if !held {
            empty += 1;
        }
    }

    for at in 0..FX_LEGACY_COUNT {
        if let Some(legacy) = values.legacy[at].as_deref() {
            if !legacy.is_empty() {
                append_effect(LEGACY_NAMES[at], "legacy", legacy);
            }
        }
    }

    out.push_str("],\"emptySlots\":");
    out.push_str(&empty.to_string());
    out
}

/// Exact port of `held_slots` in `/native/system-apo/setup/slot_report.cpp`:
/// Returns the rungs of `APO_SLOT_LADDER` that `plan_move` would refuse because a vendor's
/// effect holds that single or legacy value.
pub fn held_slots_in_ladder_order(
    values: &EndpointFxValues,
    backup: &EndpointFxValues,
    clsid: &str,
) -> Vec<ApoSlot> {
    let mut held = Vec::new();
    for slot in APO_SLOT_LADDER {
        if plan_move(values, backup, clsid, slot).refused.is_some() {
            held.push(slot);
        }
    }
    held
}

// ============================================================================
// 5. Elevated Command Runner (`commands.h/.cpp`)
// ============================================================================

/// `kMaxDllBytes` in `/native/system-apo/setup/commands.cpp` (`64 MB`).
pub const MAX_SETUP_DLL_BYTES: usize = 64 * 1024 * 1024;

/// Exact port of `Options` in `/native/system-apo/setup/commands.h`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetupOptions {
    pub command: String,
    pub guids: Vec<String>,
    pub attach_all: bool,
    pub restart_audio: bool,
    pub purge: bool,
    pub slot: Option<ApoSlot>,
}

fn fail_command(result: &mut SetupCommandResult, message: String) {
    result.ok = false;
    result.error = message;
}

/// Exact port of `slot_for` in `/native/system-apo/setup/commands.cpp`:
/// 1. Returns `remembered_slot_on_disk` if this endpoint was previously attached with `--slot`.
/// 2. Otherwise returns `default_slot_for(original, endpoint_is_combined(guid))`.
pub fn slot_for_endpoint<R: RegistryBackend>(
    paths: &SetupPaths,
    reg: &R,
    guid: &str,
    original: &EndpointFxValues,
) -> ApoSlot {
    if let Some(learned) = remembered_slot_on_disk(paths, guid) {
        return learned;
    }
    default_slot_for(original, endpoint_is_combined(reg, guid))
}

/// Exact port of `attach_one` in `/native/system-apo/setup/commands.cpp`.
pub fn attach_one_endpoint<R: RegistryBackend>(
    paths: &SetupPaths,
    reg: &mut R,
    guid: &str,
    named: Option<ApoSlot>,
    attached: &mut bool,
    error: &mut String,
) -> bool {
    let before = match read_fx_values(reg, guid) {
        Ok(v) => v,
        Err(e) => {
            *error = e;
            return false;
        }
    };
    let Some(engine_root) = paths.engine_root() else {
        *error = "could not find the machine's program data directory".to_string();
        return false;
    };
    if !is_attached(&before, ENGINE_CLSID_UPPER) {
        if let Err(e) = save_backup_once_on_disk(&engine_root, guid, &before) {
            *error = e;
            return false;
        }
    }
    let saved = load_backup_from_disk(&engine_root, guid);
    let reference = saved.as_ref().unwrap_or(&before);
    let slot = named.unwrap_or_else(|| slot_for_endpoint(paths, reg, guid, reference));
    let plan = plan_move(&before, reference, ENGINE_CLSID_UPPER, slot);
    if let Some(refused) = plan.refused {
        *error = format!(
            "cannot attach to {} on {}: {}",
            slot.slot_name(),
            guid,
            refused
        );
        return false;
    }
    if plan.changed {
        if let Err(e) = write_fx_values(reg, guid, &before, &plan.after) {
            *error = e;
            return false;
        }
    }
    if named.is_some() {
        remember_slot_on_disk(paths, guid, slot);
    }
    *attached = is_attached(&plan.after, ENGINE_CLSID_UPPER);
    true
}

/// Exact port of `detach_one` in `/native/system-apo/setup/commands.cpp`.
pub fn detach_one_endpoint<R: RegistryBackend>(
    paths: &SetupPaths,
    reg: &mut R,
    guid: &str,
    attached: &mut bool,
    error: &mut String,
) -> bool {
    let current = match read_fx_values(reg, guid) {
        Ok(v) => v,
        Err(e) => {
            *error = e;
            return false;
        }
    };
    let saved = paths
        .engine_root()
        .and_then(|root| load_backup_from_disk(&root, guid));
    let reference = saved.as_ref().unwrap_or(&current);
    let plan = plan_detach(&current, reference, ENGINE_CLSID_UPPER);
    if plan.changed {
        if let Err(e) = write_fx_values(reg, guid, &current, &plan.after) {
            *error = e;
            return false;
        }
    }
    *attached = is_attached(&plan.after, ENGINE_CLSID_UPPER);
    if !*attached {
        if let Some(root) = paths.engine_root() {
            remove_backup_on_disk(&root, guid);
        }
    }
    true
}

/// Exact port of `endpoints_with_backups` in `/native/system-apo/setup/commands.cpp`.
pub fn endpoints_with_backups(paths: &SetupPaths) -> Vec<String> {
    let Some(dir) = paths.backup_dir() else {
        return Vec::new();
    };
    let mut guids = Vec::new();
    for name in SetupPaths::files_matching(&dir, "*.json") {
        let Some(dot) = name.rfind('.') else {
            continue;
        };
        let guid = &name[..dot];
        if is_valid_endpoint_guid(guid) {
            guids.push(guid.to_string());
        }
    }
    guids
}

/// Exact port of `attach_each` in `/native/system-apo/setup/commands.cpp`.
pub fn attach_each_endpoint<R: RegistryBackend>(
    paths: &SetupPaths,
    reg: &mut R,
    guids: &[String],
    slot: Option<ApoSlot>,
    result: &mut SetupCommandResult,
) {
    for guid in guids {
        if !endpoint_key_exists(reg, guid) {
            fail_command(result, format!("there is no output with the id {}", guid));
            return;
        }
        let mut attached = false;
        if !attach_one_endpoint(paths, reg, guid, slot, &mut attached, &mut result.error) {
            result.ok = false;
            result.endpoints.push(EndpointAttachResult {
                guid: guid.clone(),
                ok: attached,
                changed: false,
                error: String::new(),
            });
            return;
        }
        result.endpoints.push(EndpointAttachResult {
            guid: guid.clone(),
            ok: attached,
            changed: true,
            error: String::new(),
        });
    }
}

/// Exact port of `attach_all_endpoints` in `/native/system-apo/setup/commands.cpp`.
pub fn attach_all_endpoints<R: RegistryBackend>(
    paths: &SetupPaths,
    reg: &mut R,
    guids: &[String],
    slot: Option<ApoSlot>,
    result: &mut SetupCommandResult,
) {
    for guid in guids {
        let mut attached = false;
        let mut error = String::new();
        if !endpoint_key_exists(reg, guid) {
            error = format!("there is no output with the id {}", guid);
        } else if !attach_one_endpoint(paths, reg, guid, slot, &mut attached, &mut error) {
            if error.is_empty() {
                error = "the output could not be attached".to_string();
            }
        }
        result.endpoints.push(EndpointAttachResult {
            guid: guid.clone(),
            ok: attached,
            changed: attached,
            error,
        });
    }
    let outcome = summarise_attach_all_endpoints(&result.endpoints);
    if !outcome.ok {
        fail_command(result, outcome.error);
    }
}

/// Exact port of `suspend_apo_one` in `/native/system-apo/setup/commands.cpp`.
pub fn suspend_apo_one<R: RegistryBackend>(
    paths: &SetupPaths,
    reg: &mut R,
    guid: &str,
    attached: &mut bool,
    error: &mut String,
) -> bool {
    let before = match read_fx_values(reg, guid) {
        Ok(v) => v,
        Err(e) => {
            *error = e;
            return false;
        }
    };
    let plan = plan_suspend_apo(&before);
    if !plan.changed {
        *attached = false;
        return true;
    }
    let Some(engine_root) = paths.engine_root() else {
        *error = "could not find the machine's program data directory".to_string();
        return false;
    };
    if let Err(e) = save_apo_off_once_on_disk(&engine_root, guid, &before) {
        *error = e;
        return false;
    }
    if let Err(e) = write_fx_values(reg, guid, &before, &plan.after) {
        *error = e;
        return false;
    }
    *attached = false;
    true
}

/// Exact port of `restore_apo_one` in `/native/system-apo/setup/commands.cpp`.
pub fn restore_apo_one<R: RegistryBackend>(
    paths: &SetupPaths,
    reg: &mut R,
    guid: &str,
    attached: &mut bool,
    error: &mut String,
) -> bool {
    let Some(engine_root) = paths.engine_root() else {
        *error = "could not find the machine's program data directory".to_string();
        return false;
    };
    let Some(saved) = load_apo_off_from_disk(&engine_root, guid) else {
        *attached = false;
        return true;
    };
    let current = match read_fx_values(reg, guid) {
        Ok(v) => v,
        Err(e) => {
            *error = e;
            return false;
        }
    };
    let plan = plan_restore_apo_with_keep(&current, &saved, ENGINE_CLSID_UPPER);
    if plan.changed {
        if let Err(e) = write_fx_values(reg, guid, &current, &plan.after) {
            *error = e;
            return false;
        }
    }
    remove_apo_off_on_disk(&engine_root, guid);
    *attached = true;
    true
}

/// Exact port of `suspend_apo_all` in `/native/system-apo/setup/commands.cpp`.
pub fn suspend_apo_all<R: RegistryBackend, E: EndpointEnumeratorBackend>(
    paths: &SetupPaths,
    reg: &mut R,
    enumerator: &E,
    result: &mut SetupCommandResult,
) {
    let endpoints = match list_render_endpoints(enumerator) {
        Ok(list) => list,
        Err(e) => {
            fail_command(result, e);
            return;
        }
    };
    for endpoint in endpoints {
        if !endpoint_key_exists(reg, &endpoint.guid) {
            continue;
        }
        let mut attached = false;
        let mut error = String::new();
        if !suspend_apo_one(paths, reg, &endpoint.guid, &mut attached, &mut error) {
            if error.is_empty() {
                error = "Equalizer APO could not be switched off on this output".to_string();
            }
        }
        result.endpoints.push(EndpointAttachResult {
            guid: endpoint.guid,
            ok: attached,
            changed: error.is_empty(),
            error,
        });
    }
    // Note: in `suspend_apo_one`, `attached` is `false` on success (reporting whether Equalizer APO
    // is still attached afterwards), while `summarise_attach_all` checks `!any_failed || any_attached`.
    let outcome = summarise_attach_all_endpoints(&result.endpoints);
    if !outcome.ok {
        fail_command(result, outcome.error);
    }
}

/// Exact port of `restore_apo_all` in `/native/system-apo/setup/commands.cpp`.
pub fn restore_apo_all<R: RegistryBackend>(
    paths: &SetupPaths,
    reg: &mut R,
    result: &mut SetupCommandResult,
) {
    let Some(engine_root) = paths.engine_root() else {
        return;
    };
    for guid in apo_off_endpoints_on_disk(&engine_root) {
        let mut attached = false;
        let mut error = String::new();
        if !restore_apo_one(paths, reg, &guid, &mut attached, &mut error) && error.is_empty() {
            error = "Equalizer APO could not be put back on this output".to_string();
        }
        result.endpoints.push(EndpointAttachResult {
            guid,
            ok: attached,
            changed: attached,
            error,
        });
    }
    let failed = result
        .endpoints
        .iter()
        .filter(|one| !one.error.is_empty())
        .count();
    if failed != 0 {
        fail_command(
            result,
            format!(
                "Equalizer APO could not be put back on {} output(s)",
                failed
            ),
        );
    }
}

/// Exact port of `detach_each` in `/native/system-apo/setup/commands.cpp`.
pub fn detach_each_endpoint<R: RegistryBackend>(
    paths: &SetupPaths,
    reg: &mut R,
    guids: &[String],
    result: &mut SetupCommandResult,
) {
    for guid in guids {
        let mut attached = true;
        if !detach_one_endpoint(paths, reg, guid, &mut attached, &mut result.error) {
            result.ok = false;
            result.endpoints.push(EndpointAttachResult {
                guid: guid.clone(),
                ok: attached,
                changed: false,
                error: String::new(),
            });
            return;
        }
        result.endpoints.push(EndpointAttachResult {
            guid: guid.clone(),
            ok: attached,
            changed: !attached,
            error: String::new(),
        });
    }
}

/// Exact port of `complete_modes` in `/native/system-apo/setup/commands.cpp`:
/// Populates missing `modes` values on every endpoint that has a backup file (`plan_mode_in_place`).
pub fn complete_modes<R: RegistryBackend>(
    paths: &SetupPaths,
    reg: &mut R,
    result: &mut SetupCommandResult,
) {
    for guid in endpoints_with_backups(paths) {
        let before = match read_fx_values(reg, &guid) {
            Ok(v) => v,
            Err(error) => {
                result.endpoints.push(EndpointAttachResult {
                    guid,
                    ok: false,
                    changed: false,
                    error: format!("could not read the output's effects: {}", error),
                });
                continue;
            }
        };
        let plan = plan_mode_in_place(&before, ENGINE_CLSID_UPPER);
        if !plan.changed {
            continue;
        }
        let mut error = String::new();
        if let Err(e) = write_fx_values(reg, &guid, &before, &plan.after) {
            error = format!("could not give the engine its processing mode: {}", e);
        }
        result.endpoints.push(EndpointAttachResult {
            guid,
            ok: true,
            changed: error.is_empty(),
            error,
        });
    }
}

/// Exact port of `remove_install_tree` in `/native/system-apo/setup/commands.cpp`:
/// Deletes installed files in `directory`, moving locked DLLs aside to `<file>.removed` when
/// `audiodg.exe` still holds them mapped.
pub fn remove_install_tree(directory: &Path) {
    for name in SetupPaths::files_matching(directory, "*") {
        let file = directory.join(&name);
        if std::fs::remove_file(&file).is_ok() {
            continue;
        }
        let aside = PathBuf::from(format!("{}.removed", file.display()));
        let _ = std::fs::remove_file(&aside);
        let _ = std::fs::rename(&file, &aside);
    }
    let _ = std::fs::remove_dir(directory);
}

/// Exact port of `ensure_engine_tree` in `/native/system-apo/setup/commands.cpp`.
pub fn ensure_engine_tree<S: SecurityBackend>(
    paths: &SetupPaths,
    security: &mut S,
) -> Result<(), String> {
    let (
        Some(parent),
        Some(root),
        Some(config_dir),
        Some(backup_dir),
        Some(apo_off_dir),
        Some(slots_dir),
        Some(log_path),
    ) = (
        paths.engine_parent(),
        paths.engine_root(),
        paths.config_dir(),
        paths.backup_dir(),
        paths.apo_off_dir(),
        paths.slots_dir(),
        paths.setup_log_path(),
    )
    else {
        return Err("could not find the machine's program data directory".to_string());
    };

    if !paths.ensure_directory(&root)
        || !paths.ensure_directory(&config_dir)
        || !paths.ensure_directory(&backup_dir)
        || !paths.ensure_directory(&apo_off_dir)
        || !paths.ensure_directory(&slots_dir)
    {
        return Err(format!("could not create {}", root.display()));
    }

    for folder in [&parent, &root, &backup_dir, &apo_off_dir, &slots_dir] {
        take_ownership(security, folder)?;
    }

    apply_read_only_acl(security, &parent)?;
    apply_engine_acl(security, &root)?;
    apply_config_acl(security, &config_dir)?;
    apply_read_only_acl(security, &backup_dir)?;
    apply_read_only_acl(security, &apo_off_dir)?;
    apply_read_only_acl(security, &slots_dir)?;

    if !SetupPaths::path_exists(&log_path) && !paths.append_utf8(&log_path, "") {
        return Err(format!("could not create {}", log_path.display()));
    }
    apply_log_acl(security, &log_path)?;

    let config_txt = config_dir.join("config.txt");
    if !SetupPaths::path_exists(&config_txt) && !paths.write_utf8(&config_txt, "") {
        return Err(format!("could not create {}", config_txt.display()));
    }
    Ok(())
}

/// Exact port of `run_command` in `/native/system-apo/setup/commands.cpp`.
pub fn run_setup_command<
    R: RegistryBackend,
    S: SecurityBackend,
    E: EndpointEnumeratorBackend,
    C: ServiceControl,
>(
    options: &SetupOptions,
    paths: &SetupPaths,
    manifest: &[ShippedDllEntry],
    reg: &mut R,
    security: &mut S,
    enumerator: &E,
    services: &mut C,
    result: &mut SetupCommandResult,
) {
    if options.command == "install" {
        let source = &paths.module_dir;
        let Some(target) = paths.install_dir() else {
            fail_command(
                result,
                "could not work out where to install from or to".to_string(),
            );
            return;
        };
        if source.as_os_str().is_empty() {
            fail_command(
                result,
                "could not work out where to install from or to".to_string(),
            );
            return;
        }
        let engine_dll = source.join("FluidEQ-Engine.dll");
        if !SetupPaths::path_exists(&engine_dll) {
            fail_command(
                result,
                "FluidEQ-Engine.dll is not next to the setup program".to_string(),
            );
            return;
        }
        if !paths.ensure_directory(&target) {
            fail_command(result, format!("could not create {}", target.display()));
            return;
        }

        let mut verified: Vec<(String, Vec<u8>)> = Vec::new();
        for name in SetupPaths::files_matching(source, "*.dll") {
            let expected = shipped_digest(manifest, &name);
            let bytes = expected
                .and_then(|_| SetupPaths::read_locked(&source.join(&name), MAX_SETUP_DLL_BYTES));
            match (expected, bytes) {
                (Some(exp), Some(data)) if sha256_hex(&data) == exp => {
                    verified.push((name, data));
                }
                _ => {
                    fail_command(
                        result,
                        format!(
                            "{} beside the setup program is not the one it was built with, so nothing was installed",
                            name
                        ),
                    );
                    return;
                }
            }
        }

        for (name, bytes) in &verified {
            if let Err(e) =
                SetupPaths::replace_with_bytes_simulated(bytes, &target.join(name), false)
            {
                fail_command(result, e);
                return;
            }
        }

        let installed_engine = target.join("FluidEQ-Engine.dll");
        let installed_engine_str = installed_engine.to_string_lossy().into_owned();
        if let Err(e) = register_engine(reg, &installed_engine_str) {
            fail_command(result, e);
            return;
        }
        if let Err(e) = enable_unsigned_effects(reg) {
            fail_command(result, e);
            return;
        }
        if let Err(e) = ensure_engine_tree(paths, security) {
            fail_command(result, e);
            return;
        }

        if options.attach_all {
            let found = match list_render_endpoints(enumerator) {
                Ok(list) => list,
                Err(e) => {
                    fail_command(result, e);
                    return;
                }
            };
            let guids: Vec<String> = found.into_iter().map(|ep| ep.guid).collect();
            attach_all_endpoints(paths, reg, &guids, options.slot, result);
            if !result.ok {
                return;
            }
        }

        complete_modes(paths, reg, result);
        if options.restart_audio {
            if let Err(e) = restart_audio(services) {
                fail_command(result, e);
            }
        }
        return;
    }

    if options.command == "uninstall" {
        let mut guids = endpoints_with_backups(paths);
        if let Ok(active) = list_render_endpoints(enumerator) {
            for endpoint in active {
                if let Ok(values) = read_fx_values(reg, &endpoint.guid) {
                    if is_attached(&values, ENGINE_CLSID_UPPER) && !guids.contains(&endpoint.guid) {
                        guids.push(endpoint.guid);
                    }
                }
            }
        }
        detach_each_endpoint(paths, reg, &guids, result);
        if !result.ok {
            return;
        }

        let mut restored = SetupCommandResult::default();
        restore_apo_all(paths, reg, &mut restored);
        let apo_left_off = !restored.ok;
        for one in restored.endpoints {
            result.endpoints.push(one);
        }

        if let Err(e) = unregister_engine(reg) {
            fail_command(result, e);
            return;
        }
        if let Some(target) = paths.install_dir() {
            remove_install_tree(&target);
        }
        if options.purge && !apo_left_off {
            if let Some(root) = paths.engine_root() {
                SetupPaths::delete_directory_tree(&root);
            }
        }
        if apo_left_off {
            fail_command(result, restored.error);
        }
        return;
    }

    if options.command == "restart-audio" {
        if let Err(e) = restart_audio(services) {
            fail_command(result, e);
        }
        return;
    }

    if options.command == "attach" {
        if let Err(e) = ensure_engine_tree(paths, security) {
            fail_command(result, e);
            return;
        }
        attach_each_endpoint(paths, reg, &options.guids, options.slot, result);
    } else if options.command == "detach" {
        detach_each_endpoint(paths, reg, &options.guids, result);
    } else if options.command == "suspend-apo" {
        if let Err(e) = ensure_engine_tree(paths, security) {
            fail_command(result, e);
            return;
        }
        suspend_apo_all(paths, reg, enumerator, result);
    } else if options.command == "restore-apo" {
        restore_apo_all(paths, reg, result);
    } else {
        fail_command(
            result,
            format!(
                "the command {} reached the runner, which does not know how to run it",
                options.command
            ),
        );
    }

    if result.ok && options.restart_audio {
        if let Err(e) = restart_audio(services) {
            fail_command(result, e);
        }
    }
}

/// Exact port of `result_json` in `/native/system-apo/setup/commands.cpp`:
/// `{"command":"...","ok":true|false,"error":"...","endpoints":[{"guid":"...","attached":true|false(,"error":"...")}]}`
pub fn result_json(command: &str, result: &SetupCommandResult) -> String {
    let mut out = String::from("{\"command\":\"");
    out.push_str(&json_escape(command));
    out.push_str("\",\"ok\":");
    out.push_str(if result.ok { "true" } else { "false" });
    out.push_str(",\"error\":\"");
    out.push_str(&json_escape(&result.error));
    out.push_str("\",\"endpoints\":[");
    for (at, ep) in result.endpoints.iter().enumerate() {
        if at != 0 {
            out.push(',');
        }
        out.push_str("{\"guid\":\"");
        out.push_str(&json_escape(&ep.guid));
        out.push_str("\",\"attached\":");
        out.push_str(if ep.ok { "true" } else { "false" });
        if !ep.error.is_empty() {
            out.push_str(",\"error\":\"");
            out.push_str(&json_escape(&ep.error));
            out.push('"');
        }
        out.push('}');
    }
    out.push_str("]}");
    out
}

// ============================================================================
// 6. Command-Line Parser, Status Report & Elevation Orchestrator (`main.cpp`)
// ============================================================================

/// `kUsage` in `/native/system-apo/setup/main.cpp`.
pub const SETUP_USAGE_TEXT: &str = concat!(
    "FluidEQ-Engine-Setup <command> [options]\n\n",
    "  install [--attach-all] [--restart-audio]\n",
    "  attach <output-id>... [--slot efx|mfx|sfx|efx-single|mfx-single|sfx-single|gfx|lfx] [--restart-audio]\n",
    "  detach <output-id>... [--restart-audio]\n",
    "  uninstall [--purge]\n",
    "  suspend-apo [--restart-audio]\n",
    "  restore-apo [--restart-audio]\n",
    "  restart-audio\n",
    "  status\n",
    "  settle\n\n",
    "An output id looks like {00000000-0000-0000-0000-000000000000} and is\n",
    "listed by the status command. status and settle (which waits until\n",
    "Windows audio has finished starting or stopping) never ask for\n",
    "administrator rights. Every other command is tried up to three times,\n",
    "waiting for Windows audio to settle between tries.\n\n",
    "From an interactive shell, pipe or capture the output (the helper is a\n",
    "windowed program and the shell will not wait for it otherwise).\n\n",
    "Exit codes: 0 done, 1 command line, 2 elevation declined, 3 failed.\n"
);

/// Exact port of `parse` in `/native/system-apo/setup/main.cpp`:
/// Parses `argv[1..]` into `SetupOptions`, enforcing all command-specific flag restrictions.
pub fn parse_setup_command_line(args: &[String]) -> Option<SetupOptions> {
    if args.is_empty() {
        return None;
    }
    let command = args[0].clone();
    let takes_guids = command == "attach" || command == "detach";
    if command != "install"
        && !takes_guids
        && command != "uninstall"
        && command != "suspend-apo"
        && command != "restore-apo"
        && command != "restart-audio"
        && command != "status"
        && command != "settle"
    {
        return None;
    }

    let mut options = SetupOptions {
        command,
        ..SetupOptions::default()
    };

    let mut at = 1usize;
    while at < args.len() {
        let argument = &args[at];
        if argument == "--attach-all" && options.command == "install" {
            options.attach_all = true;
        } else if argument == "--restart-audio"
            && options.command != "uninstall"
            && options.command != "restart-audio"
            && options.command != "status"
            && options.command != "settle"
        {
            options.restart_audio = true;
        } else if argument == "--purge" && options.command == "uninstall" {
            options.purge = true;
        } else if argument == "--slot" && options.command == "attach" {
            if at + 1 >= args.len() {
                return None;
            }
            at += 1;
            let parsed_slot = ApoSlot::from_slot_name(&args[at])?;
            options.slot = Some(parsed_slot);
        } else if takes_guids && is_valid_endpoint_guid(argument) {
            options.guids.push(argument.clone());
        } else {
            return None;
        }
        at += 1;
    }

    if takes_guids && options.guids.is_empty() {
        return None;
    }
    Some(options)
}

/// Exact port of `runtime_beside` in `/native/system-apo/setup/main.cpp`:
/// Checks whether `msvcp140.dll`, `vcruntime140.dll`, and `vcruntime140_1.dll` exist in the
/// same directory as `dll_path`.
pub fn runtime_beside(dll_path: &Path) -> bool {
    let Some(directory) = dll_path.parent() else {
        return false;
    };
    if directory.as_os_str().is_empty() {
        return false;
    }
    for name in ["msvcp140.dll", "vcruntime140.dll", "vcruntime140_1.dll"] {
        if !SetupPaths::path_exists(&directory.join(name)) {
            return false;
        }
    }
    true
}

/// Exact port of `engine_has_run` in `/native/system-apo/setup/main.cpp`:
/// Checks whether `<engine_root>\engine.log` or any `status-*.json` file exists.
pub fn engine_has_run(paths: &SetupPaths) -> bool {
    let Some(root) = paths.engine_root() else {
        return false;
    };
    SetupPaths::path_exists(&root.join("engine.log"))
        || !SetupPaths::files_matching(&root, "status-*.json").is_empty()
}

/// Exact port of `registered_class_name` in `/native/system-apo/setup/main.cpp`:
/// Reads the default `REG_SZ` value of `SOFTWARE\Classes\CLSID\<clsid>`.
pub fn registered_class_name<R: RegistryBackend>(reg: &R, clsid: &str) -> String {
    let path = format!("SOFTWARE\\Classes\\CLSID\\{}", clsid);
    match reg.query_value(&path, "") {
        Ok(Some(raw)) if raw.value_type == REG_SZ_TYPE => decode_sz(&raw.bytes),
        _ => String::new(),
    }
}

/// Exact port of `print_status` in `/native/system-apo/setup/main.cpp`:
/// Never elevates and never writes. Queries `list_render_endpoints` up to `COMMAND_TRIES` times
/// waiting for Windows audio to settle between tries, and returns `(exit_code, json_line)`.
pub fn build_status_report_json<
    R: RegistryBackend,
    S: SecurityBackend,
    E: EndpointEnumeratorBackend,
    W: ServiceStateWaiter,
>(
    paths: &SetupPaths,
    reg: &R,
    security: &S,
    enumerator: &E,
    waiter: &mut W,
    dll_version_lookup: &dyn Fn(&Path) -> String,
) -> (i32, String) {
    let mut endpoints: Vec<SetupEndpoint> = Vec::new();
    let listed = run_with_retries(
        COMMAND_TRIES,
        |attempt| {
            endpoints.clear();
            match list_render_endpoints(enumerator) {
                Ok(found) => {
                    endpoints = found;
                    attempt.ok = true;
                }
                Err(e) => {
                    attempt.ok = false;
                    attempt.error = e;
                }
            }
        },
        |why| match wait_audio_settled(waiter) {
            Ok(()) => true,
            Err(e) => {
                *why = e;
                false
            }
        },
    );

    if !listed.ok {
        return (
            SETUP_EXIT_FAILED,
            format!("{{\"error\":\"{}\"}}", json_escape(&listed.error)),
        );
    }

    let registered = registered_dll_path(reg);
    let registered_path = PathBuf::from(&registered);
    let installed = !registered.is_empty()
        && SetupPaths::path_exists(&registered_path)
        && apo_record_present(reg);

    let fallback_dll_path = paths
        .installed_dll_path()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let active_dll_str = if installed {
        registered.as_str()
    } else {
        fallback_dll_path.as_str()
    };
    let active_dll_path = PathBuf::from(active_dll_str);
    let dll_version = if installed {
        dll_version_lookup(&registered_path)
    } else {
        String::new()
    };
    let config_dir_str = paths
        .config_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let engine_root_path = paths.engine_root().unwrap_or_default();

    let mut out = String::from("{\"installed\":");
    out.push_str(if installed { "true" } else { "false" });
    out.push_str(",\"dllPath\":\"");
    out.push_str(&json_escape(active_dll_str));
    out.push_str("\",\"dllVersion\":\"");
    out.push_str(&json_escape(&dll_version));
    out.push_str("\",\"configDir\":\"");
    out.push_str(&json_escape(&config_dir_str));
    out.push_str("\",\"unsignedAllowed\":");
    out.push_str(if unsigned_effects_enabled(reg) {
        "true"
    } else {
        "false"
    });
    out.push_str(",\"runtimeBeside\":");
    out.push_str(if runtime_beside(&active_dll_path) {
        "true"
    } else {
        "false"
    });
    out.push_str(",\"everRan\":");
    out.push_str(if engine_has_run(paths) {
        "true"
    } else {
        "false"
    });
    out.push_str(",\"serviceCanWrite\":");
    out.push_str(
        if !engine_root_path.as_os_str().is_empty()
            && service_can_write(security, &engine_root_path)
        {
            "true"
        } else {
            "false"
        },
    );
    out.push_str(",\"endpoints\":[");

    for (at, ep) in endpoints.iter().enumerate() {
        if at != 0 {
            out.push(',');
        }
        let values = read_fx_values(reg, &ep.guid).unwrap_or_default();
        let attached = is_attached(&values, ENGINE_CLSID_UPPER);
        let backup_exists = !engine_root_path.as_os_str().is_empty()
            && backup_exists_on_disk(&engine_root_path, &ep.guid);
        let slot = slot_of(&values, ENGINE_CLSID_UPPER);
        let tried = remembered_slots_on_disk(paths, &ep.guid);
        let backup_ref = if !engine_root_path.as_os_str().is_empty() {
            load_backup_from_disk(&engine_root_path, &ep.guid)
        } else {
            None
        };
        let held = held_slots_in_ladder_order(
            &values,
            backup_ref.as_ref().unwrap_or(&values),
            ENGINE_CLSID_UPPER,
        );

        out.push_str("{\"guid\":\"");
        out.push_str(&json_escape(&ep.guid));
        out.push_str("\",\"name\":\"");
        out.push_str(&json_escape(&ep.name));
        out.push_str("\",\"attached\":");
        out.push_str(if attached { "true" } else { "false" });
        out.push_str(",\"backupExists\":");
        out.push_str(if backup_exists { "true" } else { "false" });
        out.push_str(",\"slot\":");
        match slot {
            Some(s) => {
                out.push('"');
                out.push_str(s.slot_name());
                out.push('"');
            }
            None => out.push_str("null"),
        }
        out.push_str(",\"modeMissing\":");
        out.push_str(if mode_missing(&values, ENGINE_CLSID_UPPER) {
            "true"
        } else {
            "false"
        });
        out.push_str(",\"slotsTried\":[");
        for (step, s) in tried.iter().enumerate() {
            if step != 0 {
                out.push(',');
            }
            out.push('"');
            out.push_str(s.slot_name());
            out.push('"');
        }
        out.push_str("],\"slotsHeld\":[");
        for (step, s) in held.iter().enumerate() {
            if step != 0 {
                out.push(',');
            }
            out.push('"');
            out.push_str(s.slot_name());
            out.push('"');
        }
        out.push_str("],");
        out.push_str(&describe_slots_json(&values, |clsid| {
            registered_class_name(reg, clsid)
        }));
        out.push('}');
    }

    out.push_str("]}");
    (SETUP_EXIT_OK, out)
}

/// Exact port of `print_settled` in `/native/system-apo/setup/main.cpp`.
pub fn build_settled_report_json<W: ServiceStateWaiter>(waiter: &mut W) -> (i32, String) {
    let mut result = SetupCommandResult::default();
    if let Err(e) = wait_audio_settled(waiter) {
        result.ok = false;
        result.error = e;
    }
    let json = result_json("settle", &result);
    (
        if result.ok {
            SETUP_EXIT_OK
        } else {
            SETUP_EXIT_FAILED
        },
        json,
    )
}

/// Exact port of `note_run` formatting in `/native/system-apo/setup/main.cpp`.
pub fn format_note_run_entry(
    iso_utc_timestamp: &str,
    arguments: &[String],
    result: &SetupCommandResult,
    code: i32,
) -> String {
    let mut line = String::from(iso_utc_timestamp);
    line.push_str(" setup");
    for arg in arguments {
        line.push(' ');
        line.push_str(arg);
    }
    line.push_str(if result.ok { " -> ok" } else { " -> FAILED" });
    line.push_str(&format!(" (exit {})", code));
    if !result.error.is_empty() {
        line.push_str(": ");
        line.push_str(&result.error);
    }
    for one in &result.endpoints {
        line.push_str("\n    ");
        line.push_str(&one.guid);
        line.push_str(if one.ok { " attached" } else { " not attached" });
        if !one.error.is_empty() {
            line.push_str(": ");
            line.push_str(&one.error);
        }
    }
    line.push('\n');
    line
}

/// Appends one `note_run` entry to `<engine_root>\setup.log` if `<engine_root>` exists (`main.cpp`).
pub fn note_run_on_disk(
    paths: &SetupPaths,
    iso_utc_timestamp: &str,
    arguments: &[String],
    result: &SetupCommandResult,
    code: i32,
) {
    let (Some(root), Some(log_path)) = (paths.engine_root(), paths.setup_log_path()) else {
        return;
    };
    if !SetupPaths::path_exists(&root) {
        return;
    }
    let line = format_note_run_entry(iso_utc_timestamp, arguments, result, code);
    let _ = paths.append_utf8(&log_path, &line);
}

/// Exact port of `run_elevated` in `/native/system-apo/setup/main.cpp`.
#[allow(clippy::too_many_arguments)]
pub fn run_elevated_command<
    R: RegistryBackend,
    S: SecurityBackend,
    E: EndpointEnumeratorBackend,
    C: ServiceControl,
    W: ServiceStateWaiter,
>(
    options: &SetupOptions,
    arguments: &[String],
    iso_utc_timestamp: &str,
    paths: &SetupPaths,
    manifest: &[ShippedDllEntry],
    reg: &mut R,
    security: &mut S,
    enumerator: &E,
    services: &mut C,
    waiter: &mut W,
) -> (i32, String) {
    let result = run_with_retries(
        COMMAND_TRIES,
        |attempt| {
            run_setup_command(
                options, paths, manifest, reg, security, enumerator, services, attempt,
            );
        },
        |why| match wait_audio_settled(waiter) {
            Ok(()) => true,
            Err(e) => {
                *why = e;
                false
            }
        },
    );

    let json = result_json(&options.command, &result);
    let code = if result.ok {
        SETUP_EXIT_OK
    } else {
        SETUP_EXIT_FAILED
    };
    note_run_on_disk(paths, iso_utc_timestamp, arguments, &result, code);

    let purged = options.command == "uninstall" && options.purge;
    let leaves_tree_alone = options.command == "restart-audio";
    if !purged {
        if !leaves_tree_alone {
            let _ = ensure_engine_tree(paths, security);
        }
        if let (Some(root), Some(res_path)) = (paths.engine_root(), paths.result_path()) {
            if !leaves_tree_alone || SetupPaths::path_exists(&root) {
                let _ = paths.write_utf8(&res_path, &json);
            }
        }
    }

    (code, json)
}

/// Exact port of `run_through_elevation` in `/native/system-apo/setup/main.cpp`:
/// 1. Deletes any stale `<engine_root>\last-setup.json` first.
/// 2. Calls `relaunch_elevated(arguments)`.
/// 3. If `code == 2` (`SETUP_EXIT_DECLINED`), logs `"the Windows permission prompt was declined"` via `note_run`.
/// 4. Reads `<engine_root>\last-setup.json` unless `code == 2`; if non-empty, returns `(code, reported_json)`.
/// 5. Otherwise synthesizes a fallback `result_json` (`"administrator rights were declined"` for `code == 2`,
///    or `"the elevated helper reported nothing"` when `error` is empty).
pub fn run_through_elevation<E: ElevationBackend>(
    paths: &SetupPaths,
    elevation: &mut E,
    arguments: &[String],
    options: &SetupOptions,
    iso_utc_timestamp: &str,
) -> (i32, String) {
    let res_path = paths.result_path();
    if let Some(ref p) = res_path {
        let _ = std::fs::remove_file(p);
    }

    let (code, error) = relaunch_elevated(elevation, arguments);
    if code == SETUP_EXIT_DECLINED {
        let declined = SetupCommandResult {
            ok: false,
            error: "the Windows permission prompt was declined".to_string(),
            endpoints: Vec::new(),
        };
        note_run_on_disk(paths, iso_utc_timestamp, arguments, &declined, code);
    }

    let reported = if code == SETUP_EXIT_DECLINED {
        None
    } else {
        res_path.as_ref().and_then(|p| paths.read_utf8(p))
    };

    if let Some(text) = reported {
        if !text.is_empty() {
            return (code, text);
        }
    }

    let mut fallback = SetupCommandResult {
        ok: code == SETUP_EXIT_OK,
        error: String::new(),
        endpoints: Vec::new(),
    };
    if code == SETUP_EXIT_DECLINED {
        fallback.error = "administrator rights were declined".to_string();
    } else if !fallback.ok {
        fallback.error = if error.is_empty() {
            "the elevated helper reported nothing".to_string()
        } else {
            error
        };
    }

    (code, result_json(&options.command, &fallback))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apo::{FX_SLOT_EFX, FX_SLOT_MFX, FX_SLOT_SFX};
    use crate::apo_setup_registry::InMemoryRegistryHive;
    use crate::apo_setup_services_and_fs::{
        InMemorySecurityBackend, AUDIO_SERVICE_NAME, BUILDER_SERVICE_NAME, SERVICE_RUNNING,
    };

    struct MockEndpoints {
        devices: Vec<(String, String)>,
    }

    impl EndpointEnumeratorBackend for MockEndpoints {
        fn open_enumerator(&self) -> Result<(), u32> {
            Ok(())
        }
        fn open_active_render_collection(&self) -> Result<(), u32> {
            Ok(())
        }
        fn device_count(&self) -> Result<usize, u32> {
            Ok(self.devices.len())
        }
        fn device_at(&self, index: usize) -> Option<(String, String)> {
            self.devices.get(index).cloned()
        }
    }

    struct MockServices;
    impl ServiceControl for MockServices {
        fn running_dependents(&mut self, _service: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }
        fn stop(&mut self, _service: &str) -> Result<(), String> {
            Ok(())
        }
        fn start(&mut self, _service: &str) -> Result<(), String> {
            Ok(())
        }
    }

    struct MockWaiter;
    impl ServiceStateWaiter for MockWaiter {
        fn query_state(&mut self, name: &str) -> Result<u32, String> {
            let _ = (AUDIO_SERVICE_NAME, BUILDER_SERVICE_NAME, name);
            Ok(SERVICE_RUNNING)
        }
        fn wait_for_change(&mut self, _name: &str, _notify_mask: u32) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn attach_outcome_matches_attach_outcome_test_cpp() {
        let attached = |guid: &str| EndpointAttachResult {
            guid: guid.to_string(),
            ok: true,
            changed: true,
            error: String::new(),
        };
        let failed = |guid: &str, why: &str| EndpointAttachResult {
            guid: guid.to_string(),
            ok: false,
            changed: false,
            error: why.to_string(),
        };

        // 1. every_endpoint_attached_is_ok
        let outcome = summarise_attach_all_endpoints(&[attached("{A}"), attached("{B}")]);
        assert!(outcome.ok);
        assert!(outcome.error.is_empty());

        // 2. one_awkward_endpoint_does_not_fail_the_install
        let outcome = summarise_attach_all_endpoints(&[
            attached("{A}"),
            failed("{B}", "composite list is REG_EXPAND_SZ"),
            attached("{C}"),
        ]);
        assert!(outcome.ok);
        assert!(outcome.error.is_empty());

        // 3. no_endpoint_attached_is_a_failure_that_says_why
        let outcome = summarise_attach_all_endpoints(&[
            failed("{A}", "access denied"),
            failed("{B}", "composite list is REG_EXPAND_SZ"),
        ]);
        assert!(!outcome.ok);
        assert!(outcome.error.contains("access denied"));
        assert!(outcome.error.contains("REG_EXPAND_SZ"));
        assert!(outcome.error.contains("{A}"));
        assert!(outcome.error.contains("{B}"));

        // 4. a_single_failing_endpoint_is_a_failure
        let outcome = summarise_attach_all_endpoints(&[failed("{A}", "access denied")]);
        assert!(!outcome.ok);

        // 5. no_endpoints_at_all_is_not_a_failure
        let outcome = summarise_attach_all_endpoints(&[]);
        assert!(outcome.ok);
        assert!(outcome.error.is_empty());
    }

    #[test]
    fn slot_report_matches_slot_report_test_cpp() {
        let lookup = |clsid: &str| -> String {
            match clsid {
                "{THX}" => "THX Spatial Audio".to_string(),
                "{OURS}" => "FluidEQ Engine".to_string(),
                _ => String::new(),
            }
        };

        let mut values = EndpointFxValues::default();
        values.composite[FX_SLOT_SFX] = Some(vec!["{THX}".to_string()]);
        values.composite[FX_SLOT_MFX] = Some(vec!["{THX}".to_string(), "{X}".to_string()]);
        values.composite[FX_SLOT_EFX] = Some(vec!["{OURS}".to_string()]);
        values.single[FX_SLOT_SFX] = Some("{APO}".to_string());
        values.legacy[0] = Some("{OLD}".to_string());

        let json = describe_slots_json(&values, lookup);
        assert!(json.contains(
            "\"slot\":\"sfx\",\"from\":\"list\",\"clsid\":\"{THX}\",\"name\":\"THX Spatial Audio\""
        ));
        assert!(json.contains("\"slot\":\"mfx\",\"from\":\"list\",\"clsid\":\"{X}\",\"name\":\"\""));
        assert!(json.contains(
            "\"slot\":\"efx\",\"from\":\"list\",\"clsid\":\"{OURS}\",\"name\":\"FluidEQ Engine\""
        ));
        assert!(json.contains("\"slot\":\"sfx\",\"from\":\"single\",\"clsid\":\"{APO}\""));
        assert!(json.contains("\"slot\":\"lfx\",\"from\":\"legacy\",\"clsid\":\"{OLD}\""));
        assert!(json.contains("\"emptySlots\":0"));

        // counts_the_slots_that_hold_nothing
        let mut sparse = EndpointFxValues::default();
        sparse.composite[FX_SLOT_EFX] = Some(vec!["{OURS}".to_string()]);
        sparse.composite[FX_SLOT_SFX] = Some(Vec::new());
        sparse.single[FX_SLOT_MFX] = Some(String::new());
        let sparse_json = describe_slots_json(&sparse, lookup);
        assert!(sparse_json.contains("\"emptySlots\":2"));

        // nothing_registered_is_three_empty_slots
        let empty_json = describe_slots_json(&EndpointFxValues::default(), lookup);
        assert_eq!(empty_json, "\"effects\":[],\"emptySlots\":3");

        // held_slots_in_ladder_order
        let ours = "{B7E2C4D1-5A8F-4C3E-9D2B-6F1A0C8E7D34}";
        let vendor = "{5C8DC6DB-1A99-46FE-90E8-8229A41DD3EF}";
        let win_lfx = "{62DC1A93-AE24-464C-A43E-452F824C4250}";

        let mut held_case = EndpointFxValues::default();
        held_case.single[FX_SLOT_SFX] = Some(vendor.to_string());
        held_case.single[FX_SLOT_EFX] = Some(ours.to_string());
        held_case.legacy[0] = Some(win_lfx.to_string());
        assert_eq!(
            held_slots_in_ladder_order(&held_case, &held_case, ours),
            vec![ApoSlot::SfxSingle]
        );
    }

    #[test]
    fn quote_and_parse_command_line_match_elevate_and_main_cpp() {
        assert_eq!(quote_command_line_arg("--attach-all"), "--attach-all");
        assert_eq!(
            quote_command_line_arg("C:\\Program Files\\FluidEQ Engine\\"),
            "\"C:\\Program Files\\FluidEQ Engine\\\\\""
        );
        assert_eq!(
            quote_command_line_arg("a\\\"b"),
            "\"a\\\\\\\"b\""
        );

        let guid = "{11111111-2222-3333-4444-555555555555}".to_string();
        let parsed = parse_setup_command_line(&[
            "attach".to_string(),
            guid.clone(),
            "--slot".to_string(),
            "mfx-single".to_string(),
            "--restart-audio".to_string(),
        ])
        .expect("valid attach command");
        assert_eq!(parsed.command, "attach");
        assert_eq!(parsed.guids, vec![guid]);
        assert_eq!(parsed.slot, Some(ApoSlot::MfxSingle));
        assert!(parsed.restart_audio);

        // `--restart-audio` is rejected on `uninstall` and `status`
        assert!(parse_setup_command_line(&[
            "uninstall".to_string(),
            "--restart-audio".to_string()
        ])
        .is_none());
        assert!(parse_setup_command_line(&["attach".to_string()]).is_none());
    }

    #[test]
    fn console_borrowing_and_end_to_end_install_status_uninstall_round_trip() {
        // Inherited stdout pipe + no inherited stderr -> attaches parent console for stderr only
        let plan = plan_borrow_caller_console(0x100, 0, Some(0x100), None, true);
        assert!(plan.attached_parent_console);
        assert_eq!(
            plan.stdout_action,
            ConsoleStreamBindAction::AlreadyBoundToInherited
        );
        assert_eq!(
            plan.stderr_action,
            ConsoleStreamBindAction::ReopenOnParentConsole
        );

        // End-to-end install -> status -> uninstall in temp sandbox directory
        let temp_root = std::env::temp_dir().join(format!(
            "fluideq_setup_cmds_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&temp_root);
        let program_data = temp_root.join("ProgramData");
        let program_files = temp_root.join("ProgramFiles");
        let module_dir = temp_root.join("AppBin");
        std::fs::create_dir_all(&module_dir).unwrap();

        let dll_bytes = b"simulated-fluideq-engine-dll-binary".to_vec();
        let dll_sha = sha256_hex(&dll_bytes);
        std::fs::write(module_dir.join("FluidEQ-Engine.dll"), &dll_bytes).unwrap();

        let manifest = vec![ShippedDllEntry {
            name: "FluidEQ-Engine.dll".to_string(),
            sha256: dll_sha,
        }];

        let paths = SetupPaths::new(program_data, program_files, module_dir);
        let mut reg = InMemoryRegistryHive::new();
        let guid = "{11111111-2222-3333-4444-555555555555}";
        let ep_key = format!(
            "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\MMDevices\\Audio\\Render\\{}",
            guid
        );
        reg.create_key(&ep_key).unwrap();
        reg.create_key(&format!("{}\\FxProperties", ep_key)).unwrap();

        let mut security = InMemorySecurityBackend::new();
        let enumerator = MockEndpoints {
            devices: vec![(
                format!("{{0.0.0.00000000}}.{}", guid.to_ascii_lowercase()),
                "Studio Headphones".to_string(),
            )],
        };
        let mut services = MockServices;
        let mut waiter = MockWaiter;

        let install_opts = SetupOptions {
            command: "install".to_string(),
            attach_all: true,
            restart_audio: true,
            ..SetupOptions::default()
        };
        let (code, install_json) = run_elevated_command(
            &install_opts,
            &[
                "install".to_string(),
                "--attach-all".to_string(),
                "--restart-audio".to_string(),
            ],
            "2026-11-05T12:00:00.000Z",
            &paths,
            &manifest,
            &mut reg,
            &mut security,
            &enumerator,
            &mut services,
            &mut waiter,
        );
        assert_eq!(code, SETUP_EXIT_OK);
        assert!(install_json.contains("\"ok\":true"));

        let (status_code, status_json) = build_status_report_json(
            &paths,
            &reg,
            &security,
            &enumerator,
            &mut waiter,
            &|_| "2.1.0.0".to_string(),
        );
        assert_eq!(status_code, SETUP_EXIT_OK);
        assert!(status_json.contains("\"installed\":true"));
        assert!(status_json.contains("\"dllVersion\":\"2.1.0.0\""));
        assert!(status_json.contains("\"attached\":true"));

        let uninstall_opts = SetupOptions {
            command: "uninstall".to_string(),
            purge: true,
            ..SetupOptions::default()
        };
        let (un_code, un_json) = run_elevated_command(
            &uninstall_opts,
            &["uninstall".to_string(), "--purge".to_string()],
            "2026-11-05T12:01:00.000Z",
            &paths,
            &manifest,
            &mut reg,
            &mut security,
            &enumerator,
            &mut services,
            &mut waiter,
        );
        assert_eq!(un_code, SETUP_EXIT_OK);
        assert!(un_json.contains("\"ok\":true"));
        assert!(!SetupPaths::path_exists(&paths.engine_root().unwrap()));

        let _ = std::fs::remove_dir_all(&temp_root);
    }
}
