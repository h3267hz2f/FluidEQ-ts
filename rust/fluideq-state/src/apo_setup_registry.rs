/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! System APO Setup Registry, Multi-SZ Decoder/Encoder, Strict JSON Backup Serializer,
//! COM Class Registration & Per-Endpoint `FxProperties` Transactions (`Slice-M9M10-1`).
//!
//! Strictly 1:1 ports 6 source modules from `/native/system-apo/setup/`:
//! 1. `multi_sz.h/.cpp` (133 lines) — Bounded `REG_MULTI_SZ` and `REG_SZ` UTF-16LE byte decoding
//!    (dropping odd trailing bytes, stopping at empty double-terminator, keeping unterminated
//!    last vendor entry) and `encode_multi_sz`.
//! 2. `json.h/.cpp` (274 lines) + `fx_list.cpp` (`to_json` / `from_json`) — `json_escape`,
//!    fail-closed `JsonScanner` (rejecting raw control chars `< 0x20` and unknown keys),
//!    and exact 5-key `FxValues` JSON round-trip (`single`, `composite`, `compositeWasSz`, `legacy`, `modes`).
//! 3. `backup.h/.cpp` (221 lines) — Write-once disk backup files (`<engine_root>/backup/<guid>.json`
//!    and `<engine_root>/apo-off/<guid>.json`) with strict 38-char GUID path validation.
//! 4. `com_paths.h/.cpp` (94 lines) — Canonical `HKLM` paths (`clsid_registration_path`,
//!    `apo_registration_path`, and `misplaced_apo_record_path`).
//! 5. `com_registration.h/.cpp` (271 lines) — `register_engine`, `unregister_engine`,
//!    `enable_unsigned_effects` (`DisableProtectedAudioDG = 1`), `unsigned_effects_enabled`,
//!    `registered_dll_path`, and `apo_record_present`.
//! 6. `reg_key.h` + `registry.h/.cpp` (580 lines) — `endpoint_key_exists`, `endpoint_is_combined`
//!    (`{b3f8fa53-0004-438e-9003-51a46e139bfc},41`), `read_fx_values` (coercing vendor `REG_SZ`
//!    in composite slots to 1-entry lists with `composite_was_sz = true`, while refusing
//!    `REG_EXPAND_SZ` where type must survive), and `write_fx_values` (enforcing `apo_only` and
//!    `ours_only` guards on single/legacy slots before writing).

use crate::apo::{
    equal_clsid_ci, is_equalizer_apo_clsid, is_valid_endpoint_guid, is_windows_default_apo_clsid,
    EndpointFxValues, FX_LEGACY_COUNT, FX_SLOT_COUNT,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

// ============================================================================
// 1. Bounded UTF-16LE `REG_MULTI_SZ` & `REG_SZ` Codec (`multi_sz.h/.cpp`)
// ============================================================================

/// Copies raw bytes into UTF-16 code units, dropping any odd trailing byte (`characters_of` in `multi_sz.cpp`).
pub fn characters_of_utf16le(bytes: &[u8]) -> Vec<u16> {
    let count = bytes.len() / 2;
    let mut text = Vec::with_capacity(count);
    for i in 0..count {
        let lo = bytes[i * 2];
        let hi = bytes[i * 2 + 1];
        text.push(u16::from_le_bytes([lo, hi]));
    }
    text
}

/// Decodes `REG_MULTI_SZ` UTF-16LE bytes into a list of strings (`decode_multi_sz` in `multi_sz.cpp`).
/// - Bounded strictly by `bytes.len()` (never trusts a trailing NUL that may be absent).
/// - Odd byte count drops the stray trailing byte.
/// - First empty entry (`0x0000`) ends the list; any padding after the double terminator is ignored.
/// - A final entry without a trailing NUL terminator is kept intact.
pub fn decode_multi_sz(bytes: &[u8]) -> Vec<String> {
    let text = characters_of_utf16le(bytes);
    let mut entries = Vec::new();
    let mut at = 0usize;
    while at < text.len() {
        let stop = text[at..]
            .iter()
            .position(|&ch| ch == 0)
            .map(|rel| at + rel)
            .unwrap_or(text.len());
        if stop == at {
            // The empty entry that ends the list.
            break;
        }
        entries.push(String::from_utf16_lossy(&text[at..stop]));
        if stop == text.len() {
            // A last entry with no terminator after it: keep vendor entry and stop.
            break;
        }
        at = stop + 1;
    }
    entries
}

/// Decodes `REG_SZ` or `REG_EXPAND_SZ` UTF-16LE bytes into a single string (`decode_sz` in `multi_sz.cpp`).
pub fn decode_sz(bytes: &[u8]) -> String {
    let text = characters_of_utf16le(bytes);
    let stop = text
        .iter()
        .position(|&ch| ch == 0)
        .unwrap_or(text.len());
    String::from_utf16_lossy(&text[..stop])
}

/// Encodes a slice of strings into `REG_MULTI_SZ` UTF-16 code units: every entry followed by `0u16`,
/// plus a final `0u16` (`encode_multi_sz` in `multi_sz.cpp`).
pub fn encode_multi_sz(entries: &[String]) -> Vec<u16> {
    let mut block = Vec::new();
    for entry in entries {
        block.extend(entry.encode_utf16());
        block.push(0);
    }
    block.push(0);
    block
}

/// Encodes `entries` into raw little-endian bytes suitable for `REG_MULTI_SZ`.
pub fn encode_multi_sz_bytes(entries: &[String]) -> Vec<u8> {
    let u16s = encode_multi_sz(entries);
    let mut bytes = Vec::with_capacity(u16s.len() * 2);
    for unit in u16s {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

/// Encodes a single string into raw little-endian bytes (with trailing UTF-16 NUL) suitable for `REG_SZ`.
pub fn encode_sz_bytes(value: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity((value.len() + 1) * 2);
    for unit in value.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes
}

// ============================================================================
// 2. Minimal Fail-Closed JSON Escaper, Scanner & `FxValues` Codec (`json.h/.cpp` & `fx_list.cpp`)
// ============================================================================

const HEX_DIGITS: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
];

fn hex_value_u16(symbol: u16) -> Option<u16> {
    if (b'0' as u16..=b'9' as u16).contains(&symbol) {
        Some(symbol - b'0' as u16)
    } else if (b'a' as u16..=b'f' as u16).contains(&symbol) {
        Some(symbol - b'a' as u16 + 10)
    } else if (b'A' as u16..=b'F' as u16).contains(&symbol) {
        Some(symbol - b'A' as u16 + 10)
    } else {
        None
    }
}

/// Exact port of `json_escape` in `/native/system-apo/setup/json.cpp`:
/// Replaces `"`, `\`, `\b`, `\f`, `\n`, `\r`, `\t`, and anything `< 0x20` as `\u00xx`.
/// Characters `>= 0x20` are left untouched.
pub fn json_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let code = c as usize;
                out.push_str("\\u00");
                out.push(HEX_DIGITS[(code >> 4) & 0xF]);
                out.push(HEX_DIGITS[code & 0xF]);
            }
            c => out.push(c),
        }
    }
    out
}

