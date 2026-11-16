/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Share Audio between computers on one network.
//!
//! One machine **hosts**: it shows a pairing code, answers a discovery word
//! only for that code, and streams its system mix — captured under the
//! playback helper's tree, so the sound it is already playing never comes
//! back — to every computer that joins. Another **joins**: it types the code,
//! finds the host by broadcast, proves the code over TCP, and hands the
//! decrypted packets to the playback helper.
//!
//! The wire itself is in `fluideq_state::share_audio`: discovery words, the
//! handshake, the keys and the sealed packets. This module owns the sockets
//! and the threads, and nothing else.

use crate::capture_pipe::{self, CaptureFrame, CaptureSession};
use crate::playback_host::{CaptureMode, PlaybackHost, PlaybackHostState};
use fluideq_state::share_audio::{
    self, AudioPacket, Discovery, Handshake, AUDIO_PORT, DISCOVERY_PORT, NONCE_BYTES,
    SALT_BYTES,
};
use serde::Serialize;
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

pub const EVENT: &str = "lan-share";
const MAX_LINE_BYTES: usize = 512;
const MAX_PACKET_BYTES: usize = 1920 * 8 * 4 + 64;
const MAX_CLIENTS: usize = 8;
const GUEST_PEER: u32 = 1;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const SCAN_WINDOW: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanShareSnapshot {
    /// The helpers that carry share audio are installed beside this build.
    pub available: bool,
    /// `off`, `host` or `guest`.
    pub role: String,
    /// The code a host shows, and a guest types.
    pub code: Option<String>,
    /// The host's name, as a guest heard it.
    pub peer_name: Option<String>,
    /// What is happening, in one short sentence.
    pub state: String,
    pub connected: u32,
    /// The receiving side's meter, from the playback helper.
    pub buffered_ms: f64,
    pub peak: f64,
    pub rms: f64,
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FoundHost {
    pub name: String,
    pub address: String,
    pub port: u16,
}

struct Discovered {
    found: FoundHost,
    salt: [u8; SALT_BYTES],
}

struct ClientSink {
    address: String,
    key: [u8; 32],
    counter: AtomicU64,
    stream: TcpStream,
    queue: SyncSender<Vec<u8>>,
}

struct Clients(Vec<ClientSink>);

impl Clients {
    fn remove(&mut self, address: &str) -> bool {
        let before = self.0.len();
        self.0.retain(|sink| sink.address != address);
        before != self.0.len()
    }

    fn broadcast(&self, packet: &AudioPacket) {
        for sink in &self.0 {
            let counter = sink.counter.fetch_add(1, Ordering::Relaxed);
            let Ok(wire) = share_audio::seal_audio(&sink.key, packet, counter) else {
                continue;
            };
            let mut framed = Vec::with_capacity(4 + wire.len());
            framed.extend_from_slice(&(wire.len() as u32).to_le_bytes());
            framed.extend_from_slice(&wire);
            // A receiver that cannot keep up loses a packet, not the stream:
            // it is live audio, and the next one carries the clock.
            let _ = sink.queue.try_send(framed);
        }
    }
}

struct HostSession {
    code: String,
    port: u16,
    stop: Arc<AtomicBool>,
    clients: Arc<Mutex<Clients>>,
    capture: CaptureSession,
}

struct GuestSession {
    stop: Arc<AtomicBool>,
    stream: TcpStream,
}

pub struct LanShareState {
    snapshot: Arc<Mutex<LanShareSnapshot>>,
    discovered: Mutex<Vec<Discovered>>,
    host: Mutex<Option<HostSession>>,
    guest: Mutex<Option<GuestSession>>,
}

impl Default for LanShareState {
    fn default() -> Self {
        Self::new()
    }
}

impl LanShareState {
    pub fn new() -> Self {
        Self {
            snapshot: Arc::new(Mutex::new(LanShareSnapshot {
                state: String::from("Off"),
                ..LanShareSnapshot::default()
            })),
            discovered: Mutex::new(Vec::new()),
            host: Mutex::new(None),
            guest: Mutex::new(None),
        }
    }

    pub fn snapshot(&self) -> LanShareSnapshot {
        self.snapshot
            .lock()
            .map(|state| state.clone())
            .unwrap_or_default()
    }

    fn edit(&self, app: &AppHandle, change: impl FnOnce(&mut LanShareSnapshot)) {
        if let Ok(mut state) = self.snapshot.lock() {
            change(&mut state);
        }
        publish(&self.snapshot, app);
    }
}

fn publish(snapshot: &Arc<Mutex<LanShareSnapshot>>, app: &AppHandle) {
    if let Ok(state) = snapshot.lock() {
        let _ = app.emit(EVENT, state.clone());
    }
}

fn host_name() -> String {
    share_audio::sanitize_name(
        &std::env::var("COMPUTERNAME").unwrap_or_else(|_| String::from("FluidEQ")),
    )
}

pub fn helpers_available(app: &AppHandle) -> bool {
    crate::second_output::capture_exe(app).is_some()
        && crate::playback_host::playback_exe(app).is_some()
}

/// Everything this app holds while it shares.
pub fn start_host(
    app: &AppHandle,
    playback: &PlaybackHostState,
    state: &LanShareState,
) -> Result<LanShareSnapshot, String> {
    if state.host.lock().map(|host| host.is_some()).unwrap_or(false) {
        return Err(String::from("this computer is already sharing"));
    }
    if !helpers_available(app) {
        return Err(String::from(
            "the share helpers are not installed in this build",
        ));
    }
    let helper = playback.ensure(app)?;

    let code = share_audio::new_pairing_code();
    let salt_bytes = share_audio::random_bytes(SALT_BYTES);
    let mut salt = [0u8; SALT_BYTES];
    salt.copy_from_slice(&salt_bytes);
    let key = share_audio::pairing_key(&code, &salt);
    let fingerprint = share_audio::code_fingerprint(&code);
    let name = host_name();

    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, AUDIO_PORT))
        .map_err(|error| format!("the share port could not be opened: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("the share port could not be read: {error}"))?
        .port();
    let discovery = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT))
        .map_err(|error| format!("the discovery port could not be opened: {error}"))?;
    let _ = discovery.set_broadcast(true);

    let stop = Arc::new(AtomicBool::new(false));
    let clients = Arc::new(Mutex::new(Clients(Vec::new())));
    let snapshot = Arc::clone(&state.snapshot);
    let capture_clients = Arc::clone(&clients);
    let capture = helper.start_capture(
        CaptureMode::Lan,
        move |frame: &CaptureFrame| forward_capture(frame, &capture_clients),
        {
            let snapshot = Arc::clone(&state.snapshot);
            let app = app.clone();
            move || {
                if let Ok(mut state) = snapshot.lock() {
                    if state.role == "host" {
                        state.problem = Some(String::from("the audio capture stopped"));
                        state.state = String::from("The capture stopped");
                    }
                }
                publish(&snapshot, &app);
            }
        },
    )?;

    {
        let snapshot = Arc::clone(&snapshot);
        let app = app.clone();
        let stop = Arc::clone(&stop);
        std::thread::Builder::new()
            .name(String::from("fluideq-share-discovery"))
            .spawn(move || {
                discovery_loop(discovery, fingerprint, name, port, salt, stop, snapshot, app)
            })
            .map_err(|error| format!("discovery could not start: {error}"))?;
    }
    {
        let snapshot = Arc::clone(&snapshot);
        let app = app.clone();
        let stop = Arc::clone(&stop);
        let clients = Arc::clone(&clients);
        std::thread::Builder::new()
            .name(String::from("fluideq-share-accept"))
            .spawn(move || {
                accept_loop(listener, key, salt, stop, clients, snapshot, app);
            })
            .map_err(|error| format!("the share listener could not start: {error}"))?;
    }

    let session = HostSession {
        code: code.clone(),
        port,
        stop,
        clients: Arc::clone(&clients),
        capture,
    };
    *state
        .host
        .lock()
        .map_err(|_| String::from("the share state is not reachable"))? = Some(session);

    state.edit(app, |state| {
        state.available = true;
        state.role = String::from("host");
        state.code = Some(code);
        state.peer_name = None;
        state.connected = 0;
        state.state = String::from("Waiting for another computer");
        state.problem = None;
        state.buffered_ms = 0.0;
        state.peak = 0.0;
        state.rms = 0.0;
    });
    Ok(state.snapshot())
}

