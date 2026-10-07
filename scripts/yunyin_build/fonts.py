"""字体选择 / 曲库字符收集 / theme-seed 生成。"""

import json
import re
import shutil
from pathlib import Path

from .config import FONT_NAMES, FONT_THEME, PROJECT_ROOT, THEME


def theme_font():
    """Per-theme font file: fonts/<FONT_THEME>/<known name> if present, else any
    font file dropped in that folder, else the project default.

    日文版本要用 fonts/japanese/MSMINCHO.TTF：以前这里只找 Noto 的名字，日文
    字体文件根本不会被选中，两个"字体版本"实际用的是同一个字体。
    """
    base = PROJECT_ROOT / "fonts" / FONT_THEME
    for name in FONT_NAMES:
        cand = base / name
        if cand.exists():
            return str(cand)
    for pat in ("*.ttf", "*.ttc", "*.otf"):
        for cand in sorted(base.glob(pat)):
            return str(cand)
    # 兜底：中文版思源黑体 CFF OTF。
    return str(PROJECT_ROOT / "fonts" / "chinese" / "SourceHanSansSC-Bold.otf")


def _ffprobe():
    """Locate a working ffprobe: system PATH first, then the Windows ffmpeg
    mount (WSL builds often lack ffprobe, which would drop tag-only CJK chars
    like album names from the baked font)."""
    for cand in (
        "ffprobe",
        "/mnt/c/Program Files/ffmpeg/bin/ffprobe.exe",
        "/mnt/c/Program Files (x86)/ffmpeg/bin/ffprobe.exe",
        "/usr/bin/ffprobe",
        "/opt/vitasdk/bin/ffprobe",
    ):
        if cand.startswith("/"):
            if Path(cand).exists():
                return cand
        elif shutil.which(cand):
            return cand
    return None


def _id3_text_chars(path):
    """Collect every character that appears in the ID3v2 text/lyrics frames of a
    music file (TIT2/TPE1/TALB/USLT/T*).  No ffprobe needed, so it also works in
    WSL builds that don't have ffmpeg — this is what keeps CJK album/title chars
    (e.g. 叶惠美) in the baked font."""
    out = set()
    try:
        with open(path, "rb") as f:
            head = f.read(3 * 1024 * 1024)
    except Exception:
        return out
    if head[:3] != b"ID3":
        return out

    def synchsafe(b):
        return ((b[0] & 0x7f) << 21) | ((b[1] & 0x7f) << 14) | \
               ((b[2] & 0x7f) << 7) | (b[3] & 0x7f)

    ver = head[3]
    tag_size = synchsafe(head[6:10]) if len(head) >= 10 else 0
    pos = 10
    end = min(10 + tag_size, len(head))
    if head[5] & 0x40 and pos + 4 <= end:
        ext = synchsafe(head[pos:pos + 4]) if ver >= 4 else \
            int.from_bytes(head[pos:pos + 4], "big")
        pos = min(pos + max(ext, 4), end)
    # 标题 / 歌手 / 专辑 / 歌词 / 自定义字段 —— 歌词一定要收：
    # 只收前三个的话，MP3 内嵌歌词里的字没烘进字库，歌词页会显示成一排方框
    # （Vita3K 截图里就是这么发现的）。
    text_frames = {
        b"TIT2", b"TPE1", b"TALB", b"USLT", b"TXXX",
    }
    while pos + 10 <= end:
        if head[pos] == 0:
            break
        fid = head[pos:pos + 4]
        fsize = synchsafe(head[pos + 4:pos + 8]) if ver >= 4 else \
            int.from_bytes(head[pos + 4:pos + 8], "big")
        pos += 10
        if fsize == 0 or pos + fsize > end:
            break
        data = head[pos:pos + fsize]
        if data and fid in text_frames:
            enc = data[0]
            # USLT = 编码(1) + 语言(3) + 描述(变长) + 正文；跳过语言码再解。
            txt = data[4:] if fid == b"USLT" else data[1:]
            try:
                if enc == 3:
                    s = txt.decode("utf-8", "ignore")
                elif enc in (1, 2):
                    s = txt.decode("utf-16", "ignore")
                else:
                    s = txt.decode("latin-1", "ignore")
                for c in s:
                    if not c.isspace() and ord(c) >= 32 and c != "\ufeff":
                        out.add(c)
            except Exception:
                pass
        pos += fsize
    return out


