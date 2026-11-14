/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

// The DSP rack: the stages in the order the chain runs them, one switch each,
// and the handful of controls a stage is actually tuned with. The names, the
// descriptions, the ranges and the one line that says what a stage is set to
// live here, so the page stays a list and app.js keeps owning the commands.

const db = (value) => {
  const number = Number(value) || 0;
  const rounded = Math.abs(number) < 0.05 ? 0 : number;
  return `${rounded > 0 ? '+' : ''}${rounded.toFixed(1)}`;
};

const pct = (value) => `${Math.round((Number(value) || 0) * 100)}%`;
const formatDb = (value) => `${db(value)} dB`;
const formatLufs = (value) => `${(Number(value) || 0).toFixed(1)} LUFS`;
const formatHz = (value) => `${Math.round(Number(value) || 0)} Hz`;
const formatMeters = (value) => `${(Number(value) || 0).toFixed(1)} m`;

const dynamicsBandParameters = () =>
  [
    { key: 'band1', index: 0, label: 'Low' },
    { key: 'band2', index: 1, label: 'Mid' },
    { key: 'band3', index: 2, label: 'High' },
  ].flatMap(({ key, index, label }) => {
    const band = (s) => s.compander?.bands?.[index] ?? {};
    return [
      {
        id: `${key}_up_threshold_db`,
        label: `${label} lift under`,
        min: -80,
        max: -10,
        step: 1,
        format: formatDb,
        value: (s) => Number(band(s).up_threshold_db ?? -40),
      },
      {
        id: `${key}_up_ratio`,
        label: `${label} lift ratio`,
        min: 1,
        max: 4,
        step: 0.1,
        format: (v) => `${(Number(v) || 1).toFixed(1)}:1`,
        value: (s) => Number(band(s).up_ratio ?? 1),
      },
      {
        id: `${key}_up_max_db`,
        label: `${label} most lift`,
        min: 0,
        max: 18,
        step: 0.5,
        format: formatDb,
        value: (s) => Number(band(s).up_max_db ?? 0),
      },
      {
        id: `${key}_down_threshold_db`,
        label: `${label} hold over`,
        min: -60,
        max: 0,
        step: 1,
        format: formatDb,
        value: (s) => Number(band(s).down_threshold_db ?? -20),
      },
      {
        id: `${key}_down_ratio`,
        label: `${label} hold ratio`,
        min: 1,
        max: 20,
        step: 0.1,
        format: (v) => `${(Number(v) || 1).toFixed(1)}:1`,
        value: (s) => Number(band(s).down_ratio ?? 1),
      },
    ];
  });

const normalizerMode = (setting) => {
  const mode = Number(setting?.mode) || 0;
  if (mode === 2) return 'Leveler';
  if (mode === 1) return 'Guard';
  return 'Off';
};

