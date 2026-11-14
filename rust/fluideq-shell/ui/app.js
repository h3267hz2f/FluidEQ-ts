/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

// The view is intentionally dependency-free: Tauri injects `__TAURI__`, and
// everything else is drawn from the snapshot the Rust state centre returns.
// No polling timers: system volume is written by the gesture that changes it,
// and the canvas is redrawn on the next animation frame.

import { registerView, viewFor } from './views/registry.js';
import { equalizer } from './views/equalizer/index.js';
import { config } from './views/config/index.js';
import { games } from './views/games/index.js';
import {
  rack,
  rackStages,
  stageIsOn,
  stageSummary,
  stageSupportsIsolate,
  stageIsolate,
  NORMALIZER_MODES,
  normalizerModeKey,
  MASTER_TARGETS,
  DYNAMICS_PRESETS,
  dynamicsPresetKey,
} from './views/rack/index.js';
import { settings } from './views/settings/index.js';
import { room, KEMAR_HEADS } from './views/room/index.js';
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
  32, 50, 80, 125, 200, 315, 500, 800, 1250, 2000, 3150, 5000, 8000, 12500,
  16000,
];

const DEMO_BANDS = [
  [32, 1.4, 'LSC', 0.71],
  [50, 1.4, 'PK', 1.41],
  [80, -0.9, 'PK', 1.41],
  [125, 0.5, 'PK', 1.41],
  [200, 0.2, 'PK', 1.41],
  [315, -1.7, 'PK', 1.41],
  [500, -0.7, 'PK', 1.41],
  [800, 0.3, 'PK', 1.41],
  [1250, 0.2, 'PK', 1.41],
  [2000, -0.3, 'PK', 1.41],
  [3150, -0.5, 'PK', 1.41],
  [5000, 0.5, 'PK', 1.41],
  [8000, 0.7, 'PK', 1.41],
  [12500, 0.9, 'PK', 1.41],
  [16000, 0.9, 'HSC', 0.71],
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
  active_preset_name: 'Balanced',
  eq_model: 'wide',
  eq_model_amount: 1,
  matched_treble: true,
  tone: {
    bass_db: 0,
    bass_freq_hz: 105,
    bass_q: 0.71,
    treble_db: 0,
    treble_freq_hz: 8500,
    treble_q: 0.71,
    low_cut_hz: 20,
    high_cut_hz: 20000,
  },
  bands: DEMO_BANDS.map((band) => ({ ...band })),
  residency: {},
  models: {},
});

const els = {
  canvas: document.getElementById('response'),
  preset: document.getElementById('preset'),
  presetSave: document.getElementById('preset-save'),
  presetDelete: document.getElementById('preset-delete'),
  chainExport: document.getElementById('chain-export'),
  chainImport: document.getElementById('chain-import'),
  chainFile: document.getElementById('chain-file'),
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
  bandFilter: document.getElementById('band-filter'),
  bandEar: document.getElementById('band-ear'),
  bandEnable: document.getElementById('band-enable'),
  bandGainValue: document.getElementById('band-gain-value'),
  bandGain: document.getElementById('band-gain'),
  bandFrequency: document.getElementById('band-frequency'),
  bandQuality: document.getElementById('band-quality'),
  resetBand: document.getElementById('reset-band'),
  preamp: document.getElementById('preamp'),
  preampValue: document.getElementById('preamp-value'),
  preampWarning: document.getElementById('preamp-warning'),
  autoHeadroom: document.getElementById('auto-headroom'),
  resetTone: document.getElementById('reset-tone'),
  toneBass: document.getElementById('tone-bass'),
  toneBassValue: document.getElementById('tone-bass-value'),
  toneTreble: document.getElementById('tone-treble'),
  toneTrebleValue: document.getElementById('tone-treble-value'),
  toneLowCut: document.getElementById('tone-low-cut'),
  toneLowCutValue: document.getElementById('tone-low-cut-value'),
  toneHighCut: document.getElementById('tone-high-cut'),
  toneHighCutValue: document.getElementById('tone-high-cut-value'),
  eqModel: document.getElementById('eq-model'),
  eqModelAmount: document.getElementById('eq-model-amount'),
  eqModelAmountValue: document.getElementById('eq-model-amount-value'),
  matchedTreble: document.getElementById('matched-treble'),
  startup: document.getElementById('startup'),
  closeToTray: document.getElementById('close-to-tray'),
  irLoad: document.getElementById('ir-load'),
  irFile: document.getElementById('ir-file'),
  irState: document.getElementById('ir-state'),
  irClear: document.getElementById('ir-clear'),
  opraVendor: document.getElementById('opra-vendor'),
  opraModel: document.getElementById('opra-model'),
  opraStatus: document.getElementById('opra-status'),
  opraClear: document.getElementById('opra-clear'),
  apoImport: document.getElementById('apo-import'),
  apoFile: document.getElementById('apo-file'),
  apoExport: document.getElementById('apo-export'),
  apoExportStatus: document.getElementById('apo-export-status'),
  bandCount: document.getElementById('band-count'),
  meterLufs: document.getElementById('meter-lufs'),
  meterShort: document.getElementById('meter-short'),
  meterPeak: document.getElementById('meter-peak'),
  meterSpectrum: document.getElementById('meter-spectrum'),
  chainState: document.getElementById('chain-state'),
  connection: document.getElementById('connection'),
  message: document.getElementById('message'),
  engineActivation: document.getElementById('engine-activation'),
  engineText: document.getElementById('engine-text'),
  engineInstall: document.getElementById('engine-install'),
  engineState: document.getElementById('engine-state'),
  deviceName: document.querySelector('.device-name'),
  pageTitle: document.getElementById('page-title'),
  pageEyebrow: document.getElementById('page-eyebrow'),
  presetBar: document.querySelector('.preset-bar'),
  settingsDevice: document.getElementById('settings-device'),
  hotkeyState: document.getElementById('hotkey-state'),
  configEndpoints: document.getElementById('config-endpoints'),
  configEndpointsEmpty: document.getElementById('config-endpoints-empty'),
  configSettle: document.getElementById('config-settle'),
  configNow: document.getElementById('config-now'),
  configProfiles: document.getElementById('config-profiles'),
  configProfilesEmpty: document.getElementById('config-profiles-empty'),
  configLog: document.getElementById('config-log'),
  configLogEmpty: document.getElementById('config-log-empty'),
  gamesList: document.getElementById('games-list'),
  gamesEmpty: document.getElementById('games-empty'),
  gamesUnavailable: document.getElementById('games-unavailable'),
  gamesForm: document.getElementById('games-form'),
  gamesExecutable: document.getElementById('games-executable'),
  gamesPreset: document.getElementById('games-preset'),
  rackStages: document.getElementById('rack-stages'),
  rackEditor: document.getElementById('rack-editor'),
  rackEditorTitle: document.getElementById('rack-editor-title'),
  rackEditorDetail: document.getElementById('rack-editor-detail'),
  rackEditorControls: document.getElementById('rack-editor-controls'),
  mediaBar: document.getElementById('media-bar'),
  mediaCover: document.getElementById('media-cover'),
  mediaTitle: document.getElementById('media-title'),
  mediaArtist: document.getElementById('media-artist'),
  mediaPlay: document.getElementById('media-play'),
  mediaPrevious: document.getElementById('media-previous'),
  mediaNext: document.getElementById('media-next'),
  mediaTime: document.getElementById('media-time'),
  roomPower: document.getElementById('room-power'),
  roomPresetChips: document.getElementById('room-preset-chips'),
  roomHeadChips: document.getElementById('room-head-chips'),
  roomSpeakerChips: document.getElementById('room-speaker-chips'),
  roomSpeakerMute: document.getElementById('room-speaker-mute'),
  roomSpeakerAngle: document.getElementById('room-speaker-angle'),
  roomSpeakerAngleVal: document.getElementById('room-speaker-angle-val'),
  roomSpeakerDist: document.getElementById('room-speaker-dist'),
  roomSpeakerDistVal: document.getElementById('room-speaker-dist-val'),
  roomSpeakerLevel: document.getElementById('room-speaker-level'),
  roomSpeakerLevelVal: document.getElementById('room-speaker-level-val'),
  roomSize: document.getElementById('room-size'),
  roomSizeVal: document.getElementById('room-size-val'),
  roomDist: document.getElementById('room-dist'),
  roomDistVal: document.getElementById('room-dist-val'),
  roomWalls: document.getElementById('room-walls'),
  roomWallsVal: document.getElementById('room-walls-val'),
  roomAmbience: document.getElementById('room-ambience'),
  roomAmbienceVal: document.getElementById('room-ambience-val'),
  roomDecay: document.getElementById('room-decay'),
  roomDecayVal: document.getElementById('room-decay-val'),
  roomBassMgmt: document.getElementById('room-bass-mgmt'),
  roomCrossover: document.getElementById('room-crossover'),
  roomCrossoverVal: document.getElementById('room-crossover-val'),
  roomCentre: document.getElementById('room-centre'),
  roomCentreVal: document.getElementById('room-centre-val'),
  roomSub: document.getElementById('room-sub'),
  roomSubVal: document.getElementById('room-sub-val'),
  roomSpace: document.getElementById('room-space'),
  roomSpaceVal: document.getElementById('room-space-val'),
  roomDamping: document.getElementById('room-damping'),
  roomDampingVal: document.getElementById('room-damping-val'),
  roomRendererVal: document.getElementById('room-renderer-val'),
  roomRendererNote: document.getElementById('room-renderer-note'),
  roomUpgrade: document.getElementById('room-upgrade'),
  roomReset: document.getElementById('room-reset'),
  roomUpmix: document.getElementById('room-upmix'),
  roomUpmixAmount: document.getElementById('room-upmix-amount'),
  roomUpmixAmountVal: document.getElementById('room-upmix-amount-val'),
};

