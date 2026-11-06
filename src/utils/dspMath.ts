/**
 * FluidEQ 2.1 — Pure DSP & Equalizer APO Math
 * Ported from native/dsp-core/src/biquad.cpp and native/dsp-core/src/biquad_matched.cpp
 * (RBJ Audio EQ Cookbook + Martin Vicanek's Matched Second-Order Digital Filters).
 */

export type FilterType = 'PK' | 'NO' | 'LSC' | 'HSC' | 'LPQ' | 'HPQ' | 'BP';

export type EqCharacterModel = 'clean' | 'proportional' | 'wide' | 'asymmetric';

export type TrebleDesignMode = 'precise' | 'classic';

export type EarChannel = 'both' | 'left' | 'right';

export interface EqBand {
  id: string;
  enabled: boolean;
  type: FilterType;
  frequency: number;
  gainDb: number;
  q: number;
  ear?: EarChannel;
}

export interface ToneSettings {
  lowCutSlope: 0 | 12 | 24;
  bassDb: number;
  midDb: number;
  trebleDb: number;
  highCutSlope: 0 | 12 | 24;
  enabled: boolean;
}

export interface BiquadCoefficients {
  b0: number;
  b1: number;
  b2: number;
  a1: number;
  a2: number;
}

const PI = 3.14159265358979323846;
const BUTTERWORTH_Q = 0.7071067811865475244;
const BUTTERWORTH_TOLERANCE = 0.02;

interface Denominator {
  a1: number;
  a2: number;
  A0: number;
  A1: number;
  A2: number;
}

interface Phis {
  phi0: number;
  phi1: number;
  phi2: number;
}

function root(value: number): number {
  return Math.sqrt(value > 0 ? value : 0);
}

function isFiniteCoeffs(k: BiquadCoefficients): boolean {
  return (
    Number.isFinite(k.b0) &&
    Number.isFinite(k.b1) &&
    Number.isFinite(k.b2) &&
    Number.isFinite(k.a1) &&
    Number.isFinite(k.a2) &&
    k.b0 !== 0
  );
}

function matchedPoles(w: number, zeta: number): Denominator {
  const decay = Math.exp(-zeta * w);
  const a1 =
    zeta <= 1.0
      ? -2.0 * decay * Math.cos(w * Math.sqrt(1.0 - zeta * zeta))
      : -2.0 * decay * Math.cosh(w * Math.sqrt(zeta * zeta - 1.0));
  const a2 = Math.exp(-2.0 * zeta * w);
  const A0 = (1.0 + a1 + a2) * (1.0 + a1 + a2);
  const A1 = (1.0 - a1 + a2) * (1.0 - a1 + a2);
  const A2 = -4.0 * a2;
  return { a1, a2, A0, A1, A2 };
}

function phisAt(w: number): Phis {
  const half = Math.sin(w / 2.0);
  const phi1 = half * half;
  const phi0 = 1.0 - phi1;
  const phi2 = 4.0 * phi0 * phi1;
  return { phi0, phi1, phi2 };
}

function unityOn(a1: number, a2: number): BiquadCoefficients {
  return { b0: 1.0, b1: a1, b2: a2, a1, a2 };
}

/**
 * Character model Q shaping from native/dsp-core/src/biquad.cpp (`modelled_quality`).
 */
export function modelledQuality(
  type: FilterType,
  gainDb: number,
  quality: number,
  model: EqCharacterModel,
  amount: number,
): number {
  const noGain = type === 'BP' || type === 'LPQ' || type === 'HPQ' || type === 'NO';
  if (model === 'clean' || amount <= 0 || gainDb === 0 || noGain) {
    return quality;
  }

  const driven = Math.min(Math.abs(gainDb), 20.0);
  const narrowing = Math.sqrt(1.0 + driven / 12.0);
  const factor = 1.0 + amount * (narrowing - 1.0);

  if (model === 'proportional') {
    const narrowed = quality * factor;
    return Math.min(narrowed, 18.0);
  }

  if (model === 'asymmetric') {
    const shaped = gainDb > 0 ? quality / factor : quality * factor;
    return Math.max(0.25, Math.min(18.0, shaped));
  }

  const isShelf = type === 'LSC' || type === 'HSC';
  const full = isShelf ? 0.4 : 0.45;
  const broadened = quality * (1.0 - amount * (1.0 - full));
  return Math.max(0.25, broadened);
}

