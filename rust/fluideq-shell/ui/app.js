/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

// The view is intentionally dependency-free: Tauri injects `__TAURI__`, and
// everything else is drawn from the snapshot the Rust state centre returns.
// No polling timers: system volume is written by the gesture that changes it,
// and the canvas is redrawn on the next animation frame.

const TAURI = window.__TAURI__;
const invoke = TAURI?.core?.invoke;

const F_MIN = 20;
const F_MAX = 20_000;
const DB_MIN = -18;
const DB_MAX = 18;
const SAMPLE_RATE = 48_000;
let sampleRate = SAMPLE_RATE;
const ISO_FREQUENCIES = [
  25, 40, 63, 100, 160, 250, 400, 630, 1000, 1600, 2500, 4000, 6300, 10000,
  16000,
];

const DEMO_BANDS = [
  [25, 3.5, 'LSC', 0.71],
  [40, 2.8, 'PK', 1.41],
  [63, 1.6, 'PK', 1.41],
  [100, 0.4, 'PK', 1.41],
  [160, -0.8, 'PK', 1.41],
  [250, -1.2, 'PK', 1.41],
  [400, -0.5, 'PK', 1.41],
  [630, 0.2, 'PK', 1.41],
  [1000, 0.8, 'PK', 1.41],
  [1600, 1.5, 'PK', 1.41],
  [2500, 2.2, 'PK', 1.41],
  [4000, 1.8, 'PK', 1.41],
  [6300, 2.6, 'PK', 1.41],
  [10000, 3.2, 'PK', 1.41],
  [16000, 2.4, 'HSC', 0.71],
].map(([frequency, gain_db, filter_type, quality], index) => ({
  id: index + 1,
  enabled: true,
  filter_type,
  frequency,
  gain_db,
  quality,
  dynamic: false,
  threshold_db: -18,
  ear: 0,
}));

const demoSnapshot = () => ({
  revision: 1,
  master_power: true,
  auto_headroom: true,
  preamp_db: 0,
  auto_preamp_db: 0,
  active_preset_name: 'Harman Target 2018',
  eq_model: 'FEQ_EQ_MODEL_CLEAN',
  eq_model_amount: 1,
  matched_treble: true,
  bands: DEMO_BANDS.map((band) => ({ ...band })),
  residency: {},
  models: {},
});

const els = {
  canvas: document.getElementById('response'),
  preset: document.getElementById('preset'),
  power: document.getElementById('power'),
  volume: document.getElementById('volume'),
  volumeValue: document.getElementById('volume-value'),
  volumeIcon: document.getElementById('volume-icon'),
  bands: document.getElementById('bands'),
  bandNumber: document.getElementById('band-number'),
  bandFreq: document.getElementById('band-freq'),
  bandType: document.getElementById('band-type'),
  bandGainValue: document.getElementById('band-gain-value'),
  bandGain: document.getElementById('band-gain'),
  bandFrequency: document.getElementById('band-frequency'),
  bandQuality: document.getElementById('band-quality'),
  resetBand: document.getElementById('reset-band'),
  preamp: document.getElementById('preamp'),
  preampValue: document.getElementById('preamp-value'),
  autoHeadroom: document.getElementById('auto-headroom'),
  bandCount: document.getElementById('band-count'),
  chainState: document.getElementById('chain-state'),
  connection: document.getElementById('connection'),
  message: document.getElementById('message'),
  engineActivation: document.getElementById('engine-activation'),
  engineText: document.getElementById('engine-text'),
  engineInstall: document.getElementById('engine-install'),
  engineState: document.getElementById('engine-state'),
  deviceName: document.querySelector('.device-name'),
};

let snapshot = demoSnapshot();
let selectedId = 4;
let drag = null;
let drawFrame = 0;
let pendingVolume = null;
let volumeFrame = 0;
let volumeAvailable = false;
let lastAudibleVolume = 0.8;
let bandCommitInFlight = false;
let bandCommitQueued = false;
let engineStatus = null;
let hostStatus = null;
let engineInstallInFlight = false;

const clamp = (value, low, high) => Math.min(high, Math.max(low, value));

function bandById(id) {
  return snapshot.bands?.find((band) => band.id === id) ?? null;
}

