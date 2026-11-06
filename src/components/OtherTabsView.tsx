import React, { useState } from 'react';
import {
  addComputer,
  initialGroup,
  orderedComputers,
  routeState,
  updateComputer,
  updateRoute,
} from '../utils/groupModel';

export const ShareAudioView: React.FC = () => {
  const [group, setGroup] = useState(initialGroup);
  const [localId, setLocalId] = useState('win');
  const [connCode] = useState('FLUIDEQ-LAN-2.94F8-77C1-ENCRYPTED');
  const [copied, setCopied] = useState(false);

  const local = group.computers.find((pc) => pc.id === localId) ?? group.computers[0];
  const peers = orderedComputers(group, local.id).filter((pc) => pc.id !== local.id);

  return (
    <div className="p-4 flex flex-col gap-4 h-full overflow-y-auto">
      <section className="bg-[#091b29] border border-[#17374b] rounded-lg p-4">
        <div className="flex flex-wrap items-center justify-between gap-3 mb-3">
          <div>
            <p className="text-[10px] font-semibold tracking-widest uppercase text-[#00e5cf]">
              LOSSLESS ENCRYPTED LAN AUDIO
            </p>
            <h2 className="text-base font-semibold text-[#ecf5fb]">
              Share audio between your computers
            </h2>
            <p className="text-xs text-[#8aa7b9] mt-0.5">
              Play your gaming PC, work laptop and media box into the one headset you are wearing — through this output&apos;s EQ and Room.
            </p>
          </div>
          <div className="flex items-center gap-2">
            <label className="text-xs text-[#8aa7b9]">Viewing as:</label>
            <select
              aria-label="Viewing as computer"
              value={local.id}
              onChange={(e) => setLocalId(e.target.value)}
              className="bg-[#06131d] border border-[#1d4259] rounded px-2.5 py-1.5 text-xs text-[#ecf5fb]"
            >
              {orderedComputers(group).map((pc) => (
                <option key={pc.id} value={pc.id}>
                  {pc.name} ({pc.platform})
                </option>
              ))}
            </select>
            <button
              type="button"
              onClick={() =>
                setGroup((prev) =>
                  addComputer(prev, {
                    id: `pc-${prev.computers.length + 1}`,
                    name: `Studio Workstation ${prev.computers.length}`,
                    platform: 'Windows',
                    online: true,
                    captureReady: true,
                    sharing: true,
                    paused: false,
                    systemVolume: 1,
                    output: 'Headphones',
                    source: 'System audio · 48 kHz PCM',
                  }),
                )
              }
              className="px-3 py-1.5 rounded bg-[#00e5cf] text-[#042326] font-semibold text-xs hover:bg-[#2bf0dc] cursor-pointer"
            >
              Add computer
            </button>
          </div>
        </div>

        {/* Connection code bar */}
        <div className="bg-[#06131d] border border-[#153347] rounded-lg p-3 flex flex-wrap items-center justify-between gap-3 mb-4">
          <div className="flex items-center gap-3">
            <span className="text-xs text-[#8aa7b9]">Group connection code:</span>
            <code className="px-2.5 py-1 rounded bg-[#040e16] border border-[#17374b] text-xs text-[#00e5cf] feq-mono">
              {connCode}
            </code>
          </div>
          <button
            type="button"
            onClick={() => {
              navigator.clipboard?.writeText(connCode).catch(() => {});
              setCopied(true);
            }}
            className="px-3 py-1 rounded border border-[#24526d] bg-[#0d2436] text-xs text-[#ecf5fb] hover:border-[#00e5cf] cursor-pointer"
          >
            {copied ? 'Copied code' : 'Copy code'}
          </button>
        </div>

        {/* Peer computers & bidirectional routing */}
        <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
          {peers.map((peer) => {
            const incoming = routeState(group, peer.id, local.id);
            const outgoing = routeState(group, local.id, peer.id);
            return (
              <div
                key={peer.id}
                className="bg-[#071622] border border-[#17374b] rounded-lg p-3.5 flex flex-col justify-between gap-3"
              >
                <div className="flex items-center justify-between">
                  <div>
                    <p className="text-xs font-semibold text-[#ecf5fb]">
                      {peer.name}{' '}
                      <span className="text-[11px] font-normal text-[#8aa7b9]">
                        · {peer.platform}
                      </span>
                    </p>
                    <p className="text-[11px] text-[#6c8ea3] mt-0.5">{peer.source}</p>
                  </div>
                  <button
                    type="button"
                    onClick={() =>
                      setGroup((prev) =>
                        updateComputer(prev, peer.id, { online: !peer.online }),
                      )
                    }
                    className={`px-2 py-0.5 rounded text-[10px] font-semibold cursor-pointer ${
                      peer.online
                        ? 'bg-[#0b2d39] text-[#00e5cf] border border-[#00e5cf]/50'
                        : 'bg-[#1a2630] text-[#8aa7b9]'
                    }`}
                  >
                    {peer.online ? 'ONLINE' : 'OFFLINE'}
                  </button>
                </div>

                <div className="flex flex-col gap-2 border-t border-[#153347] pt-2.5 text-xs">
                  <div className="flex items-center justify-between">
                    <span className="text-[#a5bdcb]">
                      Receive on {local.name} ({incoming.reason})
                    </span>
                    <input
                      type="checkbox"
                      aria-label={`Receive from ${peer.name}`}
                      checked={incoming.active}
                      onChange={(e) =>
                        setGroup((prev) =>
                          updateRoute(prev, peer.id, local.id, {
                            receive: e.target.checked,
                          }),
                        )
                      }
                      className="accent-[#00e5cf]"
                    />
                  </div>
                  <div className="flex items-center justify-between">
                    <span className="text-[#a5bdcb]">
                      Send to {peer.name} ({outgoing.reason})
                    </span>
                    <input
                      type="checkbox"
                      aria-label={`Send to ${peer.name}`}
                      checked={outgoing.active}
                      onChange={(e) =>
                        setGroup((prev) =>
                          updateRoute(prev, local.id, peer.id, {
                            send: e.target.checked,
                          }),
                        )
                      }
                      className="accent-[#00e5cf]"
                    />
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      </section>
    </div>
  );
};

export interface DemoTrack {
  id: string;
  title: string;
  artist: string;
  album: string;
  genre: string;
  duration: string;
  cover: string;
}

export const DEMO_TRACKS: readonly DemoTrack[] = [
  {
    id: 'track-1',
    title: 'Any Man of Mine',
    artist: 'Lisa McHugh',
    album: 'Nashville Sessions (24-bit / 48 kHz)',
    genre: 'Modern Country',
    duration: '3:12',
    cover: '/assets/tour/player-scene.jpg',
  },
  {
    id: 'track-2',
    title: 'Afterglow Avenue',
    artist: 'Velvet Circuit',
    album: 'Neon Horizon',
    genre: 'Synthwave',
    duration: '4:05',
    cover: '/assets/plus/neon-horizon.jpg',
  },
  {
    id: 'track-3',
    title: 'Northern Pines (Binaural Master)',
    artist: 'Aurora Ensemble',
    album: 'Still Lake',
    genre: 'Ambient Acoustic',
    duration: '3:48',
    cover: '/assets/tour/scene-alpine.jpg',
  },
  {
    id: 'track-4',
    title: 'Subsonic Protocol',
    artist: 'Kinetix Lab',
    album: 'Hyperdrive 2026',
    genre: 'Electronic',
    duration: '3:29',
    cover: '/assets/plus/hyperdrive.jpg',
  },
];

interface LibraryViewProps {
  currentTrack: DemoTrack;
  isPlaying: boolean;
  onSelectTrack: (track: DemoTrack) => void;
}

export const LibraryView: React.FC<LibraryViewProps> = ({
  currentTrack,
  isPlaying,
  onSelectTrack,
}) => {
  return (
    <div className="p-4 flex flex-col gap-4 h-full overflow-y-auto">
      <section className="bg-[#091b29] border border-[#17374b] rounded-lg p-4">
        <div className="flex items-center justify-between mb-3">
          <div>
            <p className="text-[10px] font-semibold tracking-widest uppercase text-[#00e5cf]">
              LOCAL MEDIA LIBRARY · DIRECT DSP HOST
            </p>
            <h2 className="text-base font-semibold text-[#ecf5fb]">
              Albums & Reference Tracks
            </h2>
          </div>
          <span className="text-xs text-[#8aa7b9]">
            Smart EQ per-song memory active
          </span>
        </div>

        <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-3.5">
          {DEMO_TRACKS.map((track) => {
            const active = track.id === currentTrack.id;
            return (
              <div
                key={track.id}
                onClick={() => onSelectTrack(track)}
                className={`rounded-lg overflow-hidden border transition-all cursor-pointer ${
                  active
                    ? 'bg-[#0e2e40] border-[#00e5cf] shadow-[0_0_15px_rgba(0,229,207,0.2)]'
                    : 'bg-[#071622] border-[#17374b] hover:border-[#24526d]'
                }`}
              >
                <img
                  src={track.cover}
                  alt={track.title}
                  className="w-full h-32 object-cover"
                />
                <div className="p-3">
                  <p className="text-xs font-semibold text-[#ecf5fb] truncate">
                    {track.title}
                  </p>
                  <p className="text-[11px] text-[#8aa7b9] truncate mt-0.5">
                    {track.artist} · {track.album}
                  </p>
                  <div className="flex items-center justify-between mt-2">
                    <span className="text-[10px] px-2 py-0.5 rounded bg-[#06131d] text-[#00e5cf]">
                      {track.genre}
                    </span>
                    <span className="text-[11px] text-[#8aa7b9] feq-mono">
                      {active && isPlaying ? '▶ Playing' : track.duration}
                    </span>
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      </section>
    </div>
  );
};

export const KaraokeView: React.FC<{ isPlaying: boolean }> = ({ isPlaying }) => {
  const [micEnabled, setMicEnabled] = useState(false);
  const [lyricScale, setLyricScale] = useState(100);

  return (
    <div className="p-4 flex flex-col gap-4 h-full overflow-y-auto">
      <section className="bg-[#091b29] border border-[#17374b] rounded-lg p-5 flex flex-col justify-between gap-6">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div>
            <p className="text-[10px] font-semibold tracking-widest uppercase text-[#00e5cf]">
              ULTRASTAR SYLLABLES & LIVE PITCH STAGE
            </p>
            <h2 className="text-base font-semibold text-[#ecf5fb]">
              Karaoke Stage — Afterglow Avenue
            </h2>
          </div>
          <div className="flex items-center gap-3">
            <span className="px-2.5 py-1 rounded bg-[#06131d] border border-[#1d4259] text-xs text-[#00e5cf] feq-mono">
              Chord now: Fm7 → Next: D♭maj7
            </span>
            <label className="flex items-center gap-2 text-xs cursor-pointer">
              <span>Mic Pitch Monitor</span>
              <input
                type="checkbox"
                checked={micEnabled}
                onChange={(e) => setMicEnabled(e.target.checked)}
                className="accent-[#00e5cf]"
              />
            </label>
            <label className="flex items-center gap-2 text-xs text-[#8aa7b9]">
              <span>Size</span>
              <input
                type="range"
                min={80}
                max={130}
                value={lyricScale}
                onChange={(e) => setLyricScale(Number(e.target.value))}
                className="w-20 accent-[#00e5cf]"
              />
            </label>
          </div>
        </div>

        <div className="py-8 text-center flex flex-col items-center gap-3 bg-[#05121c] rounded-lg border border-[#153347]">
          <p className="text-sm text-[#5d7c90]">
            Walking through the static of the midnight rain
          </p>
          <p
            style={{ fontSize: `${Math.round(24 * (lyricScale / 100))}px` }}
            className="font-black tracking-tight text-[#00e5cf]"
          >
            Every frequency comes back into focus again
          </p>
          <p className="text-sm text-[#8aa7b9]">
            Hold the horizon while the city lights bloom
          </p>
          <p className="text-xs text-[#5d7c90] mt-2 feq-mono">
            {isPlaying ? 'Audio clock locked · Pitch target: C4 (261.6 Hz) — IN TUNE' : 'Press Play in the transport bar below to advance lyrics'}
          </p>
        </div>
      </section>
    </div>
  );
};

export const PlusVisualizersView: React.FC<{
  activeBackdrop: string | null;
  onSelectBackdrop: (url: string | null) => void;
}> = ({ activeBackdrop, onSelectBackdrop }) => {
  const scenes = [
    {
      id: 'aurora',
      title: 'Northern Lights (Aurora)',
      creator: 'FluidEQ',
      category: '3D Living World',
      image: '/assets/tour/scene-aurora.jpg',
    },
    {
      id: 'neon-city',
      title: 'Neon City 3D',
      creator: 'FluidEQ',
      category: 'Cyberpunk Skyline',
      image: '/assets/tour/scene-neon-city.jpg',
    },
    {
      id: 'alpine',
      title: 'Alpine Lantern Lake',
      creator: 'FluidEQ',
      category: 'Mountain Reflections',
      image: '/assets/tour/scene-alpine.jpg',
    },
    {
      id: 'bloom',
      title: 'Prism Bloom',
      creator: 'FluidEQ',
      category: 'Reactive Geometry',
      image: '/assets/tour/scene-bloom.jpg',
    },
    {
      id: 'chrome',
      title: 'Liquid Chrome Sphere',
      creator: 'FluidEQ',
      category: 'Raymarched Shader',
      image: '/assets/tour/scene-chrome.jpg',
    },
    {
      id: 'nebula',
      title: 'Nebula Heart',
      creator: 'FluidEQ',
      category: 'Deep Space',
      image: '/assets/plus/nebula-heart.jpg',
    },
  ];

  return (
    <div className="p-4 flex flex-col gap-4 h-full overflow-y-auto">
      <section className="bg-[#091b29] border border-[#17374b] rounded-lg p-4">
        <div className="flex flex-wrap items-center justify-between gap-3 mb-4">
          <div>
            <p className="text-[10px] font-semibold tracking-widest uppercase text-[#00e5cf]">
              LIVING SCENES & CREATOR STUDIO
            </p>
            <h2 className="text-base font-semibold text-[#ecf5fb]">
              Visualizers: scenes that move with the bass, beat and treble
            </h2>
          </div>
          {activeBackdrop && (
            <button
              type="button"
              onClick={() => onSelectBackdrop(null)}
              className="px-3 py-1.5 rounded border border-[#24526d] bg-[#0d2436] text-xs text-[#ecf5fb] hover:border-[#00e5cf] cursor-pointer"
            >
              Clear window backdrop
            </button>
          )}
        </div>

        <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-4">
          {scenes.map((scene) => {
            const isSet = activeBackdrop === scene.image;
            return (
              <div
                key={scene.id}
                className={`rounded-lg overflow-hidden border ${
                  isSet ? 'border-[#00e5cf] bg-[#0e2e40]' : 'border-[#17374b] bg-[#071622]'
                }`}
              >
                <img
                  src={scene.image}
                  alt={scene.title}
                  className="w-full h-36 object-cover"
                />
                <div className="p-3 flex items-center justify-between">
                  <div>
                    <p className="text-xs font-semibold text-[#ecf5fb]">
                      {scene.title}
                    </p>
                    <p className="text-[11px] text-[#8aa7b9]">
                      {scene.category} · by {scene.creator}
                    </p>
                  </div>
                  <button
                    type="button"
                    onClick={() => onSelectBackdrop(isSet ? null : scene.image)}
                    className={`px-3 py-1.5 rounded text-xs font-semibold cursor-pointer ${
                      isSet
                        ? 'bg-[#17374b] text-[#00e5cf] border border-[#00e5cf]'
                        : 'bg-[#00e5cf] text-[#042326] hover:bg-[#2bf0dc]'
                    }`}
                  >
                    {isSet ? 'Active Backdrop' : 'Play on Backdrop'}
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      </section>
    </div>
  );
};
