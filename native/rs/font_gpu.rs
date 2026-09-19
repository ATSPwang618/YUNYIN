//! 流式 CJK（CJK STREAM）字形提交后，把新格子刷进 GPU 图集。
//!
//! 上游 PocketJS 0.12.0 只在 PSP / WASM 上实现了 streamed glyphs
//! （见 docs/DYNAMIC_TEXT.md 与 contracts/spec/platforms.ts：vita 的能力表
//! 里只有 text.glyphs.baked）。Vita 宿主缺的正是最后一步 —— 提交后刷新纹理：
//! `graphics.rs` 画字形时会跳过 `gid >= font.glyph_count`，而这个值只在
//! 加载 baked 图集时写过，于是流式字形"有字宽、没字墨"（生僻字显示成空占位）。
//!
//! 这里每帧比对 `Ui::raster_revision()`：变了就把有变化的槽重新刷一遍。
//! 纹理几何对不上（例如流式容量刚申请、图集变大）时退回全量注册。

use core::sync::atomic::{AtomicU64, Ordering};
use pocketjs_core::spec;

static LAST_REVISION: AtomicU64 = AtomicU64::new(0);

pub fn refresh_font_atlases() {
    let ui = unsafe { crate::ffi::ui() };
    let revision = ui.raster_revision();
    if revision == LAST_REVISION.load(Ordering::Acquire) {
        return;
    }
    LAST_REVISION.store(revision, Ordering::Release);
    for slot in 0..spec::MAX_FONT_SLOTS as u8 {
        if let Some(atlas) = ui.font_atlas(slot) {
            if !crate::graphics::refresh_font_atlas(slot, atlas) {
                crate::graphics::register_font_atlas(slot, atlas);
            }
        }
    }
}
