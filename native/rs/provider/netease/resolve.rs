//! songId → AudioInfo，以及 URL 缓存与过期策略（任务书 §50–§52）。
//!
//! 缓存策略（决定"专辑放到一半 CDN 链接失效"时的行为）：
//!
//! ```text
//! resolve(song, quality)
//!     有缓存且未接近过期   → 直接复用
//!     否则                → 问 API，并记下过期时间
//!     播放遇到 403/404/410 → 标记失效，重新解析一次，再试（Phase 5 接 UI）
//! ```
//!
//! 规矩：条目按 `(song_id, quality)` 索引；不落盘（§51）；URL 在过期前几分钟
//! 就算"该换新的"，这样长队列不会走到最后一首才发现链接死了。

use super::account::Session;
use super::api;
use super::json::Json;
use crate::media::net::transport::FormPost;
use crate::media::provider::{AudioFormat, AudioInfo, ProviderError, Quality};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// 比"真的过期"提前这么多就重新解析。
pub const EXPIRY_MARGIN_MS: u64 = 3 * 60 * 1000;
/// 服务端没给 `expi` 时的保守默认值（实测会给 1200 秒）。
pub const DEFAULT_EXPIRY_MS: u64 = 20 * 60 * 1000;
/// 不信任荒谬的 `expi`（万一服务端回了 30 天）。
pub const MAX_EXPIRY_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Clone, Debug)]
pub struct CachedUrl {
    pub song_id: String,
    pub quality: Quality,
    pub url: String,
    pub size: Option<u64>,
    pub bitrate: u32,
    pub duration_ms: u32,
    pub format: AudioFormat,
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

