//! Phase 0 网络冒烟测试（任务书 §25/§26/§27）。
//!
//! 在后台线程里跑 `native/net/yhttp.c`，把验收清单要求的每条事实都写进
//! `ux0:data/yunyin-netprobe.log`：
//!
//! ```text
//! DNS · TLS 握手 · 证书校验 · 302 · Cookie · Referer
//! Range · 206 · Content-Length · Content-Range · 取消耗时 · 内存池用量
//! ```
//!
//! 什么时候跑：只有你要求才跑。两种开关：
//!
//!   * 存在 `ux0:/data/yunyin/netprobe.url`：第一行是 URL、第二行可选 Referer，
//!     探针就只打这个 URL；或者
//!   * 存在 `ux0:/data/yunyin/debug`：跑内置的目标列表。
//!
//! 正常启动时这里什么都不做，所以正式版依旧不联网（Phase 0 想重跑，改那个文件就行）。
//!
//! C 侧通过 `yunyin_net_log` 上报；只有这个模块写报告文件，保证证据集中在一处。
#![allow(dead_code)]

use crate::media::platform::log;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use std::ffi::CString;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const REPORT: &str = "ux0:data/yunyin-netprobe.log";
const URL_FILE: &str = "ux0:data/yunyin/netprobe.url";
const DEBUG_FLAG: &str = "ux0:data/yunyin/debug";

const TLS_DEFAULT: i32 = 0;
const TLS_VERIFY: i32 = 1;

/// 失败发生在哪一步，与 `yhttp_result.err_at` 对应。
fn stage_name(at: i32) -> &'static str {
    match at {
        0 => "init",
        1 => "create",
        2 => "send",
        3 => "status",
        4 => "read",
        5 => "abort",
        _ => "?",
    }
}

/// 探针可能遇到的 Vita 错误码，让日志读起来像诊断而不是十六进制转储。
/// 取值来自 VitaSDK 头文件。
fn error_name(code: i32) -> &'static str {
    match code as u32 {
        0x80431022 => "OUT_OF_MEMORY",
        0x80431068 => "TIMEOUT",
        0x80431080 => "ABORTED",
        0x80431075 => "SSL (handshake/certificate rejected)",
        0x80410104 => "EINTR (socket read interrupted — abort landed)",
        0x80436001 => "RESOLVER_EPACKET",
        0x80436002 => "RESOLVER_ENODNS (DNS)",
        0x80436003 => "RESOLVER_ETIMEDOUT (DNS)",
        0x80436005 => "RESOLVER_EFORMAT",
        0x80436006 => "RESOLVER_ESERVERFAILURE (DNS)",
        0x80436009 => "RESOLVER_ESERVERREFUSED (DNS)",
        0x8043600A => "RESOLVER_ENORECORD (DNS)",
        0x80435022 => "SSL_OUT_OF_MEMORY",
        0x80435108 => "SSL_INVALID_FORMAT",
        0x80435060 => "HTTPS_CERT (certificate rejected)",
        _ => "",
    }
}

#[repr(C)]
struct YhttpResult {
    status: i32,
    tls_mode: i32,
    err_code: i32,
    err_at: i32,
    ssl_error: i32,
    ssl_detail: u32,
    content_length: i64,
    range_total: u64,
    range_start: i64,
    range_end: i64,
    redirected: i32,
    ca_loaded: i32,
    verify_flags: i32,
    http_pool: u32,
    ssl_pool: u32,
    headers_len: i32,
    content_type_audio: i32,
    bytes_read: i32,
    took_ms: u32,
    abort_took_ms: u32,
    aborted: i32,
}

impl Default for YhttpResult {
    fn default() -> Self {
        Self {
            status: 0,
            tls_mode: 0,
            err_code: 0,
            err_at: -1,
            ssl_error: 0,
            ssl_detail: 0,
            content_length: -1,
            range_total: 0,
            range_start: -1,
            range_end: -1,
            redirected: 0,
            ca_loaded: 0,
            verify_flags: 0,
            http_pool: 0,
            ssl_pool: 0,
            headers_len: 0,
            content_type_audio: 0,
            bytes_read: 0,
            took_ms: 0,
            abort_took_ms: 0,
            aborted: 0,
        }
    }
}

