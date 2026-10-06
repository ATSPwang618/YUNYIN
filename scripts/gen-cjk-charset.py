#!/usr/bin/env python3
"""从字体自己的 cmap 生成流式字库的字符集文件（fonts/<theme>/cjk-stream.txt）。

为什么要有这个脚本：手写字符集（GB2312、JIS 一级……）永远在赌"够不够用"——
歌名、歌手、歌词里冒出繁体字、日文汉字、生僻字时就变成方框或空白。
TTF/OTF 自己清楚它有哪些字形，所以直接取 **cmap ∩ 需要的 Unicode 区块**：
中日韩汉字（含扩展 A / 兼容区）、假名、注音、常用标点与符号。

用法：
    python3 scripts/gen-cjk-charset.py <字体文件> [输出文件]
    python3 scripts/gen-cjk-charset.py fonts/chinese/NotoSansSC-Medium.ttf
    python3 scripts/gen-cjk-charset.py fonts/japanese/MSMINCHO.TTF /tmp/jp.txt

注意：生成的文件里**只能有字符本身**，不能写注释 —— 烘焙脚本
（scripts/bake-cjk-archive.ts）是把文件里出现的每个码点都当成要烘的字。
"""

import struct
import sys
from pathlib import Path

"""要烘的 Unicode 区块：中文字 + 日文字 + 标点/符号。

每项是 (起始码点, 结束码点, 说明)。真正的集合是这些区间和字体 cmap 的交集，
所以字体没有的字不会白白占位置（也不会烘出一片空白格子）。"""
BLOCKS = [
    (0x0020, 0x007E, "ASCII"),
    (0x00A0, 0x00FF, "拉丁文补充（é ü ñ 等）"),
    (0x0100, 0x017F, "拉丁文扩展 A（Ā ā Ł ł）"),
    (0x0180, 0x024F, "拉丁文扩展 B（Ə ƒ Ș ș）"),
    (0x0370, 0x03FF, "希腊字母"),
    (0x0400, 0x04FF, "西里尔字母（俄语歌名）"),
    (0x2010, 0x2027, "破折号 / 引号 / 省略号"),
    (0x2030, 0x205E, "千分号 / 角标 / 括号"),
    (0x20A0, 0x20BF, "货币符号"),
    (0x2100, 0x214F, "字母式符号（™ № Ω）"),
    (0x2150, 0x218F, "数字形式（① ⅓ Ⅷ）"),
    (0x2190, 0x21FF, "箭头（→ ← ⇄）"),
    (0x2200, 0x22FF, "数学运算符（≈ ≤ ≥ ∞）"),
    (0x2460, 0x24FF, "带圈字母数字"),
    (0x25A0, 0x25FF, "几何图形（● ▲ ■ ▢）"),
    (0x2600, 0x26FF, "杂项符号（♪ ♫ ♥ ★ ☆）"),
    (0x2700, 0x27BF, "装饰符号（✔ ✗ ✨）"),
    (0x27F0, 0x27FF, "补充箭头（⟲ ⟳）"),
    (0x3000, 0x303F, "CJK 标点（、。「」〈〉～）"),
    (0x3040, 0x309F, "平假名（ぁ あ ゛ ゝ）"),
    (0x30A0, 0x30FF, "片假名（ァ ア ・ ー ヽ）"),
    (0x3100, 0x312F, "注音符号（ㄅ ㄆ ㄇ）"),
    (0x31F0, 0x31FF, "片假名语音扩展"),
    (0x3200, 0x32FF, "带圈 CJK（㈱ ㊙ ㋐）"),
    (0x3300, 0x33FF, "CJK 兼容（㎜ ㌔ ㍿）"),
    (0x3400, 0x4DBF, "CJK 扩展 A（生僻汉字）"),
    (0x4E00, 0x9FFF, "CJK 统一表意文字（中日韩汉字主体）"),
    (0xF900, 0xFAFF, "CJK 兼容表意文字（舊字形）"),
    (0xFE10, 0xFE1F, "竖排标点"),
    (0xFE30, 0xFE4F, "CJK 兼容形式"),
    (0xFF00, 0xFFEF, "半角 / 全角形式（Ａ １ ％ ￥）"),
    (0x1E00, 0x1EFF, "拉丁文扩展附加（越南语 ệ ợ）"),
    (0x20000, 0x2A6DF, "CJK 扩展 B（字体里有的那部分生僻字）"),
    (0x2A700, 0x2B73F, "CJK 扩展 C"),
    (0x2B740, 0x2B81F, "CJK 扩展 D"),
    (0x2B820, 0x2CEAF, "CJK 扩展 E"),
]

