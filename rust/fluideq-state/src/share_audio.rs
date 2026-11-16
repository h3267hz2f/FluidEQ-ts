/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Share Audio over a local network: the pairing code, the discovery words,
//! the handshake that proves both ends know the code, and the authenticated
//! packets that carry Float32 PCM between them.
//!
//! Nothing here touches a socket, a clock or a device. The shell owns the
//! sockets and the native helpers; this module only says what goes on the
//! wire and refuses anything that does not verify. The cipher is ChaCha20-
//! Poly1305 (RFC 8439) built on the SHA-256 already in this workspace, so the
//! feature adds no dependency and the tests can check the published vectors.
//!
//! A pairing code is short enough to read across a room, so the key is not
//! the code: both ends stretch it with the host's salt before anything else,
//! and each session then derives its own keys from the two nonces. A wrong
//! code fails the host's proof and the connection is refused before a single
//! audio byte is sent.

use crate::apo_split_and_leveling::sha256_digest;

pub const DISCOVERY_PORT: u16 = 47_810;
pub const AUDIO_PORT: u16 = 47_811;

const DISCOVERY_PREFIX: &str = "FEQ-DISCOVER 1";
const HOST_PREFIX: &str = "FEQ-HOST 1";
const JOIN_PREFIX: &str = "FEQ-JOIN 1";
const WELCOME_PREFIX: &str = "FEQ-WELCOME 1";
const REFUSED_WORD: &str = "FEQ-REFUSED";

/// 40 bits from an alphabet with no `0/O`, `1/I` or look-alike pairs.
const CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
pub const CODE_LEN: usize = 8;
pub const SALT_BYTES: usize = 16;
pub const NONCE_BYTES: usize = 16;
const STRETCH_ROUNDS: u32 = 20_000;

pub const AUDIO_MAGIC: u32 = 0x3151_4546;
pub const AUDIO_VERSION: u16 = 1;
/// Magic, version, channels, sample rate, frames, sequence and counter.
pub const AUDIO_HEADER_BYTES: usize = 28;
pub const TAG_BYTES: usize = 16;
const MAX_FRAMES: u16 = 1_920;
const MAX_CHANNELS: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareError {
    Malformed,
    Auth,
    Replay,
}

impl ShareError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Malformed => "the packet is not one this version speaks",
            Self::Auth => "the packet did not verify",
            Self::Replay => "the packet arrived twice or out of order",
        }
    }
}

// ---------------------------------------------------------------------------
// Pairing code
// ---------------------------------------------------------------------------

/// A fresh code from the alphabet above, eight characters.
pub fn new_pairing_code() -> String {
    let bytes = random_bytes(CODE_LEN);
    bytes
        .iter()
        .map(|byte| CODE_ALPHABET[(*byte as usize) & 31] as char)
        .collect()
}

/// What a person typed, as the code it means: spaces and dashes ignored,
/// letters folded up. `None` when it can never be a code.
pub fn normalize_pairing_code(input: &str) -> Option<String> {
    let mut code = String::with_capacity(CODE_LEN);
    for ch in input.chars() {
        if ch == ' ' || ch == '-' || ch == '\t' {
            continue;
        }
        let upper = ch.to_ascii_uppercase();
        if !upper.is_ascii() || !CODE_ALPHABET.contains(&(upper as u8)) {
            return None;
        }
        code.push(upper);
    }
    (code.len() == CODE_LEN).then_some(code)
}

fn code_bytes(code: &str) -> &[u8] {
    code.as_bytes()
}

