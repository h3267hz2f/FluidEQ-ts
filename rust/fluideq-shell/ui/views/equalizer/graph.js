/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

// The response graph: the same biquads the native core runs, evaluated across
// the audio band. It owns the canvas and nothing else — the view passes the
// curve, the preamp and the selected handle in on every frame, so a redraw
// never reads state it does not have.

export const F_MIN = 20;
export const F_MAX = 20_000;

const DB_MIN = -18;
const DB_MAX = 18;
const PAD = { left: 44, right: 20, top: 10, bottom: 22 };

const clamp = (value, low, high) => Math.min(high, Math.max(low, value));

function modelledQuality(filterType, gainDb, quality, eqModel, eqModelAmount) {
  const q = clamp(Number.isFinite(quality) ? quality : Math.SQRT1_2, 0.1, 18);
  const noGain =
    filterType === 'BP' ||
    filterType === 'LPQ' ||
    filterType === 'HPQ' ||
    filterType === 'NO';
  const model = String(eqModel || 'clean').toLowerCase();
  const amount = clamp(Number(eqModelAmount) || 0, 0, 1);
  if (
    model === 'clean' ||
    model === 'feq_eq_model_clean' ||
    amount <= 0 ||
    !Number.isFinite(gainDb) ||
    gainDb === 0 ||
    noGain
  ) {
    return q;
  }
  const driven = Math.min(Math.abs(gainDb), 20);
  const narrowing = Math.sqrt(1 + driven / 12);
  const factor = 1 + amount * (narrowing - 1);
  if (model === 'proportional' || model === 'feq_eq_model_proportional') {
    return Math.min(q * factor, 18);
  }
  if (model === 'asymmetric' || model === 'feq_eq_model_asymmetric') {
    const shaped = gainDb > 0 ? q / factor : q * factor;
    return clamp(shaped, 0.25, 18);
  }
  if (model === 'wide' || model === 'feq_eq_model_wide') {
    const isShelf = filterType === 'LSC' || filterType === 'HSC';
    const full = isShelf ? 0.4 : 0.45;
    return Math.max(q * (1 - amount * (1 - full)), 0.25);
  }
  return q;
}

function coefficients(band, sampleRate, eqModel = 'clean', eqModelAmount = 1) {
  const frequency = clamp(Number(band.frequency) || 1000, 10, sampleRate / 2 - 1);
  const type = band.filter_type || 'PK';
  const gain = clamp(Number(band.gain_db) || 0, -60, 60);
  const q = modelledQuality(
    type,
    gain,
    clamp(Number(band.quality) || 1, 0.05, 40),
    eqModel,
    eqModelAmount,
  );
  const w0 = (2 * Math.PI * frequency) / sampleRate;
  const cosw = Math.cos(w0);
  const sinw = Math.sin(w0);
  const alpha = sinw / (2 * q);
  const a = 10 ** (gain / 40);

  if (type === 'LSC' || type === 'HSC') {
    const sqrtA = Math.sqrt(a);
    const common = 2 * sqrtA * alpha;
    if (type === 'LSC') {
      return {
        b0: a * (a + 1 - (a - 1) * cosw + common),
        b1: 2 * a * (a - 1 - (a + 1) * cosw),
        b2: a * (a + 1 - (a - 1) * cosw - common),
        a0: a + 1 + (a - 1) * cosw + common,
        a1: -2 * (a - 1 + (a + 1) * cosw),
        a2: a + 1 + (a - 1) * cosw - common,
      };
    }
    return {
      b0: a * (a + 1 + (a - 1) * cosw + common),
      b1: -2 * a * (a - 1 + (a + 1) * cosw),
      b2: a * (a + 1 + (a - 1) * cosw - common),
      a0: a + 1 - (a - 1) * cosw + common,
      a1: 2 * (a - 1 - (a + 1) * cosw),
      a2: a + 1 - (a - 1) * cosw - common,
    };
  }

  if (type === 'NO') {
    return {
      b0: 1,
      b1: -2 * cosw,
      b2: 1,
      a0: 1 + alpha,
      a1: -2 * cosw,
      a2: 1 - alpha,
    };
  }

  if (type === 'LPQ') {
    return {
      b0: (1 - cosw) / 2,
      b1: 1 - cosw,
      b2: (1 - cosw) / 2,
      a0: 1 + alpha,
      a1: -2 * cosw,
      a2: 1 - alpha,
    };
  }

  if (type === 'HPQ') {
    return {
      b0: (1 + cosw) / 2,
      b1: -(1 + cosw),
      b2: (1 + cosw) / 2,
      a0: 1 + alpha,
      a1: -2 * cosw,
      a2: 1 - alpha,
    };
  }

  if (type === 'BP') {
    return {
      b0: alpha,
      b1: 0,
      b2: -alpha,
      a0: 1 + alpha,
      a1: -2 * cosw,
      a2: 1 - alpha,
    };
  }

  return {
    b0: 1 + alpha * a,
    b1: -2 * cosw,
    b2: 1 - alpha * a,
    a0: 1 + alpha / a,
    a1: -2 * cosw,
    a2: 1 - alpha / a,
  };
}