/// Fail-closed JSON scanner over UTF-16 code units (`JsonScanner` in `/native/system-apo/setup/json.h/.cpp`).
pub struct JsonScanner {
    units: Vec<u16>,
    at: usize,
}

impl JsonScanner {
    pub fn new(text: &str) -> Self {
        Self {
            units: text.encode_utf16().collect(),
            at: 0,
        }
    }

    fn skip_space(&mut self) {
        while self.at < self.units.len() {
            let u = self.units[self.at];
            if u == b' ' as u16 || u == b'\t' as u16 || u == b'\r' as u16 || u == b'\n' as u16 {
                self.at += 1;
            } else {
                break;
            }
        }
    }

    pub fn consume(&mut self, symbol: char) -> bool {
        self.skip_space();
        let target = symbol as u16;
        if self.at >= self.units.len() || self.units[self.at] != target {
            return false;
        }
        self.at += 1;
        true
    }

    pub fn at_end(&mut self) -> bool {
        self.skip_space();
        self.at >= self.units.len()
    }

    fn read_literal(&mut self, literal: &str) -> bool {
        self.skip_space();
        let lit_units: Vec<u16> = literal.encode_utf16().collect();
        if self.units.len() - self.at < lit_units.len() {
            return false;
        }
        if self.units[self.at..self.at + lit_units.len()] != lit_units[..] {
            return false;
        }
        self.at += lit_units.len();
        true
    }

    pub fn read_null(&mut self) -> bool {
        self.read_literal("null")
    }

    pub fn read_bool(&mut self) -> Option<bool> {
        if self.read_literal("true") {
            Some(true)
        } else if self.read_literal("false") {
            Some(false)
        } else {
            None
        }
    }

    pub fn read_string(&mut self) -> Option<String> {
        self.skip_space();
        if self.at >= self.units.len() || self.units[self.at] != b'"' as u16 {
            return None;
        }
        self.at += 1;
        let mut value_units: Vec<u16> = Vec::new();
        while self.at < self.units.len() {
            let symbol = self.units[self.at];
            self.at += 1;
            if symbol == b'"' as u16 {
                return String::from_utf16(&value_units).ok();
            }
            if symbol != b'\\' as u16 {
                // Raw control characters (< 0x20) inside a string are illegal JSON.
                if symbol < 0x20 {
                    return None;
                }
                value_units.push(symbol);
                continue;
            }
            if self.at >= self.units.len() {
                return None;
            }
            let escaped = self.units[self.at];
            self.at += 1;
            match escaped as u8 as char {
                '"' | '\\' | '/' if escaped < 0x80 => value_units.push(escaped),
                'b' if escaped < 0x80 => value_units.push(0x0008),
                'f' if escaped < 0x80 => value_units.push(0x000C),
                'n' if escaped < 0x80 => value_units.push(b'\n' as u16),
                'r' if escaped < 0x80 => value_units.push(b'\r' as u16),
                't' if escaped < 0x80 => value_units.push(b'\t' as u16),
                'u' if escaped < 0x80 => {
                    if self.units.len() - self.at < 4 {
                        return None;
                    }
                    let mut code: u16 = 0;
                    for digit in 0..4 {
                        let nibble = hex_value_u16(self.units[self.at + digit])?;
                        code = (code << 4) | nibble;
                    }
                    self.at += 4;
                    value_units.push(code);
                }
                _ => return None,
            }
        }
        None
    }
}

fn append_optional_string(out: &mut String, value: &Option<String>) {
    match value {
        None => out.push_str("null"),
        Some(s) => {
            out.push('"');
            out.push_str(&json_escape(s));
            out.push('"');
        }
    }
}

fn append_optional_list(out: &mut String, value: &Option<Vec<String>>) {
    match value {
        None => out.push_str("null"),
        Some(list) => {
            out.push('[');
            for (at, item) in list.iter().enumerate() {
                if at != 0 {
                    out.push(',');
                }
                out.push('"');
                out.push_str(&json_escape(item));
                out.push('"');
            }
            out.push(']');
        }
    }
}

fn read_bool_slots(scanner: &mut JsonScanner, out: &mut [bool]) -> bool {
    if !scanner.consume('[') {
        return false;
    }
    for at in 0..out.len() {
        if at != 0 && !scanner.consume(',') {
            return false;
        }
        let Some(b) = scanner.read_bool() else {
            return false;
        };
        out[at] = b;
    }
    scanner.consume(']')
}

fn read_string_slots(scanner: &mut JsonScanner, out: &mut [Option<String>]) -> bool {
    if !scanner.consume('[') {
        return false;
    }
    for at in 0..out.len() {
        if at != 0 && !scanner.consume(',') {
            return false;
        }
        if scanner.read_null() {
            out[at] = None;
            continue;
        }
        let Some(s) = scanner.read_string() else {
            return false;
        };
        out[at] = Some(s);
    }
    scanner.consume(']')
}

fn read_list_slots(scanner: &mut JsonScanner, out: &mut [Option<Vec<String>>]) -> bool {
    if !scanner.consume('[') {
        return false;
    }
    for at in 0..out.len() {
        if at != 0 && !scanner.consume(',') {
            return false;
        }
        if scanner.read_null() {
            out[at] = None;
            continue;
        }
        if !scanner.consume('[') {
            return false;
        }
        let mut entries = Vec::new();
        if !scanner.consume(']') {
            loop {
                let Some(entry) = scanner.read_string() else {
                    return false;
                };
                entries.push(entry);
                if scanner.consume(',') {
                    continue;
                }
                if !scanner.consume(']') {
                    return false;
                }
                break;
            }
        }
        out[at] = Some(entries);
    }
    scanner.consume(']')
}

/// Exact port of `to_json(const FxValues&)` in `/native/system-apo/setup/fx_list.cpp`.
pub fn fx_values_to_json(values: &EndpointFxValues) -> String {
    let mut out = String::from("{\"single\":[");
    for at in 0..FX_SLOT_COUNT {
        if at != 0 {
            out.push(',');
        }
        append_optional_string(&mut out, &values.single[at]);
    }
    out.push_str("],\"composite\":[");
    for at in 0..FX_SLOT_COUNT {
        if at != 0 {
            out.push(',');
        }
        append_optional_list(&mut out, &values.composite[at]);
    }
    out.push_str("],\"compositeWasSz\":[");
    for at in 0..FX_SLOT_COUNT {
        if at != 0 {
            out.push(',');
        }
        out.push_str(if values.composite_was_sz[at] {
            "true"
        } else {
            "false"
        });
    }
    out.push_str("],\"legacy\":[");
    for at in 0..FX_LEGACY_COUNT {
        if at != 0 {
            out.push(',');
        }
        append_optional_string(&mut out, &values.legacy[at]);
    }
    out.push_str("],\"modes\":[");
    for at in 0..FX_SLOT_COUNT {
        if at != 0 {
            out.push(',');
        }
        append_optional_list(&mut out, &values.modes[at]);
    }
    out.push_str("]}");
    out
}

