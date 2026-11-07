# Native engine build Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the native FluidEQ Engine DLL, setup helper, and DSP host on GitHub Actions and publish them as a verified workflow artifact.

**Architecture:** A separate Windows workflow configures the existing CMake project in `native/`, builds three targets with the Visual Studio 17 2022 x64 generator, checks the APO exports with `dumpbin`, and uploads only the built binaries. The Rust desktop build and installer are untouched.

**Tech Stack:** GitHub Actions, CMake 3.21+, Visual Studio 2022/MSVC, Windows SDK, `dumpbin`, PowerShell.

---

### Task 1: Add the native engine workflow

**Files:**
- Create: `.github/workflows/native-engine.yml`
- Create: `native/tools/generate-parameters.mjs`
- Read: `native/CMakeLists.txt` (targets `fluideq-engine`, `fluideq-engine-setup`, `fluideq-dsp-host`)
- Read: `native/system-apo/src/engine.def` (exports `DllGetClassObject`, `DllCanUnloadNow`)
- Read: `rust/fluideq-dsp-sys/build.rs` (parameter table parser and header writer)

- [ ] **Step 0: Create the generator**

The native build runs outside Cargo, so create the Node port of the Rust
`build.rs` parameter parser in `native/tools/generate-parameters.mjs`. Run:

```powershell
node native/tools/generate-parameters.mjs
```

Expected: it writes `native/dsp-core/include/fluideq/parameters.h` with the same
ids, paths, structural flags and schema version as
`target/debug/build/fluideq-dsp-sys-*/out/fluideq/parameters.h`.

- [ ] **Step 1: Create the workflow**

Use this exact content:

```yaml
name: Native engine

on:
  workflow_dispatch:
  push:
    branches:
      - rust-desktop-shell
    paths:
      - "native/**"
      - ".github/workflows/native-engine.yml"

permissions:
  contents: read

jobs:
  build:
    runs-on: windows-latest
    timeout-minutes: 30
    concurrency:
      group: native-engine-${{ github.ref }}
      cancel-in-progress: true

    steps:
      - uses: actions/checkout@v4

      - name: Generate the native parameter header
        run: node native/tools/generate-parameters.mjs

      - uses: ilammy/msvc-dev-cmd@v1
        with:
          arch: x64

      - name: Configure the native engine
        run: >
          cmake -S native -B native/.build/engine
          -A x64
          "-DFEQ_CORE_VERSION=2.1.0"
          "-DFEQ_BUILD_REVISION=${{ github.sha }}"

      - name: Build the engine, setup helper and DSP host
        run: >
          cmake --build native/.build/engine --config Release
          --target fluideq-engine fluideq-engine-setup fluideq-dsp-host
          --parallel

      - name: Verify the engine DLL exports
        shell: pwsh
        run: |
          $dll = "native/.build/engine/bin/FluidEQ-Engine.dll"
          if (-not (Test-Path $dll)) { throw "missing $dll" }
          $exports = & dumpbin /nologo /exports $dll
          foreach ($name in @("DllGetClassObject", "DllCanUnloadNow")) {
            if (-not ($exports -match "\b$name\b")) { throw "missing export $name" }
          }

      - name: Verify the helper and host exist
        shell: pwsh
        run: |
          foreach ($file in @(
            "native/.build/engine/bin/FluidEQ-Engine-Setup.exe",
            "native/.build/engine/bin/FluidEQ-DSP.exe"
          )) {
            if (-not (Test-Path $file)) { throw "missing $file" }
          }

      - uses: actions/upload-artifact@v4
        with:
          name: FluidEQ-native-engine
          path: |
            native/.build/engine/bin/FluidEQ-Engine.dll
            native/.build/engine/bin/FluidEQ-Engine-Setup.exe
            native/.build/engine/bin/FluidEQ-DSP.exe
          if-no-files-found: error
          retention-days: 14
```

- [ ] **Step 2: Check the YAML parses**

Run:

```powershell
Get-Content .github/workflows/native-engine.yml -Raw | Out-Null
```

Expected: no output, exit code 0. GitHub is the real YAML validator; the next
task runs it.

- [ ] **Step 3: Commit**

```powershell
git add .github/workflows/native-engine.yml docs/superpowers/specs/2026-11-07-native-engine-build-design.md docs/superpowers/plans/2026-11-07-native-engine-build.md
git commit -m "Build the native engine artifacts on GitHub Actions"
```

### Task 2: Publish the branch

**Files:**
- No repository files changed.

- [ ] **Step 1: Push the commit**

The normal HTTPS git path is blocked in this environment, so update the branch
through the GitHub API with the existing follow-up script pattern:

```powershell
pwsh -NoProfile -File "$env:TEMP\opencode\push-followup-via-api.ps1"
```

Expected:

```text
tree: <local tree sha>
local tree: <same sha>
commit: <local HEAD sha>
local:  <same sha>
branch: refs/heads/rust-desktop-shell -> <same sha>
```

- [ ] **Step 2: Verify the remote ref**

```powershell
gh api "repos/h3267hz2f/FluidEQ-ts/git/ref/heads/rust-desktop-shell" --jq '.object.sha'
git rev-parse HEAD
```

Expected: both commands print the same SHA.

### Task 3: Run the native workflow

**Files:**
- No repository files changed.

- [ ] **Step 1: Trigger the workflow**

```powershell
gh workflow run native-engine.yml --ref rust-desktop-shell
```

Expected: `Created workflow_dispatch event for native-engine.yml`.

- [ ] **Step 2: Watch the run**

```powershell
gh run list --workflow native-engine.yml --limit 1
$run = gh run list --workflow native-engine.yml --limit 1 --json databaseId --jq '.[0].databaseId'
gh run watch $run --exit-status
```

Expected: the run finishes with `completed success`.

- [ ] **Step 3: Download and inspect the artifact**

```powershell
$run = gh run list --workflow native-engine.yml --limit 1 --json databaseId --jq '.[0].databaseId'
gh run download $run -n FluidEQ-native-engine -D "$env:TEMP\fluideq-native-engine"
Get-ChildItem "$env:TEMP\fluideq-native-engine" | Select-Object Name, Length
```

Expected: `FluidEQ-Engine.dll`, `FluidEQ-Engine-Setup.exe`, and
`FluidEQ-DSP.exe` with non-zero lengths.

- [ ] **Step 4: Check the downloaded DLL exports**

```powershell
$dll = "$env:TEMP\fluideq-native-engine\FluidEQ-Engine.dll"
$exports = & dumpbin /nologo /exports $dll
$exports | Select-String 'DllGetClassObject|DllCanUnloadNow'
```

Expected: both names appear.

### Task 4: Record the result

**Files:**
- Modify: `docs/superpowers/specs/2026-11-07-native-engine-build-design.md`
- Modify: `docs/superpowers/plans/2026-11-07-native-engine-build.md`

- [ ] **Step 1: Record the workflow run and artifact sizes**

Add a short "Result" section to the design with the run URL, commit SHA, and
the three artifact sizes. Do not commit the binaries.

- [ ] **Step 2: Commit the result**

```powershell
git add docs/superpowers/specs/2026-11-07-native-engine-build-design.md docs/superpowers/plans/2026-11-07-native-engine-build.md
git commit -m "Record the native engine build result"
```