/**
 * RBJ Audio EQ Cookbook coefficients (`feq_biquad_coefficients`).
 */
export function biquadCoefficientsCookbook(
  type: FilterType,
  frequency: number,
  gainDb: number,
  quality: number,
  sampleRate: number,
): BiquadCoefficients {
  const amplitude = Math.pow(10.0, gainDb / 40.0);
  const safeFrequency = Math.min(Math.max(frequency, 1.0), sampleRate * 0.499);
  const safeQ = Math.max(quality, 0.05);
  const omega = (2.0 * PI * safeFrequency) / sampleRate;
  const cosine = Math.cos(omega);
  const sine = Math.sin(omega);
  const alpha = sine / (2.0 * safeQ);

  let b0 = 1.0;
  let b1 = 0.0;
  let b2 = 0.0;
  let a0 = 1.0;
  let a1 = 0.0;
  let a2 = 0.0;

  switch (type) {
    case 'PK':
      b0 = 1.0 + alpha * amplitude;
      b1 = -2.0 * cosine;
      b2 = 1.0 - alpha * amplitude;
      a0 = 1.0 + alpha / amplitude;
      a1 = -2.0 * cosine;
      a2 = 1.0 - alpha / amplitude;
      break;

    case 'NO':
      b0 = 1.0;
      b1 = -2.0 * cosine;
      b2 = 1.0;
      a0 = 1.0 + alpha;
      a1 = -2.0 * cosine;
      a2 = 1.0 - alpha;
      break;

    case 'LPQ':
      b0 = (1.0 - cosine) / 2.0;
      b1 = 1.0 - cosine;
      b2 = (1.0 - cosine) / 2.0;
      a0 = 1.0 + alpha;
      a1 = -2.0 * cosine;
      a2 = 1.0 - alpha;
      break;

    case 'HPQ':
      b0 = (1.0 + cosine) / 2.0;
      b1 = -(1.0 + cosine);
      b2 = (1.0 + cosine) / 2.0;
      a0 = 1.0 + alpha;
      a1 = -2.0 * cosine;
      a2 = 1.0 - alpha;
      break;

    case 'BP':
      b0 = alpha;
      b1 = 0.0;
      b2 = -alpha;
      a0 = 1.0 + alpha;
      a1 = -2.0 * cosine;
      a2 = 1.0 - alpha;
      break;

    case 'LSC':
    case 'HSC': {
      const beta = 2.0 * Math.sqrt(amplitude) * alpha;
      if (type === 'LSC') {
        b0 = amplitude * (amplitude + 1.0 - (amplitude - 1.0) * cosine + beta);
        b1 = 2.0 * amplitude * (amplitude - 1.0 - (amplitude + 1.0) * cosine);
        b2 = amplitude * (amplitude + 1.0 - (amplitude - 1.0) * cosine - beta);
        a0 = amplitude + 1.0 + (amplitude - 1.0) * cosine + beta;
        a1 = -2.0 * (amplitude - 1.0 + (amplitude + 1.0) * cosine);
        a2 = amplitude + 1.0 + (amplitude - 1.0) * cosine - beta;
      } else {
        b0 = amplitude * (amplitude + 1.0 + (amplitude - 1.0) * cosine + beta);
        b1 = -2.0 * amplitude * (amplitude - 1.0 + (amplitude + 1.0) * cosine);
        b2 = amplitude * (amplitude + 1.0 + (amplitude - 1.0) * cosine - beta);
        a0 = amplitude + 1.0 - (amplitude - 1.0) * cosine + beta;
        a1 = 2.0 * (amplitude - 1.0 - (amplitude + 1.0) * cosine);
        a2 = amplitude + 1.0 - (amplitude - 1.0) * cosine - beta;
      }
      break;
    }
  }

  return {
    b0: b0 / a0,
    b1: b1 / a0,
    b2: b2 / a0,
    a1: a1 / a0,
    a2: a2 / a0,
  };
}

