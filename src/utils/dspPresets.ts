import { EqBand } from './dspMath';

export const ISO_15_FREQUENCIES = [
  26, 41, 64, 101, 161, 251, 401, 631, 1000, 1600, 2500, 4000, 6300, 10200, 16300,
] as const;

export const ISO_10_FREQUENCIES = [
  31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000,
] as const;

export function createDefaultEqBands(frequencies: readonly number[] = ISO_15_FREQUENCIES): EqBand[] {
  return frequencies.map((freq, idx) => ({
    id: `band-${idx + 1}`,
    enabled: true,
    type: idx === 0 ? 'LSC' : idx === frequencies.length - 1 ? 'HSC' : 'PK',
    frequency: freq,
    gainDb: 0,
    q: idx === 0 || idx === frequencies.length - 1 ? 0.7 : 1.41,
    ear: 'both',
  }));
}

export interface PresetDefinition {
  id: string;
  name: string;
  category: 'Music Style' | 'Gaming' | 'Studio & Voice' | 'Cinema';
  description: string;
  bands: EqBand[];
}

export const FACTORY_PRESETS: readonly PresetDefinition[] = [
  {
    id: 'modern-country',
    name: 'Modern Country',
    category: 'Music Style',
    description: 'Tight acoustic body, warm low-mid foundation, and open vocal & string air without sibilance.',
    bands: [
      { id: 'p-1', enabled: true, type: 'LSC', frequency: 85, gainDb: 1.8, q: 0.7 },
      { id: 'p-2', enabled: true, type: 'PK', frequency: 130, gainDb: 1.4, q: 1.2 },
      { id: 'p-3', enabled: true, type: 'PK', frequency: 320, gainDb: -1.6, q: 1.5 },
      { id: 'p-4', enabled: true, type: 'PK', frequency: 500, gainDb: -1.1, q: 1.8 },
      { id: 'p-5', enabled: true, type: 'PK', frequency: 2800, gainDb: 2.1, q: 1.4 },
      { id: 'p-6', enabled: true, type: 'PK', frequency: 4400, gainDb: 4.8, q: 1.9 },
      { id: 'p-7', enabled: true, type: 'PK', frequency: 7800, gainDb: -2.2, q: 3.1 },
      { id: 'p-8', enabled: true, type: 'HSC', frequency: 11000, gainDb: 1.6, q: 0.7 },
    ],
  },
  {
    id: 'harman-over-ear',
    name: 'Harman 2018 Reference',
    category: 'Studio & Voice',
    description: 'Balanced sub-bass shelf (+4.5 dB below 105 Hz) with natural pinna gain around 3 kHz.',
    bands: [
      { id: 'p-1', enabled: true, type: 'LSC', frequency: 105, gainDb: 4.5, q: 0.7 },
      { id: 'p-2', enabled: true, type: 'PK', frequency: 240, gainDb: -1.4, q: 1.1 },
      { id: 'p-3', enabled: true, type: 'PK', frequency: 1200, gainDb: 0.8, q: 1.5 },
      { id: 'p-4', enabled: true, type: 'PK', frequency: 3100, gainDb: 3.2, q: 1.3 },
      { id: 'p-5', enabled: true, type: 'HSC', frequency: 10000, gainDb: -1.2, q: 0.7 },
    ],
  },
  {
    id: 'synthwave-horizon',
    name: 'Synthwave & Electronic',
    category: 'Music Style',
    description: 'Deep sub-bass extension, scooped boxy mids, and crystalline analog synth top end.',
    bands: [
      { id: 'p-1', enabled: true, type: 'LSC', frequency: 75, gainDb: 5.2, q: 0.7 },
      { id: 'p-2', enabled: true, type: 'PK', frequency: 160, gainDb: 1.8, q: 1.4 },
      { id: 'p-3', enabled: true, type: 'PK', frequency: 420, gainDb: -2.5, q: 1.3 },
      { id: 'p-4', enabled: true, type: 'PK', frequency: 2400, gainDb: 1.9, q: 1.6 },
      { id: 'p-5', enabled: true, type: 'HSC', frequency: 9500, gainDb: 3.8, q: 0.7 },
    ],
  },
  {
    id: 'vocal-presence',
    name: 'Vocal Presence & Acoustic',
    category: 'Studio & Voice',
    description: 'Brings lead vocals and intimate instruments forward while clearing low-end rumble.',
    bands: [
      { id: 'p-1', enabled: true, type: 'LSC', frequency: 90, gainDb: -2.0, q: 0.7 },
      { id: 'p-2', enabled: true, type: 'PK', frequency: 280, gainDb: -1.5, q: 1.4 },
      { id: 'p-3', enabled: true, type: 'PK', frequency: 1800, gainDb: 2.8, q: 1.2 },
      { id: 'p-4', enabled: true, type: 'PK', frequency: 3600, gainDb: 3.4, q: 1.5 },
      { id: 'p-5', enabled: true, type: 'HSC', frequency: 10500, gainDb: 2.0, q: 0.7 },
    ],
  },
  {
    id: 'competitive-footsteps',
    name: 'Tactical Shooter (Footsteps)',
    category: 'Gaming',
    description: 'Attenuates sub-bass blast masking and lifts the 300 Hz, 2.4 kHz and 7 kHz transient bands.',
    bands: [
      { id: 'p-1', enabled: true, type: 'LSC', frequency: 110, gainDb: -4.2, q: 0.7 },
      { id: 'p-2', enabled: true, type: 'PK', frequency: 300, gainDb: 2.6, q: 1.8 },
      { id: 'p-3', enabled: true, type: 'PK', frequency: 2400, gainDb: 4.5, q: 1.6 },
      { id: 'p-4', enabled: true, type: 'PK', frequency: 7000, gainDb: 3.2, q: 2.0 },
    ],
  },
  {
    id: 'cinema-atmos',
    name: 'Cinema & Soundstage',
    category: 'Cinema',
    description: 'Subwoofer LFE weight below 65 Hz with clear dialogue articulation at 2.2 kHz.',
    bands: [
      { id: 'p-1', enabled: true, type: 'LSC', frequency: 65, gainDb: 5.5, q: 0.7 },
      { id: 'p-2', enabled: true, type: 'PK', frequency: 210, gainDb: -1.8, q: 1.4 },
      { id: 'p-3', enabled: true, type: 'PK', frequency: 2200, gainDb: 2.9, q: 1.5 },
      { id: 'p-4', enabled: true, type: 'HSC', frequency: 10000, gainDb: 2.4, q: 0.7 },
    ],
  },
];

