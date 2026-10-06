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
//! source/ net/ provider/               引擎接缝（见 docs/架构.md）
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
    log::append("yunyin: start (in-process BGM) 版本 01.10");
    ps_lock::init();
    bgm::acquire_on_start();
    /* 预挂 AAC 硬解模块：第一次播 M4A 时才加载会让渲染卡 4.5 秒（真机日志）。 */
    let avcdec = decoder::preload_codecs();
    /*
     * 运行环境一行：一眼分清"这份日志是模拟器跑的"还是"真机跑的"。
     *
     *   AVCDEC=ok      机器自带 AAC 硬解模块挂上了（真机 / 支持完整的模拟器）
     *   AVCDEC=不可用  多半是模拟器（Vita3K 对硬件解码支持不完整）
     *   网络=connected 由 SceNetCtl 报的实时联网状态
     */
    /*
     * 启动面包屑：真机"一进去就崩"时，最后写到哪一行就是最后执行到哪一步。
     * 之前只有"AVCDEC 预挂"和"运行环境"两行，中间隔着网络查询 —— 粒度太粗，
     * 白花了一轮往返。现在每一步之前都留一句，崩在哪一步一眼可见。
     */
    log::append("yunyin: 启动 1/4 查询联网状态");
    let net_ok = net::http::online();
    log::append("yunyin: 启动 2/4 网络状态已取到");
    log::append(&format!(
        "yunyin: 运行环境 AVCDEC={} 网络={} {}",
        if avcdec >= 0 { "ok" } else { "不可用（模拟器？）" },
        if net_ok { "connected" } else { "offline" },
        if avcdec >= 0 && net_ok { "" } else { "← 功能可能受限" }
    ));
    power::start();
    log::append("yunyin: 启动 3/4 装桥接");
    bridge::install(ctx, global);
    cjk_host::install(ctx, global);
    log::append("yunyin: 启动 4/4 桥接完成，进入正常帧循环");
    /* Phase 0 network probe: inert unless the card asks for it
     * (ux0:/data/yunyin/netprobe.url or the debug flag). */
    net::probe::start_once();
    net::install_log(); /* 先接上 C 侧网络日志，在线播放也能看见 */
    /*
     * 卡里放 ux0:/data/yunyin/playlist.json 时，里面的歌会作为
     * **曲库里的独立条目**交给界面（vitaMedia.netplay()），不再偷偷占用
     * 本地第一首的位置。启动时这里只记一条日志，实际播放由用户点选触发。
     */
    let online = source::remote::netplay_tracks().len();
    if online > 0 {
        log::append(&format!(
            "remote: 在线曲目 {online} 首（playlist.json），已交给界面显示（独立成组）"
        ));
    }
    /*
     * list/ 目录：每次启动后台刷一遍在线清单（热门推荐 / 每日推荐 / 我的歌单 /
     * 四个榜单），界面直接读这些 JSON 文件 —— 这次打开就有内容，离线也能看上次的。
     */
    provider::netease::lists::sync_background(false);
}