    /// 换账号时整表清空（URL 是服务端按账号算的，不能跨账号复用）。
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

fn audio_info_from(cached: &CachedUrl) -> AudioInfo {
    AudioInfo {
        url: cached.url.clone(),
        format: cached.format,
        duration_ms: cached.duration_ms,
        bitrate: cached.bitrate,
        size: cached.size,
        source: String::from("netease"),
        song_id: cached.song_id.clone(),
        quality: cached.quality,
        expires_at: cached.expires_at_ms,
    }
}

/// API 的 `type` 字段只是提示；解码器照样按真实字节嗅一遍（§40）。
fn format_from_api_type(t: Option<&str>) -> AudioFormat {
    match t {
        Some("mp3") => AudioFormat::Mp3,
        Some("m4a") | Some("aac") => AudioFormat::M4a,
        Some("flac") => AudioFormat::Flac,
        Some("ogg") => AudioFormat::OggVorbis,
        _ => AudioFormat::Unknown,
    }
}

fn cached_from_body(
    song_id: &str,
    quality: Quality,
    body: &str,
    now_ms: u64,
) -> Result<CachedUrl, ProviderError> {
    let root = Json::parse(body)
        .map_err(|e| ProviderError::Network(format!("网易云响应不是 JSON：{e}")))?;
    match root.get("code").and_then(Json::as_i64) {
        Some(200) => {}
        Some(c) => return Err(ProviderError::Network(format!("网易云接口 code={c}"))),
        None => {
            return Err(ProviderError::Network(String::from(
                "网易云响应缺少 code 字段",
            )))
        }
    }
    let item = root
        .get("data")
        .and_then(|d| d.at(0))
        .ok_or(ProviderError::NotFound)?;
    match item.get("code").and_then(Json::as_i64) {
        Some(200) => {}
        Some(404) => return Err(ProviderError::NotFound),
        Some(c) => return Err(ProviderError::Network(format!("网易云歌曲 code={c}"))),
        None => {
            return Err(ProviderError::Network(String::from(
                "网易云歌曲响应缺少 code 字段",
            )))
        }
    }
    let url = item
        .get("url")
        .and_then(Json::as_str)
        .unwrap_or("")
        .trim();
    if url.is_empty() {
        /* 实测：匿名拿不到版权的歌就是这里 url=null + code=404。 */
        return Err(ProviderError::NotFound);
    }
    let size = item
        .get("size")
        .and_then(Json::as_u64)
        .filter(|n| *n > 0);
    let bitrate = item
        .get("br")
        .and_then(Json::as_u64)
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32;
    let duration_ms = item
        .get("time")
        .and_then(Json::as_u64)
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32;
    let format = format_from_api_type(item.get("type").and_then(Json::as_str));
    let expi_s = item.get("expi").and_then(Json::as_u64).unwrap_or(0);
    let ttl_ms = if expi_s == 0 {
        DEFAULT_EXPIRY_MS
    } else {
        expi_s.saturating_mul(1000).min(MAX_EXPIRY_MS)
    };
    Ok(CachedUrl {
        song_id: String::from(song_id),
        quality,
        url: String::from(url),
        size,
        bitrate,
        duration_ms,
        format,
        expires_at_ms: now_ms.saturating_add(ttl_ms),
    })
}

/// 唯一入口：先看缓存，未命中才走网络。`secret` 与 `now_ms` 由调用方注入
/// （真机上是系统时间，测试里是固定值），让整条链在没有网络时也可复现。
pub fn resolve(
    song_id: &str,
    quality: Quality,
    secret: &[u8; 16],
    post: &mut dyn FormPost,
    session: &Session,
    cache: &mut UrlCache,
    now_ms: u64,
) -> Result<AudioInfo, ProviderError> {
    /* 歌曲 ID 只可能是数字：顺手把"文件名里混进来的怪东西"挡在请求之前。 */
    if song_id.is_empty() || !song_id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ProviderError::NotFound);
    }
    if let Some(hit) = cache.get(song_id, quality, now_ms) {
        return Ok(audio_info_from(hit));
    }
    let call = api::Call::url_quality(song_id, quality, super::quality_id(quality));
    let body = api::call(&call, secret, post, session)?;
    let entry = cached_from_body(song_id, quality, &body, now_ms)?;
    let info = audio_info_from(&entry);
    cache.put(entry);
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::provider::netease::test_support::{
        FakePost, FIXED_SECRET, NOT_FOUND_FIXTURE, URL_V1_FIXTURE,
    };

    const NOW: u64 = 1_000_000;
    const TEST_ID: &str = "3346495279";

    fn resolve_test(
        id: &str,
        post: &mut FakePost,
        cache: &mut UrlCache,
        now: u64,
    ) -> Result<AudioInfo, ProviderError> {
        resolve(
            id,
            Quality::Auto,
            FIXED_SECRET,
            post,
            &Session::anonymous(),
            cache,
            now,
        )
    }

    #[test]
    fn resolve_maps_live_response_into_audio_info_and_cache() {
        let mut post = FakePost::ok(URL_V1_FIXTURE);
        let mut cache = UrlCache::default();
        let info = resolve_test(TEST_ID, &mut post, &mut cache, NOW).unwrap();
        assert_eq!(info.url, "http://example-cdn.invalid/aa/bb/cc.m4a?token=xyz");
        assert_eq!(info.format, AudioFormat::M4a);
        assert_eq!(info.size, Some(7_771_899));
        assert_eq!(info.bitrate, 256_009);
        assert_eq!(info.duration_ms, 241_379);
        assert_eq!(info.expires_at, NOW + 1_200_000);
        assert_eq!(info.song_id, TEST_ID);
        assert_eq!(info.source, "netease");
        assert_eq!(cache.len(), 1);
        assert!(cache.get(TEST_ID, Quality::Auto, NOW).is_some());
    }

    #[test]
    fn second_resolve_reuses_cache_until_expiry_margin() {
        let mut post = FakePost::ok(URL_V1_FIXTURE);
        let mut cache = UrlCache::default();
        resolve_test(TEST_ID, &mut post, &mut cache, NOW).unwrap();
        resolve_test(TEST_ID, &mut post, &mut cache, NOW + 60_000).unwrap();
        assert_eq!(post.calls(), 1, "有效期内不该再发请求");
        resolve_test(TEST_ID, &mut post, &mut cache, NOW + 1_100_000).unwrap();
        assert_eq!(post.calls(), 2, "距过期不足 3 分钟必须重新解析");
    }

    #[test]
    fn resolve_maps_missing_rights_to_not_found_without_caching() {
        let mut post = FakePost::ok(NOT_FOUND_FIXTURE);
        let mut cache = UrlCache::default();
        let err = resolve_test("186016", &mut post, &mut cache, NOW).unwrap_err();
        assert_eq!(err, ProviderError::NotFound);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn resolve_reports_bad_json_as_network_error() {
        let mut post = FakePost::ok("<html>gateway</html>");
        let mut cache = UrlCache::default();
        let err = resolve_test(TEST_ID, &mut post, &mut cache, NOW).unwrap_err();
        assert!(
            matches!(err, ProviderError::Network(ref m) if m.contains("JSON")),
            "got {err:?}"
        );
    }

    #[test]
    fn resolve_rejects_non_numeric_song_id_without_a_request() {
        let mut post = FakePost::ok(URL_V1_FIXTURE);
        let mut cache = UrlCache::default();
        let err = resolve_test("../../etc/passwd", &mut post, &mut cache, NOW).unwrap_err();
        assert_eq!(err, ProviderError::NotFound);
        assert_eq!(post.calls(), 0);
    }

    #[test]
    fn cache_respects_margin_and_invalidate() {
        let mut cache = UrlCache::default();
        cache.put(CachedUrl {
            song_id: String::from("s"),
            quality: Quality::Auto,
            url: String::from("http://example.invalid/"),
            size: None,
            bitrate: 0,
            duration_ms: 0,
            format: AudioFormat::Unknown,
            expires_at_ms: 1_000_000,
        });
        assert!(cache.get("s", Quality::Auto, 0).is_some());
        assert!(cache.get("s", Quality::Auto, 900_000).is_none());
        assert!(cache.get("s", Quality::High, 0).is_none());
        cache.invalidate("s");
        assert_eq!(cache.len(), 0);
    }
}