export interface DriverTypeProfile {
  id: string;
  name: string;
  subtitle: string;
  note: string;
  summaryTag: string;
  bands: EqBand[];
}

export const DRIVER_PROFILES: readonly DriverTypeProfile[] = [
  {
    id: 'bio-cellulose',
    name: 'Bio-cellulose',
    subtitle: 'Naturally well-damped diaphragm',
    summaryTag: '8 kHz  +0.9 dB',
    note: 'A small air lift to try only if the sound feels too soft. Material alone cannot locate a treble peak. This broad taste adjustment avoids guessed resonance cuts.',
    bands: [
      { id: 'drv-1', enabled: true, type: 'HSC', frequency: 8000, gainDb: 0.9, q: 0.7 },
    ],
  },
  {
    id: 'planar-magnetic',
    name: 'Planar magnetic',
    subtitle: 'Linear low-end with upper-treble energy',
    summaryTag: '55 Hz +2.2 dB · 9 kHz -1.4 dB',
    note: 'Supports deep sub-bass extension where open planars roll off and gently tames upper-harmonic glare.',
    bands: [
      { id: 'drv-1', enabled: true, type: 'LSC', frequency: 55, gainDb: 2.2, q: 0.7 },
      { id: 'drv-2', enabled: true, type: 'PK', frequency: 9000, gainDb: -1.4, q: 1.8 },
    ],
  },
  {
    id: 'dynamic-open',
    name: 'Dynamic open-back',
    subtitle: 'Airy staging with mid-bass warmth',
    summaryTag: '45 Hz +3.1 dB · 180 Hz -1.2 dB',
    note: 'Extends sub-bass reach and tightens the 180 Hz upper-bass hump typical of vented open-back dynamic drivers.',
    bands: [
      { id: 'drv-1', enabled: true, type: 'LSC', frequency: 45, gainDb: 3.1, q: 0.7 },
      { id: 'drv-2', enabled: true, type: 'PK', frequency: 180, gainDb: -1.2, q: 1.3 },
    ],
  },
  {
    id: 'hybrid-iem',
    name: 'Hybrid IEM (DD + BA)',
    subtitle: 'Coherent crossover smoothing',
    summaryTag: '320 Hz +0.8 dB · 6.5 kHz -1.6 dB',
    note: 'Bridges the dynamic woofer to balanced-armature midrange transition and softens BA metallic timbre.',
    bands: [
      { id: 'drv-1', enabled: true, type: 'PK', frequency: 320, gainDb: 0.8, q: 1.4 },
      { id: 'drv-2', enabled: true, type: 'PK', frequency: 6500, gainDb: -1.6, q: 2.2 },
    ],
  },
];