/// Exact port of `from_json(std::wstring_view)` in `/native/system-apo/setup/fx_list.cpp`:
/// Refuses unknown or duplicate keys and requires all 5 keys (`single`, `composite`,
/// `compositeWasSz`, `legacy`, `modes`) with nothing trailing.
pub fn fx_values_from_json(text: &str) -> Option<EndpointFxValues> {
    let mut scanner = JsonScanner::new(text);
    if !scanner.consume('{') {
        return None;
    }
    let mut values = EndpointFxValues::default();
    let mut seen_single = false;
    let mut seen_composite = false;
    let mut seen_composite_was_sz = false;
    let mut seen_legacy = false;
    let mut seen_modes = false;

    if !scanner.consume('}') {
        loop {
            let key = scanner.read_string()?;
            if !scanner.consume(':') {
                return None;
            }
            if key == "single" && !seen_single {
                seen_single = true;
                if !read_string_slots(&mut scanner, &mut values.single) {
                    return None;
                }
            } else if key == "composite" && !seen_composite {
                seen_composite = true;
                if !read_list_slots(&mut scanner, &mut values.composite) {
                    return None;
                }
            } else if key == "compositeWasSz" && !seen_composite_was_sz {
                seen_composite_was_sz = true;
                if !read_bool_slots(&mut scanner, &mut values.composite_was_sz) {
                    return None;
                }
            } else if key == "legacy" && !seen_legacy {
                seen_legacy = true;
                if !read_string_slots(&mut scanner, &mut values.legacy) {
                    return None;
                }
            } else if key == "modes" && !seen_modes {
                seen_modes = true;
                if !read_list_slots(&mut scanner, &mut values.modes) {
                    return None;
                }
            } else {
                return None;
            }

            if scanner.consume(',') {
                continue;
            }
            if !scanner.consume('}') {
                return None;
            }
            break;
        }
    }

    if !scanner.at_end()
        || !seen_single
        || !seen_composite
        || !seen_composite_was_sz
        || !seen_legacy
        || !seen_modes
    {
        return None;
    }
    Some(values)
}

// ============================================================================
// 3. Write-Once Disk Backup & `apo-off` Persistence (`backup.h/.cpp`)
// ============================================================================

/// `<engine_root>/backup/<guid>.json`, or `None` when `guid` is not a valid 38-char GUID
/// (`backup_path` in `/native/system-apo/setup/backup.cpp`).
pub fn backup_path(engine_root: &Path, guid: &str) -> Option<PathBuf> {
    if !is_valid_endpoint_guid(guid) || engine_root.as_os_str().is_empty() {
        return None;
    }
    Some(engine_root.join("backup").join(format!("{}.json", guid)))
}

/// `<engine_root>/apo-off/<guid>.json`, or `None` when `guid` is not a valid 38-char GUID
/// (`apo_off_path` in `/native/system-apo/setup/backup.cpp`).
pub fn apo_off_path(engine_root: &Path, guid: &str) -> Option<PathBuf> {
    if !is_valid_endpoint_guid(guid) || engine_root.as_os_str().is_empty() {
        return None;
    }
    Some(engine_root.join("apo-off").join(format!("{}.json", guid)))
}

