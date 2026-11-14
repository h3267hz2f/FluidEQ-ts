/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Flat `f64` Wire Encoder & Decoder for `FeqChainSettings`.
//!
//! Matches `/native/dsp-core/src/chain_decode.cpp` (`feq_chain_settings_decode`)
//! word-for-word:
//! - Lead block: `FEQ_CHAIN_PARAM_LEAD` (158) `f64` words `[0..158)`
//! - EQ band block: `band_count * FEQ_CHAIN_BAND_PARAMS` (`band_count * 7`) words
//! - Post-band trailers:
//!   1. Normalizer (`3` words: `mode`, `ceiling_db`, `target_lufs`)
//!   2. Low-latency / Game mode (`1` word: `0.0` or `1.0`)
//!   3. Tagged Room v2 trailer (`11` words starting with `FEQ_CHAIN_ROOM_TAG`)
//!   4. Tagged Footsteps trailer (`7` words starting with `FEQ_CHAIN_FOOTSTEPS_TAG`)
//!   5. Tagged Dynamics trailer (`23` words starting with `FEQ_CHAIN_DYNAMICS_TAG`)
//!   6. Tagged Tone trailer (`4 + tone_bands * 4` words starting with `FEQ_CHAIN_TONE_TAG`)

use fluideq_dsp_sys::{
    FeqChainSettings, FeqFilterType, FEQ_CHAIN_BAND_PARAMS, FEQ_CHAIN_DEESSER_FIELDS,
    FEQ_CHAIN_DEESSER_SIGNATURE, FEQ_CHAIN_DYNAMICS_FIELDS, FEQ_CHAIN_DYNAMICS_TAG,
    FEQ_CHAIN_DYNAMICS_TRAILER, FEQ_CHAIN_DYNAMICS_VERSION, FEQ_CHAIN_EXCITER_BANDS,
    FEQ_CHAIN_FOOTSTEPS_FIELDS, FEQ_CHAIN_FOOTSTEPS_TAG, FEQ_CHAIN_FOOTSTEPS_VERSION,
    FEQ_CHAIN_MAX_EQ_BANDS, FEQ_CHAIN_MAX_TONE_BANDS, FEQ_CHAIN_PARAM_LEAD, FEQ_CHAIN_ROOM_FIELDS,
    FEQ_CHAIN_ROOM_TAG, FEQ_CHAIN_ROOM_VERSION, FEQ_CHAIN_TONE_TAG, FEQ_CHAIN_TONE_VERSION,
    FEQ_COMPANDER_BANDS, FEQ_ROOM_MUTE_SLOTS, FEQ_ROOM_SPEAKERS,
};

#[inline]
fn bool_word(v: i32) -> f64 {
    if v != 0 {
        1.0
    } else {
        0.0
    }
}

#[inline]
fn read_bool(v: f64) -> i32 {
    if v.is_finite() && v >= 0.5 {
        1
    } else {
        0
    }
}

#[inline]
fn clamp_f64(v: f64, lo: f64, hi: f64) -> f64 {
    if !v.is_finite() {
        lo
    } else {
        v.clamp(lo, hi)
    }
}

