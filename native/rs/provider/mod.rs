//! Providers and the format question (task book §41/§42).
//!
//! A provider answers one question — "given a song id, where are the bytes and
//! what are they?" — and returns an `AudioInfo`.  It never returns a socket, and
//! the decoder never sees the provider's objects (§15).
//!
//! `AudioFormat::sniff` also lives here, because §38/§39 are explicit that the
//! format must come from the bytes, never from the URL suffix: a NetEase CDN URL
//! has no useful extension at all.
#![allow(dead_code)]

pub mod netease;

use alloc::format;
use alloc::string::String;

/// Quality request, mapped by each provider onto its own ladder (§47).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    /// Best the account is allowed to have.
    Auto,
    Low,
    Medium,
    High,
    Lossless,
}

/// Container/codec we can actually decode.
///
/// Six families today: the five library decoders plus M4A (AAC), which is
/// demuxed by `ym4a.c` and decoded by the hardware block through `yaac.c`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioFormat {
    Mp3,
    OggVorbis,
    Opus,
    Wav,
    Flac,
    M4a,
    Unknown,
}

impl AudioFormat {
    /// What we hand to `yplayer.c` so it picks the right decoder without being
    /// asked to re-sniff a network stream.
    pub fn name(&self) -> &'static str {
        match self {
            AudioFormat::Mp3 => "mp3",
            AudioFormat::OggVorbis => "ogg",
            AudioFormat::Opus => "opus",
            AudioFormat::Wav => "wav",
            AudioFormat::Flac => "flac",
            AudioFormat::M4a => "m4a",
            AudioFormat::Unknown => "unknown",
        }
    }

    /// True when the shipping player can decode it today.
    pub fn is_playable(&self) -> bool {
        !matches!(self, AudioFormat::Unknown)
    }

    /// Identify a container from its first bytes (§39).
    ///
    /// Ordered so that the containers that can be confused with each other are
    /// checked first: Ogg appears twice (Vorbis vs Opus) and MP4 hides `ftyp`
    /// behind a 4-byte size field, not at offset 0.
    pub fn sniff(head: &[u8]) -> Self {
        if head.len() >= 4 && &head[0..4] == b"fLaC" {
            return AudioFormat::Flac;
        }
        if head.len() >= 4 && &head[0..4] == b"OggS" {
            /* Ogg carries both Vorbis and Opus; the codec name sits in the first
             * page, near the start. */
            let probe = &head[..head.len().min(64)];
            if contains(probe, b"OpusHead") {
                return AudioFormat::Opus;
            }
            if contains(probe, b"vorbis") {
                return AudioFormat::OggVorbis;
            }
            return AudioFormat::OggVorbis; /* Ogg without either tag: treat as Vorbis */
        }
        if head.len() >= 12 && &head[4..8] == b"ftyp" {
            return AudioFormat::M4a;
        }
        if head.len() >= 12 && &head[8..12] == b"WAVE" {
            return AudioFormat::Wav;
        }
        if head.len() >= 3 && &head[0..3] == b"ID3" {
            return AudioFormat::Mp3;
        }
        if head.len() >= 2 && head[0] == 0xFF && (head[1] & 0xE0) == 0xE0 {
            return AudioFormat::Mp3; /* MPEG frame sync, tag-less MP3 */
        }
        AudioFormat::Unknown
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Everything the player needs to start playing a stream, and nothing else.
///
/// Extra fields (§41: `source`, `song_id`, `quality`, `expires_at`) are for the
/// UI and for re-resolving an expired URL; a decoder must not read them.
#[derive(Clone, Debug)]
pub struct AudioInfo {
    pub url: String,
    pub format: AudioFormat,
    /// Duration as claimed by the provider.  Zero when unknown — the player then
    /// learns it from the decoder (§37).
    pub duration_ms: u32,
    pub bitrate: u32,
    pub size: Option<u64>,
    pub source: String,
    pub song_id: String,
    pub quality: Quality,
    /// Absolute time (ms since epoch) after which `url` must be re-resolved.
    pub expires_at: u64,
}

impl AudioInfo {
    /// A local file never expires and has no provider behind it.
    pub fn local(path: &str, format: AudioFormat, size: Option<u64>) -> Self {
        Self {
            url: String::from(path),
            format,
            duration_ms: 0,
            bitrate: 0,
            size,
            source: String::from("local"),
            song_id: String::new(),
            quality: Quality::Auto,
            expires_at: 0,
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "{} {} {}ms bitrate={} size={:?} src={}",
            self.source,
            self.format.name(),
            self.duration_ms,
            self.bitrate,
            self.size,
            self.song_id
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderError {
    /// No provider handles this id yet.
    Unsupported,
    Network(String),
    /// Cookie/session missing or rejected (§48).
    Auth(String),
    /// The account is not allowed this quality (§47).
    VipRequired,
    NotFound,
    /// The CDN URL went stale; re-resolve and retry (§52).
    Expired,
    Cancelled,
}

/// The provider seam.  Adding QQ Music later means adding an implementation,
/// not touching the player (§43).
pub trait MusicProvider {
    fn name(&self) -> &'static str;
    fn resolve(&self, song_id: &str, quality: Quality) -> Result<AudioInfo, ProviderError>;
}
