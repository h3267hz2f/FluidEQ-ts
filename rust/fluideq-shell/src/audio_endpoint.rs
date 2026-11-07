/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! The one real system-audio control in the first vertical slice.
//!
//! The app's own DSP lives in the C++ core and is applied by the APO. Until
//! that path is installed, the shell still has to prove it can change what the
//! listener hears: the default render endpoint's master volume is a genuine
//! Windows setting, not a value kept in the web view, and a restart shows the
//! change persisted.

#[cfg(target_os = "windows")]
mod platform {
    use windows::core::GUID;
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    /// `MMDeviceEnumerator` (`{BCDE0395-E52F-467C-8E3D-C4579291692E}`). The
    /// windows crate publishes the interface but not every class constant, so
    /// the CLSID is spelled here rather than guessed from a generated symbol.
    const CLSID_MM_DEVICE_ENUMERATOR: GUID =
        GUID::from_u128(0xBCDE0395_E52F_467C_8E3D_C4579291692E);

    /// COM is per-thread. Commands arrive on whichever worker the runtime
    /// chooses, so each call initialises its own apartment and releases it.
    struct Apartment {
        owned: bool,
    }

    impl Apartment {
        fn enter() -> Self {
            // `RPC_E_CHANGED_MODE` means somebody already put this thread in
            // an apartment; COM is usable and we must not uninitialise theirs.
            let owned = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
            Self { owned }
        }
    }

    impl Drop for Apartment {
        fn drop(&mut self) {
            if self.owned {
                unsafe { CoUninitialize() };
            }
        }
    }

    fn endpoint_volume() -> Result<IAudioEndpointVolume, String> {
        let _apartment = Apartment::enter();
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&CLSID_MM_DEVICE_ENUMERATOR, None, CLSCTX_ALL)
                    .map_err(|error| format!("audio endpoint enumerator: {error}"))?;
            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|error| format!("default render endpoint: {error}"))?;
            device
                .Activate(CLSCTX_ALL, None)
                .map_err(|error| format!("endpoint volume: {error}"))
        }
    }

    pub fn get_master_volume() -> Result<f64, String> {
        unsafe {
            endpoint_volume()?
                .GetMasterVolumeLevelScalar()
                .map(|value| f64::from(value))
                .map_err(|error| format!("read master volume: {error}"))
        }
    }

    pub fn set_master_volume(scalar: f64) -> Result<f64, String> {
        let scalar = if scalar.is_finite() { scalar } else { 0.0 };
        let scalar = scalar.clamp(0.0, 1.0) as f32;
        unsafe {
            let volume = endpoint_volume()?;
            volume
                .SetMasterVolumeLevelScalar(scalar, std::ptr::null())
                .map_err(|error| format!("write master volume: {error}"))?;
            volume
                .GetMasterVolumeLevelScalar()
                .map(|value| f64::from(value))
                .map_err(|error| format!("read master volume: {error}"))
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    pub fn get_master_volume() -> Result<f64, String> {
        Err("system volume control is only wired on Windows".to_string())
    }

    pub fn set_master_volume(_scalar: f64) -> Result<f64, String> {
        Err("system volume control is only wired on Windows".to_string())
    }
}

pub use platform::{get_master_volume, set_master_volume};

#[cfg(all(test, target_os = "windows"))]
mod tests {
    // Hosted Windows runners have no default render endpoint, so this stays out
    // of the normal test run. Run it on a real desktop with `-- --ignored`.
    #[test]
    #[ignore = "requires a real Windows audio endpoint"]
    fn default_render_volume_is_a_scalar() {
        let volume =
            super::get_master_volume().expect("the default render endpoint must expose volume");
        assert!(
            (0.0..=1.0).contains(&volume),
            "endpoint volume {volume} is outside the scalar range"
        );
    }
}
