/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! The wire between this app and one capture helper.
//!
//! A capture helper started by the playback helper cannot use the app's
//! stdio, so it opens two connections to a named pipe this side serves: one
//! carries the frames (ready, reply, failure, delay, audio), the other the one
//! command per line that opens, levels or stops a mirror. Both connections
//! introduce themselves with the token that rode on the helper's command line;
//! anything else on the pipe is dropped before it can speak.
//!
//! The pipe starts before the helper is asked to start, because the helper
//! connects the moment it runs. `open_capture` returns as soon as the first
//! instance exists; a caller that queues its first command does not have to
//! wait for the handshake, and a start the playback helper refuses unblocks
//! the accept with a deliberately bad introduction instead of a timer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// The 24-byte header in `capture_queue.h`, field for field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureHeader {
    pub kind: u32,
    pub sequence: u32,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u16,
    pub payload_bytes: u32,
}

pub const HEADER_BYTES: usize = 24;
const MAGIC: u32 = 0x314e_414c;
pub const KIND_READY: u32 = 1;
pub const KIND_AUDIO: u32 = 2;
pub const KIND_REPLY: u32 = 3;
pub const KIND_FAILURE: u32 = 4;
pub const KIND_DELAY: u32 = 5;
const MAX_PAYLOAD_BYTES: usize = 1_920 * 8 * 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureFrame {
    pub header: CaptureHeader,
    pub payload: Vec<u8>,
}

impl CaptureHeader {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != HEADER_BYTES {
            return None;
        }
        let word =
            |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
        if word(0) != MAGIC {
            return None;
        }
        Some(Self {
            kind: word(4),
            sequence: word(8),
            sample_rate: word(12),
            channels: u16::from_le_bytes([bytes[16], bytes[17]]),
            frames: u16::from_le_bytes([bytes[18], bytes[19]]),
            payload_bytes: word(20),
        })
    }

    pub fn payload_allowed(&self) -> bool {
        self.payload_bytes as usize <= MAX_PAYLOAD_BYTES
    }
}

#[derive(Default)]
struct WriterState {
    handle: Option<CommandWriter>,
    pending: Vec<String>,
}

struct CommandWriter {
    #[cfg(target_os = "windows")]
    handle: imp::Handle,
}

impl CommandWriter {
    fn write_line(&mut self, line: &str) -> Result<(), String> {
        let mut bytes = line.as_bytes().to_vec();
        bytes.push(b'\n');
        #[cfg(target_os = "windows")]
        {
            imp::write_all(&self.handle, &bytes)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = bytes;
            Err(String::from(
                "audio sharing needs the Windows capture helper",
            ))
        }
    }
}

struct SessionInner {
    writer: Mutex<WriterState>,
    closed: AtomicBool,
    /// Both introductions arrived; the helper is attached to the wire.
    connected: AtomicBool,
    name: String,
    /// A bad introduction that makes the accept give up when the playback
    /// helper refused to start the capture.
    rejected: AtomicBool,
}

/// One capture helper, as the app sees it.
#[derive(Clone)]
pub struct CaptureSession {
    inner: Arc<SessionInner>,
}

impl CaptureSession {
    /// Queues a mirror command when the helper has not introduced itself yet.
    pub fn send(&self, line: &str) -> Result<(), String> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(String::from("the capture helper is not running"));
        }
        let mut writer = self
            .inner
            .writer
            .lock()
            .map_err(|_| String::from("the capture helper is not reachable"))?;
        match writer.handle.as_mut() {
            Some(handle) => handle.write_line(line),
            None => {
                if writer.pending.len() >= 64 {
                    return Err(String::from("the capture helper is not listening"));
                }
                writer.pending.push(line.to_string());
                Ok(())
            }
        }
    }

    /// Ends the helper: closing the command side is what its reader watches,
    /// and the frames side then closes behind it. A helper that never arrived
    /// is unblocked with a bad introduction.
    pub fn shutdown(&self) {
        self.inner.closed.store(true, Ordering::Release);
        if let Ok(mut writer) = self.inner.writer.lock() {
            writer.handle = None;
            writer.pending.clear();
        }
        if self.inner.rejected.swap(true, Ordering::AcqRel)
            || self.inner.connected.load(Ordering::Acquire)
        {
            return;
        }
        #[cfg(target_os = "windows")]
        {
            imp::reject(&self.inner.name);
        }
    }
}

