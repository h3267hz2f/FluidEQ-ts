/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! The activation bridge to the native engine setup helper.
//!
//! The Tauri installer now carries `engine-bin/FluidEQ-Engine-Setup.exe`,
//! `FluidEQ-Engine.dll`, the DSP host, and the MSVC runtime beside them
//! (`stage-native-engine.mjs`). The helper is the only part of FluidEQ that
//! may write the audio registry, and it runs the elevated half itself: this
//! module either captures stdout for the commands that need no administrator
//! (`status`), or starts `install` with the `runas` verb and reads the
//! `last-setup.json` the elevated helper leaves behind.
//!
//! No code here runs automatically. The user decides when the engine is
//! attached and when Windows audio restarts.

use std::path::PathBuf;

use serde_json::Value;
use tauri::{AppHandle, Manager};

/// The setup helper, bundled under this resource directory.
const ENGINE_SETUP_RESOURCE: &str = "engine-bin/FluidEQ-Engine-Setup.exe";
/// `%ProgramData%\FluidEQ\engine\last-setup.json`, written by the elevated half.
const ENGINE_RESULT_RELATIVE: &str = "FluidEQ\\engine\\last-setup.json";

/// The setup executable beside this app's bundled engine resources.
pub fn engine_setup_exe(app: &AppHandle) -> Result<PathBuf, String> {
    let setup_path = app
        .path()
        .resolve(ENGINE_SETUP_RESOURCE, tauri::path::BaseDirectory::Resource)
        .map_err(|error| format!("could not find {ENGINE_SETUP_RESOURCE}: {error}"))?;
    if !setup_path.is_file() {
        return Err(format!(
            "{ENGINE_SETUP_RESOURCE} is not installed beside this app; run the engine staging build before activation"
        ));
    }
    Ok(setup_path)
}

/// Parse the helper's one-line JSON answer.
///
/// A line of diagnostic text may precede it, and the JSON itself has nested
/// objects, so the first `{` and the last `}` are taken. Anything else is a
/// refusal: the caller must never guess what a half-unreadable status says.
pub fn parse_setup_json(raw: &str) -> Result<Value, String> {
    let trimmed = raw.trim();
    let start = trimmed
        .find('{')
        .ok_or_else(|| "the engine setup helper returned no JSON".to_string())?;
    let end = trimmed
        .rfind('}')
        .ok_or_else(|| "the engine setup helper returned no JSON".to_string())?;
    if end < start {
        return Err("the engine setup helper returned truncated JSON".to_string());
    }
    serde_json::from_str::<Value>(&trimmed[start..=end])
        .map_err(|error| format!("the engine setup helper returned unreadable JSON: {error}"))
}

fn error_from_json(json: &Value) -> Option<String> {
    json.get("error")
        .and_then(Value::as_str)
        .filter(|error| !error.is_empty())
        .map(str::to_string)
}

fn require_success(json: &Value, what: &str) -> Result<(), String> {
    if let Some(error) = error_from_json(json) {
        return Err(format!("{what} failed: {error}"));
    }
    if json.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(format!("{what} failed"));
    }
    Ok(())
}

/// The lines of `engine.log` that explain the current audio path.
///
/// The engine writes its own verdict whenever it reloads: which files it read,
/// whether it is processing or passing the endpoint through, and why. That is
/// the only place a "written but never instantiated" output says what Windows
/// did with it, so the status carries the last few lines to the window.
pub fn diagnosis_lines(text: &str, limit: usize) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| {
            line.starts_with("chain loaded:")
                || line.starts_with("pass-through:")
                || *line == "processing this endpoint"
                || line.starts_with("graph warning:")
                || line.starts_with("could not write this output's status")
        })
        .map(str::to_string)
        .collect();
    let keep_from = lines.len().saturating_sub(limit);
    lines.drain(..keep_from);
    lines
}

/// The tail of `<engine root>\engine.log`, filtered by `diagnosis_lines`.
pub fn log_tail() -> Vec<String> {
    let path = fluideq_state::apo_analysis_and_status::log_path();
    if path.is_empty() {
        return Vec::new();
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => diagnosis_lines(&text, 8),
        Err(_) => Vec::new(),
    }
}

