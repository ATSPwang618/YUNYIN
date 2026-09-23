//! Directory listing via `yunyin_listdir.c` (SceIo).

use alloc::string::String;
use std::ffi::CString;

extern "C" {
    fn yunyin_list_dir(path: *const u8, out: *mut u8, cap: i32) -> i32;
}

pub fn list_dir(path: &str) -> String {
    let c_path = match CString::new(path) {
        Ok(c) => c,
        Err(_) => return "[]".to_string(),
    };
    let mut buf = vec![0u8; 256 * 1024];
    let n = unsafe {
        yunyin_list_dir(
            c_path.as_ptr() as *const u8,
            buf.as_mut_ptr(),
            buf.len() as i32,
        )
    };
    if n <= 0 {
        return "[]".to_string();
    }
    let n = (n as usize).min(buf.len());
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

pub fn roots_json() -> &'static str {
    "[{\"id\":\"app0\",\"path\":\"app0:music\"},{\"id\":\"ux0\",\"path\":\"ux0:\"},{\"id\":\"uma0\",\"path\":\"uma0:\"},{\"id\":\"ur0\",\"path\":\"ur0:\"}]"
}
