"""宿主补丁（图形/字库）：流式 CJK、字形内缩、字图集 GPU 上传、dirty 区间。

这些补丁和 0.13 的渲染模型有冲突，正式包用 YUNYIN_BARE_GRAPHICS=1 整组跳过。"""

from .config import PKJ
from .patching import patch


def patch_streamed_cjk():
    """Vita is density-2; PocketJS 0.12.0 PJFA/font_stream is density-1.
    Keep the PFS1/PFG1/PFB1 protocol but allow coverage-sized cells at the
    host's raster density so STREAM CJK can land on Vita."""
    fs = PKJ / "engine/core/src/font_stream.rs"
    t = fs.read_text()
    t2 = t.replace(
        "            || b[14] != self.raster_density\n            || b[14] != 1\n            || b[13] == 0\n",
        "            || b[14] != self.raster_density\n            || b[13] == 0\n",
    )
    if t2 == t:
        print("[build-vpk] font_stream density-1 guard already patched or missing")
    t = t2
    old_budget = (
        "        if other + capacity * (a.cell_w.max(b[9] as u32) * a.cell_h.max(b[10] as u32)) as usize\n"
        "            > MAX_BYTES\n"
    )
    new_budget = (
        "        let cov_w = a.coverage_width().max(b[9] as u32);\n"
        "        let cov_h = a.coverage_height().max(b[10] as u32);\n"
        "        if other + capacity * (cov_w as usize) * (cov_h as usize) > MAX_BYTES\n"
    )
    if old_budget in t:
        t = t.replace(old_budget, new_budget)
        print("[build-vpk] patch: font_stream byte budget uses coverage")
    old_pad = (
        "        let old_w = self.cell_w as usize;\n"
        "        let old_h = self.cell_h as usize;\n"
        "        let cw = w.max(old_w);\n"
        "        let ch = h.max(old_h);\n"
        "        if cw * ch > MAX_PIXELS {\n"
        "            return false;\n"
        "        }\n"
        "        let mut pixels = vec![0; (base as usize + capacity) * cw * ch];\n"
        "        for g in 0..base as usize {\n"
        "            for y in 0..old_h {\n"
        "                pixels[g * cw * ch + y * cw..g * cw * ch + y * cw + old_w].copy_from_slice(\n"
        "                    &self.bitmap\n"
        "                        [g * old_w * old_h + y * old_w..g * old_w * old_h + (y + 1) * old_w],\n"
        "                );\n"
        "            }\n"
        "        }\n"
        "        self.bitmap = pixels;\n"
        "        self.cell_w = cw as u32;\n"
        "        self.cell_h = ch as u32;\n"
    )
    new_pad = (
        "        let old_w = self.cell_w as usize;\n"
        "        let old_h = self.cell_h as usize;\n"
        "        let d = self.raster_density as usize;\n"
        "        if d == 0 || w % d != 0 || h % d != 0 {\n"
        "            return false;\n"
        "        }\n"
        "        let old_cov_w = old_w * d;\n"
        "        let old_cov_h = old_h * d;\n"
        "        let new_w = (w / d).max(old_w);\n"
        "        let new_h = (h / d).max(old_h);\n"
        "        let new_cov_w = new_w * d;\n"
        "        let new_cov_h = new_h * d;\n"
        "        if new_cov_w * new_cov_h > MAX_PIXELS {\n"
        "            return false;\n"
        "        }\n"
        "        let mut pixels = vec![0; (base as usize + capacity) * new_cov_w * new_cov_h];\n"
        "        for g in 0..base as usize {\n"
        "            for y in 0..old_cov_h {\n"
        "                let from = g * old_cov_w * old_cov_h + y * old_cov_w;\n"
        "                let to = g * new_cov_w * new_cov_h + y * new_cov_w;\n"
        "                pixels[to..to + old_cov_w].copy_from_slice(&self.bitmap[from..from + old_cov_w]);\n"
        "            }\n"
        "        }\n"
        "        self.bitmap = pixels;\n"
        "        self.cell_w = new_w as u32;\n"
        "        self.cell_h = new_h as u32;\n"
    )
    if old_pad in t:
        t = t.replace(old_pad, new_pad)
        print("[build-vpk] patch: font_stream configure pads coverage cells")
    else:
        print("[build-vpk] font_stream pad block already patched or missing")
    old_dest = (
        "            let dest_cell = (self.cell_w * self.cell_h) as usize;\n"
        "            let dest = &mut self.bitmap[gid as usize * dest_cell..(gid as usize + 1) * dest_cell];\n"
        "            dest.fill(0);\n"
        "            entry.ink_width = 0;\n"
        "            for p in 0..cell {\n"
        "                let alpha = ((b[at + 8 + p / 4] >> (6 - 2 * (p % 4))) & 3) * 85;\n"
        "                dest[p / s.width * self.cell_w as usize + p % s.width] = alpha;\n"
    )
    new_dest = (
        "            let dest_w = self.cell_w as usize * self.raster_density as usize;\n"
        "            let dest_cell = dest_w * self.cell_h as usize * self.raster_density as usize;\n"
        "            let dest = &mut self.bitmap[gid as usize * dest_cell..(gid as usize + 1) * dest_cell];\n"
        "            dest.fill(0);\n"
        "            entry.ink_width = 0;\n"
        "            for p in 0..cell {\n"
        "                let alpha = ((b[at + 8 + p / 4] >> (6 - 2 * (p % 4))) & 3) * 85;\n"
        "                dest[p / s.width * dest_w + p % s.width] = alpha;\n"
    )
    if old_dest in t:
        t = t.replace(old_dest, new_dest)
        print("[build-vpk] patch: font_stream commit writes coverage")

    # density=2 的坑：ink_width 必须是"逻辑像素"宽度。原始代码用
    # `p % s.width`（位图列号）来量墨迹宽度，density=2 时 s.width 是逻辑宽的
    # 两倍；而这个值会存进 texture_cell_w，texture_coverage_width() 又会再乘
    # 一次 raster_density —— GPU 单元格于是变成两倍宽，流式字形采样落到点阵
    # 外面，画出来就是"有字宽、没字墨"（生僻字变空占位）。
    # 这里把列号先换算回逻辑列：logical_col = bitmap_col / density。
    old_ink = (
        "                if alpha != 0 {\n"
        "                    entry.ink_width = entry.ink_width.max((p % s.width + 1) as u32);\n"
        "                }\n"
    )
    new_ink = (
        "                if alpha != 0 {\n"
        "                    entry.ink_width = entry.ink_width.max(\n"
        "                        ((p % s.width) / self.raster_density as usize + 1) as u32,\n"
        "                    );\n"
        "                }\n"
    )
    if old_ink in t:
        t = t.replace(old_ink, new_ink, 1)
        print("[build-vpk] patch: font_stream ink_width in logical px (density 2)")
    else:
        print("[build-vpk] font_stream ink_width patch already applied or missing")

    # 诊断用：font_stream_stats() 除了原有的 resident/pending/rejected…，再多报
    #   inked      常驻字形里"真的有墨"的个数（0 = 字库读出来的点阵是空的）
    #   cellW/H、texCellW、density、base、capacity  图集几何
    # 只读计数，不改变任何绘制行为；正式版日志默认关，只有卡里有 debug 文件时才写。
    old_stats = (
        "        let (mut resident, mut bytes, mut pending, mut evictions, mut rejected, mut absent) =\n"
        "            (0, 0, 0, 0, 0, 0);\n"
        "        for slot in 0..crate::spec::MAX_FONT_SLOTS {\n"
        "            if let Some(a) = self.fonts.atlas(slot as u8) {\n"
        "                if let Some(s) = &a.stream {\n"
        "                    resident += s.entries.iter().filter(|e| e.cp != u32::MAX).count();\n"
        "                    bytes += a.stream_bytes();\n"
        "                    pending += s\n"
        "                        .wanted\n"
        "                        .iter()\n"
        "                        .filter(|cp| a.lookup(**cp).is_none() && !s.absent.contains(cp))\n"
        "                        .count();\n"
        "                    evictions += s.evictions;\n"
        "                    rejected += s.rejected;\n"
        "                    absent += s.absent.len();\n"
        "                }\n"
        "            }\n"
        "        }\n"
        "        format!(\"{{\\\"resident\\\":{},\\\"bytes\\\":{},\\\"pending\\\":{},\\\"evictions\\\":{},\\\"rejected\\\":{},\\\"unsupported\\\":{}}}\",resident,bytes,pending,evictions,rejected,absent)\n"
    )
    new_stats = (
        "        let (mut resident, mut bytes, mut pending, mut evictions, mut rejected, mut absent, mut inked) =\n"
        "            (0, 0, 0, 0, 0, 0, 0);\n"
        "        let (mut cell_w, mut cell_h, mut tex_cell_w, mut density, mut base, mut capacity) =\n"
        "            (0u32, 0u32, 0u32, 0u32, 0u32, 0u32);\n"
        "        for slot in 0..crate::spec::MAX_FONT_SLOTS {\n"
        "            if let Some(a) = self.fonts.atlas(slot as u8) {\n"
        "                if let Some(s) = &a.stream {\n"
        "                    let d = a.raster_density as usize;\n"
        "                    let cell = (a.cell_w as usize * d) * (a.cell_h as usize * d);\n"
        "                    if cell > 0 {\n"
        "                        for (i, e) in s.entries.iter().enumerate() {\n"
        "                            if e.cp == u32::MAX {\n"
        "                                continue;\n"
        "                            }\n"
        "                            let from = (s.base as usize + i) * cell;\n"
        "                            if a.bitmap.get(from..from + cell).map_or(false, |c| c.iter().any(|v| *v != 0)) {\n"
        "                                inked += 1;\n"
        "                            }\n"
        "                        }\n"
        "                    }\n"
        "                    resident += s.entries.iter().filter(|e| e.cp != u32::MAX).count();\n"
        "                    bytes += a.stream_bytes();\n"
        "                    pending += s\n"
        "                        .wanted\n"
        "                        .iter()\n"
        "                        .filter(|cp| a.lookup(**cp).is_none() && !s.absent.contains(cp))\n"
        "                        .count();\n"
        "                    evictions += s.evictions;\n"
        "                    rejected += s.rejected;\n"
        "                    absent += s.absent.len();\n"
        "                    cell_w = a.cell_w;\n"
        "                    cell_h = a.cell_h;\n"
        "                    tex_cell_w = a.texture_cell_w;\n"
        "                    density = a.raster_density as u32;\n"
        "                    base = s.base as u32;\n"
        "                    capacity = s.entries.len() as u32;\n"
        "                }\n"
        "            }\n"
        "        }\n"
        "        format!(\"{{\\\"resident\\\":{},\\\"bytes\\\":{},\\\"pending\\\":{},\\\"evictions\\\":{},\\\"rejected\\\":{},\\\"unsupported\\\":{},\\\"inked\\\":{},\\\"cellW\\\":{},\\\"cellH\\\":{},\\\"texCellW\\\":{},\\\"density\\\":{},\\\"base\\\":{},\\\"capacity\\\":{}}}\",resident,bytes,pending,evictions,rejected,absent,inked,cell_w,cell_h,tex_cell_w,density,base,capacity)\n"
    )
    if old_stats in t:
        t = t.replace(old_stats, new_stats, 1)
        print("[build-vpk] patch: font_stream_stats(+inked, +geometry)")
    else:
        print("[build-vpk] font_stream_stats patch already applied or missing")
    fs.write_text(t)

    fa = PKJ / "engine/core/src/font_archive.rs"
    a = fa.read_text()
    a2 = a.replace("                || s.density != 1\n", "                || (s.density != 1 && s.density != 2)\n")
    if a2 != a:
        print("[build-vpk] patch: PJFA reader allows density 2")
        fa.write_text(a2)

    spec = PKJ / "contracts/spec/font-archive.ts"
    s = spec.read_text()
    s2 = s.replace("      density !== 1 ||\n", "      (density !== 1 && density !== 2) ||\n")
    if s2 != s:
        print("[build-vpk] patch: decodeArchiveFace allows density 2")
        spec.write_text(s2)

    plat = PKJ / "contracts/spec/platforms.ts"
    p = plat.read_text()
    vita_cap = (
        '      "input.analog.left",\n'
        '      "input.buttons",\n'
        '      "input.cursor",\n'
        '      "input.touch",\n'
        '      "text.glyphs.baked",\n'
    )
    vita_cap_new = (
        '      "input.analog.left",\n'
        '      "input.buttons",\n'
        '      "input.cursor",\n'
        '      "input.touch",\n'
        '      "io.offload",\n'
        '      "text.glyphs.baked",\n'
        '      "text.glyphs.streamed",\n'
    )
    if vita_cap in p:
        p = p.replace(vita_cap, vita_cap_new, 1)
        plat.write_text(p)
        print("[build-vpk] patch: vita profile advertises streamed CJK + io.offload")
    elif "text.glyphs.streamed" in p.split("vita:")[1].split("pocketbook:")[0]:
        print("[build-vpk] vita streamed CJK already advertised")
    main = PKJ / "hosts/vita/src/main.rs"
    m = main.read_text()
    needle = "        if let Err(error) = runtime.frame_with_input(buttons, analog, &touches) {"
    inject = (
        "        pocketjs_vita::media::offload_local::frame();\n"
        "        if let Err(error) = runtime.frame_with_input(buttons, analog, &touches) {"
    )
    if "offload_local::frame" not in m and needle in m:
        m = m.replace(needle, inject, 1)
        main.write_text(m)
        print("[build-vpk] patch: main.rs offload_local::frame")


