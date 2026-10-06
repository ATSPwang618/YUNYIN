//! 网易云 API 的接口面（任务书 §45–§47）。
//!
//! 这个文件里只有三件事：把 `Call` 变成明文 JSON、按 flavour 加密成表单、
//! 通过 `FormPost` 接缝发出去。**它不许碰 socket**，也不许碰解码器。
//!
//! 端点字符串只允许出现在这里（任务书规矩），别处一律引用常量。

use super::account::Session;
use super::{crypto, REFERER};
use crate::media::net::transport::{FormPost, FormRequest, PostError, TlsMode};
use crate::media::provider::{MusicProvider, ProviderError, Quality};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub const HOST: &str = "https://music.163.com";

/// 默认响应上限（几 KB 级的普通接口）。
const BODY_CAP_DEFAULT: usize = 128 * 1024;
/// 「整张歌单 / 榜单」类接口的响应上限：热歌榜 200 首的 detail 有几百 KB，
/// 128 KiB 会把这类调用直接判死（真机日志里就是 `响应超过 131072 字节`）。
/// 2 MiB：真机上 1000 首的大歌单（`/api/v6/playlist/detail` 带完整歌曲节点）
/// 能超过 1 MiB —— 日志里出现过 `响应超过 1048576 字节` 被整条判死。
/// 单首歌的接口仍然只给 128 KiB（BODY_CAP_DEFAULT）。
const BODY_CAP_LIST: usize = 2 * 1024 * 1024;
/// 走大缓冲的端点：拉列表，不拉单曲。
const BIG_PATHS: &[&str] = &[
    PATH_PLAYLIST_DETAIL,
    PATH_SONG_DETAIL,
    "/api/user/playlist",
    "/api/personalized/playlist",
    "/api/discovery/recommend/songs",
    /* v1 的每日推荐：响应里 30+ 首歌的完整节点也能超过 128 KiB
     * （日志里就是这么失败的：响应超过 131072 字节）。 */
    "/api/v1/discovery/recommend/songs",
];

/// 这个端点的响应缓冲该给多大。
fn body_cap_for(path: &str) -> usize {
    if BIG_PATHS.iter().any(|p| path == *p) {
        BODY_CAP_LIST
    } else {
        BODY_CAP_DEFAULT
    }
}

/// 歌曲详情：歌名/歌手/专辑/时长 —— 在线曲目在解析出 URL 之前就要能显示在曲库里。
pub const PATH_SONG_DETAIL: &str = "/api/v3/song/detail";
/// 播放地址：相当于参考实现里的 `GetDownloadURL`（§45）。
pub const PATH_SONG_URL_V1: &str = "/api/song/enhance/player/url/v1";
/// 歌词查询，和本地标签读取给文件提供的歌词对应（§43）。
pub const PATH_LYRIC: &str = "/api/song/lyric";
/// 歌单内容，给在线曲库界面用。
pub const PATH_PLAYLIST_DETAIL: &str = "/api/v6/playlist/detail";
/// 扫码登录第一步：取一个 unikey（二维码内容就是 `…/login?codekey=<unikey>`）。
pub const PATH_LOGIN_QR_UNIKEY: &str = "/api/login/qrcode/unikey";
/// 扫码登录第二步：轮询扫码/确认状态；803 时响应头里带 MUSIC_U。
pub const PATH_LOGIN_QR_POLL: &str = "/api/login/qrcode/client/login";

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
    pub params: Vec<(String, String)>,
}

impl Call {
    pub fn url_quality(song_id: &str, quality: Quality, level: &str) -> Self {
        Self {
            path: PATH_SONG_URL_V1,
            flavour: Flavour::WeApi,
            params: alloc::vec![
                (String::from("ids"), format!("[{}]", song_id)),
                (String::from("level"), String::from(level)),
                (String::from("encodeType"), String::from("aac")),
                (String::from("_q"), String::from(quality_id_str(quality))),
            ],
        }
    }
}

fn quality_id_str(q: Quality) -> &'static str {
    super::quality_id(q)
}

/// 按插入顺序拼 `{"k":"v",...}` —— 顺序也是请求的一部分（固定向量锁着）。
pub fn params_json(call: &Call) -> String {
    let mut out = String::from("{");
    for (i, (k, v)) in call.params.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(&json_escape(k));
        out.push_str("\":\"");
        out.push_str(&json_escape(v));
        out.push('"');
    }
    out.push('}');
    out
}

