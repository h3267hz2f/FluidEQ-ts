/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

export const room = {
  id: 'room',
  title: 'Binaural room',
  eyebrow: 'Spatial acoustic simulation',
};

export const ROOM_PRESETS = [
  { id: 0, label: 'Studio', detail: 'Treated control room · 4.5 m · dry' },
  { id: 1, label: 'Stage', detail: 'Medium performance hall · 8.0 m · natural' },
  { id: 2, label: 'Cinema', detail: 'Commercial theater · 12.0 m · deep immersion' },
  { id: 3, label: 'Arena', detail: 'Large indoor stadium · 18.0 m · spacious reverberation' },
  { id: 4, label: 'Club', detail: 'Intimate listening lounge · 6.5 m · punchy bass' },
];

export const KEMAR_HEADS = [
  { id: 0, label: 'Small', detail: '54 cm head circumference' },
  { id: 1, label: 'Medium', detail: '56 cm standard KEMAR' },
  { id: 2, label: 'Large', detail: '58 cm head circumference' },
];
