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
        max: 20,
        step: 0.1,
        format: formatMeters,
        value: (s) => Number(s.room?.size_m ?? 6),
      },
      {
        id: 'distance_m',
        label: 'Listening distance',
        min: 0.3,
        max: 10,
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
  id === 'equaliser' ? settings?.eq : settings?.[id.replace('-', '_')];

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
  eyebrow: 'Eleven stages',
};
