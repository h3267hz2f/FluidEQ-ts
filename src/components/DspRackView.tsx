import React, { useState } from 'react';
import { ROOM_PRESETS, RoomPreset, SpeakerNode } from '../utils/dspPresets';
import { RotaryKnob } from './RotaryKnob';

export type DspStageId =
  | 'normalizer'
  | 'denoise'
  | 'deesser'
  | 'exciter'
  | 'bass-forge'
  | 'equaliser'
  | 'bass-punch'
  | 'dimension'
  | 'room'
  | 'maximizer'
  | 'master'
  | 'crossfade';

interface DspStageMeta {
  id: DspStageId;
  label: string;
  badge: string;
  description: string;
}

const DSP_STAGES: readonly DspStageMeta[] = [
  {
    id: 'normalizer',
    label: 'Normalizer',
    badge: '≈',
    description: 'Stereo-linked true-peak & per-song loudness memory ahead of the rack.',
  },
  {
    id: 'denoise',
    label: 'Denoise',
    badge: '⇪',
    description: 'Adaptive hiss floor subtraction, 50/60 Hz mains hum notch, and click repair.',
  },
  {
    id: 'deesser',
    label: 'De-esser',
    badge: 'S',
    description: 'Turns down only a voice’s sharp sibilant S band without dulling cymbals.',
  },
  {
    id: 'exciter',
    label: 'Exciter',
    badge: '↗',
    description: 'Even-order tube body, odd-order tape air, Organic density, and Timing transient focus.',
  },
  {
    id: 'bass-forge',
    label: 'Bass Forge',
    badge: '∿',
    description: 'Synthesises missing fundamental harmonics for transducers that roll off early.',
  },
  {
    id: 'equaliser',
    label: 'Equaliser',
    badge: '🎚',
    description: 'Analog-matched or linear-phase cascade with dynamic thresholds and per-ear bands.',
  },
  {
    id: 'bass-punch',
    label: 'Bass Punch',
    badge: '∧',
    description: 'Shapes the first 15 milliseconds of low-end kick attack in the time domain.',
  },
  {
    id: 'dimension',
    label: 'Dimension',
    badge: '↔',
    description: 'Only the sides are touched, so a mono listener hears exactly what they would with this off.',
  },
  {
    id: 'room',
    label: 'Room',
    badge: '(○)',
    description: '7.1 KEMAR binaural listening room with early wall reflections and late ambience.',
  },
  {
    id: 'maximizer',
    label: 'Maximizer',
    badge: '↑',
    description: 'Raises programme density while holding true peaks under the delivery ceiling.',
  },
  {
    id: 'master',
    label: 'Master',
    badge: '⇥',
    description: 'Delivery target loudness & true-peak limiter (Streaming -14 LUFS, Cinema, Reference).',
  },
];

interface DspRackViewProps {
  activePresetName: string;
  onSelectPresetName: (name: string) => void;
  presetNames: readonly string[];
}

