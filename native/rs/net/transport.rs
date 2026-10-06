//! 同步 POST 表单的传输接缝（Phase 3）。
//!
//! 为什么单独一个文件：网易云的 weapi/eapi 都是 `POST 表单 + 读 JSON 响应`，
//! 而"发请求"这件事在真机上是 SceHttp（`yhttp.c`），在电脑上必须是假的。
//! 这里只放**词汇表**（请求、响应、错误、trait），不放任何执行代码 ——
//! 这样宿主机测试可以 `#[path]` 直接挂载本文件，Provider 也能被注入口
//! 换成"从内存里吐一段 JSON"的假实现。
//!
//! 分工照旧：Provider 不认识 socket，传输层不认识网易云。
#![allow(dead_code)]

use alloc::string::String;
use alloc::vec::Vec;

/// TLS 校验模式（Phase 0 起就定下的两种）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlsMode {
    /// 校验证书链与主机名（默认）。
    Verify,
    /// 只用于 Provider 明确写明"自行固定信任"的端点。
    Insecure,
}

/// 一次 POST 表单请求的完整描述。
#[derive(Clone, Debug)]
pub struct FormRequest {
    pub url: String,
    /// 已经编码好的 `application/x-www-form-urlencoded` 正文。
    pub body: String,
    pub referer: Option<String>,
    pub cookie: Option<String>,
    pub tls: TlsMode,
    /// 响应体缓冲上限（字节）。默认 128 KiB —— Provider 的接口大多是几 KB，
    /// 但"整张歌单/榜单"的响应会到几百 KB，那些调用要显式放大（见 api.rs）。
    pub max_body: usize,
}

/// 响应里传输层能确定的事实：状态码 + 正文。
#[derive(Clone, Debug)]
pub struct FormResponse {
    pub status: u16,
    pub bytes: Vec<u8>,
    /// 响应里的 `Set-Cookie`（多个合并成一条 `k=v; k2=v2`）；登录要用。
    pub set_cookie: Option<String>,
}

impl FormResponse {
    pub fn is_ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// POST 失败的分类。故意很少：能重试的、不能重试的、被取消的。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PostError {
    /// DNS / TLS / 连接断开 / 平台错误码（附原始描述）。
    Network(String),
    /// 切歌 / 退出时取消了这次传输。
    Cancelled,
    /// 响应体超过调用方给的缓冲（Phase 3 的接口都是几十 KB 级，不该发生）。
    TooLarge(String),
}

/// 传输接缝。真机实现是 `net/http.rs` 里的 `VitaPost`；
/// 测试实现是"记录请求 + 返回预置 JSON"。
pub trait FormPost {
    fn post_form(&mut self, req: &FormRequest) -> Result<FormResponse, PostError>;
}
