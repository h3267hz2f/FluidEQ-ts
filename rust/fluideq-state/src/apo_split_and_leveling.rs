/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! System APO Shared-Memory Split-Output Ring, Channel Layout Mapper, and
//! Cross-Stream Leveling Memory Board (`Slice-M1M8-3`).
//!
//! 1:1 port of:
//! - `/native/system-apo/src/channel_layout.h` & `channel_layout.cpp`
//! - `/native/system-apo/src/split_transport.h` & `split_transport.cpp`
//! - `/native/system-apo/src/split_board.h` & `split_board.cpp`
//! - `/native/dsp-core/src/leveling_memory.cpp` & `/native/system-apo/src/leveling_board.cpp`

use crate::apo_engine_runtime::{ChannelSelection, Programme, LOW_FREQUENCY_SPEAKER};
use std::collections::BTreeMap;
use std::mem:: offset_of;
use std::sync::atomic::{fence, AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

// ============================================================================
// 1. Stream Channel Layout & Speaker Mask Mapper (`channel_layout.cpp`)
// ============================================================================

/// `SPEAKER_LOW_FREQUENCY` from `mmreg.h` (4th bit).
pub const SPEAKER_LOW_FREQUENCY_BIT: u32 = 0x8;
/// Number of room speakers (`0: FL, 1: FR, 2: C, 3: SL, 4: SR, 5: RL, 6: RR`).
pub const ROOM_SPEAKERS: usize = 7;

const SPEAKER_BITS: [u32; 11] = [
    0x1, 0x2, 0x4, 0x8, 0x10, 0x20, 0x40, 0x80, 0x100, 0x200, 0x400,
];
const SPEAKER_OF_BIT: [i32; 11] = [0, 1, 2, -1, 5, 6, -1, -1, -1, 3, 4];
const HIGHEST_SPEAKER_BIT: u32 = 0x20000;
const NO_MASK_SIX: [i32; 6] = [0, 1, 2, -1, 5, 6];
const NO_MASK_EIGHT: [i32; 8] = [0, 1, 2, -1, 5, 6, 3, 4];

/// Returns the stream channel index of the subwoofer (LFE) feed, or `-1`.
///
/// Mirrors `fluideq_engine::lfe_channel_of` in `/native/system-apo/src/channel_layout.cpp`.
pub fn lfe_channel_of(mask: u32, channels: u16) -> i32 {
    if mask == 0 {
        return if channels >= 6 { 3 } else { -1 };
    }
    if (mask & SPEAKER_LOW_FREQUENCY_BIT) == 0 {
        return -1;
    }
    let mut index: i32 = 0;
    let mut bit: u32 = 1;
    while bit < SPEAKER_LOW_FREQUENCY_BIT {
        if (mask & bit) != 0 {
            index += 1;
        }
        bit <<= 1;
    }
    if index < i32::from(channels) {
        index
    } else {
        -1
    }
}

/// Returns the room speaker index (`0..=6`) that `channel` feeds, or `-1`
/// (for LFE, out-of-range channels, or unmapped height/front-of-center positions).
///
/// Mirrors `fluideq_engine::speaker_of_channel` in `/native/system-apo/src/channel_layout.cpp`.
pub fn speaker_of_channel(mask: u32, channels: u16, channel: u32) -> i32 {
    if channel >= u32::from(channels) {
        return -1;
    }
    if mask == 0 {
        return match channels {
            2 => {
                if channel == 0 {
                    0
                } else {
                    1
                }
            }
            6 => NO_MASK_SIX[channel as usize],
            8 => NO_MASK_EIGHT[channel as usize],
            _ => -1,
        };
    }
    let mut index: u32 = 0;
    let mut bit: u32 = 1;
    while bit <= HIGHEST_SPEAKER_BIT {
        if (mask & bit) != 0 {
            if index == channel {
                for (at, &known_bit) in SPEAKER_BITS.iter().enumerate() {
                    if known_bit == bit {
                        return SPEAKER_OF_BIT[at];
                    }
                }
                return -1;
            }
            index += 1;
        }
        bit <<= 1;
    }
    -1
}

/// Resolves which stream channels `selection` names as a 32-bit channel bitmask.
///
/// Mirrors `fluideq_engine::selected_channels` in `/native/system-apo/src/channel_layout.cpp`.
pub fn selected_channels(selection: &ChannelSelection, mask: u32, channels: u16) -> u32 {
    let count = u32::from(channels).min(32);
    let every = if count >= 32 {
        !0u32
    } else {
        (1u32 << count) - 1u32
    };
    if selection.all {
        return every;
    }
    let mut bits = selection.indices & every;
    let lfe = lfe_channel_of(mask, channels);
    for channel in 0..count {
        let speaker = if channel as i32 == lfe {
            LOW_FREQUENCY_SPEAKER as i32
        } else {
            speaker_of_channel(mask, channels, channel)
        };
        if speaker >= 0 && (selection.speakers & (1u32 << (speaker as u32))) != 0 {
            bits |= 1u32 << channel;
        }
    }
    bits
}

// ============================================================================
// 2. 64-Byte Aligned Shared-Memory SplitStorage & Lease Transport (`split_transport.cpp`)
// ============================================================================

pub const SPLIT_STORAGE_MAGIC: u32 = 0x3151_4546; // "FEQ1"
pub const SPLIT_STORAGE_VERSION: u32 = 1;
pub const SPLIT_RING_FRAMES: usize = 1 << 16; // 65,536 frames (1.36 s @ 48 kHz)
pub const SPLIT_RING_STRIDE: usize = 8; // Up to 8 channels per frame
pub const SPLIT_RING_SAMPLES: usize = SPLIT_RING_FRAMES * SPLIT_RING_STRIDE;

pub const LEASE_BUSY_BIT: u64 = 1u64 << 61;
pub const LEASE_REVOKE_BIT: u64 = 1u64 << 62;
pub const LEASE_INITIALIZING_BIT: u64 = 1u64 << 63;
pub const LEASE_PID_MASK: u64 = 0xffff_ffff;
pub const LEASE_IDENTITY_MASK: u64 = !(LEASE_BUSY_BIT | LEASE_REVOKE_BIT | LEASE_INITIALIZING_BIT);
pub const LAST_LEASE_SEQUENCE: u32 = (1u32 << 29) - 1;
const CLOCK_READ_TRIES: usize = 4;

/// 64-byte cache-line aligned shared-memory layout for raw dual-output audio splitting.
///
/// Mirrors `fluideq_engine::SplitStorage` in `/native/system-apo/src/split_transport.h`.
/// `samples` begins at exact byte offset `128` (the start of the third 64-byte cache line).
#[repr(C, align(64))]
pub struct SplitStorage {
    pub magic: u32,
    pub version: u32,
    pub bytes: u32,
    pub reserved: u32,
    pub writer: AtomicU64,
    pub writer_created: AtomicU64,
    pub lease_sequence: AtomicU32,
    pub sequence: AtomicU32,
    pub end: AtomicU64,
    pub block_start: AtomicU64,
    pub ticks: AtomicI64,
    pub rate: AtomicU32,
    pub channels: AtomicU32,
    pub mask: AtomicU32,
    pub generation: AtomicU32,
    pub padding: [u32; 12],
    pub samples: [AtomicU32; SPLIT_RING_SAMPLES],
}

impl SplitStorage {
    /// Creates a zeroed `SplitStorage` on the heap (`~2 MiB`) and initializes its header.
    pub fn new_boxed() -> Box<Self> {
        let mut storage: Box<Self> = unsafe {
            let layout = std::alloc::Layout::new::<Self>();
            let ptr = std::alloc::alloc_zeroed(layout) as *mut Self;
            if ptr.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            Box::from_raw(ptr)
        };
        storage.magic = SPLIT_STORAGE_MAGIC;
        storage.version = SPLIT_STORAGE_VERSION;
        storage.bytes = std::mem::size_of::<Self>() as u32;
        storage
    }

    /// Returns the compile-time byte offset of `samples` (`128`).
    pub const fn samples_offset() -> usize {
        offset_of!(Self, samples)
    }

    /// Resets the clock under an odd-even seqlock and increments `generation`.
    ///
    /// Mirrors `SplitTransport::reset_clock` in `/native/system-apo/src/split_transport.cpp`.
    pub fn reset_clock(&self) {
        let odd = self.sequence.load(Ordering::Relaxed) | 1u32;
        self.sequence.store(odd, Ordering::Relaxed);
        fence(Ordering::Release);
        self.end.store(0, Ordering::Relaxed);
        self.block_start.store(0, Ordering::Relaxed);
        self.ticks.store(0, Ordering::Relaxed);
        self.rate.store(0, Ordering::Relaxed);
        self.channels.store(0, Ordering::Relaxed);
        self.mask.store(0, Ordering::Relaxed);
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.sequence.store(odd.wrapping_add(1), Ordering::Release);
    }
}

/// Pure Rust FIPS 180-4 SHA-256 implementation for `split_transport_name` and setup verification.
pub fn sha256_digest(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (data.len() as u64) * 8;
    let mut padded = Vec::with_capacity((data.len() + 9 + 63) & !63);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word_bytes) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word_bytes[0], word_bytes[1], word_bytes[2], word_bytes[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Computes the deterministic Session-0 `Local\FluidEQ-RawSplit-v1-<sha256>` mapping name
/// from a normalized config path and endpoint GUID.
///
/// Mirrors `fluideq_engine::split_transport_name` in `/native/system-apo/src/split_transport.cpp`.
pub fn split_transport_name(config_path: &str, endpoint: &str) -> Result<String, String> {
    if config_path.is_empty() || endpoint.is_empty() {
        return Err("split identity".to_string());
    }
    let mut trimmed = config_path.to_string();
    while trimmed.chars().count() > 3 && (trimmed.ends_with('\\') || trimmed.ends_with('/')) {
        trimmed.pop();
    }
    let combined = format!("{}|{}", trimmed, endpoint);
    let mut utf16_bytes = Vec::with_capacity(combined.len() * 2);
    for ch in combined.chars() {
        let normalized = if ch == '/' {
            '\\'
        } else {
            ch.to_lowercase().next().unwrap_or(ch)
        };
        let mut buf = [0u16; 2];
        for unit in normalized.encode_utf16(&mut buf) {
            utf16_bytes.extend_from_slice(&unit.to_le_bytes());
        }
    }
    let digest = sha256_digest(&utf16_bytes);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut name = String::with_capacity(24 + 64);
    name.push_str("Local\\FluidEQ-RawSplit-v1-");
    for &byte in &digest {
        name.push(HEX[(byte >> 4) as usize] as char);
        name.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(name)
}

/// Status of an existing writer process inspected by `prepare_writer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistingWriterProcessState {
    /// Process no longer exists (`ERROR_INVALID_PARAMETER`) or wait returned signaled (`WAIT_OBJECT_0`).
    Exited,
    /// Process is still running with the given birth timestamp (`FILETIME` 64-bit ticks).
    Running { birth_ticks: u64 },
    /// Process handle or creation time could not be queried (access denied / transient failure).
    Unqueryable,
}

/// Outcome of `SplitTransport::prepare_writer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepareWriterOutcome {
    /// This instance acquired or already holds the writer lease, or has `writers == 0`.
    ReadyOrIdle { signaled_release_event: bool },
    /// Another live process (`pid`) currently owns the writer lease; caller should wait on it.
    WaitForExistingOwner {
        owner_pid: u32,
        signaled_release_event: bool,
    },
    /// Exhausted `LAST_LEASE_SEQUENCE` or failed to query owner process.
    Failed { signaled_release_event: bool },
}

/// Lock-free / control-coordinated writer lease transport over a `SplitStorage` mapping.
///
/// Mirrors `fluideq_engine::SplitTransport` in `/native/system-apo/src/split_transport.cpp`.
pub struct SplitTransport {
    storage: Box<SplitStorage>,
    pid: u32,
    created: u64,
    name: String,
    writers: u32,
    pending_release: AtomicBool,
    failed: AtomicBool,
    ready: AtomicBool,
    lease: AtomicU64,
    writing_lease: u64,
    notified_owner: u64,
}

impl SplitTransport {
    pub fn new(name: impl Into<String>, pid: u32, created: u64) -> Result<Self, String> {
        if created == 0 {
            return Err("split process identity".to_string());
        }
        Ok(Self {
            storage: SplitStorage::new_boxed(),
            pid,
            created,
            name: name.into(),
            writers: 0,
            pending_release: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            ready: AtomicBool::new(true),
            lease: AtomicU64::new(0),
            writing_lease: 0,
            notified_owner: 0,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn storage(&self) -> &SplitStorage {
        &self.storage
    }

    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    pub fn add_writer(&mut self) {
        self.writers = self.writers.saturating_add(1);
    }

    /// Releases this instance's writer lease. Returns `true` if the named release event
    /// should be signaled immediately (`next == 0`), or `false` if a write was in flight
    /// (`LEASE_BUSY_BIT` set -> `LEASE_REVOKE_BIT` marked and `pending_release` armed).
    pub fn release_writer(&mut self) -> bool {
        let lease = self.lease.swap(0, Ordering::AcqRel);
        if lease == 0 {
            return false;
        }
        let mut owner = self.storage.writer.load(Ordering::Acquire);
        while (owner & LEASE_IDENTITY_MASK) == lease {
            let next = if (owner & LEASE_BUSY_BIT) != 0 {
                owner | LEASE_REVOKE_BIT
            } else {
                0
            };
            match self.storage.writer.compare_exchange_weak(
                owner,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    if next == 0 {
                        return true;
                    }
                    self.pending_release.store(true, Ordering::Release);
                    return false;
                }
                Err(actual) => owner = actual,
            }
        }
        false
    }

    pub fn remove_writer(&mut self) -> bool {
        if self.writers != 0 {
            self.writers -= 1;
            if self.writers == 0 {
                return self.release_writer();
            }
        }
        false
    }

    /// Claims or verifies the shared writer lease on the control thread (up to 4 attempts).
    ///
    /// Mirrors `SplitTransport::prepare_writer` in `/native/system-apo/src/split_transport.cpp`.
    pub fn prepare_writer<F>(&mut self, mut inspect_process: F) -> PrepareWriterOutcome
    where
        F: FnMut(u32) -> ExistingWriterProcessState,
    {
        let mut signaled_release_event = false;
        if self.pending_release.load(Ordering::Acquire)
            && (self.storage.writer.load(Ordering::Acquire) & LEASE_BUSY_BIT) == 0
        {
            self.pending_release.store(false, Ordering::Relaxed);
            signaled_release_event = true;
        }
        if self.writers == 0 {
            return PrepareWriterOutcome::ReadyOrIdle {
                signaled_release_event,
            };
        }

        for _attempt in 0..4 {
            let owner = self.storage.writer.load(Ordering::Acquire);
            let local_lease = self.lease.load(Ordering::Acquire);
            if local_lease != 0 && (owner & LEASE_IDENTITY_MASK) == local_lease {
                return PrepareWriterOutcome::ReadyOrIdle {
                    signaled_release_event,
                };
            }
            if owner == 0 {
                let mut previous = self.storage.lease_sequence.load(Ordering::Relaxed);
                while previous < LAST_LEASE_SEQUENCE {
                    match self.storage.lease_sequence.compare_exchange_weak(
                        previous,
                        previous + 1,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => break,
                        Err(actual) => previous = actual,
                    }
                }
                if previous >= LAST_LEASE_SEQUENCE {
                    self.failed.store(true, Ordering::Relaxed);
                    return PrepareWriterOutcome::Failed {
                        signaled_release_event,
                    };
                }
                let sequence = previous + 1;
                let new_lease = (u64::from(sequence) << 32) | u64::from(self.pid);
                if self
                    .storage
                    .writer
                    .compare_exchange(
                        owner,
                        new_lease | LEASE_INITIALIZING_BIT,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
                {
                    continue;
                }
                self.storage
                    .writer_created
                    .store(self.created, Ordering::Relaxed);
                self.storage.reset_clock();
                self.storage.writer.store(new_lease, Ordering::Release);
                self.lease.store(new_lease, Ordering::Release);
                self.failed.store(false, Ordering::Relaxed);
                return PrepareWriterOutcome::ReadyOrIdle {
                    signaled_release_event,
                };
            }

            let born = self.storage.writer_created.load(Ordering::Acquire);
            let owner_pid = (owner & LEASE_PID_MASK) as u32;
            let state = inspect_process(owner_pid);
            let gone = match state {
                ExistingWriterProcessState::Exited => true,
                ExistingWriterProcessState::Running { birth_ticks } => {
                    (owner & LEASE_INITIALIZING_BIT) == 0
                        && birth_ticks != 0
                        && birth_ticks != born
                }
                ExistingWriterProcessState::Unqueryable => false,
            };
            if gone {
                if self.storage.writer_created.load(Ordering::Acquire) == born {
                    let _ = self.storage.writer.compare_exchange(
                        owner,
                        0,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                }
                continue;
            }

            if matches!(
                state,
                ExistingWriterProcessState::Unqueryable
                    | ExistingWriterProcessState::Running { birth_ticks: 0 }
            ) {
                self.failed.store(true, Ordering::Relaxed);
                return PrepareWriterOutcome::Failed {
                    signaled_release_event,
                };
            }
            self.failed.store(false, Ordering::Relaxed);
            return PrepareWriterOutcome::WaitForExistingOwner {
                owner_pid,
                signaled_release_event,
            };
        }

        PrepareWriterOutcome::ReadyOrIdle {
            signaled_release_event: true,
        }
    }

    /// Audio thread only: one lock-free CAS attempt, zero wait, zero allocation.
    ///
    /// Mirrors `SplitTransport::begin_write` in `/native/system-apo/src/split_transport.cpp`.
    pub fn begin_write(&mut self, wake: &mut bool) -> bool {
        if !self.ready.load(Ordering::Acquire) {
            return false;
        }
        let mut expected = self.lease.load(Ordering::Acquire);
        let lease = expected;
        if lease != 0 {
            match self.storage.writer.compare_exchange(
                expected,
                lease | LEASE_BUSY_BIT,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.writing_lease = lease;
                    return true;
                }
                Err(actual) => expected = actual,
            }
        } else {
            expected = self.storage.writer.load(Ordering::Acquire);
        }
        let owner = expected & LEASE_IDENTITY_MASK;
        *wake = owner != self.notified_owner;
        self.notified_owner = owner;
        false
    }

    /// Audio thread only: completes an in-flight write. Returns `true` if a control-thread
    /// revoke occurred while copying and the caller should wake control to signal `released_`.
    ///
    /// Mirrors `SplitTransport::end_write` in `/native/system-apo/src/split_transport.cpp`.
    pub fn end_write(&mut self) -> bool {
        let lease = self.writing_lease;
        let expected = lease | LEASE_BUSY_BIT;
        if self
            .storage
            .writer
            .compare_exchange(expected, lease, Ordering::Release, Ordering::Relaxed)
            .is_ok()
        {
            return false;
        }
        let revoked = lease | LEASE_BUSY_BIT | LEASE_REVOKE_BIT;
        self.storage
            .writer
            .compare_exchange(revoked, 0, Ordering::Release, Ordering::Relaxed)
            .is_ok()
    }
}

// ============================================================================
// 3. Seqlock SplitRing (`split_board.h` & `split_board.cpp`)
// ============================================================================

/// Consistent snapshot of the `SplitRing` header read under its even sequence lock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SplitClock {
    pub end: u64,
    pub block_start: u64,
    pub ticks: i64,
    pub rate: u32,
    pub channels: u32,
    pub mask: u32,
    pub generation: u32,
}

/// Lock-free single-writer, multi-reader audio ring buffer backed by `SplitStorage`.
///
/// Mirrors `fluideq_engine::SplitRing` in `/native/system-apo/src/split_board.cpp`.
pub struct SplitRing {
    storage: Box<SplitStorage>,
}

impl Default for SplitRing {
    fn default() -> Self {
        Self::new()
    }
}

impl SplitRing {
    pub fn new() -> Self {
        Self {
            storage: SplitStorage::new_boxed(),
        }
    }

    pub fn storage(&self) -> &SplitStorage {
        &self.storage
    }

    pub fn end(&self) -> u64 {
        self.storage.end.load(Ordering::Acquire)
    }

    pub fn at(&self, frame: u64, channel: u32) -> f32 {
        let index =
            ((frame as usize) & (SPLIT_RING_FRAMES - 1)) * SPLIT_RING_STRIDE + (channel as usize);
        f32::from_bits(self.storage.samples[index].load(Ordering::Relaxed))
    }

    /// Writes `frames` of `channels` interleaved samples (or silence when `input` is `None`)
    /// and publishes the block under the odd-even sequence lock.
    ///
    /// Mirrors `SplitRing::write` and `SplitRing::publish` in `/native/system-apo/src/split_board.cpp`.
    pub fn write(
        &self,
        input: Option<&[f32]>,
        channels: u32,
        frames: u32,
        rate: u32,
        mask: u32,
        ticks: i64,
    ) -> bool {
        let ch = channels as usize;
        let fr = frames as usize;
        if ch == 0 || ch > SPLIT_RING_STRIDE || fr == 0 || fr > SPLIT_RING_FRAMES / 2 {
            return false;
        }
        if let Some(slice) = input {
            if slice.len() < fr * ch {
                return false;
            }
        }

        let start = self.storage.end.load(Ordering::Relaxed);
        for frame in 0..fr {
            let base = (((start as usize) + frame) & (SPLIT_RING_FRAMES - 1)) * SPLIT_RING_STRIDE;
            for channel in 0..ch {
                let value = match input {
                    Some(slice) => slice[frame * ch + channel],
                    None => 0.0,
                };
                self.storage.samples[base + channel].store(value.to_bits(), Ordering::Relaxed);
            }
        }

        self.publish(start, frames, channels, rate, mask, ticks);
        true
    }

    fn publish(
        &self,
        start: u64,
        frames: u32,
        channels: u32,
        rate: u32,
        mask: u32,
        ticks: i64,
    ) {
        let reshaped = rate != self.storage.rate.load(Ordering::Relaxed)
            || channels != self.storage.channels.load(Ordering::Relaxed)
            || mask != self.storage.mask.load(Ordering::Relaxed);
        let sequence = self.storage.sequence.load(Ordering::Relaxed);
        self.storage
            .sequence
            .store(sequence.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        if reshaped {
            self.storage.rate.store(rate, Ordering::Relaxed);
            self.storage.channels.store(channels, Ordering::Relaxed);
            self.storage.mask.store(mask, Ordering::Relaxed);
            let gen = self.storage.generation.load(Ordering::Relaxed);
            self.storage
                .generation
                .store(gen.wrapping_add(1), Ordering::Relaxed);
        }
        self.storage.block_start.store(start, Ordering::Relaxed);
        self.storage.ticks.store(ticks, Ordering::Relaxed);
        self.storage
            .end
            .store(start + u64::from(frames), Ordering::Relaxed);
        self.storage
            .sequence
            .store(sequence.wrapping_add(2), Ordering::Release);
    }

    /// Reads a consistent `SplitClock` snapshot in up to 4 lock-free attempts.
    ///
    /// Mirrors `SplitRing::clock` in `/native/system-apo/src/split_board.cpp`.
    pub fn clock(&self) -> Option<SplitClock> {
        for _attempt in 0..CLOCK_READ_TRIES {
            let before = self.storage.sequence.load(Ordering::Acquire);
            if (before & 1u32) != 0 {
                continue;
            }
            let read = SplitClock {
                end: self.storage.end.load(Ordering::Relaxed),
                block_start: self.storage.block_start.load(Ordering::Relaxed),
                ticks: self.storage.ticks.load(Ordering::Relaxed),
                rate: self.storage.rate.load(Ordering::Relaxed),
                channels: self.storage.channels.load(Ordering::Relaxed),
                mask: self.storage.mask.load(Ordering::Relaxed),
                generation: self.storage.generation.load(Ordering::Relaxed),
            };
            fence(Ordering::Acquire);
            if self.storage.sequence.load(Ordering::Relaxed) == before {
                return Some(read);
            }
        }
        None
    }
}

// ============================================================================
// 4. Cross-Stream Leveling Memory & Endpoint Board (`leveling_memory.cpp` & `leveling_board.cpp`)
// ============================================================================

const LEVELING_WORDS: usize = 12;
const FLAG_VALID: u64 = 1;
const FLAG_ESTABLISHED: u64 = 2;
const FLAG_NAMED: u64 = 4;
const FLAG_KNOWN: u64 = 8;
const FLAG_GLIDE: u64 = 16;
const FLAG_RETARGET: u64 = 32;
const CONTROL_READ_ATTEMPTS: usize = 1024;

/// Complete live-leveler state transferred across `audiodg.exe` APO stream rebuilds.
///
/// Mirrors `FeqLevelingState` in `/native/dsp-core/src/leveling_memory_internal.h`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeqLevelingState {
    pub cue_seen: u64,
    pub song_id: u64,
    pub gain_db: f64,
    pub target_db: f64,
    pub song_level: f64,
    pub song_peak_db: f64,
    pub settled_level: f64,
    pub settled_peak_db: f64,
    pub foreground_seconds: f64,
    pub control_target_lufs: f64,
    pub control_ceiling_db: f64,
    pub retarget_pending: bool,
    pub established: bool,
    pub named: bool,
    pub known: bool,
    pub glide: bool,
}

impl Default for FeqLevelingState {
    fn default() -> Self {
        Self {
            cue_seen: 0,
            song_id: 0,
            gain_db: 0.0,
            target_db: 0.0,
            song_level: -120.0,
            song_peak_db: -120.0,
            settled_level: -120.0,
            settled_peak_db: -120.0,
            foreground_seconds: 0.0,
            control_target_lufs: -120.0,
            control_ceiling_db: -120.0,
            retarget_pending: false,
            established: false,
            named: false,
            known: false,
            glide: false,
        }
    }
}

/// Settled loudness report for a song that finished playing.
///
/// Mirrors `FeqSongLevel` and `fluideq_engine::SongReport`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SongReport {
    pub song_id: u64,
    pub level_lufs: f64,
    pub peak_db: f64,
    pub seconds: f64,
}

/// Cue announcement read lock-free by the audio thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelingCue {
    pub sequence: u64,
    pub song_id: u64,
    pub level_lufs: f64,
    pub peak_db: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LearnedReadStatus {
    Busy,
    Empty,
    Ready,
}

/// Sequence-locked leveling memory shared between watcher threads and real-time audio threads.
///
/// Mirrors `FeqLevelingMemory` in `/native/dsp-core/src/leveling_memory.cpp`.
pub struct FeqLevelingMemory {
    announcer: Mutex<()>,
    cue_sequence: AtomicU64,
    cue_id: AtomicU64,
    cue_level: AtomicU64,
    cue_peak: AtomicU64,
    learned_sequence: AtomicU64,
    learned: [AtomicU64; LEVELING_WORDS],
}

impl Default for FeqLevelingMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl FeqLevelingMemory {
    pub fn new() -> Self {
        Self {
            announcer: Mutex::new(()),
            cue_sequence: AtomicU64::new(0),
            cue_id: AtomicU64::new(0),
            cue_level: AtomicU64::new((-120.0_f64).to_bits()),
            cue_peak: AtomicU64::new((-120.0_f64).to_bits()),
            learned_sequence: AtomicU64::new(0),
            learned: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// Audio thread: reads the latest song cue without locking or waiting.
    pub fn read_cue(&self) -> Option<LevelingCue> {
        let before = self.cue_sequence.load(Ordering::Acquire);
        if (before & 1) != 0 {
            return None;
        }
        let cue = LevelingCue {
            sequence: before,
            song_id: self.cue_id.load(Ordering::Acquire),
            level_lufs: f64::from_bits(self.cue_level.load(Ordering::Acquire)),
            peak_db: f64::from_bits(self.cue_peak.load(Ordering::Acquire)),
        };
        if self.cue_sequence.load(Ordering::Acquire) == before {
            Some(cue)
        } else {
            None
        }
    }

    /// Audio thread: reads the learned leveling state without locking.
    pub fn read_learned(&self) -> (LearnedReadStatus, FeqLevelingState) {
        let before = self.learned_sequence.load(Ordering::Acquire);
        if (before & 1) != 0 {
            return (LearnedReadStatus::Busy, FeqLevelingState::default());
        }
        let mut words = [0u64; LEVELING_WORDS];
        for (at, slot) in self.learned.iter().enumerate() {
            words[at] = slot.load(Ordering::Acquire);
        }
        if self.learned_sequence.load(Ordering::Acquire) != before {
            return (LearnedReadStatus::Busy, FeqLevelingState::default());
        }
        let flags = words[9];
        if (flags & FLAG_VALID) == 0 {
            return (LearnedReadStatus::Empty, FeqLevelingState::default());
        }
        let state = FeqLevelingState {
            cue_seen: words[0],
            song_id: words[1],
            gain_db: f64::from_bits(words[2]),
            target_db: f64::from_bits(words[3]),
            song_level: f64::from_bits(words[4]),
            song_peak_db: f64::from_bits(words[5]),
            settled_level: f64::from_bits(words[6]),
            settled_peak_db: f64::from_bits(words[7]),
            foreground_seconds: f64::from_bits(words[8]),
            established: (flags & FLAG_ESTABLISHED) != 0,
            named: (flags & FLAG_NAMED) != 0,
            known: (flags & FLAG_KNOWN) != 0,
            glide: (flags & FLAG_GLIDE) != 0,
            retarget_pending: (flags & FLAG_RETARGET) != 0,
            control_target_lufs: f64::from_bits(words[10]),
            control_ceiling_db: f64::from_bits(words[11]),
        };
        (LearnedReadStatus::Ready, state)
    }

    /// Audio thread: publishes the current `FeqLevelingState` via odd-even seqlock CAS.
    pub fn publish(&self, state: &FeqLevelingState, valid: bool) -> bool {
        let sequence = self.learned_sequence.load(Ordering::Acquire);
        if (sequence & 1) != 0
            || self
                .learned_sequence
                .compare_exchange(
                    sequence,
                    sequence.wrapping_add(1),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
        {
            return false;
        }
        let flags = if valid {
            FLAG_VALID
                | if state.established { FLAG_ESTABLISHED } else { 0 }
                | if state.named { FLAG_NAMED } else { 0 }
                | if state.known { FLAG_KNOWN } else { 0 }
                | if state.glide { FLAG_GLIDE } else { 0 }
                | if state.retarget_pending { FLAG_RETARGET } else { 0 }
        } else {
            0
        };
        let words: [u64; LEVELING_WORDS] = [
            state.cue_seen,
            state.song_id,
            state.gain_db.to_bits(),
            state.target_db.to_bits(),
            state.song_level.to_bits(),
            state.song_peak_db.to_bits(),
            state.settled_level.to_bits(),
            state.settled_peak_db.to_bits(),
            state.foreground_seconds.to_bits(),
            flags,
            state.control_target_lufs.to_bits(),
            state.control_ceiling_db.to_bits(),
        ];
        for (at, &w) in words.iter().enumerate() {
            self.learned[at].store(w, Ordering::Release);
        }
        self.learned_sequence
            .store(sequence.wrapping_add(2), Ordering::Release);
        true
    }

    /// Control thread: announces the programme now playing. Returns `true` when `song_id`
    /// differs from the last announced song, or `false` if the same song was announced again.
    ///
    /// Mirrors `feq_leveling_memory_begin_song` in `/native/dsp-core/src/leveling_memory.cpp`.
    pub fn begin_song(&self, song_id: u64, known_level_lufs: f64, known_peak_db: f64) -> bool {
        let _guard = match self.announcer.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let sequence = self.cue_sequence.load(Ordering::Acquire);
        if sequence != 0 && self.cue_id.load(Ordering::Acquire) == song_id {
            return false;
        }
        let clean_level = if known_level_lufs.is_finite() {
            known_level_lufs
        } else {
            -120.0
        };
        let clean_peak = if known_peak_db.is_finite() {
            known_peak_db
        } else {
            -120.0
        };
        self.cue_sequence
            .store(sequence.wrapping_add(1), Ordering::Release);
        self.cue_id.store(song_id, Ordering::Release);
        self.cue_level
            .store(clean_level.to_bits(), Ordering::Release);
        self.cue_peak
            .store(clean_peak.to_bits(), Ordering::Release);
        self.cue_sequence
            .store(sequence.wrapping_add(2), Ordering::Release);
        true
    }

    /// Any thread: reads what the leveler learned about the settled song.
    ///
    /// Mirrors `feq_leveling_memory_read_song` in `/native/dsp-core/src/leveling_memory.cpp`.
    pub fn read_song(&self) -> Option<SongReport> {
        for _attempt in 0..CONTROL_READ_ATTEMPTS {
            let (status, state) = self.read_learned();
            match status {
                LearnedReadStatus::Empty => return None,
                LearnedReadStatus::Ready => {
                    if state.song_id == 0 || state.settled_level <= -100.0 {
                        return None;
                    }
                    return Some(SongReport {
                        song_id: state.song_id,
                        level_lufs: state.settled_level,
                        peak_db: state.settled_peak_db,
                        seconds: state.foreground_seconds,
                    });
                }
                LearnedReadStatus::Busy => std::thread::yield_now(),
            }
        }
        None
    }
}

/// Per-endpoint leveling instance that survives APO stream unlock/relock cycles.
///
/// Mirrors `fluideq_engine::Leveling` in `/native/system-apo/src/leveling_board.cpp`.
pub struct EndpointLeveling {
    memory: Arc<FeqLevelingMemory>,
    mutex: Mutex<Option<SongReport>>,
}

/// Alias used by `ApoWatcher` (`watcher.h` / `watcher_log.cpp`).
pub type EndpointLevelingCoordinator = EndpointLeveling;

impl Default for EndpointLeveling {
    fn default() -> Self {
        Self::new()
    }
}

impl EndpointLeveling {
    pub fn new() -> Self {
        Self {
            memory: Arc::new(FeqLevelingMemory::new()),
            mutex: Mutex::new(None),
        }
    }

    pub fn memory(&self) -> &Arc<FeqLevelingMemory> {
        &self.memory
    }

    /// Announces the programme now playing. Captures the finished song's report
    /// *before* the announcement reaches the audio thread, and ignores title flickers.
    ///
    /// Mirrors `Leveling::announce` in `/native/system-apo/src/leveling_board.cpp`.
    pub fn announce(&self, programme: &Programme) -> bool {
        let mut last_guard = match self.mutex.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let finished = self.memory.read_song();
        if !self.memory.begin_song(
            programme.song_id,
            programme.level_lufs,
            programme.peak_db,
        ) {
            return false;
        }
        let Some(report) = finished else {
            return false;
        };
        if report.song_id == programme.song_id {
            return false;
        }
        *last_guard = Some(report);
        true
    }

    pub fn last_song(&self) -> Option<SongReport> {
        match self.mutex.lock() {
            Ok(g) => *g,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }
}

/// Process-lifetime registry of per-endpoint `EndpointLeveling` instances.
///
/// Mirrors `fluideq_engine::leveling_for` in `/native/system-apo/src/leveling_board.cpp`.
#[derive(Default)]
pub struct LevelingBoard {
    outputs: Mutex<BTreeMap<String, Arc<EndpointLeveling>>>,
}

impl LevelingBoard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn leveling_for(&self, endpoint: &str) -> Arc<EndpointLeveling> {
        if endpoint.is_empty() {
            return Arc::new(EndpointLeveling::new());
        }
        let mut map = match self.outputs.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        map.entry(endpoint.to_string())
            .or_insert_with(|| Arc::new(EndpointLeveling::new()))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_storage_samples_offset_is_exact_128_bytes() {
        assert_eq!(SplitStorage::samples_offset(), 128);
        assert_eq!(std::mem::align_of::<SplitStorage>(), 64);
    }

    #[test]
    fn channel_layout_resolves_lfe_and_7_1_room_speakers() {
        assert_eq!(lfe_channel_of(0, 2), -1);
        assert_eq!(lfe_channel_of(0, 6), 3);
        // 5.1 mask (FL|FR|FC|LFE|BL|BR = 0x3F): LFE is at channel index 3
        assert_eq!(lfe_channel_of(0x3f, 6), 3);
        assert_eq!(speaker_of_channel(0x3f, 6, 0), 0); // FL
        assert_eq!(speaker_of_channel(0x3f, 6, 2), 2); // FC
        assert_eq!(speaker_of_channel(0x3f, 6, 3), -1); // LFE
        assert_eq!(speaker_of_channel(0x3f, 6, 4), 5); // BL -> RL (5)
    }

    #[test]
    fn split_transport_name_normalizes_separators_case_and_hashes_utf16le() {
        let a = split_transport_name(
            "C:\\Program Files\\FluidEQ\\config/",
            "{A1B2C3D4-E5F6-4711-8899-001122334455}",
        )
        .unwrap();
        let b = split_transport_name(
            "c:/program files/fluideq/config\\",
            "{a1b2c3d4-e5f6-4711-8899-001122334455}",
        )
        .unwrap();
        assert_eq!(a, b);
        assert!(a.starts_with("Local\\FluidEQ-RawSplit-v1-"));
        assert_eq!(a.len(), "Local\\FluidEQ-RawSplit-v1-".len() + 64);
    }

    #[test]
    fn split_transport_handles_in_flight_revoke_without_racing_writer() {
        let mut transport =
            SplitTransport::new("Local\\FluidEQ-RawSplit-v1-test", 4242, 999_111).unwrap();
        transport.add_writer();
        assert_eq!(
            transport.prepare_writer(|_| ExistingWriterProcessState::Exited),
            PrepareWriterOutcome::ReadyOrIdle {
                signaled_release_event: false
            }
        );

        let mut wake = false;
        assert!(transport.begin_write(&mut wake));
        // Control thread removes writer while audio thread is in the middle of copying (`LEASE_BUSY_BIT` set)
        assert!(!transport.remove_writer());
        // Audio thread finishes write and detects `LEASE_REVOKE_BIT`, clearing writer to 0 and returning true
        assert!(transport.end_write());
        assert_eq!(transport.storage().writer.load(Ordering::Acquire), 0);
    }

    #[test]
    fn split_ring_publishes_samples_and_increments_generation_on_reshape() {
        let ring = SplitRing::new();
        let samples = [0.25_f32, -0.25, 0.5, -0.5];
        assert!(ring.write(Some(&samples), 2, 2, 48_000, 0x3, 123_456));
        let clk1 = ring.clock().unwrap();
        assert_eq!(clk1.end, 2);
        assert_eq!(clk1.block_start, 0);
        assert_eq!(clk1.rate, 48_000);
        assert_eq!(clk1.generation, 1);
        assert!((ring.at(0, 0) - 0.25).abs() < 1e-6);
        assert!((ring.at(1, 1) - (-0.5)).abs() < 1e-6);

        // Same format does not bump generation
        assert!(ring.write(Some(&samples), 2, 2, 48_000, 0x3, 124_000));
        assert_eq!(ring.clock().unwrap().generation, 1);

        // Rate change bumps generation
        assert!(ring.write(Some(&samples), 2, 2, 96_000, 0x3, 125_000));
        assert_eq!(ring.clock().unwrap().generation, 2);
    }

    #[test]
    fn leveling_board_preserves_learned_song_across_streams_and_ignores_title_flicker() {
        let board = LevelingBoard::new();
        let endpoint = "{a1b2c3d4-e5f6-4711-8899-001122334455}";
        let lv = board.leveling_for(endpoint);

        // Announce Song A
        let song_a = Programme {
            song_id: 0x0123_4567_89ab_cdef,
            level_lufs: -120.0,
            peak_db: -120.0,
        };
        assert!(!lv.announce(&song_a));

        // Audio thread publishes learned loudness for Song A
        let learned = FeqLevelingState {
            cue_seen: 2,
            song_id: song_a.song_id,
            settled_level: -14.2,
            settled_peak_db: -0.8,
            foreground_seconds: 185.0,
            established: true,
            named: true,
            ..Default::default()
        };
        assert!(lv.memory().publish(&learned, true));

        // Title flickers to 0 and back to Song A -> must NOT report Song A as ended when returning to Song A
        let empty_prog = Programme::default();
        assert!(lv.announce(&empty_prog));
        assert_eq!(lv.last_song().unwrap().song_id, song_a.song_id);

        // Second watcher on the same endpoint shares the exact same Leveling instance
        let lv_second = board.leveling_for(endpoint);
        assert_eq!(
            lv_second.last_song().unwrap().level_lufs,
            -14.2
        );
    }
}
