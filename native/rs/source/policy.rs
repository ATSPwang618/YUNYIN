//! 缓冲策略：**一切以秒为准**，不看固定 KB（纯逻辑，宿主可测）。
//!
//! 为什么：512 KB 在 128 kbps 下是 32 秒、在 320 kbps 下只有 13 秒 ——
//! 同一个字节数，能撑的时间差 2.5 倍。所以启动/维持/恢复三个阈值都用
//! "buffer 秒数"表达，字节数只是用来换算的输入。
//!
//! 三个阈值 + 一条迟滞：
//!   * `START_BUFFER_MS` 首次启动要凑够 10 秒（真机首包 1.8–2.7 秒，太少会一开就卡）；
//!   * `MIN_BUFFER_MS`   播放中掉到 5 秒以下才进入 rebuffer（不要抖一下就停）；
//!   * `RESUME_BUFFER_MS` rebuffer 之后要回到 8 秒才继续（迟滞，避免抖动）；
//!   * `TARGET_BUFFER_MS` 网络好时希望维持 20 秒 —— 预取是否让路的判据。

#![allow(dead_code)]

/// 首次启动播放需要的 buffer。
pub const START_BUFFER_MS: u64 = 10_000;
/// 播放中低于这个值进入 rebuffer。
pub const MIN_BUFFER_MS: u64 = 5_000;
/// rebuffer 之后回到这个值才继续播（迟滞）。
pub const RESUME_BUFFER_MS: u64 = 8_000;
/// 网络正常时希望维持的 buffer（预取让路判据）。
pub const TARGET_BUFFER_MS: u64 = 20_000;

/*
 * 预读预算（滚动预读队列的总字节数）—— 真机 2026-10-06 反馈："每次只缓存一小点，
 * 播几秒就又要缓冲"。
 *
 * 为什么按"整首的 1/3"给：一首歌的码率差异极大（128 kbps 到 1.6 Mbps 无损），
 * 用固定字节数表达，同一个数字在两种码率下能撑的秒数差 10 倍。按整首比例给，
 * 慢网下也能一次攒出"能稳住一阵子"的提前量；播到后面取数线程自己接着补剩下的。
 *
 * 为什么还要夹在 [MIN, MAX]：一首 50 MB 的无损，1/3 就是 16 MB —— 那是**磁盘**
 * 缓存该干的事（cache.dat 环形缓存），不该整块塞进 RAM。真机空闲内存虽然够，
 * 但块越大，解码器跨窗那一下的抖动越明显。
 */
/// 预读预算下限（再短的歌也至少提前抓这么多，反正到底就停）。
pub const READAHEAD_MIN_BYTES: u64 = 2 * 1024 * 1024;
/// 预读预算上限（RAM 保护：队头 + 队列 + 取数中转最多 ~8 MiB）。
pub const READAHEAD_MAX_BYTES: u64 = 6 * 1024 * 1024;
/// 长度未知时（没有 Content-Length）用的中间值。
pub const READAHEAD_FALLBACK_BYTES: u64 = 4 * 1024 * 1024;

/// 预读预算：整首的 1/3，夹在 `[READAHEAD_MIN_BYTES, READAHEAD_MAX_BYTES]`。
pub fn readahead_budget(total: Option<u64>) -> u64 {
    match total {
        Some(total) if total > 0 => (total / 3).clamp(READAHEAD_MIN_BYTES, READAHEAD_MAX_BYTES),
        _ => READAHEAD_FALLBACK_BYTES,
    }
}
/// 预取**真正开跑**要求的 buffer：宁可晚一点备下一首，也不挤当前曲（2×TARGET）。
pub const PREFETCH_MIN_BUFFER_MS: u64 = 40_000;
/// 用户刚操作过（按键）之后这段时间内不预取：
/// 真机日志里"按钮上下切页面卡顿"就是后台整首下载挤出来的。
pub const PREFETCH_QUIET_MS: u64 = 3_000;
/// 码率完全未知时的兜底（192 kbps 是网易云"标准"档）。
pub const FALLBACK_BITRATE_BPS: u32 = 192_000;

/// 字节数 → 能播多少毫秒。
///
/// `bitrate_bps == 0`（未知）时用 `fallback_bps`，再没有就用默认兜底。
pub fn buffer_ms(bytes: u64, bitrate_bps: u32, fallback_bps: u32) -> u64 {
    let bps = if bitrate_bps > 0 {
        bitrate_bps
    } else if fallback_bps > 0 {
        fallback_bps
    } else {
        FALLBACK_BITRATE_BPS
    };
    bytes.saturating_mul(8).saturating_mul(1000) / bps as u64
}

/// 用"总字节数 + 时长"推平均码率（整首歌的平均值，够用来算 buffer 秒数）。
pub fn bitrate_from_size_duration(total: u64, dur_ms: u64) -> Option<u32> {
    if total == 0 || dur_ms == 0 {
        return None;
    }
    let bps = total.saturating_mul(8).saturating_mul(1000) / dur_ms;
    if bps == 0 || bps > u32::MAX as u64 {
        None
    } else {
        Some(bps as u32)
    }
}

