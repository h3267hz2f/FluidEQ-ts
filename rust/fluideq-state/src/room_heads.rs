/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Parser and on-demand cache for `/assets/room/heads/{small,medium,large}.txt`
//! KEMAR HRTF impulse response banks used by `feq_chain_set_room_head`.
//!
//! File format (from `/.erb/scripts/build-room-heads.ts`):
//! - Header line: `# FluidEQ room head v1 <small|medium|large>`
//! - For each rate (`44100`, `48000`, `96000`):
//!   - `rate <rate> directions 24 taps <256|512>`
//!   - 24 lines (one per horizontal azimuth `0°, 15°, ..., 345°`), each containing
//!     `taps * 2` space-separated floats (`left[0..taps]` followed by `right[0..taps]`).

use fluideq_dsp_sys::{FeqRoomHead, FEQ_ROOM_DIRECTIONS, FEQ_ROOM_MAX_HEAD_TAPS};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_HEAD_DIRECTIONS: u32 = 72;
pub const MAX_HEAD_TAPS: u32 = 4096;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomHeadRateBlock {
    pub rate: u32,
    pub directions: u32,
    pub taps: u32,
    /// Planar buffer of length `directions * taps` (`24 * taps`) for left ear.
    pub left: Vec<f32>,
    /// Planar buffer of length `directions * taps` (`24 * taps`) for right ear.
    pub right: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomHeadBank {
    pub head_id: String,
    pub blocks: BTreeMap<u32, RoomHeadRateBlock>,
}

impl RoomHeadBank {
    /// Parses a FluidEQ room head v1 text file (`small.txt`, `medium.txt`, `large.txt`).
    pub fn parse_text(text: &str) -> Result<Self, String> {
        let mut lines = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty());

        let first = lines
            .next()
            .ok_or_else(|| "Empty room head file".to_string())?;
        let head_id = first
            .strip_prefix("# FluidEQ room head v1 ")
            .ok_or_else(|| format!("Invalid room head header: {}", first))?
            .trim()
            .to_string();

        let mut blocks = BTreeMap::new();

        while let Some(rate_line) = lines.next() {
            if rate_line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = rate_line.split_whitespace().collect();
            if parts.len() != 6
                || parts[0] != "rate"
                || parts[2] != "directions"
                || parts[4] != "taps"
            {
                return Err(format!("Malformed rate header line: {}", rate_line));
            }
            let rate: u32 = parts[1]
                .parse()
                .map_err(|_| format!("Invalid rate value: {}", parts[1]))?;
            let directions: u32 = parts[3]
                .parse()
                .map_err(|_| format!("Invalid directions value: {}", parts[3]))?;
            let taps: u32 = parts[5]
                .parse()
                .map_err(|_| format!("Invalid taps value: {}", parts[5]))?;

            let _ = (FEQ_ROOM_DIRECTIONS, FEQ_ROOM_MAX_HEAD_TAPS);
            if directions == 0
                || directions > MAX_HEAD_DIRECTIONS
                || taps == 0
                || taps > MAX_HEAD_TAPS
            {
                return Err(format!(
                    "Unsupported directions/taps combination: directions={} taps={}",
                    directions, taps
                ));
            }

            let total_len = (directions as usize) * (taps as usize);
            let mut left = Vec::with_capacity(total_len);
            let mut right = Vec::with_capacity(total_len);

            for dir_idx in 0..directions {
                let row = lines.next().ok_or_else(|| {
                    format!(
                        "Unexpected EOF reading rate {} direction {}",
                        rate, dir_idx
                    )
                })?;
                let mut nums = row.split_whitespace();
                for _ in 0..taps {
                    let tok = nums.next().ok_or_else(|| {
                        format!("Missing left tap in rate {} dir {}", rate, dir_idx)
                    })?;
                    let val: f32 = tok
                        .parse()
                        .map_err(|_| format!("Invalid float token: {}", tok))?;
                    left.push(val);
                }
                for _ in 0..taps {
                    let tok = nums.next().ok_or_else(|| {
                        format!("Missing right tap in rate {} dir {}", rate, dir_idx)
                    })?;
                    let val: f32 = tok
                        .parse()
                        .map_err(|_| format!("Invalid float token: {}", tok))?;
                    right.push(val);
                }
            }

            blocks.insert(
                rate,
                RoomHeadRateBlock {
                    rate,
                    directions,
                    taps,
                    left,
                    right,
                },
            );
        }

        if blocks.is_empty() {
            return Err("Room head file contained no rate blocks".to_string());
        }

        Ok(Self { head_id, blocks })
    }

    /// Selects the exact matching rate block and `doubling` shift for `stream_rate`,
    /// mirroring `feq_room_head_rate` in `/native/dsp-core/src/room_rate.cpp` and
    /// `load_room_head` in `/native/system-apo/src/room_head.cpp`.
    pub fn select_for_stream_rate(&self, stream_rate: u32) -> Option<(&RoomHeadRateBlock, i32)> {
        let (base_rate, doubling) = select_head_rate_and_doubling(stream_rate);
        if base_rate == 0 {
            return None;
        }
        self.blocks.get(&base_rate).map(|block| (block, doubling))
    }
}