def patch_graphics_glyph():
    """Vita GPU glyph draw: inset each glyph's source rect by half a coverage
    texel so nearest-neighbour sampling never lands on the cell boundary and
    bleeds a thin white line at the text edge (the software rasterizer is
    already clean, so this is a GPU-only fix)."""
    f = PKJ / "hosts/vita/src/graphics.rs"
    t = f.read_text()
    old = (
        "                        let coverage_scale = SCALE / font.raster_density as f32;\n"
        "                        vita2d_draw_texture_tint_part_scale(\n"
        "                            font.texture.ptr,\n"
        "                            x,\n"
        "                            y,\n"
        "                            sx as f32,\n"
        "                            sy as f32,\n"
        "                            font.coverage_w as f32,\n"
        "                            font.coverage_h as f32,\n"
        "                            coverage_scale,\n"
        "                            coverage_scale,\n"
        "                            color,\n"
        "                        );"
    )
    new = (
        "                        let coverage_scale = SCALE / font.raster_density as f32;\n"
        "                        // Inset the source a half coverage texel on each side so\n"
        "                        // the POINT sampler never lands on the cell's boundary\n"
        "                        // texel (which bleeds a thin white line at the edge when\n"
        "                        // neighbours/padding are white). The scale is stretched to\n"
        "                        // keep the drawn glyph the same logical size.\n"
        "                        let inset = 0.5f32;\n"
        "                        let iw = font.coverage_w as f32 - inset * 2.0;\n"
        "                        let ih = font.coverage_h as f32 - inset * 2.0;\n"
        "                        let isx = sx as f32 + inset;\n"
        "                        let isy = sy as f32 + inset;\n"
        "                        let iscale_x = (font.coverage_w as f32 / iw) * coverage_scale;\n"
        "                        let iscale_y = (font.coverage_h as f32 / ih) * coverage_scale;\n"
        "                        vita2d_draw_texture_tint_part_scale(\n"
        "                            font.texture.ptr,\n"
        "                            x,\n"
        "                            y,\n"
        "                            isx,\n"
        "                            isy,\n"
        "                            iw,\n"
        "                            ih,\n"
        "                            iscale_x,\n"
        "                            iscale_y,\n"
        "                            color,\n"
        "                        );"
    )
    if new in t:
        print("[build-vpk] graphics.rs glyph inset already patched")
        return
    if old not in t:
        print("[build-vpk] WARN: graphics.rs glyph-inset pattern not found (PocketJS v0.12.0 layout may have changed); skipping")
        return
    f.write_text(t.replace(old, new, 1))
    print("[build-vpk] graphics.rs patched: inset glyph source rect")