/// What discovery may say about a code without saying the code. An address
/// only answers the fingerprint it holds, so a wrong room does not reply.
pub fn code_fingerprint(code: &str) -> [u8; 8] {
    let mut material = Vec::with_capacity(32 + CODE_LEN);
    material.extend_from_slice(b"FluidEQ-Share-fingerprint-v1");
    material.extend_from_slice(code_bytes(code));
    let digest = sha256_digest(&material);
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

/// The key both ends hold before the handshake. The salt is public; the code
/// is the secret, and stretching is what makes an eavesdropped proof cost
/// more than trying every code once.
pub fn pairing_key(code: &str, salt: &[u8; SALT_BYTES]) -> [u8; 32] {
    pbkdf2_sha256(code_bytes(code), salt, STRETCH_ROUNDS, 32)
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discovery {
    /// "Is the host with this code on my network?"
    Discover { fingerprint: [u8; 8] },
    /// "Here I am." The salt and TCP port travel in the clear; neither is a
    /// secret, and the guest needs both before it can prove the code.
    Host {
        name: String,
        port: u16,
        salt: [u8; SALT_BYTES],
    },
}

pub fn encode_discover(fingerprint: &[u8; 8]) -> Vec<u8> {
    format!("{DISCOVERY_PREFIX} {}\n", hex_lower(fingerprint)).into_bytes()
}

pub fn encode_host(name: &str, port: u16, salt: &[u8; SALT_BYTES]) -> Vec<u8> {
    format!("{HOST_PREFIX} {} {port} {}\n", sanitize_name(name), hex_lower(salt)).into_bytes()
}

pub fn parse_discovery(bytes: &[u8]) -> Option<Discovery> {
    let text = std::str::from_utf8(bytes).ok()?.trim_end_matches(['\n', '\r']);
    if let Some(rest) = text.strip_prefix(DISCOVERY_PREFIX) {
        let fingerprint = parse_hex::<8>(rest.trim())?;
        return Some(Discovery::Discover { fingerprint });
    }
    if let Some(rest) = text.strip_prefix(HOST_PREFIX) {
        let mut parts = rest.trim().split(' ');
        let name = parts.next()?;
        let port = parts.next()?.parse::<u16>().ok()?;
        let salt = parse_hex::<SALT_BYTES>(parts.next()?)?;
        if parts.next().is_some() || name.is_empty() || name.len() > 32 {
            return None;
        }
        return Some(Discovery::Host {
            name: name.to_string(),
            port,
            salt,
        });
    }
    None
}

/// A name safe to put in a one-line word and to show in a list.
pub fn sanitize_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|ch| if ch == ' ' { '-' } else { ch })
        .filter(|ch| ch.is_ascii_graphic())
        .take(32)
        .collect();
    if out.is_empty() {
        out.push_str("FluidEQ");
    }
    out
}

// ---------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handshake {
    Join {
        client_nonce: [u8; NONCE_BYTES],
        proof: [u8; 32],
    },
    Welcome {
        host_nonce: [u8; NONCE_BYTES],
        proof: [u8; 32],
    },
    Refused,
}

pub fn client_proof(key: &[u8; 32], salt: &[u8; SALT_BYTES], nonce: &[u8; NONCE_BYTES]) -> [u8; 32] {
    let mut material = Vec::with_capacity(64);
    material.extend_from_slice(b"FluidEQ-Share-client-v1");
    material.extend_from_slice(salt);
    material.extend_from_slice(nonce);
    hmac_sha256(key, &material)
}

pub fn host_proof(
    key: &[u8; 32],
    salt: &[u8; SALT_BYTES],
    client_nonce: &[u8; NONCE_BYTES],
    host_nonce: &[u8; NONCE_BYTES],
) -> [u8; 32] {
    let mut material = Vec::with_capacity(64);
    material.extend_from_slice(b"FluidEQ-Share-host-v1");
    material.extend_from_slice(salt);
    material.extend_from_slice(client_nonce);
    material.extend_from_slice(host_nonce);
    hmac_sha256(key, &material)
}

/// `(client to host, host to client)`. Both nonces are in both keys, so no
/// key is ever used by two sessions.
pub fn session_keys(
    key: &[u8; 32],
    client_nonce: &[u8; NONCE_BYTES],
    host_nonce: &[u8; NONCE_BYTES],
) -> ([u8; 32], [u8; 32]) {
    let mut shared = Vec::with_capacity(32);
    shared.extend_from_slice(client_nonce);
    shared.extend_from_slice(host_nonce);
    let to_host = hmac_sha256(key, &[b"FluidEQ-Share-c2s-v1".as_slice(), &shared].concat());
    let to_client = hmac_sha256(key, &[b"FluidEQ-Share-s2c-v1".as_slice(), &shared].concat());
    (to_host, to_client)
}

pub fn encode_join(client_nonce: &[u8; NONCE_BYTES], proof: &[u8; 32]) -> Vec<u8> {
    format!("{JOIN_PREFIX} {} {}\n", hex_lower(client_nonce), hex_lower(proof)).into_bytes()
}

pub fn encode_welcome(host_nonce: &[u8; NONCE_BYTES], proof: &[u8; 32]) -> Vec<u8> {
    format!("{WELCOME_PREFIX} {} {}\n", hex_lower(host_nonce), hex_lower(proof)).into_bytes()
}

pub fn encode_refused() -> Vec<u8> {
    format!("{REFUSED_WORD}\n").into_bytes()
}

