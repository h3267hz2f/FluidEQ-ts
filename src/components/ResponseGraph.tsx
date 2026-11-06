import React, { useCallback, useEffect, useRef } from 'react';
import { computeTieredCanvasDpr, EqBand } from '../utils/dspMath';

export type GraphLookId =
  | 'rainbow-bars'
  | 'analyzer'
  | 'synthwave'
  | 'glass-towers'
  | 'led-wall'
  | 'horizon';

interface ResponseGraphProps {
  freqGrid: readonly number[];
  eqCurveDb: readonly number[];
  toneCurveDb: readonly number[];
  presetCurveDb: readonly number[];
  headphoneCurveDb: readonly number[];
  driverCurveDb: readonly number[];
  combinedCurveDb: readonly number[];
  bands: readonly EqBand[];
  selectedBandIndex: number;
  onSelectBand: (index: number) => void;
  onUpdateBand: (index: number, patch: Partial<EqBand>) => void;
  look: GraphLookId;
  isPlaying: boolean;
  systemEqEnabled: boolean;
  showResponseGraph: boolean;
}

const FREQ_TICKS = [10, 20, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000];
const DB_TICKS = [20, 10, 0, -10, -20];
const BAR_COUNT = 28;

function freqToX(freq: number, width: number, padLeft: number, padRight: number): number {
  const minLog = Math.log10(10);
  const maxLog = Math.log10(20000);
  const clamped = Math.max(10, Math.min(20000, freq));
  const t = (Math.log10(clamped) - minLog) / (maxLog - minLog);
  return padLeft + t * (width - padLeft - padRight);
}

function xToFreq(x: number, width: number, padLeft: number, padRight: number): number {
  const minLog = Math.log10(10);
  const maxLog = Math.log10(20000);
  const plotW = Math.max(1, width - padLeft - padRight);
  const t = Math.max(0, Math.min(1, (x - padLeft) / plotW));
  return Math.round(Math.pow(10, minLog + t * (maxLog - minLog)));
}

function dbToY(db: number, height: number, padTop: number, padBottom: number): number {
  const clamped = Math.max(-24, Math.min(24, db));
  const t = (24 - clamped) / 48;
  return padTop + t * (height - padTop - padBottom);
}

function yToDb(y: number, height: number, padTop: number, padBottom: number): number {
  const plotH = Math.max(1, height - padTop - padBottom);
  const t = Math.max(0, Math.min(1, (y - padTop) / plotH));
  const db = 24 - t * 48;
  return Math.round(Math.max(-20, Math.min(20, db)) * 10) / 10;
}

interface OffscreenBarAtlas {
  canvas: HTMLCanvasElement;
  width: number;
  height: number;
  look: GraphLookId;
}

/**
 * Pre-bakes a full-height 28-bar gradient/LED sprite sheet onto an offscreen canvas
 * once per resize or look change, eliminating per-frame gradient allocations.
 */
