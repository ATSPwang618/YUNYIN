//! Vita2D native text backend (the only runtime text renderer).
//!
//! PocketJS core already has a native-text DrawList path (`TEXT_RUN`).  This
//! module supplies the Vita-side measure/draw callbacks so plain, untracked
//! text bypasses the 2048x2048 PJFA atlas entirely.
//!
//! The runtime intentionally has one font path: Vita2D's PVF loader.  There is
//! no TTF fallback, so a missing or invalid PVF is reported as `disabled`
//! instead of silently reintroducing a second renderer.

use alloc::boxed::Box;
use alloc::ffi::CString;
use alloc::format;
use alloc::string::String;
use alloc::collections::BTreeMap;
use core::ffi::c_char;
use core::hash::{Hash, Hasher};
use core::ptr;
use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::collections::hash_map::DefaultHasher;
use std::sync::Mutex;

use pocketjs_core::text::MeasureFn;
use vita2d_sys::*;

/// PocketJS lays out the scene at 480x272 and vita2d draws at 960x544.
const LOGICAL_TO_PHYSICAL: f32 = 2.0;
/// This is the raster size used by upstream libvita2d before its public
/// scale is applied.  Keep it only as a calibration constant: each native
/// face below is rasterized at the effective size of one UI slot, so the
/// draw call itself can stay at scale 1.0.
const PVF_BASE_CHAR_SIZE: f32 = 10.125;
const PVF_SCALE_REFERENCE: f32 = 16.0;
/// Source Han Sans' visible ink reaches a little above the line advance
/// reported by vita2d_pvf_text_height.  A 1.0 ratio keeps the first line
/// inside a fixed two-line row instead of clipping its top pixels.
/// vita2d's PVF API exposes a baseline draw call but no ascent query, so keep
/// the font metrics in one explicit place instead of treating text height as
/// the baseline offset.
const PVF_ASCENT_RATIO: f32 = 1.0;
/// The PVF bitmap includes antialiased edge pixels outside the nominal CSS
/// slot. Text rows use a 16px logical line box for the 12px glyphs; a 32px
/// item gives two lines exactly enough room while preserving the compact list.
const PVF_LINE_BOX_EXTRA: f32 = 4.0;

extern "C" {
    /// Added by the tiny libvita2d PVF patch.  It changes the PVF rasterizer
    /// size before any glyph enters that face's atlas.
    fn vita2d_pvf_set_char_size(font: *mut vita2d_pvf, size: f32) -> i32;
    fn vita2d_pvf_get_glyph_stats(
        font: *mut vita2d_pvf,
        glyphs: *mut u32,
        failures: *mut u32,
    );
}

const PVF_FACE_SIZES: [u32; 2] = [12, 16];

#[derive(Clone, Copy)]
struct PvfFace {
    font: *mut vita2d_pvf,
    size_px: u32,
}

#[derive(Clone, Copy)]
struct PvfBackend {
    faces: [PvfFace; PVF_FACE_SIZES.len()],
}

#[derive(Clone, Copy)]
enum Backend {
    Pvf(PvfBackend),
}

static mut BACKEND: Option<Backend> = None;
const MODE_NONE: u8 = 0;
const MODE_PVF: u8 = 1;
static BACKEND_MODE: AtomicU8 = AtomicU8::new(MODE_NONE);
static TEXT_RUNS: AtomicU64 = AtomicU64::new(0);
static TEXT_BYTES: AtomicU64 = AtomicU64::new(0);
static LEGACY_GLYPH_OPS: AtomicU64 = AtomicU64::new(0);
/* Native draw-list construction asks for the same width twice: once while
 * emitting TEXT_RUN and once again for alignment in draw_text().  Keep this
 * small process-lifetime cache bounded; it never owns glyph pixels. */
