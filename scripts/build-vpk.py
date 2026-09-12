#!/usr/bin/env python3
"""Yunyin - build the PS Vita VPK (PocketJS).

End-to-end pipeline:
  1. stage the app source into the PocketJS repo  (apps/yunyin)
  2. stage the native host media-decode patch       (hosts/vita)
  3. patch the Vita host so the media module compiles in
  4. bake a CJK font atlas (Noto Sans SC, all weights, density chosen to fit
     the 2048 texture cap while keeping cap-height glyphs intact)
  5. run the PocketJS Vita build (tools/build.ts + tools/vita.ts)
  6. repack the VPK with the app's TITLE_ID

Requires (inside the WSL2 distro):
  * VitaSDK  at /opt/vitasdk   (must include libSceAudiodec_stub.a)
  * bun      at /root/.bun/bin/bun
  * the PocketJS framework checkout at /root/pocketjs

Font versions (env vars):
  YUNYIN_FONT=chinese   fonts/chinese/NotoSansSC-Medium.ttf + fonts/chinese/chars.txt
  YUNYIN_FONT=japanese  fonts/japanese/MSMINCHO.TTF      + fonts/japanese/chars.txt
  YUNYIN_OUT=<name>     output name -> dist/<name>.vpk (default yunyin-main)
  YUNYIN_THEME=<skin>   startup skin baked first: light / dark / pure / anime
Build both font versions with scripts/build-variants.sh.
"""

import json
import math
import os
import re
import shutil
import struct
import subprocess
import zipfile
import zlib
from pathlib import Path

# --- config ---------------------------------------------------------------
PROJECT_ROOT = Path("/mnt/d/AI-PSVITA/yunyin")  # this project's root
PKJ = Path("/root/pocketjs")                    # PocketJS framework checkout
VITASDK = "/opt/vitasdk"
BUN = "/root/.bun/bin/bun"
APP_NAME = "yunyin"                            # pocketjs app dir name
APP_ID = "yunyin-main"                         # pocket.json -> app.output（框架产物名）
# 最终 VPK 文件名：同一份代码可以打包成多个字体版本
# （YUNYIN_OUT=yunyin-cn / yunyin-jp -> dist/<名字>.vpk）。
OUT = os.environ.get("YUNYIN_OUT", APP_ID)
APP_TITLE = "云音"                             # param.sfo TITLE（LiveArea 气泡下方显示名）
# param.sfo 里的 APP_VER（VitaShell 里看到的版本号），发布新版本时改这里
APP_VER = os.environ.get("YUNYIN_APP_VER", "00.50")
TITLE_ID = os.environ.get("YUNYIN_TITLE_ID", "")  # 留空 = 用 app/catalog.ts 的 TITLE_ID / PF2A47F97
THEME = os.environ.get("YUNYIN_THEME", "light")  # 皮肤主题：light / dark / pure / anime
# 字体主题：每套皮肤默认用一套字体（chinese / japanese），可单独用 YUNYIN_FONT 覆盖。
FONT_BY_THEME = {"light": "chinese", "dark": "japanese", "pure": "chinese", "anime": "japanese"}
FONT_THEME = os.environ.get("YUNYIN_FONT", FONT_BY_THEME.get(THEME, THEME))
DENSITY = 2                                    # see note in build_vpk()
PAD_SIZE = 0x1000                              # VitaSDK SCE-header layout pad (auto-adjusted)


FONT_NAMES = ("NotoSansSC-Medium.ttf", "NotoSansSC-Regular.ttf", "MSMINCHO.TTF",
              "font.ttf")


def theme_font():
    """Per-theme font file: fonts/<FONT_THEME>/<known name> if present, else any
    font file dropped in that folder, else the project default.

    日文版本要用 fonts/japanese/MSMINCHO.TTF：以前这里只找 Noto 的名字，日文
    字体文件根本不会被选中，两个"字体版本"实际用的是同一个字体。
    """
    base = PROJECT_ROOT / "fonts" / FONT_THEME
    for name in FONT_NAMES:
        cand = base / name
        if cand.exists():
            return str(cand)
    for pat in ("*.ttf", "*.ttc", "*.otf"):
        for cand in sorted(base.glob(pat)):
            return str(cand)
    # 兜底：中文版字体（fonts/chinese/ 那份，项目里只有这一份 Noto）
    return str(PROJECT_ROOT / "fonts" / "chinese" / "NotoSansSC-Medium.ttf")


