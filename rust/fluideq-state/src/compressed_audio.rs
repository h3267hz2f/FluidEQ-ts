/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Which decoder the native chain would reach for, decided by content.
//!
//! Exact port of the order in `feq_decoder_ops` (`pcm_decoder.cpp`): WAV and
//! AIFF by their container magic, then FLAC and Ogg by theirs, then the
//! platform decoder's MPEG-4 family, and finally MP3, which has no magic
//! number at a fixed offset and is offered to the frame scanner regardless.
//! Deciding by extension is what turns a renamed FLAC into silence.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioContainer {
    Wav,
    Aiff,
    Flac,
    OggVorbis,
    /// ISO base media (`ftyp`): MP4/M4A, where AAC lives.
    Mp4,
    /// ASF (`30 26 B2 75`), which carries WMA.
    Asf,
    /// The fallback: the MP3 frame scanner decides whether it really is one.
    Mp3,
}

/// Classifies the first bytes of a file. `None` means fewer than four bytes:
/// no decoder in the native chain can say anything about it.
pub fn sniff_container(header: &[u8]) -> Option<AudioContainer> {
    if header.len() < 4 {
        return None;
    }
    if header.starts_with(b"RIFF")
        && header.len() >= 12
        && &header[8..12] == b"WAVE"
    {
        return Some(AudioContainer::Wav);
    }
    if header.starts_with(b"FORM")
        && header.len() >= 12
        && (&header[8..12] == b"AIFF" || &header[8..12] == b"AIFC")
    {
        return Some(AudioContainer::Aiff);
    }
    if header.starts_with(b"fLaC") {
        return Some(AudioContainer::Flac);
    }
    if header.starts_with(b"OggS") {
        return Some(AudioContainer::OggVorbis);
    }
    if header.len() >= 12 && &header[4..8] == b"ftyp" {
        return Some(AudioContainer::Mp4);
    }
    if header.starts_with(&[0x30, 0x26, 0xB2, 0x75]) {
        return Some(AudioContainer::Asf);
    }
    Some(AudioContainer::Mp3)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sniff(header: &[u8]) -> Option<AudioContainer> {
        sniff_container(header)
    }

    #[test]
    fn containers_are_recognised_by_content_in_the_native_order() {
        let mut riff = Vec::from(*b"RIFF\0\0\0\0WAVE");
        riff.extend_from_slice(b"fmt ");
        assert_eq!(sniff(&riff), Some(AudioContainer::Wav));

        let mut aiff = Vec::from(*b"FORM\0\0\0\0AIFF");
        aiff.extend_from_slice(b"COMM");
        assert_eq!(sniff(&aiff), Some(AudioContainer::Aiff));
        let mut aifc = Vec::from(*b"FORM\0\0\0\0AIFC");
        aifc.extend_from_slice(b"COMM");
        assert_eq!(sniff(&aifc), Some(AudioContainer::Aiff));

        assert_eq!(sniff(b"fLaC\0\0\0\0"), Some(AudioContainer::Flac));
        assert_eq!(sniff(b"OggS\0\x02\0\0"), Some(AudioContainer::OggVorbis));

        let mut mp4 = Vec::from(*b"\0\0\0\x20ftypM4A ");
        mp4.extend_from_slice(b"\0\0\0\0");
        assert_eq!(sniff(&mp4), Some(AudioContainer::Mp4));

        assert_eq!(
            sniff(&[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11]),
            Some(AudioContainer::Asf)
        );
    }

    #[test]
    fn a_riff_that_is_not_wave_and_an_mp3_frame_both_fall_through_to_mp3() {
        // RIFF is a container family, not a promise of audio the PCM reader
        // takes; the native chain offers anything else to the MP3 scanner.
        assert_eq!(
            sniff(b"RIFF\0\0\0\0AVI \0\0\0\0"),
            Some(AudioContainer::Mp3)
        );
        assert_eq!(
            sniff(&[0xFF, 0xFB, 0x90, 0x64]),
            Some(AudioContainer::Mp3)
        );
        // An ID3 tag can push the first frame far in, so the magic alone
        // cannot rule it out either.
        assert_eq!(sniff(b"ID3\x04\0\0\0\0\0\0"), Some(AudioContainer::Mp3));
    }

    #[test]
    fn a_header_shorter_than_four_bytes_names_no_decoder() {
        assert_eq!(sniff(b""), None);
        assert_eq!(sniff(b"fLa"), None);
        // The positive control: one more byte is enough to classify.
        assert_eq!(sniff(b"fLaC"), Some(AudioContainer::Flac));
    }
}
