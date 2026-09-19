//! Yunyin native host: `globalThis.vitaMedia`.
//!
//! All formats decode in-process (`yplayer.c`) and play through the Vita BGM
//! port (ElevenMPVScrobbling path). Sound belongs to this process, so tearing
//! the LiveArea bubble stops playback without a QUIT watchdog.

use alloc::string::String;

pub mod bgm;
mod bridge;
mod cjk_host;
mod decoder;
mod fs;
mod font_gpu;
mod log;
pub mod offload_local;
mod power;
mod ps_lock;
mod store;
mod tags;

pub use font_gpu::refresh_font_atlases;

pub(crate) const COVER_PX: u32 = 256;
pub(crate) const MAX_ART: usize = 1024 * 1024;
pub(crate) const PREFIX_CAP: usize = MAX_ART + 65536;

pub(crate) fn json_escape(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            _ => o.push(c),
        }
    }
    o
}

/// Install `globalThis.vitaMedia`.
///
/// # Safety
/// Same realm, render thread, once per guest.
pub unsafe fn register(ctx: *mut libquickjs_sys::JSContext, global: libquickjs_sys::JSValue) {
    log::init();
    log::append("yunyin: start (in-process BGM)");
    ps_lock::init();
    bgm::acquire_on_start();
    power::start();
    bridge::install(ctx, global);
    cjk_host::install(ctx, global);
}
