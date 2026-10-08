//! 画面没变就跳过整帧重绘。
//!
//! Vita host 原来每帧无条件走 `runtime.render() + graphics::present()`：
//! 重新构建顶点、提交 GXM、交换缓冲。暂停时 / 停在设置页时画面完全没动，
//! 这些工作是纯浪费（发热、耗电）；播放时通常只有进度条在动。
//!
//! 这里照桌面 host 的做法（hosts/desktop/src/main.rs）：把 **DrawList 的内容 +
//! raster_revision**（纹理/字体/样式内容的版本号）做成一个哈希，和上一帧一样
//! 就返回 false，宿主跳过这一帧的绘制。第一帧一定画（不然后面永远空屏）。
//!
//! 另外这里给整条主循环**限帧到 60fps**。
//!
//! 为什么：宿主的循环是"能跑多快跑多快"（只在画面变了才 present），空闲时真机实测
//! **300~380 fps** —— 每秒 350 次 JS tick + DrawList 构建，等于白烧一个核。用户看到
//! 的"3 个核 80%、第 4 个看戏"里，这一核就是白烧的（Vita 的第 4 个核 CPU3 是系统
//! 保留的，应用默认用不了）。Vita 屏幕本来就只有 60Hz，限到 60fps 不损失观感：
//! 动画/滚动是按时间插值的，画面一变就照常跑；代价只是按键延迟最多多一帧（≤16ms）。
//! 音频不受影响 —— 音频是 `yunyin-bgm` 原生线程在喂口，guest 的 `audioEngine.pump()`
//! 在这个版本里是空实现。

use alloc::format;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::media::platform::log;

static LAST_HASH: AtomicU64 = AtomicU64::new(0);
static SEEN: AtomicBool = AtomicBool::new(false);
static FRAME_TICKS: AtomicU32 = AtomicU32::new(0);
static LAST_FRAME_MS: AtomicU64 = AtomicU64::new(0);

/// 主循环的目标帧间隔（ms）。16 ≈ 60fps，对齐 Vita 屏幕刷新率。
const FRAME_PERIOD_MS: u64 = 16;

pub fn frame_changed() -> bool {
    /* 主线程也争取一次第 4 个核（只做一次，见 platform/cpu.rs）。 */
    static AFFINITY: AtomicBool = AtomicBool::new(false);
    if !AFFINITY.swap(true, Ordering::AcqRel) {
        crate::media::platform::cpu::widen("主循环");
    }
    let ui = unsafe { crate::ffi::ui() };
    /* 先读 revision：draw() 会借用 ui，之后不能再读。 */
    let revision = ui.raster_revision();
    /*
     * 量一下"构建绘制列表"本身花多久。
     *
     * 为什么单独测它：真机日志里慢帧只报 `HOST: guest 帧耗时`（JS 侧），
     * 而我们的 JS 账本又说"没有具名操作超阈值" —— 两边夹出来的嫌疑就是这一步：
     * 把组件树摊平成 DrawList（~2000+ words）。有了这行才能确认，
     * 也才能判断"该减节点"还是"该减 JS 逻辑"。≥40ms 才写，不刷屏。
     */
    let draw_t0 = crate::media::platform::time::now_ms();
    let list = ui.draw();
    let draw_ms = crate::media::platform::time::now_ms().saturating_sub(draw_t0);
    if draw_ms >= 40 && log::enabled() {
        log::append(&alloc::format!(
            "HOST: draw 耗时 {}ms（words={}）",
            draw_ms,
            list.words.len()
        ));
    }
    let hash_t0 = crate::media::platform::time::now_ms();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for word in &list.words {
        hash ^= *word as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash ^= revision.rotate_left(7);
    let hash_ms = crate::media::platform::time::now_ms().saturating_sub(hash_t0);
    let seen = SEEN.swap(true, Ordering::AcqRel);
    let changed = !(seen && LAST_HASH.load(Ordering::Acquire) == hash);
    if changed {
        LAST_HASH.store(hash, Ordering::Release);
    }
    /* 日志开着时，每 60 帧记一行：这一帧绘制字数量 + 有没有真的重绘。
     * 用来判断"卡"在哪一层（words 就是 DrawList 的大小，1 个字 = 1 个绘制参数）。 */
    if log::enabled() {
        let tick = FRAME_TICKS.fetch_add(1, Ordering::AcqRel) + 1;
        if tick % 60 == 0 {
            log::append(&format!(
                "frame: words={} draw_ms={} hash_ms={} raster_rev={} present={}",
                list.words.len(),
                draw_ms,
                hash_ms,
                revision,
                if changed { 1 } else { 0 }
            ));
            crate::media::native_text::log_window();
        }
    }
    /* 限帧：把剩下的时间睡掉（见文件头的说明）。第一帧不睡，免得启动就慢半拍。 */
    let now_ms = crate::media::platform::time::now_ms();
    let prev_ms = LAST_FRAME_MS.swap(now_ms, Ordering::AcqRel);
    if prev_ms != 0 {
        let elapsed = now_ms.saturating_sub(prev_ms);
        if elapsed < FRAME_PERIOD_MS {
            unsafe {
                vitasdk_sys::sceKernelDelayThread(((FRAME_PERIOD_MS - elapsed) * 1000) as u32)
            };
        }
    }
    changed
}
