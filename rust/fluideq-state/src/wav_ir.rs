/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Zero-Dependency RIFF/WAVE & FORM/AIFF(-C) Impulse Response and PCM Decoder/Encoder.
//!
//! Ported from `/native/dsp-host/src/decoders/pcm_decoder.cpp` and `/native/dsp-host/src/host_commands.cpp`:
//! - Supports RIFF/WAVE: 8-bit unsigned PCM (128 = silence), 16/24/32-bit signed PCM,
//!   32/64-bit IEEE Float, and 40-byte `WAVE_FORMAT_EXTENSIBLE` (`0xFFFE`) subformat GUIDs.
//! - Supports Apple FORM/AIFF and FORM/AIFC: 80-bit IEEE extended-precision float sample rate
//!   (`read_extended80`), big-endian `COMM` & `SSND` chunks, and AIFF-C codecs `NONE`,
//!   `sowt` (little-endian PCM in big-endian container), and `fl32`/`FL32`.
//! - Enforces hardware safety bounds (`MAX_FILE_CHANNELS = 32`, `MAX_FILE_RATE = 768_000`)
//!   to prevent malformed headers from triggering multi-gigabyte allocations.
//! - Provides `encode_float32_wav` (matching `write_float_wav` in `host_commands.cpp`)
//!   for bit-accurate offline render comparison without 16-bit quantization.

use serde::{Deserialize, Serialize};

pub const WAVE_FORMAT_PCM: u16 = 0x0001;
pub const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
pub const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

