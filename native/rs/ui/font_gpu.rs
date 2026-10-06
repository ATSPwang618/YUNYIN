//! 流式 CJK（CJK STREAM）字形提交后，把新格子刷进 GPU 图集。
//!
//! 上游 PocketJS 0.12.0 只在 PSP / WASM 上实现了 streamed glyphs
//! （见 docs/DYNAMIC_TEXT.md 与 contracts/spec/platforms.ts：vita 的能力表里
//! 只有 text.glyphs.baked）。Vita 宿主缺的正是最后一步 —— 提交后刷新纹理：
//! `graphics.rs` 画字形时会跳过 `gid >= font.glyph_count`，而这个值只在加载
//! baked 图集时写过，于是流式字形"有字宽、没字墨"。
//!
//! 这里的策略（对应实机性能要求）：
//! 1. 按 **每个槽自己的字体 revision** 判断，只有该槽的字形变了才刷新
//!    —— 图片/样式变化推高的是 raster_revision，不再连带重传字体纹理；
//! 2. 该槽内部只上传**变化的字形**（宿主给出 entry 下标区间），不重写整个
//!    流式区域；只有刚申请容量、纹理几何变了才退回全量重建；
//! 3. 真正写纹理内存之前等上一帧 GPU 画完（vita2d_wait_rendering_done），
//!    避免写到正在被采样的格子。

use core::sync::atomic::{AtomicU64, Ordering};
use pocketjs_core::spec;

use crate::media::platform::log;

static LAST_REV: [AtomicU64; spec::MAX_FONT_SLOTS] =
    [const { AtomicU64::new(0) }; spec::MAX_FONT_SLOTS];

pub fn refresh_font_atlases() {
    let ui = unsafe { crate::ffi::ui() };
    for slot in 0..spec::MAX_FONT_SLOTS as u8 {
        let revision = ui.font_atlas_revision(slot);
        if revision == LAST_REV[slot as usize].load(Ordering::Acquire) {
            continue;
        }
        LAST_REV[slot as usize].store(revision, Ordering::Release);
        /* 先取变化范围再借 atlas：take_* 要 &mut Ui。 */
        let dirty = ui.take_font_stream_dirty(slot);
        let Some(atlas) = ui.font_atlas(slot) else {
            continue;
        };
        if log::enabled() {
            /* 日志开着才写：一眼能看出是不是"只传变化的那几格"。 */
            if dirty >= 0 {
                log::append(&format!(
                    "gpu: slot {} cells {}..{}",
                    slot,
                    dirty >> 16,
                    dirty & 0xffff
                ));
            } else {
                log::append(&format!("gpu: slot {} full refresh", slot));
            }
        }
        if !crate::graphics::refresh_font_atlas(slot, atlas, dirty) {
            crate::graphics::register_font_atlas(slot, atlas);
        }
    }
}