pub fn backup_exists_on_disk(engine_root: &Path, guid: &str) -> bool {
    backup_path(engine_root, guid)
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// Writes `values` to `<engine_root>/backup/<guid>.json` ONLY if no backup file exists yet
/// (`save_backup_once` in `/native/system-apo/setup/backup.cpp`).
pub fn save_backup_once_on_disk(
    engine_root: &Path,
    guid: &str,
    values: &EndpointFxValues,
) -> Result<(), String> {
    let Some(path) = backup_path(engine_root, guid) else {
        return Err(format!("no backup location for {}", guid));
    };
    if path.exists() {
        return Ok(());
    }
    let dir = engine_root.join("backup");
    if let Err(e) = fs::create_dir_all(&dir) {
        return Err(format!("could not create {}: {}", dir.display(), e));
    }
    let json = fx_values_to_json(values);
    if let Err(e) = fs::write(&path, json.as_bytes()) {
        return Err(format!("could not write {}: {}", path.display(), e));
    }
    Ok(())
}

/// Reads and validates `<engine_root>/backup/<guid>.json` (`load_backup` in `backup.cpp`).
pub fn load_backup_from_disk(engine_root: &Path, guid: &str) -> Option<EndpointFxValues> {
    let path = backup_path(engine_root, guid)?;
    let text = fs::read_to_string(path).ok()?;
    fx_values_from_json(&text)
}

/// Deletes `<engine_root>/backup/<guid>.json` once an endpoint is restored (`remove_backup` in `backup.cpp`).
pub fn remove_backup_on_disk(engine_root: &Path, guid: &str) {
    if let Some(path) = backup_path(engine_root, guid) {
        let _ = fs::remove_file(path);
    }
}

/// Writes `values` to `<engine_root>/apo-off/<guid>.json` ONLY if no `apo-off` file exists yet
/// (`save_apo_off_once` in `/native/system-apo/setup/backup.cpp`).
pub fn save_apo_off_once_on_disk(
    engine_root: &Path,
    guid: &str,
    values: &EndpointFxValues,
) -> Result<(), String> {
    let Some(path) = apo_off_path(engine_root, guid) else {
        return Err(format!(
            "no location to record Equalizer APO's state for {}",
            guid
        ));
    };
    if path.exists() {
        return Ok(());
    }
    let dir = engine_root.join("apo-off");
    if let Err(e) = fs::create_dir_all(&dir) {
        return Err(format!("could not create {}: {}", dir.display(), e));
    }
    let json = fx_values_to_json(values);
    if let Err(e) = fs::write(&path, json.as_bytes()) {
        return Err(format!("could not write {}: {}", path.display(), e));
    }
    Ok(())
}

/// Reads and validates `<engine_root>/apo-off/<guid>.json` (`load_apo_off` in `backup.cpp`).
pub fn load_apo_off_from_disk(engine_root: &Path, guid: &str) -> Option<EndpointFxValues> {
    let path = apo_off_path(engine_root, guid)?;
    let text = fs::read_to_string(path).ok()?;
    fx_values_from_json(&text)
}

/// Deletes `<engine_root>/apo-off/<guid>.json` (`remove_apo_off` in `backup.cpp`).
pub fn remove_apo_off_on_disk(engine_root: &Path, guid: &str) {
    if let Some(path) = apo_off_path(engine_root, guid) {
        let _ = fs::remove_file(path);
    }
}

/// Lists every valid endpoint GUID recorded under `<engine_root>/apo-off/*.json`
/// (`apo_off_endpoints` in `/native/system-apo/setup/backup.cpp`).
pub fn apo_off_endpoints_on_disk(engine_root: &Path) -> Vec<String> {
    let mut guids = Vec::new();
    if engine_root.as_os_str().is_empty() {
        return guids;
    }
    let dir = engine_root.join("apo-off");
    let Ok(entries) = fs::read_dir(dir) else {
        return guids;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !name.ends_with(".json") {
            continue;
        }
        let Some(dot) = name.rfind('.') else {
            continue;
        };
        let guid = &name[..dot];
        if is_valid_endpoint_guid(guid) {
            guids.push(guid.to_string());
        }
    }
    guids.sort();
    guids
}

// ============================================================================
// 4. COM Registration Paths & Machine-Wide Registration (`com_paths.h/.cpp` & `com_registration.h/.cpp`)
// ============================================================================

/// `{B7E2C4D1-5A8F-4C3E-9D2B-6F1A0C8E7D34}` — FluidEQ Engine COM class ID (`kEngineClsid` in `com_paths.cpp`).
pub const SETUP_ENGINE_CLSID: &str = "{B7E2C4D1-5A8F-4C3E-9D2B-6F1A0C8E7D34}";
pub const ENGINE_CLSID_UPPER: &str = SETUP_ENGINE_CLSID;

/// `"FluidEQ Engine"` (`kEngineFriendlyName` in `com_registration.cpp`).
pub const ENGINE_FRIENDLY_NAME: &str = "FluidEQ Engine";

/// Copyright notice written to `AudioProcessingObjects` (`com_registration.cpp`).
pub const ENGINE_COPYRIGHT: &str =
    "Copyright (C) 2026 Ivan Carmenates Garcia. GPL-3.0-or-later.";

/// `{FD7F2B29-24D0-4B5C-B177-592C39F9CA10}` — `IID_IAudioProcessingObject` (`kApoInterface` in `com_registration.cpp`).
pub const APO_INTERFACE_IID: &str = "{FD7F2B29-24D0-4B5C-B177-592C39F9CA10}";

/// `SOFTWARE\Microsoft\Windows\CurrentVersion\Audio` (`kAudioPath` in `com_registration.cpp`).
pub const AUDIO_POLICY_PATH: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Audio";

/// `SOFTWARE\Classes\CLSID\{B7E2C4D1-5A8F-4C3E-9D2B-6F1A0C8E7D34}` (`clsid_registration_path` in `com_paths.cpp`).
pub fn clsid_registration_path() -> String {
    format!("SOFTWARE\\Classes\\CLSID\\{}", SETUP_ENGINE_CLSID)
}

/// `SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{B7E2C4D1-5A8F-4C3E-9D2B-6F1A0C8E7D34}`
/// (`apo_registration_path` in `com_paths.cpp`).
pub fn apo_registration_path() -> String {
    format!(
        "SOFTWARE\\Classes\\AudioEngine\\AudioProcessingObjects\\{}",
        SETUP_ENGINE_CLSID
    )
}

/// `SOFTWARE\Microsoft\Windows\CurrentVersion\Audio\AudioProcessingObjects\{B7E2C4D1-5A8F-4C3E-9D2B-6F1A0C8E7D34}`
/// (`misplaced_apo_record_path` in `com_paths.cpp`).
pub fn misplaced_apo_record_path() -> String {
    format!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Audio\\AudioProcessingObjects\\{}",
        SETUP_ENGINE_CLSID
    )
}

// ============================================================================
// 5. Typed 64-Bit Registry Abstraction & `FxProperties` Transaction Engine (`reg_key.h` & `registry.h/.cpp`)
// ============================================================================

pub const REG_NONE_TYPE: u32 = 0;
pub const REG_SZ_TYPE: u32 = 1;
pub const REG_EXPAND_SZ_TYPE: u32 = 2;
pub const REG_BINARY_TYPE: u32 = 3;
pub const REG_DWORD_TYPE: u32 = 4;
pub const REG_MULTI_SZ_TYPE: u32 = 7;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRegistryValue {
    pub value_type: u32,
    pub bytes: Vec<u8>,
}

/// Abstracts `HKEY_LOCAL_MACHINE` (`KEY_WOW64_64KEY`) operations from `reg_key.h` so both
/// `com_registration.cpp` and `registry.cpp` run with 100% branch fidelity in tests and on Windows.
pub trait RegistryBackend {
    fn key_exists(&self, path: &str) -> Result<bool, String>;
    fn ensure_key(&mut self, path: &str) -> Result<(), String>;
    fn delete_tree(&mut self, parent_path: &str, child_name: &str) -> Result<(), String>;
    fn query_value(&self, key_path: &str, value_name: &str) -> Result<Option<RawRegistryValue>, String>;
    fn set_string(&mut self, key_path: &str, value_name: &str, value: &str) -> Result<(), String>;
    fn set_dword(&mut self, key_path: &str, value_name: &str, value: u32) -> Result<(), String>;
    fn set_multi_sz(
        &mut self,
        key_path: &str,
        value_name: &str,
        entries: &[String],
    ) -> Result<(), String>;
    fn delete_value(&mut self, key_path: &str, value_name: &str) -> Result<(), String>;
}

/// Deterministic in-memory `HKEY_LOCAL_MACHINE` registry hive supporting exact `REG_SZ`,
/// `REG_EXPAND_SZ`, `REG_MULTI_SZ`, and `REG_DWORD` raw byte representations.
#[derive(Debug, Default, Clone)]
pub struct InMemoryRegistryHive {
    keys: BTreeMap<String, BTreeMap<String, RawRegistryValue>>,
}

impl InMemoryRegistryHive {
    pub fn new() -> Self {
        Self::default()
    }

    fn normalize_key(path: &str) -> String {
        path.trim_matches('\\').to_ascii_uppercase()
    }

    fn normalize_value_name(name: &str) -> String {
        name.to_ascii_uppercase()
    }

    /// Inserts a raw registry value (e.g., to test `REG_EXPAND_SZ` refusal or malformed `REG_MULTI_SZ`).
    pub fn set_raw_value(
        &mut self,
        key_path: &str,
        value_name: &str,
        value_type: u32,
        bytes: Vec<u8>,
    ) {
        let key = Self::normalize_key(key_path);
        let val = Self::normalize_value_name(value_name);
        self.keys
            .entry(key)
            .or_default()
            .insert(val, RawRegistryValue { value_type, bytes });
    }

    pub fn create_key(&mut self, path: &str) -> Result<(), String> {
        self.ensure_key(path)
    }
}