pub const MAX_FILE_CHANNELS: u16 = 32;
pub const MAX_FILE_RATE: u32 = 768_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SampleEncoding {
    UnsignedByte,
    SignedInt { big_endian: bool, bytes: usize },
    Float32 { big_endian: bool },
    Float64 { big_endian: bool },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecodedImpulseResponse {
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: usize,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

/// What the window needs to show about a loaded impulse response: long enough
/// to recognise the file, without shipping the samples back over the IPC.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IrSummary {
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: usize,
    pub seconds: f64,
    pub left_peak_db: f64,
    pub right_peak_db: f64,
}

fn peak_db(samples: &[f32]) -> f64 {
    let peak = samples
        .iter()
        .fold(0.0_f32, |worst, &sample| worst.max(sample.abs()));
    if peak <= 0.0 {
        -120.0
    } else {
        (20.0 * f64::from(peak).log10()).max(-120.0)
    }
}

impl IrSummary {
    pub fn from_decoded(decoded: &DecodedImpulseResponse) -> Self {
        Self {
            sample_rate: decoded.sample_rate,
            channels: decoded.channels,
            frames: decoded.frames,
            seconds: if decoded.sample_rate == 0 {
                0.0
            } else {
                decoded.frames as f64 / f64::from(decoded.sample_rate)
            },
            left_peak_db: peak_db(&decoded.left),
            right_peak_db: peak_db(&decoded.right),
        }
    }
}

/// Decodes an 80-bit IEEE extended precision float (`SANE` / Apple AIFF `COMM` sample rate).
/// Exact port of `read_extended80` in `/native/dsp-host/src/decoders/pcm_decoder.cpp`.
pub fn read_extended80(at: &[u8; 10]) -> f64 {
    let sign = if (at[0] & 0x80) != 0 { -1.0 } else { 1.0 };
    let exponent = (((at[0] & 0x7F) as i32) << 8) | (at[1] as i32);
    let mut mantissa: u64 = 0;
    for b in &at[2..10] {
        mantissa = (mantissa << 8) | (*b as u64);
    }
    if (exponent == 0 && mantissa == 0) || exponent == 0x7FFF {
        return 0.0;
    }
    sign * (mantissa as f64) * 2.0_f64.powi(exponent - 16383 - 63)
}

/// Encodes a 48_000 Hz or arbitrary positive sample rate into an 80-bit IEEE extended float for testing/headers.
pub fn write_extended80(rate: u32) -> [u8; 10] {
    if rate == 0 {
        return [0u8; 10];
    }
    let leading = rate.leading_zeros();
    let shift = 31 - leading; // 0..31
    let exponent = (16383 + shift) as u16;
    let mantissa = (rate as u64) << (63 - shift);
    let mut out = [0u8; 10];
    out[0..2].copy_from_slice(&exponent.to_be_bytes());
    out[2..10].copy_from_slice(&mantissa.to_be_bytes());
    out
}

/// Inspects the first 12 magic bytes of a file/buffer (`RIFF....WAVE` or `FORM....AIFF`/`AIFC`),
/// matching `feq_decoder_handles` in `/native/dsp-host/src/decoders/pcm_decoder.cpp`.
pub fn handles_pcm_magic(bytes: &[u8]) -> bool {
    if bytes.len() < 12 {
        return false;
    }
    let is_wav = &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE";
    let is_aiff = &bytes[0..4] == b"FORM" && (&bytes[8..12] == b"AIFF" || &bytes[8..12] == b"AIFC");
    is_wav || is_aiff
}

/// Unified entry point that decodes either RIFF/WAVE or FORM/AIFF(-C) into planar stereo `f32` buffers.
pub fn decode_pcm_impulse_response(bytes: &[u8]) -> Result<DecodedImpulseResponse, String> {
    if bytes.len() >= 12 && &bytes[0..4] == b"FORM" && (&bytes[8..12] == b"AIFF" || &bytes[8..12] == b"AIFC") {
        decode_aiff_impulse_response(bytes)
    } else {
        decode_wav_impulse_response(bytes)
    }
}

/// Parses a RIFF/WAVE byte slice into planar `left` and `right` `f32` impulse buffers.
pub fn decode_wav_impulse_response(bytes: &[u8]) -> Result<DecodedImpulseResponse, String> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("Not a valid RIFF/WAVE file".to_string());
    }

    let mut cursor = 12usize;
    let mut have_format = false;
    let mut format_tag: u16 = 0;
    let mut channels: u16 = 0;
    let mut sample_rate: u32 = 0;
    let mut bits_per_sample: u16 = 0;
    let mut data_slice: Option<&[u8]> = None;

    while cursor + 8 <= bytes.len() {
        let chunk_id = &bytes[cursor..cursor + 4];
        let chunk_size = u32::from_le_bytes([
            bytes[cursor + 4],
            bytes[cursor + 5],
            bytes[cursor + 6],
            bytes[cursor + 7],
        ]) as usize;
        let chunk_start = cursor + 8;
        let chunk_end = chunk_start.saturating_add(chunk_size).min(bytes.len());

        if chunk_id == b"fmt " && chunk_size >= 16 && chunk_end <= bytes.len() {
            let fmt = &bytes[chunk_start..chunk_end];
            format_tag = u16::from_le_bytes([fmt[0], fmt[1]]);
            channels = u16::from_le_bytes([fmt[2], fmt[3]]);
            sample_rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
            bits_per_sample = u16::from_le_bytes([fmt[14], fmt[15]]);
            if format_tag == WAVE_FORMAT_EXTENSIBLE && fmt.len() >= 40 {
                format_tag = u16::from_le_bytes([fmt[24], fmt[25]]);
            }
            have_format = true;
            if data_slice.is_some() {
                break;
            }
        } else if chunk_id == b"data" && chunk_end <= bytes.len() {
            data_slice = Some(&bytes[chunk_start..chunk_end]);
            if have_format {
                break;
            }
        }

        // RIFF chunks are word-aligned (padded to even byte boundary)
        let padded = chunk_size + (chunk_size & 1);
        cursor = chunk_start.saturating_add(padded);
    }

    let raw_data = data_slice.ok_or_else(|| "WAVE file missing 'data' chunk".to_string())?;
    if !have_format
        || channels == 0
        || channels > MAX_FILE_CHANNELS
        || sample_rate == 0
        || sample_rate > MAX_FILE_RATE
        || raw_data.is_empty()
    {
        return Err("Invalid or out-of-bounds WAVE fmt header".to_string());
    }

    let encoding = match (format_tag, bits_per_sample) {
        (WAVE_FORMAT_PCM, 8) => SampleEncoding::UnsignedByte,
        (WAVE_FORMAT_PCM, 16 | 24 | 32) => SampleEncoding::SignedInt {
            big_endian: false,
            bytes: (bits_per_sample / 8) as usize,
        },
        (WAVE_FORMAT_IEEE_FLOAT, 32) => SampleEncoding::Float32 { big_endian: false },
        (WAVE_FORMAT_IEEE_FLOAT, 64) => SampleEncoding::Float64 { big_endian: false },
        _ => {
            return Err(format!(
                "Unsupported WAVE format_tag={} bits={}",
                format_tag, bits_per_sample
            ))
        }
    };

    let bytes_per_sample = (bits_per_sample / 8) as usize;
    let frame_stride = bytes_per_sample * (channels as usize);
    let frames = raw_data.len() / frame_stride;
    if frames == 0 {
        return Err("WAVE file contains 0 complete frames".to_string());
    }

    decode_planar_stereo(raw_data, frames, channels, bytes_per_sample, sample_rate, encoding)
}

