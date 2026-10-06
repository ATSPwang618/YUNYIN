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
use super::diskcache;
use super::{AudioSource, SourceError};
use crate::media::platform::log;
use crate::media::net::http::Stream;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::ffi::CString;
use std::sync::Mutex;

use super::policy::{self, GateDecision, GateState, NetState};

type RemoteSource = HttpRangeSource<CacheTransport<Stream>>;

/*
 * 秒级 Gate 的状态（一份，跟着当前在线源走）。
 *
 * 这里只放**策略输入**：码率（由 size + duration 推）、实测下载速度（滑动平均）、
 * 以及 Gate 自己的迟滞状态机。所有阈值都在 policy.rs 里，宿主上测过。
 */
static BITRATE_BPS: AtomicU32 = AtomicU32::new(0);
static SPEED_BPS: AtomicU64 = AtomicU64::new(0);
static SAMPLE_AT_MS: AtomicU64 = AtomicU64::new(0);
static SAMPLE_BYTES: AtomicU64 = AtomicU64::new(0);
static GATE: Mutex<GateState> = Mutex::new(GateState::new());

/// 取数层外面套的一层缓存（见 source/diskcache.rs 的设计说明）：
///
/// * **命中**（这首歌完整下过一次）：完全不碰网络，直接从 `cache.dat` 读；
/// * **未命中**：照旧走网络，顺手把"顺序下到的"字节写进 `cache.dat`。
///
/// 解码器探测文件尾巴时会乱序读，那种字节不写（`Writer::accepts`），
/// 免得把缓存写成中间带洞的碎片。
struct CacheTransport<T: ByteTransport + 'static> {
    net: Option<T>,
    disk: Option<diskcache::Reader>,
    writer: Option<diskcache::Writer<'static>>,
}

impl<T: ByteTransport + 'static> CacheTransport<T> {
    fn from_disk(reader: diskcache::Reader) -> Self {
        Self {
            net: None,
            disk: Some(reader),
            writer: None,
        }
    }

    fn from_net(net: T, key: &str, total: u64) -> Self {
        let writer = if total > 0 {
            diskcache::global().begin_write(key, total)
        } else {
            None /* 长度未知（没有 Content-Length）：不缓存，免得写出半截 */
        };
        Self {
            net: Some(net),
            disk: None,
            writer,
        }
    }
}

impl<T: ByteTransport + 'static> ByteTransport for CacheTransport<T> {
    fn read_at(&mut self, off: u64, dst: &mut [u8]) -> Result<usize, SourceError> {
        if let Some(reader) = self.disk.as_mut() {
            return reader.read_at(off, dst).map_err(SourceError::Network);
        }
        let Some(net) = self.net.as_mut() else {
            return Ok(0);
        };
        let n = net.read_at(off, dst)?;
        if n > 0 {
            if let Some(w) = self.writer.as_mut() {
                if w.accepts(off) {
                    w.append(&dst[..n]);
                }
                if w.is_full() {
                    if let Some(w) = self.writer.take() {
                        w.finish();
                    }
                }
            }
        }
        Ok(n)
    }

    fn size(&self) -> Option<u64> {
        if let Some(reader) = &self.disk {
            return Some(reader.size());
        }
        self.net.as_ref().and_then(|n| n.size())
    }
}

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
 * `yp_io` 回调表必须**一直活着**，直到解码器被关掉。
 *
 * 解码器会把传进去的 `const yp_io *` 原样存进自己的状态（C 侧本地播放用的是
 * `static yp_io file_io;`，就是这个道理）。以前这里直接在 open_remote() 里建了个
 * 局部 `YpIo` 传指针 —— 函数一返回栈帧就没了，解码器手里就是野指针。打开阶段它
 * 不需要读数据所以看不出问题，等播放中解码器再要数据时，读回调里的 io->ctx 已经是
 * 垃圾，于是访问野地址崩溃（真机 dump：PC 落在 mp3_io_read，DFAR 是个堆地址）。
 * 所以这里用静态槽保活，close_remote() 关掉解码器之后再释放。
 */
