/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

// The view is intentionally dependency-free: Tauri injects `__TAURI__`, and
// everything else is drawn from the snapshot the Rust state centre returns.
// No polling timers: system volume is written by the gesture that changes it,
// and the canvas is redrawn on the next animation frame.

import { registerView } from './views/registry.js';
import { equalizer } from './views/equalizer/index.js';
import {
  F_MIN,
  F_MAX,
  dbOf,
  frequencyOf,
  xOf,
  yOf,
} from './views/equalizer/graph.js';

const TAURI = window.__TAURI__;
const invoke = TAURI?.core?.invoke;

// Below this the curve's peaks are high enough that the guard is doing the
// cutting; trimming further turns the output quiet instead of fixing them.
const PREAMP_WARN_DB = -30;
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
  close_to_tray: true,
  preamp_db: 0,
  auto_preamp_db: 0,
  preamp_total_db: 0,
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
  presetSave: document.getElementById('preset-save'),
  presetDelete: document.getElementById('preset-delete'),
  presetForm: document.getElementById('preset-form'),
  presetName: document.getElementById('preset-name'),
  presetConfirm: document.getElementById('preset-confirm'),
  presetCancel: document.getElementById('preset-cancel'),
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
  preampWarning: document.getElementById('preamp-warning'),
  autoHeadroom: document.getElementById('auto-headroom'),
  startup: document.getElementById('startup'),
  closeToTray: document.getElementById('close-to-tray'),
  irLoad: document.getElementById('ir-load'),
  irFile: document.getElementById('ir-file'),
  irState: document.getElementById('ir-state'),
  irClear: document.getElementById('ir-clear'),
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

const graph = registerView(equalizer).createGraph(els.canvas);

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
let presetNames = [];
let presetBusy = false;
let startupEnabled = false;
let startupBusy = false;

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

function scheduleDraw() {
  if (drawFrame) return;
  drawFrame = requestAnimationFrame(() => {
    drawFrame = 0;
    drawGraph();
  });
}

