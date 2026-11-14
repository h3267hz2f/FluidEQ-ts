/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

// The Share page: a second output played by the native capture helper, which
// taps the system mix before endpoint effects. Nothing here guesses at what is
// running — the helper's own replies say which device opened, how far behind
// it is, and why it stopped.
export const share = {
  id: 'share',
  title: 'Share',
  eyebrow: 'Second output',
};