export const rackStages = [
  {
    id: 'normalizer',
    label: 'Normalizer',
    badge: '≈',
    description: 'True-peak and per-song loudness memory ahead of the rack.',
    summary: (s) =>
      `Target ${db(s.normalizer?.target_lufs)} LUFS · ${normalizerMode(s.normalizer)}`,
    parameters: [
      {
        id: 'target_lufs',
        label: 'Target loudness',
        min: -35,
        max: -5,
        step: 0.5,
        format: formatLufs,
        value: (s) => Number(s.normalizer?.target_lufs ?? -14),
      },
      {
        id: 'ceiling_db',
        label: 'True-peak ceiling',
        min: -6,
        max: 0,
        step: 0.1,
        format: formatDb,
        value: (s) => Number(s.normalizer?.ceiling_db ?? -1),
      },
    ],
  },
  {
    id: 'denoise',
    label: 'Denoise',
    badge: '⇪',
    description: 'Removes hiss, mains hum and clicks without gating the programme.',
    summary: (s) => `Hiss floor ${db(s.denoise?.hiss?.floor_db)} dB`,
    parameters: [
      {
        id: 'hiss_amount',
        label: 'Hiss reduction',
        min: 0,
        max: 1,
        step: 0.01,
        format: pct,
        value: (s) => Number(s.denoise?.hiss?.amount ?? 0.5),
      },
      {
        id: 'hiss_floor_db',
        label: 'Noise floor',
        min: -90,
        max: -30,
        step: 1,
        format: formatDb,
        value: (s) => Number(s.denoise?.hiss?.floor_db ?? -60),
      },
    ],
  },
  {
    id: 'deesser',
    label: 'De-esser',
    badge: 'S',
    description: 'Turns down the sibilant band without dulling cymbals.',
    summary: (s) =>
      `Range ${db(s.deesser?.range_db)} dB · ${Math.round(
        Number(s.deesser?.low_hz) || 0
      )}–${Math.round(Number(s.deesser?.high_hz) || 0)} Hz`,
    parameters: [
      {
        id: 'sensitivity',
        label: 'Sensitivity',
        min: 0,
        max: 1,
        step: 0.01,
        format: pct,
        value: (s) => Number(s.deesser?.sensitivity ?? 0.55),
      },
      {
        id: 'range_db',
        label: 'Range',
        min: 0,
        max: 24,
        step: 0.5,
        format: formatDb,
        value: (s) => Number(s.deesser?.range_db ?? 6),
      },
      {
        id: 'low_hz',
        label: 'Lowest band',
        min: 1000,
        max: 16000,
        step: 100,
        format: formatHz,
        value: (s) => Number(s.deesser?.low_hz ?? 4500),
      },
      {
        id: 'high_hz',
        label: 'Highest band',
        min: 2000,
        max: 20000,
        step: 100,
        format: formatHz,
        value: (s) => Number(s.deesser?.high_hz ?? 11000),
      },
    ],
  },
  {
    id: 'exciter',
    label: 'Exciter',
    badge: '↗',
    description: 'Tube body, tape air and transient focus.',
    summary: (s) => `Organic ${pct(s.exciter?.organic_amount)}`,
    parameters: [
      {
        id: 'organic_amount',
        label: 'Organic density',
        min: 0,
        max: 1,
        step: 0.01,
        format: pct,
        value: (s) => Number(s.exciter?.organic_amount ?? 0.4),
      },
    ],
  },
  {
    id: 'bass-forge',
    label: 'Bass Forge',
    badge: '∿',
    description: 'Synthesises the missing fundamental below the driver’s roll-off.',
    summary: (s) =>
      `Drive ${db(s.bass_forge?.drive_db)} dB · mix ${pct(s.bass_forge?.mix)}`,
    parameters: [
      {
        id: 'drive_db',
        label: 'Drive',
        min: 0,
        max: 18,
        step: 0.1,
        format: formatDb,
        value: (s) => Number(s.bass_forge?.drive_db ?? 0),
      },
      {
        id: 'sub_amount',
        label: 'Sub harmonic',
        min: 0,
        max: 1,
        step: 0.01,
        format: pct,
        value: (s) => Number(s.bass_forge?.sub_amount ?? 0.5),
      },
      {
        id: 'mix',
        label: 'Mix',
        min: 0,
        max: 1,
        step: 0.01,
        format: pct,
        value: (s) => Number(s.bass_forge?.mix ?? 0.5),
      },
    ],
  },
  {
    id: 'equaliser',
    label: 'Equaliser',
    badge: '⌁',
    description: 'The parametric curve, with dynamic thresholds and per-ear bands.',
    summary: (s) => `${Number(s.eq?.band_count) || 0} bands`,
    parameters: [
      {
        id: 'model_amount',
        label: 'Model character',
        min: 0,
        max: 1,
        step: 0.01,
        format: pct,
        value: (s) => Number(s.eq?.model_amount ?? 1),
      },
    ],
  },
  {
    id: 'footsteps',
    label: 'Footsteps',
    badge: 'F',
    description:
      'Lifts a game’s quiet steps and calms its rumble without touching the rest.',
    summary: (s) =>
      `Lift ${pct(s.footsteps?.lift)} · calm ${pct(s.footsteps?.calm)}`,
    parameters: [
      {
        id: 'lift',
        label: 'Step lift',
        min: 0,
        max: 1,
        step: 0.01,
        format: pct,
        value: (s) => Number(s.footsteps?.lift ?? 0.85),
      },
      {
        id: 'calm',
        label: 'Rumble calm',
        min: 0,
        max: 1,
        step: 0.01,
        format: pct,
        value: (s) => Number(s.footsteps?.calm ?? 0),
      },
      {
        id: 'environment',
        label: 'Environment',
        min: -1,
        max: 1,
        step: 0.01,
        format: (v) => `${Number(v) > 0 ? '+' : ''}${Math.round((Number(v) || 0) * 100)}%`,
        value: (s) => Number(s.footsteps?.environment ?? 0),
      },
      {
        id: 'voices',
        label: 'Voices',
        min: 0,
        max: 1,
        step: 0.01,
        format: pct,
        value: (s) => Number(s.footsteps?.voices ?? 0),
      },
    ],
  },
  {
    id: 'bass-punch',
    label: 'Bass Punch',
    badge: '∧',
    description: 'Shapes the first milliseconds of low-end attack.',
    summary: (s) =>
      `Attack ${pct(s.bass_punch?.attack)} · mix ${pct(s.bass_punch?.mix)}`,
    parameters: [
      {
        id: 'attack',
        label: 'Attack',
        min: 0,
        max: 1,
        step: 0.01,
        format: pct,
        value: (s) => Number(s.bass_punch?.attack ?? 0.5),
      },
      {
        id: 'sustain',
        label: 'Sustain',
        min: 0,
        max: 1,
        step: 0.01,
        format: pct,
        value: (s) => Number(s.bass_punch?.sustain ?? 0.5),
      },
      {
        id: 'mix',
        label: 'Mix',
        min: 0,
        max: 1,
        step: 0.01,
        format: pct,
        value: (s) => Number(s.bass_punch?.mix ?? 0.5),
      },
    ],
  },
  {
    id: 'dimension',
    label: 'Dimension',
    badge: '↔',
    description: 'Touches only the sides, so a mono listener loses nothing.',
    summary: (s) => `Mid width ${pct(s.dimension?.mid_width)}`,
    parameters: [
      {
        id: 'mid_width',
        label: 'Side width',
        min: 0,
        max: 2,
        step: 0.01,
        format: pct,
        value: (s) => Number(s.dimension?.mid_width ?? 1),
      },
      {
        id: 'decorrelation',
        label: 'Decorrelation',
        min: 0,
        max: 1,
        step: 0.01,
        format: pct,
        value: (s) => Number(s.dimension?.decorrelation ?? 0.3),
      },
    ],
  },
  {
    id: 'room',
    label: 'Room',
    badge: '(○)',
    description: 'Binaural listening room with early reflections and late ambience.',
    summary: (s) =>
      `Room ${(Number(s.room?.size_m) || 0).toFixed(1)} m · ambience ${pct(
        s.room?.ambience_mix
      )}`,
    parameters: [
      {
        id: 'size_m',
        label: 'Room size',
        min: 2,
        max: 12,
        step: 0.1,
        format: formatMeters,
        value: (s) => Number(s.room?.size_m ?? 6),
      },
      {
        id: 'distance_m',
        label: 'Listening distance',
        min: 0.5,
        max: 6,
        step: 0.1,
        format: formatMeters,
        value: (s) => Number(s.room?.distance_m ?? 1),
      },
      {
        id: 'ambience_mix',
        label: 'Ambience',
        min: 0,
        max: 1,
        step: 0.01,
        format: pct,
        value: (s) => Number(s.room?.ambience_mix ?? 0.2),
      },
    ],
  },
  {
    id: 'dynamics',
    label: 'Dynamics',
    badge: '⇅',
    description:
      'Lifts what is quiet and holds what is loud, in three bands, before the ceiling.',
    summary: (s) =>
      `Make-up ${db(s.compander?.makeup_db)} dB · mix ${pct(s.compander?.mix)}`,
    parameters: [
      ...dynamicsBandParameters(),
      {
        id: 'attack_ms',
        label: 'Attack',
        min: 1,
        max: 100,
        step: 1,
        format: (v) => `${Math.round(Number(v) || 0)} ms`,
        value: (s) => Number(s.compander?.attack_ms ?? 10),
      },
      {
        id: 'release_ms',
        label: 'Release',
        min: 20,
        max: 1000,
        step: 5,
        format: (v) => `${Math.round(Number(v) || 0)} ms`,
        value: (s) => Number(s.compander?.release_ms ?? 200),
      },
      {
        id: 'mix',
        label: 'Mix',
        min: 0,
        max: 1,
        step: 0.01,
        format: pct,
        value: (s) => Number(s.compander?.mix ?? 1),
      },
      {
        id: 'makeup_db',
        label: 'Make-up',
        min: -12,
        max: 12,
        step: 0.5,
        format: formatDb,
        value: (s) => Number(s.compander?.makeup_db ?? 0),
      },
    ],
  },
  {
    id: 'maximizer',
    label: 'Maximizer',
    badge: '↑',
    description: 'Raises density while holding true peaks under the ceiling.',
    summary: (s) =>
      `Drive ${db(s.maximizer?.drive_db)} dB · ceiling ${db(
        s.maximizer?.ceiling_db
      )}`,
    parameters: [
      {
        id: 'drive_db',
        label: 'Drive',
        min: 0,
        max: 24,
        step: 0.1,
        format: formatDb,
        value: (s) => Number(s.maximizer?.drive_db ?? 0),
      },
    ],
  },
  {
    id: 'master',
    label: 'Master',
    badge: '⇥',
    description: 'Delivery trim, target loudness and the true-peak limiter.',
    summary: (s) =>
      `Trim ${db(s.master?.output_trim_db)} dB · target ${db(
        s.master?.loudness_target_lufs
      )} LUFS`,
    parameters: [
      {
        id: 'output_trim_db',
        label: 'Output trim',
        min: -24,
        max: 12,
        step: 0.1,
        format: formatDb,
        value: (s) => Number(s.master?.output_trim_db ?? 0),
      },
      {
        id: 'loudness_target_lufs',
        label: 'Target loudness',
        min: -24,
        max: -8,
        step: 0.5,
        format: formatLufs,
        value: (s) => Number(s.master?.loudness_target_lufs ?? -14),
      },
    ],
  },
];

