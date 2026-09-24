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
  * VitaSDK  at /opt/vitasdk   (must include libSceAudiodec_stub.a and
                               libSceSysmem_stub.a — M4A/AAC needs both)
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
PROJECT_ROOT = Path(__file__).resolve().parent.parent
PKJ = Path(os.environ.get("POCKETJS_ROOT", "/root/pocketjs"))                    # PocketJS framework checkout
VITASDK = os.environ.get("VITASDK", "/opt/vitasdk")
BUN = "/root/.bun/bin/bun"
APP_NAME = "yunyin"                            # pocketjs app dir name
APP_ID = "yunyin-main"                         # pocket.json -> app.output（框架产物名）
# 最终 VPK 文件名：同一份代码可以打包成多个字体版本
# （YUNYIN_OUT=yunyin-cn / yunyin-jp -> dist/<名字>.vpk）。
OUT = os.environ.get("YUNYIN_OUT", APP_ID)
APP_TITLE = "云音"                             # param.sfo TITLE（LiveArea 气泡下方显示名）
# param.sfo 里的 APP_VER（VitaShell 里看到的版本号），发布新版本时改这里
APP_VER = os.environ.get("YUNYIN_APP_VER", "00.86")
TITLE_ID = os.environ.get("YUNYIN_TITLE_ID", "")  # 留空 = 用 app/catalog.ts 的 TITLE_ID / PF2A47F97
THEME = os.environ.get("YUNYIN_THEME", "dark")  # 皮肤主题：light / dark / pure / anime
# 默认 Noto Sans SC。日文曲库才切 MSMINCHO：YUNYIN_FONT=japanese
# dark/anime 以前绑日文字体会让简体 UI（首页/专辑/设置）变成 □□□。
FONT_BY_THEME = {"light": "chinese", "dark": "chinese", "pure": "chinese", "anime": "chinese"}
FONT_THEME = os.environ.get("YUNYIN_FONT", FONT_BY_THEME.get(THEME, "chinese"))
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
    shutil.copytree(
        PROJECT_ROOT / "native",
        native,
        ignore=shutil.ignore_patterns("rs", "media.rs", "*.S"),
    )

    media_dst = PKJ / "hosts/vita/src/media"
    if media_dst.exists():
        shutil.rmtree(media_dst)
    old_flat = PKJ / "hosts/vita/src/media.rs"
    if old_flat.exists():
        old_flat.unlink()
    shutil.copytree(PROJECT_ROOT / "native" / "rs", media_dst)

    # VitaSDK's vita-elf-create needs >= 2948 bytes of gap at the end of
    # segment 0 for its SCE header.  Injected at build time only.
    mod_rs = media_dst / "mod.rs"
    text = mod_rs.read_text()
    if "YUNYIN_ELF_PAD" not in text:
        mod_rs.write_text(
            text
            + "\n\n// build-vpk: VitaSDK SCE-header alignment pad (nudges segment 0 "
            "past a 4KB boundary so vita-elf-create can fit its SCE data).\n"
            "#[used]\n"
            f"static YUNYIN_ELF_PAD: [u8; {PAD_SIZE}] = [0u8; {PAD_SIZE}];\n"
        )
    print("[build-vpk] staged app + native/rs -> hosts/vita/src/media/")


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
    # 需要的 stub：
    #   ScePower      电源 tick
    #   SceAppMgr     BGM 口（sceAppMgrAcquireBgmPort）
    #   SceShellSvc   锁 PS 键（sceShellUtilLock/Unlock）
    #   SceAudiodec   M4A 里的 AAC 硬件解码（sceAudiodecInitLibrary/CreateDecoder/Decode）
    #   SceSysmem     上面那条要的 uncached memblock（sceKernelAllocMemBlock 等）
    # 逐个补进 features 列表，重复构建也安全。
    for needed in ("ScePower_stub", "SceAppMgr_stub", "SceShellSvc_stub",
                   "SceAudiodec_stub", "SceSysmem_stub",
                   # Phase 0 network probe (native/net/yhttp.c)
                   "SceHttp_stub", "SceSsl_stub", "SceNet_stub",
                   "SceNetCtl_stub"):
        if f'"{needed}"' not in c:
            c = c.replace(
                'vitasdk-sys = { version = "0.3.3", features = [',
                f'vitasdk-sys = {{ version = "0.3.3", features = ["{needed}", ',
                1,
            )
    cargo.write_text(c)

    build = PKJ / "hosts/vita/build.rs"
    b = build.read_text()
    if "use std::path::{Path, PathBuf};" not in b:
        b = "use std::path::{Path, PathBuf};\n" + b
    marker = '    println!("cargo:rerun-if-env-changed=POCKETJS_CAPTURE_DIR");'
    # Rewrite the native block unless it already matches exactly what this
    # project ships.  An older experiment left the PocketJS checkout's block
    # referencing ym4a.c/yhttp.c files that stage() had already deleted, so the
    # stale block simply broke the next build; comparing against every file we
    # ship keeps the two in step.
    needs_cc = (
        "host/yunyin_listdir.c" not in b
        or "audio/yplayer.c" not in b
        or "audio/yp_io_file.c" not in b
        or "audio/ym4a.c" not in b
        or "audio/yaac.c" not in b
        or "net/yhttp.c" not in b
        or 'join("yhttp.c")' in b  # the abandoned flat-path block
        or "yunyin_shellsvc_stub.S" in b
        or "empva_bridge" in b
        or "taihen_loader" in b
        or 'cargo:rustc-link-lib=mpg123' not in b
    )
    if needs_cc:
        b = re.sub(
            r'\n    let native = Path::new\("native"\);'
            r'\n    if native\.join\("[a-z0-9_]+\.c"\)\.exists\(\) \{\n'
            r'.*?println!\("cargo:rerun-if-changed=native/[a-z0-9_]+\.c"\);\n'
            r'    \}',
            '', b, flags=re.S)
        b = re.sub(
            r'\n    \{ let native = Path::new\("native"\);'
            r'.*?\n    \}',
            '\n', b, flags=re.S)
        blocks = (
            '\n    { let native = Path::new("native");'
            # C sources are grouped: audio/ (player + M4A + AAC hardware),
            # host/ (image + directory listing + the shared log helper).
            # Both folders are include roots so a file can say "ym4a.h"
            # (same folder) or "vendor/dr_wav.h" (native/ root).
            '\n      let audio = native.join("audio");'
            '\n      let host = native.join("host");'
            '\n      cc::Build::new().file(audio.join("yplayer.c"))'
            '.include(native).include(&audio).include(&host)'
            '.define("YPLAYER", None).compile("yplayer");'
            # Phase 1：解码器的输入层（yp_io 的文件实现）
            '\n      cc::Build::new().file(audio.join("yp_io_file.c"))'
            '.include(native).include(&audio).include(&host)'
            '.compile("yp_io_file");'
            '\n      cc::Build::new().file(audio.join("ym4a.c"))'
            '.include(native).include(&audio).include(&host)'
            '.compile("ym4a");'
            '\n      cc::Build::new().file(audio.join("yaac.c"))'
            '.include(native).include(&audio).include(&host)'
            '.compile("yaac");'
            '\n      cc::Build::new().file(host.join("yunyin_image.c"))'
            '.include(native).include(&host)'
            '.define("STBI_NO_STDIO", None).compile("yunyin_image");'
            '\n      cc::Build::new().file(host.join("yunyin_listdir.c"))'
            '.include(native).include(&host)'
            '.compile("yunyin_listdir");'
            # Phase 0 transport probe: Vita-only (SceNet/SceSsl/SceHttp).
            '\n      let net = native.join("net");'
            '\n      cc::Build::new().file(net.join("yhttp.c"))'
            '.include(native).include(&net).include(&host)'
            '.compile("yhttp");'
            '\n      println!("cargo:rustc-link-lib=mpg123");'
            '\n      println!("cargo:rustc-link-lib=vorbisfile");'
            '\n      println!("cargo:rustc-link-lib=vorbis");'
            '\n      println!("cargo:rustc-link-lib=ogg");'
            '\n      println!("cargo:rustc-link-lib=opusfile");'
            '\n      println!("cargo:rustc-link-lib=opus");'
            '\n      println!("cargo:rustc-link-search=native/libs");'
            '\n      println!("cargo:rerun-if-changed=native/audio");'
            '\n      println!("cargo:rerun-if-changed=native/host");'
            '\n      println!("cargo:rerun-if-changed=native/net");'
            '\n      println!("cargo:rerun-if-changed=native/vendor");'
            '\n    }'
        )
        if marker not in b:
            raise SystemExit("[build-vpk] build.rs POCKETJS_CAPTURE_DIR marker not found")
        b = b.replace(marker, blocks + "\n" + marker)
    build.write_text(b)
    patch_streamed_cjk()
    print("[build-vpk] host patched (v0.12.0 anchors, no SceShellSvc)")


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


