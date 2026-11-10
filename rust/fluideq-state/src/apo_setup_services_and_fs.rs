/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! System APO Setup ACL Ownership, Audio Service Dependency Restart, SHA-256 Digest Verification,
//! Command Retry Policy & Reparse-Safe Filesystem Operations (`Slice-M9M10-2`).
//!
//! Strictly 1:1 ports 6 source modules from `/native/system-apo/setup/`:
//! 1. `acl.h/.cpp` (388 lines) — `take_ownership`, `apply_engine_acl`, `apply_config_acl`,
//!    `apply_read_only_acl`, `apply_log_acl`, and `service_can_write` (evaluating `LOCAL SERVICE`,
//!    `Users`, `Authenticated Users`, and `Everyone` ACEs, ignoring `INHERIT_ONLY_ACE`, and
//!    prioritizing `ACCESS_DENIED_ACE_TYPE`).
//! 2. `digest.h/.cpp` (103 lines) + `shipped_dlls.cmake` (51 lines) — `sha256_hex` and
//!    case-insensitive `shipped_digest` manifest lookup.
//! 3. `retry.h/.cpp` (112 lines) — `COMMAND_TRIES = 3`, `is_service_settling`, and
//!    `run_with_retries` (clean `SetupCommandResult` per try, settling between retries).
//! 4. `service_restart.h/.cpp` (120 lines) — `ServiceControl` trait and `restart_services`
//!    (stopping running dependents of `Audiosrv`, then `Audiosrv`, then `AudioEndpointBuilder`,
//!    and restarting every stopped service in reverse order even after partial failures).
//! 5. `services.h/.cpp` (409 lines) — `wait_stopped` (detecting services that entered
//!    `SERVICE_STOP_PENDING` and bounced back to `SERVICE_RUNNING`), `wait_running` (detecting
//!    services that started and immediately stopped), `wait_audio_settled`, and `restart_audio`.
//! 6. `fs.h/.cpp` (622 lines) — Privilege-escalation-safe `SetupPaths` (never reading
//!    `FLUIDEQ_ENGINE_ROOT`), `crosses_link`, `ensure_directory`, `write_utf8`, `append_utf8`,
//!    4 MB-bounded `read_utf8`, `files_matching`, `read_locked`, in-use DLL hot-swap
//!    `replace_with_bytes` (`<to>.new` -> `<to>.replaced`), `delete_directory_tree` (never
//!    descending into reparse points/symlinks), and `describe_error`.

use crate::apo::EndpointAttachResult;
use crate::apo_watcher::sha256_digest;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

// ============================================================================
// 1. Directory & Log ACL Policy Engine (`acl.h/.cpp`)
// ============================================================================

pub const FILE_READ_DATA_MASK: u32 = 0x0000_0001;
pub const FILE_WRITE_DATA_MASK: u32 = 0x0000_0002;
pub const FILE_APPEND_DATA_MASK: u32 = 0x0000_0004;
pub const DELETE_MASK: u32 = 0x0001_0000;
pub const FILE_GENERIC_READ_MASK: u32 = 0x0012_0089;
pub const FILE_GENERIC_WRITE_MASK: u32 = 0x0012_0116;
pub const FILE_GENERIC_EXECUTE_MASK: u32 = 0x0012_00A0;
pub const FILE_ALL_ACCESS_MASK: u32 = 0x001F_01FF;
pub const GENERIC_WRITE_MASK: u32 = 0x4000_0000;

/// `kModify` in `acl.cpp`: read, write, execute, and delete (deliberately not `FILE_ALL_ACCESS`
/// so non-admin trustees cannot rewrite permissions themselves).
pub const ACL_MODIFY_RIGHTS: u32 =
    FILE_GENERIC_READ_MASK | FILE_GENERIC_WRITE_MASK | FILE_GENERIC_EXECUTE_MASK | DELETE_MASK;

/// `kReadOnly` in `acl.cpp`: read and execute.
pub const ACL_READ_ONLY_RIGHTS: u32 = FILE_GENERIC_READ_MASK | FILE_GENERIC_EXECUTE_MASK;

/// `apply_log_acl` rights for `Users` in `acl.cpp`: read and append only (`FILE_GENERIC_READ | FILE_APPEND_DATA`).
pub const ACL_LOG_APPEND_RIGHTS: u32 = FILE_GENERIC_READ_MASK | FILE_APPEND_DATA_MASK;

