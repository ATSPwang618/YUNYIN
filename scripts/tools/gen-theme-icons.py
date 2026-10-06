#!/usr/bin/env python3
"""给每套彩色主题生成**自己那一套**播放器图标（不再是所有主题共用浅色那套）。

素材只有浅色/深色两套，彩色主题按「换色」派生：把图标里**蓝色系**的描边/高亮
改成主题的强调色，其余（白底、白图形、灰阴影）原样保留。做法是 HSV 换色：

    饱和度够 + 色相落在蓝色区间  → 把色相换成目标色相，S/V 不动
    其它像素（白、灰、黑、透明） → 原样

所以白色音符不会被染色，亮蓝描边会变成翠绿/紫，浅蓝底会变成对应浅色底 —— 观感
跟手绘一套一致，而不是"复制粘贴改个文件夹"。

用法：

    python3 scripts/tools/gen-theme-icons.py          # 生成/覆盖 asset/ui/<主题>/*.png
    python3 scripts/tools/gen-theme-icons.py --check   # 只检查是否已生成

主题 → 目标色相（度）在 THEMES 里；`light` / `dark` 是原生素材，本脚本不碰。
"""

import colorsys
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]        # scripts/tools/x.py → 仓库根
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))  # scripts/（yunyin_build）

from yunyin_build.assets import _png_decode, _png_encode  # noqa: E402

UI = ROOT / "asset" / "ui"
SOURCE = "light"

# 主题名 → 目标色相（度）。blue 保留原素材的蓝，只做统一化处理。
THEMES = {
    "blue": 212.0,
    "green": 152.0,
    "purple": 268.0,
}

# 原素材里"蓝"的色相区间（0–360），只换这个区间的像素。
BLUE_RANGE = (170.0, 260.0)
MIN_SAT = 0.12          # 低于这个饱和度算灰/白，不动


def recolor(rgba: bytearray, target_hue: float) -> bytearray:
    out = bytearray(len(rgba))
    for i in range(0, len(rgba), 4):
        r, g, b, a = rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]
        if a == 0:
            continue
        h, s, v = colorsys.rgb_to_hsv(r / 255.0, g / 255.0, b / 255.0)
        deg = h * 360.0
        if s >= MIN_SAT and BLUE_RANGE[0] <= deg <= BLUE_RANGE[1]:
            nr, ng, nb = colorsys.hsv_to_rgb(target_hue / 360.0, s, v)
            r, g, b = round(nr * 255), round(ng * 255), round(nb * 255)
        out[i], out[i + 1], out[i + 2], out[i + 3] = r, g, b, a
    return out


def main() -> int:
    src = UI / SOURCE
    icons = sorted(src.glob("icon_*.png"))
    if not icons:
        raise SystemExit(f"没有源图标：{src}")

    check = "--check" in sys.argv
    missing = []
    for theme, hue in THEMES.items():
        dst = UI / theme
        dst.mkdir(parents=True, exist_ok=True)
        for icon in icons:
            target = dst / icon.name
            if check:
                if not target.exists():
                    missing.append(str(target.relative_to(ROOT)))
                continue
            w, h, rgba = _png_decode(icon)
            _png_encode(target, w, h, recolor(rgba, hue))
        if not check:
            print(f"{theme}: {len(icons)} 个图标 → {dst.relative_to(ROOT)}")

    if check:
        if missing:
            print("缺少派生图标：\n  " + "\n  ".join(missing))
            return 1
        print("派生图标齐全")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
