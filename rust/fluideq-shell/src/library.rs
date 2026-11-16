/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! The Library page's state: the folders the owner chose, the tracks the last
//! scan found, and the cover the page is looking at.
//!
//! A scan runs on its own thread and publishes a snapshot when it starts, as
//! it finds tracks, and when it ends, so the page never waits on a disk. The
//! folder list lives in `library.json` beside the other state; a folder that
//! disappeared is reported by the scan, not deleted from the list behind the
//! owner's back.

use fluideq_state::library::{self, CoverArt, Track};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

pub const EVENT: &str = "library";
const LIBRARY_FILE: &str = "library.json";
const PROGRESS_EVERY: usize = 100;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibrarySnapshot {
    pub folders: Vec<String>,
    pub tracks: Vec<Track>,
    pub scanning: bool,
    pub scanned: usize,
    pub skipped: usize,
    pub problem: Option<String>,
}

pub struct LibraryState(Arc<Library>);

struct Library {
    data_dir: PathBuf,
    roots: Mutex<Vec<PathBuf>>,
    snapshot: Arc<Mutex<LibrarySnapshot>>,
    scanning: Arc<AtomicBool>,
    pending: Arc<AtomicBool>,
}

#[derive(Serialize, Deserialize, Default)]
struct LibraryFile {
    folders: Vec<String>,
}

impl LibraryState {
    pub fn new(data_dir: PathBuf) -> Self {
        let roots = load_roots(&data_dir);
        let folders = roots
            .iter()
            .map(|root| root.to_string_lossy().to_string())
            .collect();
        Self(Arc::new(Library {
            data_dir,
            roots: Mutex::new(roots),
            snapshot: Arc::new(Mutex::new(LibrarySnapshot {
                folders,
                ..LibrarySnapshot::default()
            })),
            scanning: Arc::new(AtomicBool::new(false)),
            pending: Arc::new(AtomicBool::new(false)),
        }))
    }

    pub fn snapshot(&self) -> LibrarySnapshot {
        self.0
            .snapshot
            .lock()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_default()
    }

    pub fn add_folder(
        &self,
        app: &AppHandle,
        folder: &str,
    ) -> Result<LibrarySnapshot, String> {
        let path = folder_path(folder)?;
        {
            let mut roots = self
                .0
                .roots
                .lock()
                .map_err(|_| String::from("the library is not reachable"))?;
            if roots.iter().any(|root| same_path(root, &path)) {
                return Err(String::from("that folder is already in the library"));
            }
            roots.push(path);
            save_roots(&self.0.data_dir, &roots)?;
        }
        self.publish_folders(app);
        self.scan(app);
        Ok(self.snapshot())
    }

    pub fn remove_folder(
        &self,
        app: &AppHandle,
        folder: &str,
    ) -> Result<LibrarySnapshot, String> {
        {
            let mut roots = self
                .0
                .roots
                .lock()
                .map_err(|_| String::from("the library is not reachable"))?;
            let before = roots.len();
            roots.retain(|root| !same_path(root, Path::new(folder.trim())));
            if roots.len() == before {
                return Err(String::from("that folder is not in the library"));
            }
            save_roots(&self.0.data_dir, &roots)?;
            if roots.is_empty() {
                if let Ok(mut snapshot) = self.0.snapshot.lock() {
                    snapshot.tracks.clear();
                    snapshot.scanned = 0;
                    snapshot.skipped = 0;
                    snapshot.problem = None;
                }
            }
        }
        self.publish_folders(app);
        if !self.0.roots.lock().map(|roots| roots.is_empty()).unwrap_or(true) {
            self.scan(app);
        } else {
            publish(&self.0.snapshot, app);
        }
        Ok(self.snapshot())
    }

    /// Starts a scan, or asks the running one to go again when the folders
    /// changed while it was walking them.
    pub fn scan(&self, app: &AppHandle) {
        if self.0.scanning.swap(true, Ordering::AcqRel) {
            self.0.pending.store(true, Ordering::Release);
            return;
        }
        if let Ok(mut snapshot) = self.0.snapshot.lock() {
            snapshot.scanning = true;
            snapshot.scanned = 0;
            snapshot.problem = None;
        }
        publish(&self.0.snapshot, app);
        spawn_worker(Arc::clone(&self.0), app.clone());
    }

    pub fn cover(&self, path: &str) -> Result<Option<CoverArt>, String> {
        let allowed = self
            .0
            .snapshot
            .lock()
            .map(|snapshot| snapshot.tracks.iter().any(|track| track.path == path))
            .unwrap_or(false);
        if !allowed {
            return Ok(None);
        }
        Ok(library::read_cover(Path::new(path)))
    }