export interface OutputEndpoint {
  id: string;
  slug: string;
  name: string;
  sampleRate: number;
  channels: string;
  activeProfile: string;
  profiles: string[];
  mirrorEnabled: boolean;
  mirrorGain: number;
}

export const INITIAL_OUTPUTS: OutputEndpoint[] = [
  {
    id: '{46558bc9-ef9b-4aa7-b317-dee9e93a5267}',
    slug: '665f99a8cefb',
    name: 'Speakers (Razer Kraken V4 Pro - Game)',
    sampleRate: 48000,
    channels: '7.1 Surround',
    activeProfile: 'Untitled profile 1',
    profiles: ['Untitled profile 1', 'Music & Hi-Fi', 'Competitive FPS'],
    mirrorEnabled: false,
    mirrorGain: 1.0,
  },
  {
    id: '{89123ab1-cc41-4092-991a-77812399ab01}',
    slug: '89123ab1cc41',
    name: 'Headphones (Apple AirPods Max USB-C)',
    sampleRate: 48000,
    channels: 'Stereo',
    activeProfile: 'Harman Studio',
    profiles: ['Harman Studio', 'Late Night'],
    mirrorEnabled: true,
    mirrorGain: 0.85,
  },
  {
    id: '{33190ef2-12a8-4b11-8890-112233445566}',
    slug: '33190ef212a8',
    name: 'Speakers (Realtek High Definition Audio)',
    sampleRate: 96000,
    channels: 'Stereo',
    activeProfile: 'Desk Nearfield',
    profiles: ['Desk Nearfield', 'Cinema Sub'],
    mirrorEnabled: false,
    mirrorGain: 0.75,
  },
  {
    id: '{77441cd0-55b2-4e88-a123-998877665544}',
    slug: '77441cd055b2',
    name: 'Odyssey G5 (NVIDIA High Definition Audio)',
    sampleRate: 48000,
    channels: 'Stereo',
    activeProfile: 'Monitor Voice Clear',
    profiles: ['Monitor Voice Clear'],
    mirrorEnabled: false,
    mirrorGain: 0.7,
  },
];

export interface SpeakerNode {
  id: 'FL' | 'FR' | 'C' | 'SUB' | 'SL' | 'SR' | 'RL' | 'RR';
  label: string;
  angleDeg: number;
  distanceM: number;
  levelDb: number;
  muted: boolean;
  solo: boolean;
  activeInStereo: boolean;
}

export interface RoomPreset {
  id: string;
  name: string;
  badge: string;
  summary: string;
  spacePct: number;
  ambiencePct: number;
  distanceM: number;
  roomSizeM: number;
  wallsAbsorption: number;
  tailSeconds: number;
  tailToneHz: number;
  stereoFill: boolean;
  stereoAmount: number;
  crossoverHz: number;
  speakers: SpeakerNode[];
}

export const DEFAULT_SPEAKERS: SpeakerNode[] = [
  { id: 'FL', label: 'Front left', angleDeg: -30, distanceM: 1.6, levelDb: 0.0, muted: false, solo: false, activeInStereo: true },
  { id: 'FR', label: 'Front right', angleDeg: 30, distanceM: 1.6, levelDb: 0.0, muted: false, solo: false, activeInStereo: true },
  { id: 'C', label: 'Centre', angleDeg: 0, distanceM: 1.6, levelDb: 0.0, muted: false, solo: false, activeInStereo: false },
  { id: 'SUB', label: 'Subwoofer', angleDeg: -58, distanceM: 2.1, levelDb: 0.5, muted: false, solo: false, activeInStereo: false },
  { id: 'SL', label: 'Side left', angleDeg: -95, distanceM: 1.5, levelDb: -0.5, muted: false, solo: false, activeInStereo: false },
  { id: 'SR', label: 'Side right', angleDeg: 95, distanceM: 1.5, levelDb: -0.5, muted: false, solo: false, activeInStereo: false },
  { id: 'RL', label: 'Rear left', angleDeg: -142, distanceM: 1.7, levelDb: -1.0, muted: false, solo: false, activeInStereo: false },
  { id: 'RR', label: 'Rear right', angleDeg: 142, distanceM: 1.7, levelDb: -1.0, muted: false, solo: false, activeInStereo: false },
];