pub fn stop_host(app: &AppHandle, state: &LanShareState) -> Result<LanShareSnapshot, String> {
    let session = state
        .host
        .lock()
        .map_err(|_| String::from("the share state is not reachable"))?
        .take();
    if let Some(session) = session {
        session.stop.store(true, Ordering::Release);
        session.capture.shutdown();
        if let Ok(clients) = session.clients.lock() {
            for sink in &clients.0 {
                let _ = sink.stream.shutdown(Shutdown::Both);
            }
        }
        // A connection to our own listener is how the blocked accept learns
        // to look at the flag; it is closed before it can say hello.
        let _ = TcpStream::connect((Ipv4Addr::LOCALHOST, session.port));
        if let Ok(socket) = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)) {
            let word = share_audio::encode_discover(&share_audio::code_fingerprint(&session.code));
            let _ = socket.send_to(&word, (Ipv4Addr::LOCALHOST, DISCOVERY_PORT));
        }
    }
    state.edit(app, |state| {
        state.role = String::from("off");
        state.code = None;
        state.peer_name = None;
        state.connected = 0;
        state.state = String::from("Off");
        state.problem = None;
        state.buffered_ms = 0.0;
        state.peak = 0.0;
        state.rms = 0.0;
    });
    Ok(state.snapshot())
}