/// Encodes `FeqChainSettings` into the flat `Vec<f64>` wire payload consumed by
/// `feq_engine_prepare_config` and `feq_chain_settings_decode`.
pub fn encode_chain_settings_wire(settings: &FeqChainSettings) -> Vec<f64> {
    let band_count = (settings.eq.band_count as usize).min(FEQ_CHAIN_MAX_EQ_BANDS);
    let tone_count = (settings.tone.band_count as usize).min(FEQ_CHAIN_MAX_TONE_BANDS);
    let emit_footsteps = settings.footsteps.enabled != 0
        || settings.footsteps.isolate != 0
        || (settings.footsteps.lift - 0.85).abs() > 1e-9
        || (settings.footsteps.calm - 0.5).abs() > 1e-9;
    let emit_dynamics = settings.compander.enabled != 0;

    let total_capacity = FEQ_CHAIN_PARAM_LEAD
        + band_count * FEQ_CHAIN_BAND_PARAMS
        + 4
        + 11
        + if emit_footsteps { 7 } else { 0 }
        + if emit_dynamics {
            FEQ_CHAIN_DYNAMICS_TRAILER
        } else {
            0
        }
        + if tone_count >= 1 {
            4 + tone_count * 4
        } else {
            0
        };

    let mut out = vec![0.0_f64; FEQ_CHAIN_PARAM_LEAD];
    out.reserve(total_capacity.saturating_sub(FEQ_CHAIN_PARAM_LEAD));

    // [0..2]: Master chain switch + retired guard slot
    out[0] = bool_word(settings.enabled);
    out[1] = 0.0;

    // [2..11]: Exciter header (9 words)
    out[2] = bool_word(settings.exciter.enabled);
    out[3] = bool_word(settings.exciter.isolate);
    out[4] = settings.exciter.stereo as f64;
    out[5] = bool_word(settings.exciter.align_enabled);
    out[6] = settings.exciter.align_amount;
    out[7] = bool_word(settings.exciter.organic_enabled);
    out[8] = settings.exciter.organic_amount;
    out[9] = settings.exciter.organic_focus_hz;
    out[10] = settings.exciter.organic_range;

    // [11..29]: Exciter 3 bands x 6 words (18 words)
    let mut cursor = 11;
    for band_idx in 0..FEQ_CHAIN_EXCITER_BANDS {
        let b = &settings.exciter.bands[band_idx];
        out[cursor] = bool_word(b.enabled);
        out[cursor + 1] = b.freq_hz;
        out[cursor + 2] = b.range;
        out[cursor + 3] = b.drive;
        out[cursor + 4] = b.mix;
        out[cursor + 5] = b.texture;
        cursor += 6;
    }

    // [29..40]: EQ header (11 words)
    out[29] = bool_word(settings.eq.enabled);
    out[30] = bool_word(settings.eq.isolate);
    out[31] = settings.eq.model as f64;
    out[32] = settings.eq.model_amount;
    out[33] = settings.eq.engine as f64;
    out[34] = settings.eq.phase as f64;
    out[35] = settings.eq.stereo as f64;
    out[36] = settings.eq.mono_below_hz;
    out[37] = settings.eq.oversample as f64;
    out[38] = settings.eq.subsonic_hz;
    out[39] = settings.eq.fuzz_amount;

    // [40]: Retired multiband compressor switch (must be 0.0)
    out[40] = 0.0;

    // [41..53]: De-esser (12 words) + [53..55] signature pair + [55] exciter oversampling + [56..58] spare
    out[41] = bool_word(settings.deesser.enabled);
    out[42] = bool_word(settings.deesser.isolate);
    out[43] = settings.deesser.sensitivity;
    out[44] = settings.deesser.low_hz;
    out[45] = settings.deesser.high_hz;
    out[46] = settings.deesser.range_db;
    out[47] = settings.deesser.width_oct;
    out[48] = settings.deesser.mode as f64;
    out[49] = settings.deesser.attack_ms;
    out[50] = settings.deesser.release_ms;
    out[51] = bool_word(settings.deesser.centre_only);
    out[52] = clamp_f64(settings.deesser.keep_cymbals, 0.0, 1.0);
    out[53] = FEQ_CHAIN_DEESSER_SIGNATURE;
    out[54] = FEQ_CHAIN_DEESSER_FIELDS;
    out[55] = settings.exciter.oversampling as f64;
    out[56] = 0.0;
    out[57] = 0.0;

    // [58..65]: Dimension (7 words)
    out[58] = bool_word(settings.dimension.enabled);
    out[59] = settings.dimension.low_width;
    out[60] = settings.dimension.mid_width;
    out[61] = settings.dimension.high_width;
    out[62] = settings.dimension.low_hz;
    out[63] = settings.dimension.high_hz;
    out[64] = settings.dimension.decorrelation;

    // [65..70]: Maximizer (5 words)
    out[65] = bool_word(settings.maximizer.enabled);
    out[66] = settings.maximizer.drive_db;
    out[67] = settings.maximizer.ceiling_db;
    out[68] = settings.maximizer.look_ahead_ms;
    out[69] = settings.maximizer.release_ms;

    // [70..77]: Master (7 words)
    out[70] = bool_word(settings.master.enabled);
    out[71] = settings.master.output_trim_db;
    out[72] = bool_word(settings.master.loudness_maximize);
    out[73] = settings.master.loudness_target_lufs;
    out[74] = settings.master.ceiling_db;
    out[75] = settings.master.release_ms;
    out[76] = bool_word(settings.master.matched_bypass);

    // [77..96]: Denoise (19 words)
    out[77] = bool_word(settings.denoise.enabled);
    out[78] = bool_word(settings.denoise.isolate);
    out[79] = settings.denoise.profile_source as f64;
    out[80] = bool_word(settings.denoise.hiss.enabled);
    out[81] = settings.denoise.hiss.amount;
    out[82] = settings.denoise.hiss.floor_db;
    out[83] = settings.denoise.hiss.sensitivity_db;
    out[84] = settings.denoise.hiss.smoothing;
    out[85] = bool_word(settings.denoise.hum.enabled);
    out[86] = settings.denoise.hum.mode as f64;
    out[87] = settings.denoise.hum.harmonics;
    out[88] = settings.denoise.hum.depth_db;
    out[89] = settings.denoise.hum.quality;
    out[90] = bool_word(settings.denoise.click.enabled);
    out[91] = settings.denoise.click.sensitivity;
    out[92] = settings.denoise.click.max_repair_samples;
    out[93] = bool_word(settings.denoise.voice.enabled);
    out[94] = settings.denoise.voice.mode as f64;
    out[95] = settings.denoise.voice.amount;

    // [96..104]: Bass Forge (8 words)
    out[96] = bool_word(settings.bass_forge.enabled);
    out[97] = bool_word(settings.bass_forge.isolate);
    out[98] = settings.bass_forge.split_hz;
    out[99] = settings.bass_forge.drive_db;
    out[100] = settings.bass_forge.sub_amount;
    out[101] = settings.bass_forge.presence_amount;
    out[102] = settings.bass_forge.texture;
    out[103] = settings.bass_forge.mix;

    // [104..113]: Bass Punch (9 words)
    out[104] = bool_word(settings.bass_punch.enabled);
    out[105] = bool_word(settings.bass_punch.isolate);
    out[106] = settings.bass_punch.split_hz;
    out[107] = settings.bass_punch.attack;
    out[108] = settings.bass_punch.sustain;
    out[109] = settings.bass_punch.bloom_amount;
    out[110] = settings.bass_punch.bloom_decay_ms;
    out[111] = settings.bass_punch.duck;
    out[112] = settings.bass_punch.mix;

    // [113..155]: Room (42 words, with eq.matched at [121])
    out[113] = bool_word(settings.room.enabled);
    out[114] = settings.room.preset as f64;
    out[115] = settings.room.size_m;
    out[116] = settings.room.walls;
    out[117] = settings.room.distance_m;
    out[118] = settings.room.centre_db;
    out[119] = settings.room.sub_db;
    out[120] = settings.room.head as f64;
    out[121] = bool_word(settings.eq.matched);
    out[122..122 + FEQ_ROOM_SPEAKERS].copy_from_slice(&settings.room.angle_deg);
    out[129..129 + FEQ_ROOM_SPEAKERS].copy_from_slice(&settings.room.level_db);
    out[136] = bool_word(settings.room.bass_management);
    out[137] = settings.room.crossover_hz;
    out[138] = bool_word(settings.room.music_upmix);
    out[139] = settings.room.upmix_amount;
    out[140..140 + FEQ_ROOM_SPEAKERS].copy_from_slice(&settings.room.speaker_distance_m);
    for s in 0..FEQ_ROOM_MUTE_SLOTS {
        out[147 + s] = (settings.room.mute[s] & 3) as f64;
    }

    // [155..158]: Surround all channels, Master peak limiting, EQ band count
    out[155] = bool_word(settings.surround_all_channels);
    out[156] = settings.master.peak_limiting_db;
    out[157] = band_count as f64;

    // EQ Bands (`band_count * 7` words)
    for b in 0..band_count {
        let band = &settings.eq.bands[b];
        let switch_word = if band.enabled == 0 {
            0.0
        } else {
            (1 + band.ear.clamp(0, 2)) as f64
        };
        out.push(switch_word);
        out.push(band.filter_type as i32 as f64);
        out.push(band.frequency);
        out.push(band.gain_db);
        out.push(band.quality);
        out.push(bool_word(band.dynamic));
        out.push(band.threshold_db);
    }

    // Normalizer trailer (3 words) + Low-latency trailer (1 word)
    out.push(settings.normalizer.mode as f64);
    out.push(settings.normalizer.ceiling_db);
    out.push(settings.normalizer.target_lufs);
    out.push(bool_word(settings.low_latency));

    // Room v2 trailer (11 words)
    out.push(FEQ_CHAIN_ROOM_TAG);
    out.push(FEQ_CHAIN_ROOM_VERSION);
    out.push(FEQ_CHAIN_ROOM_FIELDS);
    out.push(settings.room.renderer_version as f64);
    out.push(settings.room.early_reflection_db);
    out.push(settings.room.ambience_mix);
    out.push(settings.room.ambience_decay_s);
    out.push(settings.room.ambience_damping_hz);
    out.push(bool_word(settings.room.preserve_position));
    out.push(bool_word(settings.room.compare_original));
    out.push(bool_word(settings.room.source_already_spatial));

    // Footsteps trailer (7 words) — emitted when footsteps is active or customized
    if emit_footsteps {
        out.push(FEQ_CHAIN_FOOTSTEPS_TAG);
        out.push(FEQ_CHAIN_FOOTSTEPS_VERSION);
        out.push(FEQ_CHAIN_FOOTSTEPS_FIELDS);
        out.push(bool_word(settings.footsteps.enabled));
        out.push(bool_word(settings.footsteps.isolate));
        out.push(settings.footsteps.lift);
        out.push(settings.footsteps.calm);
    }

    // Dynamics trailer (23 words) — written only while the stage is on.
    if emit_dynamics {
        out.push(FEQ_CHAIN_DYNAMICS_TAG);
        out.push(FEQ_CHAIN_DYNAMICS_VERSION);
        out.push(FEQ_CHAIN_DYNAMICS_FIELDS);
        out.push(bool_word(settings.compander.enabled));
        for band in &settings.compander.bands {
            out.push(band.up_threshold_db);
            out.push(band.up_ratio);
            out.push(band.up_max_db);
            out.push(band.down_threshold_db);
            out.push(band.down_ratio);
        }
        out.push(settings.compander.attack_ms);
        out.push(settings.compander.release_ms);
        out.push(settings.compander.mix);
        out.push(settings.compander.makeup_db);
    }

    // Tone trailer (`4 + tone_count * 4` words) — C++ `chain_decode.cpp` requires `tone[2] >= 1`
    if tone_count >= 1 {
        out.push(FEQ_CHAIN_TONE_TAG);
        out.push(FEQ_CHAIN_TONE_VERSION);
        out.push(tone_count as f64);
        out.push(bool_word(settings.tone.matched));
        for b in 0..tone_count {
            let tb = &settings.tone.bands[b];
            out.push(tb.filter_type as i32 as f64);
            out.push(tb.frequency);
            out.push(tb.gain_db);
            out.push(tb.quality);
        }
    }

    out
}

