"""YUNYIN 构建工具包：入口是 ../build-vpk.py，这里是按职责拆开的实现。

  config.py            路径 / 产物名 / 字体 / 诊断开关（唯一配置入口）
  proc.py              子进程封装（bun / vita-* 工具的统一环境）
  patching.py          通用文本补丁助手
  assets.py            皮肤 PNG 归一化 + images.json
  patches_host.py      宿主补丁：帧循环 / 正式包开关 / 诊断 / 原生字体
  patches_pocketjs.py  PocketJS 固定源码补丁
  fonts.py             字体选择 / 曲库字符收集 / theme-seed
  pack.py              暂存 / 编译 / VPK 打包
"""