/*
 * YpIo 里装的是函数指针与 ctx 裸指针（回调表本身就该是裸的），所以包一层显式声明
 * "这份东西由我们自己保证跨线程使用是安全的"——它只在拿锁的代码里被读写。
 */
struct IoKeepAlive(alloc::boxed::Box<YpIo>);
unsafe impl Send for IoKeepAlive {}

static IO_SLOT: Mutex<Option<IoKeepAlive>> = Mutex::new(None);

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

/// 当前**播放世代**（只读）。
///
/// 这就是全链路唯一的"播放世代"来源：预取、缓存写入等后台任务每一步都拿它比对，
/// 切歌/停止本身就让旧任务失效 —— 不依赖任何调用方记得去取消。
pub fn generation() -> u32 {
    OPEN_TOKEN.load(Ordering::Acquire)
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
             *
             * 注意用 trace_f：播放阶段（音频线程）**不拼字符串、不写文件** ——
             * 解码回调里做这些真的崩过一次；那时候只记数，收尾时统计一行。
             */
            super::http::trace_f(|| format!("remote: 解码器读在线字节失败 {:?}", e));
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
    cookie: &str,
    duration_ms: i64,
    token: u32,
) -> Result<(), SourceError> {
    /* 每一步都单独记日志：真机上"在线打开失败"必须能分辨是取数没打开、
     * 还是解码器不认识这份流。
     *
     * 顺序有讲究：**先看磁盘缓存**，命中就一个网络请求都不发（同一个 key 是
     * 同一首歌，换过一次 CDN 地址也认）。 */
    let key = diskcache::key_for_url(url);
    let cache = diskcache::global();
    /* 登记"正在播的那首"：环形缓存要盖到它时会主动放弃这次缓存写入。 */
    diskcache::set_protected(&key);
    let transport = match cache
        .lookup(&key)
        .and_then(|(off, len, total)| cache.open_reader(off, len, total))
    {
        Some(reader) => {
            log::append(&format!(
                "remote: 命中磁盘缓存 {}（{} KB，零网络请求）",
                key,
                reader.size() / 1024
            ));
            /* 整首都在盘上 = 缓冲条直接拉满 */
            let total = reader.size();
            super::http::buf_mark_all(total);
            CacheTransport::from_disk(reader)
        }
        None => {
            let stream = match Stream::open(url, referer, cookie, 0) {
                Ok(t) => t,
                Err(e) => {
                    log::append(&format!("remote: 取数线程打不开流 {:?}", e));
                    return Err(e);
                }
            };
            let size = stream.size();
            log::append(&format!(
                "remote: 流已打开 size={:?}（key={}，Cookie {} 字节）",
                size,
                key,
                cookie.len()
            ));
            CacheTransport::from_net(stream, &key, size.unwrap_or(0))
        }
    };
    if !token_current(token) {
        log::append(&format!(
            "remote: 打开途中被新的播放请求取代（generation={token}，当前 {}），放弃这条路",
            generation()
        ));
        return Err(SourceError::Cancelled);
    }
    /*
     * 秒级 Gate 的输入全部从这一首重新开始：码率（size + duration 推）、
     * 速度采样、迟滞状态机。不重置的话上一首的"已经播过"状态会漏到新歌上 ——
     * 新歌刚从 0 开始缓冲，Gate 却以为早就开播了，于是立刻放行、一开就卡。
     */
    let total_bytes = transport.size().unwrap_or(0);
    let bps = policy::bitrate_from_size_duration(total_bytes, duration_ms.max(0) as u64)
        .unwrap_or(0);
    BITRATE_BPS.store(bps, Ordering::Release);
    SPEED_BPS.store(0, Ordering::Release);
    SAMPLE_AT_MS.store(0, Ordering::Release);
    SAMPLE_BYTES.store(0, Ordering::Release);
    if let Ok(mut g) = GATE.lock() {
        *g = GateState::new();
    }
    /*
     * 预读预算：整首的 1/3，夹在 2–6 MiB（policy::readahead_budget）。
     *
     * 真机 2026-10-06 反馈："每次只缓存一小点，播几秒又要缓冲" —— 只加粗单个窗口
     * 治不了本（抓完一窗取数线程还是闲着），所以改成"滚动预读队列 + 预算"：
     * 取数线程只要提前量没到预算就一直往后抓，播到后面自己接着补剩下的。
     * 这里把算出来的数写进日志，真机上对照"缓冲不足/恢复"就能判断预算够不够。
     */
    let budget = policy::readahead_budget(if total_bytes > 0 { Some(total_bytes) } else { None });
    log::append(&format!(
        "remote: 预读预算 {} KB（整首 {} KB 的 1/3，夹在 {}-{} KB；播到后面继续补）",
        budget / 1024,
        total_bytes / 1024,
        policy::READAHEAD_MIN_BYTES / 1024,
        policy::READAHEAD_MAX_BYTES / 1024
    ));
    let source = HttpRangeSource::new(url, transport, budget);
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
    let io = alloc::boxed::Box::new(YpIo {
        ctx,
        read: Some(io_read),
        seek: Some(io_seek),
        tell: Some(io_tell),
        size: Some(io_size),
        close: Some(io_close),
    });
    /*
     * 先把上一份回调表丢掉（它的解码器已经在 session_end() 里关过了），
     * 再把这一份放进静态槽 —— 解码器只拿指针，所以这份表必须活到 yp_close()。
     */
    if let Ok(mut slot) = IO_SLOT.lock() {
        *slot = Some(IoKeepAlive(io));
    }
    let io_ptr: *const YpIo = match IO_SLOT.lock() {
        Ok(g) => match g.as_ref() {
            Some(b) => &*b.0 as *const YpIo,
            None => core::ptr::null(),
        },
        Err(_) => core::ptr::null(),
    };
    if io_ptr.is_null() {
        drop(slot.take());
        clear_io_slot();
        log::append("remote: 回调表保活失败，放弃这次打开");
        return Err(SourceError::Unsupported);
    }
    let hint = CString::new(url).unwrap_or_default();
    let player = unsafe {
        yp_open_io(
            io_ptr,
            0, /* 源的生命周期由 REMOTE 管 */
            hint.as_ptr(),
            0, /* 格式自动：先嗅字节，再按后缀 */
            duration_ms,
        )
    };
    if player.is_null() {
        drop(slot.take());
        clear_io_slot();
        log::append("remote: 解码器打不开这份流（格式认不出或解码器失败）");
        return Err(SourceError::Unsupported);
    }
    if !token_current(token) {
        unsafe { yp_close(player) };
        drop(slot.take());
        clear_io_slot();
        log::append("remote: 打开完成后已被新的播放请求取代，已丢弃");
        return Err(SourceError::Cancelled);
    }
    log::append("remote: 解码器已接上在线源");
    crate::media::decoder::adopt_remote(player, ctx);
    /*
     * 从这里开始进入播放阶段：解码器的每一次读都发生在音频线程的回调里，
     * 那些地方**绝不能写日志**（真机上崩过一次，栈顶就是 Rust 的字符串格式化）。
     * 逐条轨迹到此为止，只留原子计数。
     */
    super::http::trace_off();
    Ok(())
}

