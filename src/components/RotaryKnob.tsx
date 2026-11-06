import React, { useRef } from 'react';

interface RotaryKnobProps {
  label: string;
  value: number;
  min: number;
  max: number;
  step?: number;
  defaultValue?: number;
  unit?: string;
  formatValue?: (val: number) => string;
  size?: number;
  onChange: (next: number) => void;
  accentColor?: string;
}

export const RotaryKnob: React.FC<RotaryKnobProps> = ({
  label,
  value,
  min,
  max,
  step = 0.1,
  defaultValue = 0,
  unit = 'dB',
  formatValue,
  size = 54,
  onChange,
  accentColor = '#00e5cf',
}) => {
  const dragStartY = useRef<number | null>(null);
  const dragStartVal = useRef<number>(value);

  const clamped = Math.max(min, Math.min(max, value));
  const ratio = max > min ? (clamped - min) / (max - min) : 0;

  // Sweep from -135 deg to +135 deg (total 270 deg)
  const startAngle = -135;
  const endAngle = 135;
  const currentAngle = startAngle + ratio * (endAngle - startAngle);

  const cx = size / 2;
  const cy = size / 2;
  const r = size / 2 - 6;

  const polarToCartesian = (angleDeg: number, radius: number) => {
    const rad = ((angleDeg - 90) * Math.PI) / 180;
    return {
      x: cx + radius * Math.cos(rad),
      y: cy + radius * Math.sin(rad),
    };
  };

  const describeArc = (fromDeg: number, toDeg: number, radius: number) => {
    if (Math.abs(toDeg - fromDeg) < 0.5) {
      return '';
    }
    const start = polarToCartesian(fromDeg, radius);
    const end = polarToCartesian(toDeg, radius);
    const largeArc = Math.abs(toDeg - fromDeg) > 180 ? 1 : 0;
    const sweep = toDeg > fromDeg ? 1 : 0;
    return `M ${start.x.toFixed(2)} ${start.y.toFixed(2)} A ${radius} ${radius} 0 ${largeArc} ${sweep} ${end.x.toFixed(2)} ${end.y.toFixed(2)}`;
  };

  const trackPath = describeArc(startAngle, endAngle, r);
  const zeroRatio = min < 0 && max > 0 ? (0 - min) / (max - min) : 0;
  const zeroAngle = startAngle + zeroRatio * (endAngle - startAngle);
  const activePath =
    min < 0 && max > 0
      ? describeArc(Math.min(zeroAngle, currentAngle), Math.max(zeroAngle, currentAngle), r)
      : describeArc(startAngle, currentAngle, r);

  const indicatorInner = polarToCartesian(currentAngle, r - 8);
  const indicatorOuter = polarToCartesian(currentAngle, r - 2);

  const displayText = formatValue
    ? formatValue(clamped)
    : `${clamped > 0 && min < 0 ? '+' : ''}${clamped.toFixed(step >= 1 ? 0 : 1)} ${unit}`;

  const handlePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    dragStartY.current = e.clientY;
    dragStartVal.current = clamped;
  };

  const handlePointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (dragStartY.current === null) return;
    const deltaY = dragStartY.current - e.clientY;
    const range = max - min;
    const rawNext = dragStartVal.current + (deltaY / 110) * range;
    const stepped = Math.round(rawNext / step) * step;
    const next = Math.max(min, Math.min(max, Number(stepped.toFixed(2))));
    onChange(next);
  };

  const handlePointerUp = () => {
    dragStartY.current = null;
  };

  return (
    <div className="flex flex-col items-center select-none gap-1">
      <span className="text-[10px] font-semibold tracking-wider uppercase text-[#8aa7b9]">
        {label}
      </span>
      <div
        role="slider"
        tabIndex={0}
        aria-label={label}
        aria-valuemin={min}
        aria-valuemax={max}
        aria-valuenow={clamped}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        onDoubleClick={() => onChange(defaultValue)}
        onKeyDown={(e) => {
          if (e.key === 'ArrowUp' || e.key === 'ArrowRight') {
            e.preventDefault();
            onChange(Math.min(max, Number((clamped + step).toFixed(2))));
          } else if (e.key === 'ArrowDown' || e.key === 'ArrowLeft') {
            e.preventDefault();
            onChange(Math.max(min, Number((clamped - step).toFixed(2))));
          } else if (e.key === 'Home') {
            e.preventDefault();
            onChange(defaultValue);
          }
        }}
        className="relative cursor-ns-resize focus:outline-none focus:ring-1 focus:ring-[#00e5cf] rounded-full"
        style={{ width: size, height: size }}
        title={`${label}: ${displayText} (Drag vertically or use arrow keys, double-click to reset)`}
      >
        <svg width={size} height={size} className="overflow-visible">
          <circle
            cx={cx}
            cy={cy}
            r={r - 3}
            fill="#071824"
            stroke="#17374b"
            strokeWidth="1"
          />
          {trackPath && (
            <path
              d={trackPath}
              fill="none"
              stroke="#153347"
              strokeWidth="3.2"
              strokeLinecap="round"
            />
          )}
          {activePath && (
            <path
              d={activePath}
              fill="none"
              stroke={accentColor}
              strokeWidth="3.2"
              strokeLinecap="round"
            />
          )}
          <line
            x1={indicatorInner.x}
            y1={indicatorInner.y}
            x2={indicatorOuter.x}
            y2={indicatorOuter.y}
            stroke={accentColor}
            strokeWidth="2.2"
            strokeLinecap="round"
          />
        </svg>
      </div>
      <span className="text-[11px] font-medium text-[#ecf5fb] feq-mono">
        {displayText}
      </span>
    </div>
  );
};
