/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

// The DSP rack: the stages in the order the chain runs them, one switch each.
// The names, the descriptions and the one line that says what a stage is set
// to live here, so the page stays a list and app.js keeps owning the commands.

const db = (value) => {
  const number = Number(value) || 0;
  const rounded = Math.abs(number) < 0.05 ? 0 : number;
  return `${rounded > 0 ? '+' : ''}${rounded.toFixed(1)}`;
};

const pct = (value) => `${Math.round((Number(value) || 0) * 100)}%`;

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
  },
  {
    id: 'denoise',
    label: 'Denoise',
    badge: '⇪',
    description: 'Removes hiss, mains hum and clicks without gating the programme.',
    summary: (s) => `Hiss floor ${db(s.denoise?.hiss?.floor_db)} dB`,
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
  },
  {
    id: 'exciter',
    label: 'Exciter',
    badge: '↗',
    description: 'Tube body, tape air and transient focus.',
    summary: (s) => `Organic ${pct(s.exciter?.organic_amount)}`,
  },
  {
    id: 'bass-forge',
    label: 'Bass Forge',
    badge: '∿',
    description: 'Synthesises the missing fundamental below the driver’s roll-off.',
    summary: (s) =>
      `Drive ${db(s.bass_forge?.drive_db)} dB · mix ${pct(s.bass_forge?.mix)}`,
  },
  {
    id: 'equaliser',
    label: 'Equaliser',
    badge: '⌁',
    description: 'The parametric curve, with dynamic thresholds and per-ear bands.',
    summary: (s) => `${Number(s.eq?.band_count) || 0} bands`,
  },
  {
    id: 'bass-punch',
    label: 'Bass Punch',
    badge: '∧',
    description: 'Shapes the first milliseconds of low-end attack.',
    summary: (s) =>
      `Attack ${pct(s.bass_punch?.attack)} · mix ${pct(s.bass_punch?.mix)}`,
  },
  {
    id: 'dimension',
    label: 'Dimension',
    badge: '↔',
    description: 'Touches only the sides, so a mono listener loses nothing.',
    summary: (s) => `Mid width ${pct(s.dimension?.mid_width)}`,
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
