#!/usr/bin/env bash
# YUNYIN 本地 PocketJS Playground —— 浏览器里实时预览 app.tsx（Mock 数据）。
#
#   wsl -d pocket-ubuntu -u root bash -lc 'cd /mnt/d/AI-PSVITA/yunyin && bash scripts/playground.sh'
#
# 完成后打开： http://127.0.0.1:8130/index.html?demo=yunyin-main
# 每次改了 app.tsx，重跑本脚本（或用 --watch 自动重建）再刷新即可。
set -e
export PATH="/root/.bun/bin:/root/.cargo/bin:$PATH"

SRC=/mnt/d/AI-PSVITA/yunyin
PKJ=/root/pocketjs
APP=yunyin
OUT=yunyin-main
FONT="$SRC/fonts/chinese/NotoSansSC-Medium.ttf"

echo "== 1/4 安装 wasm32 目标（如无） =="
rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true

echo "== 2/4 构建 WASM 引擎（首次较慢，已存在则跳过） =="
cd "$PKJ"
if [ ! -f hosts/web/pocketjs.wasm ]; then
  /root/.bun/bin/bun tools/wasm.ts
else
  echo "  (pocketjs.wasm 已存在，跳过)"
fi

echo "== 3/4 写入 app 源码 + 资源 =="
rm -rf "$PKJ/apps/$APP"
mkdir -p "$PKJ/apps/$APP"
cp -r "$SRC/app/." "$PKJ/apps/$APP/"
[ -d "$SRC/asset" ] && cp -r "$SRC/asset" "$PKJ/apps/$APP/asset"

echo "== 4/4 编译 web bundle 并启动服务 =="
/root/.bun/bin/bun tools/build.ts "$OUT" --density=2 \
  --font-regular="$FONT" --font-bold="$FONT" --font-mono="$FONT"

echo ""
echo "打开浏览器： http://127.0.0.1:8130/index.html?demo=$OUT"
echo "（Ctrl+C 退出；改 app.tsx 后重跑本脚本再刷新）"
exec /root/.bun/bin/bun hosts/web/serve.ts