#[allow(clippy::too_many_arguments)]
fn discovery_loop(
    socket: UdpSocket,
    fingerprint: [u8; 8],
    name: String,
    port: u16,
    salt: [u8; SALT_BYTES],
    stop: Arc<AtomicBool>,
    snapshot: Arc<Mutex<LanShareSnapshot>>,
    app: AppHandle,
) {
    let reply = share_audio::encode_host(&name, port, &salt);
    let mut buffer = [0u8; 256];
    while !stop.load(Ordering::Acquire) {
        let Ok((read, from)) = socket.recv_from(&mut buffer) else {
            continue;
        };
        if stop.load(Ordering::Acquire) {
            break;
        }
        if let Some(Discovery::Discover {
            fingerprint: wanted,
        }) = share_audio::parse_discovery(&buffer[..read])
        {
            if wanted == fingerprint {
                let _ = socket.send_to(&reply, from);
            }
        }
    }
    drop(snapshot);
    drop(app);
}

fn accept_loop(
    listener: TcpListener,
    key: [u8; 32],
    salt: [u8; SALT_BYTES],
    stop: Arc<AtomicBool>,
    clients: Arc<Mutex<Clients>>,
    snapshot: Arc<Mutex<LanShareSnapshot>>,
    app: AppHandle,
) {
    for stream in listener.incoming() {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let Ok(stream) = stream else {
            continue;
        };
        let peers = clients.lock().map(|clients| clients.0.len()).unwrap_or(0);
        if peers >= MAX_CLIENTS {
            let _ = stream.shutdown(Shutdown::Both);
            continue;
        }
        let clients = Arc::clone(&clients);
        let snapshot = Arc::clone(&snapshot);
        let app = app.clone();
        let _ = std::thread::Builder::new()
            .name(String::from("fluideq-share-client"))
            .spawn(move || join_client(stream, key, salt, clients, snapshot, app));
    }
}