def harvest_chars():
    """Sharpen the font.  At density 2 the Vita atlas limit shrinks to ~2520
    glyphs (a 17x24 logical cell becomes 34x48 coverage), so the broad GB2312
    set no longer fits.  Instead we bake ASCII + the characters actually present
    in the music library (filenames + ID3/Vorbis tags, re-harvested each build)
    + common UI symbols.  That keeps the atlas small enough for density 2 AND
    renders every song title/artist the library currently uses.
    """
    out = set(chr(i) for i in range(32, 127))  # ASCII always
    extra = set()
    # The app scans ux0:/data/yunyin/music at runtime (the real/media folder is
    # hidden by SceIo), so the baked atlas must be harvested from the SAME
    # location.  Fall back to the older data/music / ux0:/music layouts.
    for music_root in (
        Path("/mnt/d/PSV/vita-game/ux0/data/yunyin/music"),
        Path("/mnt/d/PSV/vita-game/ux0/data/music"),
        Path("/mnt/d/PSV/vita-game/ux0/music"),
        PROJECT_ROOT / "music",
    ):
        if not music_root.is_dir():
            continue
        for f in music_root.rglob("*"):
            if not f.is_file():
                continue
            for c in f.name:
                if not c.isspace():
                    extra.add(c)
            for c in _id3_text_chars(f):
                extra.add(c)
    for c in "…♪♫♬★☆♥♡♠♣♦◆●◎○▲△▼▽→←↑↓·•—–「」『』【】（）《》〈〉＝，。％‰×□▢ ▶◀‖⇄↻≪≫‹›⟲⟳":
        out.add(c)
    # Bake every CJK ideograph that appears in the UI source, so hardcoded
    # Chinese labels (menus, hints, breadcrumbs) render instead of tofu.
    # ASCII + punctuation are already covered above; only Hanzi need this.
    # These are reserved and never truncated by the 2400 cap.
    try:
        ui = (PROJECT_ROOT / "app" / "app.tsx").read_text(encoding="utf-8", errors="ignore")
        for c in ui:
            if "\u4e00" <= c <= "\u9fff":
                out.add(c)
    except Exception:
        pass
    extra |= _theme_chars()
    extra -= out
    harvest_must = "".join(sorted(out))
    harvest_extra = "".join(sorted(extra))
    room = max(0, 2400 - len(harvest_must))
    harvest = harvest_must + harvest_extra[:room]
    print(f"[build-vpk] font={FONT_THEME} reserved={len(harvest_must)} extra={len(harvest_extra)} baked={len(harvest)}")
    return harvest


def _theme_chars():
    """Per-theme extra chars for the baked font atlas.

    Reads fonts/<THEME>/chars.txt (or asset/ui/<THEME>/chars.txt); each line is
    either a run of literal characters or a hex codepoint range like:
        U+3040-U+30FF       (hiragana + katakana)
        4E00-9FFF           (CJK ideographs)
    '#' starts a comment.  Merged into the theme's atlas (capped by the 2048px
    texture limit, so list only the chars you actually need).
    """
    out = set()
    for base in (
        PROJECT_ROOT / "fonts" / FONT_THEME,
        PROJECT_ROOT / "fonts" / THEME,
        PROJECT_ROOT / "asset" / "ui" / THEME,
    ):
        p = base / "chars.txt"
        if not p.exists():
            continue
        try:
            for raw in p.read_text(encoding="utf-8", errors="ignore").splitlines():
                # 行尾注释要一起砍掉：写成 "U+3040-U+30FF   # 假名" 时旧代码
                # 匹配不到区间，只会把这一行的字面字符（U、+、3、0……和注释
                # 里的汉字）塞进字集，区间本身反而丢了。
                s = raw.split("#", 1)[0].strip()
                if not s:
                    continue
                m = re.match(
                    r"(?:U\+)?([0-9a-fA-F]{4,6})\s*-\s*(?:U\+)?([0-9a-fA-F]{4,6})$", s)
                if m:
                    a = int(m.group(1), 16)
                    b = int(m.group(2), 16)
                    if b - a > 0x2FFF:  # sanity cap a single range
                        b = a + 0x2FFF
                    for cp in range(a, b + 1):
                        if 0x20 <= cp <= 0x10FFFF:
                            out.add(chr(cp))
                else:
                    for c in s:
                        if not c.isspace():
                            out.add(c)
        except Exception:
            pass
    return out


def make_theme_seed():
    """Regenerate app/theme-seed.tsx from app/colors.json so every color class
    referenced in the JSON appears as a class= literal and gets baked by the
    PocketJS compiler (resolveStyle only matches baked literals)."""
    cfg = json.loads((PROJECT_ROOT / "app" / "colors.json").read_text(encoding="utf-8"))
    seen: list[str] = []
    for section in ("bg", "ui"):
        for theme in cfg.get(section, {}).values():
            for v in theme.values():
                if v and v not in seen:
                    seen.append(v)
    # 每个 <Text> 附一个空格子内容，避免被编译器当作空节点优化掉，
    # 从而确保这些完整 class 字面量真的进样式表（resolveStyle 才能命中）。
    lines = "\n".join(f'      <Text class="{v}"> </Text>' for v in seen)
    src = (
        "// auto-generated by scripts/build-vpk.py from app/colors.json (do not edit)\n"
        'import { View, Text } from "@pocketjs/framework/components";\n'
        "export default function ThemeSeed() {\n"
        "  return (\n"
        '    <View style={{ opacity: 0, width: 0, height: 0 }}>\n'
        f"{lines}\n"
        "    </View>\n"
        "  );\n"
        "}\n"
    )
    (PROJECT_ROOT / "app" / "theme-seed.tsx").write_text(src, encoding="utf-8")
