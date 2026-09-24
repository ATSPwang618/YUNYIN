//! 网络层（Phase 0/2，任务书 §20/§21）。
//!
//! 这一层只管**传输**：跑在 Vita 自带网络栈上的 HTTP，包含平台真正需要的那些能力
//! —— TLS、Range、重定向、Cookie、取消。它不认识任何音乐平台；平台也永远不碰 socket。
//!
//! 当前状态：类型与 `HttpClient` 接缝已就位；真机冒烟测试（§25/§26）已经确认
//! HTTPS + Range + 206 + Cookie + 取消都能用，`yhttp.c`（§21）就是这一层的实现。
#![allow(dead_code)]

use crate::media::platform::log;

pub mod http;
pub mod probe;

extern "C" {
    fn yhttp_set_log(fn_: Option<unsafe extern "C" fn(*const u8, u32)>);
}

/// 把 C 侧网络层的日志接到主日志上。
///
/// 以前这活儿是探针线程干的（`probe.rs`），于是"不开探针"时在线播放的
/// C 日志就没人接了；现在启动就装一次，谁都能看见。
#[no_mangle]
unsafe extern "C" fn yunyin_net_log_main(text: *const u8, len: u32) {
    if text.is_null() || len == 0 {
        return;
    }
    let bytes = unsafe { core::slice::from_raw_parts(text, len as usize) };
    let line = alloc::string::String::from_utf8_lossy(bytes);
    log::append(&alloc::format!("[net] {}", line.trim_end()));
}

pub fn install_log() {
    unsafe { yhttp_set_log(Some(yunyin_net_log_main)) };
}

/// 网络层做到哪一步了。给界面用 —— About 页可以老老实实写"网络：未实现"，
/// 而不是假装能用。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetState {
    /// 还没实现（Phase 0 之前）。
    Absent,
    /// 传输能用，Provider 还没有（Phase 2）。
    Transport,
    /// Provider 也能用了（Phase 3 及以后）。
    Ready,
}

pub fn state() -> NetState {
    /* Phase 0：探针已经证明传输可用，但还没有 Provider。 */
    NetState::Absent
}