fn join_client(
    mut stream: TcpStream,
    key: [u8; 32],
    salt: [u8; SALT_BYTES],
    clients: Arc<Mutex<Clients>>,
    snapshot: Arc<Mutex<LanShareSnapshot>>,
    app: AppHandle,
) {
    let peer = stream
        .peer_addr()
        .map(|address| address.to_string())
        .unwrap_or_else(|_| String::from("another computer"));
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT));

    let Some(word) = read_line(&mut stream) else {
        return;
    };
    let Some(Handshake::Join {
        client_nonce,
        proof,
    }) = share_audio::parse_handshake(&word)
    else {
        let _ = stream.write_all(&share_audio::encode_refused());
        let _ = stream.shutdown(Shutdown::Both);
        return;
    };
    if !share_audio::proof_matches(&share_audio::client_proof(&key, &salt, &client_nonce), &proof) {
        let _ = stream.write_all(&share_audio::encode_refused());
        let _ = stream.shutdown(Shutdown::Both);
        return;
    }

    let host_nonce_bytes = share_audio::random_bytes(NONCE_BYTES);
    let mut host_nonce = [0u8; NONCE_BYTES];
    host_nonce.copy_from_slice(&host_nonce_bytes);
    let welcome = share_audio::encode_welcome(
        &host_nonce,
        &share_audio::host_proof(&key, &salt, &client_nonce, &host_nonce),
    );
    if stream.write_all(&welcome).is_err() {
        let _ = stream.shutdown(Shutdown::Both);
        return;
    }
    let _ = stream.set_read_timeout(None);

    let (_, to_client) = share_audio::session_keys(&key, &client_nonce, &host_nonce);
    let (queue, receiver) = sync_channel::<Vec<u8>>(32);
    let Ok(writer_stream) = stream.try_clone() else {
        let _ = stream.shutdown(Shutdown::Both);
        return;
    };
    let sink_stream = match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => {
            let _ = stream.shutdown(Shutdown::Both);
            return;
        }
    };
    if let Ok(mut clients) = clients.lock() {
        clients.0.push(ClientSink {
            address: peer.clone(),
            key: to_client,
            counter: AtomicU64::new(1),
            stream: sink_stream,
            queue,
        });
    }
    if let Ok(mut state) = snapshot.lock() {
        state.connected = state.connected.saturating_add(1);
        state.state = connected_word(state.connected);
        state.problem = None;
    }
    publish(&snapshot, &app);

    let _ = std::thread::Builder::new()
        .name(String::from("fluideq-share-writer"))
        .spawn(move || {
            let mut stream = writer_stream;
            for packet in receiver {
                if stream.write_all(&packet).is_err() {
                    break;
                }
            }
            let _ = stream.shutdown(Shutdown::Both);
        });

    // The guest says nothing after the handshake; a read that ends is how
    // this side learns it left.
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
    let removed = clients
        .lock()
        .map(|mut clients| clients.remove(&peer))
        .unwrap_or(false);
    if removed {
        if let Ok(mut state) = snapshot.lock() {
            state.connected = state.connected.saturating_sub(1);
            state.state = connected_word(state.connected);
        }
        publish(&snapshot, &app);
    }
}

fn connected_word(count: u32) -> String {
    match count {
        0 => String::from("Waiting for another computer"),
        1 => String::from("One computer is listening"),
        count => format!("{count} computers are listening"),
    }
}

fn forward_capture(frame: &CaptureFrame, clients: &Arc<Mutex<Clients>>) {
    if frame.header.kind != capture_pipe::KIND_AUDIO {
        return;
    }
    let channels = frame.header.channels;
    let frames = frame.header.frames;
    if channels == 0 || channels > 8 || frames == 0 {
        return;
    }
    let expected = frames as usize * channels as usize;
    if frame.payload.len() != expected * 4 {
        return;
    }
    let packet = AudioPacket {
        sequence: frame.header.sequence,
        sample_rate: frame.header.sample_rate,
        channels: channels as u8,
        frames,
        samples: frame
            .payload
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
            .collect(),
    };
    if let Ok(clients) = clients.lock() {
        clients.broadcast(&packet);
    }
}

fn read_line(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return None,
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) =>
            {
                return None
            }
            Err(_) => return None,
        }
        if byte[0] == b'\n' {
            return Some(line);
        }
        if line.len() >= MAX_LINE_BYTES {
            return None;
        }
        line.push(byte[0]);
    }
}

