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
"""

import os
import re
import shutil
import struct
import subprocess
import zipfile
from pathlib import Path

# --- config ---------------------------------------------------------------
PROJECT_ROOT = Path("/mnt/d/AI-PSVITA/yunyin")  # this project's root
PKJ = Path("/root/pocketjs")                    # PocketJS framework checkout
VITASDK = "/opt/vitasdk"
BUN = "/root/.bun/bin/bun"
APP_NAME = "yunyin"                            # pocketjs app dir name
OUT_NAME = "yunyin-main"                       # pocket.json -> app.output
DENSITY = 2                                    # see note in build_vpk()
PAD_SIZE = 0x1000                              # VitaSDK SCE-header layout pad (auto-adjusted)


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


# --- 1/2 stage source + native -------------------------------------------
def stage():
    app_dst = PKJ / "apps" / APP_NAME
    if app_dst.exists():
        shutil.rmtree(app_dst)
    shutil.copytree(PROJECT_ROOT / "app", app_dst)

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
    cargo.write_text(c)

    build = PKJ / "hosts/vita/build.rs"
    b = build.read_text()
    if "use std::path::{Path, PathBuf};" not in b:
        b = "use std::path::{Path, PathBuf};\n" + b
    marker = '    println!("cargo:rerun-if-env-changed=POCKETJS_CAPTURE_DIR");'
    if "yunyin_listdir.c" not in b or "yplayer.c" not in b:
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
    # The app scans ux0:/data/music at runtime (the real/media folder is hidden
    # by SceIo), so the baked atlas must be harvested from the SAME location.
    # Fall back to ux0:/music for older layouts.
    for music_root in (
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
    harvest = "".join(sorted(out))
    return harvest[:2400]


# --- 4/5 bake + vite build ------------------------------------------------
def build_vpk():
    # Density 2 (raster 2 samples/logical px) renders sharp glyphs; a density-1
    # raster is upscaled 2x on the Vita surface and looks blurry.  Because the
    # atlas limit is 2048px we keep the charset small (ASCII + live music tags
    # + symbols) so a 17x24 cell at density 2 (34x48 coverage) fits.
    harvest = harvest_chars()
    extra = ["--extra-chars=" + harvest] if harvest else []
    font = str(PROJECT_ROOT / "fonts" / "NotoSansSC-Medium.ttf")
    run([BUN, "tools/build.ts", OUT_NAME, f"--density={DENSITY}",
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
    built = PKJ / "dist" / "vita" / f"{OUT_NAME}.vpk"
    if not built.exists():
        raise SystemExit(f"[build-vpk] vpk missing: {built}")
    staging = Path("/tmp/yunyin-vpk")
    shutil.rmtree(staging, ignore_errors=True)
    staging.mkdir()
    with zipfile.ZipFile(built) as z:
        z.extractall(staging)
    sce = PROJECT_ROOT / "app" / "sce_sys"
    if sce.exists():
        shutil.copytree(sce, staging / "sce_sys", dirs_exist_ok=True)
    (staging / "sce_sys").mkdir(parents=True, exist_ok=True)
    title_id = app_const("TITLE_ID") or "PF2A47F75"
    run([f"{VITASDK}/bin/vita-mksfoex", "-d", "ATTRIBUTE2=12",
         "-s", f"TITLE_ID={title_id}", "Yunyin", str(staging / "sce_sys/param.sfo")])
    out = PROJECT_ROOT / "dist" / f"{OUT_NAME}.vpk"
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
        for p in sorted(staging.rglob("*")):
            if p.is_file():
                z.write(p, p.relative_to(staging).as_posix())
    print(f"[build-vpk] VPK ready: {out} ({out.stat().st_size} bytes, title {title_id})")


if __name__ == "__main__":
    if not (PROJECT_ROOT / "fonts" / "NotoSansSC-Medium.ttf").exists():
        raise SystemExit("fonts/NotoSansSC-Medium.ttf missing")
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