/// Parses an Apple `FORM`/`AIFF` or `FORM`/`AIFC` byte slice into planar `left` and `right` `f32` buffers.
pub fn decode_aiff_impulse_response(bytes: &[u8]) -> Result<DecodedImpulseResponse, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"FORM" {
        return Err("Not a valid FORM/AIFF file".to_string());
    }
    let compressed = &bytes[8..12] == b"AIFC";
    if !compressed && &bytes[8..12] != b"AIFF" {
        return Err("Unsupported FORM type (expected AIFF or AIFC)".to_string());
    }

    let mut cursor = 12usize;
    let mut have_common = false;
    let mut channels: u16 = 0;
    let mut total_frames: usize = 0;
    let mut bits_per_sample: u16 = 0;
    let mut sample_rate: u32 = 0;
    let mut big_endian = true;
    let mut is_float32 = false;
    let mut ssnd_slice: Option<&[u8]> = None;

    while cursor + 8 <= bytes.len() {
        let chunk_id = &bytes[cursor..cursor + 4];
        let chunk_size = u32::from_be_bytes([
            bytes[cursor + 4],
            bytes[cursor + 5],
            bytes[cursor + 6],
            bytes[cursor + 7],
        ]) as usize;
        let chunk_start = cursor + 8;
        let chunk_end = chunk_start.saturating_add(chunk_size).min(bytes.len());

        if chunk_id == b"COMM" && chunk_size >= 18 && chunk_end <= bytes.len() {
            let comm = &bytes[chunk_start..chunk_end];
            channels = u16::from_be_bytes([comm[0], comm[1]]);
            total_frames = u32::from_be_bytes([comm[2], comm[3], comm[4], comm[5]]) as usize;
            bits_per_sample = u16::from_be_bytes([comm[6], comm[7]]);
            let mut ext = [0u8; 10];
            ext.copy_from_slice(&comm[8..18]);
            let rate = read_extended80(&ext);
            sample_rate = if rate > 0.0 && rate <= (MAX_FILE_RATE as f64) {
                rate.round() as u32
            } else {
                0
            };

            if compressed && comm.len() >= 22 {
                let codec = &comm[18..22];
                if codec == b"sowt" {
                    big_endian = false;
                } else if codec == b"fl32" || codec == b"FL32" {
                    bits_per_sample = 32;
                    is_float32 = true;
                } else if codec != b"NONE" {
                    return Err("Unsupported AIFF-C codec".to_string());
                }
            }
            have_common = true;
            if ssnd_slice.is_some() {
                break;
            }
        } else if chunk_id == b"SSND" && chunk_size >= 8 && chunk_end <= bytes.len() {
            let ssnd = &bytes[chunk_start..chunk_end];
            let offset = u32::from_be_bytes([ssnd[0], ssnd[1], ssnd[2], ssnd[3]]) as usize;
            let data_start = 8usize.saturating_add(offset);
            if data_start <= ssnd.len() {
                ssnd_slice = Some(&ssnd[data_start..]);
            }
            if have_common {
                break;
            }
        }

        let padded = chunk_size + (chunk_size & 1);
        cursor = chunk_start.saturating_add(padded);
    }

    let raw_data = ssnd_slice.ok_or_else(|| "AIFF file missing 'SSND' chunk".to_string())?;
    if !have_common
        || channels == 0
        || channels > MAX_FILE_CHANNELS
        || sample_rate == 0
        || total_frames == 0
    {
        return Err("Invalid or out-of-bounds AIFF COMM header".to_string());
    }

    let encoding = if is_float32 {
        SampleEncoding::Float32 { big_endian }
    } else if matches!(bits_per_sample, 8 | 16 | 24 | 32) {
        SampleEncoding::SignedInt {
            big_endian,
            bytes: (bits_per_sample / 8) as usize,
        }
    } else {
        return Err(format!("Unsupported AIFF bits_per_sample={}", bits_per_sample));
    };

    let bytes_per_sample = (bits_per_sample / 8) as usize;
    let frame_stride = bytes_per_sample * (channels as usize);
    let available_frames = (raw_data.len() / frame_stride).min(total_frames);
    if available_frames == 0 {
        return Err("AIFF file contains 0 readable frames".to_string());
    }

    decode_planar_stereo(
        raw_data,
        available_frames,
        channels,
        bytes_per_sample,
        sample_rate,
        encoding,
    )
}

fn decode_planar_stereo(
    raw_data: &[u8],
    frames: usize,
    channels: u16,
    bytes_per_sample: usize,
    sample_rate: u32,
    encoding: SampleEncoding,
) -> Result<DecodedImpulseResponse, String> {
    let frame_stride = bytes_per_sample * (channels as usize);
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);

    for f in 0..frames {
        let base = f * frame_stride;
        let l_sample = decode_sample(&raw_data[base..base + bytes_per_sample], encoding);
        let r_sample = if channels >= 2 {
            let r_base = base + bytes_per_sample;
            decode_sample(&raw_data[r_base..r_base + bytes_per_sample], encoding)
        } else {
            // Mono is duplicated into right channel so a mono IR/track never plays left-only
            l_sample
        };
        left.push(l_sample);
        right.push(r_sample);
    }

    Ok(DecodedImpulseResponse {
        sample_rate,
        channels,
        frames,
        left,
        right,
    })
}