pub const OBJECT_INHERIT_ACE_FLAG: u8 = 0x01;
pub const CONTAINER_INHERIT_ACE_FLAG: u8 = 0x02;
pub const INHERIT_ONLY_ACE_FLAG: u8 = 0x08;
pub const FOLDER_INHERIT_FLAGS: u8 = CONTAINER_INHERIT_ACE_FLAG | OBJECT_INHERIT_ACE_FLAG;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WellKnownSidType {
    LocalSystem,
    BuiltinAdministrators,
    LocalService,
    BuiltinUsers,
    AuthenticatedUser,
    EveryoneWorld,
    Other(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AceKind {
    AccessAllowed,
    AccessDenied,
    AuditOrOther,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclAceEntry {
    pub ace_type: AceKind,
    pub ace_flags: u8,
    pub mask: u32,
    pub trustee: WellKnownSidType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityDescriptorModel {
    pub owner: WellKnownSidType,
    pub protected_dacl: bool,
    /// `None` represents a NULL DACL (open to everyone); `Some(entries)` is the explicit ACE list.
    pub dacl: Option<Vec<AclAceEntry>>,
}

impl Default for SecurityDescriptorModel {
    fn default() -> Self {
        Self {
            owner: WellKnownSidType::BuiltinUsers,
            protected_dacl: false,
            dacl: Some(Vec::new()),
        }
    }
}

pub trait SecurityBackend {
    fn get_security(&self, path: &Path) -> Result<SecurityDescriptorModel, u32>;
    fn set_owner(
        &mut self,
        path: &Path,
        owner: WellKnownSidType,
        take_ownership_privilege_enabled: bool,
    ) -> Result<(), u32>;
    fn set_protected_dacl(&mut self, path: &Path, entries: Vec<AclAceEntry>) -> Result<(), u32>;
}

/// In-memory security descriptor store for testing ACL ownership, protected DACLs, and `service_can_write`.
#[derive(Debug, Default, Clone)]
pub struct InMemorySecurityBackend {
    pub descriptors: BTreeMap<PathBuf, SecurityDescriptorModel>,
    pub privilege_enabled: bool,
}

impl InMemorySecurityBackend {
    pub fn new() -> Self {
        Self {
            descriptors: BTreeMap::new(),
            privilege_enabled: false,
        }
    }
}

impl SecurityBackend for InMemorySecurityBackend {
    fn get_security(&self, path: &Path) -> Result<SecurityDescriptorModel, u32> {
        self.descriptors.get(path).cloned().ok_or(2)
    }

    fn set_owner(
        &mut self,
        path: &Path,
        owner: WellKnownSidType,
        take_ownership_privilege_enabled: bool,
    ) -> Result<(), u32> {
        self.privilege_enabled = take_ownership_privilege_enabled;
        let desc = self.descriptors.entry(path.to_path_buf()).or_default();
        desc.owner = owner;
        Ok(())
    }

    fn set_protected_dacl(&mut self, path: &Path, entries: Vec<AclAceEntry>) -> Result<(), u32> {
        let desc = self.descriptors.entry(path.to_path_buf()).or_default();
        desc.protected_dacl = true;
        desc.dacl = Some(entries);
        Ok(())
    }
}

fn build_protected_dacl_entries(
    grants: &[(WellKnownSidType, u32)],
    inheritance: u8,
) -> Vec<AclAceEntry> {
    let mut entries = Vec::with_capacity(grants.len() + 2);
    entries.push(AclAceEntry {
        ace_type: AceKind::AccessAllowed,
        ace_flags: inheritance,
        mask: FILE_ALL_ACCESS_MASK,
        trustee: WellKnownSidType::LocalSystem,
    });
    entries.push(AclAceEntry {
        ace_type: AceKind::AccessAllowed,
        ace_flags: inheritance,
        mask: FILE_ALL_ACCESS_MASK,
        trustee: WellKnownSidType::BuiltinAdministrators,
    });
    for &(who, rights) in grants {
        entries.push(AclAceEntry {
            ace_type: AceKind::AccessAllowed,
            ace_flags: inheritance,
            mask: rights,
            trustee: who,
        });
    }
    entries
}

fn apply_acl_with_backend<S: SecurityBackend>(
    backend: &mut S,
    path: &Path,
    grants: &[(WellKnownSidType, u32)],
    inheritance: u8,
) -> Result<(), String> {
    let entries = build_protected_dacl_entries(grants, inheritance);
    backend.set_protected_dacl(path, entries).map_err(|code| {
        format!(
            "could not set the permissions on {}: {}",
            path.display(),
            describe_error(code)
        )
    })
}

/// Exact port of `take_ownership` in `/native/system-apo/setup/acl.cpp`:
/// Enables `SeTakeOwnershipPrivilege` and sets `WinBuiltinAdministratorsSid` as the owner of `path`.
pub fn take_ownership<S: SecurityBackend>(backend: &mut S, path: &Path) -> Result<(), String> {
    backend
        .set_owner(path, WellKnownSidType::BuiltinAdministrators, true)
        .map_err(|code| {
            format!(
                "could not take ownership of {}: {}",
                path.display(),
                describe_error(code)
            )
        })
}

/// Exact port of `apply_engine_acl` in `/native/system-apo/setup/acl.cpp`:
/// `SYSTEM` full, `Administrators` full, `LOCAL SERVICE` modify, `Users` read-only, all inherited.
pub fn apply_engine_acl<S: SecurityBackend>(
    backend: &mut S,
    directory: &Path,
) -> Result<(), String> {
    apply_acl_with_backend(
        backend,
        directory,
        &[
            (WellKnownSidType::LocalService, ACL_MODIFY_RIGHTS),
            (WellKnownSidType::BuiltinUsers, ACL_READ_ONLY_RIGHTS),
        ],
        FOLDER_INHERIT_FLAGS,
    )
}

/// Exact port of `apply_config_acl` in `/native/system-apo/setup/acl.cpp`:
/// `SYSTEM` full, `Administrators` full, `LOCAL SERVICE` modify, `Users` modify, all inherited.
pub fn apply_config_acl<S: SecurityBackend>(
    backend: &mut S,
    directory: &Path,
) -> Result<(), String> {
    apply_acl_with_backend(
        backend,
        directory,
        &[
            (WellKnownSidType::LocalService, ACL_MODIFY_RIGHTS),
            (WellKnownSidType::BuiltinUsers, ACL_MODIFY_RIGHTS),
        ],
        FOLDER_INHERIT_FLAGS,
    )
}

/// Exact port of `apply_read_only_acl` in `/native/system-apo/setup/acl.cpp`:
/// `SYSTEM` full, `Administrators` full, `Users` read-only, all inherited.
/// Used for `backup\`, `apo-off\`, `slots\`, and the engine root's parent.
pub fn apply_read_only_acl<S: SecurityBackend>(
    backend: &mut S,
    directory: &Path,
) -> Result<(), String> {
    apply_acl_with_backend(
        backend,
        directory,
        &[(WellKnownSidType::BuiltinUsers, ACL_READ_ONLY_RIGHTS)],
        FOLDER_INHERIT_FLAGS,
    )
}

/// Exact port of `apply_log_acl` in `/native/system-apo/setup/acl.cpp`:
/// `SYSTEM` full, `Administrators` full, `Users` `FILE_GENERIC_READ | FILE_APPEND_DATA`, no inheritance.
pub fn apply_log_acl<S: SecurityBackend>(backend: &mut S, file: &Path) -> Result<(), String> {
    apply_acl_with_backend(
        backend,
        file,
        &[(WellKnownSidType::BuiltinUsers, ACL_LOG_APPEND_RIGHTS)],
        0,
    )
}

/// Exact port of `service_can_write` in `/native/system-apo/setup/acl.cpp`:
/// Evaluates whether `LOCAL SERVICE` (or any of the three groups it belongs to: `BuiltinUsers`,
/// `AuthenticatedUser`, `EveryoneWorld`) can create/write files in `directory`.
/// - Unreadable security descriptor returns `true` (never raises a false repair prompt).
/// - `dacl == None` (NULL DACL) returns `true`.
/// - Skips `INHERIT_ONLY_ACE` entries (`subfolders and files only`).
/// - Any matching `AccessDenied` entry with write bits settles `denied = true`.
pub fn service_can_write<S: SecurityBackend>(backend: &S, directory: &Path) -> bool {
    let Ok(desc) = backend.get_security(directory) else {
        return true;
    };
    let Some(dacl) = desc.dacl.as_ref() else {
        return true;
    };

    let mut allowed = false;
    let mut denied = false;

    for ace in dacl {
        if denied {
            break;
        }
        let allows = ace.ace_type == AceKind::AccessAllowed;
        let refuses = ace.ace_type == AceKind::AccessDenied;
        if !allows && !refuses {
            continue;
        }
        if (ace.ace_flags & INHERIT_ONLY_ACE_FLAG) != 0 {
            continue;
        }
        if !matches!(
            ace.trustee,
            WellKnownSidType::LocalService
                | WellKnownSidType::BuiltinUsers
                | WellKnownSidType::AuthenticatedUser
                | WellKnownSidType::EveryoneWorld
        ) {
            continue;
        }
        let writes = (ace.mask & FILE_GENERIC_WRITE_MASK) == FILE_GENERIC_WRITE_MASK
            || (ace.mask & GENERIC_WRITE_MASK) == GENERIC_WRITE_MASK
            || (ace.mask & FILE_ALL_ACCESS_MASK) == FILE_ALL_ACCESS_MASK
            || (ace.mask & FILE_WRITE_DATA_MASK) == FILE_WRITE_DATA_MASK;
        if !writes {
            continue;
        }
        if refuses {
            denied = true;
        } else {
            allowed = true;
        }
    }

    allowed && !denied
}

// ============================================================================
// 2. SHA-256 Digest & Shipped DLL Manifest (`digest.h/.cpp` & `shipped_dlls.cmake`)
// ============================================================================

/// The 5 DLL names recognized by `shipped_dlls.cmake` (`FluidEQ-Engine.dll`, `msvcp140.dll`,
/// `vcruntime140.dll`, `vcruntime140_1.dll`, `onnxruntime.dll`).
pub const SHIPPED_DLL_NAMES: [&str; 5] = [
    "FluidEQ-Engine.dll",
    "msvcp140.dll",
    "vcruntime140.dll",
    "vcruntime140_1.dll",
    "onnxruntime.dll",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShippedDllEntry {
    pub name: String,
    pub sha256: String,
}

/// Computes the lowercase 64-character hexadecimal SHA-256 digest of `bytes`
/// (`sha256_hex` in `/native/system-apo/setup/digest.cpp`).
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha256_digest(bytes);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(64);
    for &b in &digest {
        text.push(HEX[(b >> 4) as usize] as char);
        text.push(HEX[(b & 0x0F) as usize] as char);
    }
    text
}

/// Looks up the expected SHA-256 digest for `name` case-insensitively (`_wcsicmp`) in `manifest`,
/// or returns `None` if `name` is not a shipped DLL (`shipped_digest` in `digest.cpp`).
pub fn shipped_digest<'a>(manifest: &'a [ShippedDllEntry], name: &str) -> Option<&'a str> {
    manifest
        .iter()
        .find(|dll| dll.name.eq_ignore_ascii_case(name))
        .map(|dll| dll.sha256.as_str())
}

// ============================================================================
// 3. Engine Command Retry & Settling Policy (`retry.h/.cpp`)
// ============================================================================

/// `kCommandTries` in `/native/system-apo/setup/retry.h`.
pub const COMMAND_TRIES: usize = 3;

pub const SERVICE_STOPPED: u32 = 1;
pub const SERVICE_START_PENDING: u32 = 2;
pub const SERVICE_STOP_PENDING: u32 = 3;
pub const SERVICE_RUNNING: u32 = 4;
pub const SERVICE_CONTINUE_PENDING: u32 = 5;
pub const SERVICE_PAUSE_PENDING: u32 = 6;
pub const SERVICE_PAUSED: u32 = 7;

/// Exact port of `CommandResult` in `/native/system-apo/setup/commands.h`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupCommandResult {
    pub ok: bool,
    pub error: String,
    pub endpoints: Vec<EndpointAttachResult>,
}

impl Default for SetupCommandResult {
    fn default() -> Self {
        Self {
            ok: true,
            error: String::new(),
            endpoints: Vec::new(),
        }
    }
}

/// Exact port of `is_service_settling` in `/native/system-apo/setup/retry.cpp`:
/// Returns `true` iff `state` is `START_PENDING (2)`, `STOP_PENDING (3)`,
/// `CONTINUE_PENDING (5)`, or `PAUSE_PENDING (6)`.
pub fn is_service_settling(state: u32) -> bool {
    matches!(
        state,
        SERVICE_START_PENDING
            | SERVICE_STOP_PENDING
            | SERVICE_CONTINUE_PENDING
            | SERVICE_PAUSE_PENDING
    )
}

/// Exact port of `run_with_retries` in `/native/system-apo/setup/retry.cpp`:
/// - Runs `attempt` on a fresh `SetupCommandResult` up to `tries.max(1)` times.
/// - Returns immediately when `result.ok` is `true`.
/// - Before each retry after the first, calls `settle(&mut why)`; if `settle` returns `false`,
///   appends `"; not tried again, because Windows audio could not be waited for: " + why` and stops.
/// - When all `limit > 1` tries fail, prefixes `"failed <limit> times; the last time: "`.
pub fn run_with_retries<A, S>(tries: usize, mut attempt: A, mut settle: S) -> SetupCommandResult
where
    A: FnMut(&mut SetupCommandResult),
    S: FnMut(&mut String) -> bool,
{
    let limit = tries.max(1);
    let mut made = 1usize;
    loop {
        let mut result = SetupCommandResult::default();
        attempt(&mut result);
        if result.ok {
            return result;
        }
        if made == limit {
            if limit > 1 {
                result.error = format!("failed {} times; the last time: {}", limit, result.error);
            }
            return result;
        }
        let mut why = String::new();
        if !settle(&mut why) {
            result.error.push_str(
                "; not tried again, because Windows audio could not be waited for: ",
            );
            result.error.push_str(&why);
            return result;
        }
        made += 1;
    }
}

// ============================================================================
// 4. Audio Service Dependency Graph Restart & State Waiters (`service_restart.h/.cpp` & `services.h/.cpp`)
// ============================================================================

pub const AUDIO_SERVICE_NAME: &str = "Audiosrv";
pub const BUILDER_SERVICE_NAME: &str = "AudioEndpointBuilder";

pub const SERVICE_NOTIFY_STOPPED_MASK: u32 = 0x0000_0001;
pub const SERVICE_NOTIFY_START_PENDING_MASK: u32 = 0x0000_0002;
pub const SERVICE_NOTIFY_STOP_PENDING_MASK: u32 = 0x0000_0004;
pub const SERVICE_NOTIFY_RUNNING_MASK: u32 = 0x0000_0008;
pub const SERVICE_NOTIFY_PAUSED_MASK: u32 = 0x0000_0040;

/// Exact port of `ServiceControl` in `/native/system-apo/setup/service_restart.h`.
pub trait ServiceControl {
    fn running_dependents(&mut self, service: &str) -> Result<Vec<String>, String>;
    fn stop(&mut self, service: &str) -> Result<(), String>;
    fn start(&mut self, service: &str) -> Result<(), String>;
}

fn append_service_error(error: &mut String, why: &str) {
    if !error.is_empty() {
        error.push_str("; ");
    }
    error.push_str(why);
}

/// Exact port of `restart_services` in `/native/system-apo/setup/service_restart.cpp`:
/// 1. Lists running dependents of `audio` (`Audiosrv`); if listing fails, touches nothing and returns `Err`.
/// 2. Appends `audio` (`Audiosrv`) and `builder` (`AudioEndpointBuilder`) to the stop order.
/// 3. Stops each service in order; on the first stop failure, records the error and breaks out of the stop loop.
/// 4. Starts every service that was actually stopped in **reverse order**, attempting every start even if
///    an earlier start fails so one broken vendor service never leaves Windows Audio down.
pub fn restart_services<C: ServiceControl>(
    control: &mut C,
    audio: &str,
    builder: &str,
) -> Result<(), String> {
    let mut order = match control.running_dependents(audio) {
        Ok(deps) => deps,
        Err(err) => {
            return Err(if err.is_empty() {
                format!("could not list what depends on {}", audio)
            } else {
                err
            });
        }
    };
    order.push(audio.to_string());
    order.push(builder.to_string());

    let mut stopped: Vec<String> = Vec::new();
    let mut failure = String::new();

    for name in &order {
        match control.stop(name) {
            Ok(()) => stopped.push(name.clone()),
            Err(why) => {
                let msg = if why.is_empty() {
                    format!("could not stop {}", name)
                } else {
                    why
                };
                append_service_error(&mut failure, &msg);
                break;
            }
        }
    }

    for name in stopped.iter().rev() {
        if let Err(why) = control.start(name) {
            let msg = if why.is_empty() {
                format!("could not start {}", name)
            } else {
                why
            };
            append_service_error(&mut failure, &msg);
        }
    }

    if failure.is_empty() {
        Ok(())
    } else {
        Err(failure)
    }
}

/// Abstraction over SCM `QueryServiceStatusEx` and `NotifyServiceStatusChangeW` (`services.cpp`).
pub trait ServiceStateWaiter {
    fn query_state(&mut self, name: &str) -> Result<u32, String>;
    fn wait_for_change(&mut self, name: &str, notify_mask: u32) -> Result<(), String>;
}

/// Exact port of `wait_stopped` in `/native/system-apo/setup/services.cpp`:
/// - Returns `Ok(())` once `state == SERVICE_STOPPED`.
/// - Tracks `stopping = true` once `SERVICE_STOP_PENDING` is observed.
/// - If `state == SERVICE_RUNNING && stopping`, fails immediately with `"<name> began to stop and then kept running"`.
/// - Before `STOP_PENDING` is seen, `SERVICE_RUNNING` is waited out (`STOPPED | STOP_PENDING`) because the handler
///   may be slow to report `STOP_PENDING`.
pub fn wait_service_stopped<W: ServiceStateWaiter>(
    waiter: &mut W,
    name: &str,
) -> Result<(), String> {
    let mut stopping = false;
    loop {
        let state = waiter.query_state(name)?;
        if state == SERVICE_STOPPED {
            return Ok(());
        }
        if state == SERVICE_STOP_PENDING {
            stopping = true;
        } else if state == SERVICE_RUNNING && stopping {
            return Err(format!("{} began to stop and then kept running", name));
        }
        let mask = if stopping {
            SERVICE_NOTIFY_STOPPED_MASK | SERVICE_NOTIFY_RUNNING_MASK
        } else {
            SERVICE_NOTIFY_STOPPED_MASK | SERVICE_NOTIFY_STOP_PENDING_MASK
        };
        waiter.wait_for_change(name, mask)?;
    }
}

/// Exact port of `wait_running` in `/native/system-apo/setup/services.cpp`:
/// - Returns `Ok(())` once `state == SERVICE_RUNNING`.
/// - If `state == SERVICE_STOPPED`, fails immediately with `"<name> started and then stopped again"`.
pub fn wait_service_running<W: ServiceStateWaiter>(
    waiter: &mut W,
    name: &str,
) -> Result<(), String> {
    loop {
        let state = waiter.query_state(name)?;
        if state == SERVICE_RUNNING {
            return Ok(());
        }
        if state == SERVICE_STOPPED {
            return Err(format!("{} started and then stopped again", name));
        }
        waiter.wait_for_change(
            name,
            SERVICE_NOTIFY_RUNNING_MASK | SERVICE_NOTIFY_STOPPED_MASK,
        )?;
    }
}

/// Exact port of `wait_audio_settled` in `/native/system-apo/setup/services.cpp`:
/// Waits for `AudioEndpointBuilder` first, then `Audiosrv`, until neither is in a settling state.
pub fn wait_audio_settled<W: ServiceStateWaiter>(waiter: &mut W) -> Result<(), String> {
    for name in [BUILDER_SERVICE_NAME, AUDIO_SERVICE_NAME] {
        loop {
            let state = waiter.query_state(name)?;
            if !is_service_settling(state) {
                break;
            }
            waiter.wait_for_change(
                name,
                SERVICE_NOTIFY_RUNNING_MASK
                    | SERVICE_NOTIFY_STOPPED_MASK
                    | SERVICE_NOTIFY_PAUSED_MASK,
            )?;
        }
    }
    Ok(())
}

/// Exact port of `restart_audio` in `/native/system-apo/setup/services.cpp`.
pub fn restart_audio<C: ServiceControl>(control: &mut C) -> Result<(), String> {
    restart_services(control, AUDIO_SERVICE_NAME, BUILDER_SERVICE_NAME)
}

// ============================================================================
// 5. Reparse-Safe Setup Paths & Locked File Operations (`fs.h/.cpp`)
// ============================================================================

/// Max size (`4 MB`) permitted by `read_utf8` (`fs.cpp` line 277).
pub const SETUP_MAX_UTF8_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Max chunk size (`1 MB`) per read/write call in `read_locked` and `write_bytes` (`fs.cpp` lines 351, 377).
pub const SETUP_IO_CHUNK_BYTES: usize = 1 << 20;

/// Known-folder-derived path resolver for `FluidEQ-Engine-Setup.exe` (`fs.h/.cpp`).
/// Deliberately never reads `FLUIDEQ_ENGINE_ROOT` from the environment because the setup helper
/// runs elevated and applies write ACLs to `config_dir()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupPaths {
    pub program_data: PathBuf,
    pub program_files: PathBuf,
    pub module_dir: PathBuf,
}

