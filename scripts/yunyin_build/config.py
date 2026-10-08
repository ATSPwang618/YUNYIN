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


# 版本号只有这一处来源（发布新版本改这里）。
# app/pocket.json、app/core/theme.ts、native 启动日志由 build-vpk.py 在构建前
# 校验一致，漏改任何一处都会直接报错。
APP_VER = os.environ.get("YUNYIN_APP_VER", "01.10")
APP_VERSION = APP_VER.lstrip("0") or APP_VER    # 展示用（01.10 -> 1.10）
# app/pocket.json 的 version 必须是严格的 X.Y.Z（PocketJS 校验），所以再补一位。
APP_VERSION_SEMVER = APP_VERSION + ".0"        # -> 1.10.0


# 诊断开关：YUNYIN_NO_COVER=1 时跳过内嵌封面贴图上传（排查 0.13 灰屏用）。
NO_COVER = os.environ.get("YUNYIN_NO_COVER") == "1"


# 诊断开关：YUNYIN_CATCH_HANG=1 时保留 0.13 的 guest 中断，但把帧预算从
# 250ms 放宽到 2s —— 慢帧能正常完成，真死循环会在 2 秒后被掐并重启 guest
# （日志里出现新的 `yunyin: start`），用来判断"卡死"是 JS 死循环还是宿主僵死。
CATCH_HANG = os.environ.get("YUNYIN_CATCH_HANG") == "1"


# 诊断开关：YUNYIN_NO_FRAME_SKIP=1 时保留 PocketJS 原始的每帧 render/present，
# 用来隔离宿主的 frame_changed() 跳帧逻辑；正常包不要打开。
NO_FRAME_SKIP = os.environ.get("YUNYIN_NO_FRAME_SKIP") == "1"


TITLE_ID = os.environ.get("YUNYIN_TITLE_ID", "")  # 留空 = 用 app/catalog.ts 的 TITLE_ID / PF2A47F97


# 只影响构建期烘焙出来的 PocketJS 字体归档（运行期不画它，见 pack.build_vpk()）。
DENSITY = 2


PAD_SIZE = 0x1000                              # VitaSDK SCE-header layout pad (auto-adjusted)


# 随包字体的选择：VPK 里的 app0:/fonts/yunyin.pvf 取 fonts/<FONT_THEME>/ 下的文件。
# chinese = 思源黑体 SC Bold（默认），japanese = MSMINCHO.TTF。
FONT_THEME = os.environ.get("YUNYIN_FONT", "chinese")


FONT_NAMES = ("SourceHanSansSC-Bold.otf", "MSMINCHO.TTF", "font.ttf")
