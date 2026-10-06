#!/usr/bin/env python3
"""把 Vita 崩溃日志里的 `elf+偏移` 反查成最近的函数名。

用法（WSL 里，python3 直接跑）：
    python3 scripts/symbolize-crash.py <pocketjs-vita.elf> 0xd0fcf 0xd6c1

release 构建通常没有调试行（addr2line 只能给 ??），但符号表一般还在，
所以这里用 nm 找"地址 <= 崩溃地址 的最近符号"，输出 `函数 + 偏移`。
"""
import bisect
import subprocess
import sys


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    elf = sys.argv[1]
    addrs = [int(a, 16) for a in sys.argv[2:]]

    nm = subprocess.run(
        ["/opt/vitasdk/bin/arm-vita-eabi-nm", "-n", elf],
        capture_output=True,
        text=True,
        check=False,
    )
    if nm.returncode != 0:
        print("nm failed:", nm.stderr.strip())
        return 1

    syms = []
    for line in nm.stdout.splitlines():
        parts = line.split()
        if len(parts) >= 3:
            try:
                syms.append((int(parts[0], 16), parts[2]))
            except ValueError:
                pass
    syms.sort()
    starts = [s[0] for s in syms]

    for addr in addrs:
        i = bisect.bisect_right(starts, addr) - 1
        if i < 0:
            print(f"0x{addr:x} -> ?")
            continue
        base, name = syms[i]
        print(f"0x{addr:x} -> {name} +0x{addr - base:x}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
