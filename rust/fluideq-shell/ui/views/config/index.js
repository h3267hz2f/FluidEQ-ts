/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

// The Config page: what Windows has attached, what FluidEQ holds for each
// output, and the engine's own verdicts — read from the same status and
// snapshot the rest of the shell already has, so it never becomes a second
// description of the audio path.
export const config = {
  id: 'config',
  title: 'Config',
  eyebrow: 'Engine config',
};