#[inline]
fn decode_sample(at: &[u8], encoding: SampleEncoding) -> f32 {
    let v_f64 = match encoding {
        SampleEncoding::UnsignedByte => ((at[0] as f64) - 128.0) / 128.0,
        SampleEncoding::Float32 { big_endian } => {
            let bits = if big_endian {
                u32::from_be_bytes([at[0], at[1], at[2], at[3]])
            } else {
                u32::from_le_bytes([at[0], at[1], at[2], at[3]])
            };
            f32::from_bits(bits) as f64
        }
        SampleEncoding::Float64 { big_endian } => {
            let mut arr = [0u8; 8];
            arr.copy_from_slice(&at[0..8]);
            let bits = if big_endian {
                u64::from_be_bytes(arr)
            } else {
                u64::from_le_bytes(arr)
            };
            f64::from_bits(bits)
        }
        SampleEncoding::SignedInt { big_endian, bytes } => {
            let mut value: i32 = 0;
            for index in 0..bytes {
                let byte_idx = if big_endian { index } else { bytes - 1 - index };
                value = (value << 8) | (at[byte_idx] as i32);
            }
            let bits = (bytes * 8) as u32;
            if bits < 32 {
                let sign_bit = 1i32 << (bits - 1);
                if (value & sign_bit) != 0 {
                    value -= 1i32 << bits;
                }
                (value as f64) / (sign_bit as f64)
            } else {
                (value as f64) / 2_147_483_648.0
            }
        }
    };
    if v_f64.is_finite() {
        v_f64 as f32
    } else {
        0.0
    }
}

/// Encodes interleaved `f32` audio into a 44-byte header + IEEE Float32 RIFF/WAVE byte vector.
/// Exact port of `write_float_wav` in `/native/dsp-host/src/host_commands.cpp`.
pub fn encode_float32_wav(interleaved: &[f32], sample_rate: u32, channels: u16) -> Vec<u8> {
    let data_bytes = (interleaved.len() * 4) as u32;
    let byte_rate = sample_rate * (channels as u32) * 4;
    let block_align = channels * 4;
    let riff_size = 36u32.saturating_add(data_bytes);

    let mut out = Vec::with_capacity(44 + interleaved.len() * 4);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&WAVE_FORMAT_IEEE_FLOAT.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());

    for &sample in interleaved {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

// ============================================================================
// Compressed Audio Content Magic Sniffer, Header Parsers, Channel Fold-Down &
// Media Foundation Remainder Buffer (`compressed_decoder.cpp`, `media_foundation_decoder.cpp`)
// ============================================================================

/// Codec classification by content magic rather than file extension.
///
/// Mirrors `Codec` in `/native/dsp-host/src/decoders/compressed_decoder.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompressedCodec {
    None,
    Mp3,
    Flac,
    Vorbis,
}

/// Inspects the first 4 bytes of a stream/file by content rather than extension.
///
/// - Fewer than 4 bytes -> `CompressedCodec::None`
/// - `"fLaC"` -> `CompressedCodec::Flac`
/// - `"OggS"` -> `CompressedCodec::Vorbis`
/// - Anything else >= 4 bytes -> `CompressedCodec::Mp3` (since an ID3v2 tag can push the first
///   MPEG frame arbitrarily far in, `dr_mp3` scans for a valid frame header).
///
/// Exact port of `sniff` in `/native/dsp-host/src/decoders/compressed_decoder.cpp`.
pub fn sniff_compressed_codec(header: &[u8]) -> CompressedCodec {
    if header.len() < 4 {
        return CompressedCodec::None;
    }
    if &header[0..4] == b"fLaC" {
        return CompressedCodec::Flac;
    }
    if &header[0..4] == b"OggS" {
        return CompressedCodec::Vorbis;
    }
    CompressedCodec::Mp3
}

/// Reads the first 4 bytes of `path` on disk and classifies its `CompressedCodec`.
pub fn sniff_compressed_codec_from_file(path: &std::path::Path) -> CompressedCodec {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return CompressedCodec::None;
    };
    let mut header = [0u8; 4];
    let Ok(got) = file.read(&mut header) else {
        return CompressedCodec::None;
    };
    sniff_compressed_codec(&header[..got])
}

/// Stream format metadata extracted from a compressed audio header (`FeqDecoderInfo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompressedStreamSpec {
    pub codec: CompressedCodec,
    pub file_channels: u32,
    pub out_channels: u32,
    pub sample_rate: u32,
    pub total_frames: u64,
}

/// Parses a native FLAC `fLaC` + `STREAMINFO` (block type 0, 34 bytes) header.
pub fn parse_flac_streaminfo_header(bytes: &[u8], out_channels: u32) -> Option<CompressedStreamSpec> {
    if bytes.len() < 42 || &bytes[0..4] != b"fLaC" {
        return None;
    }
    let block_type = bytes[4] & 0x7F;
    let block_len = ((bytes[5] as usize) << 16) | ((bytes[6] as usize) << 8) | (bytes[7] as usize);
    if block_type != 0 || block_len < 34 || bytes.len() < 8 + block_len {
        return None;
    }
    // STREAMINFO bytes 10..18 (file offset 18..26):
    // [20 bits sample rate][3 bits (channels - 1)][5 bits (bits_per_sample - 1)][36 bits total samples]
    let b = &bytes[18..26];
    let sample_rate = ((b[0] as u32) << 12) | ((b[1] as u32) << 4) | ((b[2] as u32) >> 4);
    let file_channels = (((b[2] >> 1) & 0x07) as u32) + 1;
    let total_frames = (((b[3] & 0x0F) as u64) << 32)
        | ((b[4] as u64) << 24)
        | ((b[5] as u64) << 16)
        | ((b[6] as u64) << 8)
        | (b[7] as u64);
    if sample_rate == 0 || file_channels == 0 {
        return None;
    }
    Some(CompressedStreamSpec {
        codec: CompressedCodec::Flac,
        file_channels,
        out_channels: if out_channels > 0 { out_channels } else { 2 },
        sample_rate,
        total_frames,
    })
}