const settingFor = (settings, id) =>
  id === 'equaliser'
    ? settings?.eq
    : id === 'dynamics'
      ? settings?.compander
      : settings?.[id.replace('-', '_')];

export const ISOLATE_STAGES = [
  'denoise',
  'deesser',
  'exciter',
  'equaliser',
  'footsteps',
  'bass-forge',
  'bass-punch',
];

export function stageSupportsIsolate(id) {
  return ISOLATE_STAGES.includes(id);
}

export function stageIsolate(settings, id) {
  return Number(settingFor(settings, id)?.isolate) > 0;
}

export const NORMALIZER_MODES = [
  { id: 'off', label: 'Off', detail: 'Pass-through' },
  { id: 'guard', label: 'Guard', detail: 'Peak protection only' },
  { id: 'leveler', label: 'Leveler', detail: 'Per-song loudness memory' },
];

export function normalizerModeKey(settings) {
  const mode = Number(settings?.normalizer?.mode) || 0;
  if (mode === 2) return 'leveler';
  if (mode === 1) return 'guard';
  return 'off';
}

export const MASTER_TARGETS = [
  { id: 'streaming', label: 'Streaming', detail: '-14 LUFS · -1 dBTP' },
  { id: 'podcast', label: 'Podcast', detail: '-16 LUFS · -1 dBTP' },
  { id: 'reference', label: 'Reference', detail: '-18 LUFS · -1 dBTP' },
  { id: 'cinema', label: 'Cinema', detail: '-24 LUFS · -2 dBTP' },
];

export function stageIsOn(settings, id) {
  if (id === 'normalizer') return Number(settings?.normalizer?.mode) > 0;
  return Number(settingFor(settings, id)?.enabled) > 0;
}

export function stageSummary(stage, settings) {
  try {
    return stage.summary(settings ?? {});
  } catch {
    return '';
  }
}

export const rack = {
  id: 'rack',
  title: 'DSP rack',
  eyebrow: 'Thirteen stages',
};