extern "C" {
    fn yhttp_set_log(fn_: Option<unsafe extern "C" fn(*const u8, u32)>);
    fn yhttp_init() -> i32;
    fn yhttp_term();
    fn yhttp_online() -> i32;
    fn yhttp_memory(pool: *mut u32, in_use: *mut u32, peak: *mut u32) -> i32;
    fn yhttp_ca_http_pool() -> u32;
    fn yhttp_ca_ssl_pool() -> u32;
    fn yhttp_probe(
        url: *const i8,
        range: *const i8,
        referer: *const i8,
        cookie: *const i8,
        tls_mode: i32,
        auto_redirect: i32,
        out: *mut u8,
        out_cap: i32,
        res: *mut YhttpResult,
    ) -> i32;
    fn yhttp_abort_probe(
        url: *const i8,
        referer: *const i8,
        tls_mode: i32,
        wait_ms: u32,
        res: *mut YhttpResult,
    ) -> i32;
}

static STARTED: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);
static RUNS: AtomicU32 = AtomicU32::new(0);

/* --------------------------------------------------------------- report -- */

fn report(line: &str) {
    /* 和主日志共用一把锁：这份报告也可能被取数线程和探针线程同时写。 */
    let _guard = log::lock();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(REPORT)
    {
        let _ = writeln!(f, "{}", line);
    }
}

/// `yhttp.c` 的日志出口（C 侧不写文件）。
#[no_mangle]
pub unsafe extern "C" fn yunyin_net_log(text: *const u8, len: u32) {
    if text.is_null() || len == 0 {
        return;
    }
    let bytes = core::slice::from_raw_parts(text, len as usize);
    let line = String::from_utf8_lossy(bytes);
    report(line.trim_end());
    log::append(&format!("[net] {}", line.trim_end()));
}

/* -------------------------------------------------------------- helpers -- */

fn magic_of(b: &[u8]) -> &'static str {
    if b.len() >= 4 && &b[0..4] == b"fLaC" {
        "FLAC"
    } else if b.len() >= 3 && &b[0..3] == b"ID3" {
        "MP3(ID3)"
    } else if b.len() >= 2 && b[0] == 0xFF && (b[1] & 0xE0) == 0xE0 {
        "MP3(frame)"
    } else if b.len() >= 4 && &b[0..4] == b"OggS" {
        let probe = &b[..b.len().min(64)];
        if probe.windows(8).any(|w| w == b"OpusHead") {
            "OGG/Opus"
        } else {
            "OGG/Vorbis"
        }
    } else if b.len() >= 12 && &b[4..8] == b"ftyp" {
        "M4A(ftyp)"
    } else if b.len() >= 12 && &b[8..12] == b"WAVE" {
        "WAV"
    } else {
        "?"
    }
}

fn hex4(b: &[u8]) -> String {
    let mut s = String::new();
    for byte in &b[..b.len().min(4)] {
        s.push_str(&format!("{:02X}", byte));
    }
    s
}

fn tls_name(mode: i32) -> &'static str {
    if mode == TLS_VERIFY { "verify" } else { "default" }
}

fn cstr(s: &str) -> Option<CString> {
    CString::new(s).ok()
}

