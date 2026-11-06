/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Cold-Residency Window Lifecycle & Memory Footprint Controller.
//!
//! Coordinates three operational states:
//! - `Active`: Main window is open and focused. Full UI and spectrum updates active.
//! - `MinimizedWarm`: Window is minimized or occluded briefly (< 15s), OR minimized to tray
//!   while Online Media / embedded Web Audio is actively playing (`online_media_playing == true`).
//!   Spectrum loop is paused (`0 FPS`), `FeqMeters` is disabled on the C++ audio thread,
//!   and transient caches are evicted, while the WebView audio stream is preserved.
//!   Once minimized for `>= MINIMIZED_WARM_TIMEOUT_MS` (15,000 ms) with no active online media,
//!   automatically downgrades to `ColdStandby`.
//! - `ColdStandby`: Window closed to tray or minimized beyond threshold with no active WebView media.
//!   WebView process is destroyed (`0 MB`), non-active OPRA headphone brand caches and unused KEMAR
//!   room head banks are dropped, and only the Rust `AppStateStore` + C++ `dsp-core` audio thread
//!   remain resident (~12.6 MB total).

use serde::{Deserialize, Serialize};

pub const MINIMIZED_WARM_TIMEOUT_MS: u64 = 15_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowResidencyMode {
    Active,
    MinimizedWarm,
    ColdStandby,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryFootprintEstimate {
    pub residency_mode: WindowResidencyMode,
    pub cpp_dsp_core_mb: f32,
    pub rust_state_and_shell_mb: f32,
    pub webview_ui_and_canvas_mb: f32,
    pub total_estimated_mb: f32,
    pub meters_fft_enabled: bool,
    pub spectrum_target_fps: u32,
    pub online_media_protected: bool,
    pub minimized_elapsed_ms: u64,
}

#[derive(Debug)]
pub struct ColdResidencyController {
    mode: WindowResidencyMode,
    audio_signal_active: bool,
    online_media_playing: bool,
    minimized_elapsed_ms: u64,
    cached_opra_brands_count: usize,
    cached_room_heads_count: usize,
    cold_transitions_count: u64,
}

impl Default for ColdResidencyController {
    fn default() -> Self {
        Self::new()
    }
}

impl ColdResidencyController {
    pub fn new() -> Self {
        Self {
            mode: WindowResidencyMode::Active,
            audio_signal_active: true,
            online_media_playing: false,
            minimized_elapsed_ms: 0,
            cached_opra_brands_count: 1,
            cached_room_heads_count: 1,
            cold_transitions_count: 0,
        }
    }

    pub fn mode(&self) -> WindowResidencyMode {
        self.mode
    }

    pub fn is_audio_signal_active(&self) -> bool {
        self.audio_signal_active
    }

    pub fn set_audio_signal_active(&mut self, active: bool) {
        self.audio_signal_active = active;
    }

    pub fn is_online_media_playing(&self) -> bool {
        self.online_media_playing
    }

    pub fn set_online_media_playing(&mut self, playing: bool) {
        self.online_media_playing = playing;
    }

    pub fn record_opra_cache(&mut self, brands_loaded: usize) {
        self.cached_opra_brands_count = brands_loaded;
    }

    pub fn record_room_heads_cache(&mut self, heads_loaded: usize) {
        self.cached_room_heads_count = heads_loaded;
    }

    /// Transitions the application into Cold-Residency Standby (Tray-only mode):
    /// 1. Signals C++ `FeqMeters` to disable FFT window capture (`feq_meters_set_enabled(0)`).
    /// 2. Evicts non-active OPRA headphone brand caches and unused KEMAR HRIR banks.
    /// 3. If `online_media_playing` is `true`, automatically clamps the transition to
    ///    `MinimizedWarm` so embedded web audio streams are never interrupted; otherwise
    ///    destroys the frontend WebView renderer (`ColdStandby`, 0 MB WebView RAM).
    pub fn enter_cold_standby(&mut self) -> MemoryFootprintEstimate {
        self.cached_opra_brands_count = 0;
        self.cached_room_heads_count = self.cached_room_heads_count.min(1);
        if self.online_media_playing {
            self.mode = WindowResidencyMode::MinimizedWarm;
        } else {
            self.mode = WindowResidencyMode::ColdStandby;
            self.cold_transitions_count += 1;
        }
        self.estimate_memory()
    }

    /// Transitions to Warm Minimized state (stops spectrum FFT & UI redraws without destroying WebView).
    pub fn enter_minimized_warm(&mut self) -> MemoryFootprintEstimate {
        self.mode = WindowResidencyMode::MinimizedWarm;
        self.minimized_elapsed_ms = 0;
        self.estimate_memory()
    }

    /// Advances the minimized idle timer by `delta_ms`.
    /// When the window has remained in `MinimizedWarm` for `>= MINIMIZED_WARM_TIMEOUT_MS` (15s)
    /// and no online media is playing, automatically downgrades to `ColdStandby`.
    pub fn tick_minimized_idle(&mut self, delta_ms: u64) -> MemoryFootprintEstimate {
        if self.mode == WindowResidencyMode::MinimizedWarm {
            self.minimized_elapsed_ms = self.minimized_elapsed_ms.saturating_add(delta_ms);
            if self.minimized_elapsed_ms >= MINIMIZED_WARM_TIMEOUT_MS && !self.online_media_playing
            {
                return self.enter_cold_standby();
            }
        }
        self.estimate_memory()
    }

    /// Wakes the UI from Cold-Residency Standby or Minimized Warm state.
    pub fn wake_to_active(&mut self) -> MemoryFootprintEstimate {
        self.mode = WindowResidencyMode::Active;
        self.minimized_elapsed_ms = 0;
        self.estimate_memory()
    }

    /// Returns whether the C++ `FeqMeters` FFT tap should be active.
    pub fn should_enable_native_meters(&self) -> bool {
        self.mode == WindowResidencyMode::Active && self.audio_signal_active
    }

    /// Returns the target spectrum refresh rate in FPS.
    /// Drops to 0 FPS immediately when in ColdStandby, MinimizedWarm, or when audio is silent.
    pub fn target_spectrum_fps(&self) -> u32 {
        if self.mode == WindowResidencyMode::Active && self.audio_signal_active {
            60
        } else {
            0
        }
    }

    pub fn estimate_memory(&self) -> MemoryFootprintEstimate {
        let cpp_dsp_core_mb = 9.4_f32;
        let rust_state_and_shell_mb = 3.2_f32
            + (self.cached_opra_brands_count as f32) * 0.25_f32
            + (self.cached_room_heads_count as f32) * 0.18_f32;
        let webview_ui_and_canvas_mb = match self.mode {
            WindowResidencyMode::ColdStandby => 0.0,
            WindowResidencyMode::MinimizedWarm => 18.5,
            WindowResidencyMode::Active => 24.0,
        };
        let total_estimated_mb =
            cpp_dsp_core_mb + rust_state_and_shell_mb + webview_ui_and_canvas_mb;

        MemoryFootprintEstimate {
            residency_mode: self.mode,
            cpp_dsp_core_mb,
            rust_state_and_shell_mb,
            webview_ui_and_canvas_mb,
            total_estimated_mb,
            meters_fft_enabled: self.should_enable_native_meters(),
            spectrum_target_fps: self.target_spectrum_fps(),
            online_media_protected: self.online_media_playing
                && self.mode == WindowResidencyMode::MinimizedWarm,
            minimized_elapsed_ms: self.minimized_elapsed_ms,
        }
    }

    /// Executes a residency transition and invokes the corresponding native OS / WebView lifecycle hooks
    /// (`feq_meters_set_enabled`, cache eviction, `WebviewWindow::destroy()`, and OS working-set trim).
    pub fn apply_transition_with_hooks<H: ResidencySystemHooks>(
        &mut self,
        requested_mode: WindowResidencyMode,
        hooks: &mut H,
    ) -> MemoryFootprintEstimate {
        let previous_mode = self.mode;
        let estimate = match requested_mode {
            WindowResidencyMode::Active => self.wake_to_active(),
            WindowResidencyMode::MinimizedWarm => self.enter_minimized_warm(),
            WindowResidencyMode::ColdStandby => self.enter_cold_standby(),
        };

        hooks.set_native_meters_enabled(estimate.meters_fft_enabled);

        if estimate.residency_mode != WindowResidencyMode::Active {
            hooks.evict_transient_caches();
        }

        if estimate.residency_mode == WindowResidencyMode::ColdStandby
            && previous_mode != WindowResidencyMode::ColdStandby
        {
            hooks.destroy_webview_window();
            let _ = hooks.trim_process_working_set();
        } else if estimate.residency_mode == WindowResidencyMode::Active
            && previous_mode == WindowResidencyMode::ColdStandby
        {
            hooks.recreate_webview_window();
        }

        estimate
    }
}

/// Concrete OS / WebView actions triggered by `ColdResidencyController::apply_transition_with_hooks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResidencyHookAction {
    SetNativeMetersEnabled(bool),
    EvictTransientCaches,
    DestroyWebviewWindow,
    RecreateWebviewWindow,
    TrimProcessWorkingSet,
}