# 流式 CJK 提交后刷新 GPU 图集用的宿主函数（追加到 graphics.rs 末尾）。
REFRESH_FONT_ATLAS_FN = r"""
/// 流式字形（CJK STREAM）提交后：把 gid >= baked 的格子就地写进已有纹理。
///
/// 上游 PocketJS 0.12.0 只在 PSP / WASM 上实现 streamed glyphs
/// （docs/DYNAMIC_TEXT.md：other native hosts need these operations before they
/// can advertise streamed glyph support），Vita 宿主缺的正是这一步：画字形时
/// `gid >= font.glyph_count` 会被直接跳过，而 glyph_count 只在加载 baked 图集
/// 时写过 —— 于是流式字形有字宽、没字墨（生僻字显示成空占位）。
///
/// 这里只重写**这次真正变化**的那几个字形格子（dirty 由宿主给出），不重建纹理
/// （Vita3K 的 GXM 模拟反复销毁纹理容易出问题）。宿主会在同一帧的多个
/// slot 更新前统一等待一次，避免每个 slot 都触发一次 GPU 同步。
/// 返回 false = 纹理不存在或几何对不上，调用方应改用 register_font_atlas()。
pub fn wait_for_font_atlas_refresh() {
    unsafe { vita2d_wait_rendering_done(); }
}

pub fn refresh_font_atlas(slot: u8, atlas: &Atlas, dirty: i64, wait: bool) -> bool {
    let coverage_w = atlas.coverage_width();
    let coverage_h = atlas.coverage_height();
    unsafe {
        let Some(font) = fonts().get(&slot) else {
            return false;
        };
        if font.glyph_count != atlas.glyph_count
            || font.coverage_w != coverage_w
            || font.coverage_h != coverage_h
            || font.cols == 0
        {
            return false;
        }
        /* 只上传变化的字形：dirty 是宿主给的 entry 下标闭区间（(lo<<16)|hi），
         * -1 表示"没有增量信息"（刚申请容量）→ 退回整个流式区域。 */
        let (from, to) = if dirty >= 0 {
            let lo = (dirty >> 16) as u16;
            let hi = (dirty & 0xffff) as u16;
            (
                font.baked.saturating_add(lo),
                font.baked.saturating_add(hi),
            )
        } else {
            (font.baked, font.glyph_count.saturating_sub(1))
        };
        if from >= font.glyph_count || to < from {
            return true;
        }
        let to = to.min(font.glyph_count - 1);
        /* 这些格子可能还在被采样。调用方通常已经在批量更新前等待过；
         * wait=true 只保留给单独调用此函数的兼容路径。 */
        if wait {
            vita2d_wait_rendering_done();
        }
        let stride = vita2d_texture_get_stride(font.texture.ptr) as usize;
        let dst = vita2d_texture_get_datap(font.texture.ptr) as *mut u8;
        if dst.is_null() {
            return false;
        }
        for gid in from..=to {
            let gx = (gid as u32 % font.cols) * coverage_w;
            let gy = (gid as u32 / font.cols) * coverage_h;
            let rows = atlas.glyph_rows(gid);
            for y in 0..coverage_h as usize {
                let src = rows.as_ptr().add(y * atlas.bytes_per_row());
                for x in 0..coverage_w as usize {
                    let out = dst.add((gy as usize + y) * stride + (gx as usize + x) * 4);
                    *out = 255;
                    *out.add(1) = 255;
                    *out.add(2) = 255;
                    *out.add(3) = *src.add(x);
                }
            }
        }
        true
    }
}
"""


