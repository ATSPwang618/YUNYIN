//! 网易云音乐 Provider（Phase 3，任务书 §44–§52）。
//!
//! 当前状态：结构与职责已经写清楚，`resolve()` 返回 `Unsupported` ——
//! 在网络层接上之前不假装能播。
//!
//! 职责（只有这些）：
//!   1. 拿歌曲 ID + 音质，向 API 要一个可播放的 URL；
//!   2. 带上会话 Cookie、Referer 和桌面浏览器 User-Agent（§48）；
//!   3. 汇报 `AudioInfo`：URL、大小、码率、格式提示、过期时间；
//!   4. 在**内存里**缓存 URL，过期后重新解析（§50/§51/§52）。
//!
//! 它绝对不能：自己开 socket、解码任何东西、或在没有明确开关的情况下把 Cookie 写进磁盘。
#![allow(dead_code)]

pub mod account;
pub mod api;
pub mod crypto;
pub mod login;
pub mod qr;
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

    fn resolve(&self, song_id: &str, quality: Quality) -> Result<AudioInfo, ProviderError> {
        resolve::fetch(song_id, quality)
    }
}

/// CDN 期望的 Referer。放在 Provider 旁边，因为这是"平台的事实"，
/// 不是"传输层的事实"。
pub const REFERER: &str = "https://music.163.com/";

pub use resolve::prepare_play_url;

/// 匿名播放地址。曲库扫描只拼这个字符串，不发网络请求。
pub fn anonymous_media_url(song_id: &str) -> Option<String> {
    if song_id.is_empty() || song_id.len() > 20 || !song_id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(alloc::format!(
        "https://music.163.com/song/media/outer/url?id={song_id}.mp3"
    ))
}
pub const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
     Chrome/120.0.0.0 Safari/537.36";

/// API 里音质档位的名字（§47）。Provider 负责把我们内部的 `Quality` 映射过去，
/// 播放器其他地方永远看不到这些字符串。
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