pub fn parse_handshake(bytes: &[u8]) -> Option<Handshake> {
    let text = std::str::from_utf8(bytes).ok()?.trim_end_matches(['\n', '\r']);
    if text == REFUSED_WORD {
        return Some(Handshake::Refused);
    }
    if let Some(rest) = text.strip_prefix(JOIN_PREFIX) {
        let mut parts = rest.trim().split(' ');
        let client_nonce = parse_hex::<NONCE_BYTES>(parts.next()?)?;
        let proof = parse_hex::<32>(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        return Some(Handshake::Join {
            client_nonce,
            proof,
        });
    }
    if let Some(rest) = text.strip_prefix(WELCOME_PREFIX) {
        let mut parts = rest.trim().split(' ');
        let host_nonce = parse_hex::<NONCE_BYTES>(parts.next()?)?;
        let proof = parse_hex::<32>(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        return Some(Handshake::Welcome {
            host_nonce,
            proof,
        });
    }
    None
}

// ---------------------------------------------------------------------------
// Audio packets
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct AudioPacket {
    pub sequence: u32,
    pub sample_rate: u32,
    pub channels: u8,
    pub frames: u16,
    pub samples: Vec<f32>,
}

impl AudioPacket {
    fn validate(&self) -> Result<(), ShareError> {
        if self.channels == 0
            || self.channels > MAX_CHANNELS
            || self.frames == 0
            || self.frames > MAX_FRAMES
        {
            return Err(ShareError::Malformed);
        }
        let expected = self.frames as usize * self.channels as usize;
        if self.samples.len() != expected {
            return Err(ShareError::Malformed);
        }
        Ok(())
    }
}

fn packet_header(packet: &AudioPacket, counter: u64) -> [u8; AUDIO_HEADER_BYTES] {
    let mut out = [0u8; AUDIO_HEADER_BYTES];
    out[0..4].copy_from_slice(&AUDIO_MAGIC.to_le_bytes());
    out[4..6].copy_from_slice(&AUDIO_VERSION.to_le_bytes());
    out[6] = packet.channels;
    out[7] = 0;
    out[8..12].copy_from_slice(&packet.sample_rate.to_le_bytes());
    out[12..14].copy_from_slice(&packet.frames.to_le_bytes());
    out[14..16].copy_from_slice(&0u16.to_le_bytes());
    out[16..20].copy_from_slice(&packet.sequence.to_le_bytes());
    out[20..28].copy_from_slice(&counter.to_le_bytes());
    out
}

fn parse_header(bytes: &[u8]) -> Result<(u32, u8, u16, u32, u64), ShareError> {
    if bytes.len() < AUDIO_HEADER_BYTES {
        return Err(ShareError::Malformed);
    }
    let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    if word(0) != AUDIO_MAGIC || u16::from_le_bytes([bytes[4], bytes[5]]) != AUDIO_VERSION {
        return Err(ShareError::Malformed);
    }
    let channels = bytes[6];
    let sample_rate = word(8);
    let frames = u16::from_le_bytes([bytes[12], bytes[13]]);
    let sequence = word(16);
    let counter = u64::from_le_bytes(bytes[20..28].try_into().unwrap());
    if channels == 0
        || channels > MAX_CHANNELS
        || frames == 0
        || frames > MAX_FRAMES
        || !(8_000..=384_000).contains(&sample_rate)
    {
        return Err(ShareError::Malformed);
    }
    Ok((sequence, channels, frames, sample_rate, counter))
}

/// The nonce is this session's stream id followed by the packet counter.
fn packet_nonce(key: &[u8; 32], counter: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[..4].copy_from_slice(&key[..4]);
    nonce[4..].copy_from_slice(&counter.to_be_bytes());
    nonce
}

pub fn seal_audio(key: &[u8; 32], packet: &AudioPacket, counter: u64) -> Result<Vec<u8>, ShareError> {
    packet.validate()?;
    let header = packet_header(packet, counter);
    let mut plain = Vec::with_capacity(packet.samples.len() * 4);
    for sample in &packet.samples {
        plain.extend_from_slice(&sample.to_le_bytes());
    }
    let sealed = aead_seal(key, &packet_nonce(key, counter), &header, &plain);
    let mut out = Vec::with_capacity(AUDIO_HEADER_BYTES + sealed.len());
    out.extend_from_slice(&header);
    out.extend_from_slice(&sealed);
    Ok(out)
}

pub fn open_audio(key: &[u8; 32], bytes: &[u8]) -> Result<(AudioPacket, u64), ShareError> {
    let (sequence, channels, frames, sample_rate, counter) = parse_header(bytes)?;
    let expected = frames as usize * channels as usize;
    if bytes.len() != AUDIO_HEADER_BYTES + expected * 4 + TAG_BYTES {
        return Err(ShareError::Malformed);
    }
    let nonce = packet_nonce(key, counter);
    let plain = aead_open(
        key,
        &nonce,
        &bytes[..AUDIO_HEADER_BYTES],
        &bytes[AUDIO_HEADER_BYTES..],
    )
    .ok_or(ShareError::Auth)?;
    let mut samples = Vec::with_capacity(expected);
    for chunk in plain.chunks_exact(4) {
        samples.push(f32::from_le_bytes(chunk.try_into().unwrap()));
    }
    let packet = AudioPacket {
        sequence,
        sample_rate,
        channels,
        frames,
        samples,
    };
    packet.validate()?;
    Ok((packet, counter))
}

/// TCP keeps the packets in order, so anything not strictly newer is a
/// replay and is dropped instead of played again.
#[derive(Debug, Default, Clone)]
pub struct PacketWindow {
    highest: Option<u64>,
}

impl PacketWindow {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn accept(&mut self, counter: u64) -> bool {
        match self.highest {
            Some(highest) if counter <= highest => false,
            _ => {
                self.highest = Some(counter);
                true
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SHA-256 helpers
// ---------------------------------------------------------------------------

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&sha256_digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Vec::with_capacity(64 + message.len());
    for byte in &block {
        inner.push(byte ^ 0x36);
    }
    inner.extend_from_slice(message);
    let inner_digest = sha256_digest(&inner);
    let mut outer = Vec::with_capacity(96);
    for byte in &block {
        outer.push(byte ^ 0x5c);
    }
    outer.extend_from_slice(&inner_digest);
    sha256_digest(&outer)
}

fn pbkdf2_sha256(password: &[u8], salt: &[u8], rounds: u32, length: usize) -> [u8; 32] {
    debug_assert_eq!(length, 32);
    let mut first = Vec::with_capacity(salt.len() + 4);
    first.extend_from_slice(salt);
    first.extend_from_slice(&1u32.to_be_bytes());
    let mut block = hmac_sha256(password, &first);
    let mut result = block;
    for _ in 1..rounds {
        block = hmac_sha256(password, &block);
        for (byte, add) in result.iter_mut().zip(block.iter()) {
            *byte ^= add;
        }
    }
    result
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

pub fn proof_matches(expected: &[u8; 32], given: &[u8; 32]) -> bool {
    constant_time_eq(expected, given)
}

// ---------------------------------------------------------------------------
// Hex
// ---------------------------------------------------------------------------

fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(*byte >> 4) as usize] as char);
        out.push(DIGITS[(*byte & 0x0f) as usize] as char);
    }
    out
}

fn parse_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (index, chunk) in text.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_digit(chunk[0])?;
        let low = hex_digit(chunk[1])?;
        out[index] = (high << 4) | low;
    }
    Some(out)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Entropy
// ---------------------------------------------------------------------------

/// Nonces, salts and codes. The OS answers; the fallback only keeps a build
/// that has neither a working `/dev/urandom` nor `bcrypt` from returning the
/// same bytes twice, and the handshake still refuses a wrong code.
pub fn random_bytes(count: usize) -> Vec<u8> {
    let mut out = vec![0u8; count];
    if fill_from_os(&mut out) {
        return out;
    }
    fallback_entropy(&mut out);
    out
}

#[cfg(unix)]
fn fill_from_os(out: &mut [u8]) -> bool {
    use std::io::Read;
    let mut file = match std::fs::File::open("/dev/urandom") {
        Ok(file) => file,
        Err(_) => return false,
    };
    file.read_exact(out).is_ok()
}

#[cfg(windows)]
fn fill_from_os(out: &mut [u8]) -> bool {
    use std::ffi::c_void;
    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;
    #[link(name = "bcrypt")]
    extern "system" {
        fn BCryptGenRandom(
            algorithm: *mut c_void,
            buffer: *mut u8,
            length: u32,
            flags: u32,
        ) -> i32;
    }
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            out.as_mut_ptr(),
            out.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    status == 0
}

#[cfg(not(any(unix, windows)))]
fn fill_from_os(_out: &mut [u8]) -> bool {
    false
}

fn fallback_entropy(out: &mut [u8]) {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut seed = Vec::new();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    seed.extend_from_slice(&nanos.to_le_bytes());
    seed.extend_from_slice(&std::process::id().to_le_bytes());
    seed.extend_from_slice(&COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    seed.extend_from_slice(&(&seed as *const Vec<u8> as usize).to_le_bytes());
    let digest = sha256_digest(&seed);
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = digest[index % digest.len()];
    }
}

// ---------------------------------------------------------------------------
// ChaCha20-Poly1305 (RFC 8439)
// ---------------------------------------------------------------------------

fn quarter_round(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    state[a] = state[a].wrapping_add(state[b]);
    state[d] ^= state[a];
    state[d] = state[d].rotate_left(16);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] ^= state[c];
    state[b] = state[b].rotate_left(12);
    state[a] = state[a].wrapping_add(state[b]);
    state[d] ^= state[a];
    state[d] = state[d].rotate_left(8);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] ^= state[c];
    state[b] = state[b].rotate_left(7);
}