function buildOffscreenBarAtlas(
  width: number,
  height: number,
  look: GraphLookId,
): OffscreenBarAtlas | null {
  if (typeof document === 'undefined') return null;
  const off = document.createElement('canvas');
  off.width = Math.max(64, Math.floor(width));
  off.height = Math.max(64, Math.floor(height));
  const ctx = off.getContext('2d');
  if (!ctx) return null;

  const padLeft = 44;
  const padRight = 44;
  const padTop = 20;
  const padBottom = 26;
  const plotW = Math.max(10, width - padLeft - padRight);
  const plotH = Math.max(10, height - padTop - padBottom);
  const barSlotW = plotW / BAR_COUNT;
  const barW = Math.max(4, barSlotW - 5);

  for (let i = 0; i < BAR_COUNT; i += 1) {
    const ratio = i / (BAR_COUNT - 1);
    const x = padLeft + i * barSlotW + 2.5;
    const rCol = Math.round(18 + ratio * 58);
    const gCol = Math.round(145 - ratio * 55);
    const bCol = Math.round(136 + ratio * 38);

    if (look === 'led-wall') {
      const segCount = 14;
      const segH = (plotH - segCount * 2) / segCount;
      for (let s = 0; s < segCount; s += 1) {
        const sy = height - padBottom - (s + 1) * (segH + 2);
        ctx.fillStyle = `rgba(${rCol}, ${gCol}, ${bCol}, 0.84)`;
        ctx.fillRect(x, sy, barW, segH);
      }
    } else {
      const barGrad = ctx.createLinearGradient(x, padTop, x, height - padBottom);
      barGrad.addColorStop(0, `rgba(${rCol + 26}, ${gCol + 32}, ${bCol + 28}, 0.86)`);
      barGrad.addColorStop(1, `rgba(${rCol}, ${gCol}, ${bCol}, 0.44)`);
      ctx.fillStyle = barGrad;
      ctx.fillRect(x, padTop, barW, plotH);
    }
  }

  return { canvas: off, width, height, look };
}

