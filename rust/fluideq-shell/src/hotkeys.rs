/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Global hotkeys on their own Win32 message-loop thread.
//!
//! They are registered with a null window, which binds them to the calling
//! thread's queue rather than to a window that may be hidden in the tray. A
//! registration another program already holds fails individually and is
//! remembered, so the window can say which shortcut is unavailable instead of
//! leaving it silently dead.

use std::sync::{Mutex, OnceLock};

const BYPASS_ID: i32 = 1;
const VOLUME_UP_ID: i32 = 2;
const VOLUME_DOWN_ID: i32 = 3;
const VOLUME_STEP: f64 = 0.05;

static FAILURES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

fn failure_slot() -> &'static Mutex<Vec<String>> {
    FAILURES.get_or_init(|| Mutex::new(Vec::new()))
}

fn record_failure(message: String) {
    if let Ok(mut failures) = failure_slot().lock() {
        failures.push(message);
    }
}

/// Shortcuts that could not be registered, with the reason a user can act on.
pub fn failures() -> Vec<String> {
    failure_slot()
        .lock()
        .map(|failures| failures.clone())
        .unwrap_or_default()
}

fn nudge_volume(app: &tauri::AppHandle, delta: f64) {
    let Ok(current) = crate::audio_endpoint::get_master_volume() else {
        return;
    };
    let wanted = (current + delta).clamp(0.0, 1.0);
    if let Ok(volume) = crate::audio_endpoint::set_master_volume(wanted) {
        let _ = tauri::Emitter::emit(app, "volume", volume);
    }
}

#[cfg(target_os = "windows")]
pub fn spawn(app: tauri::AppHandle, on_bypass: fn(&tauri::AppHandle)) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, TranslateMessage, MSG, WM_HOTKEY,
    };

    let thread = std::thread::Builder::new()
        .name("fluideq-hotkeys".to_string())
        .spawn(move || unsafe {
            let modifiers = MOD_CONTROL | MOD_ALT | MOD_NOREPEAT;
            // Ctrl+Alt is deliberately uncommon: a global Win+ combination
            // would collide with the shell, and a bare function key with the
            // games this app is often running beside.
            let bindings = [
                (BYPASS_ID, "Ctrl+Alt+B", 0x42u32),
                (VOLUME_UP_ID, "Ctrl+Alt+Up", 0x26u32),
                (VOLUME_DOWN_ID, "Ctrl+Alt+Down", 0x28u32),
            ];
            let mut any = false;
            for (id, name, key) in bindings {
                if RegisterHotKey(HWND::default(), id, modifiers, key).is_ok() {
                    any = true;
                } else {
                    record_failure(format!("{name} is already taken"));
                }
            }
            if !any {
                return;
            }
            let mut message = MSG::default();
            while GetMessageW(&mut message, HWND::default(), 0, 0).0 != 0 {
                if message.message == WM_HOTKEY {
                    match message.wParam.0 as i32 {
                        BYPASS_ID => on_bypass(&app),
                        VOLUME_UP_ID => nudge_volume(&app, VOLUME_STEP),
                        VOLUME_DOWN_ID => nudge_volume(&app, -VOLUME_STEP),
                        _ => {}
                    }
                }
            }
        });
    if let Err(error) = thread {
        record_failure(format!("could not start the hotkey listener: {error}"));
    }
}

#[cfg(not(target_os = "windows"))]
pub fn spawn(_app: tauri::AppHandle, _on_bypass: fn(&tauri::AppHandle)) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_registration_is_reported_once_per_shortcut() {
        let before = failures().len();
        record_failure("Ctrl+Alt+B is already taken".to_string());
        let after = failures();
        assert_eq!(after.len(), before + 1);
        assert_eq!(after.last().unwrap(), "Ctrl+Alt+B is already taken");

        // The positive control: reading twice does not duplicate the record.
        assert_eq!(failures().len(), after.len());
    }
}