/// 发一次请求，并按 §26 想要的格式写报告。
///
/// `auto_redirect = false` 是**看见** 302 的办法：开启自动重定向时，
/// 库内部就把 302 消化掉了，只能看到最后的 206。
fn attempt(note: &str, url: &str, referer: &str, cookie: &str, tls: i32,
           range: &str, read_cap: usize, auto_redirect: bool, judge: Judge) {
    let mut res = YhttpResult::default();
    let Some(c_url) = cstr(url) else {
        report("NET attempt   URL contains NUL, skipped");
        return;
    };
    let c_range = cstr(range).unwrap_or_default();
    let c_referer = cstr(referer).unwrap_or_default();
    let c_cookie = cstr(cookie).unwrap_or_default();

    report(&format!(
        "NET attempt   [{}] tls={} range={} url={}",
        note,
        tls_name(tls),
        if range.is_empty() { "-" } else { range },
        url
    ));

    let mut buf = vec![0u8; read_cap];
    unsafe {
        yhttp_probe(
            c_url.as_ptr(),
            c_range.as_ptr(),
            c_referer.as_ptr(),
            c_cookie.as_ptr(),
            tls,
            if auto_redirect { 1 } else { 0 },
            buf.as_mut_ptr(),
            buf.len() as i32,
            &mut res,
        );
    }

    let first = &buf[..res.bytes_read.max(0) as usize];
    let (judge_name, pass, why) = match judge {
        Judge::Range206 => (
            "range206",
            res.status == 206 && res.bytes_read > 0,
            if res.status == 206 { "" } else { "expected 206" },
        ),
        Judge::Redirect302 => (
            "redirect302",
            res.status == 302,
            if res.status == 302 { "" } else { "expected a visible 302" },
        ),
        Judge::CertReject => (
            "cert-reject",
            res.status == 0,
            if res.status == 0 {
                "certificate refused (verification is active)"
            } else {
                "certificate ACCEPTED: nothing is being verified"
            },
        ),
        Judge::Anything => ("anything", true, ""),
    };
    report(&format!(
        "NET result    status={} bytes={} cl={} range={}-{}/{} audio={} \
         auto_redirect={} headers={} took={}ms ca={} enable_option={} http_pool={} \
         ssl_pool={}",
        res.status,
        res.bytes_read,
        res.content_length,
        res.range_start,
        res.range_end,
        res.range_total,
        res.content_type_audio,
        if auto_redirect { 1 } else { 0 },
        res.headers_len,
        res.took_ms,
        res.ca_loaded,
        /* sceHttpsEnableOption 成功时返回 0，所以这里打印"结论"而不是那个 0，
         * 否则日志看起来像"什么都没打开"。 */
        if tls == TLS_VERIFY {
            if res.verify_flags >= 0 { "ok" } else { "FAILED" }
        } else {
            "n/a"
        },
        res.http_pool,
        res.ssl_pool
    ));
    if res.bytes_read > 0 {
        report(&format!(
            "NET payload   first={} magic={}",
            hex4(first),
            magic_of(first)
        ));
    }
    if res.err_code != 0 {
        report(&format!(
            "NET error     stage={} code=0x{:08X} {}",
            stage_name(res.err_at),
            res.err_code as u32,
            error_name(res.err_code)
        ));
    }
    if res.ssl_error != 0 || res.ssl_detail != 0 {
        report(&format!(
            "NET tls       ssl_error=0x{:08X} detail=0x{:08X}",
            res.ssl_error as u32, res.ssl_detail
        ));
    }
    report(&format!(
        "NET verdict   [{}] {}={} cert_store={} enable_option={} {} {}",
        note,
        judge_name,
        pass,
        if tls == TLS_VERIFY { if res.ca_loaded != 0 { "loaded" } else { "NOT-LOADED" } } else { "n/a" },
        if tls == TLS_VERIFY {
            if res.verify_flags >= 0 { "ok" } else { "FAILED" }
        } else {
            "n/a"
        },
        if pass { "PASS" } else { "CHECK" },
        why
    ));
}

/// 这个目标是用来证明什么的 —— 302 那一轮不能用 206 去判。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Judge {
    Range206,
    Redirect302,
    /// 故意用坏证书的主机：**被拒绝**才算通过；返回 200 反而说明根本没在校验。
    CertReject,
    Anything,
}

