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
use core::ffi::c_void;
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
}

/* 当前在线源（同一时刻只有一个，和播放器一致）。 */
static REMOTE: Mutex<Option<alloc::boxed::Box<RemoteSource>>> = Mutex::new(None);

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
            log::append(&format!("remote: read 失败 {:?}", e));
            -1
        }
    }
}

unsafe extern "C" fn io_seek(ctx: *mut c_void, off: i64, _whence: i32) -> i64 {
    if ctx.is_null() {
        return -1;
    }
    let src = &mut *(ctx as *mut RemoteSource);
    match src.seek(off.max(0) as u64) {
        Ok(()) => off.max(0),
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
pub fn open_remote(url: &str, referer: &str, duration_ms: i64) -> Result<(), SourceError> {
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

/// 卡里放 `ux0:/data/yunyin/netplay.url`（第一行 URL，第二行可选 Referer）时，
/// 启动就试着播放它 —— Phase 2 的真机验收开关，正式版没有这个文件就不起作用。
pub fn maybe_autoplay() {
    let Ok(text) = std::fs::read_to_string(NETPLAY_FILE) else {
        return;
    };
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let Some(url) = lines.next().map(|s| s.trim()) else {
        return;
    };
    let referer = lines.next().unwrap_or("").trim();
    log::append(&format!("remote: netplay.url -> {}", url));
    crate::media::bgm::play_url(url, referer, -1);
}