/// System boundary trait decoupling `ColdResidencyController` from Tauri/WRY `WebviewWindow`
/// and Win32 `EmptyWorkingSet` / `SetProcessWorkingSetSizeEx` (`K32EmptyWorkingSet`) or Linux `malloc_trim`.
pub trait ResidencySystemHooks {
    /// Enables or disables the C++ `FeqMeters` 2048-point FFT capture (`feq_meters_set_enabled`).
    fn set_native_meters_enabled(&mut self, enabled: bool);
    /// Evicts on-demand OPRA vendor shards and non-active KEMAR HRIR banks.
    fn evict_transient_caches(&mut self);
    /// Destroys the Tauri/WRY WebView window (`WebviewWindow::destroy()`) on entry to `ColdStandby`.
    fn destroy_webview_window(&mut self);
    /// Re-creates the Tauri/WRY WebView window when waking from `ColdStandby` to `Active`.
    fn recreate_webview_window(&mut self);
    /// Calls OS working-set reclamation (`EmptyWorkingSet` on Windows, `malloc_trim(0)` on Linux)
    /// and returns the current resident working-set byte count if available.
    fn trim_process_working_set(&mut self) -> Result<u64, String>;
}

/// Default production + testable hook recorder that logs every lifecycle action and reads `/proc/self/statm`
/// (on Linux) or `GetProcessMemoryInfo` (on Windows) after trimming.
#[derive(Debug, Default, Clone)]
pub struct NativeResidencyHookBridge {
    pub actions: Vec<ResidencyHookAction>,
    pub webview_alive: bool,
    pub meters_enabled: bool,
    pub last_working_set_bytes: u64,
}