def patch_font_gpu():
    """CJK STREAM：让 Vita 宿主在流式字形提交后刷新 GPU 图集。

    上游 v0.12.0 只在 PSP / WASM 上实现了 streamed glyphs（见
    docs/DYNAMIC_TEXT.md 与 contracts/spec/platforms.ts：vita 的能力表里
    只有 text.glyphs.baked）。Vita 宿主少了"提交后刷新纹理"这一步，画字形时
    `gid >= font.glyph_count` 被跳过 → 流式字形有字宽没字墨（空占位）。
    """
    f = PKJ / "hosts/vita/src/graphics.rs"
    t = f.read_text()

    old_struct = (
        "#[derive(Clone, Copy)]\n"
        "struct FontTexture {\n"
        "    texture: Texture,\n"
        "    glyph_count: u16,\n"
    )
    new_struct = (
        "#[derive(Clone, Copy)]\n"
        "struct FontTexture {\n"
        "    texture: Texture,\n"
        "    glyph_count: u16,\n"
        "    /// 第一次注册（baked 图集）时的字形数：gid >= baked 的都是流式字形。\n"
        "    baked: u16,\n"
    )
    # 注意：先判"已打过"。打过补丁后，old_struct 仍然是新文本的前缀，
    # 反过来判会重复插入一个 baked 字段（E0124）。
    if "gid >= baked" in t:
        print("[build-vpk] FontTexture.baked already patched")
    elif old_struct in t:
        t = t.replace(old_struct, new_struct, 1)
        print("[build-vpk] patch: FontTexture.baked")
    else:
        raise SystemExit("[build-vpk] graphics.rs FontTexture anchor not found")

    old_lit = (
        "        let font = FontTexture {\n"
        "            texture,\n"
        "            glyph_count: atlas.glyph_count,\n"
        "            coverage_w,\n"
    )
    new_lit = (
        "        let baked = fonts()\n"
        "            .get(&slot)\n"
        "            .map_or(atlas.glyph_count, |old| old.baked.min(old.glyph_count));\n"
        "        let font = FontTexture {\n"
        "            texture,\n"
        "            glyph_count: atlas.glyph_count,\n"
        "            baked,\n"
        "            coverage_w,\n"
    )
    if "let baked = fonts()" in t:
        print("[build-vpk] register_font_atlas baked already patched")
    elif old_lit in t:
        t = t.replace(old_lit, new_lit, 1)
        print("[build-vpk] patch: register_font_atlas keeps baked count")
    else:
        raise SystemExit("[build-vpk] register_font_atlas anchor not found")

    # 这段函数是追加在文件末尾的；已存在就整段换成新版（签名/内容会随版本变），
    # 否则旧版函数会和调用方对不上（参数个数不同）。
    marker = "/// 流式字形（CJK STREAM）提交后：把 gid >= baked 的格子就地写进已有纹理。"
    if marker in t:
        t = t[: t.index(marker)] + REFRESH_FONT_ATLAS_FN.lstrip("\n")
        print("[build-vpk] patch: graphics refresh_font_atlas() refreshed")
    else:
        t = t.rstrip() + "\n" + REFRESH_FONT_ATLAS_FN
        print("[build-vpk] patch: graphics refresh_font_atlas()")
    f.write_text(t)

    # main.rs 的帧循环补丁（refresh_font_atlases 钩子 + 帧跳过）不在这里 ——
    # 它和图形管线无关，0.13 正式包（BARE_GRAPHICS=1）也要打，
    # 所以拆去了 patches_host.patch_host_frame_loop()。


