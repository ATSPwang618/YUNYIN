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
/// （Vita3K 的 GXM 模拟反复销毁纹理容易出问题），写之前等上一帧 GPU 画完。
/// 返回 false = 纹理不存在或几何对不上，调用方应改用 register_font_atlas()。
pub fn refresh_font_atlas(slot: u8, atlas: &Atlas, dirty: i64) -> bool {
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
        /* 等上一帧 GPU 画完再改纹理内存：这些格子可能还在被采样。 */
        vita2d_wait_rendering_done();
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
