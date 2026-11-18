/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! The local library's reading half: which files are music, what their tags
//! say, how long they are and what cover they carry.
//!
//! No decoder and no crate: an ID3v2 tag, a FLAC metadata chain and a WAV
//! header are small, self-describing formats, and reading only those parts of
//! a file is what keeps a scan of a whole music folder cheap. The container is
//! decided by content, like the native chain does, so a renamed FLAC still
//! reads as one instead of falling back to its extension.
//!
//! Playing the tracks is not this module's job; the deck commands take a path.

use serde::Serialize;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const MAX_TAG_BYTES: u64 = 8 * 1024 * 1024;
const MAX_FLAC_BLOCK_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_COVER_BYTES: usize = 2 * 1024 * 1024;
const MAX_DEPTH: usize = 12;
pub const MAX_TRACKS: usize = 50_000;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub path: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    #[serde(default)]
    pub genre: Option<String>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub duration_ms: Option<u64>,
    /// `FLAC`, `MP3`, `WAV`, or the family the content belongs to.
    pub format: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverArt {
    pub mime: String,
    /// Bare base64; the page adds the `data:` prefix.
    pub data: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScanReport {
    pub tracks: Vec<Track>,
    pub skipped: usize,
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Tags {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    genre: Option<String>,
    track: Option<u32>,
    disc: Option<u32>,
    duration_ms: Option<u64>,
}

/// The extensions a music folder scan will open. Content still decides how a
/// file is read; this only keeps the scan from touching everything else.
pub fn is_audio_path(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
        return false;
    };
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "mp3"
            | "flac"
            | "wav"
            | "aif"
            | "aiff"
            | "aifc"
            | "m4a"
            | "mp4"
            | "aac"
            | "ogg"
            | "oga"
            | "opus"
            | "wma"
    )
}

fn skip_directory(name: &str) -> bool {
    name.starts_with('.')
        || name.starts_with('$')
        || name.eq_ignore_ascii_case("System Volume Information")
}

/// Walks every root, reads each track's tags, and reports progress as tracks
/// are added so a slow folder still shows movement in the page.
pub fn scan_folders<F>(roots: &[PathBuf], mut progress: F) -> ScanReport
where
    F: FnMut(usize),
{
    let mut report = ScanReport::default();
    'roots: for root in roots {
        if !root.is_dir() {
            report.skipped += 1;
            continue;
        }
        let mut stack = vec![(root.clone(), 0usize)];
        while let Some((directory, depth)) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                report.skipped += 1;
                continue;
            };
            let mut children = Vec::new();
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if metadata.is_dir() {
                    let name = path
                        .file_name()
                        .map(|name| name.to_string_lossy())
                        .unwrap_or_default();
                    if depth < MAX_DEPTH && !skip_directory(&name) {
                        children.push(path);
                    }
                } else if metadata.is_file() && is_audio_path(&path) {
                    if report.tracks.len() >= MAX_TRACKS {
                        report.problem = Some(format!(
                            "Stopped after {MAX_TRACKS} tracks; narrow the folders to see the rest."
                        ));
                        break 'roots;
                    }
                    match read_track(&path) {
                        Some(track) => {
                            report.tracks.push(track);
                            progress(report.tracks.len());
                        }
                        None => report.skipped += 1,
                    }
                }
            }
            stack.extend(children.into_iter().map(|path| (path, depth + 1)));
        }
    }
    report.tracks.sort_by_cached_key(sort_key);
    report
}

fn sort_key(track: &Track) -> (String, String, u32, u32, String, String) {
    (
        track.artist.as_deref().unwrap_or_default().to_lowercase(),
        track.album.as_deref().unwrap_or_default().to_lowercase(),
        track.disc.unwrap_or(0),
        track.track.unwrap_or(0),
        track.title.to_lowercase(),
        track.path.to_lowercase(),
    )
}