/// 参数值里只可能出现 ASCII（歌曲 ID、音质名），但照样转义 —— 不能让一个
/// 恶意文件名把参数 JSON 撕开。
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// `application/x-www-form-urlencoded` 的转义（base64 的 `+ / =` 必须转）。
pub fn form_urlencode(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0F) as usize] as char);
            }
        }
    }
    out
}

/// 真正发一次调用：组装 → 加密 → POST → 返回响应正文。
///
/// `secret` 由调用方提供（每次请求 16 个 ASCII 字符），测试里换成固定值就能
/// 逐字节锁住请求体。
pub struct ApiReply {
    pub body: String,
    /// 响应头里的 Set-Cookie（登录成功时要靠它拿 MUSIC_U）。
    pub set_cookie: Option<String>,
}

pub fn call(
    c: &Call,
    secret: &[u8; 16],
    post: &mut dyn FormPost,
    session: &Session,
) -> Result<String, ProviderError> {
    call_full(c, secret, post, session).map(|r| r.body)
}

/// 与 `call` 相同，但把响应头里的 Set-Cookie 一起带回来（登录用）。
pub fn call_full(
    c: &Call,
    secret: &[u8; 16],
    post: &mut dyn FormPost,
    session: &Session,
) -> Result<ApiReply, ProviderError> {
    let (url, body) = match c.flavour {
        Flavour::WeApi => {
            let payload = crypto::weapi_payload(&params_json(c), secret)
                .map_err(|e| ProviderError::Network(String::from(e)))?;
            /* 端点表里写的是 `/api/...`；weapi 的实际路径是把 `/api` 换成 `/weapi`。 */
            let api_path = c.path.strip_prefix("/api").unwrap_or(c.path);
            (
                format!("{HOST}/weapi{api_path}?csrf_token="),
                format!(
                    "params={}&encSecKey={}",
                    form_urlencode(&payload.params),
                    form_urlencode(payload.enc_sec_key.as_deref().unwrap_or("")),
                ),
            )
        }
        Flavour::EApi => {
            /* eapi 的 host 与 `/eapi/` 路径前缀还没在真机上验过；加密本身
             * 已在 crypto 里就绪并有固定向量，等 Phase 4 真要用时再接上，
             * 现在不假装能用。 */
            return Err(ProviderError::Unsupported);
        }
        Flavour::Plain => return Err(ProviderError::Unsupported),
    };
    let req = FormRequest {
        url,
        body,
        referer: Some(String::from(REFERER)),
        cookie: session.cookie_header(),
        tls: TlsMode::Verify,
        max_body: body_cap_for(c.path),
    };
    /*
     * 曾经在这里做过"HTTPS 失败就降级成纯 HTTP 重试"的兜底，用来救 Vita3K。
     * 实测证明没用：旧版模拟器（Build 3917）连纯 HTTP 也走不通
     * （`sceNetRecv` 直接超时），而它坏的是整个 SceHttp，不是 TLS；
     * 换新版模拟器（Build 4124）HTTPS 本来就是好的。
     *
     * 这条路会把会话 Cookie 明文发出去，收益为零，所以撤掉。
     * 模拟器相关结论见 docs/排障.md。
     */
    match post.post_form(&req) {
        Ok(resp) if resp.is_ok() => {
            let body = String::from_utf8_lossy(&resp.bytes).into_owned();
            Ok(ApiReply {
                body,
                set_cookie: resp.set_cookie,
            })
        }
        Ok(resp) => Err(ProviderError::Network(format!("网易云 HTTP {}", resp.status))),
        Err(PostError::Cancelled) => Err(ProviderError::Cancelled),
        Err(PostError::Network(m)) => Err(ProviderError::Network(m)),
        Err(PostError::TooLarge(m)) => Err(ProviderError::Network(m)),
    }
}