# --- helpers --------------------------------------------------------------
def run(cmd, cwd=PKJ):
    e = dict(os.environ, HOME="/root", VITASDK=VITASDK, BUN_INSTALL="/root/.bun",
             PATH=f"{VITASDK}/bin:/root/.bun/bin:/root/.cargo/bin:" + os.environ.get("PATH", ""))
    print(">>>", " ".join(str(c) for c in cmd))
    subprocess.run([str(c) for c in cmd], cwd=str(cwd), env=e, check=True)


def patch(text, find, inject, desc):
    if inject in text:
        return text
    if find not in text:
        raise SystemExit(f"[build-vpk] pattern not found for {desc}: {find[:60]!r}")
    print(f"[build-vpk] patch: {desc}")
    return text.replace(find, find + inject)


# --- skin art normalisation ----------------------------------------------
# Every skin PNG is baked into a GPU texture, and the PocketJS texture format
# requires power-of-two dims with a hard 512px cap per side (contracts/spec
# TEX_MAX_DIM, re-checked by the engine when the texture is uploaded).  The
# art is authored at whatever size the drawing tool produced, so oversized /
# odd-sized skin PNGs are downscaled to the closest allowed size while
# *staging* — the project's own files are never touched.  Skin images are
# drawn stretched over their rect (`absolute inset-0 w-full h-full`), so the
# target aspect only decides how many texels the GPU gets, not the geometry.
TEX_MAX_DIM = 512
_POW2_DIMS = [1 << i for i in range(TEX_MAX_DIM.bit_length() - 1, -1, -1)]


def _is_pow2(n):
    return n > 0 and (n & (n - 1)) == 0


def _png_decode(path):
    """Reader for the format the skin art uses: 8-bit RGBA, non-interlaced.
    Returns (w, h, rgba bytearray)."""
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"not a PNG: {path}")
    pos, idat, hdr = 8, bytearray(), None
    while pos + 12 <= len(data):
        ln = int.from_bytes(data[pos:pos + 4], "big")
        typ = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + ln]
        pos += 12 + ln
        if typ == b"IHDR":
            hdr = struct.unpack(">IIBBBBB", body)
        elif typ == b"IDAT":
            idat += body
        elif typ == b"IEND":
            break
    if hdr is None:
        raise ValueError(f"no IHDR: {path}")
    w, h, depth, ctype, _comp, _filt, interlace = hdr
    if (depth, ctype, interlace) != (8, 6, 0):
        raise ValueError(f"unsupported PNG (depth={depth} ctype={ctype} "
                         f"interlace={interlace}): {path}")
    raw = zlib.decompress(bytes(idat))
    stride = w * 4
    out = bytearray(w * h * 4)
    prev = bytes(stride)
    p = 0
    for y in range(h):
        f = raw[p]
        p += 1
        line = bytearray(raw[p:p + stride])
        p += stride
        if f == 1:
            for i in range(4, stride):
                line[i] = (line[i] + line[i - 4]) & 0xFF
        elif f == 2:
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif f == 3:
            for i in range(stride):
                a = line[i - 4] if i >= 4 else 0
                line[i] = (line[i] + ((a + prev[i]) >> 1)) & 0xFF
        elif f == 4:
            for i in range(stride):
                a = line[i - 4] if i >= 4 else 0
                b = prev[i]
                c = prev[i - 4] if i >= 4 else 0
                pa = abs(b - c)
                pb = abs(a - c)
                pc = abs(a + b - 2 * c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[i] = (line[i] + pr) & 0xFF
        elif f != 0:
            raise ValueError(f"bad PNG filter {f}: {path}")
        out[y * stride:(y + 1) * stride] = line
        prev = line
    return w, h, out


def _png_encode(path, w, h, rgba):
    """Write 8-bit RGBA in a single filter-0 IDAT (what the baker reads back)."""
    stride = w * 4
    raw = bytearray()
    for y in range(h):
        raw.append(0)
        raw += rgba[y * stride:(y + 1) * stride]

    def chunk(typ, body):
        return (struct.pack(">I", len(body)) + typ + body +
                struct.pack(">I", zlib.crc32(typ + body) & 0xFFFFFFFF))

    ihdr = struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)
    path.write_bytes(b"\x89PNG\r\n\x1a\n" +
                     chunk(b"IHDR", ihdr) +
                     chunk(b"IDAT", zlib.compress(bytes(raw), 9)) +
                     chunk(b"IEND", b""))


