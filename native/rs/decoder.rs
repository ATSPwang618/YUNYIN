//! FFI to `yplayer.c` — one pull-style decoder for every format.

use std::ffi::CString;

extern "C" {
    fn yp_open(path: *const i8) -> i32;
    fn yp_rate() -> i32;
    fn yp_channels() -> i32;
    fn yp_decode(buf: *mut i16, max_frames: i32) -> i32;
    fn yp_seek(frame: i64) -> i32;
    fn yp_position() -> i64;
    fn yp_length() -> i64;
    fn yp_close();
}

pub fn open(path: &str) -> bool {
    let Ok(c_path) = CString::new(path) else {
        return false;
    };
    unsafe { yp_open(c_path.as_ptr()) == 0 }
}

pub fn close() {
    unsafe { yp_close() }
}

pub fn rate() -> i32 {
    unsafe { yp_rate() }.max(1)
}

pub fn channels() -> i32 {
    unsafe { yp_channels() }.max(1)
}

pub fn decode(buf: &mut [i16], max_frames: i32) -> i32 {
    if buf.is_empty() || max_frames <= 0 {
        return 0;
    }
    unsafe { yp_decode(buf.as_mut_ptr(), max_frames) }
}

#[allow(dead_code)]
pub fn seek(frame: i64) -> bool {
    unsafe { yp_seek(frame) == 0 }
}

pub fn position() -> i64 {
    unsafe { yp_position() }.max(0)
}

pub fn length() -> i64 {
    unsafe { yp_length() }.max(0)
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