export const ResponseGraph: React.FC<ResponseGraphProps> = ({
  freqGrid,
  eqCurveDb,
  toneCurveDb,
  presetCurveDb,
  headphoneCurveDb,
  driverCurveDb,
  combinedCurveDb,
  bands,
  selectedBandIndex,
  onSelectBand,
  onUpdateBand,
  look,
  isPlaying,
  systemEqEnabled,
  showResponseGraph,
}) => {
  // Bottom layer: dynamic real-time spectrum bars (1x–1.25x DPR, 0 FPS when silent/paused)
  const spectrumCanvasRef = useRef<HTMLCanvasElement | null>(null);
  // Top layer: static interactive EQ curves & handles (up to 2x DPR, 0 FPS unless edited)
  const curveCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const barAtlasRef = useRef<OffscreenBarAtlas | null>(null);
  const draggingBandRef = useRef<number | null>(null);
  const peakHoldRef = useRef<number[]>(Array.from({ length: 32 }, () => 0.15));

  const dynamicStateRef = useRef({
    combinedCurveDb,
    look,
    isPlaying,
  });
  dynamicStateRef.current = {
    combinedCurveDb,
    look,
    isPlaying,
  };

  // Repaint the static top layer ONLY when curves, bands, selection, or visibility change
  const repaintStaticCurveLayer = useCallback(() => {
    const canvas = curveCanvasRef.current;
    if (!canvas) return;
    const rect = canvas.getBoundingClientRect();
    const { curveDpr } = computeTieredCanvasDpr(window.devicePixelRatio || 1);
    const targetW = Math.max(300, Math.floor(rect.width * curveDpr));
    const targetH = Math.max(180, Math.floor(rect.height * curveDpr));
    if (canvas.width !== targetW || canvas.height !== targetH) {
      canvas.width = targetW;
      canvas.height = targetH;
    }
    const ctx = canvas.getContext('2d');
    if (!ctx) return;
    ctx.save();
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.scale(curveDpr, curveDpr);
    drawStaticCurveLayer(ctx, rect.width, rect.height, {
      freqGrid,
      eqCurveDb,
      toneCurveDb,
      presetCurveDb,
      headphoneCurveDb,
      driverCurveDb,
      combinedCurveDb,
      bands,
      selectedBandIndex,
      systemEqEnabled,
      showResponseGraph,
    });
    ctx.restore();
  }, [
    freqGrid,
    eqCurveDb,
    toneCurveDb,
    presetCurveDb,
    headphoneCurveDb,
    driverCurveDb,
    combinedCurveDb,
    bands,
    selectedBandIndex,
    systemEqEnabled,
    showResponseGraph,
  ]);

  useEffect(() => {
    repaintStaticCurveLayer();
    const handleResize = () => repaintStaticCurveLayer();
    window.addEventListener('resize', handleResize);
    return () => window.removeEventListener('resize', handleResize);
  }, [repaintStaticCurveLayer]);

  // Dynamic spectrum layer loop: runs ONLY while `isPlaying` is true and document is visible.
  // When paused/silent or hidden, draws one settled frame and stops `requestAnimationFrame` (0 FPS).
  useEffect(() => {
    let rafId = 0;
    let lastDrawTime = -1;

    const paintSpectrumOnce = (timestamp: number) => {
      const canvas = spectrumCanvasRef.current;
      if (!canvas) return;
      const rect = canvas.getBoundingClientRect();
      const { spectrumDpr } = computeTieredCanvasDpr(window.devicePixelRatio || 1);
      const targetW = Math.max(300, Math.floor(rect.width * spectrumDpr));
      const targetH = Math.max(180, Math.floor(rect.height * spectrumDpr));
      if (canvas.width !== targetW || canvas.height !== targetH) {
        canvas.width = targetW;
        canvas.height = targetH;
      }

      const currentAtlas = barAtlasRef.current;
      if (
        !currentAtlas ||
        Math.abs(currentAtlas.width - rect.width) > 1 ||
        Math.abs(currentAtlas.height - rect.height) > 1 ||
        currentAtlas.look !== dynamicStateRef.current.look
      ) {
        barAtlasRef.current = buildOffscreenBarAtlas(
          rect.width,
          rect.height,
          dynamicStateRef.current.look,
        );
      }

      const ctx = canvas.getContext('2d');
      if (ctx) {
        ctx.save();
        ctx.scale(spectrumDpr, spectrumDpr);
        drawDynamicSpectrumLayer(
          ctx,
          rect.width,
          rect.height,
          timestamp / 1000,
          dynamicStateRef.current,
          peakHoldRef.current,
          barAtlasRef.current,
        );
        ctx.restore();
      }
    };

    // Always draw one immediate frame when `isPlaying` or `look` toggles
    paintSpectrumOnce(performance.now());

    // Zero-FPS auto-sleep when audio is paused/silent
    if (!isPlaying) {
      return undefined;
    }

    const stepLoop = (timestamp: number) => {
      if (typeof document !== 'undefined' && document.visibilityState === 'hidden') {
        rafId = window.requestAnimationFrame(stepLoop);
        return;
      }
      if (timestamp - lastDrawTime >= 160) {
        lastDrawTime = timestamp;
        paintSpectrumOnce(timestamp);
      }
      rafId = window.requestAnimationFrame(stepLoop);
    };

    rafId = window.requestAnimationFrame(stepLoop);
    return () => {
      window.cancelAnimationFrame(rafId);
    };
  }, [isPlaying, look]);

  const handlePointerDown = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const canvas = curveCanvasRef.current;
    if (!canvas) return;
    const rect = canvas.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;
    const padLeft = 44;
    const padRight = 44;
    const padTop = 20;
    const padBottom = 26;

    let bestIdx = -1;
    let bestDist = 18;
    bands.forEach((b, idx) => {
      const bx = freqToX(b.frequency, rect.width, padLeft, padRight);
      const by = dbToY(b.gainDb, rect.height, padTop, padBottom);
      const dist = Math.hypot(x - bx, y - by);
      if (dist < bestDist) {
        bestDist = dist;
        bestIdx = idx;
      }
    });

    if (bestIdx !== -1) {
      e.currentTarget.setPointerCapture(e.pointerId);
      draggingBandRef.current = bestIdx;
      onSelectBand(bestIdx);
    }
  };

  const handlePointerMove = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const idx = draggingBandRef.current;
    if (idx === null) return;
    const canvas = curveCanvasRef.current;
    if (!canvas) return;
    const rect = canvas.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;
    const padLeft = 44;
    const padRight = 44;
    const padTop = 20;
    const padBottom = 26;

    const nextGain = yToDb(y, rect.height, padTop, padBottom);
    const nextFreq = xToFreq(x, rect.width, padLeft, padRight);
    onUpdateBand(idx, { gainDb: nextGain, frequency: nextFreq });
  };

  const handlePointerUp = () => {
    draggingBandRef.current = null;
  };

  return (
    <div className="relative w-full h-full select-none">
      {/* Layer 1: Dynamic Spectrum Canvas (Tiered 1x–1.25x DPR, Pre-baked Sprite Blit, Silence 0-FPS Gate) */}
      <canvas
        ref={spectrumCanvasRef}
        data-testid="eq-spectrum-canvas"
        aria-hidden="true"
        className="absolute inset-0 w-full h-full block pointer-events-none"
      />
      {/* Layer 2: Static Interactive Curve & Handles Canvas (Crisp High-DPI, Event-Driven 0-FPS at Rest) */}
      <canvas
        ref={curveCanvasRef}
        data-testid="eq-response-canvas"
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        className="relative z-10 w-full h-full block cursor-crosshair"
      />
    </div>
  );
};