function matchedBell(w0: number, gainDb: number, quality: number): BiquadCoefficients {
  if (gainDb === 0) {
    const d = matchedPoles(w0, 1.0 / (2.0 * quality));
    return unityOn(d.a1, d.a2);
  }
  const cut = gainDb < 0;
  const g = Math.pow(10.0, Math.abs(gainDb) / 20.0);
  const d = matchedPoles(w0, 1.0 / (2.0 * quality * Math.sqrt(g)));
  const p = phisAt(w0);
  const B0 = d.A0;
  const R1 = (d.A0 * p.phi0 + d.A1 * p.phi1 + d.A2 * p.phi2) * g * g;
  const R2 = (-d.A0 + d.A1 + 4.0 * (p.phi0 - p.phi1) * d.A2) * g * g;
  const B2 = (R1 - R2 * p.phi1 - B0) / (4.0 * p.phi1 * p.phi1);
  const B1 = R2 + B0 + 4.0 * (p.phi1 - p.phi0) * B2;
  const W = 0.5 * (root(B0) + root(B1));
  const b0 = 0.5 * (W + root(W * W + B2));
  const b1 = 0.5 * (root(B0) - root(B1));
  const b2 = -B2 / (4.0 * b0);
  const boost: BiquadCoefficients = { b0, b1, b2, a1: d.a1, a2: d.a2 };
  if (!cut) {
    return boost;
  }
  return {
    b0: 1.0 / boost.b0,
    b1: boost.a1 / boost.b0,
    b2: boost.a2 / boost.b0,
    a1: boost.b1 / boost.b0,
    a2: boost.b2 / boost.b0,
  };
}

function matchedLowPass(w0: number, quality: number): BiquadCoefficients {
  const d = matchedPoles(w0, 1.0 / (2.0 * quality));
  const p = phisAt(w0);
  const B0 = d.A0;
  const R1 = (d.A0 * p.phi0 + d.A1 * p.phi1 + d.A2 * p.phi2) * quality * quality;
  const B1 = (R1 - B0 * p.phi0) / p.phi1;
  const b0 = 0.5 * (root(B0) + root(B1));
  const b1 = root(B0) - b0;
  return { b0, b1, b2: 0.0, a1: d.a1, a2: d.a2 };
}

function matchedHighPass(w0: number, quality: number): BiquadCoefficients {
  const d = matchedPoles(w0, 1.0 / (2.0 * quality));
  const p = phisAt(w0);
  const b0 = (quality * root(d.A0 * p.phi0 + d.A1 * p.phi1 + d.A2 * p.phi2)) / (4.0 * p.phi1);
  return { b0, b1: -2.0 * b0, b2: b0, a1: d.a1, a2: d.a2 };
}

function matchedBandPass(w0: number, quality: number): BiquadCoefficients {
  const d = matchedPoles(w0, 1.0 / (2.0 * quality));
  const p = phisAt(w0);
  const R1 = d.A0 * p.phi0 + d.A1 * p.phi1 + d.A2 * p.phi2;
  const R2 = -d.A0 + d.A1 + 4.0 * (p.phi0 - p.phi1) * d.A2;
  const B2 = (R1 - R2 * p.phi1) / (4.0 * p.phi1 * p.phi1);
  const B1 = R2 + 4.0 * (p.phi1 - p.phi0) * B2;
  const b1 = -0.5 * root(B1);
  const b0 = 0.5 * (root(B2 + b1 * b1) - b1);
  const b2 = -b0 - b1;
  return { b0, b1, b2, a1: d.a1, a2: d.a2 };
}