impl RegistryBackend for InMemoryRegistryHive {
    fn key_exists(&self, path: &str) -> Result<bool, String> {
        let norm = Self::normalize_key(path);
        let prefix = format!("{}\\", norm);
        Ok(self
            .keys
            .keys()
            .any(|k| k == &norm || k.starts_with(&prefix)))
    }

    fn ensure_key(&mut self, path: &str) -> Result<(), String> {
        let norm = Self::normalize_key(path);
        self.keys.entry(norm).or_default();
        Ok(())
    }

    fn delete_tree(&mut self, parent_path: &str, child_name: &str) -> Result<(), String> {
        let full = if parent_path.is_empty() {
            Self::normalize_key(child_name)
        } else {
            Self::normalize_key(&format!("{}\\{}", parent_path, child_name))
        };
        let prefix = format!("{}\\", full);
        self.keys
            .retain(|k, _| k != &full && !k.starts_with(&prefix));
        Ok(())
    }

    fn query_value(
        &self,
        key_path: &str,
        value_name: &str,
    ) -> Result<Option<RawRegistryValue>, String> {
        let norm_key = Self::normalize_key(key_path);
        let norm_val = Self::normalize_value_name(value_name);
        let Some(values) = self.keys.get(&norm_key) else {
            return Ok(None);
        };
        Ok(values.get(&norm_val).cloned())
    }

    fn set_string(&mut self, key_path: &str, value_name: &str, value: &str) -> Result<(), String> {
        self.set_raw_value(
            key_path,
            value_name,
            REG_SZ_TYPE,
            encode_sz_bytes(value),
        );
        Ok(())
    }

    fn set_dword(&mut self, key_path: &str, value_name: &str, value: u32) -> Result<(), String> {
        self.set_raw_value(
            key_path,
            value_name,
            REG_DWORD_TYPE,
            value.to_le_bytes().to_vec(),
        );
        Ok(())
    }

    fn set_multi_sz(
        &mut self,
        key_path: &str,
        value_name: &str,
        entries: &[String],
    ) -> Result<(), String> {
        self.set_raw_value(
            key_path,
            value_name,
            REG_MULTI_SZ_TYPE,
            encode_multi_sz_bytes(entries),
        );
        Ok(())
    }

    fn delete_value(&mut self, key_path: &str, value_name: &str) -> Result<(), String> {
        let norm_key = Self::normalize_key(key_path);
        let norm_val = Self::normalize_value_name(value_name);
        if let Some(values) = self.keys.get_mut(&norm_key) {
            values.remove(&norm_val);
        }
        Ok(())
    }
}

fn delete_engine_tree<R: RegistryBackend>(reg: &mut R, path: &str) -> Result<(), String> {
    let Some(slash) = path.rfind('\\') else {
        return Err(format!("invalid registry path: {}", path));
    };
    let above = &path[..slash];
    if !reg.key_exists(above)? {
        return Ok(());
    }
    reg.delete_tree(above, SETUP_ENGINE_CLSID)
}

/// Exact port of `register_engine` in `/native/system-apo/setup/com_registration.cpp`:
/// 1. Removes `misplaced_apo_record_path()` first so old installs leave no stale record.
/// 2. Writes `SOFTWARE\Classes\CLSID\{clsid}` default value (`"FluidEQ Engine"`) and
///    `InProcServer32` (`dll_path`, `ThreadingModel = "Both"`).
/// 3. Writes `SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{clsid}` (`FriendlyName`,
///    `Copyright`, `APOInterface0`, and the 9 `DWORD` registration attributes: `MajorVersion=1`,
///    `MinorVersion=0`, `Flags=0xF`, `MinInputConnections=1`, `MaxInputConnections=1`,
///    `MinOutputConnections=1`, `MaxOutputConnections=1`, `MaxInstances=0xFFFFFFFF`, `NumAPOInterfaces=1`).
pub fn register_engine<R: RegistryBackend>(reg: &mut R, dll_path: &str) -> Result<(), String> {
    delete_engine_tree(reg, &misplaced_apo_record_path())?;

    let clsid_key = clsid_registration_path();
    reg.ensure_key(&clsid_key)
        .and_then(|()| reg.set_string(&clsid_key, "", ENGINE_FRIENDLY_NAME))
        .map_err(|e| format!("could not write the class registration: {}", e))?;

    let server_key = format!("{}\\InProcServer32", clsid_key);
    reg.ensure_key(&server_key)
        .and_then(|()| reg.set_string(&server_key, "", dll_path))
        .and_then(|()| reg.set_string(&server_key, "ThreadingModel", "Both"))
        .map_err(|e| format!("could not write the in-process server path: {}", e))?;

    let apo_key = apo_registration_path();
    reg.ensure_key(&apo_key)
        .and_then(|()| reg.set_string(&apo_key, "FriendlyName", ENGINE_FRIENDLY_NAME))
        .and_then(|()| reg.set_string(&apo_key, "Copyright", ENGINE_COPYRIGHT))
        .and_then(|()| reg.set_string(&apo_key, "APOInterface0", APO_INTERFACE_IID))
        .map_err(|e| {
            format!(
                "could not write the audio processing object registration: {}",
                e
            )
        })?;

    let numbers: [(&str, u32); 9] = [
        ("MajorVersion", 1),
        ("MinorVersion", 0),
        ("Flags", 0xF),
        ("MinInputConnections", 1),
        ("MaxInputConnections", 1),
        ("MinOutputConnections", 1),
        ("MaxOutputConnections", 1),
        ("MaxInstances", 0xFFFF_FFFF),
        ("NumAPOInterfaces", 1),
    ];
    for (name, val) in numbers {
        reg.set_dword(&apo_key, name, val).map_err(|e| {
            format!(
                "could not write the audio processing object registration: {}",
                e
            )
        })?;
    }
    Ok(())
}

/// Exact port of `unregister_engine` in `/native/system-apo/setup/com_registration.cpp`:
/// Removes `clsid_registration_path()`, `apo_registration_path()`, and `misplaced_apo_record_path()`,
/// leaving `DisableProtectedAudioDG` untouched.
pub fn unregister_engine<R: RegistryBackend>(reg: &mut R) -> Result<(), String> {
    for path in [
        clsid_registration_path(),
        apo_registration_path(),
        misplaced_apo_record_path(),
    ] {
        delete_engine_tree(reg, &path)?;
    }
    Ok(())
}

/// Exact port of `apo_record_present` in `/native/system-apo/setup/com_registration.cpp`.
pub fn apo_record_present<R: RegistryBackend>(reg: &R) -> bool {
    reg.key_exists(&apo_registration_path()).unwrap_or(false)
}

/// Exact port of `enable_unsigned_effects` in `/native/system-apo/setup/com_registration.cpp`:
/// Sets `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio\DisableProtectedAudioDG = 1`.
pub fn enable_unsigned_effects<R: RegistryBackend>(reg: &mut R) -> Result<(), String> {
    reg.ensure_key(AUDIO_POLICY_PATH)
        .and_then(|()| reg.set_dword(AUDIO_POLICY_PATH, "DisableProtectedAudioDG", 1))
        .map_err(|e| format!("could not allow unsigned audio effects: {}", e))
}