export const DspRackView: React.FC<DspRackViewProps> = ({
  activePresetName,
  onSelectPresetName,
  presetNames,
}) => {
  const [selectedStage, setSelectedStage] = useState<DspStageId>('room');
  const [stageEnabled, setStageEnabled] = useState<Record<DspStageId, boolean>>({
    normalizer: true,
    denoise: false,
    deesser: false,
    exciter: false,
    'bass-forge': false,
    equaliser: true,
    'bass-punch': false,
    dimension: true,
    room: true,
    maximizer: true,
    master: false,
    crossfade: true,
  });

  // Room state
  const [roomPresetId, setRoomPresetId] = useState<string>('game-world');
  const currentRoomPreset: RoomPreset =
    ROOM_PRESETS.find((p) => p.id === roomPresetId) ?? ROOM_PRESETS[0];
  const [speakers, setSpeakers] = useState<SpeakerNode[]>(currentRoomPreset.speakers);
  const [selectedSpeakerId, setSelectedSpeakerId] = useState<SpeakerNode['id']>('FL');
  const [spacePct, setSpacePct] = useState<number>(currentRoomPreset.spacePct);
  const [ambiencePct, setAmbiencePct] = useState<number>(currentRoomPreset.ambiencePct);
  const [distanceM, setDistanceM] = useState<number>(currentRoomPreset.distanceM);
  const [roomSizeM, setRoomSizeM] = useState<number>(currentRoomPreset.roomSizeM);
  const [wallsAbs, setWallsAbs] = useState<number>(currentRoomPreset.wallsAbsorption);
  const [tailSec, setTailSec] = useState<number>(currentRoomPreset.tailSeconds);
  const [tailToneHz, setTailToneHz] = useState<number>(currentRoomPreset.tailToneHz);
  const [keepPositions, setKeepPositions] = useState<boolean>(true);
  const [stereoFill, setStereoFill] = useState<boolean>(currentRoomPreset.stereoFill);
  const [headSize, setHeadSize] = useState<'Small' | 'Medium' | 'Large'>('Medium');
  const [listeningTestStep, setListeningTestStep] = useState<number | null>(null);

  // Dimension state
  const [lowWidth, setLowWidth] = useState(0.6);
  const [midWidth, setMidWidth] = useState(1.0);
  const [highWidth, setHighWidth] = useState(1.2);
  const [lowSplitHz, setLowSplitHz] = useState(150);
  const [highSplitHz, setHighSplitHz] = useState(3200);
  const [spreadAmount, setSpreadAmount] = useState(0.15);

  const applyRoomPreset = (id: string) => {
    const found = ROOM_PRESETS.find((r) => r.id === id);
    if (!found) return;
    setRoomPresetId(found.id);
    setSpeakers(found.speakers);
    setSpacePct(found.spacePct);
    setAmbiencePct(found.ambiencePct);
    setDistanceM(found.distanceM);
    setRoomSizeM(found.roomSizeM);
    setWallsAbs(found.wallsAbsorption);
    setTailSec(found.tailSeconds);
    setTailToneHz(found.tailToneHz);
    setStereoFill(found.stereoFill);
  };

  const selectedSpeaker =
    speakers.find((s) => s.id === selectedSpeakerId) ?? speakers[0];

  const updateSelectedSpeaker = (patch: Partial<SpeakerNode>) => {
    setSpeakers((prev) =>
      prev.map((s) => (s.id === selectedSpeaker.id ? { ...s, ...patch } : s)),
    );
  };

  const toggleStage = (id: DspStageId) => {
    setStageEnabled((prev) => ({ ...prev, [id]: !prev[id] }));
  };

  return (
    <div className="flex flex-col h-full bg-[#06141f] text-[#ecf5fb] overflow-hidden">
      {/* DSP Rack Top Header */}
      <header className="flex flex-wrap items-center justify-between gap-3 px-4 py-2.5 border-b border-[#17374b] bg-[#081926]">
        <div className="flex items-center gap-3">
          <span className="text-sm font-bold tracking-wide">DSP</span>
          <span className="text-[11px] uppercase tracking-wider text-[#8aa7b9]">
            PRESETS
          </span>
          <select
            aria-label="DSP Rack Chain Preset"
            value={activePresetName}
            onChange={(e) => onSelectPresetName(e.target.value)}
            className="bg-[#06131d] border border-[#1d4259] rounded px-3 py-1 text-xs text-[#ecf5fb] focus:outline-none focus:border-[#00e5cf]"
          >
            {presetNames.map((name) => (
              <option key={name} value={name}>
                {name}
              </option>
            ))}
          </select>
          <button
            type="button"
            onClick={() => applyRoomPreset(roomPresetId)}
            className="px-2.5 py-1 rounded border border-[#1d4259] bg-[#0a1e2e] text-xs text-[#a5bdcb] hover:border-[#00e5cf] hover:text-[#ecf5fb] cursor-pointer"
          >
            Reset
          </button>
        </div>

        <div className="flex items-center gap-3 text-xs">
          <span className="px-2.5 py-1 rounded bg-[#071622] border border-[#17374b] text-[#8aa7b9] feq-mono">
            Processing delay 34 ms
          </span>
          <label className="flex items-center gap-2 cursor-pointer">
            <span className="text-[#8aa7b9]">Rack On</span>
            <input
              type="checkbox"
              checked={stageEnabled[selectedStage]}
              onChange={() => toggleStage(selectedStage)}
              className="accent-[#00e5cf] w-4 h-4"
            />
          </label>
        </div>
      </header>

      {/* Main 2-column DSP layout: Left 11-stage rail + Right stage workspace */}
      <div className="grid grid-cols-1 md:grid-cols-[200px_1fr] flex-1 min-h-0 overflow-hidden">
        {/* Left Stage Rail */}
        <aside className="border-r border-[#17374b] bg-[#071622] p-2 flex flex-col gap-1 overflow-y-auto">
          {DSP_STAGES.map((stage) => {
            const isSelected = selectedStage === stage.id;
            const isOn = stageEnabled[stage.id];
            return (
              <button
                key={stage.id}
                type="button"
                onClick={() => setSelectedStage(stage.id)}
                className={`flex items-center justify-between px-3 py-2 rounded text-xs transition-colors cursor-pointer ${
                  isSelected
                    ? 'bg-[#0e2d3e] text-[#00e5cf] border border-[#00e5cf]/60 font-semibold'
                    : 'text-[#a5bdcb] hover:bg-[#0b2131] border border-transparent'
                }`}
              >
                <span className="flex items-center gap-2">
                  <span className="w-4 text-center feq-mono text-[11px] opacity-80">
                    {stage.badge}
                  </span>
                  <span>{stage.label}</span>
                </span>
                <span
                  onClick={(e) => {
                    e.stopPropagation();
                    toggleStage(stage.id);
                  }}
                  title={isOn ? 'Stage active (click to toggle)' : 'Stage bypassed (click to toggle)'}
                  className={`w-2 h-2 rounded-full ${
                    isOn ? 'bg-[#00e5cf] shadow-[0_0_6px_#00e5cf]' : 'bg-[#2b4c61]'
                  }`}
                />
              </button>
            );
          })}
        </aside>

        {/* Right Stage Workspace */}
        <main className="p-4 overflow-y-auto">
          {selectedStage === 'room' ? (
            <div className="flex flex-col gap-4">
              {/* Room stage header */}
              <div className="flex flex-wrap items-center justify-between gap-3 bg-[#091b29] border border-[#17374b] rounded-lg p-3">
                <div className="flex items-center gap-3">
                  <span className="text-[11px] uppercase tracking-wider text-[#8aa7b9]">
                    PRESET
                  </span>
                  <select
                    aria-label="Room Preset"
                    value={roomPresetId}
                    onChange={(e) => applyRoomPreset(e.target.value)}
                    className="bg-[#06131d] border border-[#1d4259] rounded px-3 py-1.5 text-xs text-[#ecf5fb] focus:outline-none focus:border-[#00e5cf]"
                  >
                    {ROOM_PRESETS.map((rp) => (
                      <option key={rp.id} value={rp.id}>
                        {rp.name} ({rp.badge})
                      </option>
                    ))}
                  </select>
                  <span className="text-xs text-[#00e5cf]">
                    • {currentRoomPreset.summary}
                  </span>
                </div>

                <label className="flex items-center gap-2 text-xs cursor-pointer">
                  <span>Room On</span>
                  <input
                    type="checkbox"
                    checked={stageEnabled.room}
                    onChange={() => toggleStage('room')}
                    className="accent-[#00e5cf] w-4 h-4"
                  />
                </label>
              </div>

              {/* Room 4-column interactive grid matching docs/32-dsp-room.png */}
              <div className="grid grid-cols-1 lg:grid-cols-12 gap-4">
                {/* Left Top-Down 7.1 Speaker Map (4 cols) */}
                <div className="lg:col-span-4 bg-[#081926] border border-[#17374b] rounded-lg p-4 flex flex-col items-center justify-between">
                  <span className="text-[10px] font-semibold tracking-widest uppercase text-[#8aa7b9]">
                    FRONT
                  </span>

                  <div className="relative w-64 h-64 my-2 border border-[#1d4259] rounded-lg bg-[#05121c] flex items-center justify-center">
                    {/* Listener head in center */}
                    <div className="w-10 h-10 rounded-full bg-[#dce9f2] text-[#05121c] flex flex-col items-center justify-center shadow">
                      <div className="w-0 h-0 border-l-4 border-r-4 border-b-6 border-l-transparent border-r-transparent border-b-[#00e5cf] -mt-3 mb-0.5" />
                      <span className="text-[9px] font-bold">{headSize[0]}</span>
                    </div>

                    {/* 7.1 Speakers around listener */}
                    {speakers.map((sp) => {
                      const rad = ((sp.angleDeg - 90) * Math.PI) / 180;
                      const radiusPx = 56 + (sp.distanceM / 3.0) * 44;
                      const cx = 128 + radiusPx * Math.cos(rad);
                      const cy = 128 + radiusPx * Math.sin(rad);
                      const isSel = sp.id === selectedSpeaker.id;
                      const isLit = (stereoFill || sp.activeInStereo) && !sp.muted;

                      return (
                        <button
                          key={sp.id}
                          type="button"
                          onClick={() => setSelectedSpeakerId(sp.id)}
                          style={{
                            left: `${cx - 18}px`,
                            top: `${cy - 18}px`,
                          }}
                          className={`absolute w-9 h-9 rounded-lg flex flex-col items-center justify-center text-[10px] font-semibold transition-all cursor-pointer ${
                            isSel
                              ? 'bg-[#00e5cf]/25 border-2 border-[#00e5cf] text-[#00e5cf] shadow-[0_0_10px_rgba(0,229,207,0.4)]'
                              : isLit
                                ? 'bg-[#0d2b3e] border border-[#00e5cf]/70 text-[#ecf5fb]'
                                : 'bg-[#081824] border border-[#1d3b4f] text-[#5d7c90]'
                          }`}
                        >
                          <span>{sp.id}</span>
                        </button>
                      );
                    })}
                  </div>

                  <div className="text-center">
                    <p className="text-xs text-[#8aa7b9] feq-mono">
                      Room width: {roomSizeM.toFixed(1)} m · Listener radius: {distanceM.toFixed(1)} m
                    </p>
                    <p className="text-[11px] text-[#6c8ea3] mt-1">
                      Click any speaker in the 7.1 ring to inspect and position its angle, distance and level.
                    </p>
                  </div>
                </div>

                {/* Right 8 cols: Selected Speaker, Make It Yours, Room Character, Bass & Centre, Stereo Source, Your Head */}
                <div className="lg:col-span-8 grid grid-cols-1 sm:grid-cols-2 gap-4">
                  {/* Selected Speaker Pane */}
                  <div className="bg-[#081926] border border-[#1d4259] rounded-lg p-3.5 flex flex-col justify-between">
                    <div className="flex items-center justify-between">
                      <div className="flex items-center gap-2">
                        <span className="px-2 py-0.5 rounded bg-[#00e5cf]/20 text-[#00e5cf] text-xs font-semibold feq-mono">
                          {selectedSpeaker.id}
                        </span>
                        <span className="text-xs font-semibold text-[#ecf5fb]">
                          {selectedSpeaker.label}
                        </span>
                      </div>
                      <div className="flex items-center gap-1.5">
                        <button
                          type="button"
                          onClick={() =>
                            updateSelectedSpeaker({ muted: !selectedSpeaker.muted })
                          }
                          className={`px-2.5 py-1 rounded text-xs border cursor-pointer ${
                            selectedSpeaker.muted
                              ? 'bg-[#00e5cf] text-[#042326] border-[#00e5cf] font-semibold'
                              : 'bg-[#06131d] text-[#a5bdcb] border-[#1d4259]'
                          }`}
                        >
                          Mute
                        </button>
                        <button
                          type="button"
                          onClick={() =>
                            updateSelectedSpeaker({ solo: !selectedSpeaker.solo })
                          }
                          className={`px-2.5 py-1 rounded text-xs border cursor-pointer ${
                            selectedSpeaker.solo
                              ? 'bg-[#00e5cf] text-[#042326] border-[#00e5cf] font-semibold'
                              : 'bg-[#06131d] text-[#a5bdcb] border-[#1d4259]'
                          }`}
                        >
                          Solo
                        </button>
                      </div>
                    </div>

                    <div className="flex items-center justify-around my-3">
                      <RotaryKnob
                        label="Angle"
                        value={selectedSpeaker.angleDeg}
                        min={-180}
                        max={180}
                        step={1}
                        defaultValue={-30}
                        unit="°"
                        onChange={(angleDeg) => updateSelectedSpeaker({ angleDeg })}
                      />
                      <RotaryKnob
                        label="Distance"
                        value={selectedSpeaker.distanceM}
                        min={0.5}
                        max={3.0}
                        step={0.1}
                        defaultValue={1.6}
                        unit="m"
                        onChange={(d) => updateSelectedSpeaker({ distanceM: d })}
                      />
                      <RotaryKnob
                        label="Level"
                        value={selectedSpeaker.levelDb}
                        min={-12}
                        max={6}
                        step={0.5}
                        defaultValue={0}
                        unit="dB"
                        onChange={(levelDb) => updateSelectedSpeaker({ levelDb })}
                      />
                    </div>
                    <p className="text-[11px] text-[#8aa7b9] text-center">
                      {selectedSpeaker.activeInStereo || stereoFill
                        ? 'This speaker is actively fed in the current listening mode.'
                        : 'Waiting for a discrete 5.1 / 7.1 stream or Fill the room.'}
                    </p>
                  </div>

                  {/* Make It Yours */}
                  <div className="bg-[#081926] border border-[#17374b] rounded-lg p-3.5 flex flex-col justify-between">
                    <p className="text-[10px] font-semibold tracking-widest uppercase text-[#8aa7b9] text-center">
                      MAKE IT YOURS
                    </p>
                    <div className="flex items-center justify-around my-3">
                      <RotaryKnob
                        label="Space"
                        value={spacePct}
                        min={0}
                        max={100}
                        step={1}
                        defaultValue={61}
                        unit="%"
                        onChange={setSpacePct}
                      />
                      <RotaryKnob
                        label="Ambience"
                        value={ambiencePct}
                        min={0}
                        max={100}
                        step={1}
                        defaultValue={30}
                        unit="%"
                        onChange={setAmbiencePct}
                      />
                      <RotaryKnob
                        label="Distance"
                        value={distanceM}
                        min={0.8}
                        max={3.0}
                        step={0.1}
                        defaultValue={1.6}
                        unit="m"
                        onChange={setDistanceM}
                      />
                    </div>
                    <p className="text-[11px] text-[#8aa7b9] text-center">
                      Space is how much of the walls you hear. Ambience is the soft tail after them.
                    </p>
                  </div>

                  {/* Room Character */}
                  <div className="bg-[#081926] border border-[#17374b] rounded-lg p-3.5 flex flex-col justify-between">
                    <p className="text-[10px] font-semibold tracking-widest uppercase text-[#8aa7b9] text-center">
                      ROOM CHARACTER
                    </p>
                    <div className="flex items-center justify-around my-2">
                      <RotaryKnob
                        label="Size"
                        value={roomSizeM}
                        min={3.0}
                        max={15.0}
                        step={0.2}
                        defaultValue={5.0}
                        unit="m"
                        size={48}
                        onChange={setRoomSizeM}
                      />
                      <RotaryKnob
                        label="Walls"
                        value={wallsAbs}
                        min={0.1}
                        max={0.95}
                        step={0.05}
                        defaultValue={0.55}
                        unit=""
                        size={48}
                        onChange={setWallsAbs}
                      />
                      <RotaryKnob
                        label="Tail"
                        value={tailSec}
                        min={0.1}
                        max={1.8}
                        step={0.05}
                        defaultValue={0.25}
                        unit="s"
                        size={48}
                        onChange={setTailSec}
                      />
                      <RotaryKnob
                        label="Tone"
                        value={tailToneHz}
                        min={1500}
                        max={12000}
                        step={100}
                        defaultValue={8000}
                        unit="Hz"
                        size={48}
                        onChange={setTailToneHz}
                      />
                    </div>
                    <label className="flex items-center justify-center gap-2 text-xs text-[#ecf5fb] mt-2 cursor-pointer">
                      <input
                        type="checkbox"
                        checked={keepPositions}
                        onChange={(e) => setKeepPositions(e.target.checked)}
                        className="accent-[#00e5cf]"
                      />
                      <span>Keep their positions (rests Dimension widening)</span>
                    </label>
                  </div>

                  {/* Stereo Source & KEMAR Head Fit */}
                  <div className="bg-[#081926] border border-[#17374b] rounded-lg p-3.5 flex flex-col justify-between gap-3">
                    <div>
                      <p className="text-[10px] font-semibold tracking-widest uppercase text-[#8aa7b9] text-center mb-2">
                        STEREO SOURCE & YOUR HEAD
                      </p>
                      <div className="flex items-center justify-center gap-2">
                        <button
                          type="button"
                          onClick={() => setStereoFill(false)}
                          className={`px-3 py-1 rounded text-xs border cursor-pointer ${
                            !stereoFill
                              ? 'bg-[#0e2e40] border-[#00e5cf] text-[#00e5cf] font-semibold'
                              : 'bg-[#06131d] border-[#1d4259] text-[#8aa7b9]'
                          }`}
                        >
                          Front stage
                        </button>
                        <button
                          type="button"
                          onClick={() => setStereoFill(true)}
                          className={`px-3 py-1 rounded text-xs border cursor-pointer ${
                            stereoFill
                              ? 'bg-[#0e2e40] border-[#00e5cf] text-[#00e5cf] font-semibold'
                              : 'bg-[#06131d] border-[#1d4259] text-[#8aa7b9]'
                          }`}
                        >
                          Fill the room
                        </button>
                      </div>
                    </div>

                    <div className="flex flex-col items-center gap-2 border-t border-[#153347] pt-2">
                      <div className="flex items-center gap-1.5">
                        {(['Small', 'Medium', 'Large'] as const).map((sz) => (
                          <button
                            key={sz}
                            type="button"
                            onClick={() => setHeadSize(sz)}
                            className={`px-2.5 py-1 rounded text-xs border cursor-pointer ${
                              headSize === sz
                                ? 'bg-[#0e2e40] border-[#00e5cf] text-[#00e5cf] font-semibold'
                                : 'bg-[#06131d] border-[#1d4259] text-[#8aa7b9]'
                            }`}
                          >
                            {sz}
                          </button>
                        ))}
                      </div>
                      <button
                        type="button"
                        onClick={() =>
                          setListeningTestStep(listeningTestStep === null ? 1 : null)
                        }
                        className="px-3 py-1 rounded border border-[#24526d] bg-[#0b2232] text-xs text-[#ecf5fb] hover:border-[#00e5cf] cursor-pointer"
                      >
                        {listeningTestStep === null
                          ? 'Start the listening test'
                          : `Listening test pair ${listeningTestStep} of 5 (Click to close)`}
                      </button>
                    </div>
                  </div>
                </div>
              </div>
            </div>
          ) : (
            /* Dimension & Other 10 DSP Stages View */
            <div className="flex flex-col gap-4">
              <div className="bg-[#091b29] border border-[#17374b] rounded-lg p-4">
                <div className="flex items-center justify-between mb-2">
                  <div>
                    <p className="text-[10px] font-semibold tracking-widest uppercase text-[#00e5cf]">
                      STUDIO RACK STAGE
                    </p>
                    <h2 className="text-base font-semibold text-[#ecf5fb]">
                      {DSP_STAGES.find((s) => s.id === selectedStage)?.label}
                    </h2>
                    <p className="text-xs text-[#8aa7b9] mt-0.5">
                      {DSP_STAGES.find((s) => s.id === selectedStage)?.description}
                    </p>
                  </div>
                  <button
                    type="button"
                    onClick={() => toggleStage(selectedStage)}
                    className={`px-3.5 py-1.5 rounded text-xs font-semibold cursor-pointer ${
                      stageEnabled[selectedStage]
                        ? 'bg-[#00e5cf] text-[#042326]'
                        : 'bg-[#0d2436] text-[#8aa7b9] border border-[#24526d]'
                    }`}
                  >
                    {stageEnabled[selectedStage] ? 'Stage On' : 'Bypassed'}
                  </button>
                </div>

                {/* Stage-specific interactive vector scope & frequency width plot */}
                <div className="grid grid-cols-1 lg:grid-cols-2 gap-6 mt-4">
                  <div className="bg-[#06131d] border border-[#17374b] rounded-lg p-4 flex items-center justify-around">
                    <RotaryKnob
                      label="Low width"
                      value={lowWidth}
                      min={0}
                      max={2.0}
                      step={0.05}
                      defaultValue={0.6}
                      unit="×"
                      onChange={setLowWidth}
                    />
                    <RotaryKnob
                      label="Mid width"
                      value={midWidth}
                      min={0}
                      max={2.0}
                      step={0.05}
                      defaultValue={1.0}
                      unit="×"
                      onChange={setMidWidth}
                    />
                    <RotaryKnob
                      label="High width"
                      value={highWidth}
                      min={0}
                      max={2.0}
                      step={0.05}
                      defaultValue={1.2}
                      unit="×"
                      onChange={setHighWidth}
                    />
                  </div>

                  <div className="bg-[#06131d] border border-[#17374b] rounded-lg p-4 flex items-center justify-around">
                    <RotaryKnob
                      label="Low split"
                      value={lowSplitHz}
                      min={40}
                      max={400}
                      step={10}
                      defaultValue={150}
                      unit="Hz"
                      onChange={setLowSplitHz}
                    />
                    <RotaryKnob
                      label="High split"
                      value={highSplitHz}
                      min={1200}
                      max={8000}
                      step={100}
                      defaultValue={3200}
                      unit="Hz"
                      onChange={setHighSplitHz}
                    />
                    <RotaryKnob
                      label="Spread"
                      value={spreadAmount}
                      min={0}
                      max={1.0}
                      step={0.05}
                      defaultValue={0.15}
                      unit=""
                      onChange={setSpreadAmount}
                    />
                  </div>
                </div>
              </div>
            </div>
          )}
        </main>
      </div>
    </div>
  );
};