function matchedShelf(high: boolean, fc: number, gainDb: number): BiquadCoefficients {
  const G = Math.pow(10.0, gainDb / 20.0);
  let g = high ? G : 1.0 / G;
  if (Math.abs(1.0 - g) < 1e-6) {
    g = 1.00001;
  }
  const fc4 = fc * fc * fc * fc;
  const hNyquist = (fc4 + g) / (fc4 + 1.0 / g);
  const f1 = fc / Math.sqrt(0.16 + 1.543 * fc * fc);
  const f2 = fc / Math.sqrt(0.947 + 3.806 * fc * fc);
  const hAt = (f: number) => {
    const f4 = f * f * f * f;
    return (fc4 + f4 * g) / (fc4 + f4 / g);
  };
  const h1 = hAt(f1);
  const h2 = hAt(f2);
  const s1 = Math.sin((PI * f1) / 2.0);
  const s2 = Math.sin((PI * f2) / 2.0);
  const p1 = s1 * s1;
  const p2 = s2 * s2;
  const d1 = (h1 - 1.0) * (1.0 - p1);
  const d2 = (h2 - 1.0) * (1.0 - p2);
  const c11 = -p1 * d1;
  const c12 = p1 * p1 * (hNyquist - h1);
  const c21 = -p2 * d2;
  const c22 = p2 * p2 * (hNyquist - h2);
  const alpha = (c22 * d1 - c12 * d2) / (c11 * c22 - c12 * c21);
  const AA1 = (d1 - c11 * alpha) / c12;
  const BB1 = hNyquist * AA1;
  const AA2 = (alpha - AA1) / 4.0;
  const BB2 = (alpha - BB1) / 4.0;
  const v = 0.5 * (1.0 + root(AA1));
  const w = 0.5 * (1.0 + root(BB1));
  const a0 = 0.5 * (v + root(v * v + AA2));
  const beta0 = 0.5 * (w + root(w * w + BB2));
  const k: BiquadCoefficients = {
    a1: (1.0 - v) / a0,
    a2: -AA2 / (4.0 * a0 * a0),
    b0: beta0 / a0,
    b1: (1.0 - w) / a0,
    b2: -BB2 / (4.0 * beta0 * a0),
  };
  if (!high) {
    k.b0 *= G;
    k.b1 *= G;
    k.b2 *= G;
  }
  return k;
}

/**
 * Martin Vicanek analog-matched biquad (`feq_biquad_coefficients_matched`).
 */
export function biquadCoefficientsMatched(
  type: FilterType,
  frequency: number,
  gainDb: number,
  quality: number,
  sampleRate: number,
): BiquadCoefficients {
  const cookbook = biquadCoefficientsCookbook(type, frequency, gainDb, quality, sampleRate);
  if (!(sampleRate > 0) || !(frequency > 0) || !(quality > 0)) {
    return cookbook;
  }
  const safe = Math.min(frequency, sampleRate * 0.499);
  const w0 = (2.0 * PI * safe) / sampleRate;
  let matched = cookbook;

  switch (type) {
    case 'PK':
      matched = matchedBell(w0, gainDb, quality);
      break;
    case 'LPQ':
      matched = matchedLowPass(w0, quality);
      break;
    case 'HPQ':
      matched = matchedHighPass(w0, quality);
      break;
    case 'BP':
      matched = matchedBandPass(w0, quality);
      break;
    case 'LSC':
    case 'HSC':
      if (Math.abs(quality - BUTTERWORTH_Q) > BUTTERWORTH_TOLERANCE) {
        return cookbook;
      }
      matched = matchedShelf(
        type === 'HSC',
        Math.min(frequency, 2.0 * sampleRate) / (sampleRate / 2.0),
        gainDb,
      );
      if (gainDb === 0) {
        matched = unityOn(matched.a1, matched.a2);
      }
      break;
    case 'NO':
      return cookbook;
  }

  const stable = Math.abs(matched.a2) < 1.0 && Math.abs(matched.a1) < 1.0 + matched.a2;
  return isFiniteCoeffs(matched) && stable ? matched : cookbook;
}

/**
 * Full FluidEQ designed biquad (`feq_biquad_coefficients_designed`).
 */
export function biquadCoefficientsDesigned(
  type: FilterType,
  frequency: number,
  gainDb: number,
  quality: number,
  sampleRate: number,
  model: EqCharacterModel = 'clean',
  amount = 1.0,
  trebleMode: TrebleDesignMode = 'precise',
): BiquadCoefficients {
  const shapedQ = modelledQuality(type, gainDb, quality, model, amount);
  return trebleMode === 'precise'
    ? biquadCoefficientsMatched(type, frequency, gainDb, shapedQ, sampleRate)
    : biquadCoefficientsCookbook(type, frequency, gainDb, shapedQ, sampleRate);
}

