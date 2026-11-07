# Native engine bundle plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the native Windows engine binaries inside the Tauri NSIS installer without breaking `tauri dev`.

**Architecture:** A release-only Node script builds the native engine targets with CMake, copies `FluidEQ-Engine.dll`, `FluidEQ-Engine-Setup.exe`, and `FluidEQ-DSP.exe` into `rust/fluideq-shell/engine-bin/`, and writes a full `tauri.bundle.conf.json` from `tauri.conf.json` plus a `bundle.resources` list. The Rust desktop workflow runs that script before `tauri build`, passes `--config tauri.bundle.conf.json`, and verifies the installer contains the three files with `7z l`.

**Tech Stack:** Node.js, CMake, MSVC, Tauri v2, NSIS, GitHub Actions.

---

### Task 1: Add the engine staging script

**Files:**
- Create: `rust/fluideq-shell/scripts/stage-native-engine.mjs`
- Read: `rust/fluideq-shell/tauri.conf.json` (base config)
- Read: `native/tools/generate-parameters.mjs` (parameter header)

- [ ] **Step 1: Create the staging script**

The script must:

1. Fail unless `process.platform === 'win32'`.
2. Run `node native/tools/generate-parameters.mjs`.
3. Configure `native/.build/engine` with `-A x64`,
   `-DFEQ_CORE_VERSION=2.1.0`, and
   `-DFEQ_BUILD_REVISION=${GITHUB_SHA || unknown}`.
4. Build `fluideq-engine`, `fluideq-engine-setup`, and `fluideq-dsp-host`.
5. Copy the three build outputs into `rust/fluideq-shell/engine-bin/`.
6. Read `rust/fluideq-shell/tauri.conf.json`, add:
   ```json
   {
     "bundle": {
       "resources": [
         "engine-bin/FluidEQ-Engine.dll",
         "engine-bin/FluidEQ-Engine-Setup.exe",
         "engine-bin/FluidEQ-DSP.exe"
       ]
     }
   }
   ```
   and write `rust/fluideq-shell/tauri.bundle.conf.json`.

- [ ] **Step 2: Ignore the staged files**

Append to `.gitignore`:

```gitignore
rust/fluideq-shell/engine-bin/
rust/fluideq-shell/tauri.bundle.conf.json
```

- [ ] **Step 3: Check the script syntax**

```powershell
node --check rust/fluideq-shell/scripts/stage-native-engine.mjs
```

Expected: no output, exit code 0.

### Task 2: Bundle the engine in the Rust workflow

**Files:**
- Modify: `.github/workflows/rust-desktop.yml`
- Read: `.github/workflows/rust-desktop.yml`

- [ ] **Step 1: Stage the engine before the bundle**

Add before the Tauri build step, with the same `if:` condition:

```yaml
      - name: Stage the native engine into the installer
        if: github.event_name != 'workflow_dispatch' || inputs.bundle
        run: node rust/fluideq-shell/scripts/stage-native-engine.mjs
```

- [ ] **Step 2: Build with the override config**

Change the Tauri build command to:

```powershell
npx -y @tauri-apps/cli@2 build --features link-cpp-core --config tauri.bundle.conf.json
```

- [ ] **Step 3: Verify the installer**

Add after the build, before upload:

```yaml
      - name: Verify the installer carries the engine
        if: github.event_name != 'workflow_dispatch' || inputs.bundle
        shell: pwsh
        run: |
          $installer = Get-Item target/release/bundle/nsis/*.exe | Select-Object -First 1
          $listing = & 7z l $installer.FullName
          foreach ($name in @("FluidEQ-Engine.dll", "FluidEQ-Engine-Setup.exe", "FluidEQ-DSP.exe")) {
            if (-not ($listing | Select-String -SimpleMatch $name)) { throw "installer is missing $name" }
          }
```

### Task 3: Run the bundle workflow

**Files:**
- No repository files changed.

- [ ] **Step 1: Commit and push**

The normal HTTPS git path is blocked, so push through the GitHub API script:

```powershell
pwsh -NoProfile -File "$env:TEMP\opencode\push-followup-via-api.ps1"
```

Expected: local and remote branch SHA match.

- [ ] **Step 2: Run the Rust desktop workflow**

```powershell
gh workflow run rust-desktop.yml --ref rust-desktop-shell -f bundle=true
```

- [ ] **Step 3: Watch and download the installer**

```powershell
gh run list --workflow rust-desktop.yml --limit 1
gh run watch <run-id> --exit-status
gh run download <run-id> -n FluidEQ-installer -D "$env:TEMP\fluideq-installer"
Get-ChildItem "$env:TEMP\fluideq-installer" | Select-Object Name, Length
```

Expected: the workflow finishes green, the verify step passes, and the installer
artifact is larger than the previous shell-only build.