def patch_font_dirty():
    """让 core 记录"这次到底写了哪几个流式字形"，宿主才能只上传那几个格子。

    上游的 Stream 只 bump 一个整体 revision，宿主只能整片重传。这里加一对
    dirty_lo/dirty_hi（entry 下标区间），提交时顺手标一下；再由 Ui 暴露
    take_font_stream_dirty(slot) 取走并清空（查询即清除，天然合并同一帧的多次提交）。
    """
    fs = PKJ / "engine/core/src/font_stream.rs"
    t = fs.read_text()

    old_struct = (
        "    evictions: u64,\n"
        "    rejected: u64,\n"
        "}\n"
    )
    new_struct = (
        "    evictions: u64,\n"
        "    rejected: u64,\n"
        "    /// 上次被宿主取走的、发生变化的最小/最大 entry 下标。\n"
        "    pub(crate) dirty_lo: Cell<usize>,\n"
        "    pub(crate) dirty_hi: Cell<usize>,\n"
        "}\n"
    )
    if "dirty_lo: Cell<usize>" in t:
        print("[build-vpk] font_stream dirty range already patched")
    elif old_struct in t:
        t = t.replace(old_struct, new_struct, 1)
        print("[build-vpk] patch: Stream dirty_lo/dirty_hi")
    else:
        raise SystemExit("[build-vpk] font_stream Stream struct anchor not found")

    old_init = (
        "            advance: b[13],\n"
        "            evictions: 0,\n"
        "            rejected: 0,\n"
        "        });\n"
    )
    new_init = (
        "            advance: b[13],\n"
        "            evictions: 0,\n"
        "            rejected: 0,\n"
        "            dirty_lo: Cell::new(usize::MAX),\n"
        "            dirty_hi: Cell::new(0),\n"
        "        });\n"
    )
    if old_init in t:
        t = t.replace(old_init, new_init, 1)
        print("[build-vpk] patch: Stream dirty range init")

    old_mark = (
        "            );\n"
        "            changed += 1;\n"
        "        }\n"
    )
    new_mark = (
        "            );\n"
        "            /* 记下这次真正写了哪个字形：宿主只上传这一段。 */\n"
        "            if index < s.dirty_lo.get() {\n"
        "                s.dirty_lo.set(index);\n"
        "            }\n"
        "            if index > s.dirty_hi.get() {\n"
        "                s.dirty_hi.set(index);\n"
        "            }\n"
        "            changed += 1;\n"
        "        }\n"
    )
    if "s.dirty_lo.set(index)" in t:
        print("[build-vpk] font_stream commit dirty mark already patched")
    elif old_mark in t:
        t = t.replace(old_mark, new_mark, 1)
        print("[build-vpk] patch: Stream commit marks dirty range")
    else:
        raise SystemExit("[build-vpk] font_stream commit anchor not found")
    fs.write_text(t)

    lib = PKJ / "engine/core/src/lib.rs"
    l = lib.read_text()
    old_getter = (
        "    pub fn font_atlas_revision(&self, slot: u8) -> u64 {\n"
        "        self.font_revisions.get(slot as usize).copied().unwrap_or(0)\n"
        "    }\n"
    )
    new_getter = old_getter + (
        "\n"
        "    /// 取走某个槽自上次调用以来变化的流式字形下标范围（entry 下标，闭区间，\n"
        "    /// 返回值 (lo << 16) | hi）。没有变化返回 -1；查询即清除。\n"
        "    pub fn take_font_stream_dirty(&mut self, slot: u8) -> i64 {\n"
        "        let Some(atlas) = self.fonts.atlas_mut(slot) else {\n"
        "            return -1;\n"
        "        };\n"
        "        let Some(stream) = atlas.stream.as_mut() else {\n"
        "            return -1;\n"
        "        };\n"
        "        let lo = stream.dirty_lo.replace(usize::MAX);\n"
        "        let hi = stream.dirty_hi.replace(0);\n"
        "        if lo == usize::MAX || hi < lo {\n"
        "            return -1;\n"
        "        }\n"
        "        ((lo as i64) << 16) | (hi as i64)\n"
        "    }\n"
    )
    if "take_font_stream_dirty" in l:
        print("[build-vpk] Ui::take_font_stream_dirty already patched")
    elif old_getter in l:
        lib.write_text(l.replace(old_getter, new_getter, 1))
        print("[build-vpk] patch: Ui::take_font_stream_dirty")
    else:
        raise SystemExit("[build-vpk] lib.rs font_atlas_revision anchor not found")


