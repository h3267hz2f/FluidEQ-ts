/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! The bridge from the Rust state centre to the engine running inside audiodg.
//!
//! The engine reads `<engine root>\config\config.txt`, and it treats FluidEQ as
//! absent — pass-through — unless the app holds `\\.\pipe\FluidEQ-Engine-Owner`
//! open for as long as it runs. Without both halves an EQ edit changes the
//! window and nothing else.

use fluideq_dsp_sys::FeqEqModel;
use fluideq_state::apo::format_bands_as_apo_lines;
use fluideq_state::apo_analysis_and_status::{config_dir, owner_pipe_name};
use fluideq_state::biquad::{modelled_quality, BandFilterKind, EqBandSpec};
use fluideq_state::AppStateSnapshot;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicBool, Ordering};

const OWNER_MARKER: &str = "fluideq-owner.txt";
/// The engine loads `Convolution:` paths relative to its config folder, so the
/// app rewrites this one file rather than naming the user's original location.
pub const CONVOLUTION_FILE: &str = "fluideq-convolution.wav";

/// Why the last publish failed, so the app can say so instead of showing a
/// chain that edits the window and nothing else.
static LAST_ERROR: OnceLock<Mutex<Option<String>>> = OnceLock::new();

#[cfg(target_os = "windows")]
static OWNER_READY: AtomicBool = AtomicBool::new(false);

fn error_slot() -> &'static Mutex<Option<String>> {
    LAST_ERROR.get_or_init(|| Mutex::new(None))
}

fn note_error(message: Option<String>) {
    if let Ok(mut slot) = error_slot().lock() {
        *slot = message;
    }
}

/// The last reason the engine could not be reached, if there is one.
pub fn last_error() -> Option<String> {
    if let Ok(slot) = error_slot().lock() {
        if let Some(message) = slot.clone() {
            return Some(message);
        }
    }
    #[cfg(target_os = "windows")]
    if !OWNER_READY.load(Ordering::Relaxed) {
        return Some("the engine owner pipe did not come up".to_string());
    }
    None
}

fn parse_model(name: &str) -> FeqEqModel {
    match name.trim().to_ascii_lowercase().as_str() {
        "proportional" | "feq_eq_model_proportional" => FeqEqModel::FEQ_EQ_MODEL_PROPORTIONAL,
        "wide" | "feq_eq_model_wide" => FeqEqModel::FEQ_EQ_MODEL_WIDE,
        "asymmetric" | "feq_eq_model_asymmetric" => FeqEqModel::FEQ_EQ_MODEL_ASYMMETRIC,
        _ => FeqEqModel::FEQ_EQ_MODEL_CLEAN,
    }
}