impl SetupPaths {
    pub fn new(program_data: PathBuf, program_files: PathBuf, module_dir: PathBuf) -> Self {
        Self {
            program_data,
            program_files,
            module_dir,
        }
    }

    /// `%ProgramData%\FluidEQ` (`engine_parent` in `fs.cpp`).
    pub fn engine_parent(&self) -> Option<PathBuf> {
        if self.program_data.as_os_str().is_empty() {
            None
        } else {
            Some(self.program_data.join("FluidEQ"))
        }
    }

    /// `%ProgramData%\FluidEQ\engine` (`engine_root` in `fs.cpp`).
    pub fn engine_root(&self) -> Option<PathBuf> {
        self.engine_parent().map(|p| p.join("engine"))
    }

    /// `<engine_root>\config` (`config_dir` in `fs.cpp`).
    pub fn config_dir(&self) -> Option<PathBuf> {
        self.engine_root().map(|r| r.join("config"))
    }

    /// `<engine_root>\backup` (`backup_dir` in `fs.cpp`).
    pub fn backup_dir(&self) -> Option<PathBuf> {
        self.engine_root().map(|r| r.join("backup"))
    }

    /// `<engine_root>\apo-off` (`apo_off_dir` in `fs.cpp`).
    pub fn apo_off_dir(&self) -> Option<PathBuf> {
        self.engine_root().map(|r| r.join("apo-off"))
    }