/// Parses an Ogg Vorbis identification header packet (`\x01vorbis`) inside an `OggS` stream.
pub fn parse_ogg_vorbis_ident_header(bytes: &[u8], out_channels: u32) -> Option<CompressedStreamSpec> {
    if bytes.len() < 58 || &bytes[0..4] != b"OggS" {
        return None;
    }
    let pos = bytes.windows(7).position(|w| w == b"\x01vorbis")?;
    let ident = &bytes[pos..];
    if ident.len() < 16 {
        return None;
    }
    let version = u32::from_le_bytes([ident[7], ident[8], ident[9], ident[10]]);
    let file_channels = ident[11] as u32;
    let sample_rate = u32::from_le_bytes([ident[12], ident[13], ident[14], ident[15]]);
    if version != 0 || file_channels == 0 || sample_rate == 0 {
        return None;
    }
    Some(CompressedStreamSpec {
        codec: CompressedCodec::Vorbis,
        file_channels,
        out_channels: if out_channels > 0 { out_channels } else { 2 },
        sample_rate,
        total_frames: 0,
    })
}

/// De-interleaves `produced` frames of `file_channels` audio into `out_channels` planar buffers,
/// duplicating mono across all output channels and clamping/dropping excess channels without
/// inventing an uncalibrated surround matrix.
///
/// Exact port of the channel mapping loop in `compressed_read` (`compressed_decoder.cpp`)
/// and `media_read` (`media_foundation_decoder.cpp`).
pub fn deinterleave_and_map_channels(
    interleaved: &[f32],
    file_channels: u32,
    out_channels: u32,
    produced: u32,
    channels_out: &mut [Vec<f32>],
) -> u32 {
    if file_channels == 0 || out_channels == 0 || produced == 0 {
        return 0;
    }
    let fc = file_channels as usize;
    let oc = (out_channels as usize).min(channels_out.len());
    let avail_frames = (interleaved.len() / fc).min(produced as usize);

    for ch in channels_out.iter_mut().take(oc) {
        if ch.len() < avail_frames {
            ch.resize(avail_frames, 0.0);
        }
    }

    for frame in 0..avail_frames {
        let base = frame * fc;
        for (channel, out_plane) in channels_out.iter_mut().enumerate().take(oc) {
            let from = if fc == 1 {
                0
            } else if channel < fc {
                channel
            } else {
                fc - 1
            };
            out_plane[frame] = interleaved[base + from];
        }
    }

    avail_frames as u32
}

/// Media Foundation counts time in 100-nanosecond units everywhere.
pub const MF_HUNDRED_NANOS_PER_SECOND: f64 = 10_000_000.0;

/// Converts a Media Foundation `MF_PD_DURATION` (in 100-ns units) to total PCM frames.
pub fn mf_duration_frames_from_100ns(hundred_nanos: i64, sample_rate: u32) -> u64 {
    if hundred_nanos <= 0 || sample_rate == 0 {
        return 0;
    }
    (((hundred_nanos as f64) / MF_HUNDRED_NANOS_PER_SECOND) * f64::from(sample_rate)) as u64
}

/// Converts a target PCM frame index to Media Foundation `PROPVARIANT` 100-ns position units.
pub fn mf_seek_100ns_from_frame(frame: u64, sample_rate: u32) -> Option<i64> {
    if sample_rate == 0 {
        return None;
    }
    Some((((frame as f64) / f64::from(sample_rate)) * MF_HUNDRED_NANOS_PER_SECOND) as i64)
}

/// Remainder packet buffer for compressed codecs (such as AAC 1024-frame `IMFSample` packets)
/// where a single decoded packet exceeds the caller's requested block size.
///
/// Mirrors `MediaFile` (`pending`, `pending_at`, `exhausted`, `media_read`, `media_seek`)
/// in `/native/dsp-host/src/decoders/platform/windows/media_foundation_decoder.cpp`.
#[derive(Debug, Clone)]
pub struct MediaFoundationPacketBuffer {
    pub file_channels: u32,
    pub out_channels: u32,
    pub sample_rate: u32,
    pub total_frames: u64,
    pub position: u64,
    pending: Vec<f32>,
    pending_at: usize,
    pub exhausted: bool,
}

impl MediaFoundationPacketBuffer {
    pub fn new(file_channels: u32, out_channels: u32, sample_rate: u32, total_frames: u64) -> Self {
        Self {
            file_channels: file_channels.max(1),
            out_channels: if out_channels > 0 { out_channels } else { 2 },
            sample_rate,
            total_frames,
            position: 0,
            pending: Vec::new(),
            pending_at: 0,
            exhausted: false,
        }
    }

    pub fn available_pending_frames(&self) -> u32 {
        let fc = self.file_channels as usize;
        if self.pending_at >= self.pending.len() || fc == 0 {
            0
        } else {
            ((self.pending.len() - self.pending_at) / fc) as u32
        }
    }