def _tex_fit(w, h):
    """Closest allowed texture size: power-of-two per side, <= TEX_MAX_DIM and
    never upscaled.  Picked by aspect so the resample stays near-isotropic."""
    src_ar = w / h
    best = None
    for tw in _POW2_DIMS:
        if tw > w:
            continue
        for th in _POW2_DIMS:
            if th > h:
                continue
            key = (abs(math.log2((tw / th) / src_ar)), -(tw * th))
            if best is None or key < best[0]:
                best = (key, tw, th)
    return (best[1], best[2]) if best else (w, h)


def _box_downscale(rgba, w, h, tw, th):
    """Area-average downscale — the right low-pass for these scale factors.
    Averages premultiplied alpha so alpha edges don't pick up a dark fringe."""
    cols = [(x * w // tw, max(x * w // tw + 1, (x + 1) * w // tw))
            for x in range(tw)]
    rows = [(y * h // th, max(y * h // th + 1, (y + 1) * h // th))
            for y in range(th)]
    out = bytearray(tw * th * 4)
    for ty in range(th):
        y0, y1 = rows[ty]
        orow = ty * tw * 4
        for tx in range(tw):
            x0, x1 = cols[tx]
            r = g = b = a = n = 0
            for y in range(y0, y1):
                base = (y * w + x0) * 4
                for i in range(base, base + (x1 - x0) * 4, 4):
                    al = rgba[i + 3]
                    r += rgba[i] * al
                    g += rgba[i + 1] * al
                    b += rgba[i + 2] * al
                    a += al
                    n += 1
            o = orow + tx * 4
            out[o] = r // a if a else 0
            out[o + 1] = g // a if a else 0
            out[o + 2] = b // a if a else 0
            out[o + 3] = a // n if n else 0
    return out


def normalize_assets(root):
    """Downscale every staged skin PNG the Vita texture format can't take."""
    if not root.is_dir():
        return
    fixed = []
    for p in sorted(root.rglob("*.png")):
        try:
            w, h, rgba = _png_decode(p)
        except ValueError as exc:
            print(f"[build-vpk] asset left as-is: {exc}")
            continue
        if w <= TEX_MAX_DIM and h <= TEX_MAX_DIM and _is_pow2(w) and _is_pow2(h):
            continue
        tw, th = _tex_fit(w, h)
        if (tw, th) == (w, h):
            continue
        _png_encode(p, tw, th, _box_downscale(rgba, w, h, tw, th))
        fixed.append(f"{p.relative_to(root).as_posix()}: {w}x{h} -> {tw}x{th}")
    if fixed:
        print(f"[build-vpk] resized {len(fixed)} skin image(s) to the "
              f"{TEX_MAX_DIM}px power-of-two texture limit:")
        for line in fixed:
            print("           ", line)


def refresh_image_manifest(app_dst):
    """Keep the staged images.json in sync with the skin art.

    images.json only supplies per-image *metadata* (the linear flag the baker
    needs for art drawn scaled, and optional psm), so a missing or stale entry
    silently falls back to nearest sampling.  Every PNG shipped in the app is
    skinned art drawn scaled/rotated, so it wants linear sampling: add any
    asset that is missing, drop asset/ entries whose file is gone (the file
    used to carry stale `asset/ui/pur/...` and Windows-style `ui\\dark\\...`
    keys) and write the paths with forward slashes.
    """
    manifest = app_dst / "images.json"
    try:
        meta = json.loads(manifest.read_text(encoding="utf-8")) if manifest.exists() else {}
    except Exception as exc:
        print(f"[build-vpk] images.json unreadable ({exc}); rebuilding it")
        meta = {}
    out, dropped = {}, []
    for key, val in meta.items():
        name = key.replace("\\", "/")
        if name.startswith("asset/") and not (app_dst / name).exists():
            dropped.append(key)
            continue
        out[name] = val
    added = []
    for p in sorted((app_dst / "asset").rglob("*.png")):
        name = p.relative_to(app_dst).as_posix()
        if name not in out:
            out[name] = {"linear": True}
            added.append(name)
    manifest.write_text(json.dumps(out, ensure_ascii=False, indent=2) + "\n",
                        encoding="utf-8")
    print(f"[build-vpk] images.json: {len(out)} entries "
          f"(+{len(added)} added, -{len(dropped)} stale)")


# --- 1/2 stage source + native -------------------------------------------
def stage():
    app_dst = PKJ / "apps" / APP_NAME
    if app_dst.exists():
        shutil.rmtree(app_dst)
    # theme-seed.tsx is generated from colors.json and has to exist *before* the
    # app is copied: the baker only bakes class literals it can see in the app
    # sources, and the seed is what carries the per-theme text classes.  (It used
    # to be regenerated after staging, so colours added to colors.json never made
    # it into the stylesheet: the staged copy still had the previous seed.)
    make_theme_seed()
    shutil.copytree(PROJECT_ROOT / "app", app_dst)
    # Themeable UI skins (asset/ui/<theme>/): copy into the staged app so
    # tools/build.ts can find + bake them (appDir/<name> lookup).
    asset_src = PROJECT_ROOT / "asset"
    if asset_src.is_dir():
        shutil.copytree(asset_src, app_dst / "asset", dirs_exist_ok=True)
    normalize_assets(app_dst / "asset")
    refresh_image_manifest(app_dst)

    native = PKJ / "hosts/vita/native"
    if native.exists():
        shutil.rmtree(native)
    shutil.copytree(PROJECT_ROOT / "native", native)
    shutil.copy2(PROJECT_ROOT / "native" / "media.rs", PKJ / "hosts/vita/src/media.rs")

    # VitaSDK's vita-elf-create needs >= 2948 bytes of gap at the end of
    # segment 0 for its SCE header.  The bundled app.js rodata can leave
    # segment 0 ending right at a 4KB boundary (gap < 2948) which makes
    # vita-elf-create fail with "segment 1 overlaps".  This read-only pad
    # nudges segment 0 past the boundary so the SCE header fits.  It is
    # injected at build time only (the project's media.rs stays clean).
    media_dst = PKJ / "hosts/vita/src/media.rs"
    if "YUNYIN_ELF_PAD" not in media_dst.read_text():
        media_dst.write_text(media_dst.read_text() +
            "\n\n// build-vpk: VitaSDK SCE-header alignment pad (nudges segment 0 "
            "past a 4KB boundary so vita-elf-create can fit its SCE data).\n"
            "#[used]\n"
            f"static YUNYIN_ELF_PAD: [u8; {PAD_SIZE}] = [0u8; {PAD_SIZE}];\n")
    print("[build-vpk] staged app + native patch")


# --- 3 patch host ---------------------------------------------------------
def patch_host():
    lib = PKJ / "hosts/vita/src/lib.rs"
    t = lib.read_text()
    if "pub mod media;" not in t:
        t = patch(t, "pub mod vid;", "\npub mod media;", "lib.rs module")
    if "media::register(ctx" not in t:
        t = patch(t, "ffi::register(ctx, global, &textures, &sprites);",
                  "\n        media::register(ctx, global);", "lib.rs register")
    lib.write_text(t)

    cargo = PKJ / "hosts/vita/Cargo.toml"
    c = cargo.read_text()
    if "[build-dependencies]" not in c:
        c += "\n[build-dependencies]\ncc = \"1\"\n"
    if "SceAudiodec_stub" not in c:
        c = c.replace('features = ["SceAudio_stub"',
                      'features = ["SceAudiodec_stub", "SceAudio_stub"', 1)
    # 后台播放要用 sceShellUtilInitEvents()（先初始化 shell 事件系统，
    # 否则 appmgr 的应用事件接口会直接报错），它在这个 feature 后面。
    if "SceShellSvc_stub" not in c:
        c = c.replace('features = [', 'features = ["SceShellSvc_stub", ', 1)
    # 电源回调（息屏续播）要 scePowerRegisterCallback
    if "ScePower_stub" not in c:
        c = c.replace('features = [', 'features = ["ScePower_stub", ', 1)
    # 早期实验用过的 AppMgr feature 已经不需要（符号由默认链接提供），
    # 老环境里可能残留，顺手摘掉。
    for stale in ('"SceAppMgr_stub", ',):
        c = c.replace(stale, '')
    cargo.write_text(c)

    build = PKJ / "hosts/vita/build.rs"
    b = build.read_text()
    if "use std::path::{Path, PathBuf};" not in b:
        b = "use std::path::{Path, PathBuf};\n" + b
    marker = '    println!("cargo:rerun-if-env-changed=POCKETJS_CAPTURE_DIR");'
    if ("yunyin_listdir.c" not in b or "yplayer.c" not in b
            or "yunyin_shellsvc_stub.S" not in b
            or "empva_bridge" in b or "taihen_loader" in b):
        # Strip any previously injected yunyin_* cc blocks (they referenced
        # files we no longer ship; each cc block is guarded by .exists()).
        b = re.sub(
            r'\n    let native = Path::new\("native"\);'
            r'\n    if native\.join\("[a-z0-9_]+\.c"\)\.exists\(\) \{\n'
            r'.*?println!\("cargo:rerun-if-changed=native/[a-z0-9_]+\.c"\);\n'
            r'    \}',
            '', b, flags=re.S)
        # Strip the current cc block (wrapped in { let native = ... }) so we
        # don't accumulate duplicate block when new native sources are added.
        b = re.sub(
            r'\n    \{ let native = Path::new\("native"\);'
            r'.*?\n    \}',
            '\n', b, flags=re.S)
        blocks = (
            '\n    { let native = Path::new("native");'
            '\n      cc::Build::new().file(native.join("yplayer.c")).include(native)'
            '.define("YPLAYER", None).compile("yplayer");'
            '\n      cc::Build::new().file(native.join("yunyin_image.c")).include(native)'
            '.define("STBI_NO_STDIO", None).compile("yunyin_image");'
            '\n      cc::Build::new().file(native.join("yunyin_listdir.c")).include(native)'
            '.compile("yunyin_listdir");'
            '\n      cc::Build::new().file(native.join("yunyin_shellsvc_stub.S")).include(native)'
            '.compile("yunyin_shellsvc_stub");'
            '\n      println!("cargo:rustc-link-lib=mpg123");'
            '\n      println!("cargo:rustc-link-lib=vorbisfile");'
            '\n      println!("cargo:rustc-link-lib=vorbis");'
            '\n      println!("cargo:rustc-link-lib=ogg");'
            '\n      println!("cargo:rustc-link-lib=opusfile");'
            '\n      println!("cargo:rustc-link-lib=opus");'
            '\n      println!("cargo:rustc-link-search=native/libs");'
            '\n      println!("cargo:rerun-if-changed=native/yplayer.c");'
            '\n      println!("cargo:rerun-if-changed=native/yunyin_image.c");'
            '\n      println!("cargo:rerun-if-changed=native/yunyin_listdir.c");'
            '\n      println!("cargo:rerun-if-changed=native/yunyin_shellsvc_stub.S");'
            '\n      println!("cargo:rerun-if-changed=native/vendor");'
            '\n    }'
        )
        b = b.replace(marker, blocks + "\n" + marker)
    build.write_text(b)
    print("[build-vpk] host patched")


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
        raise SystemExit("[build-vpk] graphics.rs glyph-inset pattern not found")
    f.write_text(t.replace(old, new, 1))
    print("[build-vpk] graphics.rs patched: inset glyph source rect")


def app_const(name):
    src = PKJ / "apps" / APP_NAME / "catalog.ts"
    if not src.exists():
        return ""
    m = re.search(rf'export const {name}\s*=\s*"([^"]+)"', src.read_text())
    return m.group(1) if m else ""


def _elf_gap():
    """Return (seg0_end, seg1_start) from the just-linked Vita ELF, or None."""
    elf = PKJ / "hosts/vita/target/armv7-sony-vita-newlibeabihf/release/pocketjs-vita.elf"
    try:
        data = elf.read_bytes()
    except Exception:
        return None
    if data[:4] != b"\x7fELF":
        return None
    phoff = struct.unpack_from("<I", data, 0x1C)[0]
    phentsize = struct.unpack_from("<H", data, 0x2A)[0]
    phnum = struct.unpack_from("<H", data, 0x2C)[0]
    segs = []
    for i in range(phnum):
        off = phoff + i * phentsize
        if struct.unpack_from("<I", data, off)[0] != 1:  # PT_LOAD
            continue
        vaddr = struct.unpack_from("<I", data, off + 8)[0]
        memsz = struct.unpack_from("<I", data, off + 20)[0]
        segs.append((vaddr, vaddr + memsz))
    if len(segs) >= 2:
        return (segs[0][1], segs[1][0])
    return None


def _ffprobe():
    """Locate a working ffprobe: system PATH first, then the Windows ffmpeg
    mount (WSL builds often lack ffprobe, which would drop tag-only CJK chars
    like album names from the baked font)."""
    for cand in (
        "ffprobe",
        "/mnt/c/Program Files/ffmpeg/bin/ffprobe.exe",
        "/mnt/c/Program Files (x86)/ffmpeg/bin/ffprobe.exe",
        "/usr/bin/ffprobe",
        "/opt/vitasdk/bin/ffprobe",
    ):
        if cand.startswith("/"):
            if Path(cand).exists():
                return cand
        elif shutil.which(cand):
            return cand
    return None


def _id3_text_chars(path):
    """Collect every character that appears in the ID3v2 text/lyrics frames of a
    music file (TIT2/TPE1/TALB/USLT/T*).  No ffprobe needed, so it also works in
    WSL builds that don't have ffmpeg — this is what keeps CJK album/title chars
    (e.g. 叶惠美) in the baked font."""
    out = set()
    try:
        with open(path, "rb") as f:
            head = f.read(3 * 1024 * 1024)
    except Exception:
        return out
    if head[:3] != b"ID3":
        return out

    def synchsafe(b):
        return ((b[0] & 0x7f) << 21) | ((b[1] & 0x7f) << 14) | \
               ((b[2] & 0x7f) << 7) | (b[3] & 0x7f)

    ver = head[3]
    tag_size = synchsafe(head[6:10]) if len(head) >= 10 else 0
    pos = 10
    end = min(10 + tag_size, len(head))
    if head[5] & 0x40 and pos + 4 <= end:
        ext = synchsafe(head[pos:pos + 4]) if ver >= 4 else \
            int.from_bytes(head[pos:pos + 4], "big")
        pos = min(pos + max(ext, 4), end)
    text_frames = {
        b"TIT2", b"TPE1", b"TALB",
    }
    while pos + 10 <= end:
        if head[pos] == 0:
            break
        fid = head[pos:pos + 4]
        fsize = synchsafe(head[pos + 4:pos + 8]) if ver >= 4 else \
            int.from_bytes(head[pos + 4:pos + 8], "big")
        pos += 10
        if fsize == 0 or pos + fsize > end:
            break
        data = head[pos:pos + fsize]
        if data and fid in text_frames:
            enc = data[0]
            txt = data[1:]
            try:
                if enc == 3:
                    s = txt.decode("utf-8", "ignore")
                elif enc in (1, 2):
                    s = txt.decode("utf-16", "ignore")
                else:
                    s = txt.decode("latin-1", "ignore")
                for c in s:
                    if not c.isspace() and ord(c) >= 32 and c != "\ufeff":
                        out.add(c)
            except Exception:
                pass
        pos += fsize
    return out


def harvest_chars():
    """Sharpen the font.  At density 2 the Vita atlas limit shrinks to ~2520
    glyphs (a 17x24 logical cell becomes 34x48 coverage), so the broad GB2312
    set no longer fits.  Instead we bake ASCII + the characters actually present
    in the music library (filenames + ID3/Vorbis tags, re-harvested each build)
    + common UI symbols.  That keeps the atlas small enough for density 2 AND
    renders every song title/artist the library currently uses.
    """
    out = set(chr(i) for i in range(32, 127))  # ASCII always
    # The app scans ux0:/data/yunyin/music at runtime (the real/media folder is
    # hidden by SceIo), so the baked atlas must be harvested from the SAME
    # location.  Fall back to the older data/music / ux0:/music layouts.
    for music_root in (
        Path("/mnt/d/PSV/vita-game/ux0/data/yunyin/music"),
        Path("/mnt/d/PSV/vita-game/ux0/data/music"),
        Path("/mnt/d/PSV/vita-game/ux0/music"),
    ):
        if not music_root.is_dir():
            continue
        for f in music_root.iterdir():
            if not f.is_file():
                continue
            for c in f.name:
                if not c.isspace():
                    out.add(c)
            for c in _id3_text_chars(f):
                out.add(c)
    for c in "…♪♫♬★☆♥♡♠♣♦◆●◎○▲△▼▽→←↑↓·•—–「」『』【】（）《》〈〉＝，。％‰×□▢ ▶◀‖⇄↻≪≫‹›⟲⟳":
        out.add(c)
    # Bake every CJK ideograph that appears in the UI source, so hardcoded
    # Chinese labels (menus, hints, breadcrumbs) render instead of tofu.
    # ASCII + punctuation are already covered above; only Hanzi need this.
    try:
        ui = (PROJECT_ROOT / "app" / "app.tsx").read_text(encoding="utf-8", errors="ignore")
        for c in ui:
            if "\u4e00" <= c <= "\u9fff":
                out.add(c)
    except Exception:
        pass
    # Per-theme extended charset (e.g. a Japanese theme adds kana + kanji).
    out |= _theme_chars()
    harvest = "".join(sorted(out))
    return harvest[:2400]


def _theme_chars():
    """Per-theme extra chars for the baked font atlas.

    Reads fonts/<THEME>/chars.txt (or asset/ui/<THEME>/chars.txt); each line is
    either a run of literal characters or a hex codepoint range like:
        U+3040-U+30FF       (hiragana + katakana)
        4E00-9FFF           (CJK ideographs)
    '#' starts a comment.  Merged into the theme's atlas (capped by the 2048px
    texture limit, so list only the chars you actually need).
    """
    out = set()
    for base in (
        PROJECT_ROOT / "fonts" / FONT_THEME,
        PROJECT_ROOT / "fonts" / THEME,
        PROJECT_ROOT / "asset" / "ui" / THEME,
    ):
        p = base / "chars.txt"
        if not p.exists():
            continue
        try:
            for raw in p.read_text(encoding="utf-8", errors="ignore").splitlines():
                # 行尾注释要一起砍掉：写成 "U+3040-U+30FF   # 假名" 时旧代码
                # 匹配不到区间，只会把这一行的字面字符（U、+、3、0……和注释
                # 里的汉字）塞进字集，区间本身反而丢了。
                s = raw.split("#", 1)[0].strip()
                if not s:
                    continue
                m = re.match(
                    r"(?:U\+)?([0-9a-fA-F]{4,6})\s*-\s*(?:U\+)?([0-9a-fA-F]{4,6})$", s)
                if m:
                    a = int(m.group(1), 16)
                    b = int(m.group(2), 16)
                    if b - a > 0x2FFF:  # sanity cap a single range
                        b = a + 0x2FFF
                    for cp in range(a, b + 1):
                        if 0x20 <= cp <= 0x10FFFF:
                            out.add(chr(cp))
                else:
                    for c in s:
                        if not c.isspace():
                            out.add(c)
        except Exception:
            pass
    return out


def make_theme_seed():
    """Regenerate app/theme-seed.tsx from app/colors.json so every color class
    referenced in the JSON appears as a class= literal and gets baked by the
    PocketJS compiler (resolveStyle only matches baked literals)."""
    cfg = json.loads((PROJECT_ROOT / "app" / "colors.json").read_text(encoding="utf-8"))
    seen: list[str] = []
    for section in ("bg", "ui"):
        for theme in cfg.get(section, {}).values():
            for v in theme.values():
                if v and v not in seen:
                    seen.append(v)
    # 每个 <Text> 附一个空格子内容，避免被编译器当作空节点优化掉，
    # 从而确保这些完整 class 字面量真的进样式表（resolveStyle 才能命中）。
    lines = "\n".join(f'      <Text class="{v}"> </Text>' for v in seen)
    src = (
        "// auto-generated by scripts/build-vpk.py from app/colors.json (do not edit)\n"
        'import { View, Text } from "@pocketjs/framework/components";\n'
        "export default function ThemeSeed() {\n"
        "  return (\n"
        '    <View style={{ opacity: 0, width: 0, height: 0 }}>\n'
        f"{lines}\n"
        "    </View>\n"
        "  );\n"
        "}\n"
    )
    (PROJECT_ROOT / "app" / "theme-seed.tsx").write_text(src, encoding="utf-8")


# --- 4/5 bake + vite build ------------------------------------------------
def build_vpk():
    # Density 2 (raster 2 samples/logical px) renders sharp glyphs; a density-1
    # raster is upscaled 2x on the Vita surface and looks blurry.  Because the
    # atlas limit is 2048px we keep the charset small (ASCII + live music tags
    # + symbols) so a 17x24 cell at density 2 (34x48 coverage) fits.
    harvest = harvest_chars()
    extra = ["--extra-chars=" + harvest] if harvest else []
    font = theme_font()
    run([BUN, "tools/build.ts", APP_ID, f"--density={DENSITY}",
         f"--font-regular={font}", f"--font-bold={font}", f"--font-mono={font}"] + extra)
    # Optional frame-capture build for debugging (env YUNYIN_CAPTURE_FRAMES is
    # a comma list of frame numbers; output goes to ux0:data/pocketjs-captures).
    cap = os.environ.get("YUNYIN_CAPTURE_FRAMES", "")
    if cap:
        os.environ["POCKETJS_CAPTURE_FRAMES"] = cap
        os.environ["POCKETJS_CAPTURE_DIR"] = "ux0:data/pocketjs-captures"
        run([BUN, "tools/vita.ts", APP_NAME, "--release", "--skip-build",
             "--capture"])
    else:
        run([BUN, "tools/vita.ts", APP_NAME, "--release", "--skip-build"])


# --- 6 repack with app TITLE_ID ------------------------------------------
def repack():
    built = PKJ / "dist" / "vita" / f"{APP_ID}.vpk"
    if not built.exists():
        raise SystemExit(f"[build-vpk] vpk missing: {built}")
    staging = Path("/tmp/yunyin-vpk")
    shutil.rmtree(staging, ignore_errors=True)
    staging.mkdir()
    with zipfile.ZipFile(built) as z:
        z.extractall(staging)
    sce = PROJECT_ROOT / "app" / "sce_sys"
    if sce.exists():
        # A homebrew VPK must NOT carry retail PKG artifacts: sce_sys/package
        # (head.bin / work.bin licenses) and sce_sys/about (right.suprx).  Their
        # presence makes the Vita promoter treat it as a signed package and
        # fail with 0x80870005.  Icon / pic0 / livearea / manual are safe.
        shutil.copytree(
            sce, staging / "sce_sys", dirs_exist_ok=True,
            ignore=shutil.ignore_patterns("package", "about"))
    (staging / "sce_sys").mkdir(parents=True, exist_ok=True)
    title_id = TITLE_ID or app_const("TITLE_ID") or "PF2A47F97"
    # CATEGORY=gdc：把应用登记成"系统应用"分类。参考项目（ElevenMPV-A）就是这么
    # 做的 —— appmgr 的生命周期事件（激活/退出）只对这类应用发放，普通 homebrew
    # 分类（gd）调 sceAppMgrReceiveEventNum 会直接返回 0x8080201F。
    run([f"{VITASDK}/bin/vita-mksfoex", "-d", "ATTRIBUTE2=12",
         "-s", f"TITLE_ID={title_id}", "-s", "CATEGORY=gdc",
         "-s", f"APP_VER={APP_VER}",
         APP_TITLE, str(staging / "sce_sys/param.sfo")])
    # 用带权限的 authid 重新生成 eboot.bin。
    #
    # 框架默认用 vita-make-fself -s 生成"safe"eboot —— 那种 self 拿不到
    # appmgr 的系统级接口（实测全是 0x8080201F：应用生命周期事件、按 TITLE_ID
    # 查进程、带优先级的 BGM 端口申请都被拒）。官方 SDK 构建的应用（例如
    # ElevenMPV-A）带的是更高权限的 authid，所以它们能收到 REQUEST_QUIT，
    # 从而"用户关掉应用时停止播放"。vitasdk 的 vita-make-fself 支持同样的开关：
    #   -a : Authid for more permissions (SceShell: 0x2800000000000001)
    velf = (PKJ / "hosts/vita/target/armv7-sony-vita-newlibeabihf"
            / "release/pocketjs-vita.velf")
    if velf.exists():
        run([f"{VITASDK}/bin/vita-make-fself", "-a", "0x2800000000000001",
             str(velf), str(staging / "eboot.bin")])
        print("[build-vpk] eboot regenerated with authid 0x2800000000000001")
    else:
        print(f"[build-vpk] WARN: velf not found ({velf}), kept default eboot")
    out = PROJECT_ROOT / "dist" / f"{OUT}.vpk"
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
        for p in sorted(staging.rglob("*")):
            if p.is_file():
                z.write(p, p.relative_to(staging).as_posix())
    print(f"[build-vpk] VPK ready: {out} ({out.stat().st_size} bytes, title {title_id})")


if __name__ == "__main__":
    if not Path(theme_font()).exists():
        raise SystemExit(f"字体文件缺失：{theme_font()}")
    stage()
    patch_host()
    patch_graphics_glyph()
    try:
        build_vpk()
        repack()
    except subprocess.CalledProcessError:
        # VitaSDK's SCE header needs 2824 bytes of free space at the end of
        # segment 0.  As the JS bundle grows, segment 0 can end too close to a
        # 4KB boundary and vita-elf-create reports "segment 1 overlaps".
        # Shrink the injected rodata pad a little at a time and relink until it
        # fits, keeping a small safety buffer so one retry is enough.
        for _ in range(12):
            gap = _elf_gap()
            if not gap:
                raise
            seg0_end, seg1_start = gap
            deficit = 2824 - (seg1_start - seg0_end)
            if deficit <= 0:
                # No overlap now (e.g. a later relink happened) but build still
                # failed for another reason.
                raise
            shrink = deficit + 64
            PAD_SIZE = max(0, PAD_SIZE - shrink)
            print(
                f"[build-vpk] SCE overlap gap={seg1_start - seg0_end} need=2824 "
                f"-> pad 0x{PAD_SIZE:x} (-{shrink})"
            )
            stage()
            patch_host()
            patch_graphics_glyph()
            try:
                build_vpk()
                repack()
                break
            except subprocess.CalledProcessError:
                continue
        else:
            raise SystemExit("[build-vpk] SCE alignment still failing after retries")
