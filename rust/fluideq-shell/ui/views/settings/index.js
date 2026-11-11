/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

// The Settings page: the preferences that outlive the window. The shell still
// owns the commands; this view only says which page is which, so the settings
// markup lives with the rest of the settings and not in app.js.
export const settings = {
  id: 'settings',
  title: 'Settings',
  eyebrow: 'Preferences',
};
