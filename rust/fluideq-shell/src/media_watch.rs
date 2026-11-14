/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! The transport bar's eyes and hands: `FluidEQ-Media.exe` reads Windows'
//! media sessions and writes one line whenever what the bar draws changes.
//! The app answers with one command a line on the helper's input, and the
//! helper exits when that input closes, however FluidEQ ends.

use crate::system_daemons::{
    parse_media_watch_line, MediaWatchEvent, SystemMediaCover, SystemMediaReading,
};
use serde::Serialize;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

const MEDIA_RESOURCE: &str = "engine-bin/FluidEQ-Media.exe";
/// The app's own sessions are not "the rest of the machine".
const MEDIA_SELF_ID: &str = "com.fluideq.desktop";

#[derive(Debug, Clone, Default, Serialize)]
pub struct MediaSnapshot {
    pub reading: Option<SystemMediaReading>,
    pub cover: Option<SystemMediaCover>,
}

pub struct MediaWatch {
    child: Child,
    stdin: ChildStdin,
    latest: Arc<Mutex<MediaSnapshot>>,
}

impl MediaWatch {
    pub fn spawn(app: &AppHandle) -> Option<Self> {
        let exe = app
            .path()
            .resolve(MEDIA_RESOURCE, tauri::path::BaseDirectory::Resource)
            .ok()?;
        if !exe.is_file() {
            return None;
        }
        let mut command = Command::new(&exe);
        command
            .arg(MEDIA_SELF_ID)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().ok()?;
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;

        let latest = Arc::new(Mutex::new(MediaSnapshot::default()));
        let shared = Arc::clone(&latest);
        let app = app.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Some(event) = parse_media_watch_line(&line) else {
                    continue;
                };
                let Ok(mut snapshot) = shared.lock() else {
                    break;
                };
                match event {
                    MediaWatchEvent::Cleared => *snapshot = MediaSnapshot::default(),
                    MediaWatchEvent::Cover(cover) => snapshot.cover = Some(cover),
                    MediaWatchEvent::Reading(reading) => snapshot.reading = Some(reading),
                }
                let _ = app.emit("media", &*snapshot);
            }
        });

        Some(Self {
            child,
            stdin,
            latest,
        })
    }

    pub fn snapshot(&self) -> MediaSnapshot {
        self.latest
            .lock()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_default()
    }

    pub fn send(&mut self, command: &str) -> Result<(), String> {
        writeln!(self.stdin, "{command}")
            .and_then(|()| self.stdin.flush())
            .map_err(|error| format!("the media helper is not listening: {error}"))
    }
}

impl Drop for MediaWatch {
    fn drop(&mut self) {
        // Closing the input is the helper's own exit path; the kill is only
        // for a helper that is wedged and would otherwise outlive the app.
        let _ = self.child.kill();
    }
}

pub struct MediaWatchState(pub Mutex<Option<MediaWatch>>);

impl MediaWatchState {
    pub fn start(app: &AppHandle) -> Self {
        Self(Mutex::new(MediaWatch::spawn(app)))
    }

    pub fn snapshot(&self) -> MediaSnapshot {
        self.0
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(MediaWatch::snapshot))
            .unwrap_or_default()
    }

    pub fn send(&self, command: &str) -> Result<(), String> {
        let mut guard = self
            .0
            .lock()
            .map_err(|_| String::from("the media helper could not be reached"))?;
        let watch = guard
            .as_mut()
            .ok_or_else(|| String::from("the media helper is not installed"))?;
        watch.send(command)
    }
}
