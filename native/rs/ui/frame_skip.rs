//! 画面没变就跳过整帧重绘。
//!
//! Vita host 原来每帧无条件走 `runtime.render() + graphics::present()`：
//! 重新构建顶点、提交 GXM、交换缓冲。暂停时 / 停在设置页时画面完全没动，
//! 这些工作是纯浪费（发热、耗电），播放时也只有进度条和五根柱子在动。
//!
//! 这里照桌面 host 的做法（hosts/desktop/src/main.rs）：把 **DrawList 的内容 +
//! raster_revision**（纹理/字体/样式内容的版本号）做成一个哈希，和上一帧一样
//! 就返回 false，宿主跳过这一帧的绘制。第一帧一定画（不然后面永远空屏）。

use alloc::format;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::media::platform::log;

static LAST_HASH: AtomicU64 = AtomicU64::new(0);
static SEEN: AtomicBool = AtomicBool::new(false);
static FRAME_TICKS: AtomicU32 = AtomicU32::new(0);

pub fn frame_changed() -> bool {
    let ui = unsafe { crate::ffi::ui() };
    /* 先读 revision：draw() 会借用 ui，之后不能再读。 */
    let revision = ui.raster_revision();
    let list = ui.draw();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for word in &list.words {
        hash ^= *word as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash ^= revision.rotate_left(7);
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
                "frame: words={} present={}",
                list.words.len(),
                if changed { 1 } else { 0 }
            ));
        }
    }
    changed
}