/// Exact port of `unsigned_effects_enabled` in `/native/system-apo/setup/com_registration.cpp`.
pub fn unsigned_effects_enabled<R: RegistryBackend>(reg: &R) -> bool {
    if !reg.key_exists(AUDIO_POLICY_PATH).unwrap_or(false) {
        return false;
    }
    let Ok(Some(raw)) = reg.query_value(AUDIO_POLICY_PATH, "DisableProtectedAudioDG") else {
        return false;
    };
    if raw.value_type != REG_DWORD_TYPE || raw.bytes.len() < 4 {
        return false;
    }
    let value = u32::from_le_bytes([raw.bytes[0], raw.bytes[1], raw.bytes[2], raw.bytes[3]]);
    value != 0
}

/// Exact port of `registered_dll_path` in `/native/system-apo/setup/com_registration.cpp`.
pub fn registered_dll_path<R: RegistryBackend>(reg: &R) -> String {
    let server_key = format!("{}\\InProcServer32", clsid_registration_path());
    if !reg.key_exists(&server_key).unwrap_or(false) {
        return String::new();
    }
    let Ok(Some(raw)) = reg.query_value(&server_key, "") else {
        return String::new();
    };
    if raw.value_type != REG_SZ_TYPE && raw.value_type != REG_EXPAND_SZ_TYPE {
        return String::new();
    }
    decode_sz(&raw.bytes)
}

// ============================================================================
// 6. Per-Endpoint `FxProperties` Registry Reader & Guarded Writer (`registry.h/.cpp`)
// ============================================================================

pub const RENDER_ENDPOINTS_PATH: &str =
    "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\MMDevices\\Audio\\Render";
pub const FX_PROPERTY_SET: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d}";
pub const MODE_PROPERTY_SET: &str = "{d3993a3f-99c2-4402-b5ec-a92a0367664b}";
pub const COMBINED_ENDPOINT_PROPERTY: &str = "{b3f8fa53-0004-438e-9003-51a46e139bfc},41";

pub const SINGLE_PIDS: [i32; FX_SLOT_COUNT] = [5, 6, 7];
pub const COMPOSITE_PIDS: [i32; FX_SLOT_COUNT] = [13, 14, 15];
pub const LEGACY_PIDS: [i32; FX_LEGACY_COUNT] = [1, 2];
pub const MODE_PIDS: [i32; FX_SLOT_COUNT] = [5, 6, 7];

pub fn fx_property_value_name(property_set: &str, pid: i32) -> String {
    format!("{},{}", property_set, pid)
}

pub fn endpoint_registry_path(guid: &str) -> String {
    format!("{}\\{}", RENDER_ENDPOINTS_PATH, guid)
}

pub fn endpoint_fx_registry_path(guid: &str) -> String {
    format!("{}\\FxProperties", endpoint_registry_path(guid))
}

/// Exact port of `endpoint_key_exists` in `/native/system-apo/setup/registry.cpp`.
pub fn endpoint_key_exists<R: RegistryBackend>(reg: &R, guid: &str) -> bool {
    if !is_valid_endpoint_guid(guid) {
        return false;
    }
    reg.key_exists(&endpoint_registry_path(guid))
        .unwrap_or(false)
}

/// Exact port of `endpoint_is_combined` in `/native/system-apo/setup/registry.cpp`:
/// Checks whether Windows 11 folded a Bluetooth stereo + hands-free headset into one combined
/// endpoint by checking presence of `{b3f8fa53-0004-438e-9003-51a46e139bfc},41` under `Properties`.
pub fn endpoint_is_combined<R: RegistryBackend>(reg: &R, guid: &str) -> bool {
    if !is_valid_endpoint_guid(guid) {
        return false;
    }
    let props_path = format!("{}\\Properties", endpoint_registry_path(guid));
    if !reg.key_exists(&props_path).unwrap_or(false) {
        return false;
    }
    matches!(
        reg.query_value(&props_path, COMBINED_ENDPOINT_PROPERTY),
        Ok(Some(_))
    )
}

fn read_single_reg<R: RegistryBackend>(
    reg: &R,
    key_path: &str,
    name: &str,
) -> Result<Option<String>, String> {
    let raw = reg
        .query_value(key_path, name)
        .map_err(|_| format!("could not read {}", name))?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.value_type != REG_SZ_TYPE && raw.value_type != REG_EXPAND_SZ_TYPE {
        return Err(format!("{} is not a string", name));
    }
    Ok(Some(decode_sz(&raw.bytes)))
}

fn read_list_reg<R: RegistryBackend>(
    reg: &R,
    key_path: &str,
    name: &str,
    track_was_sz: bool,
) -> Result<(Option<Vec<String>>, bool), String> {
    let raw = reg
        .query_value(key_path, name)
        .map_err(|_| format!("could not read {}", name))?;
    let Some(raw) = raw else {
        return Ok((None, false));
    };
    if raw.value_type == REG_MULTI_SZ_TYPE {
        return Ok((Some(decode_multi_sz(&raw.bytes)), false));
    }
    // Refuses REG_EXPAND_SZ where the original type has to survive (`was_sz != nullptr`).
    if raw.value_type == REG_EXPAND_SZ_TYPE && track_was_sz {
        return Err(format!(
            "{} holds an expandable string, which this program will not convert",
            name
        ));
    }
    if raw.value_type == REG_SZ_TYPE || raw.value_type == REG_EXPAND_SZ_TYPE {
        let only = decode_sz(&raw.bytes);
        let list = if only.is_empty() {
            Vec::new()
        } else {
            vec![only]
        };
        return Ok((Some(list), track_was_sz));
    }
    Err(format!("{} is neither a string nor a list", name))
}

fn wants_sz(values: &EndpointFxValues, slot: usize) -> bool {
    values.composite_was_sz[slot]
        && values.composite[slot]
            .as_ref()
            .map(|list| list.len() <= 1)
            .unwrap_or(false)
}

fn apply_list_reg<R: RegistryBackend>(
    reg: &mut R,
    key_path: &str,
    name: &str,
    value: &Option<Vec<String>>,
    as_sz: bool,
) -> Result<(), String> {
    let Some(list) = value.as_ref() else {
        return reg.delete_value(key_path, name);
    };
    if as_sz {
        let single_str = list.first().map(|s| s.as_str()).unwrap_or("");
        reg.set_string(key_path, name, single_str)
    } else {
        reg.set_multi_sz(key_path, name, list)
    }
}

fn apply_single_reg<R: RegistryBackend>(
    reg: &mut R,
    key_path: &str,
    name: &str,
    value: &Option<String>,
) -> Result<(), String> {
    match value.as_deref() {
        None => reg.delete_value(key_path, name),
        Some(s) => reg.set_string(key_path, name, s),
    }
}