/// 释放在线源（先让 C 侧关掉播放器，再放掉源）。
pub fn close_remote() {
    crate::media::decoder::close(); /* 先停用 handle */
    let taken = match REMOTE.lock() {
        Ok(mut slot) => slot.take(),
        Err(_) => None,
    };
    /*
     * 解码器已经关了，回调表才可以丢 —— 顺序不能反：
     * 反了就等于把解码器脚下的表抽掉（这正是之前真机崩的原因）。
     */
    clear_io_slot();
    /*
     * 源的析构会 join 取数线程 —— 它可能正卡在一次网络读 + 退避重试里
     * （实测连续失败重试要 1~3 秒，网络差时更久）。以前这一步在界面线程上做，
     * 真机 0.13 的表现就是"点歌后卡死、歌还在唱"：整帧被拖过 2 秒看门狗预算，
     * guest 直接被宿主打死（E:\data\yunyin.log 最后一行停在 bgm: play）。
     * 所以真正的析构交给后台线程，界面线程立刻返回。
     * 取数线程只碰它自己的 Arc<Window>，跟全局槽无关，跨线程丢掉是安全的。
     */
    if let Some(source) = taken {
        let spawned = std::thread::Builder::new()
            .name("yunyin-src-drop".into())
            .stack_size(64 * 1024)
            .spawn(move || drop(source));
        if spawned.is_err() {
            /* 线程建不出来：闭包（连同源）已经在 spawn 失败时就地析构了，
             * 回到老行为 —— 慢一点，但不泄漏。 */
            log::append("remote: 源回收线程创建失败，就地等待取数线程退出");
        }
    }
    /* 排障用：一行计数，回答"到底是谁在反复跑"（只在 debug 日志开着时写）。 */
    super::http::trace_summary();
}