/**
 * Evaluates a single biquad filter's magnitude response in dB at `freqHz`.
 */
export function evaluateBiquadMagnitudeDb(
  coeffs: BiquadCoefficients,
  freqHz: number,
  sampleRate: number,
): number {
  const safeFreq = Math.min(Math.max(freqHz, 1.0), sampleRate * 0.499);
  const w = (2.0 * PI * safeFreq) / sampleRate;
  const phi1 = Math.pow(Math.sin(w / 2.0), 2);
  const phi0 = 1.0 - phi1;
  const phi2 = 4.0 * phi0 * phi1;

  const B0 = Math.pow(coeffs.b0 + coeffs.b1 + coeffs.b2, 2);
  const B1 = Math.pow(coeffs.b0 - coeffs.b1 + coeffs.b2, 2);
  const B2 = -4.0 * coeffs.b0 * coeffs.b2;

  const A0 = Math.pow(1.0 + coeffs.a1 + coeffs.a2, 2);
  const A1 = Math.pow(1.0 - coeffs.a1 + coeffs.a2, 2);
  const A2 = -4.0 * coeffs.a2;

  const num = B0 * phi0 + B1 * phi1 + B2 * phi2;
  const den = A0 * phi0 + A1 * phi1 + A2 * phi2;
  if (den <= 0 || num <= 0) {
    return -60;
  }
  const db = 10.0 * Math.log10(num / den);
  return Math.max(-60, Math.min(60, db));
}

/**
 * Generates log-spaced frequency points from 10 Hz to 20,000 Hz.
 */
export function createLogFrequencyGrid(pointCount = 160): number[] {
  const minLog = Math.log10(10);
  const maxLog = Math.log10(20000);
  const result: number[] = [];
  for (let i = 0; i < pointCount; i += 1) {
    const t = i / (pointCount - 1);
    result.push(Math.pow(10, minLog + t * (maxLog - minLog)));
  }
  return result;
}

/**
 * Evaluates a cascade of EqBands across a frequency grid.
 */
export function evaluateBandCascadeDb(
  bands: readonly EqBand[],
  freqGrid: readonly number[],
  sampleRate = 48000,
  model: EqCharacterModel = 'clean',
  amount = 1.0,
  trebleMode: TrebleDesignMode = 'precise',
  strength = 1.0,
): number[] {
  const active = bands.filter((b) => b.enabled);
  if (active.length === 0 || strength === 0) {
    return freqGrid.map(() => 0);
  }
  const coeffList = active.map((b) =>
    biquadCoefficientsDesigned(
      b.type,
      b.frequency,
      b.gainDb * strength,
      b.q,
      sampleRate,
      model,
      amount,
      trebleMode,
    ),
  );
  return freqGrid.map((f) => {
    let sum = 0;
    for (const c of coeffList) {
      sum += evaluateBiquadMagnitudeDb(c, f, sampleRate);
    }
    return Math.max(-60, Math.min(60, sum));
  });
}

/**
 * Builds the Tone layer bands (Bass 100 Hz LSC, Mid 1 kHz PK, Treble 10 kHz HSC)
 * and the Low/High cut Butterworth filters (20 Hz / 20 kHz, 12 or 24 dB/oct).
 */
