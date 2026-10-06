"""子进程封装：所有外部命令（bun / vita-* 工具）都从这里走。

统一注入 VitaSDK 与 bun 的环境和 PATH，保证在 WSL 里从任何 cwd 调用都能跑。
"""
import os
import subprocess

from .config import PKJ, VITASDK


def run(cmd, cwd=PKJ):
    e = dict(os.environ, HOME="/root", VITASDK=VITASDK, BUN_INSTALL="/root/.bun",
             PATH=f"{VITASDK}/bin:/root/.bun/bin:/root/.cargo/bin:" + os.environ.get("PATH", ""))
    print(">>>", " ".join(str(c) for c in cmd))
    subprocess.run([str(c) for c in cmd], cwd=str(cwd), env=e, check=True)