function selectedBand() {
  return bandById(selectedId) ?? snapshot.bands?.[0] ?? null;
}

function selectedIndex() {
  return snapshot.bands?.findIndex((band) => band.id === selectedId) ?? -1;
}

function formatHz(frequency) {
  if (!Number.isFinite(frequency)) return '—';
  if (frequency < 1000) return `${Math.round(frequency)} Hz`;
  const khz = frequency / 1000;
  return `${khz.toFixed(khz < 10 ? 1 : 0)} kHz`;
}

function formatDb(value) {
  if (!Number.isFinite(value)) return '0.0';
  const rounded = Math.abs(value) < 0.05 ? 0 : value;
  return `${rounded > 0 ? '+' : ''}${rounded.toFixed(1)}`;
}

function setConnection(text, mode) {
  els.connection.textContent = text;
  els.connection.classList.toggle('live', mode === 'live');
  els.connection.classList.toggle('preview', mode === 'preview');
}

function showMessage(text, kind) {
  els.message.textContent = text;
  els.message.classList.toggle('ok', kind === 'ok');
}

function clearMessage() {
  els.message.textContent = '';
  els.message.classList.remove('ok');
}

// A later successful read has to take back the error the earlier one left, or
// the status bar keeps describing a failure that is over.
function clearMessageStartingWith(prefix) {
  if (els.message.textContent.startsWith(prefix)) clearMessage();
}

function setRangeFill(input, percentage) {
  input.style.setProperty('--fill', `${clamp(percentage, 0, 100)}%`);
}

function setRangeValue(input, value) {
  if (document.activeElement !== input) input.value = String(value);
}

// ---------------------------------------------------------------------------
// Biquad magnitude — the same filters the native core applies, so the curve is
// the chain's response rather than a decorative spline through the handles.
// ---------------------------------------------------------------------------