const graph = registerView(equalizer).createGraph(els.canvas);
registerView(config);
registerView(rack);
registerView(room);
registerView(games);
registerView(settings);

let activeView = 'equalizer';

let snapshot = demoSnapshot();
let selectedId = 4;
let drag = null;
let drawFrame = 0;
let spectrumBins = null;
let lastDynamics = null;
let pendingVolume = null;
let volumeFrame = 0;
let volumeAvailable = false;
let lastAudibleVolume = 0.8;
let bandCommitInFlight = false;
let bandCommitQueued = false;
let engineStatus = null;
let hostStatus = null;
let engineInstallInFlight = false;
let engineEndpointInFlight = false;
let presetNames = [];
let presetCatalog = [];
let roomCatalog = [];
let selectedRoomSpeaker = 0;

// The wire's order: front left and right, centre, side pair, rear pair, then
// the subwoofer, which has a level and a mute but no place on the ring.
const ROOM_SPEAKERS = [
  ['FL', 'Front left'],
  ['FR', 'Front right'],
  ['C', 'Centre'],
  ['SL', 'Side left'],
  ['SR', 'Side right'],
  ['RL', 'Rear left'],
  ['RR', 'Rear right'],
  ['Sub', 'Subwoofer'],
];
const ROOM_SUBWOOFER = 7;
let presetBusy = false;
let startupEnabled = false;
let startupBusy = false;
let hotkeyFailures = [];
let gameRules = [];
let gameWatchAvailable = false;
let selectedRackStage = 'normalizer';
let opraVendors = [];
let opraModels = [];
let activeOpraVendor = '';
let activeOpraModel = '';

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

function updateMeters(packet) {
  const stage = packet.stage_meters ?? {};
  lastDynamics = stage.dynamics ?? null;
  paintDynamicsMeters();
  const loudness = stage.loudness ?? {};
  const integrated = Number(loudness.integrated_lufs);
  const shortTerm = Number(loudness.short_term_lufs);
  const truePeak = Number(stage.master_telemetry?.auto_headroom_true_peak_db);

  els.meterLufs.textContent = Number.isFinite(integrated)
    ? `${integrated.toFixed(1)} LUFS`
    : '—';
  els.meterShort.textContent = Number.isFinite(shortTerm)
    ? `${shortTerm.toFixed(1)} LUFS`
    : '—';
  els.meterPeak.textContent = Number.isFinite(truePeak)
    ? `${formatDb(truePeak)} dBTP`
    : '—';
  els.meterSpectrum.textContent = packet.sleeping
    ? 'Idle'
    : `${packet.target_fps || 0} fps`;
}

