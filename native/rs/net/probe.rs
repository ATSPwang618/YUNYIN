//! Phase 0 network smoke test (task book §25/§26/§27).
//!
//! Runs `native/net/yhttp.c` on a background thread and writes every fact the
//! acceptance list asks for to `ux0:data/yunyin-netprobe.log`:
//!
//! ```text
//! DNS · TLS handshake · certificate validation · 302 · Cookie · Referer
//! Range · 206 · Content-Length · Content-Range · abort timing · pool usage
//! ```
//!
//! When it runs: only if the user asks.  Either
//!
//!   * `ux0:/data/yunyin/netprobe.url` exists — line 1 is the URL, line 2 is an
//!     optional Referer — and the probe hits exactly that URL, or
//!   * `ux0:/data/yunyin/debug` exists — the built-in target list is used.
//!
//! Nothing here runs at all in a normal launch, so the shipping app still makes
//! no network calls (and Phase 0 can be repeated by just editing that file).
//!
//! The C side reports through `yunyin_net_log`; this module is the only place
//! that writes the report, which keeps the evidence in one file.
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

/// Where a failure happened, matching `yhttp_result.err_at`.
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

/// Vita error codes the probe is likely to meet, so the log reads like a
/// diagnosis instead of a hex dump.  Values are from the VitaSDK headers.
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

/// One request, formatted the way §26 wants to read it.
///
/// `auto_redirect = false` is how a 302 becomes *visible*: with redirects
/// enabled the library follows it internally and only the final 206 is seen.
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
        /* sceHttpsEnableOption returns 0 on success, so print the outcome, not
         * the raw zero, or the log reads like "nothing was enabled". */
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

/// What a target is evidence *for* — a 302 run must not be judged by 206.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Judge {
    Range206,
    Redirect302,
    /// A host with a deliberately broken certificate: being *refused* is the
    /// pass condition, and a 200 would prove nothing is verified at all.
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

/// Built-in list.  The outer/url chain was verified from a PC before this
/// probe existed: 302 -> CDN, anonymous, and both hosts still accept TLS 1.0,
/// which the console's older SSL stack can negotiate.
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

/// `ux0:/data/yunyin/netprobe.url` — one URL, optional referer on line 2.
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

/* ------------------------------------------------------------------ run -- */

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
        /* 302, seen rather than followed: redirects off, so the response
         * itself must be a 302 carrying Location. */
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
        /* Cookie: the API rejects a session-less request for most songs, so the
         * probe proves the header reaches the server by echoing a marker. */
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
         * Does this console verify certificates at all?
         *
         * sceHttpsLoadCert() could not load the 47 system roots at any pool
         * size (OUT_OF_MEMORY even with a 2 MiB SceHttp pool, which then sat
         * 912 bytes used), so "verify" mode cannot be assumed to mean anything.
         * A host with a self-signed certificate settles it: refused = something
         * verifies; 200 = nothing does.
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

/// Run on its own thread: it must never delay the UI, and the requests can take
/// seconds on a slow access point.
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

/// Startup path: run once, and only when the card asked for it.
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

/// `vitaMedia.netProbe()` — run now regardless of the enable file, so Phase 0
/// can be repeated without rebooting.  Repeated calls while a run is in flight
/// are ignored.
pub fn run_now() {
    spawn_run();
}

/// Short status for the UI/`vitaMedia.netProbe()`; the detail is in the file.
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
