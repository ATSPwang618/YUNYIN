//! 对 `yplayer.c` 的 FFI —— 一个拉模式解码器，六个格式共用。
//!
//! Phase 1 起 C 侧是 handle 接口（任务书 §30/§31）：这里保存当前 handle，
//! 其余调用都带上它。C 侧目前是单实例实现（同一时刻放一首歌），
//! Rust 这边照 handle 用，Phase 2 换成多实例时这一层不用再改。

use core::ffi::c_void;
use std::ffi::CString;
use std::sync::Mutex;

/* 用 usize 存 handle：裸指针不是 Send，放进 static Mutex 会编译不过；
 * 存成整数再转回指针，语义一样。 */
type Handle = usize;

static HANDLE: Mutex<Option<Handle>> = Mutex::new(None);

extern "C" {
    fn yp_open(path: *const i8) -> *mut c_void;
    fn yp_rate(p: *mut c_void) -> i32;
    fn yp_decode(p: *mut c_void, buf: *mut i16, max_frames: i32) -> i32;
    fn yp_seek(p: *mut c_void, frame: i64) -> i32;
    fn yp_position(p: *mut c_void) -> i64;
    fn yp_length(p: *mut c_void) -> i64;
    fn yp_close(p: *mut c_void);
}

fn handle() -> Option<*mut c_void> {
    match HANDLE.lock() {
        Ok(g) => g.map(|h| h as *mut c_void),
        Err(_) => None,
    }
}

pub fn open(path: &str) -> bool {
    let Ok(c_path) = CString::new(path) else {
        return false;
    };
    let h = unsafe { yp_open(c_path.as_ptr()) };
    if h.is_null() {
        return false;
    }
    if let Ok(mut g) = HANDLE.lock() {
        *g = Some(h as usize);
    }
    true
}

pub fn close() {
    if let Ok(mut g) = HANDLE.lock() {
        if let Some(h) = g.take() {
            unsafe { yp_close(h as *mut c_void) };
        }
    }
}

pub fn rate() -> i32 {
    match handle() {
        Some(h) => unsafe { yp_rate(h) }.max(1),
        None => 44100,
    }
}

pub fn decode(buf: &mut [i16], max_frames: i32) -> i32 {
    if buf.is_empty() || max_frames <= 0 {
        return 0;
    }
    match handle() {
        Some(h) => unsafe { yp_decode(h, buf.as_mut_ptr(), max_frames) },
        None => 0,
    }
}

#[allow(dead_code)]
pub fn seek(frame: i64) -> bool {
    match handle() {
        Some(h) => unsafe { yp_seek(h, frame) == 0 },
        None => false,
    }
}

pub fn position() -> i64 {
    match handle() {
        Some(h) => unsafe { yp_position(h) }.max(0),
        None => 0,
    }
}

pub fn length() -> i64 {
    match handle() {
        Some(h) => unsafe { yp_length(h) }.max(0),
        None => 0,
    }
}

pub fn duration_ms() -> u32 {
    let rate = rate().max(1) as u64;
    let frames = length() as u64;
    ((frames * 1000) / rate) as u32
}

pub fn position_ms() -> u32 {
    let rate = rate().max(1) as u64;
    let frames = position() as u64;
    ((frames * 1000) / rate) as u32
}
