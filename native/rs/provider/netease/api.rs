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
    /// 播放地址。`encodeType` 用 mp3（无损档才问 flac）：匿名状态下
    /// `standard` + mp3 拿回来的就是真机已经整曲播完的那份文件。
    pub fn url_quality(song_id: &str, quality: Quality, level: &str) -> Self {
        use alloc::string::String;
        use alloc::vec;
        let encode = match quality {
            Quality::Lossless => "flac",
            _ => "mp3",
        };
        Self {
            path: PATH_SONG_URL_V1,
            flavour: Flavour::WeApi,
            params: vec![
                (String::from("ids"), alloc::format!("[{song_id}]")),
                (String::from("level"), String::from(level)),
                (String::from("encodeType"), String::from(encode)),
                (String::from("csrf_token"), String::from("")),
            ],
        }
    }
}

/// 发一次 weapi。歌曲地址只在打开线程里调用；扫码登录在自己的线程里调用。
/// 曲库扫描不能走这里。失败就让调用方退回匿名 `outer/url`。
pub fn call(c: &Call) -> Result<alloc::string::String, ProviderError> {
    if c.flavour != Flavour::WeApi {
        return Err(ProviderError::Unsupported);
    }
    let payload = super::crypto::encrypt_weapi(&c.params).map_err(|_| ProviderError::Unsupported)?;
    let (body, _) = post_payload(c.path, &payload, &super::login::api_cookie())?;
    Ok(body)
}

/// `path` 用 `/api/...`，这里改成 `/weapi/...`。第二个返回值是 Set-Cookie，不写日志。
pub fn post_weapi(
    path: &str,
    json: &str,
    cookie: &str,
) -> Result<(alloc::string::String, alloc::string::String), ProviderError> {
    let secret = super::crypto::random_secret();
    let payload =
        super::crypto::encrypt_weapi_json(json, &secret).map_err(|_| ProviderError::Unsupported)?;
    post_payload(path, &payload, cookie)
}

fn post_payload(
    path: &str,
    payload: &super::crypto::Payload,
    cookie: &str,
) -> Result<(alloc::string::String, alloc::string::String), ProviderError> {
    let enc = payload.enc_sec_key.clone().unwrap_or_default();
    let body = alloc::format!(
        "params={}&encSecKey={enc}",
        super::crypto::form_escape(&payload.params)
    );
    let url = weapi_url(path);
    let cookie = if cookie.is_empty() { "os=pc" } else { cookie };
    post_form(&url, &body, cookie)
}

fn weapi_url(path: &str) -> alloc::string::String {
    let path = path.replacen("/api/", "/weapi/", 1);
    alloc::format!("{HOST}{path}")
}

fn post_form(
    url: &str,
    body: &str,
    cookie: &str,
) -> Result<(alloc::string::String, alloc::string::String), ProviderError> {
    use alloc::string::String;
    use alloc::vec;
    use std::ffi::CString;

    let Ok(c_url) = CString::new(url) else {
        return Err(ProviderError::Network(String::from("bad url")));
    };
    let Ok(c_ref) = CString::new("https://music.163.com/") else {
        return Err(ProviderError::Network(String::from("bad referer")));
    };
    let Ok(c_cookie) = CString::new(cookie) else {
        return Err(ProviderError::Network(String::from("bad cookie")));
    };
    let mut buf = vec![0u8; 16384];
    let mut set_cookie = vec![0u8; 4096];
    let mut status: i32 = 0;
    let n = unsafe {
        yhttp_post(
            c_url.as_ptr(),
            body.as_ptr(),
            body.len() as i32,
            c_ref.as_ptr(),
            c_cookie.as_ptr(),
            buf.as_mut_ptr(),
            buf.len() as i32,
            &mut status,
            set_cookie.as_mut_ptr(),
            set_cookie.len() as i32,
        )
    };
    if n < 0 {
        return Err(ProviderError::Network(alloc::format!("post {n}")));
    }
    if status != 200 {
        return Err(ProviderError::Network(alloc::format!("status {status}")));
    }
    let n = (n as usize).min(buf.len());
    let text = String::from_utf8_lossy(&buf[..n]).into_owned();
    let cookie_n = set_cookie.iter().position(|b| *b == 0).unwrap_or(set_cookie.len());
    let cookie_text = String::from_utf8_lossy(&set_cookie[..cookie_n]).into_owned();
    Ok((text, cookie_text))
}

extern "C" {
    fn yhttp_post(
        url: *const i8,
        body: *const u8,
        body_len: i32,
        referer: *const i8,
        cookie: *const i8,
        out: *mut u8,
        out_cap: i32,
        status_out: *mut i32,
        set_cookie: *mut u8,
        set_cookie_cap: i32,
    ) -> i32;
}

/// 留着给以后的 Provider 自证实现了那条接缝。
pub fn provider_name<P: MusicProvider>(p: &P) -> &'static str {
    p.name()
}