function magnitudeDb(band, frequency, sampleRate, eqModel = 'clean', eqModelAmount = 1) {
  const c = coefficients(band, sampleRate, eqModel, eqModelAmount);
  const w = (2 * Math.PI * frequency) / sampleRate;
  const cosw = Math.cos(w);
  const sinw = Math.sin(w);
  const cos2w = Math.cos(2 * w);
  const sin2w = Math.sin(2 * w);
  const numerator = Math.hypot(
    c.b0 + c.b1 * cosw + c.b2 * cos2w,
    -(c.b1 * sinw + c.b2 * sin2w),
  );
  const denominator = Math.hypot(
    c.a0 + c.a1 * cosw + c.a2 * cos2w,
    -(c.a1 * sinw + c.a2 * sin2w),
  );
  return 20 * Math.log10(Math.max(numerator / Math.max(denominator, 1e-12), 1e-9));
}

function responseDb(
  bands,
  preampDb,
  frequency,
  sampleRate,
  tone = null,
  eqModel = 'clean',
  eqModelAmount = 1,
) {
  let db = Number(preampDb) || 0;
  for (const band of bands) {
    if (band.enabled) {
      db += magnitudeDb(band, frequency, sampleRate, eqModel, eqModelAmount);
    }
  }
  if (tone) {
    const bassDb = Number(tone.bass_db) || 0;
    const trebleDb = Number(tone.treble_db) || 0;
    const lowCutHz = Number(tone.low_cut_hz) || 20;
    const highCutHz = Number(tone.high_cut_hz) || 20000;
    if (Math.abs(bassDb) > 1e-4) {
      db += magnitudeDb(
        {
          filter_type: 'LSC',
          frequency: Number(tone.bass_freq_hz) || 105,
          gain_db: bassDb,
          quality: Number(tone.bass_q) || 0.71,
        },
        frequency,
        sampleRate,
      );
    }
    if (Math.abs(trebleDb) > 1e-4) {
      db += magnitudeDb(
        {
          filter_type: 'HSC',
          frequency: Number(tone.treble_freq_hz) || 8500,
          gain_db: trebleDb,
          quality: Number(tone.treble_q) || 0.71,
        },
        frequency,
        sampleRate,
      );
    }
    if (lowCutHz > 20.5) {
      db += magnitudeDb(
        {
          filter_type: 'HPQ',
          frequency: lowCutHz,
          gain_db: 0,
          quality: Math.SQRT1_2,
        },
        frequency,
        sampleRate,
      );
    }
    if (highCutHz < 19950) {
      db += magnitudeDb(
        {
          filter_type: 'LPQ',
          frequency: highCutHz,
          gain_db: 0,
          quality: Math.SQRT1_2,
        },
        frequency,
        sampleRate,
      );
    }
  }
  return db;
}

export function xOf(frequency, width) {
  const ratio =
    Math.log10(clamp(frequency, F_MIN, F_MAX) / F_MIN) /
    Math.log10(F_MAX / F_MIN);
  return PAD.left + ratio * (width - PAD.left - PAD.right);
}

export function frequencyOf(x, width) {
  const ratio = clamp(
    (x - PAD.left) / Math.max(width - PAD.left - PAD.right, 1),
    0,
    1,
  );
  return F_MIN * (F_MAX / F_MIN) ** ratio;
}

export function yOf(db, height) {
  const ratio = (DB_MAX - clamp(db, DB_MIN, DB_MAX)) / (DB_MAX - DB_MIN);
  return PAD.top + ratio * (height - PAD.top - PAD.bottom);
}

export function dbOf(y, height) {
  const ratio = clamp(
    (y - PAD.top) / Math.max(height - PAD.top - PAD.bottom, 1),
    0,
    1,
  );
  return DB_MAX - ratio * (DB_MAX - DB_MIN);
}

