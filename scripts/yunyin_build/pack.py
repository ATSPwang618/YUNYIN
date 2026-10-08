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
    vita2d_archive = PROJECT_ROOT / "native" / "libs" / "libvita2d.a"
    if not vita2d_archive.is_file():
        raise SystemExit(
            f"[build-vpk] required Vita2D archive is missing: {vita2d_archive}"
        )
    # 这份 .a 是预编译的：scripts/patches/libvita2d-pvf-*.patch 记录的是当初改它源码的
    # 内容，构建过程不会重新应用。少了 PVF 入口符号，宿主的 extern "C" 声明就会链接
    # 失败（或更糟：链到别的实现），所以在这里先挡一道。
    archive_bytes = vita2d_archive.read_bytes()
    missing = [
        name for name in (b"vita2d_pvf_set_char_size", b"vita2d_pvf_get_glyph_stats")
        if name not in archive_bytes
    ]
    if missing:
        raise SystemExit(
            "[build-vpk] libvita2d.a 缺少 PVF 入口符号 "
            + ", ".join(name.decode() for name in missing)
            + "：请按 scripts/patches/libvita2d-pvf-*.patch 重新编译这份静态库"
        )
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
    print(
        "[build-vpk] staged app + native/rs -> hosts/vita/src/media/ "
        f"(libvita2d.a={vita2d_archive.stat().st_size} bytes)"
    )


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
    # 这里烘焙出来的是 PocketJS 侧的字体归档；运行期宿主不会再把它上传成 GPU 纹理
    # （见 patches_host.patch_host_native_text），界面文字走 Vita2D 的 ScePvf。
    # 因此不再用 --extra-chars 去收割曲库字符集：图集内容只由源码字面量决定，
    # 缺字与否跟界面无关（旧实现还会去扫 /mnt/d/PSV 下的音乐目录）。
    # 注意 build.ts 只认 --font-regular / --font-bold，没有 --font-mono。
    font = fonts.theme_font()
    run([BUN, "tools/build.ts", APP_ID, f"--density={DENSITY}",
         f"--font-regular={font}", f"--font-bold={font}"])
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
    # 运行期只认这一个字体文件（vita2d_load_custom_pvf），不打包 TTF 兜底，免得又冒出
    # 第二套渲染实现。文件只需"改名"：ScePvf 直接吃 OpenType/TTF，yunyin.pvf 只是
    # libvita2d 的加载入口名。取自 fonts/<FONT_THEME>/，所以 YUNYIN_FONT=japanese
    # 打出来的包真的用 MS Mincho。
    native_font = Path(fonts.theme_font())
    if native_font.exists():
        fonts_dir = staging / "fonts"
        fonts_dir.mkdir(parents=True, exist_ok=True)
        shutil.copy2(native_font, fonts_dir / "yunyin.pvf")
        print(
            f"[build-vpk] packed native font {native_font.name} "
            f"({native_font.stat().st_size} bytes) -> app0:/fonts/yunyin.pvf"
        )
    else:
        print(f"[build-vpk] WARN: 随包字体缺失 {native_font}，界面文字会 mode=disabled")
    # 根证书：传输层是随包的 libcurl/OpenSSL，信任库就是这一份 —— 不看固件根库。
    # 网易云整条链都挂在 DigiCert Global Root G2 上，另附完整 ca-bundle 兜底；
    # 开机由 yhttp_load_ca() 把 PEM 读进内存交给 curl（PEM 与 DER 都带上，运行时按顺序试）。
    ca_dir = PROJECT_ROOT / "certs"
    certs = sorted(ca_dir.glob("*")) if ca_dir.is_dir() else []
    if certs:
        dst = staging / "certs"
        dst.mkdir(parents=True, exist_ok=True)
        for f in certs:
            if f.is_file():
                shutil.copy2(f, dst / f.name)
        print(f"[build-vpk] packed {len(certs)} 个根证书文件 -> app0:/certs/")
    else:
        print("[build-vpk] WARN: certs/ 为空，内置根证书不会随包发布")
    out = PROJECT_ROOT / "dist" / f"{OUT}.vpk"
    # dist/ 不入库：全新克隆里没有这个目录，这里自己建（以前靠本地残留的 dist/，
    # 换台机器/新克隆就会在最后一步写 VPK 时崩）。
    out.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
        for p in sorted(staging.rglob("*")):
            if p.is_file():
                z.write(p, p.relative_to(staging).as_posix())
    print(f"[build-vpk] VPK ready: {out} ({out.stat().st_size} bytes, title {title_id})")