function coefficients(band) {
  const frequency = clamp(Number(band.frequency) || 1000, 10, sampleRate / 2 - 1);
  const q = clamp(Number(band.quality) || 1, 0.05, 40);
  const gain = clamp(Number(band.gain_db) || 0, -60, 60);
  const type = band.filter_type || 'PK';
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
    return { b0: 1, b1: -2 * cosw, b2: 1, a0: 1 + alpha, a1: -2 * cosw, a2: 1 - alpha };
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
    return { b0: alpha, b1: 0, b2: -alpha, a0: 1 + alpha, a1: -2 * cosw, a2: 1 - alpha };
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

function magnitudeDb(band, frequency) {
  const c = coefficients(band);
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

function responseDb(frequency) {
  let db = Number(snapshot.preamp_db) || 0;
  for (const band of snapshot.bands ?? []) {
    if (band.enabled) db += magnitudeDb(band, frequency);
  }
  return db;
}

// ---------------------------------------------------------------------------
// Canvas
// ---------------------------------------------------------------------------

const PAD = { left: 44, right: 20, top: 10, bottom: 22 };

function xOf(frequency, width) {
  const ratio = Math.log10(clamp(frequency, F_MIN, F_MAX) / F_MIN) / Math.log10(F_MAX / F_MIN);
  return PAD.left + ratio * (width - PAD.left - PAD.right);
}

function frequencyOf(x, width) {
  const ratio = clamp((x - PAD.left) / Math.max(width - PAD.left - PAD.right, 1), 0, 1);
  return F_MIN * (F_MAX / F_MIN) ** ratio;
}

function yOf(db, height) {
  const ratio = (DB_MAX - clamp(db, DB_MIN, DB_MAX)) / (DB_MAX - DB_MIN);
  return PAD.top + ratio * (height - PAD.top - PAD.bottom);
}

function dbOf(y, height) {
  const ratio = clamp((y - PAD.top) / Math.max(height - PAD.top - PAD.bottom, 1), 0, 1);
  return DB_MAX - ratio * (DB_MAX - DB_MIN);
}

function prepareCanvas() {
  const rect = els.canvas.getBoundingClientRect();
  if (rect.width < 4 || rect.height < 4) return null;
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  const width = Math.round(rect.width * dpr);
  const height = Math.round(rect.height * dpr);
  if (els.canvas.width !== width || els.canvas.height !== height) {
    els.canvas.width = width;
    els.canvas.height = height;
  }
  const ctx = els.canvas.getContext('2d');
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  return { ctx, width: rect.width, height: rect.height };
}

function drawGrid(ctx, width, height) {
  const frequencies = [20, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000];
  ctx.save();
  ctx.font = '10px -apple-system, "Segoe UI", Ubuntu, Cantarell, sans-serif';
  ctx.textBaseline = 'middle';

  for (const frequency of frequencies) {
    const x = xOf(frequency, width);
    ctx.strokeStyle = frequency === 1000 ? 'rgba(255,255,255,0.10)' : 'rgba(255,255,255,0.045)';
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
    ctx.strokeStyle = db === 0 ? 'rgba(255,255,255,0.12)' : 'rgba(255,255,255,0.045)';
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

function drawGraph() {
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
      yOf(responseDb(frequency), height),
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

  const band = selectedBand();
  if (band) {
    const x = xOf(band.frequency, width);
    const y = yOf(band.gain_db, height);
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

  for (const current of snapshot.bands ?? []) {
    if (!current.enabled) continue;
    const x = xOf(current.frequency, width);
    const y = yOf(current.gain_db, height);
    const selected = current.id === selectedId;
    ctx.beginPath();
    ctx.arc(x, y, selected ? 4.6 : 3.1, 0, Math.PI * 2);
    ctx.fillStyle = selected ? '#0b1517' : '#3be8c4';
    ctx.fill();
    ctx.lineWidth = selected ? 2.2 : 1;
    ctx.strokeStyle = selected ? '#ffb45e' : 'rgba(5,7,11,0.9)';
    ctx.stroke();
    if (selected) {
      ctx.beginPath();
      ctx.arc(x, y, 8.5, 0, Math.PI * 2);
      ctx.strokeStyle = 'rgba(255,180,94,0.26)';
      ctx.lineWidth = 1;
      ctx.stroke();
    }
  }
}

function scheduleDraw() {
  if (drawFrame) return;
  drawFrame = requestAnimationFrame(() => {
    drawFrame = 0;
    drawGraph();
  });
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

function renderHeader() {
  els.preset.textContent = snapshot.active_preset_name || 'Custom curve';
  els.power.classList.toggle('on', Boolean(snapshot.master_power));
  els.power.setAttribute('aria-pressed', String(Boolean(snapshot.master_power)));
  els.chainState.textContent = snapshot.master_power
    ? 'Chain enabled'
    : 'Chain bypassed';
  els.autoHeadroom.textContent = snapshot.auto_headroom ? 'On' : 'Off';
  els.autoHeadroom.setAttribute('aria-pressed', String(Boolean(snapshot.auto_headroom)));
  els.bandCount.textContent = String(snapshot.bands?.length ?? 0);
}

function renderBands() {
  const fragment = document.createDocumentFragment();
  for (const band of snapshot.bands ?? []) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = `band-chip${band.id === selectedId ? ' selected' : ''}`;
    button.setAttribute('aria-pressed', String(band.id === selectedId));
    button.setAttribute('aria-label', `${formatHz(band.frequency)}, ${formatDb(band.gain_db)} decibels`);

    const frequency = document.createElement('span');
    frequency.className = 'freq';
    frequency.textContent = formatHz(band.frequency);

    const gain = document.createElement('span');
    gain.className = 'gain';
    const label = document.createElement('span');
    label.textContent = `${formatDb(band.gain_db)} dB`;
    const meter = document.createElement('span');
    meter.className = 'meter';
    const amount = document.createElement('i');
    amount.className = band.gain_db >= 0 ? '' : 'negative';
    const width = clamp(Math.abs(band.gain_db) / 24, 0.02, 1) * 50;
    amount.style.setProperty('--amount', `${width}%`);
    meter.appendChild(amount);
    gain.append(label, meter);

    button.append(frequency, gain);
    button.addEventListener('click', () => {
      selectedId = band.id;
      renderBands();
      renderInspector();
      scheduleDraw();
    });
    fragment.appendChild(button);
  }
  els.bands.replaceChildren(fragment);
}

function renderInspector() {
  const band = selectedBand();
  if (!band) return;
  const index = selectedIndex();
  els.bandNumber.textContent = String(index + 1).padStart(2, '0');
  els.bandFreq.textContent = formatHz(band.frequency);
  els.bandType.textContent = band.filter_type || 'PK';
  els.bandGainValue.textContent = formatDb(band.gain_db);

  setRangeValue(els.bandGain, band.gain_db);
  setRangeFill(els.bandGain, ((band.gain_db + 24) / 48) * 100);

  const frequencyRatio = Math.log10(band.frequency / F_MIN) / Math.log10(F_MAX / F_MIN);
  setRangeValue(els.bandFrequency, Math.round(frequencyRatio * 1000));
  setRangeFill(els.bandFrequency, frequencyRatio * 100);

  setRangeValue(els.bandQuality, band.quality);
  setRangeFill(els.bandQuality, ((band.quality - 0.1) / 17.9) * 100);

  setRangeValue(els.preamp, snapshot.preamp_db);
  setRangeFill(els.preamp, ((snapshot.preamp_db + 24) / 48) * 100);
  renderPreampReadout();
}

// With auto headroom on, the guard starts at the curve's own level and the
// slider is a trim on top of it. Showing only the trim let a -24 dB trim look
// like the whole gain while the chain actually sat at -62 dB.
function renderPreampReadout() {
  const manual = Number(snapshot.preamp_db) || 0;
  const auto = snapshot.auto_headroom ? Number(snapshot.auto_preamp_db) || 0 : 0;
  const total = manual + auto;
  els.preampValue.textContent = `${formatDb(total)} dB`;
  els.preampValue.title = snapshot.auto_headroom
    ? `Auto headroom ${formatDb(auto)} dB + trim ${formatDb(manual)} dB`
    : '';
}

function renderAll() {
  renderHeader();
  renderBands();
  renderInspector();
  drawGraph();
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

async function commitBand() {
  const band = selectedBand();
  if (!band) return;
  if (!invoke) {
    renderAll();
    return;
  }
  if (bandCommitInFlight) {
    bandCommitQueued = true;
    return;
  }
  bandCommitInFlight = true;
  try {
    snapshot = await invoke('set_band_gain', {
      bandId: band.id,
      frequency: band.frequency,
      gainDb: band.gain_db,
      quality: band.quality,
    });
    clearMessage();
    renderAll();
  } catch (error) {
    showMessage(`Could not save the band: ${error}`);
    renderAll();
  } finally {
    bandCommitInFlight = false;
    if (bandCommitQueued) {
      bandCommitQueued = false;
      commitBand();
    }
  }
}

async function commitPreamp() {
  if (!invoke) return renderAll();
  try {
    snapshot = await invoke('set_preamp_db', { preampDb: snapshot.preamp_db });
    clearMessage();
  } catch (error) {
    showMessage(`Could not save the preamp: ${error}`);
  }
  renderAll();
}

async function commitPower() {
  const wanted = !snapshot.master_power;
  snapshot.master_power = wanted;
  renderHeader();
  scheduleDraw();
  if (!invoke) return;
  try {
    snapshot = await invoke('set_master_power', { enabled: wanted });
    clearMessage();
  } catch (error) {
    snapshot.master_power = !wanted;
    showMessage(`Could not change the chain state: ${error}`);
  }
  renderAll();
}

async function commitAutoHeadroom() {
  const wanted = !snapshot.auto_headroom;
  snapshot.auto_headroom = wanted;
  renderHeader();
  if (!invoke) return;
  try {
    snapshot = await invoke('set_auto_headroom', { enabled: wanted });
    clearMessage();
  } catch (error) {
    snapshot.auto_headroom = !wanted;
    showMessage(`Could not change auto headroom: ${error}`);
  }
  renderAll();
}

function setVolume(value) {
  const clamped = clamp(value, 0, 1);
  els.volume.value = String(clamped);
  els.volumeValue.textContent = `${Math.round(clamped * 100)}%`;
  setRangeFill(els.volume, clamped * 100);
  els.volumeIcon.classList.toggle('muted', clamped === 0);
  els.volumeIcon.setAttribute('aria-pressed', String(clamped === 0));
  if (clamped > 0) lastAudibleVolume = clamped;
  return clamped;
}

function scheduleVolumeWrite() {
  if (!invoke || !volumeAvailable) return;
  pendingVolume = Number(els.volume.value);
  if (volumeFrame) return;
  volumeFrame = requestAnimationFrame(async () => {
    volumeFrame = 0;
    const target = pendingVolume;
    pendingVolume = null;
    if (target === null) return;
    try {
      const applied = await invoke('set_output_volume', { volume: target });
      setVolume(applied);
      clearMessage();
    } catch (error) {
      showMessage(`Windows volume did not change: ${error}`);
    }
  });
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

const pointerPosition = (event) => {
  const rect = els.canvas.getBoundingClientRect();
  return { x: event.clientX - rect.left, y: event.clientY - rect.top };
};

els.canvas.addEventListener('pointerdown', (event) => {
  const { x, y } = pointerPosition(event);
  const rect = els.canvas.getBoundingClientRect();
  let nearest = null;
  let nearestDistance = Infinity;
  for (const band of snapshot.bands ?? []) {
    const dx = xOf(band.frequency, rect.width) - x;
    const dy = yOf(band.gain_db, rect.height) - y;
    const distance = Math.hypot(dx, dy);
    if (distance < nearestDistance) {
      nearestDistance = distance;
      nearest = band;
    }
  }
  if (!nearest) return;
  selectedId = nearest.id;
  drag = { pointerId: event.pointerId };
  els.canvas.setPointerCapture(event.pointerId);
  renderBands();
  renderInspector();
  scheduleDraw();
});

els.canvas.addEventListener('pointermove', (event) => {
  if (!drag || drag.pointerId !== event.pointerId) return;
  const band = selectedBand();
  if (!band) return;
  const { x, y } = pointerPosition(event);
  const rect = els.canvas.getBoundingClientRect();
  band.frequency = clamp(frequencyOf(x, rect.width), F_MIN, F_MAX);
  band.gain_db = clamp(dbOf(y, rect.height), -24, 24);
  renderInspector();
  renderBands();
  scheduleDraw();
});

function endDrag(event) {
  if (!drag || drag.pointerId !== event.pointerId) return;
  drag = null;
  if (els.canvas.hasPointerCapture(event.pointerId)) {
    els.canvas.releasePointerCapture(event.pointerId);
  }
  commitBand();
}

els.canvas.addEventListener('pointerup', endDrag);
els.canvas.addEventListener('pointercancel', endDrag);

els.bandGain.addEventListener('input', () => {
  const band = selectedBand();
  if (!band) return;
  band.gain_db = Number(els.bandGain.value);
  setRangeFill(els.bandGain, ((band.gain_db + 24) / 48) * 100);
  els.bandGainValue.textContent = formatDb(band.gain_db);
  renderBands();
  scheduleDraw();
});
els.bandGain.addEventListener('change', commitBand);

els.bandFrequency.addEventListener('input', () => {
  const band = selectedBand();
  if (!band) return;
  const ratio = Number(els.bandFrequency.value) / 1000;
  band.frequency = clamp(F_MIN * (F_MAX / F_MIN) ** ratio, F_MIN, F_MAX);
  setRangeFill(els.bandFrequency, ratio * 100);
  els.bandFreq.textContent = formatHz(band.frequency);
  renderBands();
  scheduleDraw();
});
els.bandFrequency.addEventListener('change', commitBand);

els.bandQuality.addEventListener('input', () => {
  const band = selectedBand();
  if (!band) return;
  band.quality = Number(els.bandQuality.value);
  setRangeFill(els.bandQuality, ((band.quality - 0.1) / 17.9) * 100);
  scheduleDraw();
});
els.bandQuality.addEventListener('change', commitBand);

els.resetBand.addEventListener('click', () => {
  const band = selectedBand();
  if (!band) return;
  const index = selectedIndex();
  band.frequency = ISO_FREQUENCIES[index] ?? band.frequency;
  band.gain_db = 0;
  band.quality =
    index === 0 || index === ISO_FREQUENCIES.length - 1 ? 0.71 : 1.41;
  renderBands();
  renderInspector();
  scheduleDraw();
  commitBand();
});

els.preamp.addEventListener('input', () => {
  snapshot.preamp_db = Number(els.preamp.value);
  setRangeFill(els.preamp, ((snapshot.preamp_db + 24) / 48) * 100);
  renderPreampReadout();
  scheduleDraw();
});
els.preamp.addEventListener('change', commitPreamp);

els.power.addEventListener('click', commitPower);
els.autoHeadroom.addEventListener('click', commitAutoHeadroom);

els.volume.addEventListener('input', () => {
  setVolume(Number(els.volume.value));
  scheduleVolumeWrite();
});

els.volumeIcon.addEventListener('click', () => {
  const next = Number(els.volume.value) > 0 ? 0 : lastAudibleVolume || 0.8;
  setVolume(next);
  scheduleVolumeWrite();
});

new ResizeObserver(scheduleDraw).observe(els.canvas.parentElement);
window.addEventListener('resize', scheduleDraw);
// The user may turn a device's enhancements on or off in Windows while this
// window is open, or switch the default output outright; asking again when it
// comes back to the front keeps the pill, the footer and the volume honest.
window.addEventListener('focus', () => {
  if (!invoke || engineInstallInFlight) return;
  refreshEngineStatus();
  syncOutputVolume();
  refreshEngineHost();
});

async function syncOutputVolume() {
  try {
    const volume = await invoke('get_output_volume');
    volumeAvailable = true;
    els.volume.disabled = false;
    setVolume(volume);
  } catch {
    volumeAvailable = false;
    els.volume.disabled = true;
    els.volumeValue.textContent = '—';
  }
}

// ---------------------------------------------------------------------------
// System engine activation
// ---------------------------------------------------------------------------

function renderCoreState() {
  const rate = Number(engineStatus?.defaultDevice?.sampleRate);
  const rateText =
    Number.isFinite(rate) && rate > 0
      ? `${(rate / 1000).toFixed(rate % 1000 === 0 ? 0 : 1)} kHz`
      : '';
  const core = hostStatus?.core_version || snapshot.native_abi_version || '';
  els.engineState.textContent = ['C++ core', core, rateText]
    .filter(Boolean)
    .join(' · ');
}

function engineBlockers(status) {
  if (!status?.installed) return [];
  const blockers = [];
  if (status.unsignedAllowed === false) {
    blockers.push('Windows is blocking unsigned effects');
  }
  if (status.runtimeBeside === false) {
    blockers.push('the engine runtime is missing');
  }
  if (status.serviceCanWrite === false) {
    blockers.push('the audio service cannot write the engine folder');
  }
  return blockers;
}

function renderEngineStatus(status) {
  engineStatus = status;
  const endpoints = Array.isArray(status?.endpoints) ? status.endpoints : [];
  const attached = endpoints.filter((endpoint) => endpoint.attached).length;
  const installed = Boolean(status?.installed);
  const attachable = endpoints.length > attached;
  const bypassed = endpoints.filter(
    (endpoint) => endpoint.attached && endpoint.tpBypassed
  ).length;
  const unloaded = endpoints.filter(
    (endpoint) => endpoint.attached && endpoint.loaded === false
  ).length;
  const stale =
    installed &&
    Boolean(status?.dllVersion) &&
    Boolean(status?.bundledVersion) &&
    status.dllVersion !== status.bundledVersion;
  const blockers = engineBlockers(status);
  const blocked = blockers.length > 0;
  const bridgeError =
    typeof status?.bridgeError === 'string' ? status.bridgeError : '';
  const active =
    installed && attached > 0 && bypassed === 0 && !blocked && !bridgeError;
  const working = active && unloaded < attached;
  const needsAttention =
    attachable || bypassed > 0 || stale || blocked || Boolean(bridgeError);

  const device = status?.defaultDevice;
  if (device?.name) {
    els.deviceName.textContent = device.name;
  }
  const mixRate = Number(device?.sampleRate);
  if (Number.isFinite(mixRate) && mixRate > 0 && mixRate !== sampleRate) {
    sampleRate = mixRate;
    scheduleDraw();
  }
  renderCoreState();

  const notes = blockers.slice();
  if (bridgeError) {
    notes.unshift(`Engine bridge: ${bridgeError}`);
  }
  if (unloaded > 0) {
    notes.push(
      `${unloaded} attached output${unloaded === 1 ? ' has' : 's have'} never loaded the engine`
    );
  }

  els.engineActivation.hidden = false;
  els.engineActivation.title = notes.join('\n');
  els.engineActivation.classList.toggle('active', working);
  if (bypassed > 0) {
    els.engineText.textContent = `Windows is bypassing the engine on ${bypassed} output${bypassed === 1 ? '' : 's'}`;
  } else if (blocked) {
    els.engineText.textContent = `Engine cannot load: ${blockers[0]}`;
  } else if (bridgeError) {
    els.engineText.textContent = `Engine config: ${bridgeError}`;
  } else if (active) {
    els.engineText.textContent = stale
      ? `Engine update available · ${status.dllVersion} → ${status.bundledVersion}`
      : unloaded === attached
        ? 'Engine attached, never loaded'
        : unloaded > 0
          ? `Engine loaded on ${attached - unloaded} of ${attached} outputs`
          : attachable
            ? `Engine on ${attached} of ${endpoints.length} outputs`
            : `Engine on ${attached} output${attached === 1 ? '' : 's'}`;
  } else if (installed) {
    els.engineText.textContent = endpoints.length
      ? 'Engine installed, not attached'
      : 'Engine installed, no outputs found';
  } else {
    els.engineText.textContent = 'System engine off';
  }
  els.engineInstall.hidden = installed && !needsAttention;
  els.engineInstall.textContent = installed
    ? blocked || bridgeError
      ? 'Repair engine'
      : bypassed
        ? 'Fix engine activation'
        : stale
          ? 'Update engine'
          : 'Attach all outputs'
    : 'Enable system audio';
  clearMessageStartingWith('Engine status:');
}

async function refreshEngineHost() {
  if (!invoke) return;
  try {
    hostStatus = await invoke('engine_host_status');
  } catch {
    hostStatus = null;
  }
  renderCoreState();
  if (hostStatus?.last_error) {
    showMessage(`DSP host: ${hostStatus.last_error}`);
  } else {
    clearMessageStartingWith('DSP host:');
  }
}

async function refreshEngineStatus() {
  try {
    renderEngineStatus(await invoke('engine_status'));
  } catch (error) {
    els.engineActivation.hidden = false;
    els.engineActivation.classList.remove('active');
    els.engineText.textContent = 'System engine unavailable';
    els.engineInstall.hidden = true;
    showMessage(`Engine status: ${error}`);
  }
}

els.engineInstall.addEventListener('click', async () => {
  if (engineInstallInFlight) return;
  engineInstallInFlight = true;
  els.engineInstall.disabled = true;
  els.engineInstall.textContent = 'Installing…';
  try {
    renderEngineStatus(await invoke('install_engine'));
    showMessage('System engine enabled — Windows audio restarted', 'ok');
  } catch (error) {
    showMessage(`Engine was not enabled: ${error}`);
    if (engineStatus) renderEngineStatus(engineStatus);
  } finally {
    engineInstallInFlight = false;
    els.engineInstall.disabled = false;
  }
});

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

async function boot() {
  renderAll();
  renderCoreState();
  setRangeFill(els.volume, 80);

  if (!invoke) {
    setVolume(0.8);
    els.volume.disabled = true;
    els.volumeValue.textContent = '—';
    setConnection('Preview only — launch the desktop shell for live control', 'preview');
    return;
  }

  try {
    snapshot = await invoke('get_snapshot');
    selectedId = snapshot.bands?.[3]?.id ?? snapshot.bands?.[0]?.id ?? selectedId;
    setConnection('Rust state centre connected · C++ core linked', 'live');
    clearMessage();
    refreshEngineStatus();
  } catch (error) {
    setConnection('Rust state centre unavailable', 'preview');
    showMessage(`Could not read the state: ${error}`);
  }

  renderAll();

  try {
    const volume = await invoke('get_output_volume');
    volumeAvailable = true;
    setVolume(volume);
    clearMessage();
  } catch (error) {
    volumeAvailable = false;
    els.volume.disabled = true;
    els.volumeValue.textContent = '—';
    showMessage(`System volume is unavailable: ${error}`);
  }

  await refreshEngineHost();
}

boot();