const WIDTH_CACHE_LIMIT: usize = 2048;
/* 键用 (face, 文本 hash) 而不是 String：命中时不再每次分配一个 String
 * （以前每帧每个文本要分配两次，翻页/滚动时就是一堆 malloc/free）。
 * 文本相同才认命中，hash 撞了也只是多量一次。 */
static WIDTH_CACHE: Mutex<BTreeMap<(usize, u64), (String, i32)>> =
    Mutex::new(BTreeMap::new());
/* 高度按 (face, 该 face 的栅格槽位尺寸) 存：两个不同字号的 slot 可能落在同一个
 * face 上，只按 face 存会把高度用错（基线偏）。 */
static HEIGHT_CACHE: Mutex<[(u32, i32); PVF_FACE_SIZES.len()]> =
    Mutex::new([(0, -1); PVF_FACE_SIZES.len()]);

fn text_hash(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

#[inline]
fn slot_px(slot: u8) -> u32 {
    match slot {
        0 | 7 | 16 => 12,
        1 | 8 | 17 => 14,
        2 | 9 | 18 => 16,
        3 | 10 => 18,
        4 | 11 => 20,
        5 | 12 => 24,
        6 | 13 => 36,
        14 | 15 => 54,
        _ => 16,
    }
}

#[inline]
fn face_index(slot: u8) -> usize {
    if slot_px(slot) <= PVF_FACE_SIZES[0] {
        0
    } else {
        1
    }
}

#[inline]
fn face_for_slot(backend: Backend, slot: u8) -> (PvfFace, f32) {
    match backend {
        Backend::Pvf(pvf) => {
            let face = pvf.faces[face_index(slot)];
            // 12px and 16px are the slots shipped by this app.  The 14px
            // slot, if a future screen uses it, is downsampled from the 16px
            // face rather than magnifying a smaller bitmap.
            (face, slot_px(slot) as f32 / face.size_px as f32)
        }
    }
}

#[inline]
fn text_cstring(text: &str) -> CString {
    // A NUL cannot be represented by vita2d's C API.  QuickJS text normally
    // never contains one, but replacing it here keeps one malformed run from
    // aborting the entire render pass.
    if text.as_bytes().contains(&0) {
        CString::new(text.replace('\0', " ")).unwrap()
    } else {
        CString::new(text).unwrap()
    }
}

#[inline]
unsafe fn current() -> Option<Backend> {
    BACKEND
}

unsafe fn try_load() -> Option<Backend> {
    let pvf_path = CString::new("app0:/fonts/yunyin.pvf").unwrap();
    let mut faces = [
        PvfFace {
            font: ptr::null_mut(),
            size_px: 0,
        };
        PVF_FACE_SIZES.len()
    ];
    for (index, size_px) in PVF_FACE_SIZES.iter().copied().enumerate() {
        let pvf = vita2d_load_custom_pvf(pvf_path.as_ptr() as *const c_char);
        if pvf.is_null() {
            for face in faces {
                if !face.font.is_null() {
                    vita2d_free_pvf(face.font);
                }
            }
            return None;
        }
        let raster_size = PVF_BASE_CHAR_SIZE
            * size_px as f32
            * LOGICAL_TO_PHYSICAL
            / PVF_SCALE_REFERENCE;
        if vita2d_pvf_set_char_size(pvf, raster_size) < 0 {
            vita2d_free_pvf(pvf);
            for face in faces {
                if !face.font.is_null() {
                    vita2d_free_pvf(face.font);
                }
            }
            return None;
        }
        faces[index] = PvfFace {
            font: pvf,
            size_px,
        };
    }
    Some(Backend::Pvf(PvfBackend { faces }))
}

/// Load the experimental font once and return the core's native measure
/// callback.  The Vita render thread owns this state for the lifetime of the
/// process; it intentionally survives guest switches with the rest of
/// vita2d's process-level initialization.
pub unsafe fn install() -> Option<MeasureFn> {
    if BACKEND.is_none() {
        BACKEND = try_load();
        BACKEND_MODE.store(
            match BACKEND {
                Some(Backend::Pvf(_)) => MODE_PVF,
                None => MODE_NONE,
            },
            Ordering::Release,
        );
    }
    if BACKEND.is_some() {
        Some(Box::new(|text, slot, _tracking, line_height| {
            measure(text, slot, line_height)
        }))
    } else {
        None
    }
}

/// Emit backend state after the application logger is initialized.  Loading
/// happens during host registration, before `log::init()` in older builds, so
/// logging from `try_load()` silently lost the most useful evidence.
pub fn log_status() {
    let mode = match BACKEND_MODE.load(Ordering::Acquire) {
        MODE_PVF => "pvf",
        _ => "disabled",
    };
    crate::media::log::append(&format!(
        "native-font: mode={mode} provider=vita2d text_run=1 legacy_glyph=0"
    ));
}

#[inline]
pub fn record_text_run(bytes: usize) {
    TEXT_RUNS.fetch_add(1, Ordering::Relaxed);
    TEXT_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
}

#[inline]
pub fn record_legacy_glyph_op() {
    LEGACY_GLYPH_OPS.fetch_add(1, Ordering::Relaxed);
}

/// Periodic counters make it possible to distinguish native text throughput
/// from an accidental fallback to the old atlas path on real hardware.
pub fn log_window() {
    let runs = TEXT_RUNS.swap(0, Ordering::AcqRel);
    let bytes = TEXT_BYTES.swap(0, Ordering::AcqRel);
    let legacy = LEGACY_GLYPH_OPS.swap(0, Ordering::AcqRel);
    let (glyphs12, failures12, glyphs16, failures16) = unsafe {
        match current() {
            Some(Backend::Pvf(pvf)) => {
                let mut glyphs = [0u32; PVF_FACE_SIZES.len()];
                let mut failures = [0u32; PVF_FACE_SIZES.len()];
                for (index, face) in pvf.faces.iter().enumerate() {
                    vita2d_pvf_get_glyph_stats(
                        face.font,
                        &mut glyphs[index],
                        &mut failures[index],
                    );
                }
                (glyphs[0], failures[0], glyphs[1], failures[1])
            }
            None => (0, 0, 0, 0),
        }
    };
    crate::media::log::append(&format!(
        "native-font: window text_runs={runs} text_bytes={bytes} legacy_glyph_ops={legacy} \
         atlas12_glyphs={glyphs12} atlas12_failures={failures12} \
         atlas16_glyphs={glyphs16} atlas16_failures={failures16}"
    ));
}

#[inline]
unsafe fn native_width(backend: Backend, slot: u8, text: *const c_char) -> i32 {
    let (face, scale) = face_for_slot(backend, slot);
    match backend {
        Backend::Pvf(_) => vita2d_pvf_text_width(face.font, scale, text),
    }
}

#[inline]
unsafe fn native_height(backend: Backend, slot: u8, text: *const c_char) -> i32 {
    let (face, scale) = face_for_slot(backend, slot);
    match backend {
        Backend::Pvf(_) => vita2d_pvf_text_height(face.font, scale, text),
    }
}

fn cached_width(backend: Backend, slot: u8, text: &str) -> i32 {
    let key = (face_index(slot), text_hash(text));
    if let Ok(cache) = WIDTH_CACHE.lock() {
        if let Some((stored, width)) = cache.get(&key) {
            if stored == text {
                return *width;
            }
        }
    }
    let c = text_cstring(text);
    let width = unsafe { native_width(backend, slot, c.as_ptr() as *const c_char) };
    if let Ok(mut cache) = WIDTH_CACHE.lock() {
        if cache.len() >= WIDTH_CACHE_LIMIT {
            /* 满了整表清掉（BTreeMap 没有 LRU 顺序）；2048 条之后很少走到这里。 */
            cache.clear();
        }
        cache.insert(key, (String::from(text), width));
    }
    width
}

fn glyph_line_height(backend: Backend, slot: u8) -> f32 {
    let face = face_index(slot);
    let px = slot_px(slot);
    let height = if let Ok(mut cache) = HEIGHT_CACHE.lock() {
        if cache[face].1 < 0 || cache[face].0 != px {
            let probe = text_cstring("Ag");
            cache[face] = (
                px,
                unsafe { native_height(backend, slot, probe.as_ptr() as *const c_char) },
            );
        }
        cache[face].1
    } else {
        let probe = text_cstring("Ag");
        unsafe { native_height(backend, slot, probe.as_ptr() as *const c_char) }
    };
    if height > 0 {
        height as f32 / LOGICAL_TO_PHYSICAL
    } else {
        slot_px(slot) as f32
    }
}

#[inline]
fn layout_line_height(slot: u8) -> f32 {
    slot_px(slot) as f32 + PVF_LINE_BOX_EXTRA
}

fn measure(text: &str, slot: u8, line_height: f32) -> (f32, f32) {
    let Some(backend) = (unsafe { current() }) else {
        return (0.0, 0.0);
    };
    if text.is_empty() {
        return (0.0, 0.0);
    }
    // The PVF helper reports its internal raster line advance (vsize * scale),
    // which is not the app's logical CSS line box.  Layout must stay stable at
    // the declared slot size, otherwise two sibling text nodes in a 30px row
    // are measured at roughly 7.6px each and overlap.  Keep the native metric
    // only for baseline placement in draw_text().
    let layout_lh = layout_line_height(slot);
    let lh = if line_height.is_nan() {
        layout_lh
    } else {
        line_height.max(layout_lh).max(1.0)
    };
    let mut max_width = 0.0f32;
    let mut lines = 0usize;
    for line in text.split('\n') {
        let width = cached_width(backend, slot, line);
        max_width = max_width.max(width.max(0) as f32 / LOGICAL_TO_PHYSICAL);
        lines += 1;
    }
    (max_width, lines as f32 * lh)
}

/// Decode and draw one TEXT_RUN. Coordinates and dimensions from core are
/// logical 480x272 values; vita2d renders in the physical 960x544 scene.
pub unsafe fn draw_text(
    slot: u8,
    x: f32,
    y: f32,
    box_width: f32,
    line_height: f32,
    align: u8,
    color: u32,
    text: &str,
) {
    let Some(backend) = current() else {
        return;
    };
    let (face, scale) = face_for_slot(backend, slot);
    let glyph_lh = glyph_line_height(backend, slot);
    let layout_lh = layout_line_height(slot);
    let lh = if line_height.is_nan() {
        layout_lh
    } else {
        line_height.max(layout_lh).max(1.0)
    };
    let ascent = glyph_lh * PVF_ASCENT_RATIO;
    let mut line_top = y;
    for line in text.split('\n') {
        let c = text_cstring(line);
        let width = cached_width(backend, slot, line).max(0) as f32
            / LOGICAL_TO_PHYSICAL;
        let dx = match align {
            1 => (box_width - width) * 0.5,
            2 => box_width - width,
            _ => 0.0,
        };
        // Core TEXT_RUN coordinates are the top-left of the laid-out line
        // box, while vita2d_pvf_draw_text consumes a physical baseline.
        let baseline = line_top + ((lh - glyph_lh).max(0.0) * 0.5) + ascent;
        let px = ((x + dx) * LOGICAL_TO_PHYSICAL).round() as i32;
        let py = (baseline * LOGICAL_TO_PHYSICAL).round() as i32;
        match backend {
            Backend::Pvf(_) => {
                vita2d_pvf_draw_text(
                    face.font,
                    px,
                    py,
                    color,
                    scale,
                    c.as_ptr() as *const c_char,
                );
            }
        }
        line_top += lh;
    }
}
