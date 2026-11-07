# Native engine build and activation design

## Context

The Rust desktop shell links the original C++ `dsp-core`, but the Windows
FluidEQ Engine path is not delivered yet. The C++ sources for the system APO
(`native/system-apo`), its setup helper, and the supervised DSP host
(`native/dsp-host`) are in the tree, but the Rust workspace only compiles
`native/dsp-core`. The Tauri installer contains only `fluideq-shell.exe`.

The result is the known gap: the EQ UI writes Rust state, but Windows audio
never passes through the engine, so the EQ is silent.

## Goal

Move the native engine from "source in the tree" to a reproducible, verifiable
artifact that can be packaged by the desktop installer.

The first milestone is deliberately build-only:

- GitHub Actions configures and builds `FluidEQ-Engine.dll`,
  `FluidEQ-Engine-Setup.exe`, and `FluidEQ-DSP.exe` on `windows-latest`.
- The workflow checks that the APO exports its two required entry points and
  that all three binaries exist.
- The binaries are uploaded as a workflow artifact.

## Non-goals

- Installing or registering the APO.
- Writing `DisableProtectedAudioDG` or endpoint FX properties.
- Launching or supervising `FluidEQ-DSP` from the shell.
- Changing the Tauri UI or installer.
- Proving audible processing on a real endpoint.

Those are later milestones. Building the artifacts first keeps the failure
surface small and tells us whether the original C++ engine still compiles with
the current MSVC/Windows SDK.

## Approach

Add `.github/workflows/native-engine.yml`:

1. Check out the public repository.
2. Set up the MSVC developer environment so `dumpbin` is on `PATH`.
3. Configure `native/` with the installed Visual Studio x64 generator.
4. Build only `fluideq-engine`, `fluideq-engine-setup`, and `fluideq-dsp-host`
   in Release.
5. Assert `FluidEQ-Engine.dll` exports `DllGetClassObject` and
   `DllCanUnloadNow`.
6. Upload the three binaries as `FluidEQ-native-engine`.

The build tree lives under `native/.build/engine`, already ignored by git.

## Success criteria

- The workflow finishes green on `windows-latest`.
- The artifact contains the DLL, setup helper, and DSP host.
- `dumpbin /exports` finds both required APO entry points.
- No source or generated binary is committed; only the workflow and design
  records are.

## Risks

- `/W4 /WX` may expose warnings in a newer MSVC than the original build.
- The APO links `audiomediatypecrt`, `propsys`, `mmdevapi`, and Media
  Foundation; a missing SDK component fails the build.
- The DLL may depend on the MSVC runtime; packaging must handle that later.
- GitHub-hosted runners have no audio endpoint, so this milestone cannot test
  loading the APO into `audiodg.exe`.

## Milestones

1. Build the engine artifacts in CI (this design).
2. Copy the artifacts into the Tauri bundle and the NSIS installer.
3. Implement the install/register/uninstall flow with UAC and audio restart.
4. Launch and supervise `FluidEQ-DSP`, and send chain snapshots from Rust.
5. Verify on a real endpoint, including bypass when the shell exits.