    /// Replaces `pending` with newly decoded interleaved `f32` samples from `ReadSample`.
    pub fn push_decoded_packet(&mut self, interleaved_samples: &[f32]) {
        self.pending.clear();
        self.pending.extend_from_slice(interleaved_samples);
        self.pending_at = 0;
    }

    /// Drains up to `frames` from `pending` (calling `pull_next_packet` when empty) into
    /// `channels_out`, preserving unconsumed packet tails across reads so no 21ms AAC frame
    /// tail is ever dropped.
    pub fn read_planar<F>(&mut self, channels_out: &mut [Vec<f32>], frames: u32, mut pull_next_packet: F) -> u32
    where
        F: FnMut() -> Option<Vec<f32>>,
    {
        if frames == 0 || self.file_channels == 0 {
            return 0;
        }
        let fc = self.file_channels as usize;
        let oc = (self.out_channels as usize).min(channels_out.len());
        for ch in channels_out.iter_mut().take(oc) {
            if ch.len() < frames as usize {
                ch.resize(frames as usize, 0.0);
            }
        }

        let mut produced = 0u32;
        while produced < frames {
            let available = self.available_pending_frames();
            if available == 0 {
                if self.exhausted {
                    break;
                }
                match pull_next_packet() {
                    Some(pkt) => {
                        if pkt.is_empty() {
                            continue;
                        }
                        self.push_decoded_packet(&pkt);
                    }
                    None => {
                        self.exhausted = true;
                        break;
                    }
                }
                continue;
            }
            let span = available.min(frames - produced);
            for frame in 0..(span as usize) {
                let base = self.pending_at + frame * fc;
                for (channel, out_plane) in channels_out.iter_mut().enumerate().take(oc) {
                    let from = if fc == 1 {
                        0
                    } else if channel < fc {
                        channel
                    } else {
                        fc - 1
                    };
                    out_plane[(produced as usize) + frame] = self.pending[base + from];
                }
            }
            self.pending_at += (span as usize) * fc;
            produced += span;
        }
        self.position += u64::from(produced);
        produced
    }

    /// Clears any residual decoded samples from the old position on seek.
    ///
    /// Mirrors `media_seek` in `/native/dsp-host/src/decoders/platform/windows/media_foundation_decoder.cpp`.
    pub fn reset_on_seek(&mut self, frame: u64) -> Option<i64> {
        let target_100ns = mf_seek_100ns_from_frame(frame, self.sample_rate)?;
        self.pending.clear();
        self.pending_at = 0;
        self.exhausted = false;
        self.position = frame;
        Some(target_100ns)
    }
}

// ============================================================================
// System-APO Strict Bounds-Checked `Convolution:` WAVE Reader (`wav.h` & `wav.cpp`)
// ============================================================================

/// The largest `Convolution:` file `system-apo` will touch: 64 MiB (`kMaxWavBytes = 64u * 1024u * 1024u` in `wav.h`).
pub const MAX_APO_WAV_BYTES: usize = 64 * 1024 * 1024;

/// Decoded mono `Convolution:` impulse response (`fluideq_engine::WavData` in `/native/system-apo/src/wav.h`).
/// First channel only; channels past 0 are silently dropped rather than mixed so the measured impulse is never altered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApoWavData {
    pub sample_rate: u32,
    pub mono: Vec<f32>,
}

#[derive(Debug, Clone, Copy)]
struct ApoWavFormatInfo {
    format_tag: u16,
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
}

fn apo_read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    if offset.checked_add(2)? > bytes.len() {
        return None;
    }
    Some(u16::from_le_bytes([bytes[offset], bytes[offset + 1]]))
}

fn apo_read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    if offset.checked_add(4)? > bytes.len() {
        return None;
    }
    Some(u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ]))
}

fn parse_apo_fmt_chunk(bytes: &[u8], data_start: usize, chunk_size: u32) -> Option<ApoWavFormatInfo> {
    if chunk_size < 16 {
        return None;
    }
    let tag = apo_read_u16(bytes, data_start)?;
    let channels = apo_read_u16(bytes, data_start + 2)?;
    let sample_rate = apo_read_u32(bytes, data_start + 4)?;
    let bits = apo_read_u16(bytes, data_start + 14)?;
    let mut resolved_tag = tag;
    if resolved_tag == WAVE_FORMAT_EXTENSIBLE {
        if chunk_size < 40 {
            return None;
        }
        let sub_format = apo_read_u16(bytes, data_start + 24)?;
        if sub_format != WAVE_FORMAT_PCM && sub_format != WAVE_FORMAT_IEEE_FLOAT {
            return None;
        }
        resolved_tag = sub_format;
    } else if resolved_tag != WAVE_FORMAT_PCM && resolved_tag != WAVE_FORMAT_IEEE_FLOAT {
        return None;
    }
    Some(ApoWavFormatInfo {
        format_tag: resolved_tag,
        channels,
        sample_rate,
        bits_per_sample: bits,
    })
}

