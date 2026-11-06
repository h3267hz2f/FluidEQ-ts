import React, { useMemo, useRef, useState } from 'react';
import {
  buildToneBands,
  computeAutoPreampDb,
  computeResidencyProfile,
  createLogFrequencyGrid,
  EqBand,
  EqCharacterModel,
  evaluateBandCascadeDb,
  FilterType,
  ToneSettings,
  TrebleDesignMode,
  WindowResidencyMode,
} from './utils/dspMath';
import {
  ConvolutionImpulse,
  createDefaultEqBands,
  DRIVER_PROFILES,
  DriverTypeProfile,
  FACTORY_PRESETS,
  INITIAL_OUTPUTS,
  ISO_10_FREQUENCIES,
  ISO_15_FREQUENCIES,
  OutputEndpoint,
  PresetDefinition,
} from './utils/dspPresets';
import { RotaryKnob } from './components/RotaryKnob';
import { GraphLookId, ResponseGraph } from './components/ResponseGraph';
import {
  ConfigSubTab,
  ConvolutionSubTab,
  EqPresetsSubTab,
  MiniCurvePlot,
} from './components/EqPresetsAndConfigView';
import { DspRackView } from './components/DspRackView';
import {
  DEMO_TRACKS,
  DemoTrack,
  KaraokeView,
  LibraryView,
  PlusVisualizersView,
  ShareAudioView,
} from './components/OtherTabsView';

type MainTabId =
  | 'online-media'
  | 'share-audio'
  | 'eq'
  | 'dsp'
  | 'library'
  | 'karaoke'
  | 'plus';

type EqSubTabId =
  | 'bands'
  | 'eq-presets'
  | 'convolution'
  | 'game-presets'
  | 'config';