function drawGraph() {
  graph.draw({
    bands: snapshot.bands ?? [],
    preampDb: snapshot.preamp_db,
    selectedId,
    sampleRate,
  });
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

function renderPresetSelect() {
  const active = snapshot.active_preset_name || 'Custom curve';
  const names = presetNames.includes(active)
    ? presetNames
    : [active, ...presetNames];
  const fragment = document.createDocumentFragment();
  for (const name of names) {
    const option = document.createElement('option');
    option.value = name;
    option.textContent = name;
    fragment.appendChild(option);
  }
  els.preset.replaceChildren(fragment);
  els.preset.value = active;
}

// A preset the user deleted, or one another window saved, changes the list;
// the chosen curve is the snapshot's own name, not whatever the control says.
async function refreshPresets() {
  if (!invoke) return;
  try {
    presetNames = await invoke('list_presets');
  } catch (error) {
    showMessage(`Presets: ${error}`);
    return;
  }
  renderPresetSelect();
}

function renderIr() {
  const ir = snapshot.impulse_response;
  if (!ir) {
    els.irState.textContent = 'No impulse response';
    els.irClear.hidden = true;
    return;
  }
  const seconds = Number(ir.seconds) || 0;
  const milliseconds = seconds * 1000;
  const rate = Number(ir.sample_rate) || 0;
  const peak = Math.max(Number(ir.left_peak_db), Number(ir.right_peak_db));
  els.irState.textContent = [
    `${ir.frames} taps`,
    `${milliseconds.toFixed(milliseconds < 10 ? 1 : 0)} ms`,
    rate > 0 ? `${(rate / 1000).toFixed(1)} kHz` : '',
    Number.isFinite(peak) ? `peak ${formatDb(peak)} dB` : '',
  ]
    .filter(Boolean)
    .join(' · ');
  els.irClear.hidden = false;
}

function renderHeader() {
  renderPresetSelect();
  els.power.classList.toggle('on', Boolean(snapshot.master_power));
  els.power.setAttribute('aria-pressed', String(Boolean(snapshot.master_power)));
  els.chainState.textContent = snapshot.master_power
    ? 'Chain enabled'
    : 'Chain bypassed';
  els.autoHeadroom.textContent = snapshot.auto_headroom ? 'On' : 'Off';
  els.autoHeadroom.setAttribute('aria-pressed', String(Boolean(snapshot.auto_headroom)));
  els.closeToTray.textContent = snapshot.close_to_tray ? 'On' : 'Off';
  els.closeToTray.setAttribute(
    'aria-pressed',
    String(Boolean(snapshot.close_to_tray))
  );
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
  renderIr();
}

// With auto headroom on, the guard starts at the curve's own level and the
// slider is a trim on top of it. Showing only the trim let a -24 dB trim look
// like the whole gain while the chain actually sat at -62 dB.
function renderPreampReadout() {
  const manual = Number(snapshot.preamp_db) || 0;
  const auto = snapshot.auto_headroom
    ? Math.min(0, Number(snapshot.auto_preamp_db) || 0)
    : 0;
  const total = manual + auto;
  els.preampValue.textContent = `${formatDb(total)} dB`;
  els.preampValue.title = snapshot.auto_headroom
    ? `Auto headroom ${formatDb(auto)} dB + trim ${formatDb(manual)} dB`
    : '';
  const tooLow = total < PREAMP_WARN_DB;
  els.preampWarning.hidden = !tooLow;
  els.preampWarning.textContent = tooLow
    ? `Total gain ${formatDb(total)} dB. The curve peaks are above 0 dB — lower them instead of trimming this far.`
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

function renderStartup() {
  els.startup.textContent = startupEnabled ? 'On' : 'Off';
  els.startup.setAttribute('aria-pressed', String(startupEnabled));
}

async function refreshHotkeys() {
  if (!invoke) return;
  try {
    const failures = await invoke('hotkey_failures');
    if (Array.isArray(failures) && failures.length > 0) {
      showMessage(`Global hotkeys unavailable: ${failures.join('; ')}`);
    }
  } catch {
    // An older shell without the command has no hotkey report to show.
  }
}

async function refreshStartup() {
  if (!invoke) return renderStartup();
  try {
    startupEnabled = await invoke('get_startup_enabled');
  } catch {
    startupEnabled = false;
  }
  renderStartup();
}

async function toggleStartup() {
  if (!invoke || startupBusy) return;
  startupBusy = true;
  try {
    startupEnabled = await invoke('set_startup_enabled', {
      enabled: !startupEnabled,
    });
    showMessage(
      startupEnabled
        ? 'FluidEQ will start with Windows'
        : 'FluidEQ will not start with Windows',
      'ok'
    );
  } catch (error) {
    showMessage(`Could not change the startup entry: ${error}`);
  } finally {
    startupBusy = false;
    renderStartup();
  }
}

async function commitCloseToTray() {
  const wanted = !snapshot.close_to_tray;
  snapshot.close_to_tray = wanted;
  renderHeader();
  if (!invoke) return;
  try {
    snapshot = await invoke('set_close_to_tray', { enabled: wanted });
    clearMessage();
  } catch (error) {
    snapshot.close_to_tray = !wanted;
    showMessage(`Could not change the close behavior: ${error}`);
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

// ---------------------------------------------------------------------------
// Presets
// ---------------------------------------------------------------------------

async function applyPreset(name) {
  if (!invoke || presetBusy || !name) return;
  presetBusy = true;
  try {
    snapshot = await invoke('apply_preset', { name });
    clearMessage();
    renderAll();
  } catch (error) {
    showMessage(`Could not apply the preset: ${error}`);
    renderAll();
  } finally {
    presetBusy = false;
  }
}

els.preset.addEventListener('change', () => applyPreset(els.preset.value));

els.presetSave.addEventListener('click', () => {
  els.presetForm.hidden = false;
  const active = snapshot.active_preset_name;
  els.presetName.value =
    !active || active === 'Custom curve' ? '' : active;
  els.presetName.focus();
  els.presetName.select();
});

els.presetCancel.addEventListener('click', () => {
  els.presetForm.hidden = true;
});

els.presetForm.addEventListener('submit', async (event) => {
  event.preventDefault();
  const name = els.presetName.value.trim();
  if (!invoke || !name) return;
  try {
    presetNames = await invoke('save_preset', { name });
    snapshot.active_preset_name = name;
    els.presetForm.hidden = true;
    showMessage(`Saved the preset "${name}"`, 'ok');
    renderHeader();
  } catch (error) {
    showMessage(`Could not save the preset: ${error}`);
  }
});

els.presetDelete.addEventListener('click', async () => {
  const name = els.preset.value;
  if (!invoke || !name) return;
  if (!window.confirm(`Delete the preset "${name}"?`)) return;
  try {
    presetNames = await invoke('delete_preset', { name });
    snapshot = await invoke('get_snapshot');
    renderAll();
  } catch (error) {
    showMessage(`Could not delete the preset: ${error}`);
  }
});

// ---------------------------------------------------------------------------
// Convolution
// ---------------------------------------------------------------------------

els.irLoad.addEventListener('click', () => els.irFile.click());

els.irFile.addEventListener('change', async () => {
  const file = els.irFile.files?.[0];
  if (!invoke || !file) return;
  try {
    const bytes = new Uint8Array(await file.arrayBuffer());
    snapshot = await invoke('load_ir', { bytes: Array.from(bytes) });
    clearMessage();
    renderAll();
  } catch (error) {
    showMessage(`Could not load the impulse response: ${error}`);
  } finally {
    els.irFile.value = '';
  }
});

els.irClear.addEventListener('click', async () => {
  if (!invoke) return;
  try {
    snapshot = await invoke('clear_ir');
    clearMessage();
    renderAll();
  } catch (error) {
    showMessage(`Could not clear the impulse response: ${error}`);
  }
});

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
els.closeToTray.addEventListener('click', commitCloseToTray);
els.startup.addEventListener('click', toggleStartup);

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
  refreshStartup();
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

// An attached effect is not a loaded one: Windows creates the APO when it
// rebuilds the output's graph, and it will not when the runtime is missing,
// the folder is closed to the audio service, or the slot has no processing
// mode. Each has a different next step, so the window names it instead of
// leaving the output silent with no reason.
function loadAdvice(endpoint, status) {
  if (status?.serviceCanWrite === false) {
    return 'the audio service cannot write the engine folder — repair the engine';
  }
  if (status?.runtimeBeside === false) {
    return 'the engine runtime is missing — reinstall the engine';
  }
  if (endpoint?.modeMissing) {
    return 'the slot has no processing mode — attach again';
  }
  const log = Array.isArray(status?.engineLog) ? status.engineLog : [];
  const reason = [...log]
    .reverse()
    .find((line) => line.startsWith('pass-through:'));
  if (reason) {
    return `Windows is passing it through: ${reason.slice('pass-through: '.length)}`;
  }
  return 'restart Windows audio so the output creates the engine';
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
  const unloadedEndpoints = endpoints.filter(
    (endpoint) => endpoint.attached && endpoint.loaded === false
  );
  const unloaded = unloadedEndpoints.length;
  const advice = unloaded > 0 ? loadAdvice(unloadedEndpoints[0], status) : '';
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
    attachable || bypassed > 0 || stale || blocked || Boolean(bridgeError) || unloaded > 0;

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
      `${unloaded} attached output${unloaded === 1 ? ' has' : 's have'} never loaded the engine: ${advice}`
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
        ? `Engine attached, never loaded · ${unloadedEndpoints[0]?.name ?? 'output'}`
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
        : unloaded > 0
          ? 'Restart Windows audio'
          : stale
            ? 'Update engine'
            : 'Attach all outputs'
    : 'Enable system audio';
  if (unloaded > 0) {
    showMessage(`Engine never loaded: ${advice}`);
  } else {
    clearMessageStartingWith('Engine never loaded:');
    clearMessageStartingWith('Engine status:');
  }
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

async function followDefaultDevice(status) {
  const device = status?.defaultDevice;
  if (!invoke || !device?.guid || !device?.name) return;
  try {
    const followed = await invoke('follow_output_device', {
      guid: device.guid,
      name: device.name,
    });
    if (followed) {
      snapshot = followed;
      renderAll();
    }
  } catch (error) {
    showMessage(`Could not follow the default output: ${error}`);
  }
}

async function refreshEngineStatus() {
  try {
    const status = await invoke('engine_status');
    renderEngineStatus(status);
    await followDefaultDevice(status);
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
    refreshPresets();
    refreshStartup();
    refreshHotkeys();
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

// The tray's bypass entry changes the chain in the other direction; the
// window is told the new snapshot rather than guessing at it.
TAURI?.event?.listen?.('snapshot', (event) => {
  snapshot = event.payload;
  renderAll();
});

// The volume hotkeys change Windows directly; the slider follows the same
// number rather than waiting for the next focus to notice.
TAURI?.event?.listen?.('volume', (event) => setVolume(event.payload));

boot();