fn clear_io_slot() {
    if let Ok(mut g) = IO_SLOT.lock() {
        *g = None;
    }
}

pub fn active() -> bool {
    REMOTE.lock().map(|g| g.is_some()).unwrap_or(false)
}

/// 现在可播多少毫秒（字节数 ÷ 码率；码率未知时用 192 kbps 兜底）。
///
/// 这是整套缓冲策略的核心输入：启动要 10 秒、维持下限 5 秒、恢复 8 秒。
pub fn buffer_ms() -> u64 {
    if !active() {
        return u64::MAX; /* 本地播放：不受 Gate 限制 */
    }
    policy::buffer_ms(
        available() as u64,
        BITRATE_BPS.load(Ordering::Acquire),
        policy::FALLBACK_BITRATE_BPS,
    )
}

/// 实测下载速度（滑动平均，字节/秒）。音频线程每次过 Gate 都会喂一次采样。
fn sample_speed(bytes: u64, now_ms: u64) {
    let last_at = SAMPLE_AT_MS.swap(now_ms, Ordering::AcqRel);
    let last_bytes = SAMPLE_BYTES.swap(bytes, Ordering::AcqRel);
    if last_at == 0 || now_ms <= last_at {
        return;
    }
    let dt = now_ms - last_at;
    if dt < 1000 {
        return; /* 采样窗口太短，噪声大：不更新 */
    }
    let delta = bytes.saturating_sub(last_bytes);
    let inst = delta.saturating_mul(1000) / dt;
    /* 指数滑动平均（1/4 新值）：瞬时速度会一跳一跳，不能拿它做决策。 */
    let prev = SPEED_BPS.load(Ordering::Acquire);
    /* 下载是"一窗一窗拿到"的（2 MiB 一大笔），所以平滑要重一点，别让瞬时值乱跳。 */
    let next = if prev == 0 {
        inst
    } else {
        (prev.saturating_mul(7) + inst) / 8
    };
    SPEED_BPS.store(next, Ordering::Release);
}

/// 当前网络相对码率的分级（预取让路用它）。
pub fn net_state() -> NetState {
    policy::classify(
        SPEED_BPS.load(Ordering::Acquire),
        BITRATE_BPS.load(Ordering::Acquire),
    )
}

/// 预取下一首是否合适：缓冲已经比较足、而且网络有余量。
///
/// 规矩：**当前曲永远优先**。缓冲不到 TARGET、或网络不是 FAST，就干脆别预取。
pub fn prefetch_ok() -> bool {
    if !active() {
        return false; /* 本地播放不需要预取 */
    }
    if at_cached_end() {
        return true; /* 当前曲已经全部到手：带宽空着也是空着 */
    }
    buffer_ms() >= policy::TARGET_BUFFER_MS && net_state() == NetState::Fast
}