/// Starts a capture helper's pipe and calls `spawn` with the name, the token
/// and this session before the accept blocks, so the caller can both ask the
/// playback helper to start the capture and keep the handle it will shut down
/// if that request is refused. The frames arrive on `on_frame` until the
/// helper ends or the session is shut down; `on_end` runs exactly once after
/// the wire is gone, whichever of those happened first.
pub fn open_capture<F, E, S>(on_frame: F, on_end: E, spawn: S) -> Result<CaptureSession, String>
where
    F: FnMut(&CaptureFrame) + Send + 'static,
    E: FnOnce() + Send + 'static,
    S: FnOnce(&str, &str, &CaptureSession) -> Result<(), String>,
{
    #[cfg(target_os = "windows")]
    {
        imp::open_capture(Box::new(on_frame), Box::new(on_end), spawn)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (on_frame, on_end, spawn);
        Err(String::from(
            "audio sharing needs the Windows capture helper",
        ))
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::*;
    use std::ffi::c_void;
    use std::ptr;

    const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
    const PIPE_TYPE_BYTE: u32 = 0x0000_0000;
    const PIPE_READMODE_BYTE: u32 = 0x0000_0000;
    const PIPE_WAIT: u32 = 0x0000_0000;
    const PIPE_UNLIMITED_INSTANCES: u32 = 255;
    const ERROR_PIPE_CONNECTED: u32 = 535;
    const INVALID_HANDLE_VALUE: isize = -1;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateNamedPipeW(
            name: *const u16,
            open_mode: u32,
            pipe_mode: u32,
            max_instances: u32,
            out_buffer_size: u32,
            in_buffer_size: u32,
            default_timeout: u32,
            security_attributes: *const c_void,
        ) -> *mut c_void;
        fn ConnectNamedPipe(handle: *mut c_void, overlapped: *mut c_void) -> i32;
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security_attributes: *const c_void,
            creation: u32,
            flags: u32,
            template: *mut c_void,
        ) -> *mut c_void;
        fn ReadFile(
            handle: *mut c_void,
            buffer: *mut u8,
            length: u32,
            read: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
        fn WriteFile(
            handle: *mut c_void,
            buffer: *const u8,
            length: u32,
            written: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }

    pub(super) struct Handle(*mut c_void);
    unsafe impl Send for Handle {}

    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 as isize != INVALID_HANDLE_VALUE {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub(super) fn write_all(handle: &Handle, bytes: &[u8]) -> Result<(), String> {
        let mut sent = 0usize;
        while sent < bytes.len() {
            let mut written = 0u32;
            let result = unsafe {
                WriteFile(
                    handle.0,
                    bytes[sent..].as_ptr(),
                    (bytes.len() - sent) as u32,
                    &mut written,
                    ptr::null_mut(),
                )
            };
            if result == 0 || written == 0 {
                return Err(String::from("the capture helper stopped listening"));
            }
            sent += written as usize;
        }
        Ok(())
    }

    fn read_some(handle: &Handle, buffer: &mut [u8]) -> Result<usize, String> {
        let mut read = 0u32;
        let result = unsafe {
            ReadFile(
                handle.0,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                &mut read,
                ptr::null_mut(),
            )
        };
        if result == 0 {
            return Err(String::from("the capture helper stopped sending"));
        }
        Ok(read as usize)
    }

    fn read_exact(handle: &Handle, buffer: &mut [u8]) -> Result<(), String> {
        let mut filled = 0usize;
        while filled < buffer.len() {
            let read = read_some(handle, &mut buffer[filled..])?;
            if read == 0 {
                return Err(String::from("the capture helper stopped sending"));
            }
            filled += read;
        }
        Ok(())
    }

    fn create_instance(name: &str) -> Result<Handle, String> {
        let name = wide(name);
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                ptr::null(),
            )
        };
        if handle as isize == INVALID_HANDLE_VALUE {
            return Err(format!(
                "a capture pipe could not be opened (0x{:08X})",
                unsafe { GetLastError() }
            ));
        }
        Ok(Handle(handle))
    }

    fn connect(handle: &Handle) -> Result<(), String> {
        let connected = unsafe { ConnectNamedPipe(handle.0, ptr::null_mut()) } != 0
            || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        if connected {
            Ok(())
        } else {
            Err(format!(
                "a capture helper could not connect (0x{:08X})",
                unsafe { GetLastError() }
            ))
        }
    }

    /// One introduction line, exactly as `main.cpp` writes it.
    fn read_hello(handle: &Handle) -> Option<String> {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let read = read_some(handle, &mut byte).ok()?;
            if read == 0 {
                return None;
            }
            if byte[0] == b'\n' {
                break;
            }
            if line.len() >= 256 {
                return None;
            }
            line.push(byte[0]);
        }
        String::from_utf8(line).ok()
    }

    fn introduce(handle: &Handle, token: &str, want: &str) -> Result<(), String> {
        let hello = read_hello(handle)
            .ok_or_else(|| String::from("a capture helper did not introduce itself"))?;
        let mut parts = hello.split(' ');
        let word = parts.next().unwrap_or_default();
        let seen = parts.next().unwrap_or_default();
        let role = parts.next().unwrap_or_default();
        let pid = parts.next().unwrap_or_default();
        if word != "FLUIDEQ-CAPTURE"
            || seen != token
            || role != want
            || pid.parse::<u32>().is_err()
        {
            return Err(String::from(
                "a connection to the capture pipe was not the helper",
            ));
        }
        Ok(())
    }

    pub(super) fn open_capture<S>(
        mut on_frame: Box<dyn FnMut(&CaptureFrame) + Send>,
        on_end: Box<dyn FnOnce() + Send>,
        spawn: S,
    ) -> Result<CaptureSession, String>
    where
        S: FnOnce(&str, &str, &CaptureSession) -> Result<(), String>,
    {
        let random = fluideq_state::share_audio::random_bytes(32);
        let name = format!(
            "\\\\.\\pipe\\FluidEQ-LAN-{}",
            random[..16]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let token = random[16..]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let first = create_instance(&name)?;

        let inner = Arc::new(SessionInner {
            writer: Mutex::new(WriterState::default()),
            closed: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            name: name.clone(),
            rejected: AtomicBool::new(false),
        });
        let session = CaptureSession {
            inner: Arc::clone(&inner),
        };

        if let Err(problem) = spawn(&name, &token, &session) {
            session.shutdown();
            return Err(problem);
        }

        std::thread::Builder::new()
            .name(String::from("fluideq-capture-pipe"))
            .spawn(move || {
                let _ = serve(&name, &token, first, &inner, &mut on_frame);
                inner.closed.store(true, Ordering::Release);
                if let Ok(mut writer) = inner.writer.lock() {
                    writer.handle = None;
                    writer.pending.clear();
                }
                on_end();
            })
            .map_err(|error| format!("the capture pipe could not start: {error}"))?;

        Ok(session)
    }

    fn serve(
        name: &str,
        token: &str,
        first: Handle,
        inner: &Arc<SessionInner>,
        on_frame: &mut Box<dyn FnMut(&CaptureFrame) + Send>,
    ) -> Result<(), String> {
        connect(&first)?;
        if inner.rejected.load(Ordering::Acquire) {
            return Err(String::from("the capture helper was refused"));
        }
        introduce(&first, token, "commands")?;

        let second = create_instance(name)?;
        connect(&second)?;
        introduce(&second, token, "frames")?;

        {
            let mut writer = inner
                .writer
                .lock()
                .map_err(|_| String::from("the capture pipe is not reachable"))?;
            let mut command = CommandWriter { handle: first };
            let lines = std::mem::take(&mut writer.pending);
            for line in &lines {
                command.write_line(line)?;
            }
            writer.handle = Some(command);
        }
        inner.connected.store(true, Ordering::Release);

        let mut header = [0u8; HEADER_BYTES];
        let mut payload = Vec::new();
        while !inner.closed.load(Ordering::Acquire) {
            if read_exact(&second, &mut header).is_err() {
                break;
            }
            let Some(header) = CaptureHeader::parse(&header) else {
                break;
            };
            if !header.payload_allowed() {
                break;
            }
            payload.clear();
            if header.payload_bytes > 0 {
                payload.resize(header.payload_bytes as usize, 0);
                if read_exact(&second, &mut payload).is_err() {
                    break;
                }
            }
            let frame = CaptureFrame {
                header,
                payload: payload.clone(),
            };
            on_frame(&frame);
        }
        Ok(())
    }

    /// The rescue for a capture the playback helper never started: a bad
    /// introduction unblocks the accept without a timer.
    pub(super) fn reject(name: &str) {
        if let Ok(stream) = open_client(name) {
            let hello = format!("FLUIDEQ-CAPTURE {} commands 0\n", "0".repeat(64));
            let _ = write_all(&stream, hello.as_bytes());
        }
    }

    fn open_client(name: &str) -> Result<Handle, String> {
        let wide_name = wide(name);
        let handle = unsafe {
            CreateFileW(
                wide_name.as_ptr(),
                0x8000_0000 | 0x4000_0000,
                0,
                ptr::null(),
                3,
                0,
                ptr::null_mut(),
            )
        };
        if handle as isize == INVALID_HANDLE_VALUE {
            return Err(format!(
                "the capture pipe could not be opened (0x{:08X})",
                unsafe { GetLastError() }
            ));
        }
        Ok(Handle(handle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(kind: u32, sequence: u32, rate: u32, channels: u16, frames: u16, bytes: u32) -> Vec<u8> {
        let mut out = vec![0u8; HEADER_BYTES];
        out[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        out[4..8].copy_from_slice(&kind.to_le_bytes());
        out[8..12].copy_from_slice(&sequence.to_le_bytes());
        out[12..16].copy_from_slice(&rate.to_le_bytes());
        out[16..18].copy_from_slice(&channels.to_le_bytes());
        out[18..20].copy_from_slice(&frames.to_le_bytes());
        out[20..24].copy_from_slice(&bytes.to_le_bytes());
        out
    }

    #[test]
    fn the_capture_header_is_read_field_for_field() {
        let parsed = CaptureHeader::parse(&header(KIND_AUDIO, 9, 48_000, 2, 480, 3_840)).unwrap();
        assert_eq!(parsed.kind, KIND_AUDIO);
        assert_eq!(parsed.sequence, 9);
        assert_eq!(parsed.sample_rate, 48_000);
        assert_eq!(parsed.channels, 2);
        assert_eq!(parsed.frames, 480);
        assert_eq!(parsed.payload_bytes, 3_840);
        assert!(parsed.payload_allowed());
    }

    #[test]
    fn a_frame_from_somewhere_else_is_not_read() {
        let mut bytes = header(KIND_AUDIO, 1, 48_000, 2, 480, 3_840);
        bytes[0] = 0;
        assert!(CaptureHeader::parse(&bytes).is_none());
        assert!(CaptureHeader::parse(&bytes[..12]).is_none());
    }

    #[test]
    fn an_oversized_payload_is_refused_before_it_is_allocated() {
        let parsed = CaptureHeader::parse(&header(KIND_AUDIO, 1, 48_000, 2, 480, 1_000_000)).unwrap();
        assert!(!parsed.payload_allowed());
    }
}
