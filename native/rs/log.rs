//! Runtime log. Off unless `ux0:/data/yunyin/debug` exists.

use core::sync::atomic::{AtomicBool, Ordering};
use std::io::Write;

static LOG_ON: AtomicBool = AtomicBool::new(false);
const LOG_FLAG: &str = "ux0:data/yunyin/debug";

pub fn init() {
    if std::fs::File::open(LOG_FLAG).is_ok() {
        LOG_ON.store(true, Ordering::Relaxed);
    }
}

pub fn enabled() -> bool {
    LOG_ON.load(Ordering::Relaxed)
}

/// Append a line to `ux0:data/yunyin.log`. No-op when logging is off.
pub fn append(s: &str) {
    if !enabled() {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("ux0:data/yunyin.log")
    {
        let _ = writeln!(f, "{}", s);
    }
}