function drawGraph() {
  graph.draw({
    bands: snapshot.bands ?? [],
    preampDb: snapshot.preamp_db,
    selectedId,
    sampleRate,
    tone: snapshot.tone,
    eqModel: snapshot.eq_model,
    eqModelAmount: snapshot.eq_model_amount,
    spectrum: spectrumBins,
  });
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

const PRESET_GROUP_LABELS = [
  ['basic', 'Basic'],
  ['genre', 'Genre'],
  ['voice', 'Voice'],
  ['scene', 'Scene'],
  ['device', 'Device'],
  ['character', 'Character'],
  ['repair', 'Repair'],
  ['gaming', 'Gaming'],
  ['custom', 'Yours'],
];

// Factory curves keep the catalogue's own order under their headings; the
// user's saved files follow in one section of their own.
function renderPresetSelect() {
  const active = snapshot.active_preset_name || 'Custom curve';
  const names = presetNames.includes(active)
    ? presetNames
    : [active, ...presetNames];
  const groupOf = new Map(presetCatalog.map((entry) => [entry.name, entry.group]));
  const known = new Set(groupOf.keys());
  const present = new Set(names);
  const ordered = [
    ...presetCatalog.map((entry) => entry.name).filter((name) => present.has(name)),
    ...names.filter((name) => !known.has(name)),
  ];
  const sections = new Map();
  for (const name of ordered) {
    const group = groupOf.get(name) ?? 'custom';
    if (!sections.has(group)) sections.set(group, []);
    sections.get(group).push(name);
  }
  const fragment = document.createDocumentFragment();
  for (const [group, label] of PRESET_GROUP_LABELS) {
    const entries = sections.get(group);
    if (!entries?.length) continue;
    const optgroup = document.createElement('optgroup');
    optgroup.label = label;
    for (const name of entries) {
      const option = document.createElement('option');
      option.value = name;
      option.textContent = name;
      optgroup.appendChild(option);
    }
    fragment.appendChild(optgroup);
  }
  els.preset.replaceChildren(fragment);
  els.preset.value = active;
}

// A preset the user deleted, or one another window saved, changes the list;
// the chosen curve is the snapshot's own name, not whatever the control says.
async function refreshPresets() {
  if (!invoke) return;
  try {
    [presetNames, presetCatalog] = await Promise.all([
      invoke('list_presets'),
      invoke('list_preset_catalog'),
    ]);
  } catch (error) {
    showMessage(`Presets: ${error}`);
    return;
  }
  renderPresetSelect();
  if (activeView === 'games') renderGames();
  else renderGamePresetChoices();
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
    if (band.enabled === false) button.classList.add('off');
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

  const filterKey = band.filter_type || 'PK';
  els.bandFilter.value = filterKey;
  els.bandEar.value = String(Number(band.ear) || 0);
  const enabled = band.enabled !== false;
  els.bandEnable.textContent = enabled ? 'On' : 'Bypass';
  els.bandEnable.setAttribute('aria-pressed', String(enabled));

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
  renderToneAndVoicing();
  renderIr();
}

function normalizeEqModelKey(model) {
  const raw = String(model || 'clean').toLowerCase();
  if (raw.includes('proportional')) return 'proportional';
  if (raw.includes('wide')) return 'wide';
  if (raw.includes('asymmetric')) return 'asymmetric';
  return 'clean';
}

function ensureTone() {
  if (!snapshot.tone) {
    snapshot.tone = {
      bass_db: 0,
      bass_freq_hz: 105,
      bass_q: 0.71,
      treble_db: 0,
      treble_freq_hz: 8500,
      treble_q: 0.71,
      low_cut_hz: 20,
      high_cut_hz: 20000,
    };
  }
  return snapshot.tone;
}

function renderToneAndVoicing() {
  const tone = ensureTone();
  const bassDb = clamp(Number(tone.bass_db) || 0, -12, 12);
  const trebleDb = clamp(Number(tone.treble_db) || 0, -12, 12);
  const lowCutHz = clamp(Number(tone.low_cut_hz) || 20, 20, 250);
  const highCutHz = clamp(Number(tone.high_cut_hz) || 20000, 4000, 20000);
  const modelKey = normalizeEqModelKey(snapshot.eq_model);
  const modelAmount = clamp(
    Number.isFinite(Number(snapshot.eq_model_amount))
      ? Number(snapshot.eq_model_amount)
      : 1,
    0,
    1,
  );
  const matched = snapshot.matched_treble !== false;

  setRangeValue(els.toneBass, bassDb);
  setRangeFill(els.toneBass, ((bassDb + 12) / 24) * 100);
  els.toneBassValue.textContent = `${formatDb(bassDb)} dB`;

  setRangeValue(els.toneTreble, trebleDb);
  setRangeFill(els.toneTreble, ((trebleDb + 12) / 24) * 100);
  els.toneTrebleValue.textContent = `${formatDb(trebleDb)} dB`;

  setRangeValue(els.toneLowCut, lowCutHz);
  setRangeFill(els.toneLowCut, ((lowCutHz - 20) / 230) * 100);
  els.toneLowCutValue.textContent = formatHz(lowCutHz);

  setRangeValue(els.toneHighCut, highCutHz);
  setRangeFill(els.toneHighCut, ((highCutHz - 4000) / 16000) * 100);
  els.toneHighCutValue.textContent = formatHz(highCutHz);

  els.eqModel.value = modelKey;
  setRangeValue(els.eqModelAmount, modelAmount);
  setRangeFill(els.eqModelAmount, modelAmount * 100);
  els.eqModelAmountValue.textContent = `${Math.round(modelAmount * 100)}%`;
  els.eqModelAmount.disabled = modelKey === 'clean';

  els.matchedTreble.textContent = matched ? 'Precise' : 'Classic';
  els.matchedTreble.setAttribute('aria-pressed', String(matched));
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
  renderSettings();
  if (activeView === 'config') renderConfig();
  if (activeView === 'rack') renderRack();
  if (activeView === 'room') renderRoom();
  drawGraph();
}

// ---------------------------------------------------------------------------
// OPRA Headphone Library
// ---------------------------------------------------------------------------

function formatBrandName(slug) {
  return String(slug || '')
    .split('_')
    .map((word) => word.charAt(0).toUpperCase() + word.slice(1))
    .join(' ');
}

// Zero is a real value for several room fields (Small head, no wall
// absorption, no ambience), so a `|| fallback` would silently show the
// default and make the control look unresponsive.
function numberOr(value, fallback) {
  const number = Number(value);
  return Number.isFinite(number) ? number : fallback;
}

function renderOpraVendors() {
  const fragment = document.createDocumentFragment();
  const placeholder = document.createElement('option');
  placeholder.value = '';
  placeholder.textContent = 'Select brand…';
  fragment.appendChild(placeholder);
  for (const vendor of opraVendors) {
    const opt = document.createElement('option');
    opt.value = vendor;
    opt.textContent = formatBrandName(vendor);
    fragment.appendChild(opt);
  }
  els.opraVendor.replaceChildren(fragment);
  els.opraVendor.value = activeOpraVendor;
}

function renderOpraModels() {
  const fragment = document.createDocumentFragment();
  const placeholder = document.createElement('option');
  placeholder.value = '';
  placeholder.textContent = 'Select model…';
  fragment.appendChild(placeholder);
  for (const model of opraModels) {
    const opt = document.createElement('option');
    opt.value = model;
    opt.textContent = formatBrandName(model);
    fragment.appendChild(opt);
  }
  els.opraModel.replaceChildren(fragment);
  els.opraModel.value = activeOpraModel;
}

async function refreshOpraVendors() {
  if (!invoke) return;
  try {
    opraVendors = await invoke('list_opra_vendors');
    renderOpraVendors();
  } catch {
    // Non-fatal if OPRA directory is missing
  }
}

async function selectOpraVendor(vendor) {
  activeOpraVendor = vendor;
  activeOpraModel = '';
  if (!vendor || !invoke) {
    els.opraModel.disabled = true;
    els.opraModel.replaceChildren();
    const opt = document.createElement('option');
    opt.value = '';
    opt.textContent = 'Select model…';
    els.opraModel.appendChild(opt);
    return;
  }
  try {
    opraModels = await invoke('list_opra_models', { vendor });
    renderOpraModels();
    els.opraModel.disabled = opraModels.length === 0;
  } catch (error) {
    showMessage(`Could not list models for ${vendor}: ${error}`);
  }
}

async function applyOpraModel(model) {
  if (!model || !activeOpraVendor || !invoke) return;
  activeOpraModel = model;
  try {
    snapshot = await invoke('apply_opra_curve', {
      vendor: activeOpraVendor,
      product: model,
      curveId: null,
    });
    clearMessage();
    showMessage(
      `Applied correction for ${formatBrandName(activeOpraVendor)} ${formatBrandName(model)}`,
      'ok',
    );
    els.opraStatus.hidden = false;
    els.opraStatus.textContent = `Correction applied: ${snapshot.active_preset_name}`;
    els.opraClear.hidden = false;
    renderAll();
  } catch (error) {
    showMessage(`Could not apply curve for ${model}: ${error}`);
  }
}

function clearOpraSelection() {
  activeOpraVendor = '';
  activeOpraModel = '';
  els.opraVendor.value = '';
  els.opraModel.replaceChildren();
  const opt = document.createElement('option');
  opt.value = '';
  opt.textContent = 'Select model…';
  els.opraModel.appendChild(opt);
  els.opraModel.disabled = true;
  els.opraStatus.hidden = true;
  els.opraStatus.textContent = '';
  els.opraClear.hidden = true;
}

async function importApoFile() {
  const file = els.apoFile.files?.[0];
  if (!file || !invoke) {
    els.apoFile.value = '';
    return;
  }
  try {
    const text = await file.text();
    snapshot = await invoke('import_apo_curve', { text });
    clearMessage();
    showMessage(
      `Imported ${snapshot.bands.length} bands from ${file.name}`,
      'ok'
    );
    renderAll();
  } catch (error) {
    showMessage(`Could not import ${file.name}: ${error}`);
  } finally {
    els.apoFile.value = '';
  }
}

async function exportApoFile() {
  if (!invoke) return;
  try {
    const path = await invoke('export_apo_curve');
    els.apoExportStatus.hidden = false;
    els.apoExportStatus.textContent = `Saved to ${path}`;
    clearMessage();
  } catch (error) {
    showMessage(`Could not export the curve: ${error}`);
  }
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

function renderSettings() {
  renderStartup();
  const device = engineStatus?.defaultDevice;
  els.settingsDevice.textContent = device?.name || 'System default';
  if (hotkeyFailures.length > 0) {
    els.hotkeyState.hidden = false;
    els.hotkeyState.textContent = `Unavailable on this machine: ${hotkeyFailures.join('; ')}`;
  } else {
    els.hotkeyState.hidden = true;
    els.hotkeyState.textContent = '';
  }
}

const EQ_MODEL_NAMES = {
  clean: 'Clean',
  proportional: 'Proportional Q',
  wide: 'Wide tone',
  asymmetric: 'Asymmetric',
  FEQ_EQ_MODEL_CLEAN: 'Clean',
  FEQ_EQ_MODEL_PROPORTIONAL: 'Proportional Q',
  FEQ_EQ_MODEL_WIDE: 'Wide tone',
  FEQ_EQ_MODEL_ASYMMETRIC: 'Asymmetric',
};

function configRow(label, value) {
  const row = document.createElement('div');
  row.className = 'stat-row';
  const name = document.createElement('span');
  name.textContent = label;
  const strong = document.createElement('strong');
  strong.textContent = value;
  row.append(name, strong);
  return row;
}

function configCard(title, facts, mono, current) {
  const row = document.createElement('div');
  row.className = `config-endpoint${current ? ' current' : ''}`;

  const head = document.createElement('div');
  head.className = 'config-name';
  const label = document.createElement('span');
  label.textContent = title;
  head.appendChild(label);
  if (current) {
    const badge = document.createElement('span');
    badge.className = 'config-current';
    badge.textContent = 'Active';
    head.appendChild(badge);
  }

  const state = document.createElement('p');
  state.className = 'config-meta';
  state.textContent = facts.filter(Boolean).join(' · ');

  const address = document.createElement('p');
  address.className = 'config-mono';
  address.textContent = mono || '';

  row.append(head, state, address);
  return row;
}

function configEndpointCard(endpoint, currentGuid) {
  const emptySlots = Number(endpoint.emptySlots);
  const facts = [
    endpoint.slot ? `slot ${endpoint.slot}` : 'no slot',
    endpoint.attached ? 'attached' : 'not attached',
    endpoint.attached && !endpoint.loaded ? 'never loaded' : '',
    endpoint.tpBypassed ? 'Windows bypassing effects' : '',
    endpoint.modeMissing ? 'no processing mode' : '',
    Array.isArray(endpoint.slotsTried) && endpoint.slotsTried.length > 0
      ? `tried ${endpoint.slotsTried.join(' → ')}`
      : '',
    Number.isFinite(emptySlots)
      ? `${emptySlots} empty slot${emptySlots === 1 ? '' : 's'}`
      : '',
  ];
  const current = Boolean(currentGuid) && endpoint.guid === currentGuid;
  const card = configCard(
    endpoint.name || 'Unnamed output',
    facts,
    endpoint.guid,
    current
  );
  if (endpoint.guid) {
    const action = document.createElement('button');
    action.type = 'button';
    action.className = 'stat-toggle';
    action.textContent = endpoint.attached ? 'Detach' : 'Attach';
    action.disabled = engineEndpointInFlight;
    action.addEventListener('click', () => toggleEndpointAttachment(endpoint));
    card.append(action);
  }
  return card;
}

function configProfileCard(profile, isActive) {
  const bands = Array.isArray(profile.bands) ? profile.bands.length : 0;
  const facts = [
    profile.active_preset_name || 'Custom curve',
    `${formatDb(Number(profile.preamp_db) || 0)} dB`,
    `${bands} band${bands === 1 ? '' : 's'}`,
  ];
  return configCard(
    profile.device_name || 'Unnamed output',
    facts,
    profile.guid,
    isActive
  );
}

// The page says what Windows and the engine actually carry, not what FluidEQ
// intends: the endpoint list is the setup helper's answer, and every profile
// is the one the state centre saved for that endpoint.
function renderConfig() {
  const endpoints = Array.isArray(engineStatus?.endpoints)
    ? engineStatus.endpoints
    : [];
  const currentGuid =
    engineStatus?.defaultDevice?.guid || snapshot.active_device_guid || '';
  els.configEndpoints.replaceChildren(
    ...endpoints.map((endpoint) => configEndpointCard(endpoint, currentGuid))
  );
  els.configEndpointsEmpty.hidden = endpoints.length > 0;

  const autoPreamp = Number(snapshot.auto_preamp_db) || 0;
  els.configNow.replaceChildren(
    configRow(
      'Output',
      snapshot.active_device_name ||
        engineStatus?.defaultDevice?.name ||
        'System default'
    ),
    configRow('Preset', snapshot.active_preset_name || 'Custom curve'),
    configRow('Bands', String(snapshot.bands?.length ?? 0)),
    configRow('Preamp', `${formatDb(Number(snapshot.preamp_total_db) || 0)} dB`),
    configRow(
      'Auto headroom',
      snapshot.auto_headroom ? `On (${formatDb(autoPreamp)} dB)` : 'Off'
    ),
    configRow(
      'EQ model',
      EQ_MODEL_NAMES[snapshot.eq_model] || snapshot.eq_model || 'Clean'
    ),
    configRow('Matched treble', snapshot.matched_treble ? 'On' : 'Off'),
    configRow(
      'Tone',
      `bass ${formatDb(Number(snapshot.tone?.bass_db) || 0)} · treble ${formatDb(
        Number(snapshot.tone?.treble_db) || 0
      )} dB`
    ),
    configRow(
      'Cuts',
      `${formatHz(Number(snapshot.tone?.low_cut_hz) || 20)} – ${formatHz(
        Number(snapshot.tone?.high_cut_hz) || 20000
      )}`
    )
  );

  const registry = snapshot.device_profiles ?? {};
  const profiles = Object.values(registry.profiles ?? {})
    .filter((profile) => profile?.guid)
    .sort((left, right) =>
      String(left.device_name || '').localeCompare(String(right.device_name || ''))
    );
  els.configProfiles.replaceChildren(
    ...profiles.map((profile) =>
      configProfileCard(profile, profile.guid === registry.active_device_guid)
    )
  );
  els.configProfilesEmpty.hidden = profiles.length > 0;

  const log = Array.isArray(engineStatus?.engineLog)
    ? engineStatus.engineLog
    : [];
  els.configLog.textContent = log.join('\n');
  els.configLog.hidden = log.length === 0;
  els.configLogEmpty.hidden = log.length > 0;
}

function renderGamePresetChoices() {
  const current = els.gamesPreset.value;
  els.gamesPreset.replaceChildren(
    ...presetNames.map((name) => {
      const option = document.createElement('option');
      option.value = name;
      option.textContent = name;
      return option;
    })
  );
  if (presetNames.includes(current)) els.gamesPreset.value = current;
}

// One row per executable. Changing a row's preset writes the mapping through
// the same command as adding it, so the file on disk is always the one the
// list shows.
function renderGames() {
  els.gamesList.replaceChildren(
    ...gameRules.map((rule) => {
      const row = document.createElement('div');
      row.className = 'game-row';

      const name = document.createElement('span');
      name.className = 'game-name';
      name.textContent = rule.executable;

      const select = document.createElement('select');
      select.className = 'preset-select';
      select.setAttribute('aria-label', `Preset for ${rule.executable}`);
      const choices = presetNames.includes(rule.preset)
        ? presetNames
        : [...presetNames, rule.preset];
      for (const choice of choices) {
        const option = document.createElement('option');
        option.value = choice;
        option.textContent = choice;
        option.selected = choice === rule.preset;
        select.appendChild(option);
      }
      select.addEventListener('change', () =>
        saveGameRule(rule.executable, select.value)
      );

      const remove = document.createElement('button');
      remove.type = 'button';
      remove.className = 'ghost';
      remove.textContent = 'Remove';
      remove.addEventListener('click', () => deleteGameRule(rule.executable));

      row.append(name, select, remove);
      return row;
    })
  );
  els.gamesEmpty.hidden = gameRules.length > 0;
  els.gamesUnavailable.hidden = gameWatchAvailable;
  renderGamePresetChoices();
}

async function saveGameRule(executable, preset) {
  if (!invoke) return false;
  try {
    gameRules = await invoke('set_game_preset', { executable, preset });
    clearMessage();
    renderGames();
    return true;
  } catch (error) {
    showMessage(`Could not map ${executable}: ${error}`);
    return false;
  }
}

async function deleteGameRule(executable) {
  if (!invoke) return;
  try {
    gameRules = await invoke('remove_game_preset', { executable });
    clearMessage();
    renderGames();
  } catch (error) {
    showMessage(`Could not remove ${executable}: ${error}`);
  }
}

// The rack is drawn from the snapshot's own chain settings, so every switch
// shows what the engine will do on the next block, not what was clicked.
function renderRack() {
  const settings = snapshot.chain_settings ?? {};
  els.rackStages.replaceChildren(
    ...rackStages.map((stage) => {
      const on = stageIsOn(settings, stage.id);
      const row = document.createElement('div');
      row.className = `rack-stage${on ? ' on' : ''}${
        stage.id === selectedRackStage ? ' selected' : ''
      }`;
      row.title = stage.description;
      row.addEventListener('click', () => {
        if (selectedRackStage === stage.id) return;
        selectedRackStage = stage.id;
        renderRack();
      });

      const badge = document.createElement('span');
      badge.className = 'rack-badge';
      badge.setAttribute('aria-hidden', 'true');
      badge.textContent = stage.badge;

      const body = document.createElement('div');
      body.className = 'rack-body';
      const name = document.createElement('div');
      name.className = 'rack-name';
      name.textContent = stage.label;
      const summary = document.createElement('p');
      summary.className = 'rack-summary';
      summary.textContent = stageSummary(stage, settings);
      body.append(name, summary);

      const toggle = document.createElement('button');
      toggle.type = 'button';
      toggle.className = 'stat-toggle';
      toggle.setAttribute('aria-pressed', String(on));
      toggle.textContent = on ? 'On' : 'Off';
      toggle.addEventListener('click', (event) => {
        event.stopPropagation();
        setRackStage(stage.id, !on);
      });

      row.append(badge, body, toggle);
      return row;
    })
  );
  renderRackEditor(settings);
}

function renderRackEditor(settings) {
  const stage =
    rackStages.find((item) => item.id === selectedRackStage) ?? rackStages[0];
  els.rackEditorTitle.textContent = stage.label;
  els.rackEditorDetail.textContent = stage.description;
  els.rackEditorControls.replaceChildren(
    ...stage.parameters.map((parameter) => {
      const value = parameter.value(settings);
      const control = document.createElement('label');
      control.className = 'control';

      const head = document.createElement('span');
      const label = document.createElement('span');
      label.textContent = parameter.label;
      const output = document.createElement('output');
      output.textContent = parameter.format(value);
      head.append(label, output);

      const input = document.createElement('input');
      input.className = 'range';
      input.type = 'range';
      input.min = String(parameter.min);
      input.max = String(parameter.max);
      input.step = String(parameter.step);
      input.value = String(value);
      const fill = (next) =>
        ((next - parameter.min) / (parameter.max - parameter.min)) * 100;
      setRangeFill(input, fill(value));
      input.addEventListener('input', () => {
        const next = Number(input.value);
        output.textContent = parameter.format(next);
        setRangeFill(input, fill(next));
      });
      input.addEventListener('change', () =>
        setRackParameter(stage.id, parameter.id, Number(input.value))
      );

      control.append(head, input);
      return control;
    }),
    ...rackEditorExtras(stage, settings)
  );
  els.rackEditor.hidden = false;
  paintDynamicsMeters();
}

function currentMasterTarget(settings) {
  const target = Number(settings?.master?.loudness_target_lufs);
  if (Number.isFinite(target)) {
    if (target <= -22) return 'cinema';
    if (target <= -17) return 'reference';
    if (target <= -15) return 'podcast';
  }
  return 'streaming';
}

function buildChipGroup(title, options, selectedId, onSelect) {
  const group = document.createElement('div');
  group.className = 'rack-extra';

  const heading = document.createElement('p');
  heading.className = 'eyebrow';
  heading.textContent = title;

  const row = document.createElement('div');
  row.className = 'room-chips';
  row.append(
    ...options.map((option) => {
      const chip = document.createElement('button');
      chip.type = 'button';
      chip.className = `room-chip${option.id === selectedId ? ' selected' : ''}`;
      chip.setAttribute('aria-pressed', String(option.id === selectedId));
      const label = document.createElement('span');
      label.className = 'chip-label';
      label.textContent = option.label;
      const detail = document.createElement('span');
      detail.className = 'chip-detail';
      detail.textContent = option.detail;
      chip.append(label, detail);
      chip.addEventListener('click', () => onSelect(option.id));
      return chip;
    })
  );

  group.append(heading, row);
  return group;
}

function rackEditorExtras(stage, settings) {
  const extras = [];
  if (stageSupportsIsolate(stage.id)) {
    const row = document.createElement('div');
    row.className = 'stat-row';
    const label = document.createElement('span');
    label.textContent = 'Difference monitor';
    const toggle = document.createElement('button');
    toggle.type = 'button';
    toggle.className = 'stat-toggle';
    const on = stageIsolate(settings, stage.id);
    toggle.textContent = on ? 'Solo' : 'Off';
    toggle.setAttribute('aria-pressed', String(on));
    toggle.addEventListener('click', () => setRackIsolate(stage.id, !on));
    row.append(label, toggle);
    extras.push(row);
  }
  if (stage.id === 'footsteps') {
    for (const [id, label] of [
      ['heavy', 'Heavy steps'],
      ['duck', 'Make room for voices'],
    ]) {
      const row = document.createElement('div');
      row.className = 'stat-row';
      const heading = document.createElement('span');
      heading.textContent = label;
      const toggle = document.createElement('button');
      toggle.type = 'button';
      toggle.className = 'stat-toggle';
      const on = Number(settings?.footsteps?.[id]) > 0;
      toggle.textContent = on ? 'On' : 'Off';
      toggle.setAttribute('aria-pressed', String(on));
      toggle.addEventListener('click', () =>
        setRackParameter(stage.id, id, on ? 0 : 1)
      );
      row.append(heading, toggle);
      extras.push(row);
    }
  }
  if (stage.id === 'dynamics') {
    extras.push(buildDynamicsMeters());
    extras.push(
      buildChipGroup(
        'Dynamics profile',
        DYNAMICS_PRESETS,
        dynamicsPresetKey(settings),
        setDynamicsPreset
      )
    );
  }
  if (stage.id === 'normalizer') {
    extras.push(
      buildChipGroup(
        'Normalizer mode',
        NORMALIZER_MODES,
        normalizerModeKey(settings),
        setNormalizerMode
      )
    );
  }
  if (stage.id === 'master') {
    extras.push(
      buildChipGroup(
        'Delivery target',
        MASTER_TARGETS,
        currentMasterTarget(settings),
        setMasterTarget
      )
    );
  }
  return extras;
}

// What Dynamics is doing right now, not what it is set to. The bars cannot
// show it: the stage changes gain rather than adding harmonics, and the
// spectrum behind it is the whole mix. So the report the engine publishes is
// drawn as one line per band and repainted from the live telemetry.
function buildDynamicsMeters() {
  const group = document.createElement('div');
  group.className = 'rack-extra';

  const heading = document.createElement('p');
  heading.className = 'eyebrow';
  heading.textContent = 'Doing now';
  group.append(heading);

  for (const [label, index] of [
    ['Low', 0],
    ['Mid', 1],
    ['High', 2],
  ]) {
    const row = document.createElement('div');
    row.className = 'stat-row';
    const name = document.createElement('span');
    name.textContent = label;
    const value = document.createElement('span');
    value.dataset.dynamicsBand = String(index);
    value.textContent = '—';
    row.append(name, value);
    group.append(row);
  }
  return group;
}

function paintDynamicsMeters() {
  const nodes = document.querySelectorAll('[data-dynamics-band]');
  if (nodes.length === 0) return;
  const report = lastDynamics;
  const active = Number(report?.active) > 0;
  for (const node of nodes) {
    if (!active) {
      node.textContent = 'idle';
      continue;
    }
    const band = Number(node.dataset.dynamicsBand);
    const level = Number(report?.level_db?.[band]);
    const gain = Number(report?.gain_db?.[band]);
    const levelText = Number.isFinite(level) ? level.toFixed(1) : '—';
    const gainText = Number.isFinite(gain)
      ? `${gain >= 0 ? '+' : ''}${gain.toFixed(1)}`
      : '0.0';
    node.textContent = `${levelText} dB · ${gainText} dB`;
  }
}

async function setRackIsolate(stage, enabled) {
  if (!invoke) return;
  try {
    snapshot = await invoke('set_stage_isolate', { stage, enabled });
    clearMessage();
    renderAll();
  } catch (error) {
    showMessage(`Could not change the ${stage} monitor: ${error}`);
  }
}

async function setNormalizerMode(mode) {
  if (!invoke) return;
  try {
    snapshot = await invoke('set_normalizer_mode', { mode });
    clearMessage();
    renderAll();
  } catch (error) {
    showMessage(`Could not change the normalizer mode: ${error}`);
  }
}

async function setMasterTarget(target) {
  if (!invoke) return;
  try {
    snapshot = await invoke('set_master_target', { target });
    clearMessage();
    renderAll();
  } catch (error) {
    showMessage(`Could not apply the delivery target: ${error}`);
  }
}

async function setDynamicsPreset(preset) {
  if (!invoke) return;
  try {
    snapshot = await invoke('set_dynamics_preset', { preset });
    clearMessage();
    renderAll();
  } catch (error) {
    showMessage(`Could not apply the dynamics profile: ${error}`);
  }
}

async function runRoomCommand(command, failure) {
  if (!invoke) return;
  try {
    snapshot = await invoke(command);
    clearMessage();
    renderAll();
  } catch (error) {
    showMessage(`${failure}: ${error}`);
  }
}

async function commitRoomSpeaker(parameter, value) {
  if (!invoke) return;
  try {
    snapshot = await invoke('set_room_speaker', {
      speaker: selectedRoomSpeaker,
      parameter,
      value,
    });
    clearMessage();
    renderAll();
  } catch (error) {
    showMessage(`Could not move the speaker: ${error}`);
  }
}

async function setRackParameter(stage, parameter, value) {
  if (!invoke) return;
  try {
    snapshot = await invoke('set_stage_parameter', { stage, parameter, value });
    clearMessage();
    renderAll();
  } catch (error) {
    showMessage(`Could not change ${stage} ${parameter}: ${error}`);
    renderAll();
  }
}

async function setRackStage(stage, enabled) {
  if (!invoke) return;
  try {
    snapshot = await invoke('set_stage_enabled', { stage, enabled });
    clearMessage();
    renderAll();
  } catch (error) {
    showMessage(`Could not switch the ${stage} stage: ${error}`);
  }
}

// ---------------------------------------------------------------------------
// Room view
// ---------------------------------------------------------------------------

// Space is stored in dB, but -60 to 0 is not heard evenly: the top 24 dB carry
// the room and the rest is effectively none. The dial is a share of that
// audible range, as upstream's `roomView.ts` maps it.
const SPACE_OFF_DB = -60;
const SPACE_FLOOR_DB = -36;

function spaceOfDb(db) {
  if (db <= SPACE_OFF_DB) return 0;
  const percent = Math.round(
    ((db - SPACE_FLOOR_DB) / (0 - SPACE_FLOOR_DB)) * 100
  );
  return Math.min(100, Math.max(1, percent));
}

function dbOfSpace(percent) {
  const whole = Math.min(100, Math.max(0, Math.round(percent)));
  if (whole === 0) return SPACE_OFF_DB;
  const db = SPACE_FLOOR_DB + (whole / 100) * (0 - SPACE_FLOOR_DB);
  return Math.round(db * 100) / 100;
}

function renderRoom() {
  const settings = snapshot.chain_settings?.room ?? {};
  const enabled = Boolean(settings.enabled);
  els.roomPower.textContent = enabled ? 'On' : 'Off';
  els.roomPower.setAttribute('aria-pressed', String(enabled));

  // Space is the stored level of the walls, -60 dB for none; the dial is a
  // share of the audible range so the ear moves it evenly (upstream
  // `roomView.ts`).
  const rendererVersion = numberOr(settings.renderer_version, 1);
  const modern = rendererVersion === 2;
  els.roomRendererVal.textContent = modern ? 'Modern' : 'Classic';
  els.roomUpgrade.hidden = modern;
  els.roomRendererNote.textContent = modern
    ? 'Space and Ambience belong to the modern renderer.'
    : 'This classic room has no Space or Ambience until it moves to the modern renderer.';
  for (const control of [
    els.roomSpace,
    els.roomDamping,
    els.roomAmbience,
    els.roomDecay,
  ]) {
    control.disabled = !modern;
  }

  renderRoomPresets(numberOr(settings.preset, -1));

  const currentHead = numberOr(settings.head, 1);
  els.roomHeadChips.replaceChildren(
    ...KEMAR_HEADS.map((h) => {
      const chip = document.createElement('button');
      chip.type = 'button';
      chip.className = `room-chip${h.id === currentHead ? ' selected' : ''}`;
      chip.setAttribute('aria-pressed', String(h.id === currentHead));
      const lbl = document.createElement('span');
      lbl.className = 'chip-label';
      lbl.textContent = h.label;
      const dtl = document.createElement('span');
      dtl.className = 'chip-detail';
      dtl.textContent = h.detail;
      chip.append(lbl, dtl);
      chip.addEventListener('click', () => applyRoomHead(h.id));
      return chip;
    })
  );

  const speaker = Math.min(
    Math.max(Math.trunc(numberOr(selectedRoomSpeaker, 0)), 0),
    ROOM_SUBWOOFER
  );
  selectedRoomSpeaker = speaker;
  const isSub = speaker === ROOM_SUBWOOFER;
  const angles = Array.isArray(settings.angle_deg) ? settings.angle_deg : [];
  const levels = Array.isArray(settings.level_db) ? settings.level_db : [];
  const ownDistances = Array.isArray(settings.speaker_distance_m)
    ? settings.speaker_distance_m
    : [];
  const mutes = Array.isArray(settings.mute) ? settings.mute : [];
  els.roomSpeakerChips.replaceChildren(
    ...ROOM_SPEAKERS.map(([label, detail], index) => {
      const chip = document.createElement('button');
      chip.type = 'button';
      chip.className = `room-chip${index === speaker ? ' selected' : ''}`;
      chip.setAttribute('aria-pressed', String(index === speaker));
      const lbl = document.createElement('span');
      lbl.className = 'chip-label';
      lbl.textContent = label;
      const dtl = document.createElement('span');
      dtl.className = 'chip-detail';
      dtl.textContent =
        index === ROOM_SUBWOOFER ? detail : `${numberOr(angles[index], 0)}°`;
      chip.append(lbl, dtl);
      chip.addEventListener('click', () => {
        selectedRoomSpeaker = index;
        renderRoom();
      });
      return chip;
    })
  );

  const muted = Boolean(mutes[speaker]);
  els.roomSpeakerMute.textContent = muted ? 'Muted' : 'Off';
  els.roomSpeakerMute.setAttribute('aria-pressed', String(muted));

  const angle = numberOr(angles[speaker], 0);
  setRangeValue(els.roomSpeakerAngle, angle);
  setRangeFill(els.roomSpeakerAngle, ((angle + 180) / 360) * 100);
  els.roomSpeakerAngleVal.textContent = `${Math.round(angle)}°`;
  els.roomSpeakerAngle.disabled = isSub;

  const ownDistance = numberOr(ownDistances[speaker], 0);
  const distance = ownDistance > 0 ? ownDistance : numberOr(settings.distance_m, 1);
  setRangeValue(els.roomSpeakerDist, distance);
  setRangeFill(els.roomSpeakerDist, ((distance - 0.5) / 5.5) * 100);
  els.roomSpeakerDistVal.textContent = `${distance.toFixed(1)} m`;
  els.roomSpeakerDist.disabled = isSub;

  const level = isSub
    ? numberOr(settings.sub_db, 0)
    : numberOr(levels[speaker], 0);
  const levelMin = isSub ? -12 : -24;
  els.roomSpeakerLevel.min = String(levelMin);
  setRangeValue(els.roomSpeakerLevel, level);
  setRangeFill(els.roomSpeakerLevel, ((level - levelMin) / (12 - levelMin)) * 100);
  els.roomSpeakerLevelVal.textContent = `${formatDb(level)} dB`;

  const size = numberOr(settings.size_m, 6);
  setRangeValue(els.roomSize, size);
  setRangeFill(els.roomSize, ((size - 2) / 10) * 100);
  els.roomSizeVal.textContent = `${size.toFixed(1)} m`;

  const dist = numberOr(settings.distance_m, 1);
  setRangeValue(els.roomDist, dist);
  setRangeFill(els.roomDist, ((dist - 0.5) / 5.5) * 100);
  els.roomDistVal.textContent = `${dist.toFixed(1)} m`;

  const walls = numberOr(settings.walls, 0.45);
  setRangeValue(els.roomWalls, walls);
  setRangeFill(els.roomWalls, walls * 100);
  els.roomWallsVal.textContent = `${Math.round(walls * 100)}%`;

  const ambience = numberOr(settings.ambience_mix, 0.2);
  setRangeValue(els.roomAmbience, ambience);
  setRangeFill(els.roomAmbience, ambience * 100);
  els.roomAmbienceVal.textContent = `${Math.round(ambience * 100)}%`;

  const decay = numberOr(settings.ambience_decay_s, 0.8);
  setRangeValue(els.roomDecay, decay);
  setRangeFill(els.roomDecay, ((decay - 0.1) / 1.7) * 100);
  els.roomDecayVal.textContent = `${decay.toFixed(2)} s`;

  const space = spaceOfDb(numberOr(settings.early_reflection_db, 0));
  setRangeValue(els.roomSpace, space);
  setRangeFill(els.roomSpace, space);
  els.roomSpaceVal.textContent = `${Math.round(space)}%`;

  const damping = numberOr(settings.ambience_damping_hz, 6000);
  setRangeValue(els.roomDamping, damping);
  setRangeFill(els.roomDamping, ((damping - 1000) / 11000) * 100);
  els.roomDampingVal.textContent = `${Math.round(damping)} Hz`;

  const upmix = Boolean(settings.music_upmix);
  els.roomUpmix.textContent = upmix ? 'On' : 'Off';
  els.roomUpmix.setAttribute('aria-pressed', String(upmix));

  const upmixAmount = numberOr(settings.upmix_amount, 0.6);
  setRangeValue(els.roomUpmixAmount, upmixAmount);
  setRangeFill(els.roomUpmixAmount, upmixAmount * 100);
  els.roomUpmixAmountVal.textContent = `${Math.round(upmixAmount * 100)}%`;
  els.roomUpmixAmount.disabled = !upmix;

  const bassMgmt = Boolean(settings.bass_management);
  els.roomBassMgmt.textContent = bassMgmt ? 'On' : 'Off';
  els.roomBassMgmt.setAttribute('aria-pressed', String(bassMgmt));

  const crossover = numberOr(settings.crossover_hz, 80);
  setRangeValue(els.roomCrossover, crossover);
  setRangeFill(els.roomCrossover, ((crossover - 40) / 120) * 100);
  els.roomCrossoverVal.textContent = `${Math.round(crossover)} Hz`;

  const centre = numberOr(settings.centre_db, 0);
  setRangeValue(els.roomCentre, centre);
  setRangeFill(els.roomCentre, ((centre + 12) / 18) * 100);
  els.roomCentreVal.textContent = `${formatDb(centre)} dB`;

  const sub = numberOr(settings.sub_db, 0);
  setRangeValue(els.roomSub, sub);
  setRangeFill(els.roomSub, ((sub + 12) / 18) * 100);
  els.roomSubVal.textContent = `${formatDb(sub)} dB`;
}

// Featured rooms first, then the classic ones by group — the order the
// upstream Room card draws them in.
const ROOM_SECTIONS = [
  ['featured', 'Featured'],
  ['basic', 'Classic · Basic'],
  ['playback', 'Classic · Playback'],
  ['character', 'Classic · Character'],
];

function renderRoomPresets(currentPreset) {
  const sections = new Map();
  for (const preset of roomCatalog) {
    const key = preset.collection === 'featured' ? 'featured' : preset.group;
    if (!sections.has(key)) sections.set(key, []);
    sections.get(key).push(preset);
  }

  const children = [];
  for (const [key, title] of ROOM_SECTIONS) {
    const presets = sections.get(key);
    if (!presets?.length) continue;
    const heading = document.createElement('p');
    heading.className = 'eyebrow';
    heading.textContent = title;
    const row = document.createElement('div');
    row.className = 'room-chips';
    row.append(
      ...presets.map((preset) => {
        const selected = preset.index === currentPreset;
        const chip = document.createElement('button');
        chip.type = 'button';
        chip.className = `room-chip${selected ? ' selected' : ''}`;
        chip.setAttribute('aria-pressed', String(selected));
        const label = document.createElement('span');
        label.className = 'chip-label';
        label.textContent = preset.label;
        const detail = document.createElement('span');
        detail.className = 'chip-detail';
        detail.textContent = preset.detail;
        chip.append(label, detail);
        chip.addEventListener('click', () => applyRoomPreset(preset.index));
        return chip;
      })
    );
    children.push(heading, row);
  }
  els.roomPresetChips.replaceChildren(...children);
}

async function refreshRoomCatalog() {
  if (!invoke) return;
  try {
    roomCatalog = await invoke('list_room_presets');
  } catch (error) {
    roomCatalog = [];
    showMessage(`Room presets: ${error}`);
    return;
  }
  renderRoom();
}

async function applyRoomPreset(preset) {
  if (!invoke) return;
  try {
    snapshot = await invoke('set_room_preset', { preset });
    clearMessage();
    renderAll();
  } catch (error) {
    showMessage(`Could not apply room preset: ${error}`);
  }
}

async function applyRoomHead(head) {
  if (!invoke) return;
  try {
    snapshot = await invoke('set_room_head', { head });
    clearMessage();
    renderAll();
  } catch (error) {
    showMessage(`Could not apply KEMAR head: ${error}`);
  }
}

async function toggleRoomPower() {
  const on = Boolean(snapshot.chain_settings?.room?.enabled);
  await setRackStage('room', !on);
}

async function toggleRoomBassManagement() {
  if (!invoke) return;
  const on = Boolean(snapshot.chain_settings?.room?.bass_management);
  try {
    snapshot = await invoke('set_room_bass_management', { enabled: !on });
    clearMessage();
    renderAll();
  } catch (error) {
    showMessage(`Could not change bass management: ${error}`);
  }
}

async function refreshGameRules() {
  if (!invoke) return renderGames();
  try {
    gameRules = await invoke('list_game_presets');
  } catch {
    gameRules = [];
  }
  try {
    gameWatchAvailable = await invoke('game_watch_available');
  } catch {
    gameWatchAvailable = false;
  }
  renderGames();
}

// One page at a time: the rail says which, the panels carry the same id, and
// the header follows the page rather than repeating it in each view.
function showView(id) {
  const view = viewFor(id) ?? equalizer;
  if (activeView === view.id) return;
  activeView = view.id;
  for (const button of document.querySelectorAll('.rail-nav .nav-btn[data-view]')) {
    const current = button.dataset.view === view.id;
    button.classList.toggle('active', current);
    if (current) button.setAttribute('aria-current', 'page');
    else button.removeAttribute('aria-current');
  }
  for (const panel of document.querySelectorAll('[data-view-panel]')) {
    panel.hidden = panel.dataset.viewPanel !== view.id;
  }
  els.pageTitle.textContent = view.title;
  els.pageEyebrow.textContent = view.eyebrow;
  const equalizerView = view.id === 'equalizer';
  els.presetBar.hidden = !equalizerView;
  if (!equalizerView) els.presetForm.hidden = true;
  if (equalizerView) scheduleDraw();
  if (view.id === 'config') renderConfig();
  if (view.id === 'rack') renderRack();
  if (view.id === 'room') renderRoom();
  if (view.id === 'games') renderGames();
  if (view.id === 'settings') renderSettings();
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

async function commitBandConfig() {
  const band = selectedBand();
  if (!band) return;
  if (!invoke) {
    renderAll();
    return;
  }
  try {
    snapshot = await invoke('set_band_config', {
      bandId: band.id,
      enabled: band.enabled !== false,
      filterType: band.filter_type || 'PK',
      ear: Number(band.ear) || 0,
    });
    clearMessage();
  } catch (error) {
    showMessage(`Could not save the band shape: ${error}`);
  }
  renderAll();
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
    hotkeyFailures = Array.isArray(failures) ? failures : [];
    if (hotkeyFailures.length > 0) {
      showMessage(`Global hotkeys unavailable: ${hotkeyFailures.join('; ')}`);
    }
  } catch {
    // An older shell without the command has no hotkey report to show.
  }
  renderSettings();
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

async function commitToneAndVoicing() {
  const tone = ensureTone();
  if (!invoke) return renderAll();
  try {
    snapshot = await invoke('set_tone_and_voicing', {
      bassDb: Number(tone.bass_db) || 0,
      trebleDb: Number(tone.treble_db) || 0,
      lowCutHz: Number(tone.low_cut_hz) || 20,
      highCutHz: Number(tone.high_cut_hz) || 20000,
      eqModel: normalizeEqModelKey(snapshot.eq_model),
      eqModelAmount: Number.isFinite(Number(snapshot.eq_model_amount))
        ? Number(snapshot.eq_model_amount)
        : 1,
      matchedTreble: snapshot.matched_treble !== false,
    });
    clearMessage();
  } catch (error) {
    showMessage(`Could not save tone settings: ${error}`);
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

// A chain bundle is the whole rack, Room and voicing in one file; it is not a
// preset in the picker, so importing one names the curve and leaves the saved
// list alone.
els.chainExport.addEventListener('click', async () => {
  if (!invoke) return;
  try {
    const path = await invoke('export_chain_bundle');
    clearMessage();
    showMessage(`Chain saved to ${path}`, 'ok');
  } catch (error) {
    showMessage(`Could not export the chain: ${error}`);
  }
});

els.chainImport.addEventListener('click', () => els.chainFile.click());

els.chainFile.addEventListener('change', async () => {
  const file = els.chainFile.files?.[0];
  if (!invoke || !file) {
    els.chainFile.value = '';
    return;
  }
  try {
    const text = await file.text();
    snapshot = await invoke('import_chain_bundle', { text });
    clearMessage();
    showMessage(`Imported "${snapshot.active_preset_name}"`, 'ok');
    renderAll();
    refreshPresets();
  } catch (error) {
    showMessage(`Could not import ${file.name}: ${error}`);
  } finally {
    els.chainFile.value = '';
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

els.bandFilter.addEventListener('change', () => {
  const band = selectedBand();
  if (!band) return;
  band.filter_type = els.bandFilter.value;
  els.bandType.textContent = band.filter_type;
  scheduleDraw();
  commitBandConfig();
});

els.bandEar.addEventListener('change', () => {
  const band = selectedBand();
  if (!band) return;
  band.ear = Number(els.bandEar.value) || 0;
  scheduleDraw();
  commitBandConfig();
});

els.bandEnable.addEventListener('click', () => {
  const band = selectedBand();
  if (!band) return;
  band.enabled = band.enabled === false;
  renderBands();
  renderInspector();
  scheduleDraw();
  commitBandConfig();
});

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

els.toneBass.addEventListener('input', () => {
  const tone = ensureTone();
  tone.bass_db = Number(els.toneBass.value);
  setRangeFill(els.toneBass, ((tone.bass_db + 12) / 24) * 100);
  els.toneBassValue.textContent = `${formatDb(tone.bass_db)} dB`;
  scheduleDraw();
});
els.toneBass.addEventListener('change', commitToneAndVoicing);

els.toneTreble.addEventListener('input', () => {
  const tone = ensureTone();
  tone.treble_db = Number(els.toneTreble.value);
  setRangeFill(els.toneTreble, ((tone.treble_db + 12) / 24) * 100);
  els.toneTrebleValue.textContent = `${formatDb(tone.treble_db)} dB`;
  scheduleDraw();
});
els.toneTreble.addEventListener('change', commitToneAndVoicing);

els.toneLowCut.addEventListener('input', () => {
  const tone = ensureTone();
  tone.low_cut_hz = Number(els.toneLowCut.value);
  setRangeFill(els.toneLowCut, ((tone.low_cut_hz - 20) / 230) * 100);
  els.toneLowCutValue.textContent = formatHz(tone.low_cut_hz);
  scheduleDraw();
});
els.toneLowCut.addEventListener('change', commitToneAndVoicing);

els.toneHighCut.addEventListener('input', () => {
  const tone = ensureTone();
  tone.high_cut_hz = Number(els.toneHighCut.value);
  setRangeFill(els.toneHighCut, ((tone.high_cut_hz - 4000) / 16000) * 100);
  els.toneHighCutValue.textContent = formatHz(tone.high_cut_hz);
  scheduleDraw();
});
els.toneHighCut.addEventListener('change', commitToneAndVoicing);

els.eqModel.addEventListener('change', () => {
  snapshot.eq_model = els.eqModel.value;
  renderToneAndVoicing();
  scheduleDraw();
  commitToneAndVoicing();
});

els.eqModelAmount.addEventListener('input', () => {
  snapshot.eq_model_amount = Number(els.eqModelAmount.value);
  setRangeFill(els.eqModelAmount, snapshot.eq_model_amount * 100);
  els.eqModelAmountValue.textContent = `${Math.round(snapshot.eq_model_amount * 100)}%`;
  scheduleDraw();
});
els.eqModelAmount.addEventListener('change', commitToneAndVoicing);

els.matchedTreble.addEventListener('click', () => {
  snapshot.matched_treble = snapshot.matched_treble === false;
  renderToneAndVoicing();
  scheduleDraw();
  commitToneAndVoicing();
});

els.resetTone.addEventListener('click', () => {
  const tone = ensureTone();
  tone.bass_db = 0;
  tone.treble_db = 0;
  tone.low_cut_hz = 20;
  tone.high_cut_hz = 20000;
  renderToneAndVoicing();
  scheduleDraw();
  commitToneAndVoicing();
});

els.opraVendor.addEventListener('change', () => selectOpraVendor(els.opraVendor.value));
els.opraModel.addEventListener('change', () => applyOpraModel(els.opraModel.value));
els.opraClear.addEventListener('click', clearOpraSelection);

els.apoImport.addEventListener('click', () => els.apoFile.click());
els.apoFile.addEventListener('change', importApoFile);
els.apoExport.addEventListener('click', exportApoFile);

els.roomPower.addEventListener('click', toggleRoomPower);
els.roomBassMgmt.addEventListener('click', toggleRoomBassManagement);

els.roomUpgrade.addEventListener('click', () =>
  runRoomCommand('room_migrate_renderer', 'Could not move the room to the modern renderer')
);
els.roomReset.addEventListener('click', () =>
  runRoomCommand('room_reset', 'Could not reset the room')
);

els.roomSpace.addEventListener('input', () => {
  const percent = Number(els.roomSpace.value);
  setRangeFill(els.roomSpace, percent);
  els.roomSpaceVal.textContent = `${Math.round(percent)}%`;
});
els.roomSpace.addEventListener('change', () =>
  setRackParameter('room', 'early_reflection_db', dbOfSpace(Number(els.roomSpace.value)))
);

els.roomDamping.addEventListener('input', () => {
  const damping = Number(els.roomDamping.value);
  setRangeFill(els.roomDamping, ((damping - 1000) / 11000) * 100);
  els.roomDampingVal.textContent = `${Math.round(damping)} Hz`;
});
els.roomDamping.addEventListener('change', () =>
  setRackParameter('room', 'ambience_damping_hz', Number(els.roomDamping.value))
);

els.roomUpmix.addEventListener('click', () =>
  setRackParameter('room', 'music_upmix', Boolean(snapshot.chain_settings?.room?.music_upmix) ? 0 : 1)
);

els.roomUpmixAmount.addEventListener('input', () => {
  const amount = Number(els.roomUpmixAmount.value);
  setRangeFill(els.roomUpmixAmount, amount * 100);
  els.roomUpmixAmountVal.textContent = `${Math.round(amount * 100)}%`;
});
els.roomUpmixAmount.addEventListener('change', () =>
  setRackParameter('room', 'upmix_amount', Number(els.roomUpmixAmount.value))
);

els.roomSize.addEventListener('input', () => {
  const size = Number(els.roomSize.value);
  setRangeFill(els.roomSize, ((size - 2) / 10) * 100);
  els.roomSizeVal.textContent = `${size.toFixed(1)} m`;
});
els.roomSize.addEventListener('change', () =>
  setRackParameter('room', 'size_m', Number(els.roomSize.value))
);

els.roomDist.addEventListener('input', () => {
  const dist = Number(els.roomDist.value);
  setRangeFill(els.roomDist, ((dist - 0.5) / 5.5) * 100);
  els.roomDistVal.textContent = `${dist.toFixed(1)} m`;
});
els.roomDist.addEventListener('change', () =>
  setRackParameter('room', 'distance_m', Number(els.roomDist.value))
);

els.roomWalls.addEventListener('input', () => {
  const walls = Number(els.roomWalls.value);
  setRangeFill(els.roomWalls, walls * 100);
  els.roomWallsVal.textContent = `${Math.round(walls * 100)}%`;
});
els.roomWalls.addEventListener('change', () =>
  setRackParameter('room', 'walls', Number(els.roomWalls.value))
);

els.roomAmbience.addEventListener('input', () => {
  const ambience = Number(els.roomAmbience.value);
  setRangeFill(els.roomAmbience, ambience * 100);
  els.roomAmbienceVal.textContent = `${Math.round(ambience * 100)}%`;
});
els.roomAmbience.addEventListener('change', () =>
  setRackParameter('room', 'ambience_mix', Number(els.roomAmbience.value))
);

els.roomDecay.addEventListener('input', () => {
  const decay = Number(els.roomDecay.value);
  setRangeFill(els.roomDecay, ((decay - 0.1) / 1.7) * 100);
  els.roomDecayVal.textContent = `${decay.toFixed(2)} s`;
});
els.roomDecay.addEventListener('change', () =>
  setRackParameter('room', 'ambience_decay_s', Number(els.roomDecay.value))
);

els.roomSpeakerAngle.addEventListener('input', () => {
  const angle = Number(els.roomSpeakerAngle.value);
  setRangeFill(els.roomSpeakerAngle, ((angle + 180) / 360) * 100);
  els.roomSpeakerAngleVal.textContent = `${Math.round(angle)}°`;
});
els.roomSpeakerAngle.addEventListener('change', () =>
  commitRoomSpeaker('angle_deg', Number(els.roomSpeakerAngle.value))
);

els.roomSpeakerDist.addEventListener('input', () => {
  const distance = Number(els.roomSpeakerDist.value);
  setRangeFill(els.roomSpeakerDist, ((distance - 0.5) / 5.5) * 100);
  els.roomSpeakerDistVal.textContent = `${distance.toFixed(1)} m`;
});
els.roomSpeakerDist.addEventListener('change', () =>
  commitRoomSpeaker('distance_m', Number(els.roomSpeakerDist.value))
);

els.roomSpeakerLevel.addEventListener('input', () => {
  const level = Number(els.roomSpeakerLevel.value);
  const min = Number(els.roomSpeakerLevel.min);
  setRangeFill(els.roomSpeakerLevel, ((level - min) / (12 - min)) * 100);
  els.roomSpeakerLevelVal.textContent = `${formatDb(level)} dB`;
});
els.roomSpeakerLevel.addEventListener('change', () =>
  commitRoomSpeaker('level_db', Number(els.roomSpeakerLevel.value))
);

els.roomSpeakerMute.addEventListener('click', () => {
  const mutes = snapshot.chain_settings?.room?.mute ?? [];
  commitRoomSpeaker('mute', mutes[selectedRoomSpeaker] ? 0 : 1);
});

els.roomCrossover.addEventListener('input', () => {
  const crossover = Number(els.roomCrossover.value);
  setRangeFill(els.roomCrossover, ((crossover - 40) / 120) * 100);
  els.roomCrossoverVal.textContent = `${Math.round(crossover)} Hz`;
});
els.roomCrossover.addEventListener('change', () =>
  setRackParameter('room', 'crossover_hz', Number(els.roomCrossover.value))
);

els.roomCentre.addEventListener('input', () => {
  const centre = Number(els.roomCentre.value);
  setRangeFill(els.roomCentre, ((centre + 12) / 18) * 100);
  els.roomCentreVal.textContent = `${formatDb(centre)} dB`;
});
els.roomCentre.addEventListener('change', () =>
  setRackParameter('room', 'centre_db', Number(els.roomCentre.value))
);

els.roomSub.addEventListener('input', () => {
  const sub = Number(els.roomSub.value);
  setRangeFill(els.roomSub, ((sub + 12) / 18) * 100);
  els.roomSubVal.textContent = `${formatDb(sub)} dB`;
});
els.roomSub.addEventListener('change', () =>
  setRackParameter('room', 'sub_db', Number(els.roomSub.value))
);

for (const button of document.querySelectorAll('.rail-nav .nav-btn[data-view]')) {
  button.addEventListener('click', () => showView(button.dataset.view));
}

els.gamesForm.addEventListener('submit', async (event) => {
  event.preventDefault();
  const executable = els.gamesExecutable.value.trim();
  if (!executable) {
    showMessage('Name the game executable');
    return;
  }
  if (!els.gamesPreset.value) {
    showMessage('Save a preset before mapping a game');
    return;
  }
  if (await saveGameRule(executable, els.gamesPreset.value)) {
    els.gamesExecutable.value = '';
  }
});

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
  if (!invoke || engineInstallInFlight || engineEndpointInFlight) return;
  refreshEngineStatus();
  syncOutputVolume();
  refreshEngineHost();
  refreshStartup();
});

// Windows can drop, bypass or re-create an output while the window sits open,
// so the engine is read again on a slow beat. Quiet: a health check is not a
// message.
window.setInterval(() => {
  if (document.hidden || !invoke || engineInstallInFlight || engineEndpointInFlight) return;
  refreshEngineStatus({ quiet: true });
}, 20_000);

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

// What the one button does next. A restart is enough only when every other
// fact is healthy and an attached output simply never loaded; anything else
// rewrites the registration, which is also how a stale DLL is updated and a
// Windows bypass is cleared.
function engineAction(status) {
  const endpoints = Array.isArray(status?.endpoints) ? status.endpoints : [];
  const attached = endpoints.filter((endpoint) => endpoint.attached).length;
  const bypassed = endpoints.filter(
    (endpoint) => endpoint.attached && endpoint.tpBypassed
  ).length;
  const unloaded = endpoints.filter(
    (endpoint) => endpoint.attached && endpoint.loaded === false
  ).length;
  const installed = Boolean(status?.installed);
  const stale =
    installed &&
    Boolean(status?.dllVersion) &&
    Boolean(status?.bundledVersion) &&
    status.dllVersion !== status.bundledVersion;
  const bridgeError =
    typeof status?.bridgeError === 'string' && status.bridgeError.length > 0;
  if (!installed) return 'install';
  if (engineBlockers(status).length > 0 || bridgeError || bypassed > 0 || stale) {
    return 'install';
  }
  if (endpoints.length > attached) return 'install';
  if (unloaded > 0) return 'restart';
  return null;
}

function renderEngineStatus(status, quiet = false) {
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

  const device = status?.defaultDevice;
  if (device?.name) {
    els.deviceName.textContent = device.name;
    els.settingsDevice.textContent = device.name;
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
  const action = engineAction(status);
  els.engineInstall.hidden = action === null;
  els.engineInstall.textContent =
    action === 'restart'
      ? 'Restart Windows audio'
      : installed
        ? blocked || bridgeError
          ? 'Repair engine'
          : bypassed
            ? 'Fix engine activation'
            : stale
              ? 'Update engine'
              : 'Attach all outputs'
        : 'Enable system audio';
  if (!quiet) {
    if (unloaded > 0) {
      showMessage(`Engine never loaded: ${advice}`);
    } else {
      clearMessageStartingWith('Engine never loaded:');
      clearMessageStartingWith('Engine status:');
    }
  }
  if (activeView === 'config') renderConfig();
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

async function refreshEngineStatus({ quiet = false } = {}) {
  try {
    const status = await invoke('engine_status');
    renderEngineStatus(status, quiet);
    await followDefaultDevice(status);
  } catch (error) {
    els.engineActivation.hidden = false;
    els.engineActivation.classList.remove('active');
    els.engineText.textContent = 'System engine unavailable';
    els.engineInstall.hidden = true;
    if (!quiet) showMessage(`Engine status: ${error}`);
  }
}

// Attaching one output is the narrow move: it rewrites that output's effect
// list and nothing else, so a machine with several endpoints does not need a
// full install to add the one that was missing. Windows builds the APO on the
// output's next stream, which is why the advice after a successful attach is
// to wait or restart rather than to install again.
async function toggleEndpointAttachment(endpoint) {
  if (!invoke || !endpoint?.guid || engineEndpointInFlight) return;
  const attach = !endpoint.attached;
  const name = endpoint.name || 'the output';
  engineEndpointInFlight = true;
  renderConfig();
  try {
    const status = await invoke(attach ? 'engine_attach' : 'engine_detach', {
      guids: [endpoint.guid],
    });
    renderEngineStatus(status, true);
    showMessage(
      attach
        ? `Attached ${name}; restart Windows audio if it never loads`
        : `Detached ${name}`,
      'ok'
    );
  } catch (error) {
    showMessage(`Could not ${attach ? 'attach' : 'detach'} ${name}: ${error}`);
  } finally {
    engineEndpointInFlight = false;
    renderConfig();
  }
}

els.configSettle.addEventListener('click', async () => {
  if (!invoke) return;
  els.configSettle.disabled = true;
  els.configSettle.textContent = 'Waiting…';
  try {
    renderEngineStatus(await invoke('engine_settle'), true);
    showMessage('Windows audio has settled', 'ok');
  } catch (error) {
    showMessage(`Windows audio did not settle: ${error}`);
  } finally {
    els.configSettle.disabled = false;
    els.configSettle.textContent = 'Wait for audio';
  }
});

els.engineInstall.addEventListener('click', async () => {
  if (engineInstallInFlight) return;
  const restart = engineAction(engineStatus) === 'restart';
  engineInstallInFlight = true;
  els.engineInstall.disabled = true;
  els.engineInstall.textContent = restart ? 'Restarting…' : 'Installing…';
  try {
    renderEngineStatus(
      await invoke(restart ? 'restart_audio' : 'install_engine')
    );
    showMessage(
      restart
        ? 'Windows audio restarted — the outputs rebuild the engine'
        : 'System engine enabled — Windows audio restarted',
      'ok'
    );
  } catch (error) {
    showMessage(
      restart
        ? `Windows audio was not restarted: ${error}`
        : `Engine was not enabled: ${error}`
    );
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
    refreshMedia();
    refreshPresets();
    refreshRoomCatalog();
    refreshStartup();
    refreshHotkeys();
    refreshGameRules();
    refreshOpraVendors();
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

function mediaClock(ms) {
  if (!Number.isFinite(Number(ms)) || Number(ms) < 0) return '0:00';
  const total = Math.floor(Number(ms) / 1000);
  const minutes = Math.floor(total / 60);
  const seconds = String(total % 60).padStart(2, '0');
  return `${minutes}:${seconds}`;
}

function renderMedia(media) {
  const reading = media?.reading ?? null;
  if (!reading || !reading.title) {
    els.mediaBar.hidden = true;
    return;
  }
  els.mediaBar.hidden = false;
  els.mediaTitle.textContent = reading.title;
  els.mediaArtist.textContent = [reading.artist, reading.app]
    .filter(Boolean)
    .join(' · ');
  els.mediaPlay.textContent = reading.isPlaying ? 'Pause' : 'Play';
  els.mediaPrevious.disabled = !reading.canPrevious;
  els.mediaNext.disabled = !reading.canNext;

  const cover = media?.cover;
  if (cover?.id && cover.id === reading.coverId && cover.data) {
    const source = `data:${cover.type};base64,${cover.data}`;
    if (els.mediaCover.src !== source) els.mediaCover.src = source;
    els.mediaCover.hidden = false;
  } else {
    els.mediaCover.hidden = true;
  }

  const position = reading.positionMs;
  const duration = reading.durationMs;
  els.mediaTime.textContent = Number.isFinite(Number(duration)) && duration > 0
    ? `${mediaClock(position)} / ${mediaClock(duration)}`
    : mediaClock(position);
}

async function refreshMedia() {
  if (!invoke) return;
  try {
    renderMedia(await invoke('media_status'));
  } catch {
    renderMedia(null);
  }
}

async function sendMediaCommand(command) {
  if (!invoke) return;
  try {
    await invoke('media_command', { command });
    clearMessage();
  } catch (error) {
    showMessage(`Could not reach the player: ${error}`);
  }
}

els.mediaPlay.addEventListener('click', () => {
  const playing = Boolean(els.mediaPlay.textContent === 'Pause');
  sendMediaCommand(playing ? 'pause' : 'play');
});
els.mediaPrevious.addEventListener('click', () => sendMediaCommand('previous'));
els.mediaNext.addEventListener('click', () => sendMediaCommand('next'));

TAURI?.event?.listen?.('media', (event) => renderMedia(event.payload));

// Live spectrum and multi-stage telemetry. The Rust bridge only emits while
// the window is focused and sound is actually playing, so this listener never
// keeps the canvas animating over silence or in the tray.
TAURI?.event?.listen?.('spectrum', (event) => {
  const packet = event.payload ?? {};
  const bins = Array.isArray(packet.bins_db) ? packet.bins_db : [];
  const wasActive = spectrumBins !== null;
  spectrumBins = !packet.sleeping && bins.length > 0 ? bins : null;
  updateMeters(packet);
  if (activeView === 'equalizer' && (spectrumBins !== null || wasActive)) {
    scheduleDraw();
  }
});

// The volume hotkeys change Windows directly; the slider follows the same
// number rather than waiting for the next focus to notice.
TAURI?.event?.listen?.('volume', (event) => setVolume(event.payload));

boot();
