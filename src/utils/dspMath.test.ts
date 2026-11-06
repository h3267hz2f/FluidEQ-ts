import test from 'node:test';
import assert from 'node:assert/strict';
import {
  biquadCoefficientsCookbook,
  biquadCoefficientsMatched,
  biquadCoefficientsDesigned,
  evaluateBiquadMagnitudeDb,
  evaluateBandCascadeDb,
  computeAutoPreampDb,
  parseApoText,
  formatBandsAsApoFilterLines,
  modelledQuality,
  createLogFrequencyGrid,
  computeTieredCanvasDpr,
  computeResidencyProfile,
} from './dspMath';

test('cookbook and Vicanek analog-matched bell agree at unity (0 dB gain)', () => {
  const cookbook = biquadCoefficientsCookbook('PK', 1000, 0, 1.41, 48000);
  const matched = biquadCoefficientsMatched('PK', 1000, 0, 1.41, 48000);
  assert.ok(Math.abs(evaluateBiquadMagnitudeDb(cookbook, 1000, 48000)) < 1e-6);
  assert.ok(Math.abs(evaluateBiquadMagnitudeDb(matched, 1000, 48000)) < 1e-6);
});

test('Vicanek analog-matched bell reaches exact target gain at high treble (16 kHz at 48 kHz)', () => {
  const matched = biquadCoefficientsMatched('PK', 16000, 6.0, 2.0, 48000);
  const magAtCenter = evaluateBiquadMagnitudeDb(matched, 16000, 48000);
  assert.ok(Math.abs(magAtCenter - 6.0) < 0.05, `Expected ~6.0 dB, got ${magAtCenter}`);
});

test('matched cut is exact reciprocal of matched boost', () => {
  const boost = biquadCoefficientsMatched('PK', 4000, 6.0, 2.0, 48000);
  const cut = biquadCoefficientsMatched('PK', 4000, -6.0, 2.0, 48000);
  for (const freq of [100, 1000, 4000, 10000, 18000]) {
    const sumDb =
      evaluateBiquadMagnitudeDb(boost, freq, 48000) +
      evaluateBiquadMagnitudeDb(cut, freq, 48000);
    assert.ok(Math.abs(sumDb) < 1e-5, `Expected 0 dB sum at ${freq} Hz, got ${sumDb}`);
  }
});

test('character model Q shaping behaves according to native biquad.cpp specification', () => {
  const cleanQ = modelledQuality('PK', 6.0, 1.41, 'clean', 1.0);
  const propQ = modelledQuality('PK', 6.0, 1.41, 'proportional', 1.0);
  const wideQ = modelledQuality('PK', 6.0, 1.41, 'wide', 1.0);
  const asymBoostQ = modelledQuality('PK', 6.0, 1.41, 'asymmetric', 1.0);
  const asymCutQ = modelledQuality('PK', -6.0, 1.41, 'asymmetric', 1.0);

  assert.equal(cleanQ, 1.41);
  assert.ok(propQ > cleanQ, 'Proportional narrows Q when driven');
  assert.ok(wideQ < cleanQ, 'Wide broadens Q when driven');
  assert.ok(asymBoostQ < cleanQ, 'Asymmetric widens boosts');
  assert.ok(asymCutQ > cleanQ, 'Asymmetric narrows cuts');
});

test('computeAutoPreampDb reserves worst-case peak plus 0.3 dB margin', () => {
  const grid = createLogFrequencyGrid(64);
  const curve = evaluateBandCascadeDb(
    [{ id: 'b1', enabled: true, type: 'PK', frequency: 1000, gainDb: 6.0, q: 1.0 }],
    grid,
    48000,
  );
  const preamp = computeAutoPreampDb(curve);
  assert.ok(preamp <= -6.2 && preamp >= -6.4, `Expected around -6.3 dB, got ${preamp}`);
});

test('parseApoText and formatBandsAsApoFilterLines roundtrip Equalizer APO filter lines', () => {
  const sample = `Preamp: -4.5 dB
Filter 1: ON LSC Fc 105 Hz Gain 3.5 dB Q 0.7
Filter 2: ON PK Fc 2500 Hz Gain -2.4 dB Q 1.85`;
  const parsed = parseApoText(sample);
  assert.equal(parsed.preampDb, -4.5);
  assert.equal(parsed.bands.length, 2);
  assert.equal(parsed.bands[0].type, 'LSC');
  assert.equal(parsed.bands[0].frequency, 105);
  assert.equal(parsed.bands[1].gainDb, -2.4);

  const formatted = formatBandsAsApoFilterLines(parsed.bands);
  assert.equal(formatted[0], 'Filter 1: ON LSC Fc 105 Hz Gain 3.5 dB Q 0.7');
  assert.equal(formatted[1], 'Filter 2: ON PK Fc 2500 Hz Gain -2.4 dB Q 1.85');
});

test('biquadCoefficientsDesigned switches between precise and classic treble modes', () => {
  const precise = biquadCoefficientsDesigned('PK', 14000, 6.0, 2.5, 48000, 'clean', 1.0, 'precise');
  const classic = biquadCoefficientsDesigned('PK', 14000, 6.0, 2.5, 48000, 'clean', 1.0, 'classic');
  const preciseMag = evaluateBiquadMagnitudeDb(precise, 14000, 48000);
  const classicMag = evaluateBiquadMagnitudeDb(classic, 14000, 48000);
  assert.ok(Math.abs(preciseMag - classicMag) <= 0.5);
});

test('computeTieredCanvasDpr caps dynamic spectrum DPR while preserving crisp curve DPR', () => {
  const hiDpi = computeTieredCanvasDpr(2.5);
  assert.equal(hiDpi.spectrumDpr, 1.25);
  assert.equal(hiDpi.curveDpr, 2.0);
  assert.ok(hiDpi.memorySavedPct >= 50);

  const stdDpi = computeTieredCanvasDpr(1.0);
  assert.equal(stdDpi.spectrumDpr, 1.0);
  assert.equal(stdDpi.curveDpr, 1.0);
});

test('computeResidencyProfile drops UI memory and spectrum FPS to zero in cold-standby and silence', () => {
  const activePlaying = computeResidencyProfile('active', true);
  assert.equal(activePlaying.metersFftEnabled, true);
  assert.ok(activePlaying.spectrumTargetFps > 0);

  const activeSilent = computeResidencyProfile('active', false);
  assert.equal(activeSilent.metersFftEnabled, false);
  assert.equal(activeSilent.spectrumTargetFps, 0);

  const coldStandby = computeResidencyProfile('cold-standby', true);
  assert.equal(coldStandby.uiCanvasMb, 0);
  assert.equal(coldStandby.metersFftEnabled, false);
  assert.equal(coldStandby.spectrumTargetFps, 0);
  assert.ok(coldStandby.totalMb < 15.0);
});

