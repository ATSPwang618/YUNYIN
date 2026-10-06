"""YUNYIN 构建工具包：入口是 ../build-vpk.py，这里是按职责拆开的实现。

  config.py            路径 / 产物名 / 字体 / 诊断开关（唯一配置入口）
  proc.py              子进程封装（bun / vita-* 工具的统一环境）
  patching.py          通用文本补丁助手
  assets.py            皮肤 PNG 归一化 + images.json
  patches_host.py      宿主补丁：帧循环 / 正式包开关 / 诊断
  patches_graphics.py  宿主补丁：图形与字库
  fonts.py             字形收割 / cjk.pjfa / theme-seed
  pack.py              暂存 / 编译 / VPK 打包
"""
