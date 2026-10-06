#!/usr/bin/env python3
"""从浅色主题派生「同款扁平化、换个颜色」的主题，写回 app/colors.json。

为什么用脚本而不是手抄：一套主题有 50 多个类字面量，手抄必错（少一个角色就是
运行时没有样式）。派生规则只有两条：

  1. 强调色：`red-*` → 目标色（blue / emerald / violet / …）；
  2. 底色：浅色主题的 `bg-zinc-100`（整屏底）→ `bg-<tint>-100`。

用法（改主题列表就改下面的 THEMES）：

    python3 scripts/tools/gen-flat-themes.py            # 生成/覆盖派生主题
    python3 scripts/tools/gen-flat-themes.py --check     # 只检查有没有漂移

`light` / `dark` 是手工维护的原生主题，这个脚本**不碰**它们。
"""

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]   # scripts/tools/x.py → 仓库根
COLORS = ROOT / "app" / "colors.json"

# 主题名 -> (强调色, appRoot 底色)
THEMES = {
    "blue": ("blue", "sky"),
    "green": ("emerald", "emerald"),
    "purple": ("violet", "violet"),
}

# 从浅色主题派生；这几个 key 是"底色"，换主题时要一起染色。
TINTED = {"appRoot"}


def derive(base: dict, accent: str, tint: str) -> dict:
    out = {}
    for key, cls in base.items():
        new = cls.replace("red-", f"{accent}-")
        if key in TINTED:
            new = new.replace("bg-zinc-100", f"bg-{tint}-100")
        out[key] = new
    return out


def main() -> int:
    raw = COLORS.read_text(encoding="utf-8")
    data = json.loads(raw)
    changed = False

    for name, (accent, tint) in THEMES.items():
        for section in ("bg", "ui"):
            base = data[section].get("light")
            if base is None:
                print(f"! colors.json 缺少 {section}.light，跳过 {name}")
                continue
            want = derive(base, accent, tint)
            if data[section].get(name) != want:
                data[section][name] = want
                changed = True

    if "--check" in sys.argv:
        print("派生主题与 colors.json 一致" if not changed else "派生主题有漂移，请重新生成")
        return 1 if changed else 0

    if not changed:
        print("派生主题已是最新，无需改写")
        return 0

    COLORS.write_text(
        json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print("已写入：" + "、".join(THEMES))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