export function buildToneBands(tone: ToneSettings): {
  toneBands: EqBand[];
  cutBands: EqBand[];
} {
  if (!tone.enabled) {
    return { toneBands: [], cutBands: [] };
  }
  const toneBands: EqBand[] = [];
  if (Math.abs(tone.bassDb) > 0.01) {
    toneBands.push({
      id: 'tone-bass',
      enabled: true,
      type: 'LSC',
      frequency: 100,
      gainDb: tone.bassDb,
      q: 0.7,
    });
  }
  if (Math.abs(tone.midDb) > 0.01) {
    toneBands.push({
      id: 'tone-mid',
      enabled: true,
      type: 'PK',
      frequency: 1000,
      gainDb: tone.midDb,
      q: 0.7,
    });
  }
  if (Math.abs(tone.trebleDb) > 0.01) {
    toneBands.push({
      id: 'tone-treble',
      enabled: true,
      type: 'HSC',
      frequency: 10000,
      gainDb: tone.trebleDb,
      q: 0.7,
    });
  }

  const cutBands: EqBand[] = [];
  if (tone.lowCutSlope >= 12) {
    cutBands.push({
      id: 'cut-low-1',
      enabled: true,
      type: 'HPQ',
      frequency: 20,
      gainDb: 0,
      q: BUTTERWORTH_Q,
    });
  }
  if (tone.lowCutSlope === 24) {
    cutBands.push({
      id: 'cut-low-2',
      enabled: true,
      type: 'HPQ',
      frequency: 20,
      gainDb: 0,
      q: BUTTERWORTH_Q,
    });
  }
  if (tone.highCutSlope >= 12) {
    cutBands.push({
      id: 'cut-high-1',
      enabled: true,
      type: 'LPQ',
      frequency: 20000,
      gainDb: 0,
      q: BUTTERWORTH_Q,
    });
  }
  if (tone.highCutSlope === 24) {
    cutBands.push({
      id: 'cut-high-2',
      enabled: true,
      type: 'LPQ',
      frequency: 20000,
      gainDb: 0,
      q: BUTTERWORTH_Q,
    });
  }

  return { toneBands, cutBands };
}

/**
 * Computes the worst-case auto-normalize Preamp in dB from the combined response curve
 * so that the peak never exceeds -0.3 dBFS.
 */
export function computeAutoPreampDb(combinedCurveDb: readonly number[]): number {
  let maxBoost = 0;
  for (const db of combinedCurveDb) {
    if (db > maxBoost) {
      maxBoost = db;
    }
  }
  if (maxBoost <= 0.05) {
    return 0.0;
  }
  return Math.round((-maxBoost - 0.3) * 10) / 10;
}

/**
 * Converts an OPRA JSON band entry into a FluidEQ EqBand.
 */
export interface OpraRawBand {
  type: string;
  frequency: number;
  gain_db: number;
  q?: number;
}

export function opraBandsToEqBands(rawBands: readonly OpraRawBand[]): EqBand[] {
  return rawBands.map((b, idx) => {
    let filterType: FilterType = 'PK';
    if (b.type === 'low_shelf') {
      filterType = 'LSC';
    } else if (b.type === 'high_shelf') {
      filterType = 'HSC';
    }
    return {
      id: `opra-${idx + 1}`,
      enabled: true,
      type: filterType,
      frequency: Math.round(b.frequency),
      gainDb: Math.round(b.gain_db * 10) / 10,
      q: Math.round((b.q ?? 0.707) * 100) / 100,
    };
  });
}

/**
 * Parses Equalizer APO / Squiglink ParametricEQ text into Preamp and EqBands.
 */
export function parseApoText(text: string): { preampDb: number; bands: EqBand[] } {
  const lines = text.split(/\r?\n/);
  let preampDb = 0;
  const bands: EqBand[] = [];

  for (const rawLine of lines) {
    const line = rawLine.trim();
    if (!line || line.startsWith('#')) {
      continue;
    }
    const preampMatch = /^Preamp:\s*([+-]?\d+(?:\.\d+)?)\s*dB/i.exec(line);
    if (preampMatch) {
      preampDb = Number.parseFloat(preampMatch[1]);
      continue;
    }

    const filterMatch =
      /^Filter\s*\d*:\s*(ON|OFF)\s+(PK|PEQ|LSC|LS|HSC|HS|LPQ|LP|HPQ|HP|NO|BP)\s+Fc\s+([+-]?\d+(?:\.\d+)?)\s*Hz(?:\s+Gain\s+([+-]?\d+(?:\.\d+)?)\s*dB)?(?:\s+Q\s+([+-]?\d+(?:\.\d+)?))?/i.exec(
        line,
      );
    if (filterMatch) {
      const enabled = filterMatch[1].toUpperCase() === 'ON';
      const rawType = filterMatch[2].toUpperCase();
      let type: FilterType = 'PK';
      if (rawType === 'LSC' || rawType === 'LS') type = 'LSC';
      else if (rawType === 'HSC' || rawType === 'HS') type = 'HSC';
      else if (rawType === 'LPQ' || rawType === 'LP') type = 'LPQ';
      else if (rawType === 'HPQ' || rawType === 'HP') type = 'HPQ';
      else if (rawType === 'NO') type = 'NO';
      else if (rawType === 'BP') type = 'BP';

      const frequency = Number.parseFloat(filterMatch[3]);
      const gainDb = filterMatch[4] !== undefined ? Number.parseFloat(filterMatch[4]) : 0;
      const q = filterMatch[5] !== undefined ? Number.parseFloat(filterMatch[5]) : 0.707;

      bands.push({
        id: `apo-import-${bands.length + 1}`,
        enabled,
        type,
        frequency,
        gainDb,
        q,
      });
    }
  }

  return { preampDb, bands };
}