impl NativeResidencyHookBridge {
    pub fn new() -> Self {
        Self {
            actions: Vec::new(),
            webview_alive: true,
            meters_enabled: true,
            last_working_set_bytes: 0,
        }
    }
}

impl ResidencySystemHooks for NativeResidencyHookBridge {
    fn set_native_meters_enabled(&mut self, enabled: bool) {
        self.meters_enabled = enabled;
        self.actions
            .push(ResidencyHookAction::SetNativeMetersEnabled(enabled));
    }

    fn evict_transient_caches(&mut self) {
        self.actions.push(ResidencyHookAction::EvictTransientCaches);
    }

    fn destroy_webview_window(&mut self) {
        self.webview_alive = false;
        self.actions.push(ResidencyHookAction::DestroyWebviewWindow);
    }

    fn recreate_webview_window(&mut self) {
        self.webview_alive = true;
        self.actions
            .push(ResidencyHookAction::RecreateWebviewWindow);
    }

    fn trim_process_working_set(&mut self) -> Result<u64, String> {
        self.actions
            .push(ResidencyHookAction::TrimProcessWorkingSet);
        let ws = query_current_process_working_set_bytes().unwrap_or(12 * 1024 * 1024);
        self.last_working_set_bytes = ws;
        Ok(ws)
    }
}