/// What the page may read: only a path the last scan put in the list.
pub fn read_cover(path: &Path) -> Option<CoverArt> {
    let mut file = File::open(path).ok()?;
    let mut head = [0u8; 12];
    let read = read_up_to(&mut file, &mut head);
    let container = crate::compressed_audio::sniff_container(&head[..read])?;
    let _ = file.seek(SeekFrom::Start(0));
    let found = match container {
        crate::compressed_audio::AudioContainer::Flac => read_flac(&mut file, true).1,
        _ => read_id3(&mut file, true).1,
    }?;
    let (mime, bytes) = found;
    if bytes.is_empty() || bytes.len() > MAX_COVER_BYTES {
        return None;
    }
    let mime = image_mime(&mime, &bytes)?;
    Some(CoverArt {
        mime,
        data: base64(&bytes),
    })
}

/// Reads one file's header and tags. `None` when the file cannot be opened or
/// has nothing that can be read at all.
pub fn read_track(path: &Path) -> Option<Track> {
    let mut file = File::open(path).ok()?;
    let mut head = [0u8; 12];
    let read = read_up_to(&mut file, &mut head);
    let container = crate::compressed_audio::sniff_container(&head[..read])?;
    let (tags, format) = match container {
        crate::compressed_audio::AudioContainer::Flac => {
            let _ = file.seek(SeekFrom::Start(0));
            (read_flac(&mut file, false).0, "FLAC")
        }
        crate::compressed_audio::AudioContainer::Wav => {
            let _ = file.seek(SeekFrom::Start(0));
            (
                Tags {
                    duration_ms: wav_duration_ms(&mut file),
                    ..Tags::default()
                },
                "WAV",
            )
        }
        crate::compressed_audio::AudioContainer::Aiff => (Tags::default(), "AIFF"),
        crate::compressed_audio::AudioContainer::OggVorbis => (Tags::default(), "Ogg"),
        crate::compressed_audio::AudioContainer::Mp4 => (Tags::default(), "M4A"),
        crate::compressed_audio::AudioContainer::Asf => (Tags::default(), "WMA"),
        crate::compressed_audio::AudioContainer::Mp3 => {
            let _ = file.seek(SeekFrom::Start(0));
            (read_id3(&mut file, false).0, "MP3")
        }
    };
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().trim().to_string())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| String::from("Untitled"));
    Some(Track {
        path: path.to_string_lossy().to_string(),
        title: tags.title.unwrap_or(stem),
        artist: tags.artist,
        album: tags.album,
        genre: tags.genre,
        track: tags.track,
        disc: tags.disc,
        duration_ms: tags.duration_ms,
        format: String::from(format),
    })
}

fn read_up_to(reader: &mut impl Read, buffer: &mut [u8]) -> usize {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) | Err(_) => break,
            Ok(read) => filled += read,
        }
    }
    filled
}

fn read_exact_at(reader: &mut impl Read, buffer: &mut [u8]) -> bool {
    read_up_to(reader, buffer) == buffer.len()
}

fn syncsafe(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .take(4)
        .fold(0u64, |value, byte| (value << 7) | u64::from(byte & 0x7f))
}

fn big_endian(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0u64, |value, byte| (value << 8) | u64::from(*byte))
}

fn deunsync(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        out.push(bytes[index]);
        if bytes[index] == 0xff && bytes.get(index + 1) == Some(&0) {
            index += 2;
        } else {
            index += 1;
        }
    }
    out
}

fn first_text(value: &str) -> Option<String> {
    let value = value.split('\0').find(|part| !part.trim().is_empty())?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Old taggers write `(17)Rock`, `(17)`, or bare `17`. Keep the name when the
/// frame carries one; a table index alone is not a genre to browse by.
fn genre_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let named = match trimmed
        .strip_prefix('(')
        .and_then(|rest| rest.split_once(')'))
    {
        Some((index, tail)) if index.chars().all(|c| c.is_ascii_digit()) => {
            let tail = tail.trim();
            if tail.is_empty() {
                return None;
            }
            tail
        }
        _ => trimmed,
    };
    (!named.is_empty() && !named.chars().all(|c| c.is_ascii_digit())).then(|| named.to_string())
}

