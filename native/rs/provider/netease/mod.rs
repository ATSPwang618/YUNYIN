//! NetEase Cloud Music provider (Phase 3, task book §44–§52).
//!
//! Phase 0 status: structure and duties documented, `resolve()` reports
//! `Unsupported` so nothing pretends to work before the network layer exists.
//!
//! Duties (and only these):
//!   1. take a song id + quality, and ask the API for a playable URL;
//!   2. attach the session cookie, referer and a desktop user agent (§48);
//!   3. report `AudioInfo` — URL, size, bitrate, format hint, expiry;
//!   4. cache the URL *in memory* and re-resolve when it expires (§50/§51/§52).
//!
//! It must never: open sockets itself, decode anything, or write the cookie to
//! disk without an explicit opt-in.
#![allow(dead_code)]

pub mod account;
pub mod api;
pub mod crypto;
pub mod resolve;

use super::{AudioInfo, MusicProvider, ProviderError, Quality};
use alloc::string::String;

pub struct NetEaseProvider {
    session: account::Session,
}

impl Default for NetEaseProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl NetEaseProvider {
    pub fn new() -> Self {
        Self {
            session: account::Session::anonymous(),
        }
    }

    pub fn session(&self) -> &account::Session {
        &self.session
    }
}

impl MusicProvider for NetEaseProvider {
    fn name(&self) -> &'static str {
        "netease"
    }

    fn resolve(&self, _song_id: &str, _quality: Quality) -> Result<AudioInfo, ProviderError> {
        /* Phase 3: api::song_url() -> resolve::to_audio_info() -> URL cache. */
        Err(ProviderError::Unsupported)
    }
}

/// Referer the CDN expects; kept next to the provider because it is a provider
/// fact, not a transport fact.
pub const REFERER: &str = "https://music.163.com/";
pub const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
     Chrome/120.0.0.0 Safari/537.36";

/// Quality ids as the API names them (§47).  The provider maps our `Quality`
/// onto these; the rest of the player never sees the numbers.
pub fn quality_id(q: Quality) -> &'static str {
    match q {
        Quality::Auto | Quality::High => "exhigh",
        Quality::Low => "standard",
        Quality::Medium => "higher",
        Quality::Lossless => "lossless",
    }
}

pub fn session_cookie(session: &account::Session) -> Option<String> {
    session.cookie_header()
}