/// What the network answered to a pairing code.
pub fn scan(app: &AppHandle, state: &LanShareState, code: &str) -> Result<Vec<FoundHost>, String> {
    let code = share_audio::normalize_pairing_code(code)
        .ok_or_else(|| String::from("that is not a pairing code"))?;
    if !helpers_available(app) {
        return Err(String::from(
            "the share helpers are not installed in this build",
        ));
    }
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .map_err(|error| format!("discovery could not start: {error}"))?;
    let _ = socket.set_broadcast(true);
    let _ = socket.set_read_timeout(Some(Duration::from_millis(250)));
    let word = share_audio::encode_discover(&share_audio::code_fingerprint(&code));
    for _ in 0..3 {
        let _ = socket.send_to(&word, (Ipv4Addr::BROADCAST, DISCOVERY_PORT));
    }

    let deadline = Instant::now() + SCAN_WINDOW;
    let mut buffer = [0u8; 256];
    let mut found: Vec<Discovered> = Vec::new();
    while Instant::now() < deadline {
        match socket.recv_from(&mut buffer) {
            Ok((read, from)) => {
                if let Some(Discovery::Host { name, port, salt }) =
                    share_audio::parse_discovery(&buffer[..read])
                {
                    let address = from.ip().to_string();
                    if !found.iter().any(|known| known.found.address == address) {
                        found.push(Discovered {
                            found: FoundHost {
                                name,
                                address,
                                port,
                            },
                            salt,
                        });
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) => {}
            Err(_) => break,
        }
    }
    let hosts = found
        .iter()
        .map(|entry| entry.found.clone())
        .collect::<Vec<_>>();
    if let Ok(mut discovered) = state.discovered.lock() {
        *discovered = found;
    }
    Ok(hosts)
}

/// Joins the host at `address` and plays what it shares on `endpoint`.
pub fn join(
    app: &AppHandle,
    playback: &PlaybackHostState,
    state: &LanShareState,
    code: &str,
    address: &str,
    endpoint: &str,
) -> Result<LanShareSnapshot, String> {
    let code = share_audio::normalize_pairing_code(code)
        .ok_or_else(|| String::from("that is not a pairing code"))?;
    if state
        .guest
        .lock()
        .map(|guest| guest.is_some())
        .unwrap_or(false)
    {
        return Err(String::from("this computer is already joined"));
    }
    if !fluideq_state::apo::is_valid_endpoint_guid(endpoint) {
        return Err(String::from("pick the output to play the shared sound on"));
    }
    let (port, salt, name) = {
        let discovered = state
            .discovered
            .lock()
            .map_err(|_| String::from("the share state is not reachable"))?;
        let entry = discovered
            .iter()
            .find(|entry| entry.found.address == address)
            .ok_or_else(|| String::from("that computer is no longer answering"))?;
        (entry.found.port, entry.salt, entry.found.name.clone())
    };
    let helper = playback.ensure(app)?;
    let key = share_audio::pairing_key(&code, &salt);

    let mut stream = connect(address, port)?;
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
    let nonce_bytes = share_audio::random_bytes(NONCE_BYTES);
    let mut client_nonce = [0u8; NONCE_BYTES];
    client_nonce.copy_from_slice(&nonce_bytes);
    let join_word = share_audio::encode_join(
        &client_nonce,
        &share_audio::client_proof(&key, &salt, &client_nonce),
    );
    stream
        .write_all(&join_word)
        .map_err(|error| format!("the host did not answer: {error}"))?;
    let answer = read_line(&mut stream)
        .ok_or_else(|| String::from("the host did not answer; the code may be wrong"))?;
    let Some(Handshake::Welcome { host_nonce, proof }) = share_audio::parse_handshake(&answer)
    else {
        let _ = stream.shutdown(Shutdown::Both);
        if matches!(
            share_audio::parse_handshake(&answer),
            Some(Handshake::Refused)
        ) {
            return Err(String::from("the host refused that code"));
        }
        return Err(String::from("the host answered with something else"));
    };
    if !share_audio::proof_matches(
        &share_audio::host_proof(&key, &salt, &client_nonce, &host_nonce),
        &proof,
    ) {
        return Err(String::from("the host could not prove the code"));
    }
    let (_, to_client) = share_audio::session_keys(&key, &client_nonce, &host_nonce);
    let _ = stream.set_read_timeout(None);

    helper.open_output(endpoint)?;
    let _ = helper.smooth(GUEST_PEER, true);

    let stop = Arc::new(AtomicBool::new(false));
    let reader = stream
        .try_clone()
        .map_err(|error| format!("the share stream could not be watched: {error}"))?;
    let session_stream = stream
        .try_clone()
        .map_err(|error| format!("the share stream could not be watched: {error}"))?;
    let snapshot = Arc::clone(&state.snapshot);
    let reader_app = app.clone();
    let reader_stop = Arc::clone(&stop);
    let reader_helper = Arc::clone(&helper);
    std::thread::Builder::new()
        .name(String::from("fluideq-share-guest"))
        .spawn(move || {
            receive_loop(
                reader,
                to_client,
                &reader_helper,
                reader_stop,
                snapshot,
                reader_app,
            )
        })
        .map_err(|error| format!("the share stream could not start: {error}"))?;

    *state
        .guest
        .lock()
        .map_err(|_| String::from("the share state is not reachable"))? = Some(GuestSession {
        stop,
        stream: session_stream,
    });

    state.edit(app, |state| {
        state.available = true;
        state.role = String::from("guest");
        state.code = None;
        state.peer_name = Some(name.clone());
        state.connected = 1;
        state.state = format!("Playing what {name} shares");
        state.problem = None;
        state.buffered_ms = 0.0;
        state.peak = 0.0;
        state.rms = 0.0;
    });
    Ok(state.snapshot())
}

fn connect(address: &str, port: u16) -> Result<TcpStream, String> {
    let target = format!("{address}:{port}");
    let socket = target
        .parse()
        .map_err(|_| String::from("that is not an address"))?;
    TcpStream::connect_timeout(&socket, HANDSHAKE_TIMEOUT)
        .map_err(|error| format!("the host could not be reached: {error}"))
}

fn receive_loop(
    mut stream: TcpStream,
    key: [u8; 32],
    playback: &PlaybackHost,
    stop: Arc<AtomicBool>,
    snapshot: Arc<Mutex<LanShareSnapshot>>,
    app: AppHandle,
) {
    let mut window = share_audio::PacketWindow::new();
    let mut length = [0u8; 4];
    let mut packet = Vec::new();
    let mut since_meter = 0u32;
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        if stream.read_exact(&mut length).is_err() {
            break;
        }
        let bytes = u32::from_le_bytes(length) as usize;
        if bytes == 0 || bytes > MAX_PACKET_BYTES {
            break;
        }
        packet.resize(bytes, 0);
        if stream.read_exact(&mut packet).is_err() {
            break;
        }
        let Ok((audio, counter)) = share_audio::open_audio(&key, &packet) else {
            continue;
        };
        if !window.accept(counter) {
            continue;
        }
        if playback
            .audio(
                GUEST_PEER,
                audio.sequence,
                audio.sample_rate,
                audio.channels,
                audio.frames,
                &audio.samples,
            )
            .is_err()
        {
            break;
        }
        since_meter += 1;
        if since_meter >= 20 {
            since_meter = 0;
            let meter = playback.meter(GUEST_PEER);
            if let Ok(mut state) = snapshot.lock() {
                state.buffered_ms = meter.buffered_ms;
                state.peak = meter.peak;
                state.rms = meter.rms;
            }
            publish(&snapshot, &app);
        }
    }
    if let Ok(mut state) = snapshot.lock() {
        if state.role == "guest" {
            state.problem = Some(String::from("the other computer stopped sharing"));
            state.state = String::from("The share ended");
            state.connected = 0;
        }
    }
    publish(&snapshot, &app);
}

pub fn leave(
    app: &AppHandle,
    playback: &PlaybackHostState,
    state: &LanShareState,
) -> Result<LanShareSnapshot, String> {
    let session = state
        .guest
        .lock()
        .map_err(|_| String::from("the share state is not reachable"))?
        .take();
    if let Some(session) = session {
        session.stop.store(true, Ordering::Release);
        let _ = session.stream.shutdown(Shutdown::Both);
    }
    if let Some(helper) = playback.existing() {
        let _ = helper.remove_peer(GUEST_PEER);
        let _ = helper.close_output();
    }
    state.edit(app, |state| {
        state.role = String::from("off");
        state.code = None;
        state.peer_name = None;
        state.connected = 0;
        state.state = String::from("Off");
        state.problem = None;
        state.buffered_ms = 0.0;
        state.peak = 0.0;
        state.rms = 0.0;
    });
    Ok(state.snapshot())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_list_removes_by_address() {
        let mut clients = Clients(Vec::new());
        assert!(!clients.remove("192.168.1.2"));
    }

    #[test]
    fn the_connected_word_counts_what_is_listening() {
        assert_eq!(
            connected_word(0),
            String::from("Waiting for another computer")
        );
        assert_eq!(
            connected_word(1),
            String::from("One computer is listening")
        );
        assert_eq!(
            connected_word(3),
            String::from("3 computers are listening")
        );
    }
}
