#!/usr/bin/env bash
# 打包两个字体版本（在 WSL2 的 pocket-ubuntu 里跑）：
#   dist/yunyin-cn.vpk  中文优先：Noto Sans SC + 中文标点/全角字符集
#   dist/yunyin-jp.vpk  日文优先：MS Mincho + 假名/半角假名/日文标点
#
# 用法：wsl -d pocket-ubuntu -u root bash /mnt/d/AI-PSVITA/yunyin/scripts/build-variants.sh
# 皮肤默认色、TITLE_ID 与单版本打包完全一致，两个包只是字体不同。
set -euo pipefail

cd "$(dirname "$0")/.."

build() {
    local font="$1" out="$2"
    echo "=== $out ($font) ==="
    YUNYIN_FONT="$font" YUNYIN_OUT="$out" python3 scripts/build-vpk.py
}

build chinese  yunyin-cn
build japanese yunyin-jp

ls -l dist/yunyin-cn.vpk dist/yunyin-jp.vpk
