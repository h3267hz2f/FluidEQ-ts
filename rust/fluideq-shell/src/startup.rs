/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! The per-user startup entry: one `Run` value, written only when the user
//! turns it on. It points at this exact executable, so a moved build reads as
//! off rather than silently launching a path that no longer exists.

#[cfg(target_os = "windows")]
mod platform {
    use crate::system_daemons::win32_daemon_ffi as ffi;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE_NAME: &str = "FluidEQ";
    const KEY_WRITE: u32 = 0x20006;
    const REG_SZ: u32 = 1;
    const ERROR_FILE_NOT_FOUND: i32 = 2;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn command_line() -> Result<String, String> {
        let exe = std::env::current_exe()
            .map_err(|error| format!("could not find this executable: {error}"))?;
        Ok(format!("\"{}\"", exe.display()))
    }

    fn open(access: u32) -> Result<ffi::Hkey, String> {
        let path = wide(RUN_KEY);
        let mut key: ffi::Hkey = 0;
        let status = unsafe {
            ffi::RegOpenKeyExW(ffi::HKEY_CURRENT_USER, path.as_ptr(), 0, access, &mut key)
        };
        if status != 0 {
            return Err(format!(
                "could not open the startup registry key (error {status})"
            ));
        }
        Ok(key)
    }

    pub fn enabled() -> Result<bool, String> {
        let Ok(key) = open(ffi::KEY_READ) else {
            return Ok(false);
        };
        let name = wide(VALUE_NAME);
        let mut kind = 0u32;
        let mut size = 0u32;
        let status = unsafe {
            ffi::RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                std::ptr::null_mut(),
                &mut size,
            )
        };
        if status != 0 || kind != REG_SZ || size < 2 {
            unsafe {
                ffi::RegCloseKey(key);
            }
            return Ok(false);
        }
        let mut buffer = vec![0u8; size as usize];
        let status = unsafe {
            ffi::RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                buffer.as_mut_ptr(),
                &mut size,
            )
        };
        unsafe {
            ffi::RegCloseKey(key);
        }
        if status != 0 {
            return Err(format!(
                "could not read the startup entry (error {status})"
            ));
        }
        let units: Vec<u16> = buffer[..size as usize]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let saved = String::from_utf16_lossy(&units);
        Ok(saved.trim_end_matches('\0') == command_line()?)
    }

    pub fn set(enable: bool) -> Result<(), String> {
        let key = open(KEY_WRITE)?;
        let name = wide(VALUE_NAME);
        let status = if enable {
            let command = command_line()?;
            let data: Vec<u8> = command
                .encode_utf16()
                .chain(std::iter::once(0))
                .flat_map(u16::to_le_bytes)
                .collect();
            unsafe {
                ffi::RegSetValueExW(
                    key,
                    name.as_ptr(),
                    0,
                    REG_SZ,
                    data.as_ptr(),
                    data.len() as u32,
                )
            }
        } else {
            unsafe { ffi::RegDeleteValueW(key, name.as_ptr()) }
        };
        unsafe {
            ffi::RegCloseKey(key);
        }
        if status != 0 && (enable || status != ERROR_FILE_NOT_FOUND) {
            return Err(format!(
                "could not {} the startup entry (error {status})",
                if enable { "write" } else { "remove" }
            ));
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
pub use platform::{enabled, set};

#[cfg(not(target_os = "windows"))]
pub fn enabled() -> Result<bool, String> {
    Ok(false)
}

#[cfg(not(target_os = "windows"))]
pub fn set(_enable: bool) -> Result<(), String> {
    Err("startup entries are only written on Windows".to_string())
}
