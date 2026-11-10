/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

import { createGraph } from './graph.js';

// The EQ page. Its curve, its handles and its canvas live behind this
// descriptor; app.js keeps the shell (tray, engine status, volume) and asks
// the view for the face it draws.
export const equalizer = {
  id: 'equalizer',
  title: 'Equalizer',
  eyebrow: 'Parametric equalizer',
  createGraph,
};
