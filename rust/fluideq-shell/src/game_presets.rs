/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Game presets: which curve a game gets when it comes to the front.
//!
//! The rules are a small JSON file beside the state (`games.json`), not part
//! of the snapshot: they belong to the machine, not to one curve, and a
//! malformed file must never cost the user their EQ. `foreground-watch` says
//! which program Windows put in front (`native/foreground-watch/src/main.cpp`);
//! this module matches the executable against the rules, and the shell owns
//! what "apply a preset" and "put the old curve back" mean.
//!
//! The helper is held (`hold <pid>`) for as long as the game runs, so
//! alt-tabbing out does not end the game's sound; it is told `hold 0` when
//! the process ends. Nothing polls, and the reader thread ends when the
//! helper exits.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};

use crate::system_daemons::{
    parse_foreground_watch_line, ForegroundProgramRecord, ForegroundWatchEvent,
};

const RULES_FILE: &str = "games.json";
const WATCH_RESOURCE: &str = "engine-bin/FluidEQ-Games.exe";
const MAX_RULES: usize = 64;
const MAX_EXECUTABLE_CHARS: usize = 260;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameRule {
    pub executable: String,
    pub preset: String,
}

/// The executable's own file name, lowercased. A rule copied from a shortcut
/// may carry a whole path, and Windows does not care about case.
pub fn executable_name(raw: &str) -> String {
    raw.rsplit(['\\', '/'])
        .next()
        .unwrap_or(raw)
        .trim()
        .to_ascii_lowercase()
}

pub fn validate_rule(executable: &str, preset: &str) -> Result<GameRule, String> {
    let name = executable_name(executable);
    if name.is_empty() {
        return Err("the game executable cannot be empty".to_string());
    }
    if name.chars().count() > MAX_EXECUTABLE_CHARS {
        return Err(format!(
            "the executable name is longer than {MAX_EXECUTABLE_CHARS} characters"
        ));
    }
    if name.contains(['\t', '\n', '\r']) {
        return Err("the executable name cannot hold a tab or a line break".to_string());
    }
    let preset = preset.trim();
    if preset.is_empty() {
        return Err("choose a preset for the game".to_string());
    }
    Ok(GameRule {
        executable: name,
        preset: preset.to_string(),
    })
}

/// Replaces the rule for the same executable, or adds it. Kept separate from
/// the file so the merge rules are testable without a disk.
fn with_rule(mut rules: Vec<GameRule>, rule: GameRule) -> Result<Vec<GameRule>, String> {
    if let Some(existing) = rules
        .iter_mut()
        .find(|existing| existing.executable == rule.executable)
    {
        existing.preset = rule.preset;
    } else {
        if rules.len() >= MAX_RULES {
            return Err(format!("there are already {MAX_RULES} game presets"));
        }
        rules.push(rule);
    }
    rules.sort_by(|left, right| left.executable.cmp(&right.executable));
    Ok(rules)
}

fn without_rule(mut rules: Vec<GameRule>, executable: &str) -> Vec<GameRule> {
    let name = executable_name(executable);
    rules.retain(|rule| rule.executable != name);
    rules
}

pub fn load(data_dir: &Path) -> Vec<GameRule> {
    let Ok(bytes) = std::fs::read(data_dir.join(RULES_FILE)) else {
        return Vec::new();
    };
    let mut rules: Vec<GameRule> = serde_json::from_slice(&bytes).unwrap_or_default();
    for rule in &mut rules {
        rule.executable = executable_name(&rule.executable);
        rule.preset = rule.preset.trim().to_string();
    }
    rules.retain(|rule| !rule.executable.is_empty() && !rule.preset.is_empty());
    rules.truncate(MAX_RULES);
    rules.sort_by(|left, right| left.executable.cmp(&right.executable));
    rules
}

pub fn save(data_dir: &Path, rules: &[GameRule]) -> Result<(), String> {
    std::fs::create_dir_all(data_dir)
        .map_err(|error| format!("could not create {}: {error}", data_dir.display()))?;
    let json = serde_json::to_vec_pretty(rules)
        .map_err(|error| format!("could not encode the game presets: {error}"))?;
    let temporary = data_dir.join("games.json.tmp");
    std::fs::write(&temporary, json)
        .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, data_dir.join(RULES_FILE))
        .map_err(|error| format!("could not replace {RULES_FILE}: {error}"))
}

pub fn upsert(data_dir: &Path, executable: &str, preset: &str) -> Result<Vec<GameRule>, String> {
    let rule = validate_rule(executable, preset)?;
    let rules = with_rule(load(data_dir), rule)?;
    save(data_dir, &rules)?;
    Ok(rules)
}

pub fn remove(data_dir: &Path, executable: &str) -> Result<Vec<GameRule>, String> {
    let rules = without_rule(load(data_dir), executable);
    save(data_dir, &rules)?;
    Ok(rules)
}

/// The rule that wants this program, if any. The executable's file name is
/// the identity: two installs of the same game run the same program name.
pub fn matching_rule<'a>(
    rules: &'a [GameRule],
    record: &ForegroundProgramRecord,
) -> Option<&'a GameRule> {
    let name = executable_name(&record.executable_path);
    if name.is_empty() {
        return None;
    }
    rules.iter().find(|rule| rule.executable == name)
}

