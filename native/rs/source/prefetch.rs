//! 下一首预取：**只预取 1 首，而且随时给当前曲让路**（任务书 §7–§10/§29/§30）。
//!
//! 世代边界（2026-10 收口后定稿，**不再自建计数**）：
//!   * **Playback Generation** = `remote::generation()`（唯一，来自 `OPEN_TOKEN`）——
//!     切歌/停止本身就让它变，旧任务下一步就会退出，**不依赖任何人记得取消**；
//!   * 「声明的下一首」(`NEXT`) 只是一个"当前意图"字符串，不是第二套世代：
//!     worker 每一步比对"我还是不是被声明的那一首"，不是就退出；
//!   * **Seek Generation** 与本模块无关：那是同一个播放代内部的窗口失效，语义不同。
//!
//! 其余规矩：
//!   1. 当前曲优先：缓冲没到 TARGET(20 s) 或网络不是 FAST，就原地等，不抢带宽；
//!   2. 只预取**整首**：半截数据不登记（`diskcache::Writer` 自己保证），
//!      超过单曲上限直接跳过，别白花 I/O；
//!   3. 写入走 `diskcache` 的环形写入 —— 它自己让开"正在播的那一首"。
//!
//! 线程模型：一次预取一个短命线程（跑完就退），不常驻、不排队，Vita 上最省。
#![allow(dead_code)]

use super::diskcache;
use super::http::ByteTransport as _; /* `Stream::read_at` 由这个 trait 提供 */
use super::policy::{self, NetState};
use super::remote;
use alloc::format;
use alloc::string::String;
use std::sync::Mutex;
use std::time::Duration;

/// 单次预取上限：和 `diskcache::SINGLE_MAX`（16 MiB）对齐 —— 超过这个长度本来就进不了缓存。
pub const PREFETCH_MAX_BYTES: u64 = 16 * 1024 * 1024;
/// 让路时的检查间隔。
const YIELD_MS: u64 = 500;
/// 最多让路多少轮（约 2 分钟）；再等下去说明当前曲一直不富余，这次预取放弃。
const YIELD_ROUNDS: u32 = 240;
/// 一次读多大（顺序读，比取数窗口小一半，别和当前曲抢线程时间）。
const CHUNK: usize = 256 * 1024;

/// "当前声明的下一首"（意图，不是世代）。`None` = 取消。
static NEXT: Mutex<Option<String>> = Mutex::new(None);
/// 最后一次"用户操作"的时间戳（按键时由 JS 调 `netUserActive` 更新）。
static LAST_INPUT_MS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// 用户刚动过手（按键 / 切页 / 点歌）—— 记下来，预取在之后 3 秒内一律让路。
pub fn note_user_input() {
    LAST_INPUT_MS.store(
        crate::media::platform::time::now_ms(),
        core::sync::atomic::Ordering::Release,
    );
}

/// 现在该不该真的去下下一首 —— 判据在 policy 里（宿主测过），这里只转发。
pub fn should_run(buffer_ms: u64, net: NetState, at_end: bool) -> bool {
    let now = crate::media::platform::time::now_ms();
    let last = LAST_INPUT_MS.load(core::sync::atomic::Ordering::Acquire);
    let quiet_for = if last == 0 { u64::MAX } else { now.saturating_sub(last) };
    policy::prefetch_should_run_q(buffer_ms, net, at_end, quiet_for)
}

/// 我这一趟还要不要继续跑：**播放世代**没变，而且"声明的下一首"还是我。
fn keep_going(song_id: &str, playback: u32) -> bool {
    if policy::generation_stale(playback, remote::generation()) {
        return false;
    }
    match NEXT.lock() {
        Ok(g) => g.as_deref() == Some(song_id),
        Err(_) => false,
    }
}

/// 取消预取（停止播放 / 队列到头 / 换成本地歌时调）。
pub fn cancel() {
    if let Ok(mut g) = NEXT.lock() {
        *g = None;
    }
}

/// 声明"下一首是这首歌"（切歌时调；空串 = 只取消）。
pub fn set_next(song_id: &str) {
    let id = String::from(song_id.trim());
    if let Ok(mut g) = NEXT.lock() {
        *g = if id.is_empty() { None } else { Some(id.clone()) };
    }
    if id.is_empty() {
        return;
    }
    let playback = remote::generation();
    let spawned = std::thread::Builder::new()
        .name("yunyin-prefetch".into())
        /* 短命线程；栈给足一点（真机上栈溢出会直接跳空指针，排查代价极高）。 */
        .stack_size(96 * 1024)
        .spawn(move || worker(playback, &id));
    if spawned.is_err() {
        crate::media::platform::log::append("prefetch: 线程建不出来，跳过这次预取");
    }
}

