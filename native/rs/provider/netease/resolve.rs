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
/// CDN 地址大约还能用十几分钟。缓存短一点，长队列不会走到最后才发现链接死了。
const URL_TTL_MS: u64 = 12 * 60 * 1000;

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
    pub const fn empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

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

/// Phase 3：调 weapi，把 CDN 地址放进缓存。失败由调用方退回 outer/url。
pub fn resolve(
    song_id: &str,
    quality: Quality,
    cache: &mut UrlCache,
) -> Result<AudioInfo, ProviderError> {
    if !song_id.bytes().all(|b| b.is_ascii_digit()) || song_id.is_empty() || song_id.len() > 20 {
        return Err(ProviderError::NotFound);
    }
    let now = now_ms();
    if let Some(hit) = cache.get(song_id, quality, now) {
        return Ok(to_audio_info(
            song_id,
            quality,
            &hit.url,
            hit.size,
            hit.bitrate,
            0,
            hit.expires_at_ms,
            Some(AudioFormat::Mp3),
        ));
    }
    let level = super::quality_id(quality);
    let raw = api::call(&api::Call::url_quality(song_id, quality, level))?;
    let parsed = parse_song_url(&raw).ok_or(ProviderError::NotFound)?;
    let expires = now.saturating_add(URL_TTL_MS);
    cache.put(CachedUrl {
        song_id: String::from(song_id),
        quality,
        url: parsed.url.clone(),
        size: parsed.size,
        bitrate: parsed.bitrate,
        expires_at_ms: expires,
    });
    let format = match parsed.kind.as_str() {
        "mp3" => Some(AudioFormat::Mp3),
        "m4a" | "aac" => Some(AudioFormat::M4a),
        "flac" => Some(AudioFormat::Flac),
        _ => None,
    };
    Ok(to_audio_info(
        song_id,
        quality,
        &parsed.url,
        parsed.size,
        parsed.bitrate,
        0,
        expires,
        format,
    ))
}

/// 打开线程用的入口：有缓存就复用，没有就问一次 weapi。
pub fn fetch(song_id: &str, quality: Quality) -> Result<AudioInfo, ProviderError> {
    let mut guard = cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    resolve(song_id, quality, &mut guard)
}

/// 只升级网易云匿名 `outer/url`。其它地址原样返回。
/// weapi 失败（没网、参数被拒、响应里没有 url）时退回原来的地址，
/// 匿名播放不能因为这次解析失败而比 00.88 更差。
pub fn prepare_play_url(url: &str, referer: &str) -> (String, String) {
    let referer_out = if referer.is_empty() && is_netease_host(url) {
        String::from(super::REFERER)
    } else {
        String::from(referer)
    };
    let Some(id) = outer_song_id(url) else {
        return (String::from(url), referer_out);
    };
    match fetch(&id, Quality::Low) {
        Ok(info) if info.url.starts_with("http://") || info.url.starts_with("https://") => {
            crate::media::platform::log::append("netease: weapi 拿到播放地址，改走 CDN");
            (info.url, String::from(super::REFERER))
        }
        Ok(_) => {
            crate::media::platform::log::append("netease: weapi 没有可播地址，继续 outer/url");
            (String::from(url), referer_out)
        }
        Err(_) => {
            crate::media::platform::log::append("netease: weapi 失败，继续 outer/url");
            (String::from(url), referer_out)
        }
    }
}

fn cache() -> &'static std::sync::Mutex<UrlCache> {
    static CACHE: std::sync::Mutex<UrlCache> = std::sync::Mutex::new(UrlCache::empty());
    &CACHE
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn is_netease_host(url: &str) -> bool {
    url.contains("music.163.com") || url.contains("126.net")
}

fn outer_song_id(url: &str) -> Option<String> {
    if !url.contains("music.163.com/song/media/outer/url") {
        return None;
    }
    let after = url.split("id=").nth(1)?;
    let id: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    if id.is_empty() || id.len() > 20 {
        None
    } else {
        Some(id)
    }
}

struct ParsedUrl {
    url: String,
    size: Option<u64>,
    bitrate: u32,
    kind: String,
}

fn parse_song_url(json: &str) -> Option<ParsedUrl> {
    let url = json_string(json, "url")?;
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return None;
    }
    Some(ParsedUrl {
        url,
        size: json_u64(json, "size"),
        bitrate: json_u64(json, "br").unwrap_or(0) as u32,
        kind: json_string(json, "type").unwrap_or_default(),
    })
}

pub(crate) fn json_i32(json: &str, key: &str) -> Option<i32> {
    let pat = alloc::format!("\"{key}\":");
    let i = json.find(&pat)?;
    let rest = json[i + pat.len()..].trim_start();
    let neg = rest.starts_with('-');
    let body = if neg { &rest[1..] } else { rest };
    let digits: String = body.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let n: i32 = digits.parse().ok()?;
    Some(if neg { -n } else { n })
}

pub(crate) fn json_string(json: &str, key: &str) -> Option<String> {
    let pat = alloc::format!("\"{key}\":\"");
    let i = json.find(&pat)?;
    let rest = &json[i + pat.len()..];
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else if c == '"' {
            break;
        } else {
            out.push(c);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn json_u64(json: &str, key: &str) -> Option<u64> {
    let pat = alloc::format!("\"{key}\":");
    let i = json.find(&pat)?;
    let rest = json[i + pat.len()..].trim_start();
    if rest.starts_with("null") {
        return None;
    }
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() { None } else { digits.parse().ok() }
}