/// Decodes a flat `f64` wire slice into `FeqChainSettings`, mirroring
/// `feq_chain_settings_decode` in `/native/dsp-core/src/chain_decode.cpp` word-for-word.
pub fn decode_chain_settings_wire(values: &[f64]) -> Option<FeqChainSettings> {
    if values.len() < FEQ_CHAIN_PARAM_LEAD {
        return None;
    }
    if values.iter().any(|v| !v.is_finite()) {
        return None;
    }

    let raw_bands = values[FEQ_CHAIN_PARAM_LEAD - 1];
    if raw_bands < 0.0
        || raw_bands > (FEQ_CHAIN_MAX_EQ_BANDS as f64)
        || raw_bands.floor() != raw_bands
    {
        return None;
    }
    let band_count = raw_bands as usize;
    let legacy_count = FEQ_CHAIN_PARAM_LEAD + band_count * FEQ_CHAIN_BAND_PARAMS;
    if values.len() < legacy_count {
        return None;
    }

    let tail = values.len() - legacy_count;
    if tail == 1 || tail == 2 {
        return None;
    }

    let mut at = legacy_count;
    let normalizer_present = tail >= 3;
    if normalizer_present {
        let mode = values[at];
        if mode.floor() != mode || !(0.0..=2.0).contains(&mode) {
            return None;
        }
        if !(-12.0..=0.0).contains(&values[at + 1]) {
            return None;
        }
        if !(-30.0..=-5.0).contains(&values[at + 2]) {
            return None;
        }
        at += 3;
    }

    let mut low_latency = 0i32;
    if at < values.len()
        && values[at] != FEQ_CHAIN_ROOM_TAG
        && values[at] != FEQ_CHAIN_TONE_TAG
        && values[at] != FEQ_CHAIN_FOOTSTEPS_TAG
        && values[at] != FEQ_CHAIN_DYNAMICS_TAG
    {
        if values[at] != 0.0 && values[at] != 1.0 {
            return None;
        }
        low_latency = if values[at] >= 0.5 { 1 } else { 0 };
        at += 1;
    }

    let mut extended_room_at: Option<usize> = None;
    if at < values.len() && values[at] == FEQ_CHAIN_ROOM_TAG {
        if values.len() - at < 3
            || values[at + 1] != FEQ_CHAIN_ROOM_VERSION
            || values[at + 2] != FEQ_CHAIN_ROOM_FIELDS
        {
            return None;
        }
        let room_words = 3 + (FEQ_CHAIN_ROOM_FIELDS as usize);
        if values.len() - at < room_words {
            return None;
        }
        let rv = values[at + 3];
        if rv != 1.0 && rv != 2.0 {
            return None;
        }
        if !(-24.0..=6.0).contains(&values[at + 4])
            || !(0.0..=0.35).contains(&values[at + 5])
            || !(0.15..=1.2).contains(&values[at + 6])
            || !(1500.0..=12000.0).contains(&values[at + 7])
        {
            return None;
        }
        for flag in 0..3 {
            let v = values[at + 8 + flag];
            if v != 0.0 && v != 1.0 {
                return None;
            }
        }
        extended_room_at = Some(at);
        at += room_words;
    }

    let mut footsteps_at: Option<usize> = None;
    if at < values.len() && values[at] == FEQ_CHAIN_FOOTSTEPS_TAG {
        if values.len() - at < 3
            || values[at + 1] != FEQ_CHAIN_FOOTSTEPS_VERSION
            || values[at + 2] != FEQ_CHAIN_FOOTSTEPS_FIELDS
        {
            return None;
        }
        let foot_words = 3 + (FEQ_CHAIN_FOOTSTEPS_FIELDS as usize);
        if values.len() - at < foot_words {
            return None;
        }
        if (values[at + 3] != 0.0 && values[at + 3] != 1.0)
            || (values[at + 4] != 0.0 && values[at + 4] != 1.0)
            || !(0.0..=1.0).contains(&values[at + 5])
            || !(0.0..=1.0).contains(&values[at + 6])
        {
            return None;
        }
        footsteps_at = Some(at);
        at += foot_words;
    }

    let mut dynamics_at: Option<usize> = None;
    if at < values.len() && values[at] == FEQ_CHAIN_DYNAMICS_TAG {
        if values.len() - at < FEQ_CHAIN_DYNAMICS_TRAILER
            || values[at + 1] != FEQ_CHAIN_DYNAMICS_VERSION
            || values[at + 2] != FEQ_CHAIN_DYNAMICS_FIELDS
            || (values[at + 3] != 0.0 && values[at + 3] != 1.0)
        {
            return None;
        }
        for band in 0..FEQ_COMPANDER_BANDS {
            let read = &values[at + 4 + band * 5..at + 9 + band * 5];
            if !(-80.0..=-10.0).contains(&read[0])
                || !(1.0..=4.0).contains(&read[1])
                || !(0.0..=18.0).contains(&read[2])
                || !(-60.0..=0.0).contains(&read[3])
                || !(1.0..=20.0).contains(&read[4])
            {
                return None;
            }
        }
        let tail = at + 4 + FEQ_COMPANDER_BANDS * 5;
        if !(1.0..=100.0).contains(&values[tail])
            || !(20.0..=1000.0).contains(&values[tail + 1])
            || !(0.0..=1.0).contains(&values[tail + 2])
            || !(-12.0..=12.0).contains(&values[tail + 3])
        {
            return None;
        }
        dynamics_at = Some(at);
        at += FEQ_CHAIN_DYNAMICS_TRAILER;
    }

    let mut tone_at: Option<(usize, usize)> = None;
    if at < values.len() {
        let remaining = values.len() - at;
        if remaining < 4
            || values[at] != FEQ_CHAIN_TONE_TAG
            || values[at + 1] != FEQ_CHAIN_TONE_VERSION
        {
            return None;
        }
        let raw_tone = values[at + 2];
        if !(1.0..=(FEQ_CHAIN_MAX_TONE_BANDS as f64)).contains(&raw_tone)
            || raw_tone.floor() != raw_tone
        {
            return None;
        }
        let tone_bands = raw_tone as usize;
        if remaining != 4 + tone_bands * 4 {
            return None;
        }
        for b in 0..tone_bands {
            let bt = &values[at + 4 + b * 4..at + 8 + b * 4];
            if bt[0].floor() != bt[0]
                || !(0.0..=6.0).contains(&bt[0])
                || !(10.0..=24000.0).contains(&bt[1])
                || !(-48.0..=48.0).contains(&bt[2])
                || !(0.1..=30.0).contains(&bt[3])
            {
                return None;
            }
        }
        tone_at = Some((at, tone_bands));
        at += remaining;
    }

    if at != values.len() {
        return None;
    }

    // Validate retired multiband slot / De-Esser signature block (`values[40..58]`)
    let deesser_slot = &values[40..58];
    let has_deesser = if read_bool(deesser_slot[0]) != 0 {
        return None;
    } else if deesser_slot[13] == FEQ_CHAIN_DEESSER_SIGNATURE
        && deesser_slot[14] == FEQ_CHAIN_DEESSER_FIELDS
    {
        let mode = deesser_slot[8];
        if mode.floor() != mode || !(0.0..=2.0).contains(&mode) {
            return None;
        }
        let ov = deesser_slot[15];
        if ov != 0.0 && ov != 1.0 && ov != 2.0 && ov != 4.0 {
            return None;
        }
        true
    } else {
        for &word in &deesser_slot[1..18] {
            if word != 0.0 {
                return None;
            }
        }
        false
    };

    let mut settings = FeqChainSettings::default();
    let mut c = 0usize;

    settings.enabled = read_bool(values[c]);
    c += 2; // [1] retired guard slot

    settings.exciter.enabled = read_bool(values[c]);
    settings.exciter.isolate = read_bool(values[c + 1]);
    settings.exciter.stereo = values[c + 2] as i32;
    settings.exciter.align_enabled = read_bool(values[c + 3]);
    settings.exciter.align_amount = values[c + 4];
    settings.exciter.organic_enabled = read_bool(values[c + 5]);
    settings.exciter.organic_amount = values[c + 6];
    settings.exciter.organic_focus_hz = values[c + 7];
    settings.exciter.organic_range = values[c + 8];
    c += 9;

    for b in 0..FEQ_CHAIN_EXCITER_BANDS {
        let band = &mut settings.exciter.bands[b];
        band.enabled = read_bool(values[c]);
        band.freq_hz = values[c + 1];
        band.range = values[c + 2];
        band.drive = values[c + 3];
        band.mix = values[c + 4];
        band.texture = values[c + 5];
        c += 6;
    }

    settings.eq.enabled = read_bool(values[c]);
    settings.eq.isolate = read_bool(values[c + 1]);
    settings.eq.model = values[c + 2] as i32;
    settings.eq.model_amount = values[c + 3];
    settings.eq.engine = values[c + 4] as i32;
    settings.eq.phase = values[c + 5] as i32;
    settings.eq.stereo = values[c + 6] as i32;
    settings.eq.mono_below_hz = values[c + 7];
    settings.eq.oversample = values[c + 8] as u32;
    settings.eq.subsonic_hz = values[c + 9];
    settings.eq.fuzz_amount = values[c + 10];
    c += 11;

    if has_deesser {
        settings.deesser.enabled = read_bool(values[c + 1]);
        settings.deesser.isolate = read_bool(values[c + 2]);
        settings.deesser.sensitivity = values[c + 3];
        settings.deesser.low_hz = values[c + 4];
        settings.deesser.high_hz = values[c + 5];
        settings.deesser.range_db = values[c + 6];
        settings.deesser.width_oct = values[c + 7];
        settings.deesser.mode = values[c + 8] as i32;
        settings.deesser.attack_ms = values[c + 9];
        settings.deesser.release_ms = values[c + 10];
        settings.deesser.centre_only = read_bool(values[c + 11]);
        settings.deesser.keep_cymbals = values[c + 12];
        settings.exciter.oversampling = values[c + 15] as i32;
    }
    c += 18;

    settings.dimension.enabled = read_bool(values[c]);
    settings.dimension.low_width = values[c + 1];
    settings.dimension.mid_width = values[c + 2];
    settings.dimension.high_width = values[c + 3];
    settings.dimension.low_hz = values[c + 4];
    settings.dimension.high_hz = values[c + 5];
    settings.dimension.decorrelation = values[c + 6];
    c += 7;

    settings.maximizer.enabled = read_bool(values[c]);
    settings.maximizer.drive_db = values[c + 1];
    settings.maximizer.ceiling_db = values[c + 2];
    settings.maximizer.look_ahead_ms = values[c + 3];
    settings.maximizer.release_ms = values[c + 4];
    c += 5;

    settings.master.enabled = read_bool(values[c]);
    settings.master.output_trim_db = values[c + 1];
    settings.master.loudness_maximize = read_bool(values[c + 2]);
    settings.master.loudness_target_lufs = values[c + 3];
    settings.master.ceiling_db = values[c + 4];
    settings.master.release_ms = values[c + 5];
    settings.master.matched_bypass = read_bool(values[c + 6]);
    c += 7;

    settings.denoise.enabled = read_bool(values[c]);
    settings.denoise.isolate = read_bool(values[c + 1]);
    settings.denoise.profile_source = values[c + 2] as i32;
    settings.denoise.hiss.enabled = read_bool(values[c + 3]);
    settings.denoise.hiss.amount = values[c + 4];
    settings.denoise.hiss.floor_db = values[c + 5];
    settings.denoise.hiss.sensitivity_db = values[c + 6];
    settings.denoise.hiss.smoothing = values[c + 7];
    settings.denoise.hum.enabled = read_bool(values[c + 8]);
    settings.denoise.hum.mode = values[c + 9] as i32;
    settings.denoise.hum.harmonics = values[c + 10];
    settings.denoise.hum.depth_db = values[c + 11];
    settings.denoise.hum.quality = values[c + 12];
    settings.denoise.click.enabled = read_bool(values[c + 13]);
    settings.denoise.click.sensitivity = values[c + 14];
    settings.denoise.click.max_repair_samples = values[c + 15];
    settings.denoise.voice.enabled = read_bool(values[c + 16]);
    settings.denoise.voice.mode = values[c + 17] as i32;
    settings.denoise.voice.amount = values[c + 18];
    c += 19;

    settings.bass_forge.enabled = read_bool(values[c]);
    settings.bass_forge.isolate = read_bool(values[c + 1]);
    settings.bass_forge.split_hz = values[c + 2];
    settings.bass_forge.drive_db = values[c + 3];
    settings.bass_forge.sub_amount = values[c + 4];
    settings.bass_forge.presence_amount = values[c + 5];
    settings.bass_forge.texture = values[c + 6];
    settings.bass_forge.mix = values[c + 7];
    c += 8;

    settings.bass_punch.enabled = read_bool(values[c]);
    settings.bass_punch.isolate = read_bool(values[c + 1]);
    settings.bass_punch.split_hz = values[c + 2];
    settings.bass_punch.attack = values[c + 3];
    settings.bass_punch.sustain = values[c + 4];
    settings.bass_punch.bloom_amount = values[c + 5];
    settings.bass_punch.bloom_decay_ms = values[c + 6];
    settings.bass_punch.duck = values[c + 7];
    settings.bass_punch.mix = values[c + 8];
    c += 9;

    settings.room.enabled = read_bool(values[c]);
    settings.room.preset = values[c + 1] as i32;
    settings.room.size_m = values[c + 2];
    settings.room.walls = values[c + 3];
    settings.room.distance_m = values[c + 4];
    settings.room.centre_db = values[c + 5];
    settings.room.sub_db = values[c + 6];
    settings.room.head = values[c + 7] as i32;
    settings.eq.matched = read_bool(values[c + 8]);
    for s in 0..FEQ_ROOM_SPEAKERS {
        settings.room.angle_deg[s] = values[c + 9 + s];
    }
    for s in 0..FEQ_ROOM_SPEAKERS {
        settings.room.level_db[s] = values[c + 16 + s];
    }
    settings.room.bass_management = read_bool(values[c + 23]);
    settings.room.crossover_hz = values[c + 24];
    settings.room.music_upmix = read_bool(values[c + 25]);
    settings.room.upmix_amount = values[c + 26];
    for s in 0..FEQ_ROOM_SPEAKERS {
        settings.room.speaker_distance_m[s] = values[c + 27 + s];
    }
    for s in 0..FEQ_ROOM_MUTE_SLOTS {
        let v = values[c + 34 + s];
        settings.room.mute[s] = if v == 0.0 {
            0
        } else if v == 2.0 {
            2
        } else if v == 3.0 {
            3
        } else {
            1
        };
    }
    c += 42;

    settings.surround_all_channels = read_bool(values[c]);
    settings.master.peak_limiting_db = values[c + 1];
    c += 3; // [157] band_count already read

    settings.eq.band_count = band_count as u32;
    for b in 0..band_count {
        let band = &mut settings.eq.bands[b];
        let on = values[c];
        band.enabled = if on != 0.0 { 1 } else { 0 };
        band.ear = if on == 2.0 {
            1
        } else if on == 3.0 {
            2
        } else {
            0
        };
        band.filter_type = FeqFilterType::from_wire(values[c + 1] as i32);
        band.frequency = values[c + 2];
        band.gain_db = values[c + 3];
        band.quality = values[c + 4];
        band.dynamic = read_bool(values[c + 5]);
        band.threshold_db = values[c + 6];
        c += FEQ_CHAIN_BAND_PARAMS;
    }

    if normalizer_present {
        settings.normalizer.mode = values[legacy_count] as i32;
        settings.normalizer.ceiling_db = values[legacy_count + 1];
        settings.normalizer.target_lufs = values[legacy_count + 2];
    }

    settings.low_latency = low_latency;

    if let Some(rc) = extended_room_at {
        settings.room.renderer_version = values[rc + 3] as i32;
        settings.room.early_reflection_db = values[rc + 4];
        settings.room.ambience_mix = values[rc + 5];
        settings.room.ambience_decay_s = values[rc + 6];
        settings.room.ambience_damping_hz = values[rc + 7];
        settings.room.preserve_position = read_bool(values[rc + 8]);
        settings.room.compare_original = read_bool(values[rc + 9]);
        settings.room.source_already_spatial = read_bool(values[rc + 10]);
    }

    if let Some(fc) = footsteps_at {
        settings.footsteps.enabled = read_bool(values[fc + 3]);
        settings.footsteps.isolate = read_bool(values[fc + 4]);
        settings.footsteps.lift = values[fc + 5];
        settings.footsteps.calm = values[fc + 6];
    }

    if let Some(dc) = dynamics_at {
        settings.compander.enabled = read_bool(values[dc + 3]);
        for band in 0..FEQ_COMPANDER_BANDS {
            let read = dc + 4 + band * 5;
            settings.compander.bands[band].up_threshold_db = values[read];
            settings.compander.bands[band].up_ratio = values[read + 1];
            settings.compander.bands[band].up_max_db = values[read + 2];
            settings.compander.bands[band].down_threshold_db = values[read + 3];
            settings.compander.bands[band].down_ratio = values[read + 4];
        }
        let tail = dc + 4 + FEQ_COMPANDER_BANDS * 5;
        settings.compander.attack_ms = values[tail];
        settings.compander.release_ms = values[tail + 1];
        settings.compander.mix = values[tail + 2];
        settings.compander.makeup_db = values[tail + 3];
    }

    if let Some((tc, tone_bands)) = tone_at {
        settings.tone.band_count = tone_bands as u32;
        settings.tone.matched = read_bool(values[tc + 3]);
        for b in 0..tone_bands {
            let bt = &values[tc + 4 + b * 4..tc + 8 + b * 4];
            let tb = &mut settings.tone.bands[b];
            tb.filter_type = FeqFilterType::from_wire(bt[0] as i32);
            tb.frequency = bt[1];
            tb.gain_db = bt[2];
            tb.quality = bt[3];
        }
    }

    Some(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_chain_settings_wire_roundtrip_without_empty_tone_trailer() {
        let defaults = FeqChainSettings::default();
        let encoded = encode_chain_settings_wire(&defaults);
        // Default has `tone.band_count == 0` and `footsteps.enabled == 0`, so neither `FEQ_CHAIN_TONE_TAG`
        // nor `FEQ_CHAIN_FOOTSTEPS_TAG` should be emitted on the wire.
        assert!(!encoded.contains(&FEQ_CHAIN_TONE_TAG));
        assert!(!encoded.contains(&FEQ_CHAIN_FOOTSTEPS_TAG));
        assert!(!encoded.contains(&FEQ_CHAIN_DYNAMICS_TAG));
        let decoded = decode_chain_settings_wire(&encoded)
            .expect("default FeqChainSettings wire must decode cleanly");
        assert_eq!(decoded, defaults);
    }

    #[test]
    fn dynamics_trailer_round_trips_its_three_bands() {
        let mut original = FeqChainSettings::default();
        original.compander.enabled = 1;
        original.compander.bands[0].up_threshold_db = -48.0;
        original.compander.bands[0].up_ratio = 2.5;
        original.compander.bands[0].up_max_db = 9.0;
        original.compander.bands[1].down_threshold_db = -14.0;
        original.compander.bands[1].down_ratio = 4.0;
        original.compander.bands[2].up_ratio = 1.5;
        original.compander.bands[2].down_ratio = 8.0;
        original.compander.attack_ms = 25.0;
        original.compander.release_ms = 350.0;
        original.compander.mix = 0.75;
        original.compander.makeup_db = -3.0;

        let encoded = encode_chain_settings_wire(&original);
        assert_eq!(
            encoded
                .iter()
                .filter(|word| **word == FEQ_CHAIN_DYNAMICS_TAG)
                .count(),
            1
        );
        let decoded =
            decode_chain_settings_wire(&encoded).expect("dynamics wire must decode cleanly");
        assert_eq!(decoded.compander, original.compander);
    }

    #[test]
    fn roundtrip_full_11_stage_chain_settings_wire() {
        let mut original = FeqChainSettings::default();
        original.low_latency = 1;
        original.surround_all_channels = 1;
        original.denoise.enabled = 1;
        original.denoise.hiss.amount = 0.72;
        original.denoise.hum.harmonics = 8.0;
        original.denoise.click.max_repair_samples = 48.0;
        original.deesser.enabled = 1;
        original.deesser.range_db = 11.5;
        original.deesser.keep_cymbals = 0.65;
        original.footsteps.enabled = 1;
        original.footsteps.lift = 0.68;
        original.exciter.enabled = 1;
        original.exciter.oversampling = 2;
        original.exciter.organic_focus_hz = 110.0;
        original.exciter.organic_range = 1.5;
        for b in 0..FEQ_CHAIN_EXCITER_BANDS {
            original.exciter.bands[b].enabled = 1;
            original.exciter.bands[b].freq_hz = 120.0 * ((b + 1) as f64);
            original.exciter.bands[b].range = 1.0;
            original.exciter.bands[b].drive = 0.25;
            original.exciter.bands[b].mix = 0.3;
            original.exciter.bands[b].texture = 0.5;
        }
        original.bass_forge.enabled = 1;
        original.bass_forge.sub_amount = 0.85;
        original.bass_punch.enabled = 1;
        original.bass_punch.attack = 0.6;
        original.dimension.enabled = 1;
        original.dimension.low_hz = 180.0;
        original.dimension.high_hz = 3200.0;
        original.dimension.high_width = 1.45;
        original.room.enabled = 1;
        original.room.renderer_version = 2;
        original.room.early_reflection_db = -7.5;
        original.room.ambience_mix = 0.14;
        original.room.ambience_decay_s = 0.35;
        original.room.ambience_damping_hz = 5200.0;
        original.room.preserve_position = 1;
        original.maximizer.enabled = 1;
        original.maximizer.drive_db = 4.2;
        original.master.output_trim_db = -1.5;
        original.normalizer.mode = 2;
        original.normalizer.target_lufs = -13.0;

        original.eq.matched = 1;
        original.eq.band_count = 2;
        original.eq.bands[0].enabled = 1;
        original.eq.bands[0].ear = 1; // Left ear only
        original.eq.bands[0].filter_type = FeqFilterType::FEQ_FILTER_LSC;
        original.eq.bands[0].frequency = 65.0;
        original.eq.bands[0].gain_db = 4.5;
        original.eq.bands[0].quality = 0.71;
        original.eq.bands[0].threshold_db = -18.0;

        original.eq.bands[1].enabled = 1;
        original.eq.bands[1].ear = 0; // Stereo
        original.eq.bands[1].filter_type = FeqFilterType::FEQ_FILTER_PK;
        original.eq.bands[1].frequency = 3200.0;
        original.eq.bands[1].gain_db = -2.5;
        original.eq.bands[1].quality = 2.0;
        original.eq.bands[1].dynamic = 1;
        original.eq.bands[1].threshold_db = -21.0;

        original.tone.band_count = 1;
        original.tone.matched = 1;
        original.tone.bands[0].filter_type = FeqFilterType::FEQ_FILTER_HSC;
        original.tone.bands[0].frequency = 8500.0;
        original.tone.bands[0].gain_db = 2.0;
        original.tone.bands[0].quality = 0.71;

        let encoded = encode_chain_settings_wire(&original);
        let decoded = decode_chain_settings_wire(&encoded).expect("wire decode should succeed");

        assert_eq!(decoded, original);

        #[cfg(feature = "link-cpp-core")]
        unsafe {
            let mut cpp_decoded = std::mem::zeroed::<FeqChainSettings>();
            let ok = fluideq_dsp_sys::feq_chain_settings_decode(
                encoded.as_ptr(),
                encoded.len() as u32,
                &mut cpp_decoded,
            );
            assert_eq!(ok, 1, "C++ feq_chain_settings_decode must accept encoded wire");
            assert_eq!(cpp_decoded, decoded);
        }
    }
}

