"""构建配置：路径、产物名、字体与诊断开关。

所有环境变量都在这里读取；改构建行为只看这一个文件。"""

import os
from pathlib import Path


# --- config ---------------------------------------------------------------
# 本文件在 scripts/yunyin_build/ 下，往上三层才是仓库根（scripts/yunyin_build/x.py）。
PROJECT_ROOT = Path(__file__).resolve().parents[2]


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
APP_VER = os.environ.get("YUNYIN_APP_VER", "01.10")


# 诊断开关：YUNYIN_NO_COVER=1 时跳过内嵌封面贴图上传（排查 0.13 灰屏用）。
NO_COVER = os.environ.get("YUNYIN_NO_COVER") == "1"


# 诊断开关：YUNYIN_BARE_GRAPHICS=1 时跳过额外的宿主图形补丁；原生文字
# 后端仍然保留，因为它是 Vita2D TEXT_RUN 的实际渲染入口。
BARE_GRAPHICS = os.environ.get("YUNYIN_BARE_GRAPHICS") == "1"


# 诊断开关：YUNYIN_CATCH_HANG=1 时保留 0.13 的 guest 中断，但把帧预算从
# 250ms 放宽到 2s —— 慢帧能正常完成，真死循环会在 2 秒后被掐并重启 guest
# （日志里出现新的 `yunyin: start`），用来判断"卡死"是 JS 死循环还是宿主僵死。
CATCH_HANG = os.environ.get("YUNYIN_CATCH_HANG") == "1"


# 诊断开关：YUNYIN_NO_FRAME_SKIP=1 时保留 PocketJS 原始的每帧 render/present，
# 用来隔离宿主的 frame_changed() 跳帧逻辑；正常包不要打开。
NO_FRAME_SKIP = os.environ.get("YUNYIN_NO_FRAME_SKIP") == "1"


TITLE_ID = os.environ.get("YUNYIN_TITLE_ID", "")  # 留空 = 用 app/catalog.ts 的 TITLE_ID / PF2A47F97


THEME = os.environ.get("YUNYIN_THEME", "dark")  # 皮肤主题：light / dark / pure / anime


# 默认思源黑体 SC Bold。日文曲库才切 MSMINCHO：YUNYIN_FONT=japanese
# dark/anime 以前绑日文字体会让简体 UI（首页/专辑/设置）变成 □□□。
FONT_BY_THEME = {"light": "chinese", "dark": "chinese", "pure": "chinese", "anime": "chinese"}


FONT_THEME = os.environ.get("YUNYIN_FONT", FONT_BY_THEME.get(THEME, "chinese"))


DENSITY = 2                                    # see note in build_vpk()


PAD_SIZE = 0x1000                              # VitaSDK SCE-header layout pad (auto-adjusted)


FONT_NAMES = ("SourceHanSansSC-Bold.otf", "MSMINCHO.TTF", "font.ttf")