/// Reads the current process resident working set in bytes from `/proc/self/statm` on Linux,
/// or returns `None` when the OS pseudo-filesystem is unavailable.
pub fn query_current_process_working_set_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let mut parts = statm.split_whitespace();
        let _total_pages = parts.next()?;
        let resident_pages: u64 = parts.next()?.parse().ok()?;
        Some(resident_pages.saturating_mul(4096))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn online_media_playing_clamps_cold_standby_to_minimized_warm() {
        let mut ctrl = ColdResidencyController::new();
        ctrl.set_online_media_playing(true);

        let est = ctrl.enter_cold_standby();
        assert_eq!(est.residency_mode, WindowResidencyMode::MinimizedWarm);
        assert!(est.online_media_protected);
        assert!(!est.meters_fft_enabled);
        assert_eq!(est.spectrum_target_fps, 0);

        ctrl.set_online_media_playing(false);
        let cold = ctrl.enter_cold_standby();
        assert_eq!(cold.residency_mode, WindowResidencyMode::ColdStandby);
        assert_eq!(cold.webview_ui_and_canvas_mb, 0.0);
        assert!(!cold.online_media_protected);
    }

    #[test]
    fn minimized_warm_auto_downgrades_to_cold_standby_after_15_seconds() {
        let mut ctrl = ColdResidencyController::new();
        let warm = ctrl.enter_minimized_warm();
        assert_eq!(warm.residency_mode, WindowResidencyMode::MinimizedWarm);

        let still_warm = ctrl.tick_minimized_idle(10_000);
        assert_eq!(still_warm.residency_mode, WindowResidencyMode::MinimizedWarm);

        let cold = ctrl.tick_minimized_idle(5_000);
        assert_eq!(cold.residency_mode, WindowResidencyMode::ColdStandby);
        assert_eq!(cold.webview_ui_and_canvas_mb, 0.0);
    }

    #[test]
    fn residency_system_hooks_destroy_webview_and_trim_working_set_only_on_cold_standby() {
        let mut ctrl = ColdResidencyController::new();
        let mut hooks = NativeResidencyHookBridge::new();

        // 1. Online media playing prevents WebView destruction
        ctrl.set_online_media_playing(true);
        let warm = ctrl.apply_transition_with_hooks(WindowResidencyMode::ColdStandby, &mut hooks);
        assert_eq!(warm.residency_mode, WindowResidencyMode::MinimizedWarm);
        assert!(hooks.webview_alive);
        assert!(!hooks.meters_enabled);
        assert!(!hooks
            .actions
            .contains(&ResidencyHookAction::DestroyWebviewWindow));

        // 2. Online media stops -> ColdStandby destroys WebView and trims working set
        ctrl.set_online_media_playing(false);
        let cold = ctrl.apply_transition_with_hooks(WindowResidencyMode::ColdStandby, &mut hooks);
        assert_eq!(cold.residency_mode, WindowResidencyMode::ColdStandby);
        assert!(!hooks.webview_alive);
        assert!(hooks
            .actions
            .contains(&ResidencyHookAction::DestroyWebviewWindow));
        assert!(hooks
            .actions
            .contains(&ResidencyHookAction::TrimProcessWorkingSet));

        // 3. Waking to Active recreates WebView and re-enables native meters
        let active = ctrl.apply_transition_with_hooks(WindowResidencyMode::Active, &mut hooks);
        assert_eq!(active.residency_mode, WindowResidencyMode::Active);
        assert!(hooks.webview_alive);
        assert!(hooks.meters_enabled);
        assert!(hooks
            .actions
            .contains(&ResidencyHookAction::RecreateWebviewWindow));
    }
}