fn abort_attempt(note: &str, url: &str, referer: &str, tls: i32, wait_ms: u32) {
    let mut res = YhttpResult::default();
    let Some(c_url) = cstr(url) else { return };
    let c_referer = cstr(referer).unwrap_or_default();
    report(&format!("NET abort     [{}] url={}", note, url));
    unsafe {
        yhttp_abort_probe(
            c_url.as_ptr(),
            c_referer.as_ptr(),
            tls,
            wait_ms,
            &mut res,
        );
    }
    report(&format!(
        "NET abort     [{}] aborted={} bytes_in_flight={} stop_after_abort={}ms \
         last_read=0x{:08X} {}",
        note,
        res.aborted,
        res.bytes_read,
        res.abort_took_ms,
        res.err_code as u32,
        error_name(res.err_code)
    ));
    report(&format!(
        "NET verdict   [{}] abort {}",
        note,
        if res.aborted != 0 && res.abort_took_ms < 1000 && res.bytes_read > 0 {
            "PASS"
        } else {
            "CHECK"
        }
    ));
}

fn memory_report() {
    let (mut pool, mut used, mut peak) = (0u32, 0u32, 0u32);
    let rc = unsafe { yhttp_memory(&mut pool, &mut used, &mut peak) };
    let (ca_http, ca_ssl) = unsafe { (yhttp_ca_http_pool(), yhttp_ca_ssl_pool()) };
    report(&format!(
        "NET memory    rc={} pool={}B used={}B peak={}B | CA-ready pools: ssl={}B \
         http={}B  (ssl usage figures are in the lines above)",
        rc, pool, used, peak, ca_ssl, ca_http
    ));
}

/* --------------------------------------------------------------- targets -- */

struct Target {
    url: String,
    referer: String,
    note: &'static str,
    tls: i32,
    range: &'static str,
}

/// 内置目标列表。outer/url 这条链在探针出现之前就已经在电脑上验证过：
/// 302 → CDN，匿名可用；两个主机都还能协商上这台机器较老的 SSL 栈。
fn builtin_targets() -> alloc::vec::Vec<Target> {
    vec![
        Target {
            url: String::from("https://music.163.com/favicon.ico"),
            referer: String::new(),
            note: "range/206 on API host",
            tls: TLS_DEFAULT,
            range: "bytes=0-2047",
        },
        Target {
            url: String::from(
                "https://music.163.com/song/media/outer/url?id=3346495279.mp3",
            ),
            referer: String::from("https://music.163.com/"),
            note: "audio via outer/url -> CDN (302)",
            tls: TLS_DEFAULT,
            range: "bytes=0-65535",
        },
        Target {
            url: String::from(
                "https://music.163.com/song/media/outer/url?id=3346495279.mp3",
            ),
            referer: String::from("https://music.163.com/"),
            note: "same chain, certificate verification on",
            tls: TLS_VERIFY,
            range: "bytes=0-65535",
        },
    ]
}

/// `ux0:/data/yunyin/netprobe.url`：第一行 URL，第二行可选 Referer。
fn url_file_target() -> Option<Target> {
    let text = std::fs::read_to_string(URL_FILE).ok()?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let url = lines.next()?.trim().to_string();
    let referer = lines.next().unwrap_or("").trim().to_string();
    Some(Target {
        url,
        referer,
        note: "custom (netprobe.url)",
        tls: TLS_DEFAULT,
        range: "bytes=0-65535",
    })
}

fn probe_enabled() -> bool {
    std::fs::metadata(URL_FILE).is_ok() || std::fs::metadata(DEBUG_FLAG).is_ok()
}

/* ------------------------------------------------------------------ 运行 -- */

