//! 传输层与各 Source 共用的 HTTP 词汇表。
//!
//! 故意很小：一个描述请求的结构、播放器需要的响应事实，以及一个由 `yhttp.c`
//! 实现的 trait。这里全是传输层的东西，不含任何平台知识（§15）。
#![allow(dead_code)]

use alloc::string::String;
use alloc::vec::Vec;

/// 一次 HTTP 交换的**描述**（只描述，不执行）。
#[derive(Clone, Debug)]
pub struct Request {
    pub url: String,
    /// `bytes=0-8191`、`bytes=-12288`……（普通 GET 时为 `None`）。
    pub range: Option<String>,
    /// CDN 会检查这两个头，由 Provider 提供（§45）。
    pub referer: Option<String>,
    pub user_agent: Option<String>,
    pub cookie: Option<String>,
    /// TLS 校验模式；`Verify` 是所有地方的默认（§23）。
    pub tls: TlsMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlsMode {
    /// 校验证书链与主机名。
    Verify,
    /// 只用于 Provider 明确写明"自行固定信任"的端点
    /// （参考实现里网易云 CDN 属于这种）。
    Insecure,
}

/// 一次交换做完之后的产物。
#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    /// `Content-Range`/`Content-Length` 是 Source 得知流真实长度的途径
    /// （§19：绝不能靠 URL 后缀判断）。
    pub content_length: Option<u64>,
    pub content_range: Option<(u64, u64, u64)>,
    pub bytes: Vec<u8>,
}

impl Response {
    pub fn is_partial(&self) -> bool {
        self.status == 206
    }
    pub fn is_ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// 传输接缝。Vita 上的实现是 `yhttp.c`；测试可以换成"从内存里吐字节"的假实现，
/// 缓存逻辑就是这样在没有网络的情况下被测到的。
pub trait HttpClient {
    fn get(&mut self, req: &Request) -> Result<Response, super::super::source::SourceError>;
    /// 取消正在进行的传输（§24/§56）。
    fn cancel(&mut self) -> Result<(), super::super::source::SourceError>;
}

/// 重定向策略（§22）：可以跟随，但只允许同协议跳转，绝不从 https 降级到 http。
pub fn redirect_allowed(from: &str, to: &str) -> bool {
    let scheme = |u: &str| u.split("://").next().unwrap_or("").as_bytes().to_vec();
    scheme(from) == scheme(to)
}