fn decode_text(encoding: u8, bytes: &[u8]) -> Option<String> {
    match encoding {
        0 => {
            let text: String = bytes.iter().map(|byte| char::from(*byte)).collect();
            first_text(&text)
        }
        3 => first_text(&String::from_utf8_lossy(bytes)),
        1 | 2 => {
            let (big_endian, body) = if encoding == 2 {
                (true, bytes)
            } else if bytes.starts_with(&[0xff, 0xfe]) {
                (false, &bytes[2..])
            } else if bytes.starts_with(&[0xfe, 0xff]) {
                (true, &bytes[2..])
            } else {
                (false, bytes)
            };
            let units: Vec<u16> = body
                .chunks_exact(2)
                .map(|pair| {
                    if big_endian {
                        u16::from_be_bytes([pair[0], pair[1]])
                    } else {
                        u16::from_le_bytes([pair[0], pair[1]])
                    }
                })
                .take_while(|unit| *unit != 0)
                .collect();
            first_text(&String::from_utf16_lossy(&units))
        }
        _ => first_text(&String::from_utf8_lossy(bytes)),
    }
}

fn number(text: Option<String>) -> Option<u32> {
    let text = text?;
    text.split('/')
        .next()?
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
}

fn apply_text_frame(tags: &mut Tags, id: &[u8], body: &[u8]) {
    let Some(&encoding) = body.first() else {
        return;
    };
    let Some(text) = decode_text(encoding, &body[1..]) else {
        return;
    };
    match id {
        b"TIT2" | b"TT2" => tags.title = Some(text),
        b"TPE1" | b"TP1" => tags.artist = Some(text),
        b"TALB" | b"TAL" => tags.album = Some(text),
        b"TCON" | b"TCO" => tags.genre = genre_text(&text),
        b"TRCK" | b"TRK" => tags.track = number(Some(text)),
        b"TPOS" | b"TPA" => tags.disc = number(Some(text)),
        b"TLEN" | b"TLE" => {
            if let Ok(milliseconds) = text.trim().parse::<u64>() {
                if milliseconds > 0 {
                    tags.duration_ms = Some(milliseconds);
                }
            }
        }
        _ => {}
    }
}

fn frame_data(id: &[u8]) -> bool {
    (id.len() == 4 || id.len() == 3) && id.iter().all(|byte| byte.is_ascii_alphanumeric())
}

fn read_id3(file: &mut File, want_cover: bool) -> (Tags, Option<(String, Vec<u8>)>) {
    let mut tags = Tags::default();
    let mut header = [0u8; 10];
    if !read_exact_at(file, &mut header) || &header[..3] != b"ID3" {
        return (tags, None);
    }
    let version = header[3];
    let flags = header[5];
    let size = syncsafe(&header[6..10]).min(MAX_TAG_BYTES);
    let mut body = vec![0u8; size as usize];
    if !read_exact_at(file, &mut body) {
        return (tags, None);
    }
    if version < 4 && flags & 0x80 != 0 {
        body = deunsync(&body);
    }
    let cover = walk_id3_frames(version, &body, &mut tags, want_cover);
    (tags, cover)
}