"""区块之外的零散符号：音乐/标注类，界面里真的会用到的那些。"""
EXTRA = "♪♫♬♩♭♯★☆♥♡♦◆●◎○▲△▼▽→←↑↓·•—–「」『』【】（）《》〈〉＝，。％‰×□▢▶◀‖⇄↻≪≫‹›⟲⟳"


def u16(b, at):
    return struct.unpack_from(">H", b, at)[0]


def i16(b, at):
    return struct.unpack_from(">h", b, at)[0]


def u32(b, at):
    return struct.unpack_from(">I", b, at)[0]


def cmap_format4(t, off, out):
    seg2 = u16(t, off + 6)
    seg = seg2 // 2
    end_o = off + 14
    start_o = end_o + seg2 + 2
    delta_o = start_o + seg2
    range_o = delta_o + seg2
    for i in range(seg):
        end = u16(t, end_o + 2 * i)
        start = u16(t, start_o + 2 * i)
        if start == 0xFFFF:
            continue
        delta = i16(t, delta_o + 2 * i)
        ro = u16(t, range_o + 2 * i)
        for cp in range(start, min(end, 0xFFFF) + 1):
            if ro == 0:
                gid = (cp + delta) & 0xFFFF
            else:
                at = range_o + 2 * i + ro + 2 * (cp - start)
                if at + 2 > len(t):
                    continue
                g = u16(t, at)
                gid = 0 if g == 0 else (g + delta) & 0xFFFF
            if gid:
                out.add(cp)


def cmap_format12(t, off, out):
    n = u32(t, off + 12)
    for i in range(n):
        at = off + 16 + 12 * i
        if at + 12 > len(t):
            break
        start, end, _gid = u32(t, at), u32(t, at + 4), u32(t, at + 8)
        if end < start:
            continue
        for cp in range(start, min(end, 0x10FFFF) + 1):
            out.add(cp)


def read_cmap(path: Path) -> set[int]:
    """读 TTF/OTF 的 cmap：能拿 format 12 就拿（超集），否则退回 format 4。"""
    data = path.read_bytes()
    if len(data) < 12:
        return set()
    if data[:4] == b"ttcf":  # TTC：取第一个 face 的偏移表
        base = u32(data, 12)
    else:
        base = 0
    num_tables = u16(data, base + 4)
    cmap_off = None
    for i in range(num_tables):
        rec = base + 12 + 16 * i
        if data[rec : rec + 4] == b"cmap":
            cmap_off = u32(data, rec + 8)
            break
    if cmap_off is None:
        return set()
    sub = cmap_off
    best12, best4 = [], []
    for i in range(u16(data, sub + 2)):
        rec = sub + 4 + 8 * i
        platform, _enc, off = u16(data, rec), u16(data, rec + 2), u32(data, rec + 4)
        at = sub + off
        fmt = u16(data, at)
        if fmt == 12:
            best12.append(at)
        elif fmt == 4:
            best4.append((platform, at))
    out: set[int] = set()
    if best12:
        for at in best12:
            cmap_format12(data, at, out)
    else:
        for _platform, at in best4:
            cmap_format4(data, at, out)
    return out


def wanted(cmap: set[int]) -> list[int]:
    keep: set[int] = set()
    for a, b, _note in BLOCKS:
        keep.update(cp for cp in cmap if a <= cp <= b)
    keep.update(cp for cp in (ord(c) for c in EXTRA) if cp in cmap)
    return sorted(keep)


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        print(__doc__)
        return 1
    font = Path(argv[1])
    out = Path(argv[2]) if len(argv) > 2 else \
        Path("fonts") / "chinese" / "cjk-stream.txt"
    cmap = read_cmap(font)
    cps = wanted(cmap)
    if not cps:
        print(f"[charset] {font} 里没有可用字形（cmap 读不到？）")
        return 1
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("".join(chr(cp) for cp in cps), encoding="utf-8")
    han = sum(1 for cp in cps if 0x3400 <= cp <= 0x9FFF or 0xF900 <= cp <= 0xFAFF)
    kana = sum(1 for cp in cps if 0x3040 <= cp <= 0x30FF)
    print(f"[charset] {font.name}: cmap={len(cmap)} 收录={len(cps)} "
          f"（汉字 {han} / 假名 {kana}）→ {out} {out.stat().st_size} 字节")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
