/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! The native output watcher, so a headset plugged in while the window is in
//! the tray still moves the EQ onto its own profile. The helper only says
//! "outputs"; the app re-reads the default endpoint through the engine helper
//! it already trusts. A build without the watcher staged simply does nothing:
//! the window still reconciles on focus.

use std::io::{BufRead, BufReader};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use tauri::Manager;

const OUTPUT_WATCH_RESOURCE: &str = "engine-bin/FluidEQ-Outputs.exe";
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Starts the watcher and calls `on_outputs` after every announced change.
pub fn spawn(app: tauri::AppHandle, on_outputs: fn(&tauri::AppHandle)) {
    let Ok(path) = app.path().resolve(
        OUTPUT_WATCH_RESOURCE,
        tauri::path::BaseDirectory::Resource,
    ) else {
        return;
    };
    if !path.is_file() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("fluideq-output-watch".to_string())
        .spawn(move || {
            let mut command = Command::new(&path);
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            // The helper is a console program; without this Windows gives it a
            // visible console window the moment the app starts.
            #[cfg(windows)]
            command.creation_flags(CREATE_NO_WINDOW);
            let Ok(mut child) = command.spawn() else {
                return;
            };
            // Held for the life of the reader: closing it is how the helper
            // is told the app is gone.
            let _stdin = child.stdin.take();
            let Some(stdout) = child.stdout.take() else {
                return;
            };
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if line.trim() == "outputs" {
                    on_outputs(&app);
                }
            }
            let _ = child.wait();
        });
}