fn walk_id3_frames(
    version: u8,
    body: &[u8],
    tags: &mut Tags,
    want_cover: bool,
) -> Option<(String, Vec<u8>)> {
    let mut at = 0usize;
    let no_flags = [0u8; 2];
    while at < body.len() {
        let (id, header, flags) = if version >= 3 {
            (body.get(at..at + 4)?, 10usize, body.get(at + 8..at + 10)?)
        } else {
            (body.get(at..at + 3)?, 6usize, &no_flags[..])
        };
        if !frame_data(id) {
            break;
        }
        let size = if version >= 4 {
            syncsafe(body.get(at + 4..at + 8)?)
        } else if version == 3 {
            big_endian(body.get(at + 4..at + 8)?)
        } else {
            big_endian(body.get(at + 3..at + 6)?)
        };
        if size == 0 || size > MAX_TAG_BYTES {
            break;
        }
        let start = at + header;
        let end = start.checked_add(size as usize)?;
        if end > body.len() {
            break;
        }
        let mut frame: &[u8] = &body[start..end];
        let deunsynced = if version >= 4 && flags[1] & 0x02 != 0 {
            Some(deunsync(frame))
        } else {
            None
        };
        if let Some(bytes) = &deunsynced {
            frame = bytes;
        }
        if version >= 4 && flags[1] & 0x01 != 0 && frame.len() >= 4 {
            frame = &frame[4..];
        }
        if id == b"APIC" || id == b"PIC" {
            if want_cover {
                if let Some(cover) = id3_picture(version, frame) {
                    return Some(cover);
                }
            }
        } else {
            apply_text_frame(tags, id, frame);
        }
        at = end;
    }
    None
}

fn id3_picture(version: u8, body: &[u8]) -> Option<(String, Vec<u8>)> {
    if version >= 3 {
        let (&encoding, rest) = body.split_first()?;
        let mime_end = rest.iter().position(|byte| *byte == 0)?;
        let mime = String::from_utf8_lossy(&rest[..mime_end]).to_string();
        let rest = rest.get(mime_end + 1..)?;
        let rest = rest.get(1..)?;
        let data = skip_encoded_terminated(encoding, rest);
        Some((mime, data.to_vec()))
    } else {
        let body = body.get(3..)?;
        let rest = body.get(1..)?;
        let data = skip_encoded_terminated(0, rest);
        Some((String::new(), data.to_vec()))
    }
}

fn skip_encoded_terminated(encoding: u8, bytes: &[u8]) -> &[u8] {
    if encoding == 1 || encoding == 2 {
        let mut at = 0;
        while at + 1 < bytes.len() {
            if bytes[at] == 0 && bytes[at + 1] == 0 {
                return &bytes[at + 2..];
            }
            at += 2;
        }
        &[]
    } else {
        match bytes.iter().position(|byte| *byte == 0) {
            Some(at) => &bytes[at + 1..],
            None => &[],
        }
    }
}

fn image_mime(declared: &str, bytes: &[u8]) -> Option<String> {
    let declared = declared.trim().to_ascii_lowercase();
    if declared.starts_with("image/") {
        return Some(declared);
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some(String::from("image/jpeg"));
    }
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Some(String::from("image/png"));
    }
    if bytes.starts_with(b"GIF8") {
        return Some(String::from("image/gif"));
    }
    None
}

fn read_flac(file: &mut File, want_cover: bool) -> (Tags, Option<(String, Vec<u8>)>) {
    let mut tags = Tags::default();
    let mut cover = None;
    let mut magic = [0u8; 4];
    if !read_exact_at(file, &mut magic) || &magic != b"fLaC" {
        return (tags, cover);
    }
    loop {
        let mut header = [0u8; 4];
        if !read_exact_at(file, &mut header) {
            break;
        }
        let last = header[0] & 0x80 != 0;
        let kind = header[0] & 0x7f;
        let length = big_endian(&header[1..4]);
        if length > MAX_FLAC_BLOCK_BYTES {
            break;
        }
        let mut block = vec![0u8; length as usize];
        if !read_exact_at(file, &mut block) {
            break;
        }
        match kind {
            0 => flac_stream_info(&block, &mut tags),
            4 => flac_vorbis_comment(&block, &mut tags),
            6 if want_cover => {
                cover = flac_picture(&block);
            }
            _ => {}
        }
        if last {
            break;
        }
    }
    (tags, cover)
}

fn flac_stream_info(block: &[u8], tags: &mut Tags) {
    if block.len() < 18 {
        return;
    }
    let rate =
        (u64::from(block[10]) << 12) | (u64::from(block[11]) << 4) | (u64::from(block[12]) >> 4);
    let total = (u64::from(block[13] & 0x0f) << 32) | big_endian(&block[14..18]);
    if rate > 0 && total > 0 {
        tags.duration_ms = Some(total * 1000 / rate);
    }
}