/// 留着给以后的 Provider 自证实现了那条接缝。
pub fn provider_name<P: MusicProvider>(p: &P) -> &'static str {
    p.name()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::provider::netease::test_support::{FakePost, FIXED_SECRET};
    use alloc::vec;

    fn url_v1_call() -> Call {
        Call::url_quality("3346495279", Quality::Auto, quality_id_str(Quality::Auto))
    }

    /// 每日推荐必须走**加密** weapi，而且路径要带 `v3`。
    ///
    /// 这条是回归钉子：以前调的是 `/weapi/discovery/recommend/songs`（少一个 `v3`），
    /// 服务器只回空壳，列表永远是空的 —— 让人误以为是"没登录/没版权"。
    #[test]
    fn daily_recommend_uses_encrypted_v3_path() {
        let req_call = Call {
            path: "/api/v3/discovery/recommend/songs",
            flavour: Flavour::WeApi,
            params: vec![(String::from("br"), String::from("320000"))],
        };
        let mut post = FakePost::ok(r#"{"code":200,"data":{"dailySongs":[]}}"#);
        let _ = call(&req_call, FIXED_SECRET, &mut post, &Session::anonymous()).unwrap();
        let seen = &post.requests[0];
        assert_eq!(
            seen.url,
            "https://music.163.com/weapi/v3/discovery/recommend/songs?csrf_token="
        );
        assert!(seen.body.starts_with("params="), "请求体必须是加密表单");
        assert!(seen.body.contains("&encSecKey="), "缺少 RSA 加密的密钥段");
        /* 给"拿真接口手动验证"用：cargo test -- --nocapture 会把这段打出来。 */
        println!("DAILY_BODY={}", seen.body);
    }

    /// 真接口能用的请求体（电脑参考实现逐字节对照过）。
    const REFERENCE_BODY: &str = "params=paW%2F6pUb3aLd8mrvP2LfgxH9F%2F1lJfgOkA2RSVzlp3FUGYOG3dA8GrrxAj%2BAve25xc4%2BKprvGq5aihUrhC9viBQcBArOc8hde0n1J8hafrfEh1UxOqXz0mjCQOxdA8dyAWZpM7m9t%2F75%2F%2BUuO72Scw%3D%3D&encSecKey=35701388baf89fed412e11269b9c76625d095ecaf17f03fa018abe19ea2d38b949debf242ee39a71ca1f6cda71b1b86a45aa909ee27f7e78e267d34e732f0de948206c3340a788d0003372183e2f753c1f78b66ac23d134ac1fc9b993156520ea826b8aa89a962d4491b4b8d7e08738e1da9b07aa39bf4a7ef0b1c210728cd52";

    #[test]
    fn weapi_call_posts_the_reference_body() {
        let mut post = FakePost::ok(r#"{"code":200}"#);
        let body = call(
            &url_v1_call(),
            FIXED_SECRET,
            &mut post,
            &Session::anonymous(),
        )
        .unwrap();
        assert_eq!(body, r#"{"code":200}"#);
        assert_eq!(post.calls(), 1);
        let seen = &post.requests[0];
        assert_eq!(
            seen.url,
            "https://music.163.com/weapi/song/enhance/player/url/v1?csrf_token="
        );
        assert_eq!(seen.body, REFERENCE_BODY);
        assert_eq!(seen.referer.as_deref(), Some(REFERER));
        assert_eq!(seen.cookie, None);
        assert_eq!(seen.tls, TlsMode::Verify);
    }

    #[test]
    fn call_sends_session_cookie_when_present() {
        let mut post = FakePost::ok("{}");
        let session = Session::from_cookie("MUSIC_U=abc; __csrf=def");
        let _ = call(&url_v1_call(), FIXED_SECRET, &mut post, &session).unwrap();
        assert_eq!(
            post.requests[0].cookie.as_deref(),
            Some("MUSIC_U=abc; __csrf=def")
        );
    }

    #[test]
    fn transport_failures_map_to_provider_errors() {
        let mut post = FakePost::failing(PostError::Network(String::from("boom")));
        assert_eq!(
            call(&url_v1_call(), FIXED_SECRET, &mut post, &Session::anonymous()),
            Err(ProviderError::Network(String::from("boom")))
        );
        let mut post = FakePost::failing(PostError::Cancelled);
        assert_eq!(
            call(&url_v1_call(), FIXED_SECRET, &mut post, &Session::anonymous()),
            Err(ProviderError::Cancelled)
        );
    }

    #[test]
    fn non_2xx_status_is_a_network_error() {
        let mut post = FakePost::http_error(403, "denied");
        let err = call(
            &url_v1_call(),
            FIXED_SECRET,
            &mut post,
            &Session::anonymous(),
        )
        .unwrap_err();
        assert!(
            matches!(err, ProviderError::Network(ref m) if m.contains("403")),
            "got {err:?}"
        );
    }

    #[test]
    fn params_json_escapes_string_values() {
        let c = Call {
            path: PATH_SONG_URL_V1,
            flavour: Flavour::WeApi,
            params: vec![(String::from("q"), String::from("a\"b\\c"))],
        };
        assert_eq!(params_json(&c), r#"{"q":"a\"b\\c"}"#);
    }

    #[test]
    fn form_urlencode_matches_application_x_www_form_urlencoded() {
        assert_eq!(form_urlencode("AZaz09-._~"), "AZaz09-._~");
        assert_eq!(form_urlencode("a b+c/="), "a+b%2Bc%2F%3D");
    }
}
