//! songId → AudioInfo，以及 URL 缓存与过期策略（任务书 §50–§52）。
//!
//! 缓存策略值得先定下来，因为它决定"专辑放到一半 CDN 链接失效"时播放器的行为：
//!
//! ```text
//! resolve(song, quality)
//!     有缓存且未接近过期   → 直接复用
//!     否则                → 问 API，并记下过期时间
//!     播放遇到 403/404/410 → 标记失效，重新解析一次，再试
//! ```
//!
//! 规矩：条目按 `(song_id, quality)` 索引；不落盘（§51）；URL 在过期前几分钟就算
//! "该换新的"，这样长队列不会走到最后一首才发现链接死了。
#![allow(dead_code)]

use super::api;
use crate::media::provider::{AudioFormat, AudioInfo, ProviderError, Quality};
use alloc::string::String;
use alloc::vec::Vec;

/// 比"真的过期"提前这么多就重新解析。
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

/// 内存里的小表：50 首的队列离"需要更聪明的结构"还差得远（§51）。
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

    /// 传输层收到 403/404/410 时调用（§52）。
    pub fn invalidate(&mut self, song_id: &str) {
        self.entries.retain(|e| e.song_id != song_id);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// 把 API 的回答变成播放器需要的东西。`format_hint` 只是 API 的说法，
/// 解码器照样要按真实字节嗅一遍（§40）。
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

/// Phase 3：调 API 并组装条目。作为唯一入口，让 §52 的"重试策略"只存在一处。
pub fn resolve(
    song_id: &str,
    quality: Quality,
    _cache: &mut UrlCache,
) -> Result<AudioInfo, ProviderError> {
    let _ = api::Call::url_quality(song_id, quality, super::quality_id(quality));
    Err(ProviderError::Unsupported)
}
