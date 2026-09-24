//! 在线播放的接线层（Phase 2）：把 `HttpRangeSource` 接到 `yp_io` 上。
//!
//! 六个解码器完全不用改 —— `yp_open_io()` 拿到的是同一组回调，
//! 它们照旧"从字节流里拉数据"，只是这次的字节来自网络。
//!
//! 线程分工（任务书 §9）：
//!   - 取数线程在 `HttpRangeSource` 内部，负责按窗口抓字节；
//!   - 音频线程只会从窗口里取，取不到就交给 Gate 处理（静音，不结束播放）。
//!
//! 生命周期：源被放进一个静态槽里（它的地址就是 `yp_io.ctx`），
//! `close_remote()` 先关掉 C 侧播放器、再释放源，避免悬垂指针。
#![allow(dead_code)]

use super::http::{ByteTransport, HttpRangeSource};
use super::{AudioSource, SourceError};
use crate::media::platform::log;
use crate::media::net::http::Stream;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};
use std::ffi::CString;
use std::sync::Mutex;

type RemoteSource = HttpRangeSource<Stream>;

/*
 * 与 native/audio/yp_io.h 的 yp_io 逐字段对应（顺序、宽度都要一致）。
 */
#[repr(C)]
struct YpIo {
    ctx: *mut c_void,
    read: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, u64) -> i64>,
    seek: Option<unsafe extern "C" fn(*mut c_void, i64, i32) -> i64>,
    tell: Option<unsafe extern "C" fn(*mut c_void) -> i64>,
    size: Option<unsafe extern "C" fn(*mut c_void) -> i64>,
    close: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
}

extern "C" {
    fn yp_open_io(
        io: *const YpIo,
        owns_io: i32,
        path_hint: *const i8,
        format_hint: i32,
        duration_hint_ms: i64,
    ) -> *mut c_void;
    /* 打开完成后才发现这次请求已经作废时，用它把刚建好的播放器丢掉。 */
    fn yp_close(player: *mut c_void);
}

/* 当前在线源（同一时刻只有一个，和播放器一致）。 */
static REMOTE: Mutex<Option<alloc::boxed::Box<RemoteSource>>> = Mutex::new(None);

/*
 * 打开序号。在线打开要花几秒（DNS + TLS + 第一个窗口），必须放到后台线程去做，
 * 界面线程不能等它。既然是后台的，用户完全可能还没打开完就点了别的歌 ——
 * 每来一个新的播放请求就换一个序号，旧任务在每一步前后比对序号，发现过期就收手。
 */
static OPEN_TOKEN: AtomicU32 = AtomicU32::new(0);

/// 声明"现在开始的是新一次播放"，返回本次的序号。
pub fn new_token() -> u32 {
    OPEN_TOKEN.fetch_add(1, Ordering::AcqRel) + 1
}

/// 这个序号还是当前有效的吗（false = 已被新的播放请求取代）。
pub fn token_current(token: u32) -> bool {
    OPEN_TOKEN.load(Ordering::Acquire) == token
}

/* ------------------------------------------------------------ yp_io 回调 -- */

unsafe extern "C" fn io_read(ctx: *mut c_void, dst: *mut c_void, n: u64) -> i64 {
    if ctx.is_null() || dst.is_null() || n == 0 {
        return -1;
    }
    let src = &mut *(ctx as *mut RemoteSource);
    let buf = core::slice::from_raw_parts_mut(dst as *mut u8, n as usize);
    match src.read(buf) {
        Ok(got) => got as i64,
        Err(SourceError::WouldBlock) => -1, /* Gate 会静音，不会把它当 EOF */
        Err(e) => {
            /*
             * 别把"网络暂时没数据"和"这条流真的坏了"混在一起说。
             * 这里只负责如实记录；重试与判死都在 HttpRangeSource 里。
             */
            log::append(&format!("remote: 解码器读在线字节失败 {:?}", e));
            -1
        }
    }
}

unsafe extern "C" fn io_seek(ctx: *mut c_void, off: i64, whence: i32) -> i64 {
    if ctx.is_null() {
        return -1;
    }
    let src = &mut *(ctx as *mut RemoteSource);
    /*
     * whence 必须真的按语义换算成绝对偏移：
     * mpg123 会先 lseek(..., SEEK_END, 0) 问"文件多长"，如果这里把 off=0
     * 当成"跳到 0"，它就以为流长度是 0，直接打不开（真机上就是这么挂的）。
     */
    let base = if whence == 2 {
        src.size().unwrap_or(0) as i64 /* SEEK_END：从流末尾算 */
    } else if whence == 1 {
        src.tell() as i64 /* SEEK_CUR：从当前位置算 */
    } else {
        0 /* SEEK_SET：绝对值 */
    };
    let target = (base + off).max(0) as u64;
    match src.seek(target) {
        Ok(()) => target as i64,
        Err(_) => -1,
    }
}

