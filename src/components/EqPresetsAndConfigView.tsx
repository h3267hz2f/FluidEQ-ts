import React, { useEffect, useState } from 'react';
import {
  EqBand,
  evaluateBandCascadeDb,
  formatBandsAsApoFilterLines,
  opraBandsToEqBands,
  OpraRawBand,
  parseApoText,
} from '../utils/dspMath';
import {
  CONVOLUTION_CATALOGUE,
  ConvolutionImpulse,
  DEFAULT_SQUIGLINK_TEXT,
  DriverTypeProfile,
  OutputEndpoint,
  PresetDefinition,
} from '../utils/dspPresets';

interface OpraBrandSummary {
  slug: string;
  name: string;
}

interface OpraTargetEntry {
  gainDb?: number;
  bands?: OpraRawBand[];
}

type OpraModelMap = Record<string, Record<string, OpraTargetEntry>>;

interface MiniCurvePlotProps {
  freqGrid: readonly number[];
  curveDb: readonly number[];
  strokeColor?: string;
  compact?: boolean;
}

export const MiniCurvePlot: React.FC<MiniCurvePlotProps> = ({
  freqGrid,
  curveDb,
  strokeColor = '#c7a4eb',
  compact = false,
}) => {
  const width = 460;
  const height = compact ? 84 : 130;
  const minLog = Math.log10(20);
  const maxLog = Math.log10(20000);

  const points = freqGrid
    .map((f, idx) => {
      const clampedF = Math.max(20, Math.min(20000, f));
      const x = ((Math.log10(clampedF) - minLog) / (maxLog - minLog)) * width;
      const db = Math.max(-18, Math.min(18, curveDb[idx] ?? 0));
      const y = height / 2 - (db / 18) * (height / 2 - 10);
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(' ');

  return (
    <div className="w-full">
      <svg
        viewBox={`0 0 ${width} ${height}`}
        className={`w-full ${compact ? 'h-16' : 'h-32'} bg-[#06131e] rounded border border-[#153347]`}
      >
        <line
          x1={0}
          y1={height / 2}
          x2={width}
          y2={height / 2}
          stroke="rgba(36, 82, 109, 0.45)"
          strokeDasharray="3 3"
        />
        <polyline
          fill="none"
          stroke={strokeColor}
          strokeWidth="2.2"
          points={points}
        />
      </svg>
      <div className="flex justify-between text-[10px] text-[#7899ae] mt-1 px-1 feq-mono">
        <span>20</span>
        <span>100</span>
        <span>1k</span>
        <span>10k</span>
        <span>20k</span>
      </div>
    </div>
  );
};

interface EqPresetsViewProps {
  freqGrid: readonly number[];
  sampleRate: number;
  activeHeadphoneLabel: string;
  headphoneBands: readonly EqBand[];
  headphoneEnabled: boolean;
  onApplyHeadphoneCorrection: (label: string, bands: EqBand[]) => void;
  onApplyAsEditableEq: (bands: EqBand[]) => void;
}

export const EqPresetsSubTab: React.FC<EqPresetsViewProps> = ({
  freqGrid,
  sampleRate,
  activeHeadphoneLabel,
  headphoneBands,
  headphoneEnabled,
  onApplyHeadphoneCorrection,
  onApplyAsEditableEq,
}) => {
  const [brands, setBrands] = useState<OpraBrandSummary[]>([]);
  const [selectedBrand, setSelectedBrand] = useState<string>('apple');
  const [modelsData, setModelsData] = useState<OpraModelMap>({});
  const [selectedModelKey, setSelectedModelKey] = useState<string>('airpods_pro_2');
  const [selectedTargetKey, setSelectedTargetKey] = useState<string>('');
  const [squigText, setSquigText] = useState<string>(DEFAULT_SQUIGLINK_TEXT);
  const [statusBanner, setStatusBanner] = useState<string>('');

  useEffect(() => {
    let active = true;
    fetch('/api/opra/brands')
      .then((res) => res.json())
      .then((data: { brands?: OpraBrandSummary[] }) => {
        if (active && Array.isArray(data.brands)) {
          setBrands(data.brands);
        }
      })
      .catch((err) => {
        console.error('OPRA brands load error:', err);
      });
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    if (!selectedBrand) return;
    let active = true;
    fetch(`/api/opra/brand/${encodeURIComponent(selectedBrand)}`)
      .then((res) => res.json())
      .then((data: { models?: OpraModelMap }) => {
        if (!active || !data.models) return;
        setModelsData(data.models);
        const modelKeys = Object.keys(data.models);
        const nextModel = modelKeys.includes('airpods_pro_2')
          ? 'airpods_pro_2'
          : modelKeys[0] ?? '';
        setSelectedModelKey(nextModel);
        const targets = nextModel ? Object.keys(data.models[nextModel] ?? {}) : [];
        setSelectedTargetKey(targets[0] ?? '');
      })
      .catch((err) => {
        console.error('OPRA brand models load error:', err);
      });
    return () => {
      active = false;
    };
  }, [selectedBrand]);

  const modelKeys = Object.keys(modelsData);
  const targetMap = selectedModelKey ? modelsData[selectedModelKey] ?? {} : {};
  const targetKeys = Object.keys(targetMap);
  const currentTarget = selectedTargetKey ? targetMap[selectedTargetKey] : undefined;
  const previewOpraBands = currentTarget?.bands
    ? opraBandsToEqBands(currentTarget.bands)
    : [];

  const opraCurveDb = evaluateBandCascadeDb(
    previewOpraBands.length > 0 ? previewOpraBands : headphoneBands,
    freqGrid,
    sampleRate,
  );

  const parsedSquig = parseApoText(squigText);
  const squigCurveDb = evaluateBandCascadeDb(parsedSquig.bands, freqGrid, sampleRate);

  const formatKeyName = (raw: string) =>
    raw
      .replace(/^.*::/, '')
      .split('_')
      .map((w) => (w ? w[0].toUpperCase() + w.slice(1) : w))
      .join(' ');

  return (
    <div className="flex flex-col gap-4 p-4 overflow-y-auto h-full">
      {/* Top card: OPRA Headphone correction */}
      <section className="bg-[#091b29] border border-[#17374b] rounded-lg p-4">
        <div className="flex items-start justify-between gap-4 mb-3">
          <div>
            <p className="text-[10px] font-semibold tracking-widest uppercase text-[#00e5cf]">
              MATCH YOUR HEADPHONES
            </p>
            <h2 className="text-base font-semibold text-[#ecf5fb] mt-0.5">
              Headphone correction
            </h2>
            <p className="text-xs text-[#8aa7b9] mt-1 max-w-2xl">
              Say which headphones you are listening on and FluidEQ applies the correction published for them. It goes in as a layer of its own, with its own strength and its own switch, so your EQ bands are never touched.
            </p>
          </div>
          <span className="text-[11px] text-[#7fa1b5] shrink-0">
            OPRA library up to date — 6,229 models ({brands.length || 135} brand packs loaded)
          </span>
        </div>

        <div className="grid grid-cols-1 lg:grid-cols-2 gap-6 items-start mt-3">
          <div className="flex flex-col gap-3">
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
              <label className="flex flex-col gap-1 text-xs text-[#a5bdcb]">
                <span>Headphone brand</span>
                <select
                  aria-label="Headphone brand"
                  value={selectedBrand}
                  onChange={(e) => setSelectedBrand(e.target.value)}
                  className="bg-[#06131d] border border-[#1d4259] rounded px-2.5 py-1.5 text-xs text-[#ecf5fb] focus:outline-none focus:border-[#00e5cf]"
                >
                  {brands.map((b) => (
                    <option key={b.slug} value={b.slug}>
                      {b.name}
                    </option>
                  ))}
                </select>
              </label>

              <label className="flex flex-col gap-1 text-xs text-[#a5bdcb]">
                <span>Headphone model</span>
                <select
                  aria-label="Headphone model"
                  value={selectedModelKey}
                  onChange={(e) => {
                    const nextModel = e.target.value;
                    setSelectedModelKey(nextModel);
                    const nextTargets = Object.keys(modelsData[nextModel] ?? {});
                    setSelectedTargetKey(nextTargets[0] ?? '');
                  }}
                  className="bg-[#06131d] border border-[#1d4259] rounded px-2.5 py-1.5 text-xs text-[#ecf5fb] focus:outline-none focus:border-[#00e5cf]"
                >
                  {modelKeys.map((mKey) => (
                    <option key={mKey} value={mKey}>
                      {formatKeyName(mKey)}
                    </option>
                  ))}
                </select>
              </label>
            </div>

            <label className="flex flex-col gap-1 text-xs text-[#a5bdcb]">
              <span>Measurement / target</span>
              <select
                aria-label="Measurement / target"
                value={selectedTargetKey}
                onChange={(e) => setSelectedTargetKey(e.target.value)}
                className="bg-[#06131d] border border-[#1d4259] rounded px-2.5 py-1.5 text-xs text-[#ecf5fb] focus:outline-none focus:border-[#00e5cf]"
              >
                {targetKeys.map((tKey) => (
                  <option key={tKey} value={tKey}>
                    {formatKeyName(tKey)}
                  </option>
                ))}
              </select>
            </label>

            <div className="flex items-center gap-3 mt-1">
              <button
                type="button"
                onClick={() => {
                  if (previewOpraBands.length > 0) {
                    const label = `${formatKeyName(selectedBrand)} ${formatKeyName(selectedModelKey)} (${formatKeyName(selectedTargetKey)})`;
                    onApplyHeadphoneCorrection(label, previewOpraBands);
                    setStatusBanner(`Applied OPRA headphone correction: ${label}`);
                  }
                }}
                className="px-3.5 py-1.5 rounded bg-[#00e5cf] text-[#042326] font-semibold text-xs hover:bg-[#2bf0dc] transition-colors cursor-pointer"
              >
                Apply headset EQ
              </button>
              {activeHeadphoneLabel && (
                <span className="text-xs text-[#00e5cf]">
                  Active: {activeHeadphoneLabel} ({headphoneEnabled ? 'ON' : 'OFF'})
                </span>
              )}
            </div>
          </div>

          <div className="bg-[#071622] border border-[#153347] rounded-lg p-3">
            <div className="flex items-center justify-between mb-2">
              <span className="text-xs font-semibold text-[#ecf5fb]">
                Applied correction{' '}
                <span className="text-[#8aa7b9] font-normal">
                  {previewOpraBands.length} bands
                </span>
              </span>
              <span className="text-[11px] text-[#c7a4eb] feq-mono">
                {formatKeyName(selectedModelKey)}
              </span>
            </div>
            <MiniCurvePlot freqGrid={freqGrid} curveDb={opraCurveDb} strokeColor="#c7a4eb" />
          </div>
        </div>
      </section>

      {/* Bottom card: Guided Squiglink Import */}
      <section className="bg-[#091b29] border border-[#17374b] rounded-lg p-4">
        <p className="text-[10px] font-semibold tracking-widest uppercase text-[#00e5cf]">
          BRING YOUR CURVE WITH YOU
        </p>
        <h2 className="text-base font-semibold text-[#ecf5fb] mt-0.5">
          Import a Squiglink EQ
        </h2>
        <p className="text-xs text-[#8aa7b9] mt-1">
          Use Squiglink&apos;s calculator, then paste its export here. Apply as EQ replaces your editable bands. Apply as curve replaces the headphone correction, like OPRA, with its own strength and switch; your EQ bands stay unchanged.
        </p>

        {statusBanner && (
          <div
            role="status"
            className="mt-2 px-3 py-1.5 rounded bg-[#0e2e3d] border border-[#00e5cf]/50 text-xs text-[#00e5cf]"
          >
            {statusBanner}
          </div>
        )}

        <div className="grid grid-cols-1 lg:grid-cols-2 gap-6 mt-3">
          <div className="flex flex-col gap-2">
            <label htmlFor="squiglink-textarea" className="text-xs text-[#a5bdcb]">
              EQ import ({parsedSquig.bands.length} bands parsed · Preamp {parsedSquig.preampDb} dB)
            </label>
            <textarea
              id="squiglink-textarea"
              rows={8}
              value={squigText}
              onChange={(e) => setSquigText(e.target.value)}
              className="w-full bg-[#06131d] border border-[#1d4259] rounded p-2.5 text-xs text-[#ecf5fb] feq-mono focus:outline-none focus:border-[#00e5cf]"
            />
            <div className="flex items-center justify-end gap-2 mt-1">
              <button
                type="button"
                onClick={() => {
                  if (parsedSquig.bands.length > 0) {
                    onApplyAsEditableEq(parsedSquig.bands);
                    setStatusBanner(
                      `Loaded ${parsedSquig.bands.length} bands into Your Parametric EQ`,
                    );
                  }
                }}
                className="px-3 py-1.5 rounded border border-[#24526d] bg-[#0d2436] text-xs text-[#ecf5fb] hover:border-[#00e5cf] cursor-pointer"
              >
                Apply as EQ
              </button>
              <button
                type="button"
                onClick={() => {
                  if (parsedSquig.bands.length > 0) {
                    onApplyHeadphoneCorrection('Squiglink Custom Curve', parsedSquig.bands);
                    setStatusBanner(
                      `Applied Squiglink export (${parsedSquig.bands.length} bands) as Headphone Curve layer`,
                    );
                  }
                }}
                className="px-3 py-1.5 rounded bg-[#00e5cf] text-[#042326] font-semibold text-xs hover:bg-[#2bf0dc] cursor-pointer"
              >
                Apply as curve
              </button>
            </div>
          </div>

          <div className="bg-[#071622] border border-[#153347] rounded-lg p-3">
            <div className="flex items-center justify-between mb-2">
              <div>
                <span className="text-[10px] font-semibold tracking-wider uppercase text-[#c7a4eb]">
                  • APPLIED CURVE
                </span>
                <p className="text-xs font-semibold text-[#ecf5fb]">
                  Current EQ text ({parsedSquig.bands.length} bands · Parametric EQ)
                </p>
              </div>
              <button
                type="button"
                onClick={() => setSquigText(DEFAULT_SQUIGLINK_TEXT)}
                className="text-[11px] text-[#8aa7b9] hover:text-[#ecf5fb] cursor-pointer"
              >
                Reset sample
              </button>
            </div>
            <MiniCurvePlot freqGrid={freqGrid} curveDb={squigCurveDb} strokeColor="#c7a4eb" />
          </div>
        </div>
      </section>
    </div>
  );
};

interface ConvolutionSubTabProps {
  activeImpulse: ConvolutionImpulse | null;
  onSelectImpulse: (impulse: ConvolutionImpulse | null) => void;
}

export const ConvolutionSubTab: React.FC<ConvolutionSubTabProps> = ({
  activeImpulse,
  onSelectImpulse,
}) => {
  const [query, setQuery] = useState('');
  const filtered = CONVOLUTION_CATALOGUE.filter(
    (item) =>
      item.model.toLowerCase().includes(query.toLowerCase()) ||
      item.measuredBy.toLowerCase().includes(query.toLowerCase()),
  );

  return (
    <div className="p-4 flex flex-col gap-4 h-full overflow-y-auto">
      <section className="bg-[#091b29] border border-[#17374b] rounded-lg p-4">
        <div className="flex flex-wrap items-center justify-between gap-3 mb-3">
          <div>
            <p className="text-[10px] font-semibold tracking-widest uppercase text-[#00e5cf]">
              MINIMUM-PHASE IMPULSE LIBRARY
            </p>
            <h2 className="text-base font-semibold text-[#ecf5fb]">
              AutoEq Convolution Catalogue
            </h2>
            <p className="text-xs text-[#8aa7b9] mt-0.5">
              Each impulse matches the rate Windows runs the output at; the FluidEQ Engine converts any other rate itself.
            </p>
          </div>
          {activeImpulse && (
            <button
              type="button"
              onClick={() => onSelectImpulse(null)}
              className="px-3 py-1.5 rounded border border-[#24526d] bg-[#0d2436] text-xs text-[#ecf5fb] hover:border-[#00e5cf] cursor-pointer"
            >
              Unload impulse ({activeImpulse.model})
            </button>
          )}
        </div>

        <input
          type="text"
          aria-label="Search convolution models"
          placeholder="Search headphone model or measurement author (e.g. Sennheiser, oratory1990, crinacle)..."
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          className="w-full bg-[#06131d] border border-[#1d4259] rounded px-3 py-2 text-xs text-[#ecf5fb] mb-3 focus:outline-none focus:border-[#00e5cf]"
        />

        <div className="flex flex-col gap-2">
          {filtered.map((item) => {
            const isApplied = activeImpulse?.id === item.id;
            return (
              <div
                key={item.id}
                className={`flex items-center justify-between p-3 rounded border ${
                  isApplied
                    ? 'bg-[#0e2c3d] border-[#00e5cf]'
                    : 'bg-[#071622] border-[#153347]'
                }`}
              >
                <div>
                  <p className="text-xs font-semibold text-[#ecf5fb]">{item.model}</p>
                  <p className="text-[11px] text-[#8aa7b9] mt-0.5">
                    Measured by {item.measuredBy} · {item.rig} · {item.sampleRate}
                  </p>
                </div>
                <button
                  type="button"
                  onClick={() => onSelectImpulse(isApplied ? null : item)}
                  className={`px-3 py-1.5 rounded text-xs font-semibold cursor-pointer ${
                    isApplied
                      ? 'bg-[#17374b] text-[#00e5cf] border border-[#00e5cf]'
                      : 'bg-[#00e5cf] text-[#042326] hover:bg-[#2bf0dc]'
                  }`}
                >
                  {isApplied ? 'Active FIR' : 'Download & apply'}
                </button>
              </div>
            );
          })}
        </div>
      </section>
    </div>
  );
};

interface ConfigSubTabProps {
  outputs: readonly OutputEndpoint[];
  activeOutputId: string;
  onSelectOutput: (id: string) => void;
  preampDb: number;
  eqBands: readonly EqBand[];
  eqEnabled: boolean;
  driverProfile: DriverTypeProfile;
  driverEnabled: boolean;
  driverStrength: number;
  headphoneLabel: string;
  headphoneBands: readonly EqBand[];
  headphoneEnabled: boolean;
  headphoneStrength: number;
  activePreset: PresetDefinition;
  presetEnabled: boolean;
  presetStrength: number;
}

export const ConfigSubTab: React.FC<ConfigSubTabProps> = ({
  outputs,
  activeOutputId,
  onSelectOutput,
  preampDb,
  eqBands,
  eqEnabled,
  driverProfile,
  driverEnabled,
  driverStrength,
  headphoneLabel,
  headphoneBands,
  headphoneEnabled,
  headphoneStrength,
  activePreset,
  presetEnabled,
  presetStrength,
}) => {
  const activeOutput = outputs.find((o) => o.id === activeOutputId) ?? outputs[0];
  const slug = activeOutput.slug;

  const driverLines = formatBandsAsApoFilterLines(driverProfile.bands, driverStrength / 100);
  const hpLines = formatBandsAsApoFilterLines(headphoneBands, headphoneStrength / 100);
  const presetLines = formatBandsAsApoFilterLines(activePreset.bands, presetStrength / 100);
  const eqLines = formatBandsAsApoFilterLines(eqBands, 1.0);

  const totalFilters =
    (driverEnabled ? driverLines.length : 0) +
    (headphoneEnabled ? hpLines.length : 0) +
    (presetEnabled ? presetLines.length : 0) +
    (eqEnabled ? eqLines.length : 0);

  return (
    <div className="p-4 flex flex-col gap-4 h-full overflow-y-auto">
      <section className="bg-[#091b29] border border-[#17374b] rounded-lg p-4">
        <div className="flex items-center justify-between mb-3">
          <div>
            <p className="text-[10px] font-semibold tracking-widest uppercase text-[#8aa7b9]">
              WHAT THE ENGINE READS
            </p>
            <h2 className="text-base font-semibold text-[#ecf5fb]">
              FluidEQ Engine config
            </h2>
            <p className="text-xs text-[#8aa7b9]">
              What is on disk right now, not what FluidEQ intends.
            </p>
          </div>
          <span className="px-3 py-1 rounded-full bg-[#0b2a37] border border-[#00e5cf]/50 text-xs text-[#00e5cf]">
            • Active — the FluidEQ Engine is applying this config
          </span>
        </div>

        {/* Output endpoint cards */}
        <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-2.5 mb-4">
          {outputs.map((out) => {
            const isCurrent = out.id === activeOutput.id;
            return (
              <button
                key={out.id}
                type="button"
                onClick={() => onSelectOutput(out.id)}
                className={`text-left p-2.5 rounded border transition-colors cursor-pointer ${
                  isCurrent
                    ? 'bg-[#0e2e40] border-[#00e5cf]'
                    : 'bg-[#071622] border-[#17374b] hover:border-[#24526d]'
                }`}
              >
                <p className="text-xs font-semibold text-[#ecf5fb] truncate">
                  {out.name}
                </p>
                <p className="text-[11px] text-[#8aa7b9] truncate mt-0.5">
                  {out.activeProfile}
                </p>
                {isCurrent && (
                  <p className="text-[10px] font-semibold text-[#00e5cf] uppercase mt-1">
                    PLAYING NOW · {totalFilters} filters
                  </p>
                )}
              </button>
            );
          })}
        </div>

        {/* Include tree inspector */}
        <div className="bg-[#06131d] border border-[#17374b] rounded-lg p-3.5 flex flex-col gap-3">
          <div className="flex items-center justify-between border-b border-[#153347] pb-2">
            <div>
              <p className="text-xs font-semibold text-[#ecf5fb]">
                {activeOutput.name}{' '}
                <span className="text-[#6c8ea3] font-normal feq-mono">
                  {activeOutput.id}
                </span>
              </p>
              <p className="text-[11px] text-[#8aa7b9] mt-0.5 feq-mono">
                {totalFilters} filters · Preamp: {preampDb.toFixed(1)} dB
              </p>
            </div>
            <span className="text-[11px] text-[#00e5cf] feq-mono">
              fluideq-device-{slug}.txt
            </span>
          </div>

          <pre className="text-[11px] text-[#a5bdcb] feq-mono bg-[#051018] p-2.5 rounded border border-[#122a3a] overflow-x-auto">
{`# ${activeOutput.name} -> ${activeOutput.activeProfile}
Preamp: ${preampDb.toFixed(1)} dB
# FluidEQAutoPreamp: ON
# FluidEQCurveStage: ON`}
          </pre>

          {/* Driver layer */}
          <div className="border-l-2 border-[#00e5cf]/50 pl-3 flex flex-col gap-1.5">
            <div className="flex items-center justify-between">
              <span className="text-xs font-semibold text-[#ecf5fb] feq-mono">
                fluideq-{slug}-driver.txt{' '}
                <span className="ml-2 px-2 py-0.5 rounded bg-[#0d2738] text-[10px] text-[#00e5cf]">
                  driver {driverEnabled ? 'ON' : 'OFF'}
                </span>
              </span>
              <span className="text-[11px] text-[#8aa7b9] feq-mono">
                {driverLines.length} filter{driverLines.length === 1 ? '' : 's'}
              </span>
            </div>
            <pre className="text-[11px] text-[#9ec0d4] feq-mono bg-[#051018] p-2.5 rounded border border-[#122a3a] overflow-x-auto">
{[
  `# driver: ${activeOutput.name} -> ${driverProfile.name}`,
  '# FluidEQEqLayer: ON',
  '# FluidEQFilterDesign: MATCHED',
  ...driverLines,
].join('\n')}
            </pre>
          </div>

          {/* Headphone OPRA layer */}
          <div className="border-l-2 border-[#c7a4eb]/60 pl-3 flex flex-col gap-1.5">
            <div className="flex items-center justify-between">
              <span className="text-xs font-semibold text-[#ecf5fb] feq-mono">
                fluideq-{slug}-headphone.txt{' '}
                <span className="ml-2 px-2 py-0.5 rounded bg-[#1f1938] text-[10px] text-[#c7a4eb]">
                  headphone {headphoneEnabled ? 'ON' : 'OFF'}
                </span>
              </span>
              <span className="text-[11px] text-[#8aa7b9] feq-mono">
                {hpLines.length} filters
              </span>
            </div>
            <pre className="text-[11px] text-[#9ec0d4] feq-mono bg-[#051018] p-2.5 rounded border border-[#122a3a] overflow-x-auto">
{[
  `# headphone: ${headphoneLabel}`,
  '# FluidEQCurveLayer: ON',
  '# FluidEQFilterDesign: MATCHED',
  ...hpLines,
].join('\n')}
            </pre>
          </div>

          {/* Preset Voicing layer */}
          <div className="border-l-2 border-[#d977f2]/60 pl-3 flex flex-col gap-1.5">
            <div className="flex items-center justify-between">
              <span className="text-xs font-semibold text-[#ecf5fb] feq-mono">
                fluideq-{slug}-preset.txt{' '}
                <span className="ml-2 px-2 py-0.5 rounded bg-[#281538] text-[10px] text-[#e599f7]">
                  preset {presetEnabled ? 'ON' : 'OFF'} ({activePreset.name})
                </span>
              </span>
              <span className="text-[11px] text-[#8aa7b9] feq-mono">
                {presetLines.length} filters
              </span>
            </div>
            <pre className="text-[11px] text-[#9ec0d4] feq-mono bg-[#051018] p-2.5 rounded border border-[#122a3a] overflow-x-auto">
{[
  `# voicing: ${activeOutput.name} -> ${activePreset.name}`,
  '# FluidEQEqLayer: ON',
  '# FluidEQFilterDesign: MATCHED',
  ...presetLines,
].join('\n')}
            </pre>
          </div>
        </div>
      </section>
    </div>
  );
};
