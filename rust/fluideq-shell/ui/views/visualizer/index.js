/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

// The Visualizer page: a full-width view of the mix the engine reports,
// mirrored around the middle so level reads at a glance. The canvas is drawn
// from the same `spectrum` event the equalizer uses; the page owns no state.
export const visualizer = {
  id: 'visualizer',
  title: 'Visualizer',
  eyebrow: 'Live spectrum',
};