def patch_font_cache():
    """Reduce stream-font allocation churn and coalesce invalidation per frame.

    The upstream stream protocol is kept intact.  Admission reuses the sorted
    wanted vector instead of cloning/sorting a new union for every TextResource,
    and several glyph commits in one Ui frame share one raster invalidation.
    """
    fs = PKJ / "engine/core/src/font_stream.rs"
    t = fs.read_text()
    old_union = (
        "            let mut union = s.wanted.clone();\n"
        "            union.extend_from_slice(&scalars);\n"
        "            union.sort_unstable();\n"
        "            union.dedup();\n"
        "            if union.len() > s.entries.len() {\n"
        "                return -2;\n"
        "            }\n"
        "            let s = self.stream.as_mut().unwrap();\n"
        "            s.wanted = union;\n"
        "            s.leases.push(Lease { id, scalars });\n"
    )
    new_union = (
        "            /* Keep wanted sorted in place.  The old clone + sort path\n"
        "             * allocated a full union for every visible text node. */\n"
        "            let additional = scalars\n"
        "                .iter()\n"
        "                .filter(|cp| s.wanted.binary_search(cp).is_err())\n"
        "                .count();\n"
        "            if s.wanted.len() + additional > s.entries.len() {\n"
        "                return -2;\n"
        "            }\n"
        "            let s = self.stream.as_mut().unwrap();\n"
        "            for cp in &scalars {\n"
        "                if let Err(at) = s.wanted.binary_search(cp) {\n"
        "                    s.wanted.insert(at, *cp);\n"
        "                }\n"
        "            }\n"
        "            s.leases.push(Lease { id, scalars });\n"
    )
    if "Keep wanted sorted in place" in t:
        print("[build-vpk] font stream wanted-vector reuse already patched")
    elif old_union in t:
        t = t.replace(old_union, new_union, 1)
        print("[build-vpk] patch: font stream wanted-vector reuse")
    else:
        raise SystemExit("[build-vpk] font stream wanted union anchor not found")

    old_init = (
        "            wanted: Vec::new(),\n"
        "            leases: Vec::new(),\n"
    )
    new_init = (
        "            wanted: Vec::with_capacity(capacity),\n"
        "            leases: Vec::with_capacity(MAX_LEASES),\n"
    )
    if "wanted: Vec::with_capacity(capacity)" in t:
        print("[build-vpk] font stream cache reserve already patched")
    elif old_init in t:
        t = t.replace(old_init, new_init, 1)
        print("[build-vpk] patch: font stream cache reserve")
    else:
        raise SystemExit("[build-vpk] font stream cache init anchor not found")
    fs.write_text(t)

    lib = PKJ / "engine/core/src/lib.rs"
    l = lib.read_text()
    old_field = "    font_revisions: [u64; spec::MAX_FONT_SLOTS],\n"
    new_field = old_field + (
        "    /// Last frame that already invalidated raster output for streamed glyph commits.\n"
        "    font_stream_raster_frame: u64,\n"
    )
    if "font_stream_raster_frame" not in l:
        if old_field not in l:
            raise SystemExit("[build-vpk] Ui font revision field anchor not found")
        l = l.replace(old_field, new_field, 1)
        old_init_field = "            font_revisions: [0; spec::MAX_FONT_SLOTS],\n"
        new_init_field = old_init_field + "            font_stream_raster_frame: u64::MAX,\n"
        if old_init_field not in l:
            raise SystemExit("[build-vpk] Ui font revision init anchor not found")
        l = l.replace(old_init_field, new_init_field, 1)
        print("[build-vpk] patch: coalesced font raster invalidation")
    else:
        print("[build-vpk] coalesced font raster invalidation already patched")

    old_commit = (
        "        if n > 0 {\n"
        "            self.font_revisions[slot as usize] = self.font_revisions[slot as usize].wrapping_add(1);\n"
        "            self.mark_layout_dirty();\n"
        "            self.bump_raster_revision();\n"
        "        }\n"
    )
    new_commit = (
        "        if n > 0 {\n"
        "            self.font_revisions[slot as usize] = self.font_revisions[slot as usize].wrapping_add(1);\n"
        "            self.mark_layout_dirty();\n"
        "            /* Several offload replies may land in one guest frame.\n"
        "             * Layout is already dirty; invalidate raster output once. */\n"
        "            if self.font_stream_raster_frame != self.frame {\n"
        "                self.font_stream_raster_frame = self.frame;\n"
        "                self.bump_raster_revision();\n"
        "            }\n"
        "        }\n"
    )
    if "Several offload replies may land in one guest frame" in t:
        print("[build-vpk] font raster invalidation coalescing already patched")
    elif old_commit in t:
        t = t.replace(old_commit, new_commit, 1)
        print("[build-vpk] patch: font raster invalidation coalescing")
    else:
        raise SystemExit("[build-vpk] font stream commit anchor not found")
    fs.write_text(t)
    lib.write_text(l)


