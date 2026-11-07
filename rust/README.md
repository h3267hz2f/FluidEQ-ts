# FluidEQ — Rust desktop shell

The Rust workspace is the desktop rewrite: a Tauri 2 window, an authoritative
`fluideq-state` state centre, and the original C++ `dsp-core` statically linked
through `fluideq-dsp-sys`.

## Build

The C++ core is optional so the state crate still tests without a native
toolchain. To build the real desktop binary, link it:

```powershell
cargo build -p fluideq-shell --features link-cpp-core
target\debug\fluideq-shell.exe
```

## Test

```powershell
cargo test --workspace --features link-cpp-core
```

CI runners have no default audio endpoint, so the WASAPI read test is ignored
by default. Run it on a real Windows desktop with:

```powershell
cargo test -p fluideq-shell --features link-cpp-core default_render_volume_is_a_scalar -- --ignored
```

## Installer

```powershell
cd rust\fluideq-shell
npx -y @tauri-apps/cli@2 build --features link-cpp-core
```

The installer is written to
`target\release\bundle\nsis\FluidEQ_2.1.0_x64-setup.exe`.

## One parameter table

`src/common/dsp/nativeParameters.ts` is the single list of native parameter
ids. `rust/fluideq-dsp-sys/build.rs` generates both the C++ `parameters.h` and
the Rust constants from it on every build; there is no second copy to drift.

## First vertical slice

- The window is a Tauri WebView2 view; the chain lives in Rust and is saved to
  disk after each change, so closing the window does not lose it.
- The master volume slider writes the default Windows render endpoint's
  volume through WASAPI, and the app reads the real value back on start.
- The EQ curve, band handles, gain/frequency/Q controls and preamp all read
  and write the Rust state centre; the response is computed from the same
  biquad forms the C++ core applies.
- The state is saved to `%APPDATA%\com.fluideq.desktop\state.json` after each
  change.

## CI

`.github/workflows/rust-desktop.yml` runs the tests and builds the NSIS
installer on GitHub's Windows runners, so dependency downloads and compilation
do not use the local connection. It is manual (or tag-triggered) because the
repository is private and hosted minutes are billed; start it from
**Actions → Rust desktop → Run workflow**. The workflow caches Cargo artifacts
between runs and uploads only the installer (about 2.2 MB) for download. Set
the `bundle` input to `false` for a tests-only run.
