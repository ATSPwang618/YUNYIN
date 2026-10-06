//! 扫码登录（Phase 4）。
//!
//! 流程（电脑上真接口验证过的响应形状）：
//!
//! ```text
//! POST /weapi/login/qrcode/unikey        {type:1}        → {"code":200,"unikey":"<36位>"}
//! 二维码内容 = https://music.163.com/login?codekey=<unikey>
//! POST /weapi/login/qrcode/client/login  {key,type:1}    → code 800/801/802/803
//!   800 二维码过期 / 801 等待扫码 / 802 已扫码待确认 / 803 授权成功
//!   803 时响应头 Set-Cookie 里带 MUSIC_U（+ __csrf），这就是会话。
//! ```

use super::account::Session;
use super::api::{self, Call, Flavour};
use super::json::Json;
use crate::media::net::transport::FormPost;
use crate::media::provider::ProviderError;
use alloc::format;
use alloc::string::String;

/// 网易云 App 扫的二维码内容前缀。
pub const QR_PREFIX: &str = "https://music.163.com/login?codekey=";

pub fn qr_content(unikey: &str) -> String {
    format!("{QR_PREFIX}{unikey}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QrStatus {
    Expired,
    WaitingScan,
    WaitingConfirm,
    Confirmed,
}

/// 创建二维码的结果：unikey + 这次响应发的会话 Cookie。
///
/// **Cookie 必须跟着轮询一起发**：扫码登录本质是"手机授权一个浏览器会话"，
/// 服务器靠这个 Cookie 认出"哪个会话被授权了"。丢掉它就会出现
/// "手机显示登录成功、我们的轮询却永远 801（等待扫码）"。
#[derive(Clone, Debug, Default)]
pub struct QrStart {
    pub unikey: String,
    pub cookie: Option<String>,
}

fn qr_call(path: &'static str, params: alloc::vec::Vec<(String, String)>) -> Call {
    Call {
        path,
        flavour: Flavour::WeApi,
        params,
    }
}

/// 第一步：拿 unikey。
pub fn start(
    secret: &[u8; 16],
    post: &mut dyn FormPost,
    session: &Session,
) -> Result<QrStart, ProviderError> {
    use alloc::vec;
    let call = qr_call(
        api::PATH_LOGIN_QR_UNIKEY,
        vec![(String::from("type"), String::from("1"))],
    );
    let reply = api::call_full(&call, secret, post, session)?;
    let root = Json::parse(&reply.body)
        .map_err(|e| ProviderError::Network(format!("登录响应不是 JSON：{e}")))?;
    match root.get("code").and_then(Json::as_i64) {
        Some(200) => {}
        Some(c) => return Err(ProviderError::Network(format!("取二维码 code={c}"))),
        None => {
            return Err(ProviderError::Network(String::from(
                "取二维码响应缺少 code",
            )))
        }
    }
    let key = root
        .get("unikey")
        .and_then(Json::as_str)
        .unwrap_or("")
        .trim();
    if key.is_empty() {
        return Err(ProviderError::Network(String::from(
            "取二维码响应没有 unikey",
        )));
    }
    Ok(QrStart {
        unikey: String::from(key),
        cookie: reply.set_cookie,
    })
}

/// 第二步：轮询状态；`Confirmed` 时把 Set-Cookie 一起返回。
pub fn poll(
    unikey: &str,
    secret: &[u8; 16],
    post: &mut dyn FormPost,
    session: &Session,
) -> Result<(QrStatus, Option<String>), ProviderError> {
    use alloc::vec;
    let call = qr_call(
        api::PATH_LOGIN_QR_POLL,
        vec![
            (String::from("key"), String::from(unikey)),
            (String::from("type"), String::from("1")),
        ],
    );
    let reply = api::call_full(&call, secret, post, session)?;
    let root = Json::parse(&reply.body)
        .map_err(|e| ProviderError::Network(format!("轮询响应不是 JSON：{e}")))?;
    let code = root.get("code").and_then(Json::as_i64).unwrap_or(-1);
    let status = match code {
        800 => QrStatus::Expired,
        801 => QrStatus::WaitingScan,
        802 => QrStatus::WaitingConfirm,
        803 => QrStatus::Confirmed,
        other => {
            return Err(ProviderError::Network(format!(
                "二维码轮询 code={other}"
            )))
        }
    };
    let cookie = if status == QrStatus::Confirmed {
        reply.set_cookie
    } else {
        None
    };
    Ok((status, cookie))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::provider::netease::test_support::{FakePost, FIXED_SECRET};

    const UNIKEY: &str = "cbe2ec52-46d6-4433-8ab9-79b756d9dc5e";

    #[test]
    fn qr_content_matches_reference() {
        assert_eq!(
            qr_content(UNIKEY),
            "https://music.163.com/login?codekey=cbe2ec52-46d6-4433-8ab9-79b756d9dc5e"
        );
    }

    #[test]
    fn start_reads_unikey_from_live_shaped_response() {
        let mut post =
            FakePost::ok(r#"{"code":200,"unikey":"cbe2ec52-46d6-4433-8ab9-79b756d9dc5e"}"#);
        let reply = start(FIXED_SECRET, &mut post, &Session::anonymous()).unwrap();
        assert_eq!(reply.unikey, UNIKEY);
        assert_eq!(reply.cookie, None);
        assert!(post.requests[0]
            .url
            .ends_with("/weapi/login/qrcode/unikey?csrf_token="));
        assert!(post.requests[0].body.contains("params="));
    }

    #[test]
    fn start_keeps_the_session_cookie_from_the_unikey_response() {
        let mut post =
            FakePost::ok(r#"{"code":200,"unikey":"cbe2ec52-46d6-4433-8ab9-79b756d9dc5e"}"#)
                .with_cookie("NMTID=abc; __csrf=def");
        let reply = start(FIXED_SECRET, &mut post, &Session::anonymous()).unwrap();
        assert_eq!(reply.cookie.as_deref(), Some("NMTID=abc; __csrf=def"));
    }

    #[test]
    fn poll_sends_the_login_session_cookie() {
        let mut post = FakePost::ok(r#"{"code":801,"message":"x"}"#);
        let login_session = Session::from_cookie("NMTID=abc; __csrf=def");
        let _ = poll(UNIKEY, FIXED_SECRET, &mut post, &login_session).unwrap();
        assert_eq!(
            post.requests[0].cookie.as_deref(),
            Some("NMTID=abc; __csrf=def")
        );
    }

    #[test]
    fn poll_maps_status_codes() {
        for (code, want) in [
            (800, QrStatus::Expired),
            (801, QrStatus::WaitingScan),
            (802, QrStatus::WaitingConfirm),
        ] {
            let body = format!(r#"{{"code":{code},"message":"x"}}"#);
            let mut post = FakePost::ok(&body);
            let (status, cookie) =
                poll(UNIKEY, FIXED_SECRET, &mut post, &Session::anonymous()).unwrap();
            assert_eq!(status, want);
            assert_eq!(cookie, None);
        }
    }

    #[test]
    fn confirmed_poll_returns_set_cookie() {
        let mut post = FakePost::ok(r#"{"code":803,"message":"授权成功"}"#)
            .with_cookie("MUSIC_U=abc123; __csrf=def456");
        let (status, cookie) =
            poll(UNIKEY, FIXED_SECRET, &mut post, &Session::anonymous()).unwrap();
        assert_eq!(status, QrStatus::Confirmed);
        assert_eq!(cookie.as_deref(), Some("MUSIC_U=abc123; __csrf=def456"));
    }

    #[test]
    fn start_reports_missing_unikey() {
        let mut post = FakePost::ok(r#"{"code":200}"#);
        let err = start(FIXED_SECRET, &mut post, &Session::anonymous()).unwrap_err();
        assert!(matches!(err, ProviderError::Network(ref m) if m.contains("unikey")));
    }
}
