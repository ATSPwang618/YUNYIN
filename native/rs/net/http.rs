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

/* ------------------------------------------------------------- 流式传输 -- */
/*
 * `yhttp_stream_*`（native/net/yhttp.c）的安全包装：把 HTTP 变成"可随机读取的
 * 字节流"，再由 `source::http::HttpRangeSource` 在它上面做窗口缓存与取数线程。
 *
 * 这里只做转发：一次 `read_at` 对应一次 C 侧调用，窗口策略全在 Rust 侧，
 * 所以电脑上可以用假传输替换掉它来验证（见 source/http.rs 的测试）。
 */

use core::ffi::c_void;
use std::ffi::CString;
use crate::media::source::SourceError;

extern "C" {
    fn yhttp_stream_open(
        url: *const i8,
        referer: *const i8,
        tls_mode: i32,
        size_out: *mut i64,
        err_out: *mut i32,
    ) -> *mut c_void;
    fn yhttp_stream_read(
        s: *mut c_void,
        off: i64,
        dst: *mut c_void,
        n: i64,
    ) -> i64;
    fn yhttp_stream_cancel(s: *mut c_void);
    fn yhttp_stream_close(s: *mut c_void);
    fn yhttp_stream_error(s: *mut c_void) -> i32;
}

/// 一条打开的 HTTP 流。只在取数线程里创建与使用（不跨线程共享）。
pub struct Stream {
    p: *mut c_void,
    size: Option<u64>,
}

/* 只为能放进取数线程：Stream 从头到尾只被那一个线程碰；
 * C 侧的流对象本身不做跨线程共享（每次请求各自建）。 */
unsafe impl Send for Stream {}

impl Stream {
    /// `tls`: 0 = 默认，1 = 打开校验（两者都走 HTTPS；默认校验本来就是开的）。
    pub fn open(url: &str, referer: &str, tls: i32) -> Result<Self, SourceError> {
        let Ok(c_url) = CString::new(url) else {
            return Err(SourceError::Unsupported);
        };
        let c_ref = CString::new(referer).unwrap_or_default();
        let mut size: i64 = -1;
        let mut err: i32 = 0;
        let p = unsafe {
            yhttp_stream_open(
                c_url.as_ptr(),
                c_ref.as_ptr(),
                tls,
                &mut size as *mut i64,
                &mut err as *mut i32,
            )
        };
        if p.is_null() {
            return Err(SourceError::Network(alloc::format!(
                "yhttp_stream_open 失败 (0x{:08X})",
                err as u32
            )));
        }
        Ok(Self {
            p,
            size: if size >= 0 { Some(size as u64) } else { None },
        })
    }
}

impl super::super::source::http::ByteTransport for Stream {
    fn read_at(&mut self, off: u64, dst: &mut [u8]) -> Result<usize, SourceError> {
        if self.p.is_null() || dst.is_empty() {
            return Ok(0);
        }
        let n = unsafe {
            yhttp_stream_read(
                self.p,
                off as i64,
                dst.as_mut_ptr() as *mut c_void,
                dst.len() as i64,
            )
        };
        if n < 0 {
            /*
             * 以前这里把所有负返回值都写成 Cancelled，"真机上到底为什么读失败"
             * 就永远看不见了。现在按 C 侧记下的错误码如实区分：
             *   0 / EINTR(0x80410104) / ABORTED(0x80431080) → 取消（切歌、退出）
             *   其余 → 真错误，带上原始 Vita 错误码
             */
            let code = unsafe { yhttp_stream_error(self.p) };
            let u = code as u32;
            if code == 0 || u == 0x80410104 || u == 0x80431080 {
                return Err(SourceError::Cancelled);
            }
            return Err(SourceError::Network(alloc::format!(
                "HTTP 流读取失败 0x{:08X}{}",
                u,
                vita_error_hint(u)
            )));
        }
        Ok(n as usize)
    }

    fn size(&self) -> Option<u64> {
        self.size
    }
}

/// 常见失败码的中文解释，让真机日志能直接读（取值来自 VitaSDK 头文件）。
fn vita_error_hint(code: u32) -> &'static str {
    match code {
        0x80431022 => "（内存池不足）",
        0x80431068 => "（超时）",
        0x80431075 => "（TLS 握手/证书被拒）",
        0x80431080 => "（被中止）",
        0x80410104 => "（被取消）",
        0x80435022 => "（SSL 内存不足）",
        0x80435060 => "（证书被拒）",
        0x80436002 => "（DNS 解析不到主机）",
        0x80436003 => "（DNS 超时）",
        _ => "",
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if !self.p.is_null() {
            unsafe { yhttp_stream_cancel(self.p) };
            unsafe { yhttp_stream_close(self.p) };
            self.p = core::ptr::null_mut();
        }
    }
}