export function App() {
  // Navigation state
  const [mainTab, setMainTab] = useState<MainTabId>('eq');
  const [eqSubTab, setEqSubTab] = useState<EqSubTabId>('bands');
  const [compactPlayer, setCompactPlayer] = useState<boolean>(false);
  const [residencyMode, setResidencyMode] =
    useState<WindowResidencyMode>('active');
  const [helpOpen, setHelpOpen] = useState<boolean>(false);
  const [curvesPopoverOpen, setCurvesPopoverOpen] = useState<boolean>(false);

  // Engine & global audio state
  const [systemEqEnabled, setSystemEqEnabled] = useState<boolean>(true);
  const [autoNormalize, setAutoNormalize] = useState<boolean>(true);
  const [manualPreampDb, setManualPreampDb] = useState<number>(-1.5);
  const [showResponseGraph, setShowResponseGraph] = useState<boolean>(true);
  const [graphLook, setGraphLook] = useState<GraphLookId>('rainbow-bars');
  const [backdropUrl, setBackdropUrl] = useState<string | null>(null);

  // Outputs & per-output profiles
  const [outputs, setOutputs] = useState<OutputEndpoint[]>(INITIAL_OUTPUTS);
  const [activeOutputId, setActiveOutputId] = useState<string>(INITIAL_OUTPUTS[0].id);
  const [onePlayerAtATime, setOnePlayerAtATime] = useState<boolean>(true);
  const [newProfileName, setNewProfileName] = useState<string>('');
  const [addingProfile, setAddingProfile] = useState<boolean>(false);

  const activeOutput =
    outputs.find((o) => o.id === activeOutputId) ?? outputs[0];

  // Parametric EQ bands & character model
  const [bands, setBands] = useState<EqBand[]>(() =>
    createDefaultEqBands(ISO_15_FREQUENCIES).map((b, idx) => {
      // Subtle initial shaping matching the Modern Country screenshot nodes
      if (idx === 4) return { ...b, gainDb: 1.4 };
      if (idx === 7) return { ...b, gainDb: -1.1 };
      if (idx === 11) return { ...b, gainDb: 1.8 };
      return b;
    }),
  );
  const [selectedBandIndex, setSelectedBandIndex] = useState<number>(4);
  const [eqModel, setEqModel] = useState<EqCharacterModel>('clean');
  const [trebleMode, setTrebleMode] = useState<TrebleDesignMode>('precise');
  const [smartEqMode, setSmartEqMode] = useState<'Detail' | 'Balance' | 'Target' | 'Off'>('Detail');

  // Tone & Cuts layer
  const [tone, setTone] = useState<ToneSettings>({
    lowCutSlope: 24,
    bassDb: 0.0,
    midDb: 0.0,
    trebleDb: 0.0,
    highCutSlope: 24,
    enabled: true,
  });

  // Preset layer
  const [selectedPresetId, setSelectedPresetId] = useState<string>('modern-country');
  const [presetEnabled, setPresetEnabled] = useState<boolean>(true);
  const [presetStrength, setPresetStrength] = useState<number>(100);

  // Headphone Correction layer (OPRA / Squiglink)
  const [headphoneLabel, setHeadphoneLabel] = useState<string>(
    'Apple AirPods Pro 2 (AutoEq Crinacle)',
  );
  const [headphoneBands, setHeadphoneBands] = useState<EqBand[]>([
    { id: 'hp-1', enabled: true, type: 'LSC', frequency: 105, gainDb: 0.4, q: 0.7 },
    { id: 'hp-2', enabled: true, type: 'PK', frequency: 427, gainDb: -2.6, q: 0.8 },
    { id: 'hp-3', enabled: true, type: 'PK', frequency: 3647, gainDb: 2.3, q: 0.77 },
    { id: 'hp-4', enabled: true, type: 'PK', frequency: 6486, gainDb: -4.2, q: 4.5 },
  ]);
  const [headphoneEnabled, setHeadphoneEnabled] = useState<boolean>(true);
  const [headphoneStrength, setHeadphoneStrength] = useState<number>(100);

  // Driver compensation layer
  const [driverId, setDriverId] = useState<string>('bio-cellulose');
  const [driverEnabled, setDriverEnabled] = useState<boolean>(true);
  const [driverStrength, setDriverStrength] = useState<number>(100);

  // Convolution impulse layer
  const [activeImpulse, setActiveImpulse] = useState<ConvolutionImpulse | null>(null);

  // Transport & Playback state
  const [currentTrack, setCurrentTrack] = useState<DemoTrack>(DEMO_TRACKS[0]);
  const [isPlaying, setIsPlaying] = useState<boolean>(true);
  const [volumePct, setVolumePct] = useState<number>(70);
  const audioCtxRef = useRef<AudioContext | null>(null);

  const activePreset: PresetDefinition =
    FACTORY_PRESETS.find((p) => p.id === selectedPresetId) ?? FACTORY_PRESETS[0];

  const activeDriver: DriverTypeProfile =
    DRIVER_PROFILES.find((d) => d.id === driverId) ?? DRIVER_PROFILES[0];

  const freqGrid = useMemo(() => createLogFrequencyGrid(160), []);
  const sampleRate = activeOutput.sampleRate;

  const { toneBands, cutBands } = useMemo(() => buildToneBands(tone), [tone]);

  const eqCurveDb = useMemo(
    () =>
      evaluateBandCascadeDb(
        bands,
        freqGrid,
        sampleRate,
        eqModel,
        1.0,
        trebleMode,
        1.0,
      ),
    [bands, freqGrid, sampleRate, eqModel, trebleMode],
  );

  const toneCurveDb = useMemo(
    () =>
      evaluateBandCascadeDb(
        [...toneBands, ...cutBands],
        freqGrid,
        sampleRate,
        'clean',
        1.0,
        trebleMode,
        1.0,
      ),
    [toneBands, cutBands, freqGrid, sampleRate, trebleMode],
  );

  const presetCurveDb = useMemo(
    () =>
      presetEnabled
        ? evaluateBandCascadeDb(
            activePreset.bands,
            freqGrid,
            sampleRate,
            'clean',
            1.0,
            trebleMode,
            presetStrength / 100,
          )
        : freqGrid.map(() => 0),
    [presetEnabled, activePreset, freqGrid, sampleRate, trebleMode, presetStrength],
  );

  const headphoneCurveDb = useMemo(
    () =>
      headphoneEnabled
        ? evaluateBandCascadeDb(
            headphoneBands,
            freqGrid,
            sampleRate,
            'clean',
            1.0,
            trebleMode,
            headphoneStrength / 100,
          )
        : freqGrid.map(() => 0),
    [headphoneEnabled, headphoneBands, freqGrid, sampleRate, trebleMode, headphoneStrength],
  );

  const driverCurveDb = useMemo(
    () =>
      driverEnabled
        ? evaluateBandCascadeDb(
            activeDriver.bands,
            freqGrid,
            sampleRate,
            'clean',
            1.0,
            trebleMode,
            driverStrength / 100,
          )
        : freqGrid.map(() => 0),
    [driverEnabled, activeDriver, freqGrid, sampleRate, trebleMode, driverStrength],
  );

  const combinedCurveDb = useMemo(
    () =>
      freqGrid.map((_, i) => {
        const sum =
          (eqCurveDb[i] ?? 0) +
          (toneCurveDb[i] ?? 0) +
          (presetCurveDb[i] ?? 0) +
          (headphoneCurveDb[i] ?? 0) +
          (driverCurveDb[i] ?? 0);
        return Math.max(-24, Math.min(24, sum));
      }),
    [freqGrid, eqCurveDb, toneCurveDb, presetCurveDb, headphoneCurveDb, driverCurveDb],
  );

  const computedPreampDb = useMemo(
    () => computeAutoPreampDb(combinedCurveDb),
    [combinedCurveDb],
  );

  const effectivePreampDb = autoNormalize ? computedPreampDb : manualPreampDb;
  const residencyProfile = useMemo(
    () => computeResidencyProfile(residencyMode, isPlaying),
    [residencyMode, isPlaying],
  );

  const updateBand = (index: number, patch: Partial<EqBand>) => {
    setBands((prev) =>
      prev.map((band, idx) => (idx === index ? { ...band, ...patch } : band)),
    );
  };

  const clearEqAndTone = () => {
    setBands((prev) => prev.map((b) => ({ ...b, gainDb: 0 })));
    setTone((prev) => ({ ...prev, bassDb: 0, midDb: 0, trebleDb: 0 }));
  };

  const switchBandLayout = (count: 10 | 15) => {
    const freqs = count === 10 ? ISO_10_FREQUENCIES : ISO_15_FREQUENCIES;
    setBands(createDefaultEqBands(freqs));
    setSelectedBandIndex(0);
  };

  const addBand = () => {
    setBands((prev) => {
      if (prev.length >= 31) return prev;
      const nextBand: EqBand = {
        id: `band-${prev.length + 1}`,
        enabled: true,
        type: 'PK',
        frequency: 1200,
        gainDb: 2.0,
        q: 1.41,
        ear: 'both',
      };
      setSelectedBandIndex(prev.length);
      return [...prev, nextBand];
    });
  };

  const toggleAudiblePreviewTone = () => {
    const nextPlay = !isPlaying;
    setIsPlaying(nextPlay);
    try {
      if (nextPlay && !audioCtxRef.current && typeof window !== 'undefined' && 'AudioContext' in window) {
        const ctx = new window.AudioContext();
        audioCtxRef.current = ctx;
      }
      if (audioCtxRef.current && audioCtxRef.current.state === 'suspended') {
        audioCtxRef.current.resume().catch(() => {});
      }
    } catch {
      // Headless browser without audio device
    }
  };

  const selectedBand = bands[selectedBandIndex] ?? bands[0];

  const formatFreqShort = (f: number) =>
    f >= 1000 ? `${Number((f / 1000).toFixed(1))}k` : String(Math.round(f));

  return (
    <div
      className="flex flex-col h-screen w-screen overflow-hidden select-none text-[#ecf5fb] bg-[#06141f] relative"
      style={
        backdropUrl
          ? {
              backgroundImage: `linear-gradient(rgba(5, 18, 28, 0.82), rgba(5, 18, 28, 0.88)), url(${backdropUrl})`,
              backgroundSize: 'cover',
              backgroundPosition: 'center',
            }
          : undefined
      }
    >
      {/* Top Window Titlebar */}
      <header className="flex items-center justify-between px-3 py-1.5 bg-[#05111b]/95 border-b border-[#153347] shrink-0 gap-2">
        {/* Brand left */}
        <div className="flex items-center gap-2.5 min-w-0">
          <div className="w-7 h-7 rounded-lg bg-[#092334] border border-[#00e5cf]/60 flex items-center justify-center text-[#00e5cf] font-bold text-sm shadow-[0_0_8px_rgba(0,229,207,0.25)]">
            ∿
          </div>
          <div className="leading-tight">
            <div className="flex items-center gap-1.5">
              <span className="font-bold text-sm tracking-tight text-[#ecf5fb]">
                FluidEQ
              </span>
              <span className="px-1.5 py-0.5 rounded bg-[#0c2638] border border-[#1d4259] text-[10px] text-[#00e5cf] feq-mono">
                v2.1.0
              </span>
            </div>
            <p className="text-[10px] text-[#7fa1b5] hidden sm:block">
              Your sound. Every device. Automatically.
            </p>
          </div>
        </div>

        {/* Center Navigation Tabs + Live Output Spectrum Pill */}
        <nav
          aria-label="Primary sections"
          className="flex items-center gap-1 flex-wrap justify-center"
        >
          {(
            [
              { id: 'online-media', label: 'Online Media' },
              { id: 'share-audio', label: 'Share Audio' },
              { id: 'eq', label: 'EQ' },
            ] as const
          ).map((tab) => (
            <button
              key={tab.id}
              type="button"
              onClick={() => setMainTab(tab.id)}
              className={`px-3 py-1.5 text-xs rounded transition-colors cursor-pointer ${
                mainTab === tab.id
                  ? 'text-[#00e5cf] font-semibold border-b-2 border-[#00e5cf] bg-[#0a2132]'
                  : 'text-[#9bb7c9] hover:text-[#ecf5fb] hover:bg-[#091c2b]'
              }`}
            >
              {tab.label}
            </button>
          ))}

          {/* Live Output Spectrum Mini Pill */}
          <div className="hidden xl:flex items-center gap-3 px-3 py-1 mx-1 rounded-md bg-[#040d15] border border-[#163549]">
            <div className="flex flex-col">
              <div className="flex items-center justify-between gap-4 text-[9px] tracking-wider uppercase">
                <span className="text-[#00e5cf] font-semibold">• LIVE OUTPUT</span>
                <span className="text-[#7899ae]">RAINBOW</span>
                <span className="text-[#ecf5fb] feq-mono">-2.4 dB</span>
              </div>
              <div className="flex items-end gap-0.5 h-3.5 mt-0.5">
                {Array.from({ length: 28 }, (_, i) => {
                  const h = Math.max(
                    2,
                    Math.round(4 + 8 * Math.sin(i * 0.35 + (isPlaying ? 1.2 : 0.3))),
                  );
                  return (
                    <span
                      key={i}
                      style={{ height: `${h}px` }}
                      className="w-1 bg-gradient-to-t from-[#00e5cf] to-[#3b82f6] rounded-t-[1px] opacity-80"
                    />
                  );
                })}
              </div>
            </div>
          </div>

          {(
            [
              { id: 'dsp', label: 'DSP' },
              { id: 'library', label: 'Library' },
              { id: 'karaoke', label: 'Karaoke' },
              { id: 'plus', label: 'Plus' },
            ] as const
          ).map((tab) => (
            <button
              key={tab.id}
              type="button"
              onClick={() => setMainTab(tab.id)}
              className={`px-3 py-1.5 text-xs rounded transition-colors cursor-pointer ${
                mainTab === tab.id
                  ? 'text-[#00e5cf] font-semibold border-b-2 border-[#00e5cf] bg-[#0a2132]'
                  : 'text-[#9bb7c9] hover:text-[#ecf5fb] hover:bg-[#091c2b]'
              }`}
            >
              {tab.label}
            </button>
          ))}
        </nav>

        {/* Right Window Utilities */}
        <div className="flex items-center gap-1.5">
          <button
            type="button"
            onClick={() =>
              setBackdropUrl((prev) =>
                prev ? null : '/assets/tour/scene-aurora.jpg',
              )
            }
            title="Toggle Aurora Backdrop Visualizer"
            className={`px-2 py-1 rounded text-xs border cursor-pointer ${
              backdropUrl
                ? 'bg-[#0c2e3e] border-[#00e5cf] text-[#00e5cf]'
                : 'bg-[#081926] border-[#17374b] text-[#9bb7c9]'
            }`}
          >
            ✦ Backdrop
          </button>
          <button
            type="button"
            onClick={() =>
              setResidencyMode((prev) =>
                prev === 'cold-standby' ? 'active' : 'cold-standby',
              )
            }
            title="Toggle Tray Cold-Residency Standby (destroys UI view to save memory)"
            className={`px-2 py-1 rounded text-xs border cursor-pointer ${
              residencyMode === 'cold-standby'
                ? 'bg-[#00e5cf] border-[#00e5cf] text-[#042326] font-semibold'
                : 'bg-[#081926] border-[#17374b] text-[#9bb7c9] hover:border-[#00e5cf]'
            }`}
          >
            {residencyMode === 'cold-standby'
              ? `Wake Window (${residencyProfile.totalMb} MB)`
              : 'Tray Standby'}
          </button>
          <button
            type="button"
            onClick={() => setCompactPlayer((prev) => !prev)}
            title="Toggle Compact Player"
            className={`px-2 py-1 rounded text-xs border cursor-pointer ${
              compactPlayer
                ? 'bg-[#0c2e3e] border-[#00e5cf] text-[#00e5cf]'
                : 'bg-[#081926] border-[#17374b] text-[#9bb7c9]'
            }`}
          >
            {compactPlayer ? 'Full Window' : 'Compact'}
          </button>
          <button
            type="button"
            onClick={() => setHelpOpen((prev) => !prev)}
            className="px-2 py-1 rounded text-xs border bg-[#081926] border-[#17374b] text-[#9bb7c9] hover:border-[#00e5cf] hover:text-[#ecf5fb] cursor-pointer"
          >
            Guide (F1)
          </button>
        </div>
      </header>

      {/* Optional F1 User Guide Modal */}
      {helpOpen && (
        <div
          role="dialog"
          aria-label="FluidEQ User Guide"
          className="fixed inset-0 z-50 bg-black/60 flex items-center justify-center p-4"
        >
          <div className="bg-[#091b29] border border-[#00e5cf] rounded-xl max-w-2xl w-full p-5 shadow-2xl flex flex-col gap-3">
            <div className="flex items-center justify-between border-b border-[#17374b] pb-2.5">
              <h2 className="text-base font-semibold text-[#ecf5fb]">
                FluidEQ 2.1 — Quick Reference Guide
              </h2>
              <button
                type="button"
                onClick={() => setHelpOpen(false)}
                className="px-2.5 py-1 rounded bg-[#00e5cf] text-[#042326] text-xs font-semibold cursor-pointer"
              >
                Close
              </button>
            </div>
            <div className="text-xs text-[#a5bdcb] space-y-2 leading-relaxed">
              <p>
                <strong className="text-[#ecf5fb]">Seven layers, one chain:</strong>{' '}
                Convolution, Driver type, Headphone correction (6,229 OPRA models), Parametric EQ, Tone (Bass, Mid, Treble + Butterworth Low/High cuts), Preset curve, and Smart EQ all sum into a single computed Preamp so nothing ever clips.
              </p>
              <p>
                <strong className="text-[#ecf5fb]">Precise vs. Classic Treble:</strong>{' '}
                On the FluidEQ Engine, Precise uses Martin Vicanek&apos;s analog-matched biquads so treble bells and Butterworth shelves play as drawn up to 20 kHz without Nyquist cramping.
              </p>
              <p>
                <strong className="text-[#ecf5fb]">Every output keeps its own sound:</strong>{' '}
                Switching outputs in the right column immediately restores that device&apos;s saved profile, preset, Tone and DSP rack.
              </p>
            </div>
          </div>
        </div>
      )}

      {/* Cold-Residency Standby View vs. Full 3-Column Workstation Grid */}
      {residencyMode === 'cold-standby' ? (
        <div className="flex-1 flex items-center justify-center p-6 bg-[#05111b]">
          <div className="max-w-lg w-full bg-[#081926] border border-[#00e5cf]/60 rounded-xl p-6 shadow-2xl flex flex-col gap-4">
            <div className="flex items-center justify-between border-b border-[#153347] pb-3">
              <div>
                <p className="text-[10px] font-semibold uppercase tracking-widest text-[#00e5cf]">
                  SYSTEM TRAY · COLD RESIDENCY ACTIVE
                </p>
                <h2 className="text-base font-semibold text-[#ecf5fb] mt-0.5">
                  UI Renderer Unloaded · Audio Engine Running
                </h2>
              </div>
              <span className="px-2.5 py-1 rounded bg-[#0c2638] border border-[#00e5cf]/50 text-xs text-[#00e5cf] feq-mono font-semibold">
                {residencyProfile.totalMb} MB RAM
              </span>
            </div>

            <div className="grid grid-cols-3 gap-2.5 text-center">
              <div className="bg-[#06131d] border border-[#153347] rounded-lg p-2.5">
                <p className="text-[10px] text-[#8aa7b9]">Audio Core</p>
                <p className="text-sm font-semibold text-[#ecf5fb] feq-mono mt-0.5">
                  {residencyProfile.cppDspCoreMb} MB
                </p>
              </div>
              <div className="bg-[#06131d] border border-[#153347] rounded-lg p-2.5">
                <p className="text-[10px] text-[#8aa7b9]">State Store</p>
                <p className="text-sm font-semibold text-[#ecf5fb] feq-mono mt-0.5">
                  {residencyProfile.rustStateMb} MB
                </p>
              </div>
              <div className="bg-[#06131d] border border-[#153347] rounded-lg p-2.5">
                <p className="text-[10px] text-[#8aa7b9]">UI &amp; Spectrum</p>
                <p className="text-sm font-semibold text-[#00e5cf] feq-mono mt-0.5">
                  0 MB · 0 FPS
                </p>
              </div>
            </div>

            <div className="text-xs text-[#9bb7c9] space-y-1 bg-[#06131d] border border-[#153347] rounded-lg p-3">
              <p>
                <span className="text-[#7fa1b5]">Active Output:</span>{' '}
                <strong className="text-[#ecf5fb]">{activeOutput.name}</strong>
              </p>
              <p>
                <span className="text-[#7fa1b5]">Active Preset &amp; Headphone:</span>{' '}
                <strong className="text-[#ecf5fb]">
                  {activePreset.name} · {headphoneLabel}
                </strong>
              </p>
              <p>
                <span className="text-[#7fa1b5]">Preamp &amp; Bands:</span>{' '}
                <strong className="text-[#00e5cf] feq-mono">
                  {effectivePreampDb.toFixed(1)} dB ({bands.length} bands active)
                </strong>
              </p>
            </div>

            <button
              type="button"
              onClick={() => setResidencyMode('active')}
              className="w-full py-2 rounded-lg bg-[#00e5cf] text-[#042326] font-semibold text-xs hover:bg-[#2bf0dc] transition-colors cursor-pointer"
            >
              Restore FluidEQ Window
            </button>
          </div>
        </div>
      ) : (
      <div
        className={`flex-1 min-h-0 grid ${
          compactPlayer
            ? 'grid-cols-1 max-w-md mx-auto w-full border-x border-[#17374b]'
            : 'grid-cols-1 lg:grid-cols-[84px_minmax(0,1fr)_264px]'
        } overflow-hidden`}
      >
        {/* LEFT RAIL: System EQ Power, Preamp, Auto Normalize, Response Graph, Stereo Meter */}
        {!compactPlayer && (
          <aside
            aria-label="System EQ and Output Meter"
            className="hidden lg:flex flex-col items-center py-3 px-1.5 bg-[#06141f] border-r border-[#153347] justify-between select-none"
          >
            {/* System EQ Power Button */}
            <div className="flex flex-col items-center gap-1">
              <button
                type="button"
                aria-label="System EQ Power"
                aria-pressed={systemEqEnabled}
                onClick={() => setSystemEqEnabled((prev) => !prev)}
                className={`w-11 h-11 rounded-full flex items-center justify-center transition-all cursor-pointer border-2 ${
                  systemEqEnabled
                    ? 'bg-gradient-to-b from-[#2bf0dc] to-[#00b4a2] border-[#78fff1] text-[#032225] shadow-[0_0_16px_rgba(0,229,207,0.55)]'
                    : 'bg-[#0a1d2c] border-[#24526d] text-[#6c8ea3]'
                }`}
              >
                <span className="font-bold text-base">⏻</span>
              </button>
              <span className="text-[10px] font-semibold text-[#ecf5fb] mt-0.5">
                System EQ
              </span>
              <span
                className={`text-[8px] font-bold tracking-wider uppercase text-center leading-tight ${
                  systemEqEnabled ? 'text-[#00e5cf]' : 'text-[#6c8ea3]'
                }`}
              >
                FLUIDEQ
                <br />
                ENGINE · {systemEqEnabled ? 'ON' : 'OFF'}
              </span>
            </div>

            {/* Preamp Rotary Dial & Switches */}
            <div className="flex flex-col items-center gap-2 my-2">
              <RotaryKnob
                label="Preamp"
                value={effectivePreampDb}
                min={-20}
                max={12}
                step={0.1}
                defaultValue={-1.5}
                unit="dB"
                size={46}
                formatValue={(v) => `${v.toFixed(1)} ${autoNormalize ? 'AUTO' : 'dB'}`}
                onChange={(val) => {
                  setAutoNormalize(false);
                  setManualPreampDb(val);
                }}
              />

              <div className="flex flex-col items-center gap-1 mt-1">
                <span className="text-[9px] text-[#8aa7b9] text-center">
                  Auto normalize
                </span>
                <button
                  type="button"
                  aria-label="Auto normalize"
                  aria-pressed={autoNormalize}
                  onClick={() => setAutoNormalize((prev) => !prev)}
                  className={`w-8 h-4 rounded-full p-0.5 transition-colors cursor-pointer ${
                    autoNormalize ? 'bg-[#00e5cf]' : 'bg-[#17374b]'
                  }`}
                >
                  <span
                    className={`block w-3 h-3 rounded-full bg-[#042326] transition-transform ${
                      autoNormalize ? 'translate-x-4' : 'translate-x-0'
                    }`}
                  />
                </button>
              </div>

              <div className="flex flex-col items-center gap-1 mt-1">
                <span className="text-[9px] text-[#8aa7b9] text-center">
                  Response graph
                </span>
                <button
                  type="button"
                  aria-label="Response graph"
                  aria-pressed={showResponseGraph}
                  onClick={() => setShowResponseGraph((prev) => !prev)}
                  className={`w-8 h-4 rounded-full p-0.5 transition-colors cursor-pointer ${
                    showResponseGraph ? 'bg-[#00e5cf]' : 'bg-[#17374b]'
                  }`}
                >
                  <span
                    className={`block w-3 h-3 rounded-full bg-[#042326] transition-transform ${
                      showResponseGraph ? 'translate-x-4' : 'translate-x-0'
                    }`}
                  />
                </button>
              </div>
            </div>

            {/* Stereo L/R Output Level Meter */}
            <div className="flex flex-col items-center flex-1 min-h-[160px] w-full pt-2 border-t border-[#153347]">
              <div className="flex items-center justify-between w-full px-0.5 text-[9px] feq-mono whitespace-nowrap">
                <span className="text-[#8aa7b9]">Output</span>
                <span className="text-[#00e5cf]">
                  {isPlaying ? '-2.4 dB' : '-60 dB'}
                </span>
              </div>
              <div className="flex items-center gap-2 text-[9px] text-[#6c8ea3] mt-1">
                <span>L</span>
                <span>R</span>
              </div>
              <div className="flex-1 flex items-end justify-center gap-1.5 py-1.5 w-full">
                {/* Left bar */}
                <div className="w-2.5 h-full bg-[#040e16] rounded-full overflow-hidden border border-[#153347] flex flex-col justify-end">
                  <div
                    style={{ height: isPlaying ? '84%' : '8%' }}
                    className="w-full bg-gradient-to-t from-[#00e5cf] via-[#38bdf8] to-[#f5b942] transition-all duration-150 rounded-full"
                  />
                </div>
                {/* Right bar */}
                <div className="w-2.5 h-full bg-[#040e16] rounded-full overflow-hidden border border-[#153347] flex flex-col justify-end">
                  <div
                    style={{ height: isPlaying ? '81%' : '8%' }}
                    className="w-full bg-gradient-to-t from-[#00e5cf] via-[#38bdf8] to-[#f5b942] transition-all duration-150 rounded-full"
                  />
                </div>
              </div>
              <span className="px-2 py-0.5 rounded bg-[#091f2e] border border-[#17374b] text-[8px] tracking-wider uppercase text-[#8aa7b9]">
                FLUID
              </span>
            </div>
          </aside>
        )}

        {/* CENTER WORKSPACE */}
        <main className="flex flex-col min-w-0 min-h-0 overflow-hidden bg-[#071622]/90">
          {mainTab === 'eq' && (
            <>
              {/* Secondary EQ Sub-Tab Bar */}
              <div className="flex flex-wrap items-center justify-between px-4 py-1.5 border-b border-[#153347] bg-[#06141f]">
                <div className="flex items-center gap-4">
                  {(
                    [
                      { id: 'bands', label: 'BANDS' },
                      { id: 'eq-presets', label: 'EQ PRESETS' },
                      { id: 'convolution', label: 'CONVOLUTION' },
                      { id: 'game-presets', label: 'GAME PRESETS' },
                      { id: 'config', label: 'CONFIG' },
                    ] as const
                  ).map((sub) => (
                    <button
                      key={sub.id}
                      type="button"
                      onClick={() => setEqSubTab(sub.id)}
                      className={`py-1 text-[11px] font-semibold tracking-wider uppercase transition-colors cursor-pointer ${
                        eqSubTab === sub.id
                          ? 'text-[#00e5cf] border-b-2 border-[#00e5cf]'
                          : 'text-[#8aa7b9] hover:text-[#ecf5fb]'
                      }`}
                    >
                      {sub.label}
                    </button>
                  ))}
                </div>

                <div className="flex items-center gap-2.5 text-xs">
                  <span className="text-[#00e5cf] font-semibold">
                    FluidEQ Engine
                  </span>
                  <span className="text-[11px] text-[#8aa7b9] feq-mono">
                    34 ms
                  </span>
                </div>
              </div>

              {/* EQ Sub-Tab Content */}
              {eqSubTab === 'bands' && (
                <div className="flex-1 min-h-0 flex flex-col justify-between p-3 gap-2.5 overflow-y-auto">
                  {/* Graph Toolbar */}
                  <div className="flex flex-wrap items-center justify-between gap-2 px-1">
                    <div className="flex items-baseline gap-2">
                      <h1 className="text-sm font-semibold text-[#ecf5fb]">
                        Parametric EQ
                      </h1>
                      <span className="text-[11px] text-[#7fa1b5] feq-mono">
                        {sampleRate / 1000} kHz
                      </span>
                    </div>

                    {/* Top-right Graph Controls matching screenshot 03 */}
                    <div className="flex flex-wrap items-center gap-1.5 relative">
                      {/* Preset Picker */}
                      <select
                        aria-label="EQ Preset"
                        value={selectedPresetId}
                        onChange={(e) => {
                          setSelectedPresetId(e.target.value);
                          setPresetEnabled(true);
                        }}
                        className="bg-[#091e2e] border border-[#1d4259] rounded px-2.5 py-1 text-xs text-[#ecf5fb] focus:outline-none focus:border-[#00e5cf]"
                      >
                        {FACTORY_PRESETS.map((p) => (
                          <option key={p.id} value={p.id}>
                            ♪ {p.name}
                          </option>
                        ))}
                      </select>

                      {/* Smart EQ Picker */}
                      <select
                        aria-label="Smart EQ Mode"
                        value={smartEqMode}
                        onChange={(e) =>
                          setSmartEqMode(
                            e.target.value as 'Detail' | 'Balance' | 'Target' | 'Off',
                          )
                        }
                        className="bg-[#091e2e] border border-[#1d4259] rounded px-2 py-1 text-xs text-[#ecf5fb] focus:outline-none focus:border-[#00e5cf]"
                      >
                        <option value="Detail">✦ Detail</option>
                        <option value="Balance">✦ Balance</option>
                        <option value="Target">✦ Target</option>
                        <option value="Off">Smart EQ Off</option>
                      </select>

                      {/* Character Model & Treble Mode */}
                      <select
                        aria-label="EQ Character Mode"
                        value={eqModel}
                        onChange={(e) =>
                          setEqModel(e.target.value as EqCharacterModel)
                        }
                        className="bg-[#091e2e] border border-[#1d4259] rounded px-2 py-1 text-xs text-[#ecf5fb] focus:outline-none focus:border-[#00e5cf]"
                      >
                        <option value="clean">∿ Normal (Clean)</option>
                        <option value="proportional">∿ Proportional Q</option>
                        <option value="wide">∿ Wide Tone</option>
                        <option value="asymmetric">∿ Asymmetric</option>
                      </select>

                      {/* Band Count Layout */}
                      <select
                        aria-label="Band Count Layout"
                        value={bands.length <= 10 ? 10 : 15}
                        onChange={(e) =>
                          switchBandLayout(Number(e.target.value) as 10 | 15)
                        }
                        className="bg-[#091e2e] border border-[#1d4259] rounded px-2 py-1 text-xs text-[#ecf5fb] focus:outline-none focus:border-[#00e5cf]"
                      >
                        <option value={15}>🎚 15 Band</option>
                        <option value={10}>🎚 10 Band</option>
                      </select>

                      {/* Graph Look Selector */}
                      <select
                        aria-label="Visualizer Look"
                        value={graphLook}
                        onChange={(e) =>
                          setGraphLook(e.target.value as GraphLookId)
                        }
                        className="bg-[#091e2e] border border-[#1d4259] rounded px-2 py-1 text-xs text-[#00e5cf] focus:outline-none focus:border-[#00e5cf]"
                      >
                        <option value="rainbow-bars">Look: Rainbow Bars</option>
                        <option value="analyzer">Look: Studio Analyzer</option>
                        <option value="synthwave">Look: Synthwave</option>
                        <option value="led-wall">Look: LED Wall</option>
                      </select>

                      {/* Add Band Button */}
                      <button
                        type="button"
                        onClick={addBand}
                        title="Add parametric band"
                        className="px-2 py-1 rounded bg-[#091e2e] border border-[#1d4259] text-xs text-[#ecf5fb] hover:border-[#00e5cf] cursor-pointer"
                      >
                        ⊕
                      </button>

                      {/* Curves Layer Switcher Button */}
                      <button
                        type="button"
                        onClick={() => setCurvesPopoverOpen((prev) => !prev)}
                        className="flex items-center gap-1.5 px-2.5 py-1 rounded bg-[#091e2e] border border-[#1d4259] text-xs text-[#ecf5fb] hover:border-[#00e5cf] cursor-pointer"
                      >
                        <span className="w-2 h-2 rounded-full bg-[#c7a4eb]" />
                        <span className="w-2 h-2 rounded-full bg-[#b8f256]" />
                        <span className="w-2 h-2 rounded-full bg-[#00e5cf]" />
                        <span>Curves</span>
                      </button>

                      {/* Clear EQ Button */}
                      <button
                        type="button"
                        aria-label="Clear EQ"
                        onClick={clearEqAndTone}
                        title="Clear EQ bands and Tone to 0 dB"
                        className="px-2 py-1 rounded bg-[#091e2e] border border-[#1d4259] text-xs text-[#ecf5fb] hover:border-[#00e5cf] cursor-pointer"
                      >
                        ↺
                      </button>

                      {/* Curves Popover */}
                      {curvesPopoverOpen && (
                        <div className="absolute right-0 top-9 z-30 w-72 bg-[#081926] border border-[#00e5cf] rounded-lg p-3 shadow-xl flex flex-col gap-2 text-xs">
                          <div className="flex items-center justify-between border-b border-[#153347] pb-1.5">
                            <span className="font-semibold text-[#ecf5fb]">
                              Active Layers & Treble Design
                            </span>
                            <button
                              type="button"
                              onClick={() => setCurvesPopoverOpen(false)}
                              className="text-[#8aa7b9] hover:text-[#ecf5fb]"
                            >
                              ✕
                            </button>
                          </div>

                          <label className="flex items-center justify-between">
                            <span className="text-[#d977f2]">
                              • Preset ({activePreset.name})
                            </span>
                            <input
                              type="checkbox"
                              checked={presetEnabled}
                              onChange={(e) => setPresetEnabled(e.target.checked)}
                              className="accent-[#00e5cf]"
                            />
                          </label>

                          <label className="flex items-center justify-between">
                            <span className="text-[#c7a4eb] truncate max-w-[200px]">
                              • OPRA ({headphoneLabel})
                            </span>
                            <input
                              type="checkbox"
                              checked={headphoneEnabled}
                              onChange={(e) =>
                                setHeadphoneEnabled(e.target.checked)
                              }
                              className="accent-[#00e5cf]"
                            />
                          </label>

                          <label className="flex items-center justify-between">
                            <span className="text-[#60a5fa]">
                              • Driver ({activeDriver.name})
                            </span>
                            <input
                              type="checkbox"
                              checked={driverEnabled}
                              onChange={(e) => setDriverEnabled(e.target.checked)}
                              className="accent-[#00e5cf]"
                            />
                          </label>

                          <div className="flex items-center justify-between border-t border-[#153347] pt-2 mt-1">
                            <span className="text-[#8aa7b9]">Treble filter design</span>
                            <div className="flex gap-1">
                              {(['precise', 'classic'] as const).map((m) => (
                                <button
                                  key={m}
                                  type="button"
                                  onClick={() => setTrebleMode(m)}
                                  className={`px-2 py-0.5 rounded text-[11px] capitalize cursor-pointer ${
                                    trebleMode === m
                                      ? 'bg-[#00e5cf] text-[#042326] font-semibold'
                                      : 'bg-[#06131d] text-[#8aa7b9]'
                                  }`}
                                >
                                  {m}
                                </button>
                              ))}
                            </div>
                          </div>
                        </div>
                      )}
                    </div>
                  </div>

                  {/* Interactive Response Graph Canvas */}
                  <div className="flex-1 min-h-[210px] rounded-lg border border-[#153347] overflow-hidden relative">
                    <ResponseGraph
                      freqGrid={freqGrid}
                      eqCurveDb={eqCurveDb}
                      toneCurveDb={toneCurveDb}
                      presetCurveDb={presetCurveDb}
                      headphoneCurveDb={headphoneCurveDb}
                      driverCurveDb={driverCurveDb}
                      combinedCurveDb={combinedCurveDb}
                      bands={bands}
                      selectedBandIndex={selectedBandIndex}
                      onSelectBand={setSelectedBandIndex}
                      onUpdateBand={updateBand}
                      look={graphLook}
                      isPlaying={isPlaying}
                      systemEqEnabled={systemEqEnabled}
                      showResponseGraph={showResponseGraph}
                    />
                  </div>

                  {/* Parametric Vertical Sliders Panel */}
                  <div className="bg-[#06131e] border border-[#153347] rounded-lg px-3 py-2.5 flex flex-col gap-2">
                    {/* Selected band precision controls bar */}
                    {selectedBand && (
                      <div className="flex flex-wrap items-center justify-between gap-2 pb-2 border-b border-[#122b3d] text-xs">
                        <div className="flex items-center gap-2">
                          <span className="px-2 py-0.5 rounded bg-[#00e5cf]/20 text-[#00e5cf] font-semibold feq-mono">
                            Band {selectedBandIndex + 1}
                          </span>
                          <select
                            aria-label="Selected band filter type"
                            value={selectedBand.type}
                            onChange={(e) =>
                              updateBand(selectedBandIndex, {
                                type: e.target.value as FilterType,
                              })
                            }
                            className="bg-[#081926] border border-[#1d4259] rounded px-2 py-0.5 text-xs text-[#ecf5fb]"
                          >
                            <option value="PK">Peak / Bell (PK)</option>
                            <option value="LSC">Low Shelf (LSC)</option>
                            <option value="HSC">High Shelf (HSC)</option>
                            <option value="LPQ">Low Pass (LPQ)</option>
                            <option value="HPQ">High Pass (HPQ)</option>
                            <option value="NO">Notch (NO)</option>
                          </select>
                          <select
                            aria-label="Selected band ear channel"
                            value={selectedBand.ear ?? 'both'}
                            onChange={(e) =>
                              updateBand(selectedBandIndex, {
                                ear: e.target.value as 'both' | 'left' | 'right',
                              })
                            }
                            className="bg-[#081926] border border-[#1d4259] rounded px-2 py-0.5 text-xs text-[#ecf5fb]"
                          >
                            <option value="both">Both ears (L+R)</option>
                            <option value="left">Left ear only</option>
                            <option value="right">Right ear only</option>
                          </select>
                        </div>

                        <div className="flex items-center gap-4 feq-mono text-[11px]">
                          <label className="flex items-center gap-1.5 text-[#8aa7b9]">
                            <span>Freq:</span>
                            <input
                              type="number"
                              min={10}
                              max={20000}
                              value={selectedBand.frequency}
                              onChange={(e) =>
                                updateBand(selectedBandIndex, {
                                  frequency: Math.max(
                                    10,
                                    Math.min(20000, Number(e.target.value) || 100),
                                  ),
                                })
                              }
                              className="w-16 bg-[#081926] border border-[#1d4259] rounded px-1.5 py-0.5 text-[#ecf5fb]"
                            />
                            <span>Hz</span>
                          </label>
                          <label className="flex items-center gap-1.5 text-[#8aa7b9]">
                            <span>Q:</span>
                            <input
                              type="number"
                              step={0.1}
                              min={0.2}
                              max={18}
                              value={selectedBand.q}
                              onChange={(e) =>
                                updateBand(selectedBandIndex, {
                                  q: Math.max(
                                    0.2,
                                    Math.min(18, Number(e.target.value) || 1.0),
                                  ),
                                })
                              }
                              className="w-14 bg-[#081926] border border-[#1d4259] rounded px-1.5 py-0.5 text-[#ecf5fb]"
                            />
                          </label>
                        </div>
                      </div>
                    )}

                    {/* 15 Vertical Faders Row */}
                    <div className="flex items-center justify-between gap-1 overflow-x-auto py-1">
                      {/* dB scale column */}
                      <div className="flex flex-col justify-between h-[168px] py-2 pr-1 text-[9px] text-[#6c8ea3] feq-mono shrink-0 text-right">
                        <span>+20</span>
                        <span>+10</span>
                        <span className="text-[#ecf5fb] font-semibold">0 dB</span>
                        <span>-10</span>
                        <span>-20</span>
                      </div>

                      {bands.map((band, idx) => {
                        const isSel = idx === selectedBandIndex;
                        return (
                          <div
                            key={band.id}
                            onClick={() => setSelectedBandIndex(idx)}
                            className={`flex flex-col items-center px-1 py-1 rounded transition-colors ${
                              isSel ? 'bg-[#0b2334]' : ''
                            }`}
                          >
                            <span
                              className={`text-[10px] feq-mono mb-1 ${
                                isSel
                                  ? 'text-[#00e5cf] font-semibold'
                                  : 'text-[#9bb7c9]'
                              }`}
                            >
                              {formatFreqShort(band.frequency)}
                            </span>
                            <input
                              type="range"
                              aria-label={`Band ${idx + 1} (${formatFreqShort(band.frequency)} Hz) gain`}
                              min={-20}
                              max={20}
                              step={0.1}
                              value={band.gainDb}
                              onChange={(e) => {
                                setSelectedBandIndex(idx);
                                updateBand(idx, {
                                  gainDb: Number.parseFloat(e.target.value),
                                });
                              }}
                              onDoubleClick={() => updateBand(idx, { gainDb: 0 })}
                              className="feq-vertical-slider"
                            />
                            <span
                              className={`text-[10px] feq-mono mt-1 ${
                                Math.abs(band.gainDb) > 0.05
                                  ? 'text-[#00e5cf] font-semibold'
                                  : 'text-[#7fa1b5]'
                              }`}
                            >
                              {band.gainDb > 0
                                ? `+${band.gainDb.toFixed(1)}`
                                : band.gainDb.toFixed(1)}
                            </span>
                          </div>
                        );
                      })}
                    </div>
                  </div>

                  {/* Bottom Tone Row: LOW CUT, BASS, MID, TREBLE, HIGH CUT */}
                  <div className="bg-[#06131e] border border-[#153347] rounded-lg px-4 py-2 flex flex-wrap items-center justify-center gap-6">
                    <span className="text-xs font-semibold text-[#ecf5fb] mr-2">
                      Tone
                    </span>
                    <RotaryKnob
                      label="LOW CUT"
                      value={tone.lowCutSlope}
                      min={0}
                      max={24}
                      step={12}
                      defaultValue={24}
                      size={46}
                      formatValue={(v) => (v === 0 ? 'Off' : `${v} dB/oct`)}
                      onChange={(v) =>
                        setTone((prev) => ({
                          ...prev,
                          lowCutSlope: (v >= 18 ? 24 : v >= 6 ? 12 : 0) as 0 | 12 | 24,
                        }))
                      }
                    />
                    <RotaryKnob
                      label="BASS"
                      value={tone.bassDb}
                      min={-12}
                      max={12}
                      step={0.5}
                      defaultValue={0}
                      size={46}
                      onChange={(bassDb) => setTone((prev) => ({ ...prev, bassDb }))}
                    />
                    <RotaryKnob
                      label="MID"
                      value={tone.midDb}
                      min={-12}
                      max={12}
                      step={0.5}
                      defaultValue={0}
                      size={46}
                      onChange={(midDb) => setTone((prev) => ({ ...prev, midDb }))}
                    />
                    <RotaryKnob
                      label="TREBLE"
                      value={tone.trebleDb}
                      min={-12}
                      max={12}
                      step={0.5}
                      defaultValue={0}
                      size={46}
                      onChange={(trebleDb) =>
                        setTone((prev) => ({ ...prev, trebleDb }))
                      }
                    />
                    <RotaryKnob
                      label="HIGH CUT"
                      value={tone.highCutSlope}
                      min={0}
                      max={24}
                      step={12}
                      defaultValue={24}
                      size={46}
                      formatValue={(v) => (v === 0 ? 'Off' : `${v} dB/oct`)}
                      onChange={(v) =>
                        setTone((prev) => ({
                          ...prev,
                          highCutSlope: (v >= 18 ? 24 : v >= 6 ? 12 : 0) as 0 | 12 | 24,
                        }))
                      }
                    />
                  </div>
                </div>
              )}

              {eqSubTab === 'eq-presets' && (
                <EqPresetsSubTab
                  freqGrid={freqGrid}
                  sampleRate={sampleRate}
                  activeHeadphoneLabel={headphoneLabel}
                  headphoneBands={headphoneBands}
                  headphoneEnabled={headphoneEnabled}
                  onApplyHeadphoneCorrection={(label, nextBands) => {
                    setHeadphoneLabel(label);
                    setHeadphoneBands(nextBands);
                    setHeadphoneEnabled(true);
                  }}
                  onApplyAsEditableEq={(nextBands) => {
                    setBands(nextBands);
                    setSelectedBandIndex(0);
                    setEqSubTab('bands');
                  }}
                />
              )}

              {eqSubTab === 'convolution' && (
                <ConvolutionSubTab
                  activeImpulse={activeImpulse}
                  onSelectImpulse={setActiveImpulse}
                />
              )}

              {eqSubTab === 'game-presets' && (
                <div className="p-4 flex flex-col gap-4 h-full overflow-y-auto">
                  <section className="bg-[#091b29] border border-[#17374b] rounded-lg p-4">
                    <p className="text-[10px] font-semibold tracking-widest uppercase text-[#00e5cf]">
                      AUTOMATIC FOREGROUND GAME WATCHER
                    </p>
                    <h2 className="text-base font-semibold text-[#ecf5fb] mt-0.5">
                      Every game, its own sound
                    </h2>
                    <p className="text-xs text-[#8aa7b9] mt-1">
                      Give a game a preset and FluidEQ switches to it when the game comes to the front, keeps it while you alt-tab, and puts back what you had when the game closes.
                    </p>

                    <div className="grid grid-cols-1 md:grid-cols-3 gap-3 mt-4">
                      {[
                        {
                          game: 'Counter-Strike 2 (cs2.exe)',
                          preset: 'Tactical Shooter (Footsteps)',
                          room: 'Competitive',
                        },
                        {
                          game: 'Cyberpunk 2077 (Cyberpunk2077.exe)',
                          preset: 'Synthwave & Electronic',
                          room: 'Game World',
                        },
                        {
                          game: 'Baldur’s Gate 3 (bg3_dx11.exe)',
                          preset: 'Cinema & Soundstage',
                          room: 'Cinema',
                        },
                      ].map((item) => (
                        <div
                          key={item.game}
                          className="bg-[#071622] border border-[#17374b] rounded-lg p-3.5 flex flex-col justify-between gap-2"
                        >
                          <div>
                            <p className="text-xs font-semibold text-[#ecf5fb]">
                              {item.game}
                            </p>
                            <p className="text-[11px] text-[#00e5cf] mt-1">
                              Preset: {item.preset}
                            </p>
                            <p className="text-[11px] text-[#8aa7b9]">
                              Room: {item.room} · Footsteps detector ready
                            </p>
                          </div>
                          <button
                            type="button"
                            onClick={() => {
                              setSelectedPresetId('competitive-footsteps');
                              setEqSubTab('bands');
                            }}
                            className="px-3 py-1 rounded bg-[#0c2738] border border-[#00e5cf]/50 text-xs text-[#00e5cf] hover:bg-[#00e5cf] hover:text-[#042326] transition-colors cursor-pointer"
                          >
                            Test game profile now
                          </button>
                        </div>
                      ))}
                    </div>
                  </section>
                </div>
              )}

              {eqSubTab === 'config' && (
                <ConfigSubTab
                  outputs={outputs}
                  activeOutputId={activeOutputId}
                  onSelectOutput={setActiveOutputId}
                  preampDb={effectivePreampDb}
                  eqBands={bands}
                  eqEnabled={systemEqEnabled}
                  driverProfile={activeDriver}
                  driverEnabled={driverEnabled}
                  driverStrength={driverStrength}
                  headphoneLabel={headphoneLabel}
                  headphoneBands={headphoneBands}
                  headphoneEnabled={headphoneEnabled}
                  headphoneStrength={headphoneStrength}
                  activePreset={activePreset}
                  presetEnabled={presetEnabled}
                  presetStrength={presetStrength}
                />
              )}
            </>
          )}

          {mainTab === 'dsp' && (
            <DspRackView
              activePresetName={activePreset.name}
              presetNames={FACTORY_PRESETS.map((p) => p.name)}
              onSelectPresetName={(name) => {
                const found = FACTORY_PRESETS.find((p) => p.name === name);
                if (found) setSelectedPresetId(found.id);
              }}
            />
          )}

          {mainTab === 'share-audio' && <ShareAudioView />}

          {mainTab === 'library' && (
            <LibraryView
              currentTrack={currentTrack}
              isPlaying={isPlaying}
              onSelectTrack={(track) => {
                setCurrentTrack(track);
                setIsPlaying(true);
              }}
            />
          )}

          {mainTab === 'karaoke' && <KaraokeView isPlaying={isPlaying} />}

          {mainTab === 'plus' && (
            <PlusVisualizersView
              activeBackdrop={backdropUrl}
              onSelectBackdrop={setBackdropUrl}
            />
          )}

          {mainTab === 'online-media' && (
            <div className="p-4 flex flex-col gap-4 h-full overflow-y-auto">
              <section className="bg-[#091b29] border border-[#17374b] rounded-lg p-4">
                <p className="text-[10px] font-semibold tracking-widest uppercase text-[#00e5cf]">
                  TUNE AGAINST STREAMING SOURCES
                </p>
                <h2 className="text-base font-semibold text-[#ecf5fb] mt-0.5">
                  Online Media
                </h2>
                <p className="text-xs text-[#8aa7b9] mt-1">
                  Play YouTube, YouTube Music, Bandcamp, Twitch or Suno while watching the live spectrum and dragging your EQ bands.
                </p>
                <div className="flex flex-wrap gap-2 mt-3">
                  {['YouTube', 'YouTube Music', 'Bandcamp', 'Twitch', 'Suno'].map(
                    (site) => (
                      <button
                        key={site}
                        type="button"
                        onClick={() => setMainTab('eq')}
                        className="px-3.5 py-1.5 rounded bg-[#0c2638] border border-[#1d4259] text-xs text-[#ecf5fb] hover:border-[#00e5cf] cursor-pointer"
                      >
                        {site} · Open with Live EQ
                      </button>
                    ),
                  )}
                </div>
              </section>
            </div>
          )}
        </main>

        {/* RIGHT RAIL: Output Endpoint, Profiles, Second Output, Driver Type */}
        {!compactPlayer && (
          <aside
            aria-label="Output Device and Driver Type"
            className="hidden lg:flex flex-col justify-between p-3 bg-[#06141f] border-l border-[#153347] overflow-y-auto gap-3"
          >
            <div className="flex flex-col gap-3">
              {/* Output Box */}
              <section className="bg-[#081926] border border-[#17374b] rounded-lg p-3 flex flex-col gap-2.5">
                <div className="flex items-center justify-between">
                  <span className="text-xs font-semibold text-[#ecf5fb]">
                    🔊 Output
                  </span>
                  <span className="px-1.5 py-0.5 rounded bg-[#00e5cf] text-[#042326] font-bold text-[9px] uppercase">
                    ACTIVE
                  </span>
                </div>

                <select
                  aria-label="Active audio output device"
                  value={activeOutput.id}
                  onChange={(e) => setActiveOutputId(e.target.value)}
                  className="w-full bg-[#06131d] border border-[#1d4259] rounded px-2 py-1.5 text-xs text-[#ecf5fb] focus:outline-none focus:border-[#00e5cf]"
                >
                  {outputs.map((o) => (
                    <option key={o.id} value={o.id}>
                      {o.name}
                    </option>
                  ))}
                </select>

                <div className="flex items-center justify-between text-[11px] text-[#8aa7b9] mt-0.5">
                  <span>Profiles</span>
                  <span className="feq-mono">{activeOutput.profiles.length}</span>
                </div>

                <div className="flex flex-col gap-1">
                  {activeOutput.profiles.map((prof) => {
                    const isOn = prof === activeOutput.activeProfile;
                    return (
                      <button
                        key={prof}
                        type="button"
                        onClick={() =>
                          setOutputs((prev) =>
                            prev.map((o) =>
                              o.id === activeOutput.id
                                ? { ...o, activeProfile: prof }
                                : o,
                            ),
                          )
                        }
                        className={`flex items-center justify-between px-2.5 py-1.5 rounded text-xs cursor-pointer ${
                          isOn
                            ? 'bg-[#0e2d3e] text-[#ecf5fb] border border-[#00e5cf]/50'
                            : 'bg-[#06131d] text-[#8aa7b9] hover:text-[#ecf5fb]'
                        }`}
                      >
                        <span className="truncate">{prof}</span>
                        {isOn && (
                          <span className="px-1.5 py-0.2 rounded bg-[#00e5cf] text-[#042326] text-[9px] font-bold">
                            ON
                          </span>
                        )}
                      </button>
                    );
                  })}
                </div>

                {addingProfile ? (
                  <form
                    onSubmit={(e) => {
                      e.preventDefault();
                      const trimmed = newProfileName.trim();
                      if (!trimmed) return;
                      setOutputs((prev) =>
                        prev.map((o) =>
                          o.id === activeOutput.id
                            ? {
                                ...o,
                                profiles: [...o.profiles, trimmed],
                                activeProfile: trimmed,
                              }
                            : o,
                        ),
                      );
                      setNewProfileName('');
                      setAddingProfile(false);
                    }}
                    className="flex gap-1.5 mt-1"
                  >
                    <input
                      type="text"
                      placeholder="Profile name..."
                      value={newProfileName}
                      onChange={(e) => setNewProfileName(e.target.value)}
                      className="flex-1 bg-[#051018] border border-[#00e5cf] rounded px-2 py-1 text-xs text-[#ecf5fb]"
                    />
                    <button
                      type="submit"
                      className="px-2 py-1 rounded bg-[#00e5cf] text-[#042326] text-xs font-semibold cursor-pointer"
                    >
                      Save
                    </button>
                  </form>
                ) : (
                  <div className="grid grid-cols-3 gap-1.5 mt-1">
                    <button
                      type="button"
                      onClick={() => setAddingProfile(true)}
                      className="px-2 py-1 rounded bg-[#0b2131] border border-[#1d4259] text-[11px] text-[#ecf5fb] hover:border-[#00e5cf] cursor-pointer"
                    >
                      ⊕ New
                    </button>
                    <button
                      type="button"
                      onClick={clearEqAndTone}
                      className="px-2 py-1 rounded bg-[#0b2131] border border-[#1d4259] text-[11px] text-[#8aa7b9] hover:text-[#ecf5fb] cursor-pointer"
                    >
                      ↺ Restore
                    </button>
                    <button
                      type="button"
                      onClick={() => {}}
                      className="px-2 py-1 rounded bg-[#00e5cf] text-[#042326] font-semibold text-[11px] hover:bg-[#2bf0dc] cursor-pointer"
                    >
                      ↓ Update
                    </button>
                  </div>
                )}

                <p className="text-[10px] text-[#7fa1b5] leading-snug">
                  Edits save to the profile that is on and follow this output whenever Windows selects it.
                </p>
              </section>

              {/* Second Output Box */}
              <section className="bg-[#081926] border border-[#17374b] rounded-lg p-3 flex flex-col gap-2">
                <div className="flex items-center justify-between">
                  <span className="text-xs font-semibold text-[#ecf5fb]">
                    ⊞ Second output
                  </span>
                  <span className="text-[11px] text-[#8aa7b9]">
                    {outputs.some((o) => o.id !== activeOutput.id && o.mirrorEnabled)
                      ? 'Mirroring'
                      : 'Off'}
                  </span>
                </div>

                <label className="flex items-center justify-between gap-2 text-[11px] text-[#a5bdcb] cursor-pointer">
                  <span>One player at a time</span>
                  <input
                    type="checkbox"
                    checked={onePlayerAtATime}
                    onChange={(e) => setOnePlayerAtATime(e.target.checked)}
                    className="accent-[#00e5cf]"
                  />
                </label>

                <p className="text-[10px] text-[#6c8ea3] leading-snug">
                  Starting something here pauses the rest of the machine, and the other way round.
                </p>
              </section>

              {/* Driver Type Box */}
              <section className="bg-[#081926] border border-[#17374b] rounded-lg p-3 flex flex-col gap-2.5">
                <div className="flex items-center justify-between">
                  <span className="text-xs font-semibold text-[#ecf5fb]">
                    🎧 Driver type
                  </span>
                  <input
                    type="checkbox"
                    aria-label="Enable Driver type compensation"
                    checked={driverEnabled}
                    onChange={(e) => setDriverEnabled(e.target.checked)}
                    className="accent-[#00e5cf]"
                  />
                </div>

                <select
                  aria-label="Driver type profile"
                  value={activeDriver.id}
                  onChange={(e) => setDriverId(e.target.value)}
                  className="w-full bg-[#06131d] border border-[#1d4259] rounded px-2 py-1.5 text-xs text-[#ecf5fb] focus:outline-none focus:border-[#00e5cf]"
                >
                  {DRIVER_PROFILES.map((d) => (
                    <option key={d.id} value={d.id}>
                      {d.name} — {d.subtitle}
                    </option>
                  ))}
                </select>

                <MiniCurvePlot
                  freqGrid={freqGrid}
                  curveDb={driverCurveDb}
                  strokeColor="#60a5fa"
                  compact
                />

                <div className="flex items-center justify-between text-xs mt-1">
                  <span className="text-[#a5bdcb]">Strength</span>
                  <span className="text-[#00e5cf] font-semibold feq-mono">
                    {driverStrength}%
                  </span>
                </div>
                <input
                  type="range"
                  aria-label="Driver compensation strength"
                  min={0}
                  max={150}
                  value={driverStrength}
                  onChange={(e) => setDriverStrength(Number(e.target.value))}
                  className="w-full feq-horizontal-slider"
                />

                <div className="flex items-center gap-2 text-[11px]">
                  <span className="px-2 py-0.5 rounded bg-[#0c2638] text-[#00e5cf] feq-mono">
                    {activeDriver.summaryTag}
                  </span>
                </div>
                <p className="text-[10px] text-[#7fa1b5] leading-snug">
                  {activeDriver.note}
                </p>
              </section>
            </div>

            <div className="text-center pt-1">
              <span className="text-[11px] text-[#6c8ea3]">fluideq.com ↗</span>
            </div>
          </aside>
        )}
      </div>
      )}

      {/* Bottom Transport Bar */}
      <footer className="flex items-center justify-between px-4 py-2 bg-[#05111b] border-t border-[#153347] shrink-0 gap-4">
        {/* Track metadata */}
        <div className="flex items-center gap-3 min-w-0">
          <img
            src={currentTrack.cover}
            alt={currentTrack.title}
            className="w-9 h-9 rounded object-cover border border-[#1d4259]"
          />
          <div className="min-w-0">
            <p className="text-xs font-semibold text-[#ecf5fb] truncate">
              {currentTrack.title}
            </p>
            <p className="text-[10px] text-[#8aa7b9] truncate">
              {currentTrack.artist} ·{' '}
              <span className="text-[#00e5cf] font-semibold">SYSTEM AUDIO</span>
            </p>
          </div>
        </div>

        {/* Play / Pause / Skip Transport Controls */}
        <div className="flex items-center gap-3">
          <button
            type="button"
            aria-label="Previous track"
            onClick={() => {
              const idx = DEMO_TRACKS.findIndex((t) => t.id === currentTrack.id);
              const prev =
                DEMO_TRACKS[(idx - 1 + DEMO_TRACKS.length) % DEMO_TRACKS.length];
              setCurrentTrack(prev);
            }}
            className="text-xs text-[#9bb7c9] hover:text-[#ecf5fb] px-1.5 py-1 cursor-pointer"
          >
            ⏮
          </button>
          <button
            type="button"
            aria-label={isPlaying ? 'Pause playback' : 'Start playback'}
            onClick={toggleAudiblePreviewTone}
            className="w-8 h-8 rounded-full bg-[#00e5cf] text-[#042326] font-bold text-xs flex items-center justify-center shadow-[0_0_12px_rgba(0,229,207,0.45)] hover:bg-[#2bf0dc] cursor-pointer"
          >
            {isPlaying ? '⏸' : '▶'}
          </button>
          <button
            type="button"
            aria-label="Next track"
            onClick={() => {
              const idx = DEMO_TRACKS.findIndex((t) => t.id === currentTrack.id);
              const next = DEMO_TRACKS[(idx + 1) % DEMO_TRACKS.length];
              setCurrentTrack(next);
            }}
            className="text-xs text-[#9bb7c9] hover:text-[#ecf5fb] px-1.5 py-1 cursor-pointer"
          >
            ⏭
          </button>
        </div>

        {/* Master Windows Volume Slider */}
        <div className="flex items-center gap-2">
          <span className="text-xs text-[#8aa7b9]">🔊</span>
          <input
            type="range"
            aria-label="System output volume"
            min={0}
            max={100}
            value={volumePct}
            onChange={(e) => setVolumePct(Number(e.target.value))}
            className="w-24 feq-horizontal-slider"
          />
          <span className="text-[11px] text-[#9bb7c9] w-8 text-right feq-mono">
            {volumePct}%
          </span>
        </div>
      </footer>
    </div>
  );
}

export default App;
