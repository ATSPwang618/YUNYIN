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
    /*
     * 版本号写进日志：真机排障时第一件事就是确认"跑的是哪一版"。
     * 以前只能靠行为猜，白花了整整一轮往返。
     */
    log::append("yunyin: start (in-process BGM) 版本 00.87");
    /*
     * 排障效率：开着日志时顺便开一个"把日志回给电脑"的小服务，
     * 电脑上 `curl http://<vita-ip>:1337/ -o yunyin.log` 就能取，
     * 不用再手动拷 ux0:data/yunyin.log。
     */
    log::start_log_server();
    ps_lock::init();
    bgm::acquire_on_start();
    power::start();
    bridge::install(ctx, global);
    cjk_host::install(ctx, global);
    /* Phase 0 network probe: inert unless the card asks for it
     * (ux0:/data/yunyin/netprobe.url or the debug flag). */
    net::probe::start_once();
    net::install_log(); /* 先接上 C 侧网络日志，在线播放也能看见 */
    /*
     * Phase 2：卡里放 ux0:/data/yunyin/netplay.url 时，里面的歌会作为
     * **曲库里的独立条目**交给界面（vitaMedia.netplay()），不再偷偷占用
     * 本地第一首的位置。启动时这里只记一条日志，实际播放由用户点选触发。
     */
    let online = source::remote::netplay_tracks().len();
    if online > 0 {
        log::append(&format!(
            "remote: netplay.url 里有 {online} 首在线曲目，已交给界面显示（独立成组）"
        ));
    }
}