/// The current engine state, without asking for administrator rights.
pub fn status(app: &AppHandle) -> Result<Value, String> {
    let setup = engine_setup_exe(app)?;
    let raw = platform::run_unelevated(&setup, &["status"])?;
    let json = parse_setup_json(&raw)?;
    require_success(&json, "Engine status")?;
    Ok(json)
}

/// Install, attach to every render output, and restart Windows audio.
///
/// This prompts for administrator rights. On success it returns the fresh
/// status so the UI does not have to make a second call and render an older
/// truth.
pub fn install(app: &AppHandle) -> Result<Value, String> {
    let setup = engine_setup_exe(app)?;
    let raw = platform::run_elevated(&setup, &["install", "--attach-all", "--restart-audio"])?;
    let json = parse_setup_json(&raw)?;
    require_success(&json, "Engine install")?;
    status(app)
}

#[cfg(target_os = "windows")]
mod platform {
    use fluideq_state::apo_setup_commands::{
        format_elevated_parameters, ERROR_CANCELLED_WIN32, SETUP_EXIT_DECLINED, SETUP_EXIT_OK,
    };
    use std::iter;
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::Command;
    use std::mem;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const WAIT_OBJECT_0: u32 = 0x0000_0000;

    /// Keep null-terminated UTF-16 alive for the call in which it is used.
    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(iter::once(0)).collect()
    }

    fn win32_error(win32_error: u32) -> String {
        format!("Win32 error {win32_error}")
    }

    /// The path of the setup helper's one-line result for elevated commands.
    fn result_path() -> std::path::PathBuf {
        let program_data =
            std::env::var("ProgramData").unwrap_or_else(|_| String::from("C:\\ProgramData"));
        Path::new(&program_data).join(super::ENGINE_RESULT_RELATIVE)
    }

    /// Run a command that needs no administrator and return stdout.
    pub fn run_unelevated(exe: &Path, args: &[&str]) -> Result<String, String> {
        let output = Command::new(exe)
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|error| format!("could not run {}: {error}", exe.display()))?;
        let code = output.status.code().unwrap_or(-1);
        if code != SETUP_EXIT_OK {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let detail = if !stdout.is_empty() {
                stdout
            } else if !stderr.is_empty() {
                stderr
            } else {
                format!("exit code {code}")
            };
            return Err(detail);
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// `ShellExecuteExW` with the `runas` verb, waiting for the elevated process.
    fn shell_execute_runas(exe: &Path, parameters: Option<&str>) -> Result<Option<u32>, String> {
        let file = wide(&exe.to_string_lossy());
        let parameters_owned = parameters.map(wide);
        let parameters_ptr = parameters_owned.as_deref().unwrap_or(&[0]);
        let directory = exe
            .parent()
            .map(|parent| wide(&parent.to_string_lossy()))
            .unwrap_or_else(|| wide(""));
        let verb = wide("runas");

        let mut information = SHELLEXECUTEINFOW {
            cbSize: mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
            lpVerb: windows::core::PCWSTR(verb.as_ptr()),
            lpFile: windows::core::PCWSTR(file.as_ptr()),
            lpParameters: windows::core::PCWSTR(parameters_ptr.as_ptr()),
            lpDirectory: windows::core::PCWSTR(directory.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };

        // SAFETY: every referenced buffer is alive for the duration of this
        // call, and `information` is the only thing passed by pointer.
        unsafe { ShellExecuteExW(&mut information) }.map_err(|_| {
            let error = unsafe { GetLastError() }.0;
            if error == ERROR_CANCELLED_WIN32 {
                return String::from("administrator rights were declined");
            }
            win32_error(error)
        })?;

        let process: HANDLE = information.hProcess;
        if process.is_invalid() {
            return Err(String::from("the elevated helper did not start"));
        }

        let wait = unsafe { WaitForSingleObject(process, INFINITE) }.0;
        let mut code = 0u32;
        let read_code = unsafe { GetExitCodeProcess(process, &mut code) };
        unsafe {
            let _ = CloseHandle(process);
        }

        if wait != WAIT_OBJECT_0 {
            let error = unsafe { GetLastError() }.0;
            return Err(format!(
                "could not wait for the elevated helper: {}",
                win32_error(error)
            ));
        }
        // A read that failed is not a decline. Saying it was one sent the user
        // to accept a prompt they had already accepted.
        if let Err(error) = read_code {
            return Err(format!(
                "could not read the elevated helper's exit code: {error}"
            ));
        }
        Ok(Some(code))
    }

    /// Run a command that must write registry keys, and return the elevated
    /// half's JSON result document.
    pub fn run_elevated(exe: &Path, args: &[&str]) -> Result<String, String> {
        let owned_args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        let parameters = format_elevated_parameters(&owned_args);
        let code = shell_execute_runas(exe, parameters.as_deref())?
            .unwrap_or(SETUP_EXIT_DECLINED as u32);

        if code == ERROR_CANCELLED_WIN32 || code == SETUP_EXIT_DECLINED as u32 {
            return Err(String::from("administrator rights were declined"));
        }
        if code != SETUP_EXIT_OK as u32 {
            let path = result_path();
            if let Ok(raw) = std::fs::read_to_string(&path) {
                if let Ok(json) = super::parse_setup_json(&raw) {
                    if let Some(error) = super::error_from_json(&json) {
                        return Err(format!(
                            "the elevated helper failed with exit code {code}: {error}"
                        ));
                    }
                }
            }
            return Err(format!("the elevated helper failed with exit code {code}"));
        }

        let path = result_path();
        std::fs::read_to_string(&path).map_err(|error| {
            format!(
                "the elevated helper completed but left no result at {}: {error}",
                path.display()
            )
        })
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use std::path::Path;

    pub fn run_unelevated(exe: &Path, args: &[&str]) -> Result<String, String> {
        let _ = (exe, args);
        Err(String::from("FluidEQ engine activation is Windows-only"))
    }

    pub fn run_elevated(exe: &Path, args: &[&str]) -> Result<String, String> {
        let _ = (exe, args);
        Err(String::from("FluidEQ engine activation is Windows-only"))
    }
}

#[cfg(test)]
mod tests {
    use super::{diagnosis_lines, parse_setup_json, require_success};

    #[test]
    fn only_the_engine_verdicts_are_kept_and_the_tail_is_bounded() {
        let log = "noise\n\
                   chain loaded: files=1 (config.txt) bands=2 preamp=0.0 dB\n\
                   pass-through: FluidEQ is not running\n\
                   graph warning: sample rate changed\n\
                   processing this endpoint\n\
                   more noise\n";
        assert_eq!(
            diagnosis_lines(log, 2),
            vec![
                "graph warning: sample rate changed".to_string(),
                "processing this endpoint".to_string()
            ]
        );

        // The positive control: the reason line the user needs survives even
        // when the file only holds that one.
        assert_eq!(
            diagnosis_lines("pass-through: no config.txt in the configuration directory\n", 8),
            vec!["pass-through: no config.txt in the configuration directory"]
        );
    }

    #[test]
    fn status_json_after_diagnostic_text_parses() {
        let parsed = parse_setup_json(
            "FluidEQ-Engine-Setup status\n{\"installed\":true,\"endpoints\":[]}",
        )
        .expect("parse setup json");
        assert_eq!(parsed["installed"], serde_json::json!(true));
    }

    #[test]
    fn a_status_error_is_a_failure() {
        let parsed = parse_setup_json("{\"error\":\"could not list the outputs\"}")
            .expect("parse setup json");
        assert!(require_success(&parsed, "Engine status").is_err());
    }

    #[test]
    fn a_status_without_a_failure_is_not_a_failure() {
        let parsed = parse_setup_json("{\"installed\":false,\"endpoints\":[]}")
            .expect("parse setup json");
        assert!(require_success(&parsed, "Engine status").is_ok());
    }
}