/// Gate（§8/§65）：能不能调解码器？
///   本地播放 → 永远可以；
///   在线播放 → 缓冲够（或已到流末尾）才可以，否则音频线程输出静音。
///
/// 判据从"固定 64 KB"换成了**秒数 + 迟滞**（policy::GateState）：
/// 启动要 10 秒、播放中掉到 5 秒以下才 rebuffer、恢复到 8 秒才继续。
pub fn gate_ok() -> bool {
    if !active() {
        return true; /* 本地播放不受 Gate 限制 */
    }
    /*
     * 测速用**累计下载字节**，不是"窗口里剩多少"。
     *
     * 以前拿 `available()` 的增量估速：解码器同时在消费，稳态下增量≈0，
     * 于是真机日志里网络永远显示 `Starving`（假的），预取也因此一直被拦。
     */
    let fetched = match REMOTE.lock() {
        Ok(g) => g.as_ref().map(|s| s.fetched_total()).unwrap_or(0),
        Err(_) => 0,
    };
    sample_speed(fetched, crate::media::platform::time::now_ms());
    let ms = buffer_ms();
    let at_end = is_eof() || at_cached_end();
    let mut gate = match GATE.lock() {
        Ok(g) => g,
        Err(_) => return true, /* 锁坏了也不能把播放卡死 */
    };
    matches!(gate.decide(ms, at_end), GateDecision::Play)
}

