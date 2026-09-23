//! 网易云 API 的接口面（任务书 §45–§47）。
//!
//! 下面这些端点就是计划要用到的；参数组合与请求加密照 `music-lib`（§46）搬 ——
//!
//! 当前状态：只有常量与形状。在 `net::http` 能用之前 `call()` 一律返回
//! `Unsupported`；而且**只允许这一个地方**写端点字符串，别处不许出现。
#![allow(dead_code)]

use crate::media::provider::{MusicProvider, ProviderError, Quality};

pub const HOST: &str = "https://music.163.com";

/// 歌曲详情：歌名/歌手/专辑/时长 —— 在线曲目在解析出 URL 之前就要能显示在曲库里。
pub const PATH_SONG_DETAIL: &str = "/api/v3/song/detail";
/// 播放地址：相当于参考实现里的 `GetDownloadURL`（§45）。
pub const PATH_SONG_URL_V1: &str = "/api/song/enhance/player/url/v1";
/// 歌词查询，和本地标签读取给文件提供的歌词对应（§43）。
pub const PATH_LYRIC: &str = "/api/song/lyric";
/// 歌单内容，给在线曲库界面用。
pub const PATH_PLAYLIST_DETAIL: &str = "/api/v6/playlist/detail";

/// 这个端点要用哪种请求形式。网易云能接好几种，参考实现里这四个都用 web API（§46）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavour {
    /// 明文 query string，不加密。
    Plain,
    /// `weapi`：AES-128-CBC + RSA 包裹密钥（§46）。
    WeApi,
    /// `eapi`：固定密钥的 AES-128-ECB，客户端 App 用的那种。
    EApi,
}

/// 一次 API 调用的**描述**。
#[derive(Clone, Debug)]
pub struct Call {
    pub path: &'static str,
    pub flavour: Flavour,
    /// 明文参数；加密在发送前由 `crypto` 完成。
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

/// 真正发一次调用。Phase 3 用 `net::http` 补上；Provider 不直接调它，由
/// `resolve` 调。
pub fn call(_c: &Call) -> Result<alloc::string::String, ProviderError> {
    Err(ProviderError::Unsupported)
}

/// 留着给以后的 Provider 自证实现了那条接缝。
pub fn provider_name<P: MusicProvider>(p: &P) -> &'static str {
    p.name()
}