/// The helper beside the app's bundled engine resources, when the build
/// staged it.
pub fn watcher_path(app: &AppHandle) -> Option<PathBuf> {
    let path = app
        .path()
        .resolve(WATCH_RESOURCE, tauri::path::BaseDirectory::Resource)
        .ok()?;
    path.is_file().then_some(path)
}

pub fn watcher_available(app: &AppHandle) -> bool {
    watcher_path(app).is_some()
}

pub struct Watch {
    child: Child,
    active: Arc<Mutex<Option<u32>>>,
}

impl Watch {
    pub fn holds_game(&self) -> bool {
        self.active
            .lock()
            .map(|slot| slot.is_some())
            .unwrap_or(false)
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts the helper for `rules`, calling `on_engage` when a mapped program
/// comes to the front and `on_release` when the held one ends.
pub fn spawn(
    app: AppHandle,
    rules: Vec<GameRule>,
    on_engage: fn(&AppHandle, &str) -> bool,
    on_release: fn(&AppHandle),
) -> Option<Watch> {
    let path = watcher_path(&app)?;
    let mut child = Command::new(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let active = Arc::new(Mutex::new(None));
    let active_for_thread = Arc::clone(&active);
    std::thread::Builder::new()
        .name("fluideq-foreground-watch".to_string())
        .spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Some(event) = parse_foreground_watch_line(&line) else {
                    continue;
                };
                match event {
                    ForegroundWatchEvent::Front(record) => {
                        let Ok(mut held) = active_for_thread.lock() else {
                            return;
                        };
                        if held.is_some() {
                            continue;
                        }
                        let Some(rule) = matching_rule(&rules, &record) else {
                            continue;
                        };
                        if !on_engage(&app, &rule.preset) {
                            continue;
                        }
                        if stdin
                            .write_all(format!("hold {}\n", record.pid).as_bytes())
                            .is_err()
                        {
                            drop(held);
                            on_release(&app);
                            continue;
                        }
                        *held = Some(record.pid);
                    }
                    ForegroundWatchEvent::Gone { pid } => {
                        let Ok(mut held) = active_for_thread.lock() else {
                            return;
                        };
                        if *held != Some(pid) {
                            continue;
                        }
                        *held = None;
                        drop(held);
                        on_release(&app);
                        let _ = stdin.write_all(b"hold 0\n");
                    }
                    ForegroundWatchEvent::Open(_) | ForegroundWatchEvent::Listed => {}
                }
            }
            // The helper ended, so whatever it held ended with it.
            let stolen = active_for_thread
                .lock()
                .ok()
                .and_then(|mut slot| slot.take());
            if stolen.is_some() {
                on_release(&app);
            }
        })
        .ok()?;
    Some(Watch { child, active })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(path: &str) -> ForegroundProgramRecord {
        ForegroundProgramRecord {
            pid: 42,
            rect: None,
            description: String::new(),
            executable_path: path.to_string(),
        }
    }

    #[test]
    fn an_executable_rule_keeps_only_the_file_name() {
        assert_eq!(executable_name(r"C:\Games\CS2\cs2.exe"), "cs2.exe");
        assert_eq!(executable_name("  Cyberpunk2077.EXE  "), "cyberpunk2077.exe");
        assert_eq!(executable_name(""), "");
    }

    #[test]
    fn a_rule_needs_a_program_and_a_preset() {
        assert!(validate_rule(" ", "Warm").is_err());
        assert!(validate_rule("cs2.exe", "  ").is_err());
        let rule = validate_rule(r"D:\Steam\cs2.exe", " Tactical ").expect("valid");
        assert_eq!(rule.executable, "cs2.exe");
        assert_eq!(rule.preset, "Tactical");
    }

    #[test]
    fn adding_the_same_game_replaces_its_preset() {
        let rules = with_rule(
            Vec::new(),
            validate_rule("cs2.exe", "Tactical").expect("rule"),
        )
        .expect("added");
        let rules = with_rule(
            rules,
            validate_rule(r"C:\Steam\CS2.EXE", "Cinema").expect("rule"),
        )
        .expect("replaced");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].preset, "Cinema");
    }

    #[test]
    fn a_rule_matches_the_programs_own_name_only() {
        let rules = vec![validate_rule("cs2.exe", "Tactical").expect("rule")];
        assert!(matching_rule(&rules, &record(r"C:\Steam\cs2.exe")).is_some());
        assert!(matching_rule(&rules, &record(r"C:\Steam\steam.exe")).is_none());
        assert!(matching_rule(&rules, &record("")).is_none());
    }

    #[test]
    fn removing_a_game_forgets_it() {
        let rules = vec![
            validate_rule("cs2.exe", "Tactical").expect("rule"),
            validate_rule("bg3.exe", "Cinema").expect("rule"),
        ];
        let rules = without_rule(rules, "CS2.EXE");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].executable, "bg3.exe");
    }
}
