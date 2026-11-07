#!/usr/bin/env node
/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

/**
 * Build the native Windows engine and stage it for the Tauri NSIS installer.
 *
 * The base `tauri.conf.json` carries no engine resources so `tauri dev`
 * without a native build tree still starts. This script writes a separate
 * `tauri.bundle.conf.json` override that adds the three binaries as bundle
 * resources only for a release bundle. The override and `engine-bin/` are
 * gitignored.
 */
import {
  copyFileSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const shellDir = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '..',
);
const repoRoot = path.resolve(shellDir, '..', '..');

const nativeDir = path.join(repoRoot, 'native');
const nativeBuild = path.join(nativeDir, '.build', 'engine');
const nativeBin = path.join(nativeBuild, 'bin');
const engineDir = path.join(shellDir, 'engine-bin');
const overridePath = path.join(shellDir, 'tauri.bundle.conf.json');

const version = '2.1.0';
const revision = process.env.GITHUB_SHA || 'unknown';

const engineFiles = [
  'FluidEQ-Engine.dll',
  'FluidEQ-Engine-Setup.exe',
  'FluidEQ-DSP.exe',
];
const runtimeFiles = ['msvcp140.dll', 'vcruntime140.dll', 'vcruntime140_1.dll'];
const files = [...engineFiles, ...runtimeFiles];

function fail(message) {
  throw new Error(`stage-native-engine: ${message}`);
}

function run(command, args, cwd) {
  const result = spawnSync(command, args, {
    cwd,
    stdio: 'inherit',
    shell: process.platform === 'win32',
  });
  if (result.error) {
    if (result.error.code === 'ENOENT') {
      fail(`${command} was not found on PATH`);
    }
    fail(`${command} failed: ${result.error.message}`);
  }
  if (result.status !== 0) {
    fail(`${command} exited with code ${result.status}`);
  }
}

if (process.platform !== 'win32') {
  fail('the native FluidEQ Engine is Windows-only');
}

mkdirSync(nativeBin, { recursive: true });

function listDirectories(directory) {
  let entries = [];
  try {
    entries = readdirSync(directory, { withFileTypes: true });
  } catch {
    return [];
  }
  return entries.filter((entry) => entry.isDirectory()).map((entry) => entry.name);
}

function directoryHasRuntimeFiles(directory) {
  return runtimeFiles.every((file) => {
    try {
      return statSync(path.join(directory, file)).isFile();
    } catch {
      return false;
    }
  });
}

function findRuntimeDirectory() {
  const candidates = [];
  const redistFromEnvironment = process.env.VCToolsRedistDir;
  if (redistFromEnvironment) {
    candidates.push(path.resolve(redistFromEnvironment));
  }

  const visualStudioRoot = path.join(process.env.ProgramFiles || 'C:\\Program Files', 'Microsoft Visual Studio');
  for (const year of listDirectories(visualStudioRoot)) {
    const installRoot = path.join(visualStudioRoot, year);
    for (const edition of listDirectories(installRoot)) {
      candidates.push(
        path.join(installRoot, edition, 'VC', 'Redist', 'MSVC'),
      );
    }
  }

  for (const candidate of candidates) {
    if (!directoryHasRuntimeFiles(candidate)) {
      for (const version of listDirectories(candidate).sort().reverse()) {
        const versionRoot = path.join(candidate, version);
        for (const crt of listDirectories(path.join(versionRoot, 'x64'))) {
          const crtDirectory = path.join(versionRoot, 'x64', crt);
          if (directoryHasRuntimeFiles(crtDirectory)) {
            return crtDirectory;
          }
        }
      }
    }
  }
  return undefined;
}

const runtimeDirectory = findRuntimeDirectory();
if (!runtimeDirectory) {
  fail(`could not find the MSVC runtime DLLs (${runtimeFiles.join(', ')}) next to the engine build; run this under the MSVC developer environment`);
}
for (const file of runtimeFiles) {
  copyFileSync(path.join(runtimeDirectory, file), path.join(nativeBin, file));
}

run('node', [path.join(nativeDir, 'tools', 'generate-parameters.mjs')], repoRoot);

const cmakeBase = ['-S', 'native', '-B', path.relative(repoRoot, nativeBuild)];
const cmakeVersion = [
  `-DFEQ_CORE_VERSION=${version}`,
  `-DFEQ_BUILD_REVISION=${revision}`,
];
run('cmake', [...cmakeBase, '-A', 'x64', ...cmakeVersion], repoRoot);
run(
  'cmake',
  [
    '--build',
    path.relative(repoRoot, nativeBuild),
    '--config',
    'Release',
    '--target',
    'fluideq-engine',
    'fluideq-engine-setup',
    'fluideq-dsp-host',
    '--parallel',
  ],
  repoRoot,
);

mkdirSync(engineDir, { recursive: true });
for (const file of files) {
  const source = path.join(nativeBin, file);
  try {
    if (!statSync(source).isFile()) fail(`${source} is not a file`);
  } catch {
    fail(`missing build output ${source}`);
  }
  copyFileSync(source, path.join(engineDir, file));
}

const baseConfig = JSON.parse(
  readFileSync(path.join(shellDir, 'tauri.conf.json'), 'utf8'),
);
baseConfig.bundle = {
  ...(baseConfig.bundle || {}),
  resources: files.map((file) => `engine-bin/${file}`),
};
writeFileSync(overridePath, `${JSON.stringify(baseConfig, null, 2)}\n`);
console.log(
  `staged native engine for the installer: ${files.join(', ')} -> ${path.relative(
    repoRoot,
    engineDir,
  )}`,
);