fn chacha20_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let mut state = [0u32; 16];
    state[0] = 0x6170_7865;
    state[1] = 0x3320_646e;
    state[2] = 0x7962_2d32;
    state[3] = 0x6b20_6574;
    for index in 0..8 {
        state[4 + index] = u32::from_le_bytes(key[index * 4..index * 4 + 4].try_into().unwrap());
    }
    state[12] = counter;
    for index in 0..3 {
        state[13 + index] =
            u32::from_le_bytes(nonce[index * 4..index * 4 + 4].try_into().unwrap());
    }
    let mut working = state;
    for _ in 0..10 {
        quarter_round(&mut working, 0, 4, 8, 12);
        quarter_round(&mut working, 1, 5, 9, 13);
        quarter_round(&mut working, 2, 6, 10, 14);
        quarter_round(&mut working, 3, 7, 11, 15);
        quarter_round(&mut working, 0, 5, 10, 15);
        quarter_round(&mut working, 1, 6, 11, 12);
        quarter_round(&mut working, 2, 7, 8, 13);
        quarter_round(&mut working, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for index in 0..16 {
        let word = working[index].wrapping_add(state[index]);
        out[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

fn chacha20_xor(key: &[u8; 32], nonce: &[u8; 12], counter: u32, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut block_counter = counter;
    for chunk in data.chunks(64) {
        let stream = chacha20_block(key, block_counter, nonce);
        for (index, byte) in chunk.iter().enumerate() {
            out.push(byte ^ stream[index]);
        }
        block_counter = block_counter.wrapping_add(1);
    }
    out
}

fn poly1305(key: &[u8; 32], message: &[u8]) -> [u8; 16] {
    const MASK: u32 = 0x3ff_ffff;
    let r0 = u32::from_le_bytes([key[0], key[1], key[2], key[3]]) & 0x03ff_ffff;
    let r1 = (u32::from_le_bytes([key[3], key[4], key[5], key[6]]) >> 2) & 0x03ff_ff03;
    let r2 = (u32::from_le_bytes([key[6], key[7], key[8], key[9]]) >> 4) & 0x03ff_c0ff;
    let r3 = (u32::from_le_bytes([key[9], key[10], key[11], key[12]]) >> 6) & 0x03f0_3fff;
    let r4 = (u32::from_le_bytes([key[12], key[13], key[14], key[15]]) >> 8) & 0x000f_ffff;
    let mut h0 = 0u32;
    let mut h1 = 0u32;
    let mut h2 = 0u32;
    let mut h3 = 0u32;
    let mut h4 = 0u32;
    for chunk in message.chunks(16) {
        let mut block = [0u8; 17];
        block[..chunk.len()].copy_from_slice(chunk);
        block[chunk.len()] = 1;
        let t0 = u32::from_le_bytes(block[0..4].try_into().unwrap()) & MASK;
        let t1 = (u32::from_le_bytes(block[3..7].try_into().unwrap()) >> 2) & MASK;
        let t2 = (u32::from_le_bytes(block[6..10].try_into().unwrap()) >> 4) & MASK;
        let t3 = (u32::from_le_bytes(block[9..13].try_into().unwrap()) >> 6) & MASK;
        let t4 = ((u32::from_le_bytes(block[12..16].try_into().unwrap()) >> 8)
            | ((block[16] as u32) << 24))
            & MASK;
        h0 += t0;
        h1 += t1;
        h2 += t2;
        h3 += t3;
        h4 += t4;

        let d0 = h0 as u64 * r0 as u64
            + h1 as u64 * 5 * r4 as u64
            + h2 as u64 * 5 * r3 as u64
            + h3 as u64 * 5 * r2 as u64
            + h4 as u64 * 5 * r1 as u64;
        let mut d1 = h0 as u64 * r1 as u64
            + h1 as u64 * r0 as u64
            + h2 as u64 * 5 * r4 as u64
            + h3 as u64 * 5 * r3 as u64
            + h4 as u64 * 5 * r2 as u64;
        let mut d2 = h0 as u64 * r2 as u64
            + h1 as u64 * r1 as u64
            + h2 as u64 * r0 as u64
            + h3 as u64 * 5 * r4 as u64
            + h4 as u64 * 5 * r3 as u64;
        let mut d3 = h0 as u64 * r3 as u64
            + h1 as u64 * r2 as u64
            + h2 as u64 * r1 as u64
            + h3 as u64 * r0 as u64
            + h4 as u64 * 5 * r4 as u64;
        let mut d4 = h0 as u64 * r4 as u64
            + h1 as u64 * r3 as u64
            + h2 as u64 * r2 as u64
            + h3 as u64 * r1 as u64
            + h4 as u64 * r0 as u64;

        let mut c = d0 >> 26;
        h0 = (d0 as u32) & MASK;
        d1 += c;
        c = d1 >> 26;
        h1 = (d1 as u32) & MASK;
        d2 += c;
        c = d2 >> 26;
        h2 = (d2 as u32) & MASK;
        d3 += c;
        c = d3 >> 26;
        h3 = (d3 as u32) & MASK;
        d4 += c;
        c = d4 >> 26;
        h4 = (d4 as u32) & MASK;
        h0 += c as u32 * 5;
        c = (h0 >> 26) as u64;
        h0 &= MASK;
        h1 += c as u32;
    }

    let mut c = h1 >> 26;
    h1 &= MASK;
    h2 += c;
    c = h2 >> 26;
    h2 &= MASK;
    h3 += c;
    c = h3 >> 26;
    h3 &= MASK;
    h4 += c;
    c = h4 >> 26;
    h4 &= MASK;
    h0 += c * 5;
    c = h0 >> 26;
    h0 &= MASK;
    h1 += c;

    let mut g0 = h0.wrapping_add(5);
    c = g0 >> 26;
    g0 &= MASK;
    let mut g1 = h1.wrapping_add(c);
    c = g1 >> 26;
    g1 &= MASK;
    let mut g2 = h2.wrapping_add(c);
    c = g2 >> 26;
    g2 &= MASK;
    let mut g3 = h3.wrapping_add(c);
    c = g3 >> 26;
    g3 &= MASK;
    let g4 = h4.wrapping_add(c).wrapping_sub(1 << 26);

    let mask = (g4 >> 31).wrapping_sub(1);
    let not_mask = !mask;
    h0 = (h0 & not_mask) | (g0 & mask);
    h1 = (h1 & not_mask) | (g1 & mask);
    h2 = (h2 & not_mask) | (g2 & mask);
    h3 = (h3 & not_mask) | (g3 & mask);
    h4 = (h4 & not_mask) | (g4 & mask);

    let h0 = h0 | (h1 << 26);
    let h1 = (h1 >> 6) | (h2 << 20);
    let h2 = (h2 >> 12) | (h3 << 14);
    let h3 = (h3 >> 18) | (h4 << 8);

    let f0 = h0 as u64 + u32::from_le_bytes(key[16..20].try_into().unwrap()) as u64;
    let f1 = h1 as u64
        + u32::from_le_bytes(key[20..24].try_into().unwrap()) as u64
        + (f0 >> 32);
    let f2 = h2 as u64
        + u32::from_le_bytes(key[24..28].try_into().unwrap()) as u64
        + (f1 >> 32);
    let f3 = h3 as u64
        + u32::from_le_bytes(key[28..32].try_into().unwrap()) as u64
        + (f2 >> 32);

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&(f0 as u32).to_le_bytes());
    out[4..8].copy_from_slice(&(f1 as u32).to_le_bytes());
    out[8..12].copy_from_slice(&(f2 as u32).to_le_bytes());
    out[12..16].copy_from_slice(&(f3 as u32).to_le_bytes());
    out
}

fn pad16(out: &mut Vec<u8>, length: usize) {
    let remainder = length % 16;
    if remainder != 0 {
        out.extend(std::iter::repeat_n(0u8, 16 - remainder));
    }
}

fn poly1305_key(key: &[u8; 32], nonce: &[u8; 12]) -> [u8; 32] {
    let block = chacha20_block(key, 0, nonce);
    let mut out = [0u8; 32];
    out.copy_from_slice(&block[..32]);
    out
}

fn aead_tag(
    poly_key: &[u8; 32],
    aad: &[u8],
    ciphertext: &[u8],
) -> [u8; TAG_BYTES] {
    let mut material = Vec::with_capacity(aad.len() + ciphertext.len() + 32);
    material.extend_from_slice(aad);
    pad16(&mut material, aad.len());
    material.extend_from_slice(ciphertext);
    pad16(&mut material, ciphertext.len());
    material.extend_from_slice(&(aad.len() as u64).to_le_bytes());
    material.extend_from_slice(&(ciphertext.len() as u64).to_le_bytes());
    poly1305(poly_key, &material)
}

pub fn aead_seal(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    plaintext: &[u8],
) -> Vec<u8> {
    let ciphertext = chacha20_xor(key, nonce, 1, plaintext);
    let tag = aead_tag(&poly1305_key(key, nonce), aad, &ciphertext);
    let mut out = ciphertext;
    out.extend_from_slice(&tag);
    out
}

pub fn aead_open(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    sealed: &[u8],
) -> Option<Vec<u8>> {
    if sealed.len() < TAG_BYTES {
        return None;
    }
    let split = sealed.len() - TAG_BYTES;
    let ciphertext = &sealed[..split];
    let tag = aead_tag(&poly1305_key(key, nonce), aad, ciphertext);
    if !constant_time_eq(&tag, &sealed[split..]) {
        return None;
    }
    Some(chacha20_xor(key, nonce, 1, ciphertext))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_hex<const N: usize>(text: &str) -> [u8; N] {
        parse_hex::<N>(text).expect("test vector is hex")
    }

    const RFC_KEY: &str = "808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f";
    const RFC_NONCE: &str = "070000004041424344454647";
    const RFC_AAD: &str = "50515253c0c1c2c3c4c5c6c7";

    // RFC 8439 §2.4.2
    #[test]
    fn chacha20_block_matches_the_rfc_vector() {
        let key = from_hex::<32>(
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        );
        let nonce = from_hex::<12>("000000090000004a00000000");
        let block = chacha20_block(&key, 1, &nonce);
        let expected = from_hex::<64>(
            "10f1e7e4d13b5915500fdd1fa32071c4c7d1f4c733c068030422aa9ac3d46c4e\
             d2826446079faa0914c2d705d98b02a2b5129cd1de164eb9cbd083e8a2503c4e",
        );
        assert_eq!(block, expected);
    }

    // RFC 8439 §2.5.2
    #[test]
    fn poly1305_matches_the_rfc_vector() {
        let key = from_hex::<32>(
            "85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b",
        );
        let message = b"Cryptographic Forum Research Group";
        let expected = from_hex::<16>("a8061dc1305136c6c22b8baf0c0127a9");
        assert_eq!(poly1305(&key, message), expected);
    }

    // RFC 8439 §2.8.2
    #[test]
    fn aead_matches_the_rfc_vector() {
        let key = from_hex::<32>(RFC_KEY);
        let nonce = from_hex::<12>(RFC_NONCE);
        let aad = from_hex::<12>(RFC_AAD);
        let plaintext = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        let sealed = aead_seal(&key, &nonce, &aad, plaintext);
        let tag = from_hex::<16>("1ae10b594f09e26a7e902ecbd0600691");
        assert_eq!(&sealed[sealed.len() - TAG_BYTES..], &tag);
        let opened = aead_open(&key, &nonce, &aad, &sealed).expect("sealed here");
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn a_flipped_byte_does_not_open() {
        let key = [7u8; 32];
        let nonce = [9u8; 12];
        let mut sealed = aead_seal(&key, &nonce, b"header", b"audio");
        sealed[2] ^= 0x40;
        assert!(aead_open(&key, &nonce, b"header", &sealed).is_none());
    }

    #[test]
    fn a_short_pairing_code_is_one_people_can_type() {
        for _ in 0..64 {
            let code = new_pairing_code();
            assert_eq!(code.len(), CODE_LEN);
            assert_eq!(normalize_pairing_code(&code).as_deref(), Some(code.as_str()));
        }
        assert_eq!(
            normalize_pairing_code("abcd-efgh").as_deref(),
            Some("ABCDEFGH")
        );
        assert!(normalize_pairing_code("ABCD-EFG").is_none());
        assert!(normalize_pairing_code("ABCD0FGH").is_none());
    }

    #[test]
    fn a_fingerprint_names_the_code_without_spelling_it() {
        let one = code_fingerprint("ABCDEFGH");
        let two = code_fingerprint("ABCDEFGJ");
        assert_eq!(one, code_fingerprint("ABCDEFGH"));
        assert_ne!(one, two);
        let words = hex_lower(&one);
        assert_eq!(words.len(), 16);
        assert!(words.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn discovery_words_round_trip() {
        let fingerprint = code_fingerprint("ABCDEFGH");
        let word = encode_discover(&fingerprint);
        assert_eq!(parse_discovery(&word), Some(Discovery::Discover { fingerprint }));

        let salt = [0x5au8; SALT_BYTES];
        let word = encode_host("Ivan's PC", 47_811, &salt);
        assert_eq!(
            parse_discovery(&word),
            Some(Discovery::Host {
                name: String::from("Ivan's-PC"),
                port: 47_811,
                salt,
            })
        );
        assert!(parse_discovery(b"HEAD / HTTP/1.1\r\n").is_none());
    }

    #[test]
    fn the_handshake_proves_the_code_and_derives_fresh_keys() {
        let salt = [3u8; SALT_BYTES];
        let key = pairing_key("ABCDEFGH", &salt);
        let client_nonce = [4u8; NONCE_BYTES];
        let host_nonce = [5u8; NONCE_BYTES];
        let join = encode_join(&client_nonce, &client_proof(&key, &salt, &client_nonce));
        let Some(Handshake::Join {
            client_nonce: seen,
            proof,
        }) = parse_handshake(&join)
        else {
            panic!("the join word did not parse");
        };
        assert!(proof_matches(
            &client_proof(&key, &salt, &seen),
            &proof
        ));

        let wrong = pairing_key("ABCDEFGJ", &salt);
        assert!(!proof_matches(&client_proof(&wrong, &salt, &seen), &proof));

        let welcome = encode_welcome(
            &host_nonce,
            &host_proof(&key, &salt, &client_nonce, &host_nonce),
        );
        let Some(Handshake::Welcome {
            host_nonce: seen_host,
            proof,
        }) = parse_handshake(&welcome)
        else {
            panic!("the welcome word did not parse");
        };
        assert!(proof_matches(
            &host_proof(&key, &salt, &client_nonce, &seen_host),
            &proof
        ));

        let (to_host, to_client) = session_keys(&key, &client_nonce, &host_nonce);
        assert_ne!(to_host, to_client);
        assert_ne!(to_host, key);
        let (again_to_host, _) = session_keys(&key, &client_nonce, &host_nonce);
        assert_eq!(to_host, again_to_host);
        let other = session_keys(&key, &[6u8; NONCE_BYTES], &host_nonce).0;
        assert_ne!(to_host, other);
    }

    #[test]
    fn an_audio_packet_opens_where_it_was_sealed_and_nowhere_else() {
        let key = [11u8; 32];
        let packet = AudioPacket {
            sequence: 7,
            sample_rate: 48_000,
            channels: 2,
            frames: 3,
            samples: vec![0.0, 0.25, -0.5, 1.0, -1.0, 0.75],
        };
        let wire = seal_audio(&key, &packet, 42).expect("a valid packet");
        let (opened, counter) = open_audio(&key, &wire).expect("sealed here");
        assert_eq!(opened, packet);
        assert_eq!(counter, 42);
        assert_eq!(open_audio(&[12u8; 32], &wire).unwrap_err(), ShareError::Auth);

        let mut tampered = wire.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        assert_eq!(open_audio(&key, &tampered).unwrap_err(), ShareError::Auth);

        let mut truncated = wire;
        truncated.pop();
        assert_eq!(open_audio(&key, &truncated).unwrap_err(), ShareError::Malformed);
    }

    #[test]
    fn the_same_packet_with_another_counter_is_another_ciphertext() {
        let key = [13u8; 32];
        let packet = AudioPacket {
            sequence: 1,
            sample_rate: 44_100,
            channels: 1,
            frames: 2,
            samples: vec![0.1, 0.2],
        };
        let first = seal_audio(&key, &packet, 1).expect("sealed");
        let second = seal_audio(&key, &packet, 2).expect("sealed");
        assert_ne!(first, second);
    }

    #[test]
    fn a_replayed_counter_is_refused() {
        let mut window = PacketWindow::new();
        assert!(window.accept(1));
        assert!(window.accept(2));
        assert!(!window.accept(2));
        assert!(!window.accept(1));
        assert!(window.accept(3));
    }

    #[test]
    fn a_packet_that_is_not_a_packet_is_malformed() {
        let key = [0u8; 32];
        assert_eq!(open_audio(&key, b"hello").unwrap_err(), ShareError::Malformed);
        let mut wire = vec![0u8; AUDIO_HEADER_BYTES + TAG_BYTES];
        wire[0..4].copy_from_slice(&AUDIO_MAGIC.to_le_bytes());
        wire[4..6].copy_from_slice(&AUDIO_VERSION.to_le_bytes());
        wire[6] = 2;
        wire[12..14].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(open_audio(&key, &wire).unwrap_err(), ShareError::Malformed);
    }

    #[test]
    fn pbkdf2_matches_the_published_sha256_vectors() {
        assert_eq!(
            hex_lower(&pbkdf2_sha256(b"password", b"salt", 1, 32)),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
        assert_eq!(
            hex_lower(&pbkdf2_sha256(b"password", b"salt", 2, 32)),
            "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"
        );
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        assert_eq!(
            hex_lower(&hmac_sha256(&[0x0bu8; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn entropy_is_not_the_same_twice() {
        let one = random_bytes(32);
        let two = random_bytes(32);
        assert_eq!(one.len(), 32);
        assert_ne!(one, two);
    }
}