fn apo_read_sample(bytes: &[u8], offset: usize, format_tag: u16, bits_per_sample: u16) -> f32 {
    if format_tag == WAVE_FORMAT_IEEE_FLOAT {
        if bits_per_sample == 32 {
            return f32::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ]);
        }
        let mut raw = [0u8; 8];
        raw.copy_from_slice(&bytes[offset..offset + 8]);
        return f64::from_le_bytes(raw) as f32;
    }
    if bits_per_sample == 8 {
        return ((bytes[offset] as f32) - 128.0) / 128.0;
    }
    if bits_per_sample == 16 {
        let signed = i16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
        return (signed as f32) / 32_768.0;
    }
    if bits_per_sample == 24 {
        let mut raw = (bytes[offset] as u32)
            | ((bytes[offset + 1] as u32) << 8)
            | ((bytes[offset + 2] as u32) << 16);
        if (raw & 0x0080_0000) != 0 {
            raw |= 0xFF00_0000;
        }
        let signed = raw as i32;
        return (signed as f32) / 8_388_608.0;
    }
    let signed = i32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ]);
    (signed as f32) / 2_147_483_648.0
}

/// Exact 1:1 port of `fluideq_engine::parse_wav` in `/native/system-apo/src/wav.cpp`:
/// - Enforces `12 <= bytes.len() <= MAX_APO_WAV_BYTES` (64 MiB).
/// - Refuses any chunk whose declared `chunk_size > MAX_APO_WAV_BYTES` or `chunk_size > size - chunk_data`
///   (never clamping truncated `data` chunks).
/// - Enforces `1 <= channels <= 8` and `sample_rate > 0`.
/// - Extracts channel 0 into `ApoWavData::mono` (including `0`-frame `data` chunks so `load_impulse`
///   can log `"Convolution file has no samples"`).
pub fn parse_apo_wav(bytes: &[u8]) -> Option<ApoWavData> {
    let size = bytes.len();
    if size < 12 || size > MAX_APO_WAV_BYTES {
        return None;
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }

    let mut format: Option<ApoWavFormatInfo> = None;
    let mut data_offset = 0usize;
    let mut data_size = 0u32;
    let mut have_data = false;

    let mut pos = 12usize;
    while pos + 8 <= size {
        let id = &bytes[pos..pos + 4];
        let chunk_size = apo_read_u32(bytes, pos + 4)?;
        let chunk_data = pos + 8;
        if (chunk_size as usize) > MAX_APO_WAV_BYTES {
            return None;
        }
        if (chunk_size as usize) > size - chunk_data {
            return None;
        }
        if id == b"fmt " {
            format = Some(parse_apo_fmt_chunk(bytes, chunk_data, chunk_size)?);
        } else if id == b"data" {
            data_offset = chunk_data;
            data_size = chunk_size;
            have_data = true;
        }
        let advance = (chunk_size as usize) + ((chunk_size as usize) & 1);
        pos = chunk_data.checked_add(advance)?;
    }

    let fmt = format?;
    if !have_data || fmt.channels < 1 || fmt.channels > 8 || fmt.sample_rate == 0 {
        return None;
    }
    let bits_valid = if fmt.format_tag == WAVE_FORMAT_IEEE_FLOAT {
        fmt.bits_per_sample == 32 || fmt.bits_per_sample == 64
    } else {
        matches!(fmt.bits_per_sample, 8 | 16 | 24 | 32)
    };
    if !bits_valid {
        return None;
    }

    let bytes_per_sample = u32::from(fmt.bits_per_sample / 8);
    let block_align = u32::from(fmt.channels) * bytes_per_sample;
    let frame_count = (data_size / block_align) as usize;

    let mut mono = Vec::with_capacity(frame_count);
    for frame in 0..frame_count {
        let offset = data_offset + frame * (block_align as usize);
        mono.push(apo_read_sample(
            bytes,
            offset,
            fmt.format_tag,
            fmt.bits_per_sample,
        ));
    }

    Some(ApoWavData {
        sample_rate: fmt.sample_rate,
        mono,
    })
}

