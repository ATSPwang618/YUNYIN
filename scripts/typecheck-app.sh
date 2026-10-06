#!/usr/bin/env bash
# 云音 App 侧类型检查 —— 补上打包器不做的那道闸。
#
# 为什么需要：bun 只转译、不做类型检查，"某个符号没导出/没导入"这种错误
# 打包能过、真机一跑到那段代码就抛 ReferenceError。
# （00.91 歌词页卡死就是这么来的：cjkEpoch / cjkFont 被留在了页面文件里。）
#
# 用法（WSL 里）：
#   bash /mnt/d/AI-PSVITA/yunyin/scripts/typecheck-app.sh
#
# 判失败只看"真会炸"的错误码；其余纯类型噪音会列出来但不判失败。
set -uo pipefail

cd "$(dirname "$0")/.."
# 默认跟当前正式构建目标一致（0.13）；旧版 0.12 用 POCKETJS_ROOT=/root/pocketjs 覆盖。
PJ="${POCKETJS_ROOT:-/root/pocketjs013}"
BUN="${BUN:-/root/.bun/bin/bun}"
TARGET="$PJ/apps/yunyin"

case "$TARGET" in
  */pocketjs/apps/yunyin | */pocketjs*/apps/yunyin) ;;
  *) echo "refuse: unexpected staging target $TARGET" >&2; exit 2 ;;
esac
[ -x "$BUN" ] || { echo "bun not found: $BUN" >&2; exit 2; }
[ -f "$PJ/node_modules/typescript/bin/tsc" ] || {
  echo "typescript not found under $PJ" >&2; exit 2
}

# 暂存一份最新源码再检查（只动 pocketjs 里的 apps/yunyin 暂存目录）。
rm -rf "$TARGET"
mkdir -p "$TARGET"

# app/theme-seed.tsx 是 colors.json 的派生物（构建时生成，不入库）。
# 刚克隆下来还没构建过的仓库在这里补一刀，免得类型检查报"找不到模块"。
if [ ! -f app/theme-seed.tsx ]; then
  python3 -c "import sys; sys.path.insert(0, 'scripts'); from yunyin_build import fonts; fonts.make_theme_seed()"
fi

cp -r app/. "$TARGET/"

OUT="$(cd "$PJ" && timeout 600 "$BUN" node_modules/typescript/bin/tsc --noEmit -p tsconfig.json 2>&1 \
  | grep -E '^apps/yunyin/' | grep -v '^apps/yunyin/model/' || true)"

# TS2304 找不到名字 / TS2305 没有导出 / TS2307 找不到模块 / TS2552 拼错名字 /
# TS2459 声明了但没导出 / TS2614 模块没有该导出 / TS2724 类似的导出名 / TS1192 缺默认导出
FATAL="$(printf '%s\n' "$OUT" | grep -E 'error TS(1192|2304|2305|2307|2459|2503|2552|2614|2724)' || true)"

if [ -n "$OUT" ]; then
  echo "--- 云音 App 类型检查（app/model/ 草稿已忽略）---"
  printf '%s\n' "$OUT"
fi

if [ -n "$FATAL" ]; then
  echo ""
  echo "FAIL: $(printf '%s\n' "$FATAL" | wc -l) 个会在真机上炸的错误（未定义符号 / 缺失导出）"
  exit 1
fi

echo "OK: 没有致命类型错误（剩余纯类型噪音不影响运行）"