/// Exact port of `read_fx_values` in `/native/system-apo/setup/registry.cpp`:
/// - Validates 38-char GUID.
/// - Absent `FxProperties` key returns empty `EndpointFxValues::default()`.
/// - Reads `single[0..3]` (pids 5/6/7), `composite[0..3]` (pids 13/14/15, recording `composite_was_sz`),
///   `modes[0..3]` (pids 5/6/7), and `legacy[0..2]` (pids 1/2).
pub fn read_fx_values<R: RegistryBackend>(
    reg: &R,
    guid: &str,
) -> Result<EndpointFxValues, String> {
    if !is_valid_endpoint_guid(guid) {
        return Err(format!("not an endpoint id: {}", guid));
    }
    let fx_key = endpoint_fx_registry_path(guid);
    let exists = reg.key_exists(&fx_key).map_err(|e| {
        format!(
            "could not open the effect properties of {}: {}",
            guid, e
        )
    })?;
    if !exists {
        return Ok(EndpointFxValues::default());
    }

    let mut out = EndpointFxValues::default();
    for slot in 0..FX_SLOT_COUNT {
        let single_name = fx_property_value_name(FX_PROPERTY_SET, SINGLE_PIDS[slot]);
        out.single[slot] = read_single_reg(reg, &fx_key, &single_name)?;

        let comp_name = fx_property_value_name(FX_PROPERTY_SET, COMPOSITE_PIDS[slot]);
        let (comp_list, was_sz) = read_list_reg(reg, &fx_key, &comp_name, true)?;
        out.composite[slot] = comp_list;
        out.composite_was_sz[slot] = was_sz;

        let mode_name = fx_property_value_name(MODE_PROPERTY_SET, MODE_PIDS[slot]);
        let (mode_list, _) = read_list_reg(reg, &fx_key, &mode_name, false)?;
        out.modes[slot] = mode_list;
    }
    for slot in 0..FX_LEGACY_COUNT {
        let leg_name = fx_property_value_name(FX_PROPERTY_SET, LEGACY_PIDS[slot]);
        out.legacy[slot] = read_single_reg(reg, &fx_key, &leg_name)?;
    }
    Ok(out)
}

