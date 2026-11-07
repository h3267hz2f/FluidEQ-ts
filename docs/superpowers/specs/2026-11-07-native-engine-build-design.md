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
2. Generate `native/dsp-core/include/fluideq/parameters.h` from the TypeScript
   table with `native/tools/generate-parameters.mjs`.
3. Set up the MSVC developer environment so `dumpbin` is on `PATH`.
4. Configure `native/` with the installed Visual Studio x64 generator.
5. Build only `fluideq-engine`, `fluideq-engine-setup`, and `fluideq-dsp-host`
   in Release.
6. Assert `FluidEQ-Engine.dll` exports `DllGetClassObject` and
   `DllCanUnloadNow`.
7. Upload the three binaries as `FluidEQ-native-engine`.

The build tree lives under `native/.build/engine`, already ignored by git.

## Success criteria

- The workflow finishes green on `windows-latest`.
- The artifact contains the DLL, setup helper, and DSP host.
- `dumpbin /exports` finds both required APO entry points.
- No source or generated binary is committed; only the workflow and design
  records are.

## Result

Run `42345880973` on commit `89abe40` built all three targets in 3m13s on the
`windows-latest` runner (`Visual Studio 18.11.3`, CMake from
`C:\Program Files\CMake\bin`). The parameter header was generated from
`src/common/dsp/nativeParameters.ts` by `native/tools/generate-parameters.mjs`
before CMake. The uploaded `FluidEQ-native-engine` artifact contains:

| File | Size |
| --- | ---: |
| `FluidEQ-Engine.dll` | 945,152 |
| `FluidEQ-Engine-Setup.exe` | 249,856 |
| `FluidEQ-DSP.exe` | 677,376 |

Run URL:
<https://github.com/h3267hz2f/FluidEQ-ts/actions/runs/42345880973>.

## Bundle result

Run `42349204821` on commit `d3ce555` built the NSIS installer with the staged
engine and MSVC runtime. The `Verify the installer carries the engine` step
checked the binaries are inside the installer:

- `engine-bin/FluidEQ-Engine.dll`
- `engine-bin/FluidEQ-Engine-Setup.exe`
- `engine-bin/FluidEQ-DSP.exe`
- `engine-bin/msvcp140.dll`
- `engine-bin/vcruntime140.dll`
- `engine-bin/vcruntime140_1.dll`

The installer grew from 2,161,889 bytes to 2,835,487 bytes:

- `FluidEQ_2.1.0_x64-setup.exe`: 2,835,487 bytes.

Run URL:
<https://github.com/h3267hz2f/FluidEQ-ts/actions/runs/42349204821>. The
temporary `v2.1.0-engine-bundle-test` tag was deleted after the run.

## Activation bridge result

Commit `ae92ba3` adds `rust/fluideq-shell/src/engine_setup.rs` and exposes
`engine_status` / `install_engine` to the view. `status` runs the bundled
`FluidEQ-Engine-Setup.exe` unelevated and parses its one-line JSON; `install`
starts the same helper with `ShellExecuteExW` + `runas`, then reads
`%ProgramData%\FluidEQ\engine\last-setup.json`. Nothing runs automatically and
the helper remains the only component that writes the audio registry.

`cargo check -p fluideq-shell` and `cargo test -p fluideq-shell` pass locally
(23 passed, 1 ignored), and run `42351890431` (workflow_dispatch, `bundle=false`)
passed on the runner.

## View install control result

The shell view's status bar now carries the activation control: a status dot,
a plain-language engine state ("System engine off", "Engine installed, not
attached", "Engine on N outputs"), and one button that runs `install_engine`
and so prompts for administrator rights itself. A declined UAC prompt leaves
the previous state on screen with the reason in the message line; a successful
install renders the fresh status the command returns. The control stays hidden
in the browser preview and shows "System engine unavailable" when the bundled
helper is missing (dev builds without the staged engine). Verified in a browser
against a stubbed Tauri bridge: boot, install success, partial attach, and UAC
decline all render correctly, and the status bar wraps instead of overflowing
on narrow windows.

## DSP supervision result

`rust/fluideq-shell/src/engine_host.rs` now owns the whole `FluidEQ-DSP`
lifetime. The shell starts the bundled host with `--parent-pid` at boot,
queues the current `export_wire_parameters()` snapshot before `FEQ_CMD_START`,
and re-sends the whole snapshot after every store mutation. A writer thread
owns stdin and a reader thread owns stdout, so a blocking command write never
stops the draining that keeps the host's pipe full-proof. The reader validates
the one `FEQH` handshake, records rejected acks, and replaces a dead host within
the 3-per-60s budget, re-sending the last snapshot first. Exit sends
`FEQ_CMD_SHUTDOWN`, then stdin EOF, and kills only after a 3 s grace. A dev
build without the staged host records the failure in `engine_host_status`
instead of refusing to open the window.

`cargo check -p fluideq-shell` is warning-free and `cargo test -p fluideq-shell`
passes locally (41 passed, 1 ignored).

## Risks

- `/W4 /WX` may expose warnings in a newer MSVC than the original build.
- The APO links `audiomediatypecrt`, `propsys`, `mmdevapi`, and Media
  Foundation; a missing SDK component fails the build.
- The DLL may depend on the MSVC runtime; packaging must handle that later.
- GitHub-hosted runners have no audio endpoint, so this milestone cannot test
  loading the APO into `audiodg.exe`.

## Milestones

1. Build the engine artifacts in CI — **done**; see Result above.
2. Copy the artifacts into the Tauri bundle and the NSIS installer — **done**;
   see Bundle result above. They are shipped, not yet installed or activated.
3. Implement the install/register/uninstall flow with UAC and audio restart —
   **done**; the Rust activation bridge and the view install control are in.
4. Launch and supervise `FluidEQ-DSP`, and send chain snapshots from Rust —
   **done**; see DSP supervision result above.
5. Verify on a real endpoint, including bypass when the shell exits.