fn worker(playback: u32, song_id: &str) {
    /* 1) 先等到"当前曲不饿"为止。 */
    let mut yielded = 0u32;
    loop {
        if !keep_going(song_id, playback) {
            return;
        }
        if should_run(remote::buffer_ms(), remote::net_state(), remote::at_cached_end()) {
            break;
        }
        if yielded >= YIELD_ROUNDS {
            crate::media::platform::log::append(&format!(
                "prefetch: generation={playback} 当前曲一直不够富余，放弃预取 {song_id}"
            ));
            return;
        }
        yielded += 1;
        std::thread::sleep(Duration::from_millis(YIELD_MS));
    }

    /* 2) 解析地址（要发加密 POST，放后台线程里做）。 */
    let info = match crate::media::provider::netease::resolve_song(
        song_id,
        crate::media::provider::Quality::Low,
    ) {
        Ok(i) => i,
        Err(e) => {
            crate::media::platform::log::append(&format!(
                "prefetch: generation={playback} 解析 {song_id} 失败 {e:?}"
            ));
            return;
        }
    };
    if !keep_going(song_id, playback) {
        return;
    }
    let total = info.size.unwrap_or(0);
    if total == 0 {
        crate::media::platform::log::append(&format!(
            "prefetch: generation={playback} 长度未知，跳过（半截不入索引）"
        ));
        return;
    }
    if total > PREFETCH_MAX_BYTES {
        crate::media::platform::log::append(&format!(
            "prefetch: generation={playback} {song_id} 有 {} MB，超过单曲缓存上限，跳过",
            total / (1024 * 1024)
        ));
        return;
    }

    /* 3) 顺序抓进 cache.dat（写入器会让开正在播的那首）。 */
    let key = diskcache::key_for_url(&info.url);
    let Some(mut writer) = diskcache::global().begin_write(&key, total) else {
        crate::media::platform::log::append(&format!(
            "prefetch: generation={playback} 缓存不接收这一首（已命中/太满/被保护）"
        ));
        return;
    };
    let cookie = crate::media::provider::netease::current_session()
        .cookie_header()
        .unwrap_or_default();
    let mut stream = match crate::media::net::http::Stream::open(
        &info.url,
        crate::media::provider::netease::REFERER,
        &cookie,
        0,
    ) {
        Ok(s) => s,
        Err(e) => {
            crate::media::platform::log::append(&format!(
                "prefetch: generation={playback} 打开流失败 {e:?}"
            ));
            return;
        }
    };
    let mut buf = alloc::vec![0u8; CHUNK];
    let mut off = 0u64;
    while off < total {
        if !keep_going(song_id, playback) {
            /*
             * 世代失效（切歌/停止）或已被新的下一首取代：立刻退出。
             * 写入器析构时**只推进写指针、不登记条目**，所以不会留下
             * "看起来完整、其实是半截"的缓存 —— 这就是"失效 → 不提交半成品"。
             */
            crate::media::platform::log::append(&format!(
                "prefetch: generation={playback} 已失效（当前 {}），丢弃半成品",
                remote::generation()
            ));
            return;
        }
        if !should_run(remote::buffer_ms(), remote::net_state(), remote::at_cached_end()) {
            /* 当前曲开始饿了、或用户刚按过键：先把带宽全让出去，缓过来再继续。 */
            std::thread::sleep(Duration::from_millis(YIELD_MS));
            continue;
        }
        let want = core::cmp::min(CHUNK as u64, total - off) as usize;
        match stream.read_at(off, &mut buf[..want]) {
            Ok(0) => break,
            Ok(n) => {
                writer.append(&buf[..n]);
                off += n as u64;
                /* 分片之间也要歇一下：整首下载不能变成"持续满带宽"，否则当前曲
                 * 和界面（同一台机器上的 HTTP 栈 + 卡 IO）都会被它拖。 */
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => {
                crate::media::platform::log::append(&format!(
                    "prefetch: generation={playback} 读取失败 {e:?}"
                ));
                return;
            }
        }
    }
    if off >= total {
        writer.finish();
        crate::media::platform::log::append(&format!(
            "prefetch: generation={playback} {song_id} 已备好（{} KB，切过去零网络请求）",
            off / 1024
        ));
    }
}