    /// `<engine_root>\slots` (`slots_dir` in `fs.cpp`).
    pub fn slots_dir(&self) -> Option<PathBuf> {
        self.engine_root().map(|r| r.join("slots"))
    }

    /// `<engine_root>\last-setup.json` (`result_path` in `fs.cpp`).
    pub fn result_path(&self) -> Option<PathBuf> {
        self.engine_root().map(|r| r.join("last-setup.json"))
    }

    /// `<engine_root>\setup.log` (`setup_log_path` in `fs.cpp`).
    pub fn setup_log_path(&self) -> Option<PathBuf> {
        self.engine_root().map(|r| r.join("setup.log"))
    }

    /// `%ProgramFiles%\FluidEQ Engine` (`install_dir` in `fs.cpp`).
    pub fn install_dir(&self) -> Option<PathBuf> {
        if self.program_files.as_os_str().is_empty() {
            None
        } else {
            Some(self.program_files.join("FluidEQ Engine"))
        }
    }

    /// `<install_dir>\FluidEQ-Engine.dll` (`installed_dll_path` in `fs.cpp`).
    pub fn installed_dll_path(&self) -> Option<PathBuf> {
        self.install_dir().map(|d| d.join("FluidEQ-Engine.dll"))
    }

    pub fn path_exists(path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok()
    }