export const ROOM_PRESETS: readonly RoomPreset[] = [
  {
    id: 'game-world',
    name: 'Game World',
    badge: 'Featured',
    summary: 'Stereo → front stage · Surrounding environment without burying short cues',
    spacePct: 61,
    ambiencePct: 30,
    distanceM: 1.6,
    roomSizeM: 5.0,
    wallsAbsorption: 0.55,
    tailSeconds: 0.25,
    tailToneHz: 8000,
    stereoFill: false,
    stereoAmount: 0.6,
    crossoverHz: 80,
    speakers: DEFAULT_SPEAKERS,
  },
  {
    id: 'reference-studio',
    name: 'Reference',
    badge: 'Featured',
    summary: 'Stereo → front stage · Close, restrained mastering stage with zero tail smear',
    spacePct: 32,
    ambiencePct: 0,
    distanceM: 1.2,
    roomSizeM: 4.2,
    wallsAbsorption: 0.72,
    tailSeconds: 0.12,
    tailToneHz: 9500,
    stereoFill: false,
    stereoAmount: 0.0,
    crossoverHz: 80,
    speakers: DEFAULT_SPEAKERS.map((s) => ({ ...s, distanceM: 1.2 })),
  },
  {
    id: 'music-space',
    name: 'Music Space',
    badge: 'Featured',
    summary: 'Stereo → fills the room · Wider listening space with vocals anchored in front',
    spacePct: 54,
    ambiencePct: 24,
    distanceM: 1.8,
    roomSizeM: 5.8,
    wallsAbsorption: 0.48,
    tailSeconds: 0.35,
    tailToneHz: 7500,
    stereoFill: true,
    stereoAmount: 0.45,
    crossoverHz: 80,
    speakers: DEFAULT_SPEAKERS.map((s) => ({ ...s, activeInStereo: true })),
  },
  {
    id: 'cinema-hall',
    name: 'Cinema',
    badge: 'Featured',
    summary: 'Stereo → fills the room · Broad screen, supported centre, controlled LFE',
    spacePct: 72,
    ambiencePct: 42,
    distanceM: 2.4,
    roomSizeM: 8.5,
    wallsAbsorption: 0.5,
    tailSeconds: 0.48,
    tailToneHz: 6400,
    stereoFill: true,
    stereoAmount: 0.65,
    crossoverHz: 90,
    speakers: DEFAULT_SPEAKERS.map((s) => ({ ...s, distanceM: 2.4, activeInStereo: true })),
  },
  {
    id: 'competitive-esports',
    name: 'Competitive',
    badge: 'Featured',
    summary: 'Stereo → front stage · Pinpoint KEMAR lateral timing cues with zero room tail',
    spacePct: 18,
    ambiencePct: 0,
    distanceM: 1.1,
    roomSizeM: 3.6,
    wallsAbsorption: 0.85,
    tailSeconds: 0.1,
    tailToneHz: 10500,
    stereoFill: false,
    stereoAmount: 0.0,
    crossoverHz: 70,
    speakers: DEFAULT_SPEAKERS.map((s) => ({ ...s, distanceM: 1.1 })),
  },
  {
    id: 'live-venue',
    name: 'Live Venue',
    badge: 'Featured',
    summary: 'Stereo → fills the room · Enveloping concert stage with warm early reflections',
    spacePct: 84,
    ambiencePct: 58,
    distanceM: 2.8,
    roomSizeM: 11.0,
    wallsAbsorption: 0.38,
    tailSeconds: 0.75,
    tailToneHz: 5800,
    stereoFill: true,
    stereoAmount: 0.78,
    crossoverHz: 85,
    speakers: DEFAULT_SPEAKERS.map((s) => ({ ...s, distanceM: 2.8, activeInStereo: true })),
  },
];

