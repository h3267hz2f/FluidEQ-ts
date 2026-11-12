#!/usr/bin/env node
/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

/**
 * Emit `native/dsp-core/include/fluideq/parameters.h` from the one parameter
 * table in `native/parameters/nativeParameters.ts`.
 *
 * This mirrors `rust/fluideq-dsp-sys/build.rs`. The native engine build runs
 * CMake directly, outside Cargo, so it needs the same generated header before
 * the first translation unit is compiled. The header stays gitignored: a
 * committed copy is a copy that can drift from the table.
 */
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const nativeDir = path.resolve(here, '..');
const repoRoot = path.resolve(nativeDir, '..');
const source = path.join(
  repoRoot,
  'native',
  'parameters',
  'nativeParameters.ts',
);
const output = path.join(
  nativeDir,
  'dsp-core',
  'include',
  'fluideq',
  'parameters.h',
);

function fail(message) {
  throw new Error(`nativeParameters.ts: ${message}`);
}

/** `exciter.bands[].freqHz` becomes `EXCITER_BANDS_FREQ_HZ`. */
function macroName(parameterPath) {
  return parameterPath
    .replaceAll('[]', '')
    .split('.')
    .map((segment) => {
      const characters = [...segment];
      let out = '';
      for (let index = 0; index < characters.length; index += 1) {
        const ch = characters[index];
        const previous = index > 0 ? characters[index - 1] : undefined;
        if (
          ch >= 'A' &&
          ch <= 'Z' &&
          previous !== undefined &&
          ((previous >= 'a' && previous <= 'z') ||
            (previous >= '0' && previous <= '9'))
        ) {
          out += '_';
        }
        out += ch.toUpperCase();
      }
      return out;
    })
    .join('_');
}

function parseParameters(text) {
  const marker = 'NATIVE_DSP_PARAMETERS = [';
  const markerAt = text.indexOf(marker);
  if (markerAt < 0) fail(`\`${marker}\` not found`);
  const start = markerAt + marker.length;
  const endAt = text.indexOf('] as const', start);
  const body = text.slice(start, endAt < 0 ? text.length : endAt);

  const parameters = [];
  let depth = 0;
  let objectStart = 0;
  for (let index = 0; index < body.length; index += 1) {
    const ch = body[index];
    if (ch === '{') {
      if (depth === 0) objectStart = index;
      depth += 1;
    } else if (ch === '}' && depth > 0) {
      depth -= 1;
      if (depth === 0) {
        const object = body.slice(objectStart, index + 1);
        const idAt = object.indexOf('id:');
        if (idAt < 0) fail('parameter without an id');
        const idMatch = object
          .slice(idAt + 'id:'.length)
          .trimStart()
          .match(/^\d+/);
        if (!idMatch) fail('parameter without an id');
        const id = Number(idMatch[0]);

        const pathAt = object.indexOf('path:');
        if (pathAt < 0) fail(`parameter ${id} without a path`);
        let rest = object.slice(pathAt + 'path:'.length).trimStart();
        const quote = rest[0];
        if (quote !== "'" && quote !== '"') {
          fail(`parameter ${id} path is not a string literal`);
        }
        rest = rest.slice(1);
        const pathEnd = rest.indexOf(quote);
        if (pathEnd < 0) fail(`parameter ${id} path is unterminated`);

        parameters.push({
          id,
          path: rest.slice(0, pathEnd),
          structural: object.includes('structural: true'),
        });
      }
    }
  }

  if (parameters.length === 0) fail('no parameters parsed');
  return parameters;
}

function parseSchemaVersion(text) {
  const marker = 'NATIVE_DSP_PARAMETER_SCHEMA_VERSION = ';
  const markerAt = text.indexOf(marker);
  if (markerAt < 0) fail('schema version not found');
  const match = text
    .slice(markerAt + marker.length)
    .trimStart()
    .match(/^\d+/);
  if (!match) fail('schema version is not a number');
  return Number(match[0]);
}

function buildHeader(parameters, schemaVersion) {
  const lines = [
    '/*',
    '<FluidEQ: System-wide parametric audio equalizer interface>',
    'Copyright (C) <2026>  <Ivan Carmenates Garcia>',
    'SPDX-License-Identifier: GPL-3.0-or-later',
    '*/',
    '',
    '/*',
    ' * GENERATED FILE. Do not edit.',
    ' *',
    ' * Source: native/parameters/nativeParameters.ts',
    ' * Writer: native/tools/generate-parameters.mjs',
    ' */',
    '#ifndef FLUIDEQ_PARAMETERS_H',
    '#define FLUIDEQ_PARAMETERS_H',
    '',
    '#include <stdint.h>',
    '',
    `#define FEQ_PARAMETER_SCHEMA_VERSION ${schemaVersion}`,
    `#define FEQ_PARAMETER_COUNT ${parameters.length}`,
    '',
    'enum FeqParameterId {',
  ];
  for (const parameter of parameters) {
    lines.push(
      `  FEQ_PARAM_${macroName(parameter.path)} = ${parameter.id},`,
    );
  }
  lines.push('};');
  lines.push('');
  lines.push(
    'static const uint32_t FEQ_PARAMETER_IDS[FEQ_PARAMETER_COUNT] = {',
  );
  for (const parameter of parameters) {
    lines.push(`  ${parameter.id}u, /* ${parameter.path} */`);
  }
  lines.push('};');
  lines.push('');
  lines.push(
    'static const uint8_t FEQ_PARAMETER_STRUCTURAL[FEQ_PARAMETER_COUNT] = {',
  );
  for (const parameter of parameters) {
    lines.push(
      `  ${parameter.structural ? 1 : 0}, /* ${parameter.path} */`,
    );
  }
  lines.push('};');
  lines.push('');
  lines.push('static inline int feq_parameter_slot(uint32_t id) {');
  lines.push(
    '  for (int slot = 0; slot < FEQ_PARAMETER_COUNT; ++slot) {',
  );
  lines.push('    if (FEQ_PARAMETER_IDS[slot] == id) {');
  lines.push('      return slot;');
  lines.push('    }');
  lines.push('  }');
  lines.push('  return -1;');
  lines.push('}');
  lines.push('');
  lines.push('#endif /* FLUIDEQ_PARAMETERS_H */');
  lines.push('');
  return lines.join('\n');
}

const text = readFileSync(source, 'utf8');
const parameters = parseParameters(text);
const schemaVersion = parseSchemaVersion(text);
const generated = buildHeader(parameters, schemaVersion);

mkdirSync(path.dirname(output), { recursive: true });

let existing = '';
try {
  existing = readFileSync(output, 'utf8');
} catch {
  existing = '';
}
if (existing === generated) {
  console.log(`native parameters unchanged (${parameters.length})`);
} else {
  writeFileSync(output, generated, 'utf8');
  console.log(
    `native parameters written: ${parameters.length} ids -> ${path.relative(repoRoot, output)}`,
  );
}
