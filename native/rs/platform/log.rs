//! Runtime log. Off unless `ux0:/data/yunyin/debug` exists.
//!
//! 日志与探针报告都放在**应用自己的文件夹**里（`ux0:/data/yunyin/`），
//! 不再往 `ux0:/data/` 根目录丢文件 —— 卡上一个文件夹装完。

use core::sync::atomic::{AtomicBool, Ordering};
use std::io::Write;
use std::sync::{Mutex, MutexGuard};

static LOG_ON: AtomicBool = AtomicBool::new(false);
const LOG_FLAG: &str = "ux0:data/yunyin/debug";
const LOG_DIR: &str = "ux0:/data/yunyin";
const LOG_PATH: &str = "ux0:/data/yunyin/yunyin.log";

/*
 * 日志锁 —— **所有**写 ux0:data/yunyin.log 的地方都必须先拿它。
 *
 * 为什么必须有（00.84，真机崩溃的根因）：
 *   C 侧走 SceIo（sceIoOpen/sceIoWrite/sceIoClose），Rust 侧走 std::fs（newlib stdio），
 *   两条路径各自打开、写入、关闭同一个文件，中间没有任何互斥。在线播放一开始，
 *   音频线程 / 在线打开线程 / 取数线程 / 界面线程会同时写日志 —— 真机日志里出现过
 *   `dbg: 音频线程第一拍dbg: 音频口已起` 这种两行黏在一起的情况，就是并发写的直接证据。
 *   紧接着的崩溃（DFAR=0xc：格式化用的 trait 对象被踩成 0）说明底层 stdio/堆已经被踩坏。
 *
 * 规矩：日志是共享资源，**先拿锁、再开文件**；C 侧也统一走 yunyin_log_line()。
 */
static LOG_LOCK: Mutex<()> = Mutex::new(());

/*
 * 行缓冲 + **后台落卡线程**：append() 只在内存里搬，任何调用者都不在自己线程上碰卡。
 *
 * 为什么（真机 2026-10-08 的日志）：
 *   1) 最早每写一行都 open + write + close 一次 ux0:/data/yunyin/yunyin.log，
 *      一晚上万行 = 上万次 SD 操作；音频线程读 MP3 用的是同一张卡（BGM 口缓冲
 *      只有 ~21ms）→"本地 mp3 偶尔卡一下"；
 *   2) 改成"帧循环每 60 帧 flush 一次"后仍然卡：空闲时 60 帧只有 ~0.17s，等于
 *      每秒往卡上写 5~6 次、每次几十 ms —— 真机就是"每 1~2 秒轻卡一下"
 *      （600 帧汇总里"最慢帧"稳定在 40~66ms）。
 * 现在 append 只是 memcpy，落卡交给 start_flusher() 起的后台线程按**时间**做。
 */
const LOG_FLUSH_INTERVAL_US: u32 = 3_000_000;
const LOG_BUF_CAP: usize = 256 * 1024;
static LOG_BUF: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static LOG_DROPPED: AtomicBool = AtomicBool::new(false);

/// 拿日志锁（同进程里其它写日志的地方共用，例如探针报告文件）。
pub fn lock() -> MutexGuard<'static, ()> {
    LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn init() {
    /* 先把目录建出来：日志、list/、covers/ 都住在同一个文件夹下。
     * 全新卡上 ux0:/data/yunyin 可能还不存在，直接 append 会静默失败。 */
    let _ = std::fs::create_dir_all(LOG_DIR);
    if std::fs::File::open(LOG_FLAG).is_ok() {
        LOG_ON.store(true, Ordering::Relaxed);
        start_flusher();
    }
}

pub fn enabled() -> bool {
    LOG_ON.load(Ordering::Relaxed)
}

/// Append a line to the in-memory log buffer. No-op when logging is off.
///
/// 这里**不碰文件**：调用者可能是音频线程，写卡会把播放拖卡（见 LOG_BUF 的说明）。
pub fn append(s: &str) {
    if !enabled() {
        return;
    }
    let Ok(mut buf) = LOG_BUF.lock() else { return };
    if buf.len() >= LOG_BUF_CAP {
        /* 没人来 flush（例如卡在某一帧）：丢新的，别把内存吃光。 */
        LOG_DROPPED.store(true, Ordering::Relaxed);
        return;
    }
    let _ = writeln!(&mut *buf, "{}", s);
}

/// 后台落卡线程（启动时起一次）。
///
/// 写卡本身避不开，但频率从"每秒 5~6 次"降到"每 3 秒 1 次"，而且不占主线程、
/// 不占音频线程 —— 真机"每 1~2 秒轻卡一下"就是帧循环里那次 flush 造成的。
pub fn start_flusher() {
    let _ = std::thread::Builder::new()
        .name("yunyin-log".into())
        .stack_size(32 * 1024)
        .spawn(|| loop {
            unsafe { vitasdk_sys::sceKernelDelayThread(LOG_FLUSH_INTERVAL_US) };
            flush();
        });
}

/// 把内存里的日志写进卡：先把缓冲整个换出来，写的时候不占 LOG_BUF 锁。
pub fn flush() {
    if !enabled() {
        return;
    }
    let pending = {
        let Ok(mut buf) = LOG_BUF.lock() else { return };
        if buf.is_empty() {
            return;
        }
        if LOG_DROPPED.swap(false, Ordering::Relaxed) {
            let _ = writeln!(&mut *buf, "log: 缓冲区满，中间有日志被丢弃");
        }
        core::mem::take(&mut *buf)
    };
    let t0 = crate::media::platform::time::now_ms();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LOG_PATH)
    {
        let _ = f.write_all(&pending);
    }
    let ms = crate::media::platform::time::now_ms().saturating_sub(t0);
    /* 自己花多久也记一笔（下次一起落卡）：确认 3 秒一次到底值不值。 */
    if ms >= 5 {
        append(&format!("log: 落卡 {} 字节耗时 {}ms", pending.len(), ms));
    }
}

/// C 侧日志的统一入口 —— `native/host/yunyin_log.h` 调它。
///
/// 以前 C 自己 sceIoWrite，和 Rust 的 std::fs 并发写同一个文件；现在两边都从这里进，
/// 共用同一把 `LOG_LOCK`，真机上"两行日志黏在一起、随后崩在格式化"的问题就没了。
///
/// # Safety
/// `text` 必须指向至少 `len` 个字节（C 传的是 `msg` 与 `strlen(msg)`）。
#[no_mangle]
pub unsafe extern "C" fn yunyin_log_line(text: *const u8, len: u32) {
    if text.is_null() || len == 0 {
        return;
    }
    let bytes = core::slice::from_raw_parts(text, len as usize);
    let line = std::string::String::from_utf8_lossy(bytes);
    append(line.trim_end());
}