export interface ConvolutionImpulse {
  id: string;
  model: string;
  brand: string;
  measuredBy: string;
  rig: string;
  sampleRate: string;
  bands: EqBand[];
}

export const CONVOLUTION_CATALOGUE: readonly ConvolutionImpulse[] = [
  {
    id: 'hd650-oratory',
    model: 'Sennheiser HD 650',
    brand: 'Sennheiser',
    measuredBy: 'oratory1990',
    rig: 'GRAS 43AG-7 · Minimum-phase FIR',
    sampleRate: '48 kHz WAV',
    bands: [
      { id: 'c1', enabled: true, type: 'LSC', frequency: 75, gainDb: 5.5, q: 0.7 },
      { id: 'c2', enabled: true, type: 'PK', frequency: 190, gainDb: -2.1, q: 1.2 },
      { id: 'c3', enabled: true, type: 'PK', frequency: 3200, gainDb: 1.8, q: 1.9 },
    ],
  },
  {
    id: 'dt1990-oratory',
    model: 'beyerdynamic DT 1990 Pro (Analytic Pads)',
    brand: 'beyerdynamic',
    measuredBy: 'oratory1990',
    rig: 'GRAS 43AG-7 · Minimum-phase FIR',
    sampleRate: '48 kHz WAV',
    bands: [
      { id: 'c1', enabled: true, type: 'LSC', frequency: 90, gainDb: 3.4, q: 0.7 },
      { id: 'c2', enabled: true, type: 'PK', frequency: 8300, gainDb: -6.8, q: 4.5 },
    ],
  },
  {
    id: 'lcdx-crinacle',
    model: 'Audeze LCD-X (2021 Revision)',
    brand: 'Audeze',
    measuredBy: 'crinacle',
    rig: 'GRAS 43AG-7 · Minimum-phase FIR',
    sampleRate: '48 kHz WAV',
    bands: [
      { id: 'c1', enabled: true, type: 'LSC', frequency: 80, gainDb: 3.2, q: 0.7 },
      { id: 'c2', enabled: true, type: 'PK', frequency: 3800, gainDb: 4.9, q: 1.4 },
    ],
  },
  {
    id: 'sundara-rtings',
    model: 'HIFIMAN Sundara',
    brand: 'HIFIMAN',
    measuredBy: 'Rtings',
    rig: 'HMS II.3 · Minimum-phase FIR',
    sampleRate: '48 kHz WAV',
    bands: [
      { id: 'c1', enabled: true, type: 'LSC', frequency: 70, gainDb: 4.8, q: 0.7 },
      { id: 'c2', enabled: true, type: 'PK', frequency: 2100, gainDb: 1.9, q: 1.8 },
    ],
  },
  {
    id: ' Blessing3-super',
    model: 'Moondrop Blessing 3',
    brand: 'Moondrop',
    measuredBy: 'Super Review',
    rig: 'IEC 60318-4 (711) · Minimum-phase FIR',
    sampleRate: '48 kHz WAV',
    bands: [
      { id: 'c1', enabled: true, type: 'LSC', frequency: 105, gainDb: 2.6, q: 0.7 },
      { id: 'c2', enabled: true, type: 'PK', frequency: 3100, gainDb: -2.2, q: 2.1 },
    ],
  },
];

export const DEFAULT_SQUIGLINK_TEXT = `Preamp: -9.9 dB
Filter 1: ON PK Fc 58 Hz Gain 3.2 dB Q 2.000
Filter 2: ON PK Fc 85 Hz Gain -3.0 dB Q 2.800
Filter 3: ON PK Fc 130 Hz Gain -6.3 dB Q 0.900
Filter 4: ON PK Fc 140 Hz Gain 1.1 dB Q 3.800
Filter 5: ON PK Fc 230 Hz Gain -1.4 dB Q 2.300
Filter 6: ON PK Fc 500 Hz Gain 1.5 dB Q 1.300
Filter 7: ON PK Fc 1000 Hz Gain -2.0 dB Q 2.200
Filter 8: ON PK Fc 2600 Hz Gain -6.0 dB Q 2.900
Filter 9: ON PK Fc 4300 Hz Gain 10.6 dB Q 1.200
Filter 10: ON PK Fc 7900 Hz Gain -6.4 dB Q 5.000`;