/// 缓存是否已经接到已知的流末尾（Gate 判"末尾放行"用）。
pub fn at_cached_end() -> bool {
    match REMOTE.lock() {
        Ok(g) => match g.as_ref() {
            Some(src) => src.at_cached_end(),
            None => true,
        },
        Err(_) => true,
    }
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

/// 主动让取数线程去补数据（Gate 决定静音时调用，见 `HttpRangeSource::prime`）。
pub fn prime() {
    if let Ok(g) = REMOTE.lock() {
        if let Some(src) = g.as_ref() {
            src.prime();
        }
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
            /* 让音频线程里正在进行的 read() 立刻返回 —— session_end() 的
             * join 才不会拖住界面线程（见 HttpRangeSource::cancel 的说明）。 */
            src.cancel();
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

/* ------------------------------------------------------------ 在线清单 -- */

const PLAYLIST_FILE: &str = "ux0:data/yunyin/playlist.json";
#[derive(Clone)]
pub struct NetplayTrack {
    pub url: String,
    pub referer: String,
    pub title: String,
    /// 所属歌单名（JSON 清单里的 `name`；旧格式的两条来源为空串）。
    pub playlist: String,
}

/* ------------------------------------------------------------ JSON 歌单 -- */
/*
 * 推荐格式（取代 netease.ids / netplay.url 两个文本文件）：
 *
 * ```json
 * {
 *   "playlists": [
 *     {
 *       "name": "我的歌单",
 *       "songs": [
 *         { "id": "3346495279", "title": "显示名（可选）" },
 *         { "url": "https://…", "referer": "https://music.163.com/", "title": "可选" }
 *       ]
 *     }
 *   ]
 * }
 * ```
 *
 * 也接受顶层直接给 `"songs": [...]`（当成一张叫「在线歌曲」的清单）。
 * 解析失败只记一行日志并返回空表 —— 交给旧的 .ids/.url 兜底，
 * 绝不因为一个坏 JSON 就让在线播放整条断掉。
 */
pub fn json_playlist_tracks() -> Vec<NetplayTrack> {
    use crate::media::provider::netease::json::Json;

    let Ok(text) = std::fs::read_to_string(PLAYLIST_FILE) else {
        return Vec::new();
    };
    let root = match Json::parse(&text) {
        Ok(v) => v,
        Err(e) => {
            log::append(&format!("playlist.json: 解析失败（{e}），改用旧格式"));
            return Vec::new();
        }
    };

    let mut out: Vec<NetplayTrack> = Vec::new();
    if let Some(list) = root.get("playlists") {
        let mut i = 0;
        while let Some(pl) = list.at(i) {
            i += 1;
            let name = pl
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if let Some(songs) = pl.get("songs") {
                json_push_songs(songs, name, &mut out);
            }
        }
    }
    if let Some(songs) = root.get("songs") {
        json_push_songs(songs, "", &mut out);
    }

    if out.is_empty() {
        log::append("playlist.json: 没有可用条目（检查 songs 里的 id / url）");
    } else {
        log::append(&format!("playlist.json: 读到 {} 首在线歌", out.len()));
    }
    out
}

/// 把一张歌单里的 `songs` 数组追加到 `out`。id（数字或字符串）→ `netease:<id>`；
/// 否则看 `url`（http/https）。两条都没有的条目跳过。
fn json_push_songs(songs: &crate::media::provider::netease::json::Json, playlist: &str, out: &mut Vec<NetplayTrack>) {
    let mut i = 0;
    while let Some(song) = songs.at(i) {
        i += 1;

        let title = song.get("title").and_then(|v| v.as_str()).unwrap_or("").trim();
        let mut id = song.get("id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        if id.is_empty() {
            if let Some(n) = song.get("id").and_then(|v| v.as_i64()) {
                id = format!("{n}");
            }
        }
        let raw_url = song.get("url").and_then(|v| v.as_str()).unwrap_or("").trim();
        let referer = song.get("referer").and_then(|v| v.as_str()).unwrap_or("").trim();

        let (url, ref_default) = if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) {
            (
                format!("netease:{id}"),
                String::from(crate::media::provider::netease::REFERER),
            )
        } else if raw_url.starts_with("http://") || raw_url.starts_with("https://") {
            (
                String::from(raw_url),
                if referer.is_empty() {
                    String::from(crate::media::provider::netease::REFERER)
                } else {
                    String::from(referer)
                },
            )
        } else {
            continue; /* 既没 id 也没 url：跳过 */
        };

        let title = if title.is_empty() {
            /* id 条目直接用 ID 兜底（`url_id_hint` 只认 `id=` 那种直链），
             * 否则所有只写 id 的条目都会叫 `[在线] 网络歌曲`，看着一模一样。 */
            let hint = if !id.is_empty() { id.clone() } else { url_id_hint(&url) };
            if hint.is_empty() {
                String::from("[在线] 网络歌曲")
            } else {
                format!("[在线] {hint}")
            }
        } else {
            String::from(title)
        };

        out.push(NetplayTrack {
            url,
            referer: ref_default,
            title,
            playlist: String::from(playlist),
        });
    }
}

/// 把 `id=123456` 里的数字挑出来，用作没写显示名时的默认名字。
fn url_id_hint(url: &str) -> String {
    let after = url.split("id=").nth(1).unwrap_or("");
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits
}

/// 在线曲目清单：**只看 `playlist.json`**。
///
/// 以前还有 `netease.ids` / `netplay.url` 两个纯文本格式（早期临时测试用），
/// 2026-10 起彻底移除 —— 新格式能分组、能写直链、还能带 title，
/// 旧文件放着不读，避免"两套写法各写一半"的混乱。
pub fn netplay_tracks() -> Vec<NetplayTrack> {
    json_playlist_tracks()
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
            "{{\"url\":\"{}\",\"title\":\"{}\",\"referer\":\"{}\",\"playlist\":\"{}\"}}",
            crate::media::json_escape(&t.url),
            crate::media::json_escape(&t.title),
            crate::media::json_escape(&t.referer),
            crate::media::json_escape(&t.playlist)
        ));
    }
    s.push(']');
    s
}
