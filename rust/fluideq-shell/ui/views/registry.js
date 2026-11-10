/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

// The pages the rail can show, in one place. A view is a descriptor with an
// id and whatever it needs to build itself; the shell decides when it is
// shown, so adding the DSP rack or Room does not mean another branch in
// app.js.

const views = new Map();

export function registerView(view) {
  if (!view?.id) {
    throw new Error('a view needs an id');
  }
  views.set(view.id, view);
  return view;
}

export function viewFor(id) {
  return views.get(id) ?? null;
}

export function viewIds() {
  return [...views.keys()];
}