/// Exact port of `feq_room_head_rate(stream_rate, &doubling)` from `/native/dsp-core/src/room_rate.cpp` (lines 326–348).
///
/// - `44100 | 48000 | 96000` -> `(stream_rate, 0)`
/// - `192000` -> `(96000, 1)` (zero-stuffed 2x doubling in `FeqRoom`)
/// - `88200 | 176400 | 352800` -> `(44100, 0)` (polyphase decimation/interpolation in `RoomRate`)
/// - `384000` -> `(96000, 0)` (polyphase 4x decimation/interpolation in `RoomRate`)
/// - Any other rate -> `(0, 0)`
pub fn select_head_rate_and_doubling(stream_rate: u32) -> (u32, i32) {
    #[cfg(feature = "link-cpp-core")]
    unsafe {
        let mut doubling: std::os::raw::c_int = 0;
        let head_rate =
            fluideq_dsp_sys::feq_room_head_rate(stream_rate as f64, &mut doubling as *mut _);
        if head_rate <= 0.0 {
            return (0, 0);
        }
        (head_rate.round() as u32, doubling)
    }

    #[cfg(not(feature = "link-cpp-core"))]
    match stream_rate {
        44_100 | 48_000 | 96_000 => (stream_rate, 0),
        192_000 => (96_000, 1),
        88_200 | 176_400 | 352_800 => (44_100, 0),
        384_000 => (96_000, 0),
        _ => (0, 0),
    }
}

pub fn head_enum_to_asset_name(head: FeqRoomHead) -> &'static str {
    match head {
        FeqRoomHead::FEQ_ROOM_HEAD_SMALL => "small",
        FeqRoomHead::FEQ_ROOM_HEAD_MEDIUM => "medium",
        FeqRoomHead::FEQ_ROOM_HEAD_LARGE => "large",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_room_head_v1_block() {
        let mut synthetic = String::from("# FluidEQ room head v1 medium\n");
        synthetic.push_str("rate 48000 directions 24 taps 4\n");
        for d in 0..24 {
            let l0 = (d as f32) * 0.01;
            let r0 = (d as f32) * 0.02;
            synthetic.push_str(&format!(
                "{:.5} 0.0 0.0 0.0 {:.5} 0.0 0.0 0.0\n",
                l0, r0
            ));
        }

        let bank = RoomHeadBank::parse_text(&synthetic).expect("should parse synthetic head bank");
        assert_eq!(bank.head_id, "medium");
        let (block, doubling) = bank
            .select_for_stream_rate(48000)
            .expect("48k block should exist");
        assert_eq!(doubling, 0);
        assert_eq!(block.directions, 24);
        assert_eq!(block.taps, 4);
        assert_eq!(block.left.len(), 96);
        assert_eq!(block.right.len(), 96);
    }
}