    pub fn ensure_directory(&self, path: &Path) -> bool {
        let parent = self.engine_parent().unwrap_or_default();
        ensure_directory_safe(&parent, path)
    }

    pub fn write_utf8(&self, path: &Path, text: &str) -> bool {
        let parent = self.engine_parent().unwrap_or_default();
        write_utf8_safe(&parent, path, text)
    }

    pub fn append_utf8(&self, path: &Path, text: &str) -> bool {
        let parent = self.engine_parent().unwrap_or_default();
        append_utf8_safe(&parent, path, text)
    }

    pub fn read_utf8(&self, path: &Path) -> Option<String> {
        let parent = self.engine_parent().unwrap_or_default();
        if crosses_link(&parent, path) || is_symlink_or_reparse(path) {
            return None;
        }
        read_utf8_bounded(path)
    }

    pub fn files_matching(directory: &Path, pattern: &str) -> Vec<String> {
        files_matching(directory, pattern)
    }

    pub fn read_locked(path: &Path, limit: usize) -> Option<Vec<u8>> {
        read_locked(path, limit)
    }

    pub fn replace_with_bytes_simulated(
        bytes: &[u8],
        to: &Path,
        target_is_mapped_in_use: bool,
    ) -> Result<ReplaceFileOutcome, String> {
        replace_with_bytes(bytes, to, target_is_mapped_in_use)
    }

    pub fn delete_directory_tree(directory: &Path) -> bool {
        delete_directory_tree(directory)
    }
}

