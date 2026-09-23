//! Yunyin native host: `globalThis.vitaMedia`.
//!
//! All formats decode in-process (`yplayer.c`) and play through the Vita BGM
//! port (ElevenMPVScrobbling path). Sound belongs to this process, so tearing
//! the LiveArea bubble stops playback without a QUIT watchdog.
//!
//! 目录分工：
//!
//! ```text
//! bgm.rs decoder.rs bridge.rs tags.rs  播放器本体（音频线程 / FFI / JS 绑定 / 标签）
//! source/ net/ provider/               引擎接缝（见 docs/架构设计.md）
//! platform/                            电源、PS 键锁、文件、日志、设置
//! ui/                                  流式 CJK、字体图集、跳帧
//! ```

use alloc::string::String;

pub mod bgm;
mod bridge;
mod decoder;
pub mod net;
mod platform;
pub mod provider;
pub mod source;
mod tags;
mod ui;

/* Explicit re-exports: the PocketJS host calls these by name, and the rest of
 * the tree keeps using the short paths (`log`, `ps_lock`, ...) it always did. */
pub use platform::{fs, log, power, ps_lock, store};
pub use ui::{cjk_host, font_gpu, frame_skip, offload_local};
pub use ui::font_gpu::refresh_font_atlases;
pub use ui::frame_skip::frame_changed;

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
    /* Phase 0 network probe: inert unless the card asks for it
     * (ux0:/data/yunyin/netprobe.url or the debug flag). */
    net::probe::start_once();
}