/// Exact port of `write_fx_values` in `/native/system-apo/setup/registry.cpp`:
/// - Enforces `apo_only` and `ours_only` invariants across all `single` (pids 5/6/7) and `legacy` (pids 1/2)
///   slots BEFORE touching the registry; if any vendor single/legacy value would be altered, refuses
///   the entire write without opening the key.
/// - Writes only changed `single`, `legacy`, `composite` (including `REG_MULTI_SZ` <-> `REG_SZ` type changes
///   tracked via `wants_sz`), and `modes` values.
pub fn write_fx_values<R: RegistryBackend>(
    reg: &mut R,
    guid: &str,
    before: &EndpointFxValues,
    after: &EndpointFxValues,
) -> Result<(), String> {
    if !is_valid_endpoint_guid(guid) {
        return Err(format!("not an endpoint id: {}", guid));
    }

    let apo_only = |was: &Option<String>, now: &Option<String>| -> bool {
        if let (Some(w), None) = (was.as_deref(), now.as_deref()) {
            return is_equalizer_apo_clsid(w);
        }
        if let (None, Some(n)) = (was.as_deref(), now.as_deref()) {
            return is_equalizer_apo_clsid(n);
        }
        false
    };

    let empty_or_ours = |value: &Option<String>| -> bool {
        match value.as_deref() {
            None | Some("") => true,
            Some(v) => equal_clsid_ci(v, SETUP_ENGINE_CLSID) || is_windows_default_apo_clsid(v),
        }
    };

    let ours_only = |was: &Option<String>, now: &Option<String>| -> bool {
        empty_or_ours(was) && empty_or_ours(now)
    };

    for slot in 0..FX_SLOT_COUNT {
        if before.single[slot] != after.single[slot]
            && !apo_only(&before.single[slot], &after.single[slot])
            && !ours_only(&before.single[slot], &after.single[slot])
        {
            return Err(format!(
                "refusing to change the single effect values of {}",
                guid
            ));
        }
    }
    for slot in 0..FX_LEGACY_COUNT {
        if before.legacy[slot] != after.legacy[slot]
            && !apo_only(&before.legacy[slot], &after.legacy[slot])
            && !ours_only(&before.legacy[slot], &after.legacy[slot])
        {
            return Err(format!(
                "refusing to change the legacy effect values of {}",
                guid
            ));
        }
    }

    let fx_key = endpoint_fx_registry_path(guid);
    reg.ensure_key(&fx_key).map_err(|e| {
        format!(
            "could not open the effect properties of {}: {}",
            guid, e
        )
    })?;

    for slot in 0..FX_SLOT_COUNT {
        if before.single[slot] != after.single[slot] {
            let name = fx_property_value_name(FX_PROPERTY_SET, SINGLE_PIDS[slot]);
            apply_single_reg(reg, &fx_key, &name, &after.single[slot])
                .map_err(|e| format!("could not write {}: {}", name, e))?;
        }
    }
    for slot in 0..FX_LEGACY_COUNT {
        if before.legacy[slot] != after.legacy[slot] {
            let name = fx_property_value_name(FX_PROPERTY_SET, LEGACY_PIDS[slot]);
            apply_single_reg(reg, &fx_key, &name, &after.legacy[slot])
                .map_err(|e| format!("could not write {}: {}", name, e))?;
        }
    }
    for slot in 0..FX_SLOT_COUNT {
        let as_sz = wants_sz(after, slot);
        if before.composite[slot] != after.composite[slot] || as_sz != wants_sz(before, slot) {
            let name = fx_property_value_name(FX_PROPERTY_SET, COMPOSITE_PIDS[slot]);
            apply_list_reg(reg, &fx_key, &name, &after.composite[slot], as_sz)
                .map_err(|e| format!("could not write {}: {}", name, e))?;
        }
        if before.modes[slot] != after.modes[slot] {
            let name = fx_property_value_name(MODE_PROPERTY_SET, MODE_PIDS[slot]);
            apply_list_reg(reg, &fx_key, &name, &after.modes[slot], false)
                .map_err(|e| format!("could not write {}: {}", name, e))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apo::{
        plan_attach, plan_detach, plan_suspend_apo, ApoSlot, EQUALIZER_APO_CLSIDS, FX_SLOT_EFX,
        FX_SLOT_MFX, FX_SLOT_SFX, WINDOWS_DEFAULT_APO_CLSIDS,
    };

    #[test]
    fn multi_sz_codec_handles_odd_bytes_unterminated_tail_and_padding() {
        // Two valid entries + extra padding after the double NUL terminator + 1 stray odd byte
        let mut raw = encode_multi_sz_bytes(&["{AA}".to_string(), "{BB}".to_string()]);
        raw.extend_from_slice(&encode_sz_bytes("{PADDING_IGNORED}"));
        raw.push(0xFF); // odd trailing byte
        let decoded = decode_multi_sz(&raw);
        assert_eq!(decoded, vec!["{AA}".to_string(), "{BB}".to_string()]);

        // Unterminated single entry (no NUL at all)
        let mut unterminated = Vec::new();
        for u in "{UNTERMINATED}".encode_utf16() {
            unterminated.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(
            decode_multi_sz(&unterminated),
            vec!["{UNTERMINATED}".to_string()]
        );
    }

    #[test]
    fn json_scanner_and_fx_values_roundtrip_refuse_unknown_keys_and_control_chars() {
        let mut v = EndpointFxValues::default();
        v.single[FX_SLOT_SFX] = Some("Vendor \"Quote\" \\ Slash\nLine\u{0007}".to_string());
        v.composite[FX_SLOT_EFX] = Some(vec![SETUP_ENGINE_CLSID.to_string()]);
        v.composite_was_sz[FX_SLOT_MFX] = true;

        let json = fx_values_to_json(&v);
        assert!(json.contains("\\\"Quote\\\""));
        assert!(json.contains("\\u0007"));

        let parsed = fx_values_from_json(&json).expect("valid backup JSON");
        assert_eq!(parsed, v);

        // Unknown key or missing `compositeWasSz` must be refused
        let missing_was_sz = json.replace(",\"compositeWasSz\":[false,true,false]", "");
        assert!(fx_values_from_json(&missing_was_sz).is_none());

        // Raw control character inside string must be refused
        let raw_ctrl = json.replace("\\n", "\n");
        assert!(fx_values_from_json(&raw_ctrl).is_none());
    }

    #[test]
    fn com_registration_writes_two_canonical_keys_and_removes_misplaced_record() {
        let mut reg = InMemoryRegistryHive::new();
        // Seed a legacy misplaced record under CurrentVersion\Audio\AudioProcessingObjects
        reg.set_string(&misplaced_apo_record_path(), "FriendlyName", "Old")
            .unwrap();
        assert!(reg.key_exists(&misplaced_apo_record_path()).unwrap());

        let dll = "C:\\Program Files\\FluidEQ Engine\\FluidEQ-Engine.dll";
        register_engine(&mut reg, dll).expect("register_engine succeeds");
        assert!(!reg.key_exists(&misplaced_apo_record_path()).unwrap());
        assert!(apo_record_present(&reg));
        assert_eq!(registered_dll_path(&reg), dll);

        assert!(!unsigned_effects_enabled(&reg));
        enable_unsigned_effects(&mut reg).expect("enable_unsigned_effects succeeds");
        assert!(unsigned_effects_enabled(&reg));

        unregister_engine(&mut reg).expect("unregister_engine succeeds");
        assert!(!apo_record_present(&reg));
        assert_eq!(registered_dll_path(&reg), "");
        // DisableProtectedAudioDG is intentionally preserved on uninstall
        assert!(unsigned_effects_enabled(&reg));
    }

    #[test]
    fn registry_fx_values_coerces_vendor_sz_composite_and_guards_vendor_single_slots() {
        let mut reg = InMemoryRegistryHive::new();
        let guid = "{11111111-2222-3333-4444-555555555555}";
        let fx_key = endpoint_fx_registry_path(guid);
        let vendor_clsid = "{AA000000-0000-0000-0000-000000000001}";

        // Vendor wrote a REG_SZ (not REG_MULTI_SZ) into composite EFX (pid 15) and SFX single (pid 5)
        let efx_comp_name = fx_property_value_name(FX_PROPERTY_SET, COMPOSITE_PIDS[FX_SLOT_EFX]);
        let sfx_single_name = fx_property_value_name(FX_PROPERTY_SET, SINGLE_PIDS[FX_SLOT_SFX]);
        reg.set_string(&fx_key, &efx_comp_name, vendor_clsid)
            .unwrap();
        reg.set_string(&fx_key, &sfx_single_name, vendor_clsid)
            .unwrap();

        let original = read_fx_values(&reg, guid).expect("read_fx_values succeeds");
        assert_eq!(
            original.composite[FX_SLOT_EFX],
            Some(vec![vendor_clsid.to_string()])
        );
        assert!(original.composite_was_sz[FX_SLOT_EFX]);

        // Attach FluidEQ to EFX: composite_was_sz becomes false and writes REG_MULTI_SZ
        let attached = plan_attach(&original, SETUP_ENGINE_CLSID, ApoSlot::Efx);
        write_fx_values(&mut reg, guid, &original, &attached.after)
            .expect("write_fx_values attach succeeds");
        let raw_after_attach = reg
            .query_value(&fx_key, &efx_comp_name)
            .unwrap()
            .expect("present");
        assert_eq!(raw_after_attach.value_type, REG_MULTI_SZ_TYPE);

        // Detach FluidEQ against original backup: restores single-entry list back as REG_SZ!
        let detached = plan_detach(&attached.after, &original, SETUP_ENGINE_CLSID);
        assert!(detached.after.composite_was_sz[FX_SLOT_EFX]);
        write_fx_values(&mut reg, guid, &attached.after, &detached.after)
            .expect("write_fx_values detach restores REG_SZ");
        let raw_after_detach = reg
            .query_value(&fx_key, &efx_comp_name)
            .unwrap()
            .expect("present");
        assert_eq!(raw_after_detach.value_type, REG_SZ_TYPE);

        // Attempting to overwrite vendor's SFX single (pid 5) with anything must be refused
        let mut illegal = original.clone();
        illegal.single[FX_SLOT_SFX] = Some(SETUP_ENGINE_CLSID.to_string());
        let err = write_fx_values(&mut reg, guid, &original, &illegal).unwrap_err();
        assert!(err.contains("refusing to change the single effect values"));

        // But stripping Equalizer APO from a single slot or writing FluidEQ over Windows default APO is permitted
        let mut with_eq_apo = original.clone();
        with_eq_apo.single[FX_SLOT_MFX] = Some(EQUALIZER_APO_CLSIDS[0].to_string());
        let suspended = plan_suspend_apo(&with_eq_apo);
        assert!(write_fx_values(&mut reg, guid, &with_eq_apo, &suspended.after).is_ok());

        let mut with_win_default = original.clone();
        with_win_default.single[FX_SLOT_MFX] = Some(WINDOWS_DEFAULT_APO_CLSIDS[0].to_string());
        let mut ours_in_mfx = with_win_default.clone();
        ours_in_mfx.single[FX_SLOT_MFX] = Some(SETUP_ENGINE_CLSID.to_string());
        assert!(write_fx_values(&mut reg, guid, &with_win_default, &ours_in_mfx).is_ok());
    }
}