/// Exact 1:1 port of `fluideq_engine::read_wav` in `/native/system-apo/src/wav.cpp`:
/// Checks file size on disk (`1..=MAX_APO_WAV_BYTES`) before reading into memory and passing to `parse_apo_wav`.
pub fn read_apo_wav(path: &std::path::Path) -> Option<ApoWavData> {
    let meta = std::fs::metadata(path).ok()?;
    let len = meta.len();
    if len == 0 || len > (MAX_APO_WAV_BYTES as u64) {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() as u64 != len {
        return None;
    }
    parse_apo_wav(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes_32bit_float_stereo_wav_impulse() {
        let interleaved = vec![1.0f32, 0.5f32, -0.25f32, 0.125f32];
        let wav = encode_float32_wav(&interleaved, 48_000, 2);
        assert!(handles_pcm_magic(&wav));

        let ir = decode_pcm_impulse_response(&wav).expect("should decode float32 wav");
        assert_eq!(ir.sample_rate, 48_000);
        assert_eq!(ir.channels, 2);
        assert_eq!(ir.frames, 2);
        assert_eq!(ir.left, vec![1.0, -0.25]);
        assert_eq!(ir.right, vec![0.5, 0.125]);

        // Verify `parse_apo_wav` extracts channel 0 only and refuses truncated chunks
        let apo = parse_apo_wav(&wav).expect("parse_apo_wav should accept valid stereo float wav");
        assert_eq!(apo.sample_rate, 48_000);
        assert_eq!(apo.mono, vec![1.0, -0.25]);

        // Truncating 1 byte off the end makes the declared `data` chunk run past the buffer -> refused
        assert!(parse_apo_wav(&wav[..wav.len() - 1]).is_none());
    }

    #[test]
    fn decodes_aiff_80bit_extended_rate_and_24bit_big_endian_pcm() {
        let ext = write_extended80(48_000);
        assert_eq!(read_extended80(&ext).round() as u32, 48_000);

        let mut aiff = Vec::new();
        aiff.extend_from_slice(b"FORM");
        aiff.extend_from_slice(&46u32.to_be_bytes());
        aiff.extend_from_slice(b"AIFF");
        // COMM chunk (18 bytes)
        aiff.extend_from_slice(b"COMM");
        aiff.extend_from_slice(&18u32.to_be_bytes());
        aiff.extend_from_slice(&1u16.to_be_bytes()); // 1 channel (mono -> duplicated to stereo)
        aiff.extend_from_slice(&2u32.to_be_bytes()); // 2 frames
        aiff.extend_from_slice(&24u16.to_be_bytes()); // 24-bit big-endian integer
        aiff.extend_from_slice(&ext);
        // SSND chunk (8 byte offset/block + 6 bytes of 24-bit samples)
        aiff.extend_from_slice(b"SSND");
        aiff.extend_from_slice(&14u32.to_be_bytes());
        aiff.extend_from_slice(&0u32.to_be_bytes()); // offset
        aiff.extend_from_slice(&0u32.to_be_bytes()); // blockSize
        // Frame 0: +0.5 full scale (0x400000)
        aiff.extend_from_slice(&[0x40, 0x00, 0x00]);
        // Frame 1: -0.5 full scale (0xC00000 in 24-bit two's complement)
        aiff.extend_from_slice(&[0xC0, 0x00, 0x00]);

        assert!(handles_pcm_magic(&aiff));
        let ir = decode_pcm_impulse_response(&aiff).expect("should decode 24-bit AIFF");
        assert_eq!(ir.sample_rate, 48_000);
        assert_eq!(ir.channels, 1);
        assert_eq!(ir.frames, 2);
        assert!((ir.left[0] - 0.5).abs() < 1e-6);
        assert!((ir.left[1] + 0.5).abs() < 1e-6);
        // Mono duplicated into right channel
        assert_eq!(ir.left, ir.right);
    }

    #[test]
    fn sniffs_compressed_codecs_by_content_magic_and_maps_mono_to_stereo() {
        assert_eq!(sniff_compressed_codec(b"fLa"), CompressedCodec::None);
        assert_eq!(sniff_compressed_codec(b"fLaC\x00\x00"), CompressedCodec::Flac);
        assert_eq!(sniff_compressed_codec(b"OggS\x00\x02"), CompressedCodec::Vorbis);
        assert_eq!(sniff_compressed_codec(b"ID3\x04\x00"), CompressedCodec::Mp3);
        assert_eq!(sniff_compressed_codec(&[0xFF, 0xFB, 0x90, 0x64]), CompressedCodec::Mp3);

        // Mono input duplicated into both stereo output channels
        let mono = [0.42_f32, -0.84_f32];
        let mut planes = vec![vec![0.0; 2], vec![0.0; 2]];
        let got = deinterleave_and_map_channels(&mono, 1, 2, 2, &mut planes);
        assert_eq!(got, 2);
        assert_eq!(planes[0], vec![0.42, -0.84]);
        assert_eq!(planes[1], vec![0.42, -0.84]);
    }

    #[test]
    fn media_foundation_packet_buffer_retains_remainder_and_clears_on_seek() {
        let mut mf = MediaFoundationPacketBuffer::new(2, 2, 48_000, 480_000);
        let mut pulls = 0usize;
        let mut planes = vec![vec![0.0; 2], vec![0.0; 2]];

        // Packet has 4 stereo frames, caller asks for 2 -> 2 frames remain in `pending`
        let got1 = mf.read_planar(&mut planes, 2, || {
            pulls += 1;
            Some(vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8])
        });
        assert_eq!(got1, 2);
        assert_eq!(pulls, 1);
        assert_eq!(mf.available_pending_frames(), 2);
        assert_eq!(planes[0], vec![0.1, 0.3]);
        assert_eq!(planes[1], vec![0.2, 0.4]);

        // Seeking clears the stale 2-frame remainder
        let target_100ns = mf.reset_on_seek(48_000).unwrap();
        assert_eq!(target_100ns, 10_000_000);
        assert_eq!(mf.available_pending_frames(), 0);
    }
}


