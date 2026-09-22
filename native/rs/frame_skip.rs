//! 画面没变就跳过整帧重绘。
//!
//! Vita host 原来每帧无条件走 `runtime.render() + graphics::present()`：
//! 重新构建顶点、提交 GXM、交换缓冲。暂停时 / 停在设置页时画面完全没动，
//! 这些工作是纯浪费（发热、耗电），播放时也只有进度条和五根柱子在动。
//!
//! 这里照桌面 host 的做法（hosts/desktop/src/main.rs）：把 **DrawList 的内容 +
//! raster_revision**（纹理/字体/样式内容的版本号）做成一个哈希，和上一帧一样
//! 就返回 false，宿主跳过这一帧的绘制。第一帧一定画（不然后面永远空屏）。

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static LAST_HASH: AtomicU64 = AtomicU64::new(0);
static SEEN: AtomicBool = AtomicBool::new(false);

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
    if seen && LAST_HASH.load(Ordering::Acquire) == hash {
        return false;
    }
    LAST_HASH.store(hash, Ordering::Release);
    true
}