fn run() {
    report("=== yunyin Phase 0 network probe ===");
    report(&format!(
        "NET env       init={} online={}",
        unsafe { yhttp_init() },
        unsafe { yhttp_online() }
    ));

    let custom = url_file_target();
    if let Some(t) = custom {
        report("NET target    custom URL from netprobe.url");
        attempt(t.note, &t.url, &t.referer, "", t.tls, t.range, 64 * 1024, true,
                Judge::Anything);
        abort_attempt("custom", &t.url, &t.referer, t.tls, 400);
    } else {
        for t in builtin_targets() {
            attempt(t.note, &t.url, &t.referer, "", t.tls, t.range, 64 * 1024,
                    true, Judge::Range206);
        }
        /* 302 要"看见"而不是"跟随"：关掉自动重定向，
         * 响应本身必须就是带 Location 的 302。 */
        attempt(
            "302 visible (auto-redirect off)",
            "https://music.163.com/song/media/outer/url?id=3346495279.mp3",
            "https://music.163.com/",
            "",
            TLS_DEFAULT,
            "bytes=0-255",
            2048,
            false,
            Judge::Redirect302,
        );
        /* Cookie：无会话时多数歌曲会被拒，
         * 所以探针用一个带标记的请求证明这个头确实发到了服务器。 */
        attempt(
            "cookie header reaches server",
            "https://music.163.com/favicon.ico",
            "https://music.163.com/",
            "YUNYIN_PROBE=1",
            TLS_DEFAULT,
            "bytes=0-255",
            4096,
            true,
            Judge::Range206,
        );
        /*
         * 这台机器到底校不校验证书？
         *
         * sceHttpsLoadCert() 在任何池大小下都装不进那 47 张根证书
         * （2 MiB 的 SceHttp 池也一样 OOM，而它实际只用了 912 字节），
         * 所以"verify 模式"不能想当然地认为有意义。
         * 拿一个自签名证书的主机一试便知：被拒绝 = 有东西在校验；返回 200 = 没校验。
         */
        attempt(
            "bad certificate (self-signed) — refusal = verification works",
            "https://self-signed.badssl.com/",
            "",
            "",
            TLS_VERIFY,
            "bytes=0-255",
            2048,
            false,
            Judge::CertReject,
        );
        attempt(
            "expired certificate — refusal = validity window is checked",
            "https://expired.badssl.com/",
            "",
            "",
            TLS_VERIFY,
            "bytes=0-255",
            2048,
            false,
            Judge::CertReject,
        );
        abort_attempt(
            "outer/url transfer",
            "https://music.163.com/song/media/outer/url?id=3346495279.mp3",
            "https://music.163.com/",
            TLS_DEFAULT,
            400,
        );
    }

    memory_report();
    report("=== probe finished ===");
}

/// 放在独立线程里跑：绝不能拖住界面，而且慢接入点上一次请求要好几秒。
fn spawn_run() {
    if RUNNING.swap(true, Ordering::AcqRel) {
        return; /* already probing */
    }
    RUNS.fetch_add(1, Ordering::AcqRel);
    let _ = std::thread::Builder::new()
        .name("yunyin-netprobe".into())
        .stack_size(96 * 1024)
        .spawn(|| {
            unsafe { yhttp_set_log(Some(yunyin_net_log)) };
            run();
            log::append(&format!("net: Phase 0 report -> {REPORT}"));
            RUNNING.store(false, Ordering::Release);
        });
}

/// 启动路径：只跑一次，而且只有卡里明确要求时才跑。
pub fn start_once() {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    if !probe_enabled() {
        STARTED.store(false, Ordering::Release);
        return;
    }
    spawn_run();
}

/// `vitaMedia.netProbe()`：不管开关文件，立刻跑一次，这样 Phase 0 不用重启也能重测。
/// 上一次还没跑完时的重复调用会被忽略。
pub fn run_now() {
    spawn_run();
}

/// 给界面 / `vitaMedia.netProbe()` 看的简短状态；详细内容在报告文件里。
pub fn state_json() -> String {
    format!(
        "{{\"enabled\":{},\"started\":{},\"running\":{},\"runs\":{},\"report\":\"{}\"}}",
        probe_enabled(),
        STARTED.load(Ordering::Acquire),
        RUNNING.load(Ordering::Acquire),
        RUNS.load(Ordering::Acquire),
        REPORT
    )
}
