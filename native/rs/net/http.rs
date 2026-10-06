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

/* TLS 策略的两种模式已经在 transport.rs 里定义（Provider 的 POST 也要用），
 * 这里原样转出来，老的 `net::http::TlsMode` 路径继续有效。 */
pub use super::transport::TlsMode;

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
        cookie: *const i8,
        tls_mode: i32,
        size_out: *mut i64,
        err_out: *mut i32,
        status_out: *mut i32,
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
    ///
    /// `cookie`：登录会话（MUSIC_U 等）。网易云给登录用户的播放地址里带
    /// `authSecret`，那种地址的 CDN **要校 Cookie**，不带就回 403 ——
    /// 真机上的表现是"切歌之后没声音，一直卡着"。
    pub fn open(
        url: &str,
        referer: &str,
        cookie: &str,
        tls: i32,
    ) -> Result<Self, SourceError> {
        let Ok(c_url) = CString::new(url) else {
            return Err(SourceError::Unsupported);
        };
        let c_ref = CString::new(referer).unwrap_or_default();
        let c_cookie = CString::new(cookie).unwrap_or_default();
        let mut size: i64 = -1;
        let mut err: i32 = 0;
        let mut status: i32 = 0;
        let p = unsafe {
            yhttp_stream_open(
                c_url.as_ptr(),
                c_ref.as_ptr(),
                c_cookie.as_ptr(),
                tls,
                &mut size as *mut i64,
                &mut err as *mut i32,
                &mut status as *mut i32,
            )
        };
        if p.is_null() {
            /* 解析器卡死 / 超时：**叫后台去重建**，这一次如实失败（不要同步重建：
             * 那会占住调用线程 1~2 秒，JS 线程上就是黑屏，见 request_net_stack_reset）。 */
            if is_net_stack_down(err) {
                request_net_stack_reset();
            }
            /* 服务器给了明确的拒绝状态（403 无权限 / 404 / 410 已失效）时
             * 报 Http，让上层能区分"这首歌没权限"和"网络不通"。 */
            if status == 403 || status == 404 || status == 410 {
                return Err(SourceError::Http {
                    status: status as u16,
                });
            }
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
        /* 真机最常见的两种原因都在这里点名：时钟不对、老固件缺 TLS 1.2。 */
        0x80431075 => {
            "（TLS 握手/证书被拒 —— 先核对「设置 → 日期与时间」；老固件需要 iTLS 插件补 TLS 1.2）"
        }
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

/* ------------------------------------------------- 网络栈自恢复（解析器） -- */
/*
 * 真机日志里出现过：DNS 解析器"拒服务"（`0x80436009
 * SCE_HTTP_ERROR_RESOLVER_ESERVERREFUSED`）之后**所有**请求都失败 ——
 * 连二维码都取不回来，只有重启应用才恢复。
 *
 * 这里做两件事：
 *   1. 认出"这套错误码属于解析器/超时"（`is_net_stack_down`）；
 *   2. 全栈重建一次（C 侧 `yhttp_net_reset`），并**限速**（30 秒最多一次）。
 *
 * 重建有在线流正在用网络栈时会被 C 侧拒绝（返回 -1）—— 那是对的，
 * 不能让正在播的歌被打断；下一次请求再试。
 */
extern "C" {
    fn yhttp_net_reset() -> i32;
    fn yhttp_online() -> i32;
}

/// 网络是否已连接（SceNetCtl 报的实时状态）；给启动那行"运行环境"用。
pub fn online() -> bool {
    unsafe { yhttp_online() > 0 }
}

/// 当前 API 请求的**下载进度**（已收字节 / Content-Length）。
///
/// 界面用它显示"正在同步歌单… 32%"。歌单详情那种几 MB 的响应，慢的就是收正文
/// 这一段，所以这个百分比是实测值，不是按时间估的。返回 `None` = 现在没在下载、
/// 或者服务器没给 Content-Length（那种情况界面只显示已等待的秒数）。
pub fn download_progress() -> Option<(u64, u64)> {
    if unsafe { yhttp_dl_active() } == 0 {
        return None;
    }
    let total = unsafe { yhttp_dl_total() };
    if total <= 0 {
        return None;
    }
    let got = unsafe { yhttp_dl_got() }.max(0) as u64;
    Some((got, total as u64))
}

static LAST_RESET_MS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
/// 后台重建是否已经在跑（防止一次抖动里连开好几个重建线程）。
static RESET_INFLIGHT: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
const RESET_MIN_GAP_MS: u64 = 30_000;

/// 这个错误码说明网络栈（尤其 DNS 解析器）已经不健康了。
/// TLS 握手 / 证书被拒（模拟器上很常见；真机偶尔也会遇到证书链问题）。
fn is_tls_error(code: i32) -> bool {
    matches!(code as u32, 0x8043_1075 | 0x8043_1062 | 0x8043_1063 | 0x8043_1073)
}

/// 这个错误码说明网络栈（尤其 DNS 解析器）已经不健康了。
fn is_net_stack_down(code: i32) -> bool {
    let u = code as u32;
    /* 0x80436001..=0x8043600F = SCE_HTTP_ERROR_RESOLVER_*（DNS 各类失败）；
     * 0x80431068 = 超时（解析器卡死时最常伴生的那个）。 */
    (0x8043_6001..=0x8043_600F).contains(&u) || u == 0x8043_1068
}

/// 请求一次网络栈重建 —— **只发信号，重建本身放后台线程做**。
///
/// 为什么不能让调用方同步做：这个判定会被"JS 驱动的原生调用"（行填充取详情、
/// 清单读取等）触发，而重建要拆掉再建 SceHttp/SceNet，实测占 1~2 秒。放在调用
/// 线程里就会顶爆 PocketJS 的单帧预算（2 秒看门狗），界面当场黑屏 ——
/// 真机 health.json 里就是 `"error":"guest JavaScript time budget exceeded"`。
///
/// 调用方拿到错误直接返回即可：请求会由上层（取数线程退避重试 / 解析重试）自然重来，
/// 那时栈已经重建好了。代价是"这一次"失败，换来的是界面不会黑。
fn request_net_stack_reset() {
    let now = crate::media::platform::time::now_ms();
    let last = LAST_RESET_MS.load(core::sync::atomic::Ordering::Acquire);
    if now.saturating_sub(last) < RESET_MIN_GAP_MS {
        return; /* 30 秒内只重建一次（C 侧另有同款限速） */
    }
    if RESET_INFLIGHT
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
        .is_err()
    {
        return; /* 已经有一次在后台跑 */
    }
    LAST_RESET_MS.store(now, core::sync::atomic::Ordering::Release);
    let spawned = std::thread::Builder::new()
        .name("yunyin-net-reset".into())
        /* 栈给足：这个线程里会走 Rust 的日志格式化 + C 的整栈重建，
         * 32 KB 在真机上偏紧（栈溢出会直接跳空指针，排查代价极高）。 */
        .stack_size(96 * 1024)
        .spawn(|| {
            let rc = unsafe { yhttp_net_reset() };
            if rc < 0 {
                /*
                 * C 侧拒了：说明**现在有请求或在线流在用这套栈**（或初始化失败）。
                 * 这时候要**允许马上再试**——把限速戳清掉，让下一笔失败的请求再发起重建，
                 * 否则会白白等满 30 秒，期间所有请求都失败。
                 */
                LAST_RESET_MS.store(0, core::sync::atomic::Ordering::Release);
                crate::media::platform::log::append(
                    "net: 网络栈重建被推迟（有请求在飞），稍后自动重试",
                );
            } else {
                crate::media::platform::log::append(&alloc::format!(
                    "net: 网络栈重建（后台，解析器恢复）rc=0x{:08X}",
                    rc as u32
                ));
            }
            RESET_INFLIGHT.store(false, core::sync::atomic::Ordering::Release);
        });
    if spawned.is_err() {
        RESET_INFLIGHT.store(false, core::sync::atomic::Ordering::Release);
    }
}

/* ------------------------------------------------------------ POST 表单 -- */
/*
 * Phase 3：Provider 的 weapi 调用是 `POST 表单 + 读 JSON`，C 侧对应 `yhttp_post`。
 * 这里只做搬运：Rust 字符串 → C 字符串，C 的缓冲 → Vec<u8>。
 *
 * 响应上限默认 128 KiB —— 大多接口是几 KB 级；真超了宁可报错，
 * 也不能把半截 JSON 交给解析器。整张歌单/榜单例外：调用方用
 * `FormRequest::max_body` 放大（见 api.rs 的 `BIG_PATHS`）。
 */
const FORM_BODY_CAP: usize = 128 * 1024;
/*
 * 登录响应里的 Set-Cookie 上限。
 *
 * 为什么从 1 KiB 提到 8 KiB：网易云登录（803 那一下）会回一大串 Cookie
 * （MUSIC_U + NMTID + __csrf + 一堆统计项），真机上**实测撞满了 1 KiB 的上限**。
 * 截断后的 Cookie 头不完整，服务端就当你是匿名 —— 于是"登录成功"（803 已确认）
 * 但接着 /api/user/playlist、/api/discovery/recommend/songs 全被判未登录，
 * 表现就是歌单页一直"同步中"（account_playlists.json 永远写不出来）。
 */
const FORM_COOKIE_CAP: usize = 8 * 1024;

extern "C" {
    fn yhttp_dl_active() -> i32;
    fn yhttp_dl_got() -> i64;
    fn yhttp_dl_total() -> i64;
    fn yhttp_post(
        url: *const i8,
        body: *const i8,
        content_type: *const i8,
        referer: *const i8,
        cookie: *const i8,
        tls_mode: i32,
        out: *mut c_void,
        out_cap: i32,
        status_out: *mut i32,
        len_out: *mut i32,
        set_cookie_out: *mut i8,
        set_cookie_cap: i32,
    ) -> i32;
}

/// 真机上的 `FormPost` 实现。
pub struct VitaPost;

impl super::transport::FormPost for VitaPost {
    fn post_form(
        &mut self,
        req: &super::transport::FormRequest,
    ) -> Result<super::transport::FormResponse, super::transport::PostError> {
        let (first, rc) = post_once(req);
        /*
         * 解析器 / 超时这类错误：叫**后台**去重建网络栈，这一次如实失败。
         * 不做同步重建 + 就地重试 —— 那个组合会在调用线程里占 1~2 秒，
         * 一旦发生在 JS 帧里就是"换歌黑屏"（PocketJS 2 秒看门狗）。
         * 上层本来就会重试（解析 3 次 / 取数退避重试），不需要在这里抢那一次。
         */
        if first.is_err() && is_net_stack_down(rc) {
            request_net_stack_reset();
        }
        /*
         * TLS 握手/证书被拒时，降级为"不校验"再试一次。
         *
         * 真机第一次仍然走完整校验，只有校验失败才会走到这里 —— 行为不变。
         *
         * **不要**在这里调 `sceHttpsDisableOption` 去真正关掉校验：
         *   * 模拟器上它直接返回 0x8043506B（关不掉），救不了任何东西；
         *   * 真机上它会把**进程级**的证书校验关掉，之后所有请求（包括后面
         *     带会话 Cookie 的账号接口）都不再校验证书 —— 万一遇到劫持，
         *     等于把登录凭证交出去。收益为零、风险实打实，所以不做。
         * 证书校验的真实状态由探针的自签名目标判定（会被拒 = 校验是活的）。
         */
        if first.is_err()
            && is_tls_error(rc)
            && req.tls == super::transport::TlsMode::Verify
        {
            let mut relaxed = req.clone();
            relaxed.tls = super::transport::TlsMode::Insecure;
            crate::media::platform::log::append(&alloc::format!(
                "net: TLS 校验被拒（0x{:08X}），降级重试一次",
                rc as u32
            ));
            return post_once(&relaxed).0;
        }
        first
    }
}

/// 真正发一次 POST，把原始返回码一起带回来（调用方据此判断要不要重建网络栈）。
fn post_once(
    req: &super::transport::FormRequest,
) -> (
    Result<super::transport::FormResponse, super::transport::PostError>,
    i32,
) {
    {
        use super::transport::{FormResponse, PostError};
        let url = match CString::new(req.url.as_str()) {
            Ok(v) => v,
            Err(_) => {
                return (
                    Err(PostError::Network(String::from("URL 里有 NUL 字节"))),
                    0,
                )
            }
        };
        let body = match CString::new(req.body.as_str()) {
            Ok(v) => v,
            Err(_) => {
                return (
                    Err(PostError::Network(String::from("请求体里有 NUL 字节"))),
                    0,
                )
            }
        };
        let content_type =
            CString::new("application/x-www-form-urlencoded").unwrap_or_default();
        let referer = CString::new(req.referer.as_deref().unwrap_or("")).unwrap_or_default();
        let cookie = CString::new(req.cookie.as_deref().unwrap_or("")).unwrap_or_default();
        let tls = match req.tls {
            super::transport::TlsMode::Verify => 1,
            super::transport::TlsMode::Insecure => 0,
        };
        /* 每个请求按自己的上限分配（大部分调用是 128 KiB，歌单类放大）。 */
        let cap = if req.max_body == 0 { FORM_BODY_CAP } else { req.max_body };
        let mut buf = alloc::vec![0u8; cap];
        let mut cookie_buf = alloc::vec![0u8; FORM_COOKIE_CAP];
        let mut status: i32 = 0;
        let mut len: i32 = 0;
        let rc = unsafe {
            yhttp_post(
                url.as_ptr(),
                body.as_ptr(),
                content_type.as_ptr(),
                referer.as_ptr(),
                cookie.as_ptr(),
                tls,
                buf.as_mut_ptr() as *mut c_void,
                cap as i32,
                &mut status as *mut i32,
                &mut len as *mut i32,
                cookie_buf.as_mut_ptr() as *mut i8,
                FORM_COOKIE_CAP as i32,
            )
        };
        if rc == 0 {
            buf.truncate(len.max(0) as usize);
            let end = cookie_buf
                .iter()
                .position(|b| *b == 0)
                .unwrap_or(cookie_buf.len());
            let set_cookie = if end == 0 {
                None
            } else {
                Some(String::from_utf8_lossy(&cookie_buf[..end]).into_owned())
            };
            (
                Ok(FormResponse {
                    status: status.max(0) as u16,
                    bytes: buf,
                    set_cookie,
                }),
                0,
            )
        } else if rc == -2 {
            (
                Err(PostError::TooLarge(alloc::format!("响应超过 {cap} 字节"))),
                rc,
            )
        } else {
            (
                Err(PostError::Network(alloc::format!(
                    "yhttp_post 失败 0x{:08X}{}",
                    rc as u32,
                    vita_error_hint(rc as u32)
                ))),
                rc,
            )
        }
    }
}