def patch_stream_font_paging():
    """把一次字体预取扩大成 16 个字符的逻辑页，并按协议拆成小页提交。

    offload v1 的单条记录上限仍只能容纳 5 个 26x36 字形（返回值是 hex），
    所以这里不冒险放大所有 IO 缓冲，而是把 16 个 miss 先固定成一个逻辑页，
    再切成最多 5 个字形的 wire page。这样可以减少需求扫描和 UI 变更次数，
    同时保留现有 companion/local provider 的协议兼容性。
    """
    f = PKJ / "framework/src/fonts.ts"
    t = f.read_text()
    if "const LOGICAL_PAGE_SIZE = 16;" in t:
        print("[build-vpk] streamed font logical paging already patched")
        return

    old_decl = (
        "  const batches = new Set<Batch>(), requests = new Set<number>(), inflight = new Set<string>(),\n"
        "    retries = new Map<string, { frame: number; attempts: number }>();\n"
    )
    new_decl = (
        "  type GlyphPage = { slot: number; scalars: number[]; keys: string[] };\n"
        "  const LOGICAL_PAGE_SIZE = 16;\n"
        "  const batches = new Set<Batch>(), requests = new Set<number>(), inflight = new Set<string>(),\n"
        "    queued = new Set<string>(), pageQueue: GlyphPage[] = [],\n"
        "    retries = new Map<string, { frame: number; attempts: number }>();\n"
    )
    if old_decl not in t:
        raise SystemExit("[build-vpk] fonts.ts paging declaration anchor not found")
    t = t.replace(old_decl, new_decl, 1)

    old_reset = "    requests.clear(); inflight.clear(); retries.clear(); opening = false;\n"
    new_reset = (
        "    requests.clear(); inflight.clear(); queued.clear(); pageQueue.length = 0;\n"
        "    retries.clear(); opening = false;\n"
    )
    if old_reset not in t:
        raise SystemExit("[build-vpk] fonts.ts paging reset anchor not found")
    t = t.replace(old_reset, new_reset, 1)

    start = t.index("  const step = () => {")
    end = t.index("  // Validate configuration", start)
    new_step = r'''  const step = () => {
    if (dead) return;
    frame++;
    const current = client.session();
    if (current !== session) { session = current; reset(); }
    if (!face) {
      if (status.state !== "error" && !opening) open(); // offload supplies a bounded unavailable timeout
      return;
    }
    let loading = false;
    for (const b of batches) {
      if (status.state === "ready") admit(b);
      loading ||= b.admitted && b.state.status === "pending";
    }
    if ((!loading && !pageQueue.length) || status.paused || status.state === "error") return;

    /* One logical page covers the visible window. The wire protocol still uses
     * F.maxBatch-sized pages so a glyph reply stays within the 4096-byte record. */
    if (!pageQueue.length) {
      if (requests.size >= 2) return;
      const demand = JSON.parse(host.fontStreamRequests!()) as number[][];
      const available = demand.filter(([g, s, cp]) => g === face!.generation && slots.includes(s) &&
        !inflight.has(`${s}:${cp}`) && !queued.has(`${s}:${cp}`) &&
        (retries.get(`${s}:${cp}`)?.frame ?? 0) <= frame);
      if (!available.length) return;
      const slot = [...new Set(available.map(r => r[1]))].sort((a, b) => a - b).find(s => s > lastSlot) ?? available[0][1];
      lastSlot = slot;
      const scalars = available.filter(r => r[1] === slot).slice(0, LOGICAL_PAGE_SIZE).map(r => r[2]);
      const strike = face.strikes.find(s => s.slot === slot)!;
      const packed = Math.ceil(strike.width * strike.height / 4), stride = 8 + packed;
      const wireCount = Math.min(F.maxBatch, Math.floor((1250 - 12) / stride));
      for (let i = 0; i < scalars.length; i += wireCount) {
        const pageScalars = scalars.slice(i, i + wireCount);
        const keys = pageScalars.map(cp => `${slot}:${cp}`);
        pageQueue.push({ slot, scalars: pageScalars, keys });
        keys.forEach(k => queued.add(k));
      }
    }
    if (requests.size >= 2 || !pageQueue.length) return;
    const page = pageQueue.shift()!;
    const { slot, scalars, keys } = page;
    const strike = face.strikes.find(s => s.slot === slot)!;
    const packed = Math.ceil(strike.width * strike.height / 4), stride = 8 + packed;
    const token = serial;
    keys.forEach(k => { queued.delete(k); inflight.add(k); });
    const id = client.request("font.glyphs", JSON.stringify({ generation: face.generation, slot, scalars }), result => {
      requests.delete(id);
      if (token !== serial) return;
      keys.forEach(k => inflight.delete(k));
      let error = "";
      try {
        if (!result.ok) throw new Error(result.error);
        const bytes = decodeHex(result.value), v = new DataView(bytes.buffer);
        if (bytes.length !== 12 + scalars.length * stride || v.getUint32(0, true) !== F.glyphMagic ||
            v.getUint32(4, true) !== face!.generation || bytes[8] !== slot || bytes[9] !== scalars.length ||
            bytes[10] !== strike.width || bytes[11] !== strike.height || scalars.some((cp, i) =>
              v.getUint32(12 + i * stride, true) !== cp || bytes[17 + i * stride] > strike.width ||
              bytes[18 + i * stride] > 1 || bytes[19 + i * stride] !== 0))
          throw new Error("Font reply does not match the requested batch");
        status.loaded += host.fontStreamCommit!(bytes);
        status.error = "";
      } catch (e) { error = String(e); status.error = error; }
      for (let i = 0; i < keys.length; i++) {
        const key = keys[i], cp = scalars[i];
        if (!error) { retries.delete(key); continue; }
        const waiting = [...batches].filter(b => b.value.slot === slot && b.state.status === "pending" && b.scalars.includes(cp));
        if (!waiting.length) { retries.delete(key); continue; }
        const attempts = (retries.get(key)?.attempts ?? 0) + 1;
        if (attempts >= 3) {
          for (const b of waiting) publish(b, failed(new Error(error || "Glyph commit failed")));
          retries.delete(key);
        } else retries.set(key, { frame: frame + 30 * attempts, attempts });
      }
      /* One notification per wire page; Rust coalesces same-frame raster
       * invalidation and the next step drains the remaining pages. */
      refresh();
    });
    if (id) { requests.add(id); status.requests++; }
    else keys.forEach(k => inflight.delete(k));
  };
'''
    t = t[:start] + new_step + t[end:]
    f.write_text(t)
    print("[build-vpk] patch: streamed font logical page=16, wire pages=5")


