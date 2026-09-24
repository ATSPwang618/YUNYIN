//! Runtime log. Off unless `ux0:/data/yunyin/debug` exists.

use core::sync::atomic::{AtomicBool, Ordering};
use std::io::Write;
use std::sync::{Mutex, MutexGuard};

static LOG_ON: AtomicBool = AtomicBool::new(false);
const LOG_FLAG: &str = "ux0:data/yunyin/debug";
const LOG_PATH: &str = "ux0:data/yunyin.log";

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

/// 拿日志锁（同进程里其它写日志的地方共用，例如探针报告文件）。
pub fn lock() -> MutexGuard<'static, ()> {
    LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn init() {
    if std::fs::File::open(LOG_FLAG).is_ok() {
        LOG_ON.store(true, Ordering::Relaxed);
    }
}

/// 日志服务端口（电脑上 `curl http://<vita-ip>:1337/ -o yunyin.log`）。
const LOG_SERVE_PORT: u16 = 1337;

extern "C" {
    fn yhttp_logserve_start(path: *const i8, port: u16) -> i32;
}

/// 开着日志时，在后台开一个"把日志文件回给电脑"的小服务。
///
/// 为什么值得有：真机排障最耗人的一步是"把 ux0:data/yunyin.log 手动拷到电脑"。
/// 有了它，电脑上一条 `curl http://<vita-ip>:1337/ -o yunyin.log` 就够了
/// （浏览器直接打开那个地址也能看）。
pub fn start_log_server() {
    if !enabled() {
        return;
    }
    let Ok(path) = std::ffi::CString::new(LOG_PATH) else {
        return;
    };
    unsafe { yhttp_logserve_start(path.as_ptr(), LOG_SERVE_PORT) };
}

pub fn enabled() -> bool {
    LOG_ON.load(Ordering::Relaxed)
}

/// Append a line to `ux0:data/yunyin.log`. No-op when logging is off.
pub fn append(s: &str) {
    if !enabled() {
        return;
    }
    let _guard = lock(); /* 串行化：多线程同时 open/write/close 会把 stdio 与堆搞坏 */
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LOG_PATH)
    {
        let _ = writeln!(f, "{}", s);
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