/// 网络相对码率的分级（用滑动平均速度喂进来，别用瞬时值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetState {
    /// 速度 > 码率 × 2：可以考虑预取下一首。
    Fast,
    /// 速度 > 码率 × 1.2：只维持当前曲。
    Normal,
    /// 速度 ≈ 码率：勉强够，别再抢带宽。
    Slow,
    /// 速度 < 码率：正在饿死，全力保当前曲。
    Starving,
}

pub fn classify(speed_bps: u64, bitrate_bps: u32) -> NetState {
    let b = if bitrate_bps > 0 {
        bitrate_bps as u64
    } else {
        FALLBACK_BITRATE_BPS as u64
    };
    if speed_bps >= b.saturating_mul(2) {
        NetState::Fast
    } else if speed_bps.saturating_mul(5) >= b.saturating_mul(6) {
        NetState::Normal
    } else if speed_bps >= b {
        NetState::Slow
    } else {
        NetState::Starving
    }
}

/// 连续失败第 `fail_count` 次之后该等多久（毫秒）：200 → 500 → 1000 → 2000（封顶）。
///
/// 为什么不能一直是 300 ms：网络抖的时候会把接口打爆、把电量耗光；
/// 为什么要封顶 2 秒：等太久用户会以为卡死（重试次数本身也有上限）。
pub fn backoff_ms(fail_count: u32) -> u64 {
    match fail_count {
        0 => 200,
        1 => 500,
        2 => 1_000,
        _ => 2_000,
    }
}

/// 下一首预取该不该现在跑（纯逻辑，宿主可测）。
///
/// 规矩：**当前曲永远优先** —— 缓冲没到 TARGET、或网络不是 FAST，就先别预取。
/// 例外：当前曲已经全部到手（`at_end`），带宽闲着也是闲着。
pub fn prefetch_should_run(buffer_ms: u64, net: NetState, at_end: bool) -> bool {
    prefetch_should_run_q(buffer_ms, net, at_end, PREFETCH_QUIET_MS)
}

/// 同上，但把"用户是否刚操作过"也纳入判据（`quiet_for_ms` = 距上次按键多久）。
///
/// 规矩（按重要性）：
///   1. 当前曲已经全部到手（`at_end`）→ 带宽空着，可以直接预取；
///   2. 用户刚操作过（< 3 秒）→ **一律不预取**，保证按键响应不被后台下载挤；
///   3. 缓冲要 ≥ 2×TARGET(40s) 且网络 FAST —— 比维持播放所需更宽裕才动手。
pub fn prefetch_should_run_q(
    buffer_ms: u64,
    net: NetState,
    at_end: bool,
    quiet_for_ms: u64,
) -> bool {
    if at_end {
        return true;
    }
    if quiet_for_ms < PREFETCH_QUIET_MS {
        return false;
    }
    buffer_ms >= PREFETCH_MIN_BUFFER_MS && net == NetState::Fast
}

/// **统一世代判据**：异步任务（预取 / 缓存写入 / 解码器等待）该不该作废。
///
/// 只认**一个**世代 —— 播放世代（`remote::generation()`，即 `OPEN_TOKEN`）。
/// 切歌/停止这件事本身就让它变，所以每个旧任务下一步就会失效，
/// **不依赖任何调用方记得去取消**（以前预取只看自己那套计数器就有这个隐患）。
///
/// 注意：**seek 代数不算在内** —— 那是"同一个播放代内部换了读位置"，语义不同，
/// 合并进来会把"同一首歌里拖动进度条"误判成切歌。
pub fn generation_stale(worker_playback: u32, current_playback: u32) -> bool {
    worker_playback != current_playback
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateDecision {
    /// 可以调解码器（会真的出声）。
    Play,
    /// 只输出静音，**绝不调解码器**（WouldBlock 语义）。
    Rebuffer,
}

/// Gate 的迟滞状态机（每个播放会话一个）。
#[derive(Clone, Copy, Debug)]
pub struct GateState {
    started: bool,
    rebuffering: bool,
}

impl GateState {
    pub const fn new() -> Self {
        Self {
            started: false,
            rebuffering: true,
        }
    }

    /// `buffer_ms` = 现在可播多少毫秒；`at_end` = 已经接到流末尾（不会再有数据）。
    pub fn decide(&mut self, buffer_ms: u64, at_end: bool) -> GateDecision {
        if at_end {
            /* 末尾放行：不然最后几帧永远等不到"够 5 秒"。真机踩过。 */
            self.started = true;
            self.rebuffering = false;
            return GateDecision::Play;
        }
        if !self.started {
            if buffer_ms >= START_BUFFER_MS {
                self.started = true;
                self.rebuffering = false;
                return GateDecision::Play;
            }
            return GateDecision::Rebuffer;
        }
        if self.rebuffering {
            if buffer_ms >= RESUME_BUFFER_MS {
                self.rebuffering = false;
                return GateDecision::Play;
            }
            return GateDecision::Rebuffer;
        }
        if buffer_ms < MIN_BUFFER_MS {
            self.rebuffering = true;
            return GateDecision::Rebuffer;
        }
        GateDecision::Play
    }

    /// 这次播放是否已经"开始过"（日志与界面用）。
    pub fn started(&self) -> bool {
        self.started
    }
}

impl Default for GateState {
    fn default() -> Self {
        Self::new()
    }
}
