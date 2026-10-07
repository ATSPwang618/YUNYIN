#!/usr/bin/env python3
"""YUNYIN 一键打包：app/ + native/ → PS Vita VPK。

流水线（每一步的实现都在 scripts/yunyin_build/，模块分工见该包的 __init__）：
  1. apply_pocketjs_patch()   应用仓库内固定的 PocketJS 源码补丁
  2. pack.stage()             app/ 与 native/ 暂存进 PocketJS（apps/yunyin、hosts/vita）
  3. apply_host_patches()     应用按构建开关决定的宿主补丁
                              （帧循环 / 正式包开关 / 诊断 / Vita2D 原生字体）
  4. pack.build_vpk()         PocketJS 编译（bun tools/build.ts + tools/vita.ts）
  5. pack.repack()            换 TITLE_ID / 权限 eboot / 中文标题，重新打 VPK 到 dist/

所有可调项（路径、产物名、字体、诊断开关）集中在 yunyin_build/config.py。

Requires (inside the WSL2 distro):
  * VitaSDK  at /opt/vitasdk   (must include libSceAudiodec_stub.a and
                               libSceSysmem_stub.a — M4A/AAC needs both)
  * bun      at /root/.bun/bin/bun
  * PocketJS framework checkout at $POCKETJS_ROOT
    （默认 /root/pocketjs；当前正式配置是 0.13：POCKETJS_ROOT=/root/pocketjs013，
      并带 YUNYIN_BARE_GRAPHICS=1，见 README「自行构建」）

常用环境变量：
  YUNYIN_FONT=chinese|japanese   字体版本（默认 chinese）
  YUNYIN_OUT=<name>              输出名 -> dist/<name>.vpk（默认 yunyin-main）
  YUNYIN_THEME=<skin>            烘焙时优先的皮肤：light / dark / pure / anime
  YUNYIN_APP_VER=<ver>           param.sfo 里的 APP_VER（默认 01.00）
  诊断开关：YUNYIN_NO_COVER / YUNYIN_BARE_GRAPHICS / YUNYIN_CATCH_HANG
两个字体版本一次打完用 scripts/build-variants.sh。
"""

import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from yunyin_build import config, fonts, pack, patches_host, patches_pocketjs


def apply_host_patches(*, diagnostics: bool = True) -> None:
    """把 PocketJS 官方宿主打成 YUNYIN 宿主。

    顺序有讲究：先让 media 模块能编译进来，再落正式包开关与诊断，
    最后安装 Vita2D 原生字体桥接并移除旧字体渲染路径
    （BARE_GRAPHICS=1 时整组跳过）。
    """
    patches_host.patch_no_cover()
    patches_host.patch_host()
    patches_host.patch_host_defer_dynamic_texture_gpu()
    patches_host.patch_vita_release_guards()
    if diagnostics:
        patches_host.patch_host_frame_diag()
        patches_host.patch_host_present_diag()
    # 帧跳过放最后：它要一次性把 render/overlay/present 包进 frame_changed()，
    # 所以必须看到"已经加过计时"的最终文本（见 patches_host.patch_host_frame_loop）。
    # YUNYIN_NO_FRAME_SKIP=1 是黑屏排查用 A/B 开关，回到 PocketJS 原始帧路径。
    if config.NO_FRAME_SKIP:
        print("[build-vpk] diagnostic: keep PocketJS render/present every frame")
    else:
        patches_host.patch_host_frame_loop()
    # Must run after the frame-loop/diagnostic patches: native-only removes the
    # old PJFA refresh call and the legacy GLYPH_RUN renderer from their final
    # staged form.
    patches_host.patch_host_native_text()


def build_and_pack() -> None:
    pack.build_vpk()
    pack.repack()


def main() -> int:
    if not Path(fonts.theme_font()).exists():
        raise SystemExit(f"字体文件缺失：{fonts.theme_font()}")

    # PocketJS itself must remain a clean external checkout.  All YUNYIN
    # changes to its tracked sources live in the repository patch below.
    patches_pocketjs.apply_pocketjs_patch()
    pack.stage()
    apply_host_patches()
    try:
        build_and_pack()
    except subprocess.CalledProcessError:
        # VitaSDK 的 SCE 头需要段 0 末尾空出最多 4096 字节（实测 2824–2988，随包大小
        # 浮动）。JS 包长大后段 0 可能正好贴着 4KB 边界，vita-elf-create 会报
        # "segment 1 overlaps"。注入的 rodata 垫片（YUNYIN_ELF_PAD）能挪动段 0 的
        # 结束位置，所以这里换一个 pad 重新链接，直到落进页内合适的位置。
        #
        # 注意：这个"差多少补多少"是模 4096 的，pad 缩到 0 之后要再从小的往大试；
        # 以前只往下试、拿不到段信息就直接放弃，日文版那种更大的字库会一直失败。
        need = 4096
        lower_first = True
        for attempt in range(24):
            gap = pack._elf_gap()
            if gap is not None:
                _, seg1_start = gap
                seg0_end = gap[0]
                free = seg1_start - seg0_end
                shrink = (need - free) + 64 if need > free else 512
            else:
                shrink = 512
            if lower_first:
                if config.PAD_SIZE - shrink >= 0:
                    config.PAD_SIZE -= shrink
                else:
                    lower_first = False
                    config.PAD_SIZE = shrink
            else:
                config.PAD_SIZE += shrink
            print(
                f"[build-vpk] SCE realign #{attempt + 1}: free={gap and (gap[1] - gap[0])} "
                f"need={need} -> pad 0x{config.PAD_SIZE:x}"
            )
            pack.stage()
            apply_host_patches(diagnostics=False)
            try:
                build_and_pack()
                break
            except subprocess.CalledProcessError:
                # 只有对齐问题才值得再链一次。编译错误以前会在这里空转 24 次
                # （十几分钟）再赖到 SCE 头上 —— 先问一次 vita-elf-create，
                # 段本身没问题就把原始错误抛出去。
                elf = (config.PKJ / "hosts/vita/target/armv7-sony-vita-newlibeabihf"
                       / "release/pocketjs-vita.elf")
                probe = subprocess.run(
                    [f"{config.VITASDK}/bin/vita-elf-create", str(elf),
                     "/tmp/elf-probe.velf"],
                    capture_output=True)
                if probe.returncode == 0:
                    raise SystemExit(
                        "[build-vpk] the ELF itself is fine, so the failure above "
                        "is not an SCE alignment problem — fix that error first")
                continue
        else:
            raise SystemExit("[build-vpk] SCE alignment still failing after retries")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