def bake_cjk_archive():
    """PJFA for STREAM mode. Cached at fonts/chinese/cjk.pjfa."""
    out = PROJECT_ROOT / "fonts" / "chinese" / "cjk.pjfa"
    chars = PROJECT_ROOT / "fonts" / "chinese" / "cjk-stream.txt"
    script = PROJECT_ROOT / "scripts" / "bake-cjk-archive.ts"
    if out.exists() and out.stat().st_size > 1024:
        print(f"[build-vpk] using cached PJFA {out} ({out.stat().st_size} bytes)")
        return out
    if not script.exists() or not chars.exists():
        print("[build-vpk] WARN: CJK archive script/charset missing, STREAM will fall back")
        return None
    run([BUN, str(script),
         f"--font={theme_font()}",
         f"--out={out}",
         "--slots=0,7,8",
         f"--chars={chars}",
         "--density=2"], cwd=PROJECT_ROOT)
    return out if out.exists() else None


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

    m = PKJ / "hosts/vita/src/main.rs"
    s = m.read_text()
    if "refresh_font_atlases()" not in s:
        old_tick = "        runtime.tick();\n"
        new_tick = (
            "        runtime.tick();\n"
            "        pocketjs_vita::media::refresh_font_atlases();\n"
        )
        if old_tick not in s:
            raise SystemExit("[build-vpk] main.rs runtime.tick anchor not found")
        m.write_text(s.replace(old_tick, new_tick, 1))
        print("[build-vpk] patch: main.rs refresh_font_atlases()")
    else:
        print("[build-vpk] main.rs refresh_font_atlases already patched")

    # 画面内容没变就跳过这一帧的 render+present（见 native/rs/ui/frame_skip.rs）。
    old_present = (
        "        pocketjs_vita::media::refresh_font_atlases();\n"
        "        runtime.render();\n"
        "        graphics::present();\n"
    )
    new_present = (
        "        pocketjs_vita::media::refresh_font_atlases();\n"
        "        if pocketjs_vita::media::frame_changed() {\n"
        "            runtime.render();\n"
        "            graphics::present();\n"
        "        }\n"
    )
    if "media::frame_changed()" in s:
        print("[build-vpk] main.rs frame_changed already patched")
    elif old_present in s:
        m.write_text(s.replace(old_present, new_present, 1))
        print("[build-vpk] patch: main.rs skip unchanged frames")
    else:
        print("[build-vpk] WARN: main.rs render/present anchor not found; frames not skipped")


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
    extra = set()
    # The app scans ux0:/data/yunyin/music at runtime (the real/media folder is
    # hidden by SceIo), so the baked atlas must be harvested from the SAME
    # location.  Fall back to the older data/music / ux0:/music layouts.
    for music_root in (
        Path("/mnt/d/PSV/vita-game/ux0/data/yunyin/music"),
        Path("/mnt/d/PSV/vita-game/ux0/data/music"),
        Path("/mnt/d/PSV/vita-game/ux0/music"),
        PROJECT_ROOT / "music",
    ):
        if not music_root.is_dir():
            continue
        for f in music_root.rglob("*"):
            if not f.is_file():
                continue
            for c in f.name:
                if not c.isspace():
                    extra.add(c)
            for c in _id3_text_chars(f):
                extra.add(c)
    for c in "…♪♫♬★☆♥♡♠♣♦◆●◎○▲△▼▽→←↑↓·•—–「」『』【】（）《》〈〉＝，。％‰×□▢ ▶◀‖⇄↻≪≫‹›⟲⟳":
        out.add(c)
    # Bake every CJK ideograph that appears in the UI source, so hardcoded
    # Chinese labels (menus, hints, breadcrumbs) render instead of tofu.
    # ASCII + punctuation are already covered above; only Hanzi need this.
    # These are reserved and never truncated by the 2400 cap.
    try:
        ui = (PROJECT_ROOT / "app" / "app.tsx").read_text(encoding="utf-8", errors="ignore")
        for c in ui:
            if "\u4e00" <= c <= "\u9fff":
                out.add(c)
    except Exception:
        pass
    extra |= _theme_chars()
    extra -= out
    harvest_must = "".join(sorted(out))
    harvest_extra = "".join(sorted(extra))
    room = max(0, 2400 - len(harvest_must))
    harvest = harvest_must + harvest_extra[:room]
    print(f"[build-vpk] font={FONT_THEME} reserved={len(harvest_must)} extra={len(harvest_extra)} baked={len(harvest)}")
    return harvest


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
    # 部分系统接口。vitasdk 的 vita-make-fself 支持：
    #   -a : Authid for more permissions (SceShell: 0x2800000000000001)
    # 播放停播不靠 REQUEST_QUIT：声音在本进程 BGM 口，撕页杀进程即停。
    velf = (PKJ / "hosts/vita/target/armv7-sony-vita-newlibeabihf"
            / "release/pocketjs-vita.velf")
    if velf.exists():
        run([f"{VITASDK}/bin/vita-make-fself", "-a", "0x2800000000000001",
             str(velf), str(staging / "eboot.bin")])
        print("[build-vpk] eboot regenerated with authid 0x2800000000000001")
    else:
        print(f"[build-vpk] WARN: velf not found ({velf}), kept default eboot")
    pjfa = PROJECT_ROOT / "fonts" / "chinese" / "cjk.pjfa"
    if pjfa.exists():
        fonts_dir = staging / "fonts"
        fonts_dir.mkdir(parents=True, exist_ok=True)
        shutil.copy2(pjfa, fonts_dir / "cjk.pjfa")
        print(f"[build-vpk] packed {pjfa.name} ({pjfa.stat().st_size} bytes) -> app0:/fonts/cjk.pjfa")
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
    patch_font_gpu()
    patch_font_dirty()
    bake_cjk_archive()
    try:
        build_vpk()
        repack()
    except subprocess.CalledProcessError:
        # VitaSDK's SCE header needs up to 4096 bytes of free space at the end of
        # segment 0 (实测 2824–2988，随构建大小浮动).  As the JS bundle grows,
        # segment 0 can end too close to a 4KB boundary and vita-elf-create
        # reports "segment 1 overlaps".  The injected rodata pad
        # (YUNYIN_ELF_PAD) shifts where segment 0 ends, so we relink with a
        # different pad until one lands early enough in its 4KB page.
        #
        # 注意：这个"差多少补多少"是模 4096 的，所以 pad 缩到 0 之后要再从
        # 小的往大试；以前只往下试、而且拿不到段信息就直接放弃，日文版那种
        # 更大的字库就会一直失败。
        need = 4096
        lower_first = True
        for attempt in range(24):
            gap = _elf_gap()
            if gap is not None:
                _, seg1_start = gap
                seg0_end = gap[0]
                free = seg1_start - seg0_end
                shrink = (need - free) + 64 if need > free else 512
            else:
                shrink = 512
            if lower_first:
                if PAD_SIZE - shrink >= 0:
                    PAD_SIZE -= shrink
                else:
                    lower_first = False
                    PAD_SIZE = shrink
            else:
                PAD_SIZE += shrink
            print(
                f"[build-vpk] SCE realign #{attempt + 1}: free={gap and (gap[1] - gap[0])} "
                f"need={need} -> pad 0x{PAD_SIZE:x}"
            )
            stage()
            patch_host()
            patch_graphics_glyph()
            patch_font_gpu()
            patch_font_dirty()
            try:
                build_vpk()
                repack()
                break
            except subprocess.CalledProcessError:
                # Only alignment problems are worth another relink.  A compile
                # error used to keep this loop spinning through all 24 attempts
                # (10+ minutes) and then blame the SCE header; ask
                # vita-elf-create directly and re-raise anything else.
                elf = (PKJ / "hosts/vita/target/armv7-sony-vita-newlibeabihf"
                       / "release/pocketjs-vita.elf")
                probe = subprocess.run(
                    [f"{VITASDK}/bin/vita-elf-create", str(elf), "/tmp/elf-probe.velf"],
                    capture_output=True)
                if probe.returncode == 0:
                    raise SystemExit(
                        "[build-vpk] the ELF itself is fine, so the failure above "
                        "is not an SCE alignment problem — fix that error first")
                continue
        else:
            raise SystemExit("[build-vpk] SCE alignment still failing after retries")