function drawDynamicSpectrumLayer(
  ctx: CanvasRenderingContext2D,
  width: number,
  height: number,
  tSec: number,
  state: {
    combinedCurveDb: readonly number[];
    look: GraphLookId;
    isPlaying: boolean;
  },
  peaks: number[],
  atlas: OffscreenBarAtlas | null,
) {
  const padLeft = 44;
  const padRight = 44;
  const padTop = 20;
  const padBottom = 26;
  const plotW = Math.max(10, width - padLeft - padRight);
  const plotH = Math.max(10, height - padTop - padBottom);

  // Studio background gradient
  const bgGrad = ctx.createLinearGradient(0, 0, 0, height);
  if (state.look === 'synthwave') {
    bgGrad.addColorStop(0, '#0c0822');
    bgGrad.addColorStop(0.65, '#1b0d38');
    bgGrad.addColorStop(1, '#081320');
  } else {
    bgGrad.addColorStop(0, '#061521');
    bgGrad.addColorStop(1, '#041019');
  }
  ctx.fillStyle = bgGrad;
  ctx.fillRect(0, 0, width, height);

  if (state.look === 'synthwave') {
    const sunX = padLeft + plotW * 0.5;
    const sunY = padTop + plotH * 0.52;
    const sunGrad = ctx.createLinearGradient(sunX, sunY - 48, sunX, sunY + 48);
    sunGrad.addColorStop(0, '#ff4fd8');
    sunGrad.addColorStop(1, '#ffb347');
    ctx.fillStyle = sunGrad;
    ctx.beginPath();
    ctx.arc(sunX, sunY, 44, 0, Math.PI * 2);
    ctx.fill();
  }

  const barSlotW = plotW / BAR_COUNT;
  const barW = Math.max(4, barSlotW - 5);

  if (state.look === 'analyzer') {
    ctx.beginPath();
    for (let i = 0; i < BAR_COUNT; i += 1) {
      const ratio = i / (BAR_COUNT - 1);
      const eqBoost =
        state.combinedCurveDb[
          Math.min(
            state.combinedCurveDb.length - 1,
            Math.floor(ratio * state.combinedCurveDb.length),
          )
        ] || 0;
      const baseEnergy = state.isPlaying
        ? 0.42 +
          0.25 * Math.sin(tSec * 3.8 + i * 0.45) +
          0.16 * Math.cos(tSec * 6.5 - i * 0.3) +
          (1 - ratio * 0.45) * 0.22 +
          eqBoost * 0.014
        : 0.08;
      const clamped = Math.max(0.05, Math.min(0.95, baseEnergy));
      if (clamped > peaks[i]) {
        peaks[i] = clamped;
      } else {
        peaks[i] = Math.max(clamped, peaks[i] - 0.004);
      }
      const x = padLeft + ratio * plotW;
      const y = height - padBottom - clamped * plotH;
      if (i === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    }
    ctx.lineTo(padLeft + plotW, height - padBottom);
    ctx.lineTo(padLeft, height - padBottom);
    ctx.closePath();
    const specGrad = ctx.createLinearGradient(padLeft, 0, padLeft + plotW, 0);
    specGrad.addColorStop(0, 'rgba(16, 185, 145, 0.38)');
    specGrad.addColorStop(0.55, 'rgba(18, 132, 185, 0.38)');
    specGrad.addColorStop(1, 'rgba(124, 92, 214, 0.38)');
    ctx.fillStyle = specGrad;
    ctx.fill();
    return;
  }

  // Blit pre-baked offscreen bar atlas slices (zero per-frame gradient allocations)
  for (let i = 0; i < BAR_COUNT; i += 1) {
    const ratio = i / (BAR_COUNT - 1);
    const eqBoost =
      state.combinedCurveDb[
        Math.min(
          state.combinedCurveDb.length - 1,
          Math.floor(ratio * state.combinedCurveDb.length),
        )
      ] || 0;
    const baseEnergy = state.isPlaying
      ? 0.44 +
        0.24 * Math.sin(tSec * 4.1 + i * 0.52) +
        0.14 * Math.cos(tSec * 7.2 - i * 0.25) +
        Math.sin(i * 0.35) * 0.15 +
        eqBoost * 0.015
      : 0.08;
    const clamped = Math.max(0.06, Math.min(0.94, baseEnergy));
    if (clamped > peaks[i]) {
      peaks[i] = clamped;
    } else {
      peaks[i] = Math.max(clamped, peaks[i] - 0.003);
    }

    const x = padLeft + i * barSlotW + 2.5;
    const barH = Math.max(2, clamped * plotH);
    const y = height - padBottom - barH;

    if (atlas) {
      ctx.drawImage(atlas.canvas, x, y, barW, barH, x, y, barW, barH);
    } else {
      ctx.fillStyle = 'rgba(0, 229, 207, 0.65)';
      ctx.fillRect(x, y, barW, barH);
    }

    const peakY = height - padBottom - peaks[i] * plotH - 4;
    ctx.fillStyle = 'rgba(180, 212, 230, 0.65)';
    ctx.fillRect(x, Math.max(padTop, peakY), barW, 1.5);
  }
}

function drawStaticCurveLayer(
  ctx: CanvasRenderingContext2D,
  width: number,
  height: number,
  state: {
    freqGrid: readonly number[];
    eqCurveDb: readonly number[];
    toneCurveDb: readonly number[];
    presetCurveDb: readonly number[];
    headphoneCurveDb: readonly number[];
    driverCurveDb: readonly number[];
    combinedCurveDb: readonly number[];
    bands: readonly EqBand[];
    selectedBandIndex: number;
    systemEqEnabled: boolean;
    showResponseGraph: boolean;
  },
) {
  const padLeft = 44;
  const padRight = 44;
  const padTop = 20;
  const padBottom = 26;

  // Grid lines & axis ticks
  ctx.lineWidth = 1;
  ctx.strokeStyle = 'rgba(36, 82, 109, 0.32)';
  ctx.fillStyle = '#6c8ea3';
  ctx.font = '10px -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif';

  for (const freq of FREQ_TICKS) {
    const x = freqToX(freq, width, padLeft, padRight);
    ctx.beginPath();
    ctx.moveTo(x, padTop);
    ctx.lineTo(x, height - padBottom);
    ctx.stroke();

    const label = freq >= 1000 ? `${freq / 1000}k Hz` : `${freq} Hz`;
    ctx.textAlign = 'center';
    ctx.fillText(label, x, height - 8);
  }

  for (const db of DB_TICKS) {
    const y = dbToY(db, height, padTop, padBottom);
    ctx.beginPath();
    ctx.strokeStyle = db === 0 ? 'rgba(0, 229, 207, 0.35)' : 'rgba(36, 82, 109, 0.28)';
    ctx.moveTo(padLeft, y);
    ctx.lineTo(width - padRight, y);
    ctx.stroke();

    ctx.textAlign = 'right';
    ctx.fillStyle = '#7fa1b5';
    ctx.fillText(`${db > 0 ? `+${db}` : db} dB`, padLeft - 6, y + 3);

    const analyzerDb = Math.round(((db - 20) / 40) * 60);
    ctx.textAlign = 'left';
    ctx.fillStyle = '#5c7c90';
    ctx.fillText(`${analyzerDb} dB`, width - padRight + 6, y + 3);
  }

  if (!state.showResponseGraph) {
    return;
  }

  const drawCurve = (
    curveDb: readonly number[],
    strokeStyle: string,
    lineWidth: number,
    dashed = false,
  ) => {
    if (curveDb.length === 0) return;
    ctx.save();
    ctx.beginPath();
    if (dashed) {
      ctx.setLineDash([4, 4]);
    }
    for (let i = 0; i < state.freqGrid.length; i += 1) {
      const fx = freqToX(state.freqGrid[i], width, padLeft, padRight);
      const fy = dbToY(curveDb[i] ?? 0, height, padTop, padBottom);
      if (i === 0) ctx.moveTo(fx, fy);
      else ctx.lineTo(fx, fy);
    }
    ctx.strokeStyle = strokeStyle;
    ctx.lineWidth = lineWidth;
    ctx.stroke();
    ctx.restore();
  };

  drawCurve(state.driverCurveDb, 'rgba(96, 165, 250, 0.65)', 1.3);
  drawCurve(state.headphoneCurveDb, 'rgba(199, 164, 235, 0.85)', 1.6);
  drawCurve(state.presetCurveDb, 'rgba(217, 119, 242, 0.88)', 1.7);
  drawCurve(state.toneCurveDb, 'rgba(184, 242, 86, 0.85)', 1.6);
  drawCurve(state.combinedCurveDb, 'rgba(236, 245, 251, 0.55)', 1.4);

  const eqColor = state.systemEqEnabled ? '#00e5cf' : '#5d7c90';
  drawCurve(state.eqCurveDb, eqColor, 2.4);

  state.bands.forEach((band, idx) => {
    const bx = freqToX(band.frequency, width, padLeft, padRight);
    const by = dbToY(band.gainDb, height, padTop, padBottom);
    const isSelected = idx === state.selectedBandIndex;
    const hasGain = Math.abs(band.gainDb) >= 0.1;

    ctx.save();
    if (isSelected || hasGain) {
      if (isSelected) {
        ctx.beginPath();
        ctx.arc(bx, by, 11, 0, Math.PI * 2);
        ctx.fillStyle = 'rgba(0, 229, 207, 0.18)';
        ctx.fill();
      }
      ctx.beginPath();
      ctx.arc(bx, by, isSelected ? 7 : 5.5, 0, Math.PI * 2);
      ctx.fillStyle = isSelected ? '#00e5cf' : '#092334';
      ctx.fill();
      ctx.lineWidth = 1.8;
      ctx.strokeStyle = '#00e5cf';
      ctx.stroke();

      ctx.font = 'bold 9px -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif';
      ctx.fillStyle = isSelected ? '#042326' : '#00e5cf';
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      ctx.fillText(String(idx + 1), bx, by + 0.5);
    } else {
      ctx.strokeStyle = 'rgba(0, 229, 207, 0.55)';
      ctx.fillStyle = '#071b29';
      ctx.lineWidth = 1.2;
      ctx.beginPath();
      ctx.roundRect(bx - 5, by - 5, 10, 10, 2.5);
      ctx.fill();
      ctx.stroke();

      ctx.beginPath();
      ctx.arc(bx, by, 1.8, 0, Math.PI * 2);
      ctx.fillStyle = 'rgba(0, 229, 207, 0.75)';
      ctx.fill();
    }
    ctx.restore();
  });
}