export function createGraph(canvas) {
  function prepareCanvas() {
    const rect = canvas.getBoundingClientRect();
    if (rect.width < 4 || rect.height < 4) return null;
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const width = Math.round(rect.width * dpr);
    const height = Math.round(rect.height * dpr);
    if (canvas.width !== width || canvas.height !== height) {
      canvas.width = width;
      canvas.height = height;
    }
    const ctx = canvas.getContext('2d');
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    return { ctx, width: rect.width, height: rect.height };
  }

  function drawGrid(ctx, width, height) {
    const frequencies = [20, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000];
    ctx.save();
    ctx.font =
      '10px -apple-system, "Segoe UI", Ubuntu, Cantarell, sans-serif';
    ctx.textBaseline = 'middle';

    for (const frequency of frequencies) {
      const x = xOf(frequency, width);
      ctx.strokeStyle =
        frequency === 1000 ? 'rgba(255,255,255,0.10)' : 'rgba(255,255,255,0.045)';
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(x, PAD.top);
      ctx.lineTo(x, height - PAD.bottom);
      ctx.stroke();

      ctx.fillStyle = 'rgba(125,138,153,0.62)';
      ctx.textAlign = 'center';
      ctx.fillText(
        frequency >= 1000 ? `${frequency / 1000}k` : String(frequency),
        x,
        height - PAD.bottom + 12,
      );
    }

    for (const db of [18, 12, 6, 0, -6, -12, -18]) {
      const y = yOf(db, height);
      ctx.strokeStyle =
        db === 0 ? 'rgba(255,255,255,0.12)' : 'rgba(255,255,255,0.045)';
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(PAD.left, y);
      ctx.lineTo(width - PAD.right, y);
      ctx.stroke();

      ctx.fillStyle = 'rgba(125,138,153,0.62)';
      ctx.textAlign = 'right';
      ctx.fillText(db > 0 ? `+${db}` : String(db), PAD.left - 9, y);
    }
    ctx.restore();
  }

  function draw({
    bands = [],
    preampDb = 0,
    selectedId = null,
    sampleRate = 48_000,
    tone = null,
    eqModel = 'clean',
    eqModelAmount = 1,
  }) {
    const prepared = prepareCanvas();
    if (!prepared) return;
    const { ctx, width, height } = prepared;

    ctx.clearRect(0, 0, width, height);
    drawGrid(ctx, width, height);

    const left = PAD.left;
    const right = width - PAD.right;
    const bottom = height - PAD.bottom;
    const points = [];
    const samples = Math.max(260, Math.round((right - left) * 0.55));
    for (let index = 0; index <= samples; index += 1) {
      const ratio = index / samples;
      const frequency = F_MIN * (F_MAX / F_MIN) ** ratio;
      points.push([
        left + ratio * (right - left),
        yOf(
          responseDb(
            bands,
            preampDb,
            frequency,
            sampleRate,
            tone,
            eqModel,
            eqModelAmount,
          ),
          height,
        ),
      ]);
    }

    const fill = ctx.createLinearGradient(0, PAD.top, 0, bottom);
    fill.addColorStop(0, 'rgba(59,232,196,0.20)');
    fill.addColorStop(0.55, 'rgba(59,232,196,0.06)');
    fill.addColorStop(1, 'rgba(59,232,196,0.005)');
    ctx.beginPath();
    ctx.moveTo(points[0][0], points[0][1]);
    for (let index = 1; index < points.length; index += 1) {
      ctx.lineTo(points[index][0], points[index][1]);
    }
    ctx.lineTo(right, bottom);
    ctx.lineTo(left, bottom);
    ctx.closePath();
    ctx.fillStyle = fill;
    ctx.fill();

    const stroke = ctx.createLinearGradient(left, 0, right, 0);
    stroke.addColorStop(0, 'rgba(124,160,255,0.88)');
    stroke.addColorStop(0.4, '#3be8c4');
    stroke.addColorStop(1, '#ffcf8a');

    ctx.save();
    ctx.beginPath();
    ctx.moveTo(points[0][0], points[0][1]);
    for (let index = 1; index < points.length; index += 1) {
      ctx.lineTo(points[index][0], points[index][1]);
    }
    ctx.lineWidth = 9;
    ctx.strokeStyle = 'rgba(59,232,196,0.055)';
    ctx.stroke();
    ctx.lineWidth = 2.2;
    ctx.strokeStyle = stroke;
    ctx.shadowColor = 'rgba(59,232,196,0.38)';
    ctx.shadowBlur = 14;
    ctx.stroke();
    ctx.restore();

    const selected = bands.find((band) => band.id === selectedId) ?? null;
    if (selected) {
      const x = xOf(selected.frequency, width);
      ctx.save();
      ctx.setLineDash([3, 4]);
      ctx.strokeStyle = 'rgba(255,180,94,0.28)';
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(x, PAD.top);
      ctx.lineTo(x, bottom);
      ctx.stroke();
      ctx.restore();
    }

    for (const current of bands) {
      if (!current.enabled) continue;
      const x = xOf(current.frequency, width);
      const y = yOf(current.gain_db, height);
      const isSelected = current.id === selectedId;
      ctx.beginPath();
      ctx.arc(x, y, isSelected ? 4.6 : 3.1, 0, Math.PI * 2);
      ctx.fillStyle = isSelected ? '#0b1517' : '#3be8c4';
      ctx.fill();
      ctx.lineWidth = isSelected ? 2.2 : 1;
      ctx.strokeStyle = isSelected ? '#ffb45e' : 'rgba(5,7,11,0.9)';
      ctx.stroke();
      if (isSelected) {
        ctx.beginPath();
        ctx.arc(x, y, 8.5, 0, Math.PI * 2);
        ctx.strokeStyle = 'rgba(255,180,94,0.26)';
        ctx.lineWidth = 1;
        ctx.stroke();
      }
    }
  }

  return { draw };
}