    fn publish_folders(&self, app: &AppHandle) {
        let folders = self
            .0
            .roots
            .lock()
            .map(|roots| {
                roots
                    .iter()
                    .map(|root| root.to_string_lossy().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let Ok(mut snapshot) = self.0.snapshot.lock() {
            snapshot.folders = folders;
        }
        publish(&self.0.snapshot, app);
    }
}

fn spawn_worker(library: Arc<Library>, app: AppHandle) {
    let _ = std::thread::Builder::new()
        .name(String::from("fluideq-library-scan"))
        .spawn(move || {
            loop {
                library.pending.store(false, Ordering::Release);
                let roots = library
                    .roots
                    .lock()
                    .map(|roots| roots.clone())
                    .unwrap_or_default();
                let snapshot = Arc::clone(&library.snapshot);
                let app_progress = app.clone();
                let report = library::scan_folders(&roots, |count| {
                    if count % PROGRESS_EVERY != 0 {
                        return;
                    }
                    if let Ok(mut state) = snapshot.lock() {
                        state.scanned = count;
                    }
                    publish(&snapshot, &app_progress);
                });
                if let Ok(mut state) = library.snapshot.lock() {
                    state.tracks = report.tracks;
                    state.scanned = state.tracks.len();
                    state.skipped = report.skipped;
                    state.problem = report.problem;
                }
                publish(&library.snapshot, &app);
                if !library.pending.load(Ordering::Acquire) {
                    break;
                }
            }
            library.scanning.store(false, Ordering::Release);
            if let Ok(mut state) = library.snapshot.lock() {
                state.scanning = false;
            }
            publish(&library.snapshot, &app);
            if library.pending.swap(false, Ordering::AcqRel)
                && !library.scanning.swap(true, Ordering::AcqRel)
            {
                if let Ok(mut state) = library.snapshot.lock() {
                    state.scanning = true;
                    state.scanned = 0;
                    state.problem = None;
                }
                publish(&library.snapshot, &app);
                spawn_worker(library, app);
            }
        });
}

fn publish(snapshot: &Arc<Mutex<LibrarySnapshot>>, app: &AppHandle) {
    if let Ok(state) = snapshot.lock() {
        let _ = app.emit(EVENT, state.clone());
    }
}

fn folder_path(folder: &str) -> Result<PathBuf, String> {
    let trimmed = folder.trim().trim_matches('"');
    if trimmed.is_empty() {
        return Err(String::from("type a folder path first"));
    }
    let path = PathBuf::from(trimmed);
    if !path.is_dir() {
        return Err(format!("{trimmed} is not a folder"));
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("{trimmed} could not be read: {error}"))?;
    Ok(strip_extended(canonical))
}

fn strip_extended(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path
}

fn same_path(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

fn load_roots(data_dir: &Path) -> Vec<PathBuf> {
    let text = match std::fs::read_to_string(data_dir.join(LIBRARY_FILE)) {
        Ok(text) => text,
        Err(_) => return Vec::new(),
    };
    let file: LibraryFile = serde_json::from_str(&text).unwrap_or_default();
    file.folders
        .into_iter()
        .filter(|folder| !folder.trim().is_empty())
        .map(PathBuf::from)
        .collect()
}

fn save_roots(data_dir: &Path, roots: &[PathBuf]) -> Result<(), String> {
    let file = LibraryFile {
        folders: roots
            .iter()
            .map(|root| root.to_string_lossy().to_string())
            .collect(),
    };
    let json = serde_json::to_string_pretty(&file)
        .map_err(|error| format!("the library could not be written: {error}"))?;
    std::fs::create_dir_all(data_dir)
        .map_err(|error| format!("the library folder could not be made: {error}"))?;
    let temporary = data_dir.join(format!("{LIBRARY_FILE}.tmp"));
    std::fs::write(&temporary, json)
        .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, data_dir.join(LIBRARY_FILE))
        .map_err(|error| format!("could not replace {LIBRARY_FILE}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_extended_windows_path_is_shown_without_the_prefix() {
        assert_eq!(
            strip_extended(PathBuf::from(r"\\?\C:\Music")),
            PathBuf::from(r"C:\Music")
        );
        assert_eq!(
            strip_extended(PathBuf::from(r"\\?\UNC\server\share")),
            PathBuf::from(r"\\server\share")
        );
        assert_eq!(
            strip_extended(PathBuf::from(r"C:\Music")),
            PathBuf::from(r"C:\Music")
        );
    }

    #[test]
    fn folder_paths_compare_without_case_on_windows() {
        assert!(same_path(Path::new(r"C:\Music"), Path::new(r"c:\music")));
        assert!(!same_path(Path::new(r"C:\Music"), Path::new(r"D:\Music")));
    }

    #[test]
    fn a_missing_folder_is_refused_with_the_path_in_the_message() {
        let path = std::env::temp_dir().join("fluideq-library-does-not-exist");
        let message = folder_path(path.to_string_lossy().as_ref()).unwrap_err();
        assert!(message.contains("is not a folder"));
    }
}