def patch_stream_font_batch_limit():
    """把一次字体 offload 的 wire batch 从 4 扩到 5 个字形。

    Vita 当前 13x18 的 2-bit 字模每个字形占 234 字节，5 个字形编码后
    是 2444 个 hex 字符，仍在 offload 的 2500 字符上限内。原来的 4
    是按更保守的 1250 字节上限固定下来的，导致每个回复都更早触发一次
    host font atlas refresh；扩大到 5 可以减少刷新次数，同时仍由
    `wireCount` 按实际 strike 尺寸重新计算，遇到更大字模会自动退回。
    """
    fs = PKJ / "engine/core/src/font_stream.rs"
    t = fs.read_text()
    old = "pub const MAX_BATCH: usize = 4;"
    new = "pub const MAX_BATCH: usize = 5;"
    if old in t:
        t = t.replace(old, new, 1)
        print("[build-vpk] patch: streamed font MAX_BATCH=5")
    elif new in t:
        print("[build-vpk] streamed font MAX_BATCH=5 already patched")
    else:
        raise SystemExit("[build-vpk] font_stream MAX_BATCH anchor not found")
    fs.write_text(t)

    spec = PKJ / "contracts/spec/font-archive.ts"
    t = spec.read_text()
    old = "  maxBatch: 4,"
    new = "  maxBatch: 5,"
    if old in t:
        t = t.replace(old, new, 1)
        print("[build-vpk] patch: font archive maxBatch=5")
    elif new in t:
        print("[build-vpk] font archive maxBatch=5 already patched")
    else:
        raise SystemExit("[build-vpk] font archive maxBatch anchor not found")
    spec.write_text(t)

    fonts = PKJ / "framework/src/fonts.ts"
    t = fonts.read_text()
    old = "      const wireCount = Math.min(F.maxBatch, Math.floor((1250 - 12) / stride));"
    new = (
        "      /* The reply is hex encoded: keep the binary batch under the "
        "2500-char payload limit. */\n"
        "      const wireCount = Math.min(F.maxBatch, Math.floor((2500 / 2 - 12) / stride));"
    )
    if old in t:
        t = t.replace(old, new, 1)
        print("[build-vpk] patch: streamed font wire budget uses payload limit")
    elif "2500 / 2 - 12" in t:
        print("[build-vpk] streamed font wire budget already patched")
    else:
        raise SystemExit("[build-vpk] fonts.ts wire budget anchor not found")
    fonts.write_text(t)

    host = PKJ / "hosts/vita/src/media/ui/cjk_host.rs"
    t = host.read_text()
    old = "count > 0 && count <= 4 && slot < 24"
    new = "count > 0 && count <= 5 && slot < 24"
    if old in t:
        t = t.replace(old, new, 1)
        print("[build-vpk] patch: Vita font request count=5")
    elif new in t:
        print("[build-vpk] Vita font request count=5 already patched")
    else:
        raise SystemExit("[build-vpk] Vita font request count anchor not found")
    host.write_text(t)

    local = PKJ / "hosts/vita/src/media/ui/offload_local.rs"
    t = local.read_text()
    if "pub cps: [u32; 4]" in t or "cps: [0; 4]" in t:
        t = t.replace("pub cps: [u32; 4]", "pub cps: [u32; 5]")
        t = t.replace("cps: [0; 4]", "cps: [0; 5]")
        print("[build-vpk] patch: Vita local glyph request storage=5")
    elif "pub cps: [u32; 5]" in t and "cps: [0; 5]" in t:
        print("[build-vpk] Vita local glyph request storage=5 already patched")
    else:
        raise SystemExit("[build-vpk] Vita local glyph request storage anchor not found")
    local.write_text(t)


def patch_stream_layout_cache():
    """让 core 也缓存 streamed glyph 的布局结果。

    原始 draw path 为了防止字形补齐后显示旧 gid，直接禁止 stream atlas
    使用布局缓存；但 `font_revisions[slot]` 已经在每次提交时递增，revision
    本身就是可靠的失效键。允许带缺字占位的布局先进入 LRU，字形提交后由
    revision 让它自然重算，避免列表每帧重新收集/排版整段 CJK 文本。
    """
    draw = PKJ / "engine/core/src/draw.rs"
    t = draw.read_text()
    old_lookup = (
        "        let mut cached = atlas.stream.is_none() && layouts.get(&node_slot)\n"
        "            .is_some_and(|entry| entry.key == key && entry.text == run);\n"
    )
    new_lookup = (
        "        /* font_revisions includes streamed glyph commits.  A stream atlas\n"
        "         * can therefore use the same node-local layout LRU: a pending\n"
        "         * tofu layout is invalidated as soon as the glyph arrives. */\n"
        "        let mut cached = layouts.get(&node_slot)\n"
        "            .is_some_and(|entry| entry.key == key && entry.text == run);\n"
    )
    if "font_revisions includes streamed glyph commits" in t:
        print("[build-vpk] streamed text layout cache already patched")
    elif old_lookup not in t:
        raise SystemExit("[build-vpk] draw.rs streamed layout lookup anchor not found")
    else:
        t = t.replace(old_lookup, new_lookup, 1)
        print("[build-vpk] patch: streamed text layout lookup cache")

    old_insert = (
        "            if atlas.stream.is_none() && self.fonts.misses.get() == misses &&\n"
        "                run.capacity() <= 256 && scratch.capacity() <= 256 {\n"
    )
    new_insert = (
        "            let stream_layout = atlas.stream.is_some();\n"
        "            if (stream_layout || self.fonts.misses.get() == misses) &&\n"
        "                run.capacity() <= 256 && scratch.capacity() <= 256 {\n"
    )
    if "let stream_layout = atlas.stream.is_some();" in t:
        print("[build-vpk] streamed text layout insert cache already patched")
    elif old_insert not in t:
        raise SystemExit("[build-vpk] draw.rs streamed layout insert anchor not found")
    else:
        t = t.replace(old_insert, new_insert, 1)
        print("[build-vpk] patch: streamed text layout insert cache")
    draw.write_text(t)