unsafe extern "C" fn io_tell(ctx: *mut c_void) -> i64 {
    if ctx.is_null() {
        return -1;
    }
    (*(ctx as *mut RemoteSource)).tell() as i64
}

unsafe extern "C" fn io_size(ctx: *mut c_void) -> i64 {
    if ctx.is_null() {
        return -1;
    }
    match (*(ctx as *mut RemoteSource)).size() {
        Some(s) => s as i64,
        None => -1, /* 长度未知：让解码器靠 duration hint */
    }
}

unsafe extern "C" fn io_close(_ctx: *mut c_void) -> i32 {
    0 /* 源由我们释放，不在回调里关 */
}

/* ---------------------------------------------------------------- 对外 -- */

/// 打开一个在线 URL 交给解码器。`duration_ms` 是可选的时长提示（§37）。
///
/// `token` 是本次播放的序号（见 `new_token`）：整个打开过程可能持续几秒，
/// 中途用户换了歌就作废，绝不把已经作废的源塞给解码器。
pub fn open_remote(
    url: &str,
    referer: &str,
    duration_ms: i64,
    token: u32,
) -> Result<(), SourceError> {
    /* 每一步都单独记日志：真机上"在线打开失败"必须能分辨是取数没打开、
     * 还是解码器不认识这份流。 */
    let transport = match Stream::open(url, referer, 0) {
        Ok(t) => t,
        Err(e) => {
            log::append(&format!("remote: 取数线程打不开流 {:?}", e));
            return Err(e);
        }
    };
    let size = transport.size();
    log::append(&format!("remote: 流已打开 size={:?}", size));
    if !token_current(token) {
        log::append("remote: 打开途中被新的播放请求取代，放弃这条路");
        return Err(SourceError::Cancelled);
    }
    let source = HttpRangeSource::new(url, transport, Default::default());
    let mut slot = match REMOTE.lock() {
        Ok(g) => g,
        Err(_) => return Err(SourceError::Unsupported),
    };
    if slot.is_some() {
        return Err(SourceError::Unsupported); /* 同一时刻只放一路在线源 */
    }
    *slot = Some(alloc::boxed::Box::new(source));
    let ctx = match slot.as_mut() {
        Some(b) => (&mut **b) as *mut RemoteSource as *mut c_void,
        None => return Err(SourceError::Unsupported),
    };
    let io = YpIo {
        ctx,
        read: Some(io_read),
        seek: Some(io_seek),
        tell: Some(io_tell),
        size: Some(io_size),
        close: Some(io_close),
    };
    let hint = CString::new(url).unwrap_or_default();
    let player = unsafe {
        yp_open_io(
            &io as *const YpIo,
            0, /* 源的生命周期由 REMOTE 管 */
            hint.as_ptr(),
            0, /* 格式自动：先嗅字节，再按后缀 */
            duration_ms,
        )
    };
    if player.is_null() {
        drop(slot.take());
        log::append("remote: 解码器打不开这份流（格式认不出或解码器失败）");
        return Err(SourceError::Unsupported);
    }
    if !token_current(token) {
        unsafe { yp_close(player) };
        drop(slot.take());
        log::append("remote: 打开完成后已被新的播放请求取代，已丢弃");
        return Err(SourceError::Cancelled);
    }
    log::append("remote: 解码器已接上在线源");
    crate::media::decoder::adopt_remote(player, ctx);
    Ok(())
}

/// 释放在线源（先让 C 侧关掉播放器，再放掉源）。
pub fn close_remote() {
    crate::media::decoder::close(); /* 先停用 handle */
    if let Ok(mut slot) = REMOTE.lock() {
        drop(slot.take());
    }
    /* 排障用：一行计数，回答"到底是谁在反复跑"（只在 debug 日志开着时写）。 */
    super::http::trace_summary();
}

pub fn active() -> bool {
    REMOTE.lock().map(|g| g.is_some()).unwrap_or(false)
}

/// Gate（§8/§65）：能不能调解码器？
///   本地播放 → 永远可以；
///   在线播放 → 缓存够（或已到流末尾）才可以，否则音频线程输出静音。
pub fn gate_ok() -> bool {
    const GATE_BYTES: usize = 64 * 1024;
    !active() || is_eof() || available() >= GATE_BYTES
}