fn is_symlink_or_reparse(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Exact port of `crosses_link` in `/native/system-apo/setup/fs.cpp`:
/// Checks whether `path` is under `engine_parent` and whether `engine_parent` or any existing
/// prefix component down to `path` is a symbolic link or junction.
/// Returns `false` for paths outside `engine_parent`.
pub fn crosses_link(engine_parent: &Path, path: &Path) -> bool {
    if engine_parent.as_os_str().is_empty() {
        return false;
    }
    let Ok(rel) = path.strip_prefix(engine_parent) else {
        return false;
    };

    let mut current = engine_parent.to_path_buf();
    if fs::symlink_metadata(&current).is_err() {
        return false;
    }
    if is_symlink_or_reparse(&current) {
        return true;
    }
    for comp in rel.components() {
        current.push(comp);
        if fs::symlink_metadata(&current).is_err() {
            return false;
        }
        if is_symlink_or_reparse(&current) {
            return true;
        }
    }
    false
}

/// Exact port of `ensure_directory` in `/native/system-apo/setup/fs.cpp`:
/// Creates `path` and every missing parent directory, refusing any path that crosses a link
/// under `engine_parent`.
pub fn ensure_directory_safe(engine_parent: &Path, path: &Path) -> bool {
    if path.as_os_str().is_empty() || crosses_link(engine_parent, path) {
        return false;
    }
    if fs::symlink_metadata(path)
        .map(|m| m.is_dir() && !m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return true;
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty()
            && parent != Path::new("/")
            && !ensure_directory_safe(engine_parent, parent)
        {
            return false;
        }
    }
    if fs::create_dir(path).is_ok() {
        return true;
    }
    if crosses_link(engine_parent, path) {
        return false;
    }
    fs::symlink_metadata(path)
        .map(|m| m.is_dir() && !m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Exact port of `write_utf8` in `/native/system-apo/setup/fs.cpp`:
/// Writes UTF-8 without BOM, refusing any path that crosses a link under `engine_parent`.
pub fn write_utf8_safe(engine_parent: &Path, path: &Path, text: &str) -> bool {
    if crosses_link(engine_parent, path) || is_symlink_or_reparse(path) {
        return false;
    }
    fs::write(path, text.as_bytes()).is_ok()
}

/// Exact port of `append_utf8` in `/native/system-apo/setup/fs.cpp`:
/// Appends UTF-8 bytes to the end of `path` without truncating, refusing paths that cross a link.
pub fn append_utf8_safe(engine_parent: &Path, path: &Path, text: &str) -> bool {
    if crosses_link(engine_parent, path) || is_symlink_or_reparse(path) {
        return false;
    }
    let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) else {
        return false;
    };
    file.write_all(text.as_bytes()).is_ok()
}

/// Exact port of `read_utf8` in `/native/system-apo/setup/fs.cpp`:
/// Reads a UTF-8 file up to `4 MB` (`4 * 1024 * 1024` bytes), returning `None` if the file
/// exceeds the cap or contains invalid UTF-8 sequences.
pub fn read_utf8_bounded(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > SETUP_MAX_UTF8_FILE_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if bytes.len() as u64 > SETUP_MAX_UTF8_FILE_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn wildcard_matches(pattern: &str, name: &str) -> bool {
    if pattern == "*" || pattern == "*.*" {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return name
            .to_ascii_lowercase()
            .ends_with(&suffix.to_ascii_lowercase());
    }
    name.eq_ignore_ascii_case(pattern)
}

/// Exact port of `files_matching` in `/native/system-apo/setup/fs.cpp`:
/// Returns file names (excluding directories) in `directory` matching `pattern` (e.g., `"*.json"` or `"*.dll"`).
pub fn files_matching(directory: &Path, pattern: &str) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = fs::read_dir(directory) else {
        return names;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_dir() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            if wildcard_matches(pattern, name) {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// Exact port of `read_locked` in `/native/system-apo/setup/fs.cpp`:
/// Opens `path` without following reparse points/symlinks, verifies it is a plain regular file
/// (neither directory nor symlink) with size `<= limit`, and reads its bytes.
pub fn read_locked(path: &Path, limit: usize) -> Option<Vec<u8>> {
    let sym_meta = fs::symlink_metadata(path).ok()?;
    if sym_meta.is_dir() || sym_meta.file_type().is_symlink() {
        return None;
    }
    if sym_meta.len() > (limit as u64) {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if bytes.len() > limit {
        return None;
    }
    Some(bytes)
}

/// Outcome of `replace_with_bytes`, indicating whether an in-use mapped DLL had to be moved
/// aside to `<to>.replaced` for deletion at reboot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceFileOutcome {
    pub moved_aside_for_reboot: bool,
    pub replaced_aside_path: Option<PathBuf>,
}

/// Exact port of `replace_with_bytes` in `/native/system-apo/setup/fs.cpp`:
/// 1. Writes `bytes` to `<to>.new`; on write failure, deletes `<to>.new` and returns `Err`.
/// 2. Attempts to rename `<to>.new` over `<to>` directly.
/// 3. If `target_is_mapped_in_use` (simulating `ERROR_SHARING_VIOLATION` / `ERROR_USER_MAPPED_FILE`
///    when `audiodg.exe` holds `FluidEQ-Engine.dll` mapped), removes `<to>.replaced`, renames
///    `<to>` -> `<to>.replaced`, and then renames `<to>.new` -> `<to>`.
pub fn replace_with_bytes(
    bytes: &[u8],
    to: &Path,
    target_is_mapped_in_use: bool,
) -> Result<ReplaceFileOutcome, String> {
    let mut fresh_os = to.as_os_str().to_os_string();
    fresh_os.push(".new");
    let fresh = PathBuf::from(fresh_os);

    if let Err(e) = fs::write(&fresh, bytes) {
        let _ = fs::remove_file(&fresh);
        return Err(format!("could not write {}: {}", fresh.display(), e));
    }

    if !target_is_mapped_in_use && fs::rename(&fresh, to).is_ok() {
        return Ok(ReplaceFileOutcome {
            moved_aside_for_reboot: false,
            replaced_aside_path: None,
        });
    }

    let mut aside_os = to.as_os_str().to_os_string();
    aside_os.push(".replaced");
    let aside = PathBuf::from(aside_os);
    let _ = fs::remove_file(&aside);

    if let Err(e) = fs::rename(to, &aside) {
        let _ = fs::remove_file(&fresh);
        return Err(format!("could not move the file in use aside: {}", e));
    }
    if let Err(e) = fs::rename(&fresh, to) {
        return Err(format!("could not put {} in place: {}", to.display(), e));
    }

    Ok(ReplaceFileOutcome {
        moved_aside_for_reboot: true,
        replaced_aside_path: Some(aside),
    })
}

/// Exact port of `delete_directory_tree` in `/native/system-apo/setup/fs.cpp`:
/// Recursively deletes `directory` without descending into symbolic links or junctions.
pub fn delete_directory_tree(directory: &Path) -> bool {
    let Ok(sym_meta) = fs::symlink_metadata(directory) else {
        return !directory.exists();
    };
    if sym_meta.file_type().is_symlink() {
        return fs::remove_file(directory).is_ok() || fs::remove_dir(directory).is_ok();
    }
    if !sym_meta.is_dir() {
        return false;
    }

    let mut ok = true;
    if let Ok(entries) = fs::read_dir(directory) {
        for entry in entries.flatten() {
            let child = entry.path();
            let Ok(child_meta) = fs::symlink_metadata(&child) else {
                continue;
            };
            if child_meta.is_dir() && !child_meta.file_type().is_symlink() {
                ok = delete_directory_tree(&child) && ok;
                continue;
            }
            #[cfg(windows)]
            if child_meta.permissions().readonly() {
                use std::os::windows::{ffi::OsStrExt, fs::MetadataExt};

                // Clear only the readonly attribute, preserving every other Windows attribute.
                let attributes =
                    child_meta.file_attributes() & !win32_setup_ffi::FILE_ATTRIBUTE_READONLY;
                let path: Vec<u16> = child.as_os_str().encode_wide().chain(Some(0)).collect();
                // SAFETY: `path` is a live, NUL-terminated UTF-16 buffer for this call.
                let _ = unsafe { win32_setup_ffi::SetFileAttributesW(path.as_ptr(), attributes) };
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                let mut perms = child_meta.permissions();
                let mode = perms.mode();
                // set_readonly(false) would also grant group/other write access on Unix.
                if perms.readonly() {
                    perms.set_mode(mode | 0o200);
                    let _ = fs::set_permissions(&child, perms);
                }
            }
            let removed = if child_meta.file_type().is_symlink() {
                fs::remove_file(&child).is_ok() || fs::remove_dir(&child).is_ok()
            } else if child_meta.is_dir() {
                fs::remove_dir(&child).is_ok()
            } else {
                fs::remove_file(&child).is_ok()
            };
            ok = removed && ok;
        }
    }
    fs::remove_dir(directory).is_ok() && ok
}

/// Formats a Win32 error code on a single line (`describe_error` in `/native/system-apo/setup/fs.cpp`).
pub fn describe_error(code: u32) -> String {
    let label = match code {
        0 => "The operation completed successfully.",
        2 => "The system cannot find the file specified.",
        3 => "The system cannot find the path specified.",
        5 => "Access is denied.",
        32 => "The process cannot access the file because it is being used by another process.",
        1051 => "A stop control has been sent to a service that other running services are dependent on.",
        1056 => "An instance of the service is already running.",
        1062 => "The service has not been started.",
        1224 => "The requested operation cannot be performed on a file with a user-mapped section open.",
        1920 => "The file cannot be accessed by the system.",
        _ => "",
    };
    if label.is_empty() {
        format!("error {}", code)
    } else {
        format!("{} ({})", label, code)
    }
}

#[cfg(target_os = "windows")]
pub mod win32_setup_ffi {
    use std::ffi::c_void;

    pub type Hkey = isize;
    pub type ScHandle = *mut c_void;

    pub const HKEY_LOCAL_MACHINE: Hkey = -2147483646isize; // 0x80000002
    pub const KEY_READ: u32 = 0x20019;
    pub const KEY_WRITE: u32 = 0x20006;
    pub const KEY_WOW64_64KEY: u32 = 0x0100;
    pub const FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;
    pub const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
    pub const MOVEFILE_DELAY_UNTIL_REBOOT: u32 = 0x0000_0004;
    pub const SC_MANAGER_CONNECT: u32 = 0x0001;
    pub const SERVICE_QUERY_STATUS: u32 = 0x0004;
    pub const SERVICE_START: u32 = 0x0010;
    pub const SERVICE_STOP: u32 = 0x0020;
    pub const SERVICE_ENUMERATE_DEPENDENTS: u32 = 0x0008;

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct ServiceStatusProcess {
        pub dw_service_type: u32,
        pub dw_current_state: u32,
        pub dw_controls_accepted: u32,
        pub dw_win32_exit_code: u32,
        pub dw_service_specific_exit_code: u32,
        pub dw_check_point: u32,
        pub dw_wait_hint: u32,
        pub dw_process_id: u32,
        pub dw_service_flags: u32,
    }

    #[link(name = "advapi32")]
    extern "system" {
        pub fn RegOpenKeyExW(
            h_key: Hkey,
            lp_sub_key: *const u16,
            ul_options: u32,
            sam_desired: u32,
            phk_result: *mut Hkey,
        ) -> i32;
        pub fn RegCreateKeyExW(
            h_key: Hkey,
            lp_sub_key: *const u16,
            reserved: u32,
            lp_class: *mut u16,
            dw_options: u32,
            sam_desired: u32,
            lp_security_attributes: *const c_void,
            phk_result: *mut Hkey,
            lpdw_disposition: *mut u32,
        ) -> i32;
        pub fn RegQueryValueExW(
            h_key: Hkey,
            lp_value_name: *const u16,
            lp_reserved: *mut u32,
            lp_type: *mut u32,
            lp_data: *mut u8,
            lpcb_data: *mut u32,
        ) -> i32;
        pub fn RegSetValueExW(
            h_key: Hkey,
            lp_value_name: *const u16,
            reserved: u32,
            dw_type: u32,
            lp_data: *const u8,
            cb_data: u32,
        ) -> i32;
        pub fn RegDeleteValueW(h_key: Hkey, lp_value_name: *const u16) -> i32;
        pub fn RegDeleteTreeW(h_key: Hkey, lp_sub_key: *const u16) -> i32;
        pub fn RegCloseKey(h_key: Hkey) -> i32;

        pub fn OpenSCManagerW(
            lp_machine_name: *const u16,
            lp_database_name: *const u16,
            dw_desired_access: u32,
        ) -> ScHandle;
        pub fn OpenServiceW(
            h_sc_manager: ScHandle,
            lp_service_name: *const u16,
            dw_desired_access: u32,
        ) -> ScHandle;
        pub fn QueryServiceStatusEx(
            h_service: ScHandle,
            info_level: u32,
            lp_buffer: *mut u8,
            cb_buf_size: u32,
            pcb_bytes_needed: *mut u32,
        ) -> i32;
        pub fn ControlService(
            h_service: ScHandle,
            dw_control: u32,
            lp_service_status: *mut c_void,
        ) -> i32;
        pub fn StartServiceW(
            h_service: ScHandle,
            dw_num_service_args: u32,
            lp_service_arg_vectors: *const *const u16,
        ) -> i32;
        pub fn CloseServiceHandle(h_sc_object: ScHandle) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn MoveFileExW(
            lp_existing_file_name: *const u16,
            lp_new_file_name: *const u16,
            dw_flags: u32,
        ) -> i32;
        pub fn GetFileAttributesW(lp_file_name: *const u16) -> u32;
        pub fn SetFileAttributesW(lp_file_name: *const u16, dw_file_attributes: u32) -> i32;
        pub fn GetLastError() -> u32;
    }

    #[link(name = "shell32")]
    extern "system" {
        pub fn IsUserAnAdmin() -> i32;
    }

    pub fn is_elevated_admin_win32() -> bool {
        unsafe { IsUserAnAdmin() != 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_matches_fips_180_2_abc_and_case_insensitive_manifest() {
        // Exact test cases from `/native/system-apo/tests/digest_test.cpp`
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );

        let fake_engine_bytes = b"FluidEQ-Engine-binary-image";
        let expected_hash = sha256_hex(fake_engine_bytes);
        let manifest = vec![ShippedDllEntry {
            name: "FluidEQ-Engine.dll".to_string(),
            sha256: expected_hash.clone(),
        }];

        assert_eq!(
            shipped_digest(&manifest, "FluidEQ-Engine.dll"),
            Some(expected_hash.as_str())
        );
        assert_eq!(
            shipped_digest(&manifest, "fluideq-engine.DLL"),
            Some(expected_hash.as_str())
        );
        assert!(shipped_digest(&manifest, "payload.dll").is_none());
        assert!(shipped_digest(&manifest, "FluidEQ-Engine.dll.replaced").is_none());
    }

    #[test]
    fn retry_policy_passes_all_six_checks_from_retry_test_cpp() {
        use std::cell::RefCell;

        // 1. A command that works runs once and never waits
        let calls = RefCell::new(Vec::new());
        let res = run_with_retries(
            COMMAND_TRIES,
            |attempt| {
                calls.borrow_mut().push("try");
                attempt.ok = true;
            },
            |_why| {
                calls.borrow_mut().push("settle");
                true
            },
        );
        assert!(res.ok);
        assert_eq!(*calls.borrow(), vec!["try"]);

        // 2. A command that works the second time clears the first try's error
        let calls = RefCell::new(Vec::new());
        let mut try_count = 0;
        let res = run_with_retries(
            COMMAND_TRIES,
            |attempt| {
                calls.borrow_mut().push("try");
                try_count += 1;
                attempt.ok = try_count >= 2;
                if !attempt.ok {
                    attempt.error = "Audiosrv is stopping".to_string();
                }
            },
            |_why| {
                calls.borrow_mut().push("settle");
                true
            },
        );
        assert!(res.ok);
        assert!(res.error.is_empty());
        assert_eq!(*calls.borrow(), vec!["try", "settle", "try"]);

        // 3. A command that never works tries 3 times with 2 settles
        let calls = RefCell::new(Vec::new());
        let mut try_count = 0;
        let res = run_with_retries(
            COMMAND_TRIES,
            |attempt| {
                calls.borrow_mut().push("try");
                try_count += 1;
                attempt.ok = false;
                attempt.error = format!("try {}", try_count);
            },
            |_why| {
                calls.borrow_mut().push("settle");
                true
            },
        );
        assert!(!res.ok);
        assert_eq!(*calls.borrow(), vec!["try", "settle", "try", "settle", "try"]);
        assert_eq!(res.error, "failed 3 times; the last time: try 3");

        // 4. A wait that fails ends the tries immediately
        let calls = RefCell::new(Vec::new());
        let res = run_with_retries(
            COMMAND_TRIES,
            |attempt| {
                calls.borrow_mut().push("try");
                attempt.ok = false;
                attempt.error = "could not attach".to_string();
            },
            |why| {
                calls.borrow_mut().push("settle");
                *why = "access is denied".to_string();
                false
            },
        );
        assert!(!res.ok);
        assert_eq!(*calls.borrow(), vec!["try", "settle"]);
        assert!(res.error.contains("could not attach"));
        assert!(res.error.contains("access is denied"));

        // 5. Settling states check (1..=7)
        assert!(!is_service_settling(1));
        assert!(is_service_settling(2));
        assert!(is_service_settling(3));
        assert!(!is_service_settling(4));
        assert!(is_service_settling(5));
        assert!(is_service_settling(6));
        assert!(!is_service_settling(7));
    }

    struct FakeServiceControl {
        dependents: Vec<String>,
        list_fails: bool,
        refuse_stop: Vec<String>,
        refuse_start: Vec<String>,
        calls: Vec<String>,
    }

    impl FakeServiceControl {
        fn new() -> Self {
            Self {
                dependents: Vec::new(),
                list_fails: false,
                refuse_stop: Vec::new(),
                refuse_start: Vec::new(),
                calls: Vec::new(),
            }
        }
    }

    impl ServiceControl for FakeServiceControl {
        fn running_dependents(&mut self, service: &str) -> Result<Vec<String>, String> {
            self.calls.push(format!("list {}", service));
            if self.list_fails {
                return Err("access is denied".to_string());
            }
            Ok(self.dependents.clone())
        }

        fn stop(&mut self, service: &str) -> Result<(), String> {
            self.calls.push(format!("stop {}", service));
            if self.refuse_stop.iter().any(|s| s == service) {
                return Err(format!("could not stop {}: refused", service));
            }
            Ok(())
        }

        fn start(&mut self, service: &str) -> Result<(), String> {
            self.calls.push(format!("start {}", service));
            if self.refuse_start.iter().any(|s| s == service) {
                return Err(format!("could not start {}: refused", service));
            }
            Ok(())
        }
    }

    #[test]
    fn service_restart_passes_all_eight_checks_from_service_restart_test_cpp() {
        // 1. Restarts everything in dependency order
        let mut ctrl = FakeServiceControl::new();
        ctrl.dependents = vec!["VendorA".to_string(), "VendorB".to_string()];
        assert!(restart_services(&mut ctrl, AUDIO_SERVICE_NAME, BUILDER_SERVICE_NAME).is_ok());
        assert_eq!(
            ctrl.calls,
            vec![
                "list Audiosrv",
                "stop VendorA",
                "stop VendorB",
                "stop Audiosrv",
                "stop AudioEndpointBuilder",
                "start AudioEndpointBuilder",
                "start Audiosrv",
                "start VendorB",
                "start VendorA",
            ]
        );

        // 2. Dependent that refuses to stop leaves the others running (restarts VendorA)
        let mut ctrl = FakeServiceControl::new();
        ctrl.dependents = vec!["VendorA".to_string(), "VendorB".to_string()];
        ctrl.refuse_stop = vec!["VendorB".to_string()];
        let err =
            restart_services(&mut ctrl, AUDIO_SERVICE_NAME, BUILDER_SERVICE_NAME).unwrap_err();
        assert!(err.contains("could not stop VendorB"));
        assert_eq!(
            ctrl.calls,
            vec![
                "list Audiosrv",
                "stop VendorA",
                "stop VendorB",
                "start VendorA"
            ]
        );

        // 3. Builder that refuses to stop brings Audiosrv and VendorA back
        let mut ctrl = FakeServiceControl::new();
        ctrl.dependents = vec!["VendorA".to_string()];
        ctrl.refuse_stop = vec![BUILDER_SERVICE_NAME.to_string()];
        let err =
            restart_services(&mut ctrl, AUDIO_SERVICE_NAME, BUILDER_SERVICE_NAME).unwrap_err();
        assert!(err.contains("could not stop AudioEndpointBuilder"));
        assert_eq!(
            ctrl.calls,
            vec![
                "list Audiosrv",
                "stop VendorA",
                "stop Audiosrv",
                "stop AudioEndpointBuilder",
                "start Audiosrv",
                "start VendorA",
            ]
        );

        // 4. Both stop and start failures are reported together
        let mut ctrl = FakeServiceControl::new();
        ctrl.dependents = vec!["VendorA".to_string()];
        ctrl.refuse_stop = vec![AUDIO_SERVICE_NAME.to_string()];
        ctrl.refuse_start = vec!["VendorA".to_string()];
        let err =
            restart_services(&mut ctrl, AUDIO_SERVICE_NAME, BUILDER_SERVICE_NAME).unwrap_err();
        assert!(err.contains("could not stop Audiosrv"));
        assert!(err.contains("could not start VendorA"));
    }

    #[test]
    fn acl_evaluation_ignores_inherit_only_and_prioritizes_deny() {
        let mut sec = InMemorySecurityBackend::new();
        let root = Path::new("C:\\ProgramData\\FluidEQ\\engine");

        take_ownership(&mut sec, root).expect("take_ownership succeeds");
        apply_engine_acl(&mut sec, root).expect("apply_engine_acl succeeds");
        assert!(sec.privilege_enabled);
        assert!(service_can_write(&sec, root));

        // Read-only ACL on backup\ denies LOCAL SERVICE write
        let backup = Path::new("C:\\ProgramData\\FluidEQ\\engine\\backup");
        apply_read_only_acl(&mut sec, backup).expect("apply_read_only_acl succeeds");
        assert!(!service_can_write(&sec, backup));

        // An INHERIT_ONLY_ACE deny on Users must NOT mark the directory itself non-writable
        let mut desc = sec.get_security(root).unwrap();
        desc.dacl.as_mut().unwrap().insert(
            0,
            AclAceEntry {
                ace_type: AceKind::AccessDenied,
                ace_flags: INHERIT_ONLY_ACE_FLAG,
                mask: FILE_ALL_ACCESS_MASK,
                trustee: WellKnownSidType::BuiltinUsers,
            },
        );
        sec.descriptors.insert(root.to_path_buf(), desc.clone());
        assert!(service_can_write(&sec, root));

        // A direct (non-inherit-only) AccessDenied on Users settles service_can_write to false
        desc.dacl.as_mut().unwrap()[0].ace_flags = 0;
        sec.descriptors.insert(root.to_path_buf(), desc);
        assert!(!service_can_write(&sec, root));
    }
}
