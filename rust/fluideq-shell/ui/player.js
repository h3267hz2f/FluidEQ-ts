/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

// The compact player is the transport bar in its own window: the same media
// events, the same four commands, the same volume. It is deliberately a
// separate page, so the main window's canvas, views and state are not loaded
// twice just to show one row of text.

const TAURI = window.__TAURI__;
const invoke = TAURI?.core?.invoke;
const listen = TAURI?.event?.listen;

const els = {
  cover: document.getElementById('player-cover'),
  title: document.getElementById('player-title'),
  artist: document.getElementById('player-artist'),
  time: document.getElementById('player-time'),
  play: document.getElementById('player-play'),
  previous: document.getElementById('player-previous'),
  next: document.getElementById('player-next'),
  volume: document.getElementById('player-volume'),
  close: document.getElementById('player-close'),
};

let volumeDragging = false;

function clock(ms) {
  if (!Number.isFinite(Number(ms)) || Number(ms) < 0) return '0:00';
  const total = Math.floor(Number(ms) / 1000);
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, '0')}`;
}

function render(media) {
  const reading = media?.reading ?? null;
  if (!reading || !reading.title) {
    els.title.textContent = 'Nothing playing';
    els.artist.textContent = '';
    els.time.textContent = '';
    els.play.textContent = 'Play';
    els.play.disabled = true;
    els.previous.disabled = true;
    els.next.disabled = true;
    els.cover.hidden = true;
    return;
  }
  els.title.textContent = reading.title;
  els.artist.textContent = [reading.artist, reading.app]
    .filter(Boolean)
    .join(' · ');
  els.play.textContent = reading.isPlaying ? 'Pause' : 'Play';
  els.play.disabled = false;
  els.previous.disabled = !reading.canPrevious;
  els.next.disabled = !reading.canNext;
  els.time.textContent =
    Number(reading.durationMs) > 0
      ? `${clock(reading.positionMs)} / ${clock(reading.durationMs)}`
      : clock(reading.positionMs);

  const cover = media?.cover;
  if (cover?.id && cover.id === reading.coverId && cover.data) {
    const source = `data:${cover.type};base64,${cover.data}`;
    if (els.cover.src !== source) els.cover.src = source;
    els.cover.hidden = false;
  } else {
    els.cover.hidden = true;
  }
}

function setVolume(volume) {
  const value = Math.min(1, Math.max(0, Number(volume) || 0));
  els.volume.value = String(value);
}

async function refresh() {
  if (!invoke) return;
  try {
    render(await invoke('media_status'));
  } catch {
    render(null);
  }
  try {
    setVolume(await invoke('get_output_volume'));
  } catch {
    // A machine without an output keeps the slider where it is.
  }
}

async function command(name) {
  if (!invoke) return;
  try {
    await invoke('media_command', { command: name });
  } catch {
    // The helper stops when the app does; the next event says what holds.
  }
}

els.play.addEventListener('click', () =>
  command(els.play.textContent === 'Pause' ? 'pause' : 'play')
);
els.previous.addEventListener('click', () => command('previous'));
els.next.addEventListener('click', () => command('next'));

els.volume.addEventListener('input', () => {
  volumeDragging = true;
});
els.volume.addEventListener('change', async () => {
  volumeDragging = false;
  if (!invoke) return;
  try {
    setVolume(await invoke('set_output_volume', {
      volume: Number(els.volume.value),
    }));
  } catch {
    // Windows refused; the next volume event puts the slider back.
  }
});

els.close.addEventListener('click', () => invoke?.('hide_compact_player'));
window.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') invoke?.('hide_compact_player');
});

listen?.('media', (event) => render(event.payload));
listen?.('volume', (event) => {
  if (!volumeDragging) setVolume(event.payload);
});

refresh();