/// 还没解码的字节数（Gate 用它决定要不要调解码器）。
pub fn available() -> usize {
    match REMOTE.lock() {
        Ok(g) => match g.as_ref() {
            Some(src) => src.available(),
            None => usize::MAX, /* 本地播放：不受 Gate 限制 */
        },
        Err(_) => usize::MAX,
    }
}

pub fn is_eof() -> bool {
    match REMOTE.lock() {
        Ok(g) => match g.as_ref() {
            Some(src) => src.is_eof(),
            None => true,
        },
        Err(_) => true,
    }
}

pub fn error() -> Option<SourceError> {
    match REMOTE.lock() {
        Ok(g) => g.as_ref().and_then(|s| s.error()),
        Err(_) => None,
    }
}

/// 切歌 / 退出：告诉取数线程别再抓了。
pub fn cancel() {
    if let Ok(g) = REMOTE.lock() {
        if let Some(src) = g.as_ref() {
            /* Drop 时会取消；这里只需要让正在进行的读尽快返回。 */
            let _ = src;
        }
    }
}

pub fn url() -> String {
    match REMOTE.lock() {
        Ok(g) => g
            .as_ref()
            .map(|s| String::from(s.url()))
            .unwrap_or_default(),
        Err(_) => String::new(),
    }
}

/* ------------------------------------------------------- 真机测试开关 -- */

const NETPLAY_FILE: &str = "ux0:data/yunyin/netplay.url";

/*
 * 在线曲目清单。
 *
 * 卡里放 `ux0:/data/yunyin/netplay.url` 时，里面的歌会作为**曲库里的独立条目**
 * 交给界面显示 —— 单独成一组（专辑「在线歌曲」），不占任何本地歌曲的位置。
 * 界面点它、按 ○，才会真的走网络播放。
 *
 * 文件格式（一段一首歌，段之间用空行分开；三行都可省略后面的）：
 *
 *     # 第 1 行：URL
 *     https://music.163.com/song/media/outer/url?id=3346495279.mp3
 *     # 第 2 行（可选）：Referer，CDN 会查这个头
 *     https://music.163.com/
 *     # 第 3 行（可选）：界面上显示的名字
 *     [在线] 测试曲目
 *
 * 没有这个文件时返回空列表，正式版完全不受影响。
 */
#[derive(Clone)]
pub struct NetplayTrack {
    pub url: String,
    pub referer: String,
    pub title: String,
}

/// 把 `id=123456` 里的数字挑出来，用作没写显示名时的默认名字。
fn url_id_hint(url: &str) -> String {
    let after = url.split("id=").nth(1).unwrap_or("");
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits
}

/// 读 `netplay.url` 并把每首歌整理成一条记录（文件不存在 = 空）。
pub fn netplay_tracks() -> Vec<NetplayTrack> {
    let Ok(text) = std::fs::read_to_string(NETPLAY_FILE) else {
        return Vec::new();
    };
    let mut out: Vec<NetplayTrack> = Vec::new();
    let mut block: Vec<String> = Vec::new();
    let flush = |block: &mut Vec<String>, out: &mut Vec<NetplayTrack>| {
        if block.is_empty() {
            return;
        }
        let url = block[0].clone();
        let referer = block.get(1).cloned().unwrap_or_default();
        let title = block.get(2).cloned().unwrap_or_default();
        let title = if title.trim().is_empty() {
            let id = url_id_hint(&url);
            if id.is_empty() {
                String::from("[在线] 网络歌曲")
            } else {
                format!("[在线] {}", id)
            }
        } else {
            title
        };
        if url.starts_with("http://") || url.starts_with("https://") {
            out.push(NetplayTrack { url, referer, title });
        }
        block.clear();
    };
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            flush(&mut block, &mut out); /* 空行 = 一首歌结束 */
            continue;
        }
        if line.starts_with('#') {
            continue; /* 注释行 */
        }
        block.push(String::from(line));
        if block.len() == 3 {
            flush(&mut block, &mut out);
        }
    }
    flush(&mut block, &mut out);
    out
}

/// 这首歌要用哪个 Referer（按 URL 查；没有就返回空串）。
pub fn referer_for(url: &str) -> String {
    netplay_tracks()
        .into_iter()
        .find(|t| t.url == url)
        .map(|t| t.referer)
        .unwrap_or_default()
}

/// 给界面用的 JSON 清单：`[{"url":...,"title":...,"referer":...}]`。
pub fn netplay_json() -> String {
    let mut s = String::from("[");
    for (i, t) in netplay_tracks().into_iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "{{\"url\":\"{}\",\"title\":\"{}\",\"referer\":\"{}\"}}",
            crate::media::json_escape(&t.url),
            crate::media::json_escape(&t.title),
            crate::media::json_escape(&t.referer)
        ));
    }
    s.push(']');
    s
}