fn flac_vorbis_comment(block: &[u8], tags: &mut Tags) {
    let mut at = 0usize;
    let Some(vendor) = read_u32_le(block, &mut at) else {
        return;
    };
    if block.len() < at + vendor as usize + 4 {
        return;
    }
    at += vendor as usize;
    let Some(count) = read_u32_le(block, &mut at) else {
        return;
    };
    for _ in 0..count {
        let Some(length) = read_u32_le(block, &mut at) else {
            break;
        };
        let end = at.saturating_add(length as usize);
        let Some(entry) = block.get(at..end) else {
            break;
        };
        at = end;
        let entry = String::from_utf8_lossy(entry);
        let Some((key, value)) = entry.split_once('=') else {
            continue;
        };
        match key.to_ascii_uppercase().as_str() {
            "TITLE" => tags.title = first_text(value),
            "ARTIST" | "ALBUMARTIST" => {
                if tags.artist.is_none() {
                    tags.artist = first_text(value);
                }
            }
            "ALBUM" => tags.album = first_text(value),
            "GENRE" => tags.genre = first_text(value),
            "TRACKNUMBER" => tags.track = number(Some(value.to_string())),
            "DISCNUMBER" => tags.disc = number(Some(value.to_string())),
            _ => {}
        }
    }
}

fn read_u32_le(bytes: &[u8], at: &mut usize) -> Option<u32> {
    let slice = bytes.get(*at..*at + 4)?;
    *at += 4;
    Some(u32::from_le_bytes(slice.try_into().ok()?))
}

fn flac_picture(block: &[u8]) -> Option<(String, Vec<u8>)> {
    let mut at = 4usize;
    let mime_length = read_u32_le(block, &mut at)? as usize;
    let mime = String::from_utf8_lossy(block.get(at..at + mime_length)?).to_string();
    at += mime_length;
    let description = read_u32_le(block, &mut at)? as usize;
    at = at.checked_add(description)?;
    // Width, height, depth and colours, then the picture itself.
    at = at.checked_add(16)?;
    let data_length = read_u32_le(block, &mut at)? as usize;
    let data = block.get(at..at + data_length)?.to_vec();
    Some((mime, data))
}

fn wav_duration_ms(file: &mut File) -> Option<u64> {
    let mut riff = [0u8; 12];
    if !read_exact_at(file, &mut riff) || &riff[..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return None;
    }
    let mut byte_rate = 0u64;
    loop {
        let mut header = [0u8; 8];
        if !read_exact_at(file, &mut header) {
            return None;
        }
        let size = u64::from(u32::from_le_bytes(header[4..8].try_into().ok()?));
        match &header[..4] {
            b"fmt " => {
                let mut format = [0u8; 16];
                if size < 16 || !read_exact_at(file, &mut format) {
                    return None;
                }
                byte_rate = u64::from(u32::from_le_bytes(format[8..12].try_into().ok()?));
                let skip = size - 16 + (size & 1);
                if file.seek(SeekFrom::Current(skip as i64)).is_err() {
                    return None;
                }
            }
            b"data" => {
                if byte_rate == 0 {
                    return None;
                }
                return Some(size * 1000 / byte_rate);
            }
            _ => {
                let skip = size + (size & 1);
                if file.seek(SeekFrom::Current(skip as i64)).is_err() {
                    return None;
                }
            }
        }
    }
}