/**
 * Formats an EqBand array into Equalizer APO Filter lines.
 */
export function formatBandsAsApoFilterLines(
  bands: readonly EqBand[],
  strength = 1.0,
): string[] {
  return bands.map((band, index) => {
    const state = band.enabled ? 'ON' : 'OFF';
    const scaledGain = Math.round(band.gainDb * strength * 10) / 10;
    const freq = Math.round(band.frequency * 10) / 10;
    const q = Math.round(band.q * 100) / 100;
    if (band.type === 'LPQ' || band.type === 'HPQ' || band.type === 'NO' || band.type === 'BP') {
      return `Filter ${index + 1}: ${state} ${band.type} Fc ${freq} Hz Q ${q}`;
    }
    return `Filter ${index + 1}: ${state} ${band.type} Fc ${freq} Hz Gain ${scaledGain} dB Q ${q}`;
  });
}

export type WindowResidencyMode = 'active' | 'warm-minimized' | 'cold-standby';

export interface TieredCanvasDpr {
  spectrumDpr: number;
  curveDpr: number;
  memorySavedPct: number;
}

/**
 * Computes tiered resolution multipliers for the dual-canvas spectrum graph:
 * - Dynamic spectrum layer is capped at 1.0x–1.25x (soft color bars/waves need no retina oversampling).
 * - Static EQ curve & typography layer renders up to 2.0x for razor-sharp text and handles.
 */
export function computeTieredCanvasDpr(devicePixelRatio: number): TieredCanvasDpr {
  const safeDpr = Number.isFinite(devicePixelRatio) && devicePixelRatio > 0 ? devicePixelRatio : 1;
  const curveDpr = Math.min(2.0, Math.max(1.0, safeDpr));
  const spectrumDpr = safeDpr > 1.5 ? 1.25 : 1.0;
  const fullPixels = safeDpr * safeDpr * 2;
  const tieredPixels = spectrumDpr * spectrumDpr + curveDpr * curveDpr;
  const memorySavedPct =
    fullPixels > tieredPixels
      ? Math.round(((fullPixels - tieredPixels) / fullPixels) * 100)
      : 0;
  return { spectrumDpr, curveDpr, memorySavedPct };
}

export interface ResidencyFootprintProfile {
  mode: WindowResidencyMode;
  cppDspCoreMb: number;
  rustStateMb: number;
  uiCanvasMb: number;
  totalMb: number;
  metersFftEnabled: boolean;
  spectrumTargetFps: number;
}

/**
 * Computes the memory and refresh profile for the Rust State Store + C++ dsp-core + Cold-Residency UI.
 */
export function computeResidencyProfile(
  mode: WindowResidencyMode,
  isPlaying: boolean,
): ResidencyFootprintProfile {
  const cppDspCoreMb = 9.4;
  const rustStateMb = 3.2;
  const uiCanvasMb =
    mode === 'cold-standby' ? 0.0 : mode === 'warm-minimized' ? 14.2 : 16.4;
  const totalMb = Math.round((cppDspCoreMb + rustStateMb + uiCanvasMb) * 10) / 10;
  const metersFftEnabled = mode === 'active' && isPlaying;
  const spectrumTargetFps = metersFftEnabled ? 30 : 0;

  return {
    mode,
    cppDspCoreMb,
    rustStateMb,
    uiCanvasMb,
    totalMb,
    metersFftEnabled,
    spectrumTargetFps,
  };
}

