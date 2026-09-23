//! NetEase API surface (task book §45–§47).
//!
//! The endpoints below are the ones the plan needs; the parameter sets and the
//! request encryption are ported from `music-lib` (§46), which is the reference
//! implementation for this provider.
//!
//! Phase 3 status: constants + shape only.  `call()` returns `Unsupported` until
//! `net::http` exists, and it must stay the single place that talks to the API —
//! no endpoint string may appear anywhere else in the tree.
#![allow(dead_code)]

use crate::media::provider::{MusicProvider, ProviderError, Quality};

pub const HOST: &str = "https://music.163.com";

/// Song detail: name/artist/album/duration — needed to show a remote track in
/// the library before its URL is resolved.
pub const PATH_SONG_DETAIL: &str = "/api/v3/song/detail";
/// Play URL: the `GetDownloadURL` equivalent (§45).
pub const PATH_SONG_URL_V1: &str = "/api/song/enhance/player/url/v1";
/// Lyric lookup, matching what the local tag reader provides for files (§43).
pub const PATH_LYRIC: &str = "/api/song/lyric";
/// Playlist contents, for the streaming library view.
pub const PATH_PLAYLIST_DETAIL: &str = "/api/v6/playlist/detail";

/// Which request flavour an endpoint needs.  NetEase accepts several; the
/// reference implementation uses the web API for these four (§46).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavour {
    /// Plain query string, no encryption.
    Plain,
    /// `weapi`: AES-128-CBC + RSA-wrapped key (§46).
    WeApi,
    /// `eapi`: AES-128-ECB with a fixed key, used by the app clients.
    EApi,
}

/// One API call, described.
#[derive(Clone, Debug)]
pub struct Call {
    pub path: &'static str,
    pub flavour: Flavour,
    /// Raw parameters; encryption happens in `crypto` just before sending.
    pub params: alloc::vec::Vec<(alloc::string::String, alloc::string::String)>,
}

impl Call {
    pub fn url_quality(song_id: &str, quality: Quality, level: &str) -> Self {
        use alloc::string::String;
        use alloc::vec;
        Self {
            path: PATH_SONG_URL_V1,
            flavour: Flavour::WeApi,
            params: vec![
                (String::from("ids"), alloc::format!("[{}]", song_id)),
                (String::from("level"), String::from(level)),
                (String::from("encodeType"), String::from("aac")),
                (String::from("_q"), String::from(quality_id(quality))),
            ],
        }
    }
}

fn quality_id(q: Quality) -> &'static str {
    super::quality_id(q)
}

/// Perform a call.  Phase 3 fills this in through `net::http`; the provider never
/// calls it directly, `resolve` does.
pub fn call(_c: &Call) -> Result<alloc::string::String, ProviderError> {
    Err(ProviderError::Unsupported)
}

/// Kept so a future provider can assert it implements the seam.
pub fn provider_name<P: MusicProvider>(p: &P) -> &'static str {
    p.name()
}