/// Standard base64, written here so the page needs no extra crate to show a
/// cover that is already on disk.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        out.push(ALPHABET[(first >> 2) as usize] as char);
        out.push(ALPHABET[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(third & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn text_frame(encoding: u8, text: &[u8]) -> Vec<u8> {
        let mut frame = vec![encoding];
        frame.extend_from_slice(text);
        frame
    }

    fn id3_file(frames: &[(&[u8], Vec<u8>)], version: u8) -> Vec<u8> {
        let mut body = Vec::new();
        for (id, frame) in frames {
            body.extend_from_slice(id);
            if version >= 4 {
                body.extend_from_slice(&syncsafe_u32(frame.len() as u32));
            } else {
                body.extend_from_slice(&(frame.len() as u32).to_be_bytes());
            }
            body.extend_from_slice(&[0, 0]);
            body.extend_from_slice(frame);
        }
        let mut tag = Vec::from(*b"ID3");
        tag.push(version);
        tag.push(0);
        tag.push(0);
        tag.extend_from_slice(&syncsafe_u32(body.len() as u32));
        tag.extend_from_slice(&body);
        tag
    }

    fn syncsafe_u32(value: u32) -> [u8; 4] {
        [
            ((value >> 21) & 0x7f) as u8,
            ((value >> 14) & 0x7f) as u8,
            ((value >> 7) & 0x7f) as u8,
            (value & 0x7f) as u8,
        ]
    }

    fn temp_path(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("fluideq-library-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn base64_matches_the_standard_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn a_genre_frame_keeps_its_name_and_drops_a_bare_table_index() {
        assert_eq!(genre_text("Rock").as_deref(), Some("Rock"));
        assert_eq!(genre_text("  (17)Rock ").as_deref(), Some("Rock"));
        assert_eq!(genre_text("(17)"), None);
        assert_eq!(genre_text("17"), None);
        assert_eq!(genre_text("   "), None);
    }

    #[test]
    fn id3v23_text_frames_become_tags_and_a_title_falls_back_to_the_stem() {
        let directory = temp_path("id3v23");
        let path = directory.join("track.mp3");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(&id3_file(
            &[
                (b"TIT2", text_frame(0, b"A latin title")),
                (b"TPE1", text_frame(3, "Björk".as_bytes())),
                (b"TALB", text_frame(3, b"Homogenic")),
                (b"TCON", text_frame(0, b"(17)Rock")),
                (b"TRCK", text_frame(0, b"3/9")),
                (b"TLEN", text_frame(0, b"321000")),
            ],
            3,
        ))
        .unwrap();
        file.write_all(&[0xff, 0xfb, 0x90, 0x64]).unwrap();
        drop(file);

        let track = read_track(&path).expect("the file reads");
        assert_eq!(track.title, "A latin title");
        assert_eq!(track.artist.as_deref(), Some("Björk"));
        assert_eq!(track.album.as_deref(), Some("Homogenic"));
        assert_eq!(track.genre.as_deref(), Some("Rock"));
        assert_eq!(track.track, Some(3));
        assert_eq!(track.duration_ms, Some(321_000));
        assert_eq!(track.format, "MP3");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn id3v24_utf16_and_syncsafe_sizes_are_read() {
        let directory = temp_path("id3v24");
        let path = directory.join("utf16.mp3");
        let mut utf16 = vec![0xff, 0xfe];
        for unit in "Café".encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(&id3_file(&[(b"TIT2", text_frame(1, &utf16))], 4))
            .unwrap();
        drop(file);
        assert_eq!(read_track(&path).unwrap().title, "Café");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn an_id3_cover_is_found_and_its_mime_falls_back_to_the_magic() {
        let directory = temp_path("cover");
        let path = directory.join("cover.mp3");
        let jpeg = [0xff, 0xd8, 0xff, 0xe0, 1, 2, 3, 4];
        let mut picture = vec![0u8];
        picture.extend_from_slice(b"\0");
        picture.push(3);
        picture.extend_from_slice(b"\0");
        picture.extend_from_slice(&jpeg);
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(&id3_file(&[(b"APIC", picture)], 3)).unwrap();
        drop(file);
        let cover = read_cover(&path).expect("the picture reads");
        assert_eq!(cover.mime, "image/jpeg");
        assert_eq!(cover.data, base64(&jpeg));
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn flac_streaminfo_names_the_duration_and_the_comments_name_the_track() {
        let directory = temp_path("flac");
        let path = directory.join("song.flac");
        let mut streaminfo = vec![0u8; 34];
        // 44.1 kHz in the 20 bits at 10, 17; 44_100 samples at 14.
        streaminfo[10] = 0x0a;
        streaminfo[11] = 0xc4;
        streaminfo[12] = 0x42;
        streaminfo[13] = 0xf0;
        streaminfo[14..18].copy_from_slice(&44_100u32.to_be_bytes());

        let mut comments = Vec::new();
        comments.extend_from_slice(&2u32.to_le_bytes());
        comments.extend_from_slice(b"eq");
        comments.extend_from_slice(&4u32.to_le_bytes());
        comments.extend_from_slice(&9u32.to_le_bytes());
        comments.extend_from_slice(b"TITLE=One");
        comments.extend_from_slice(&10u32.to_le_bytes());
        comments.extend_from_slice(b"ARTIST=Two");
        comments.extend_from_slice(&10u32.to_le_bytes());
        comments.extend_from_slice(b"GENRE=Rock");
        comments.extend_from_slice(&13u32.to_le_bytes());
        comments.extend_from_slice(b"TRACKNUMBER=4");

        let mut flac = Vec::from(*b"fLaC");
        flac.push(0);
        flac.extend_from_slice(&(34u32).to_be_bytes()[1..]);
        flac.extend_from_slice(&streaminfo);
        flac.push(0x80 | 4);
        flac.extend_from_slice(&(comments.len() as u32).to_be_bytes()[1..]);
        flac.extend_from_slice(&comments);
        std::fs::write(&path, flac).unwrap();

        let track = read_track(&path).unwrap();
        assert_eq!(track.format, "FLAC");
        assert_eq!(track.title, "One");
        assert_eq!(track.artist.as_deref(), Some("Two"));
        assert_eq!(track.genre.as_deref(), Some("Rock"));
        assert_eq!(track.track, Some(4));
        assert_eq!(track.duration_ms, Some(1000));
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn wav_duration_comes_from_the_header_not_the_file_length() {
        let directory = temp_path("wav");
        let path = directory.join("tone.wav");
        let mut wav = Vec::from(*b"RIFF");
        wav.extend_from_slice(&0u32.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&48_000u32.to_le_bytes());
        wav.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&32u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
        std::fs::write(&path, wav).unwrap();
        let track = read_track(&path).unwrap();
        assert_eq!(track.title, "tone");
        assert_eq!(track.duration_ms, Some(1000));
        assert_eq!(track.format, "WAV");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_scan_walks_folders_sorts_tracks_and_ignores_everything_else() {
        let directory = temp_path("scan");
        let album = directory.join("Album");
        std::fs::create_dir_all(&album).unwrap();
        let mut file = std::fs::File::create(album.join("second.mp3")).unwrap();
        file.write_all(&id3_file(
            &[
                (b"TPE1", text_frame(3, b"Artist")),
                (b"TALB", text_frame(3, b"Album")),
                (b"TIT2", text_frame(3, b"Second")),
                (b"TRCK", text_frame(0, b"2")),
            ],
            3,
        ))
        .unwrap();
        drop(file);
        let mut file = std::fs::File::create(directory.join("first.mp3")).unwrap();
        file.write_all(&id3_file(&[(b"TIT2", text_frame(3, b"First"))], 3))
            .unwrap();
        drop(file);
        std::fs::write(directory.join("notes.txt"), b"not music").unwrap();

        let report = scan_folders(&[directory.clone()], |_| {});
        assert_eq!(report.tracks.len(), 2);
        assert_eq!(report.tracks[0].title, "First");
        assert_eq!(report.tracks[1].title, "Second");
        assert_eq!(report.skipped, 0);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn only_music_extensions_are_offered_to_the_scan() {
        assert!(is_audio_path(Path::new("a.FLAC")));
        assert!(is_audio_path(Path::new("b.mp3")));
        assert!(!is_audio_path(Path::new("c.txt")));
        assert!(!is_audio_path(Path::new("no-extension")));
    }
}
