"""进程封装 / 暂存 app+native 到 PocketJS / 编译 / VPK 打包与 SCE 对齐。"""

import os
import re
import shutil
import struct
import subprocess
import zipfile
from pathlib import Path

from . import assets, config, fonts
from .config import (APP_ID, APP_NAME, APP_TITLE, APP_VER, BUN, DENSITY, OUT,
                     PKJ, PROJECT_ROOT, TITLE_ID, VITASDK)
from .proc import run


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
    fonts.make_theme_seed()
    shutil.copytree(PROJECT_ROOT / "app", app_dst)
    # Themeable UI skins (asset/ui/<theme>/): copy into the staged app so
    # tools/build.ts can find + bake them (appDir/<name> lookup).
    asset_src = PROJECT_ROOT / "asset"
    if asset_src.is_dir():
        shutil.copytree(asset_src, app_dst / "asset", dirs_exist_ok=True)
    assets.normalize_assets(app_dst / "asset")
    assets.refresh_image_manifest(app_dst)

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
            f"static YUNYIN_ELF_PAD: [u8; {config.PAD_SIZE}] = [0u8; {config.PAD_SIZE}];\n"
        )
    print("[build-vpk] staged app + native/rs -> hosts/vita/src/media/")


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


# --- 4/5 bake + vite build ------------------------------------------------
def build_vpk():
    # Density 2 (raster 2 samples/logical px) renders sharp glyphs; a density-1
    # raster is upscaled 2x on the Vita surface and looks blurry.  Because the
    # atlas limit is 2048px we keep the charset small (ASCII + live music tags
    # + symbols) so a 17x24 cell at density 2 (34x48 coverage) fits.
    harvest = fonts.harvest_chars()
    extra = ["--extra-chars=" + harvest] if harvest else []
    font = fonts.theme_font()
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
    # dist/ 不入库：全新克隆里没有这个目录，这里自己建（以前靠本地残留的 dist/，
    # 换台机器/新克隆就会在最后一步写 VPK 时崩）。
    out.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
        for p in sorted(staging.rglob("*")):
            if p.is_file():
                z.write(p, p.relative_to(staging).as_posix())
    print(f"[build-vpk] VPK ready: {out} ({out.stat().st_size} bytes, title {title_id})")