/// The Equalizer APO text the engine parses, for the current state.
///
/// Powered off is an empty file, which the engine reads as a pass-through
/// chain. With auto headroom on, the preamp the app sized goes above the guard
/// directive and the manual preamp goes after it: the engine takes the first as
/// where its guard starts and the second as the user's own fixed trim, so the
/// Preamp slider keeps working instead of being swallowed by auto headroom.
pub fn render(snapshot: &AppStateSnapshot) -> String {
    if !snapshot.master_power {
        return String::new();
    }
    let model = parse_model(&snapshot.eq_model);
    let mut voiced_bands: Vec<EqBandSpec> = snapshot
        .bands
        .iter()
        .map(|band| {
            let mut b = *band;
            b.quality = modelled_quality(
                b.filter_type.to_feq_type(),
                b.gain_db,
                b.quality,
                model,
                snapshot.eq_model_amount,
            );
            b
        })
        .collect();
    if snapshot.tone.bass_db.abs() > 1e-4 {
        voiced_bands.push(EqBandSpec {
            id: (voiced_bands.len() + 1) as u32,
            enabled: true,
            filter_type: BandFilterKind::LSC,
            frequency: snapshot.tone.bass_freq_hz,
            gain_db: snapshot.tone.bass_db,
            quality: snapshot.tone.bass_q,
            dynamic: false,
            threshold_db: -18.0,
            ear: 0,
        });
    }
    if snapshot.tone.treble_db.abs() > 1e-4 {
        voiced_bands.push(EqBandSpec {
            id: (voiced_bands.len() + 1) as u32,
            enabled: true,
            filter_type: BandFilterKind::HSC,
            frequency: snapshot.tone.treble_freq_hz,
            gain_db: snapshot.tone.treble_db,
            quality: snapshot.tone.treble_q,
            dynamic: false,
            threshold_db: -18.0,
            ear: 0,
        });
    }
    if snapshot.tone.low_cut_hz > 20.5 {
        voiced_bands.push(EqBandSpec {
            id: (voiced_bands.len() + 1) as u32,
            enabled: true,
            filter_type: BandFilterKind::HPQ,
            frequency: snapshot.tone.low_cut_hz,
            gain_db: 0.0,
            quality: 0.71,
            dynamic: false,
            threshold_db: -18.0,
            ear: 0,
        });
    }
    if snapshot.tone.high_cut_hz < 19_950.0 {
        voiced_bands.push(EqBandSpec {
            id: (voiced_bands.len() + 1) as u32,
            enabled: true,
            filter_type: BandFilterKind::LPQ,
            frequency: snapshot.tone.high_cut_hz,
            gain_db: 0.0,
            quality: 0.71,
            dynamic: false,
            threshold_db: -18.0,
            ear: 0,
        });
    }
    let rendered = format_bands_as_apo_lines(0.0, &voiced_bands);
    let filters = rendered.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
    let mut text = String::new();
    if snapshot.auto_headroom {
        text.push_str(&format!("Preamp: {:.1} dB\n", snapshot.auto_preamp_db.min(0.0)));
        text.push_str("# FluidEQAutoPreamp: ON\n");
    }
    text.push_str(&format!("Preamp: {:.1} dB\n", snapshot.preamp_db));
    text.push_str(if snapshot.matched_treble {
        "# FluidEQFilterDesign: MATCHED\n"
    } else {
        "# FluidEQFilterDesign: COOKBOOK\n"
    });
    text.push_str(filters);
    if !filters.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if snapshot.impulse_response.is_some() {
        text.push_str(&format!("Convolution: {CONVOLUTION_FILE}\n"));
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// Writes the impulse response beside `config.txt`, where the engine resolves
/// a relative `Convolution:` line. Through a temporary file, for the same reason
/// the curve is: the watcher must never read a half-written WAV.
pub fn write_convolution(folder: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(folder)?;
    let temporary = folder.join(format!("{CONVOLUTION_FILE}.tmp"));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, folder.join(CONVOLUTION_FILE))
}

/// Removes the impulse response file; a missing one is already the goal.
pub fn remove_convolution(folder: &Path) {
    let _ = std::fs::remove_file(folder.join(CONVOLUTION_FILE));
}

/// Writes `config.txt` whole through a temporary file, so the watcher never
/// reads a half-written curve.
pub fn write_to(config_folder: &Path, snapshot: &AppStateSnapshot) -> std::io::Result<()> {
    std::fs::create_dir_all(config_folder)?;
    let temporary = config_folder.join("config.txt.tmp");
    let target = config_folder.join("config.txt");
    std::fs::write(&temporary, render(snapshot))?;
    std::fs::rename(&temporary, &target)
}

/// Publishes the current state to the engine's configuration directory.
pub fn publish(snapshot: &AppStateSnapshot) -> Result<PathBuf, String> {
    let folder = PathBuf::from(config_dir());
    if folder.as_os_str().is_empty() {
        let message = "the engine root could not be resolved".to_string();
        note_error(Some(message.clone()));
        return Err(message);
    }
    write_to(&folder, snapshot).map_err(|error| {
        let message = format!(
            "could not write {}: {error}",
            folder.join("config.txt").display()
        );
        note_error(Some(message.clone()));
        message
    })?;
    // A file written in the watched directory is what wakes the engine's
    // watcher, which then retries the owner pipe. The contents do not matter.
    let _ = std::fs::write(folder.join(OWNER_MARKER), "FluidEQ\n");
    note_error(None);
    Ok(folder)
}

/// Holds the owner pipe open for the lifetime of the process.
///
/// Waits until the first pipe instance exists before returning: the engine
/// retries the link only on a change in its configuration directory, so a
/// config written before the name exists would leave the audio bypassed until
/// the next edit.
#[cfg(target_os = "windows")]
pub fn start_owner_link() {
    let (ready, wait) = std::sync::mpsc::channel();
    pipe::spawn(owner_pipe_name(), ready);
    if wait.recv_timeout(std::time::Duration::from_secs(2)).is_ok() {
        OWNER_READY.store(true, Ordering::Relaxed);
        return;
    }
    // The pipe thread keeps retrying, so the wait is not the end of the
    // answer. Whoever is asking `last_error` hears it as soon as it succeeds.
    std::thread::spawn(move || {
        if wait.recv().is_ok() {
            OWNER_READY.store(true, Ordering::Relaxed);
        }
    });
}

#[cfg(not(target_os = "windows"))]
pub fn start_owner_link() {}

#[cfg(target_os = "windows")]
mod pipe {
    use std::ffi::c_void;
    use std::ptr;

    const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
    const PIPE_TYPE_BYTE: u32 = 0x0000_0000;
    const PIPE_READMODE_BYTE: u32 = 0x0000_0000;
    const PIPE_WAIT: u32 = 0x0000_0000;
    const PIPE_UNLIMITED_INSTANCES: u32 = 255;
    const ERROR_PIPE_CONNECTED: u32 = 535;
    const SDDL_REVISION_1: u32 = 1;
    const INVALID_HANDLE_VALUE: isize = -1;

    #[repr(C)]
    struct SecurityAttributes {
        length: u32,
        security_descriptor: *mut c_void,
        inherit_handle: i32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateNamedPipeW(
            name: *const u16,
            open_mode: u32,
            pipe_mode: u32,
            max_instances: u32,
            out_buffer_size: u32,
            in_buffer_size: u32,
            default_timeout: u32,
            security_attributes: *const c_void,
        ) -> *mut c_void;
        fn ConnectNamedPipe(handle: *mut c_void, overlapped: *mut c_void) -> i32;
        fn DisconnectNamedPipe(handle: *mut c_void) -> i32;
        fn ReadFile(
            handle: *mut c_void,
            buffer: *mut u8,
            length: u32,
            read: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetLastError() -> u32;
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text: *const u16,
            revision: u32,
            descriptor: *mut *mut c_void,
            size: *mut u32,
        ) -> i32;
    }

    /// The engine asks from audiodg.exe, which runs as LOCAL SERVICE in session
    /// 0; a pipe created in a user session with the default DACL refuses it.
    /// Read access for Everyone is enough, and the pipe carries no data at all.
    fn security() -> (SecurityAttributes, *mut c_void) {
        let sddl: Vec<u16> = "D:(A;;GR;;;WD)\0".encode_utf16().collect();
        let mut descriptor: *mut c_void = ptr::null_mut();
        let mut size = 0u32;
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                &mut size,
            )
        };
        let attributes = SecurityAttributes {
            length: std::mem::size_of::<SecurityAttributes>() as u32,
            security_descriptor: descriptor,
            inherit_handle: 0,
        };
        if converted == 0 {
            return (SecurityAttributes { security_descriptor: ptr::null_mut(), ..attributes }, ptr::null_mut());
        }
        (attributes, descriptor)
    }

    pub fn spawn(name: String, ready: std::sync::mpsc::Sender<()>) {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let _ = std::thread::Builder::new()
            .name("fluideq-owner-pipe".to_string())
            .spawn(move || unsafe {
                let mut announced = false;
                loop {
                    let (mut attributes, descriptor) = security();
                    let handle = CreateNamedPipeW(
                        wide.as_ptr(),
                        PIPE_ACCESS_DUPLEX,
                        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                        PIPE_UNLIMITED_INSTANCES,
                        0,
                        0,
                        0,
                        &mut attributes as *mut SecurityAttributes as *const c_void,
                    );
                    if !descriptor.is_null() {
                        LocalFree(descriptor);
                    }
                    if handle as isize == INVALID_HANDLE_VALUE {
                        // A transient failure (handle limit, a stale instance)
                        // must not leave audio bypassed for the whole session.
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        continue;
                    }
                    if !announced {
                        announced = true;
                        let _ = ready.send(());
                    }
                    let connected = ConnectNamedPipe(handle, ptr::null_mut()) != 0
                        || GetLastError() == ERROR_PIPE_CONNECTED;
                    if connected {
                        // The engine only reads: this blocks until it
                        // disconnects, and the process closing the handle is
                        // how it learns FluidEQ is gone.
                        let mut byte = 0u8;
                        let mut read = 0u32;
                        loop {
                            if ReadFile(handle, &mut byte, 1, &mut read, ptr::null_mut()) == 0
                                || read == 0
                            {
                                break;
                            }
                        }
                        DisconnectNamedPipe(handle);
                    }
                    CloseHandle(handle);
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluideq_state::apo::resolve_apo_endpoint_chain;
    use fluideq_state::biquad::{BandFilterKind, EqBandSpec};
    use fluideq_state::wav_ir::IrSummary;
    use fluideq_state::{AppStateStore, ToneSettings};
    use std::collections::BTreeMap;

    fn band(filter_type: BandFilterKind, frequency: f64, gain_db: f64, quality: f64) -> EqBandSpec {
        EqBandSpec {
            id: 0,
            enabled: true,
            filter_type,
            frequency,
            gain_db,
            quality,
            dynamic: false,
            threshold_db: -18.0,
            ear: 0,
        }
    }

    fn store_with_two_bands(master_power: bool, auto_headroom: bool) -> AppStateStore {
        let mut store = AppStateStore::new(48_000.0);
        store.set_eq_bands_and_config(
            vec![
                band(BandFilterKind::PK, 1_000.0, 3.0, 1.41),
                band(BandFilterKind::LSC, 100.0, -2.0, 0.71),
            ],
            "clean",
            false,
            master_power,
            auto_headroom,
            -1.0,
            ToneSettings::default(),
        );
        store
    }

    #[test]
    fn rendered_curves_round_trip_through_the_engine_resolver() {
        let snapshot = store_with_two_bands(true, false).snapshot();
        let mut files = BTreeMap::new();
        files.insert("config.txt".to_string(), render(&snapshot));

        let resolved = resolve_apo_endpoint_chain("config.txt", "", "", &files);
        assert!(resolved.matched);
        assert_eq!(resolved.bands.len(), 2);
        assert!((resolved.bands[0].gain_db - 3.0).abs() < 0.11);
        assert!((resolved.bands[1].gain_db - -2.0).abs() < 0.11);
        assert!((resolved.preamp_db - -1.0).abs() < 0.11);
    }

    #[test]
    fn positive_values_are_written_without_a_leading_plus() {
        let snapshot = store_with_two_bands(true, false).snapshot();
        let text = render(&snapshot);
        assert!(text.contains("Gain 3.0 dB"));
        // The engine parses numbers with `std::from_chars`, which takes a
        // minus but not a plus; `+3.0` loses the whole filter line.
        assert!(!text.contains('+'));
    }

    #[test]
    fn auto_headroom_writes_the_guard_directive_and_keeps_the_trim() {
        let snapshot = store_with_two_bands(true, true).snapshot();
        let text = render(&snapshot);
        assert!(text.contains("# FluidEQAutoPreamp: ON"));
        assert!(text.starts_with("Preamp:"));

        let mut files = BTreeMap::new();
        files.insert("config.txt".to_string(), text);
        let resolved = resolve_apo_endpoint_chain("config.txt", "", "", &files);
        assert!(resolved.auto_preamp);
        assert!(resolved.auto_preamp_start_db <= 0.0);
        assert!((resolved.preamp_db - -1.0).abs() < 0.11);
    }

    #[test]
    fn powered_off_is_an_empty_pass_through() {
        let snapshot = store_with_two_bands(false, false).snapshot();
        assert_eq!(render(&snapshot), "");
    }

    #[test]
    fn write_to_replaces_the_file_and_leaves_no_temporary() {
        let folder = std::env::temp_dir().join(format!(
            "fluideq-apo-bridge-{}-{}",
            std::process::id(),
            line!()
        ));
        let mut store = store_with_two_bands(true, false);
        write_to(&folder, &store.snapshot()).unwrap();
        assert!(std::fs::read_to_string(folder.join("config.txt"))
            .unwrap()
            .contains("1000.0 Hz"));

        store.update_band_gain(1, 2_000.0, 6.0, 1.0).unwrap();
        write_to(&folder, &store.snapshot()).unwrap();
        let updated = std::fs::read_to_string(folder.join("config.txt")).unwrap();
        assert!(updated.contains("2000.0 Hz"));
        assert!(!folder.join("config.txt.tmp").exists());

        let _ = std::fs::remove_dir_all(&folder);
    }

    fn ir_summary() -> IrSummary {
        IrSummary {
            sample_rate: 48_000,
            channels: 2,
            frames: 480,
            seconds: 0.01,
            left_peak_db: -1.0,
            right_peak_db: -1.0,
        }
    }

    #[test]
    fn a_loaded_impulse_response_names_the_engine_convolution_file() {
        let mut snapshot = store_with_two_bands(true, false).snapshot();
        assert!(!render(&snapshot).contains("Convolution:"));

        snapshot.impulse_response = Some(ir_summary());
        let text = render(&snapshot);
        assert!(text.contains(&format!("Convolution: {CONVOLUTION_FILE}")));

        // The bypassed chain is empty, so a loaded IR does not resurrect it.
        snapshot.master_power = false;
        assert_eq!(render(&snapshot), "");
    }

    #[test]
    fn the_convolution_file_is_replaced_whole_and_removed() {
        let folder = std::env::temp_dir().join(format!(
            "fluideq-convolution-{}-{}",
            std::process::id(),
            line!()
        ));
        write_convolution(&folder, b"first").unwrap();
        write_convolution(&folder, b"second").unwrap();
        assert_eq!(
            std::fs::read(folder.join(CONVOLUTION_FILE)).unwrap(),
            b"second"
        );
        assert!(!folder.join(format!("{CONVOLUTION_FILE}.tmp")).exists());

        remove_convolution(&folder);
        assert!(!folder.join(CONVOLUTION_FILE).exists());
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn tone_and_voicing_are_rendered_to_apo_lines() {
        let mut store = store_with_two_bands(true, false);
        let tone = ToneSettings {
            bass_db: 3.5,
            bass_freq_hz: 100.0,
            bass_q: 0.71,
            treble_db: -2.5,
            treble_freq_hz: 8000.0,
            treble_q: 0.71,
            low_cut_hz: 35.0,
            high_cut_hz: 18000.0,
        };
        store.set_tone_and_voicing(tone, "proportional", 0.75, true);
        let text = render(&store.snapshot());
        assert!(text.contains("# FluidEQFilterDesign: MATCHED"));
        assert!(text.contains("LSC Fc 100.0 Hz Gain 3.5 dB Q 0.71"));
        assert!(text.contains("HSC Fc 8000.0 Hz Gain -2.5 dB Q 0.71"));
        assert!(text.contains("HPQ Fc 35.0 Hz Q 0.71"));
        assert!(text.contains("LPQ Fc 18000.0 Hz Q 0.71"));
    }
}
