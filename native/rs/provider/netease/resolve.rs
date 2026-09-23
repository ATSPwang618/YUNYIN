//! songId -> AudioInfo, with the URL cache and expiry policy (task book
//! §50-§52).
//!
//! The cache policy is the useful part to settle early, because it decides how
//! the player behaves when a CDN URL dies mid-album:
//!
//! ```text
//! resolve(song, quality)
//!     cached and not near expiry  -> reuse
//!     otherwise                   -> ask the API, remember the expiry
//! play fails with 403/404/410     -> mark stale, re-resolve once, retry
//! ```
//!
//! Rules: entries are keyed by `(song_id, quality)`; nothing is written to disk
//! (§51); a URL counts as stale a few minutes before its stated expiry so a long
//! queue never walks off the end of it.
#![allow(dead_code)]

use super::api;
use crate::media::provider::{AudioFormat, AudioInfo, ProviderError, Quality};
use alloc::string::String;
use alloc::vec::Vec;

/// Re-resolve this long before the URL actually expires.
pub const EXPIRY_MARGIN_MS: u64 = 3 * 60 * 1000;

#[derive(Clone, Debug)]
pub struct CachedUrl {
    pub song_id: String,
    pub quality: Quality,
    pub url: String,
    pub size: Option<u64>,
    pub bitrate: u32,
    pub expires_at_ms: u64,
}

impl CachedUrl {
    pub fn is_usable_at(&self, now_ms: u64) -> bool {
        self.expires_at_ms == 0 || now_ms + EXPIRY_MARGIN_MS < self.expires_at_ms
    }
}

/// Small in-memory map; a queue of 50 songs is far below the size where a
/// smarter structure would matter (§51).
#[derive(Default)]
pub struct UrlCache {
    entries: Vec<CachedUrl>,
}

impl UrlCache {
    pub fn get(&self, song_id: &str, quality: Quality, now_ms: u64) -> Option<&CachedUrl> {
        self.entries
            .iter()
            .find(|e| e.song_id == song_id && e.quality == quality && e.is_usable_at(now_ms))
    }

    pub fn put(&mut self, entry: CachedUrl) {
        self.entries
            .retain(|e| !(e.song_id == entry.song_id && e.quality == entry.quality));
        self.entries.push(entry);
    }

    /// Called when a transfer answered 403/404/410 (§52).
    pub fn invalidate(&mut self, song_id: &str) {
        self.entries.retain(|e| e.song_id != song_id);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Turn an API answer into what the player needs.  `format_hint` is what the API
/// claims; the decoder still sniffs the real bytes (§40).
pub fn to_audio_info(
    song_id: &str,
    quality: Quality,
    url: &str,
    size: Option<u64>,
    bitrate: u32,
    duration_ms: u32,
    expires_at_ms: u64,
    format_hint: Option<AudioFormat>,
) -> AudioInfo {
    AudioInfo {
        url: String::from(url),
        format: format_hint.unwrap_or(AudioFormat::Unknown),
        duration_ms,
        bitrate,
        size,
        source: String::from("netease"),
        song_id: String::from(song_id),
        quality,
        expires_at: expires_at_ms,
    }
}

/// Phase 3: call the API and build the entry.  Single entry point so the retry
/// policy of §52 lives in exactly one place.
pub fn resolve(
    song_id: &str,
    quality: Quality,
    _cache: &mut UrlCache,
) -> Result<AudioInfo, ProviderError> {
    let _ = api::Call::url_quality(song_id, quality, super::quality_id(quality));
    Err(ProviderError::Unsupported)
}
