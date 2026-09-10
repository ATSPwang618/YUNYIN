#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""YUNYIN 标签体检 —— 一键检查曲库里的标签能不能被播放器正常读取。

判定规则与 Vita 上的播放器（native/media.rs）完全一致：
  * 只读文件开头 1MB + 64KB 的元数据前缀
  * MP3     : ID3v2 的 TIT2 / TPE1 / TALB / APIC / USLT
              编码字节 0(Latin-1) 与 3(UTF-8) 都按 UTF-8 解，1 / 2 按 UTF-16 小端解
  * FLAC    : VORBIS_COMMENT（TITLE / ARTIST / ALBUM / LYRICS）+ PICTURE
  * OGG/OPUS: VORBIS_COMMENT
  * 封面    : 只认内嵌 JPEG / PNG，且不超过 1MB，超了播放器直接忽略
  * WAV     : 播放器不读标签，只用文件名
  * ID3v1   : 写在文件尾的那种老标签，播放器不读

用法：
    python tagcheck.py                     # 打开图形界面
    python tagcheck.py --cli D:\\Music      # 命令行模式（可配合 --csv / --m3u8）
"""

import os
import re
import sys
import queue
import threading

# --- 与播放器一致的常量 ---------------------------------------------------
PREFIX_CAP = 1024 * 1024 + 65536     # media.rs: PREFIX_CAP
MAX_ART = 1024 * 1024                # media.rs: MAX_ART（封面超过就忽略）
AUDIO_EXT = (".mp3", ".flac", ".ogg", ".oga", ".opus", ".wav")
OTHER_EXT = (".m4a", ".aac", ".wma", ".aiff", ".aif", ".mp4", ".ape", ".wv")
PICARD_URL = "https://picard.musicbrainz.org/"

CJK_RE = re.compile(r"[\u3040-\u30ff\u3400-\u9fff\uff01-\uff5e]")
# 常用汉字（简繁各取一些），用来判断 GBK / Big5 哪种解码更合理
COMMON_CJK = set(
    "的一是不了人我在有他这为之大来以个中上们到说国和地也子时道出而要于就下得可你年生自会那后能对着事其里"
    "所去行过家十用发天如然作方成者多日都三小军二无同么经法当起与好看学进种将还分此心前面又定见只主没公"
    "從們來時實現點開關聲樂聽愛書寫讀臺灣體驗專輯歌手標籤規範檔案資料夾檢查結果錯誤資訊"
)


def _log(msg):
    if sys.stdout is not None:
        try:
            print(msg)
        except Exception:
            pass


def _debug_log(msg):
    """设了环境变量 TAGCHECK_DEBUG_LOG 时，把关键动作写进日志文件（排查/自调用）。"""
    path = os.environ.get("TAGCHECK_DEBUG_LOG")
    if not path:
        return
    try:
        with open(path, "a", encoding="utf-8") as f:
            f.write(msg + "\n")
    except OSError:
        pass


# ---------------------------------------------------------------------------
# 字节读取
# ---------------------------------------------------------------------------
def read_prefix(path, cap=PREFIX_CAP):
    try:
        with open(path, "rb") as f:
            return f.read(cap)
    except OSError:
        return b""


def read_tail(path, count=128):
    try:
        size = os.path.getsize(path)
        with open(path, "rb") as f:
            if size > count:
                f.seek(size - count)
            return f.read(count)
    except OSError:
        return b""


# ---------------------------------------------------------------------------
# 文本解码：复刻播放器的行为，另外猜一下真实编码
# ---------------------------------------------------------------------------
def _strip_nul(s):
    return s.split("\x00")[0].strip()


def _score(text):
    """越像正常中文/日文的解码结果分越高。"""
    if not text:
        return -1
    score = 0
    for ch in text:
        if ch in COMMON_CJK:
            score += 3
        elif "\u4e00" <= ch <= "\u9fff":
            score += 1
        elif "\u3040" <= ch <= "\u30ff":
            score += 1
        elif not ch.isprintable():
            score -= 3
    return score


def decode_id3_text(data):
    """返回 (播放器会显示的文字, 正确内容或 None, 是否乱码, 说明)。"""
    if not data:
        return "", None, False, ""
    enc = data[0]
    body = data[1:]

    if enc in (1, 2):
        # 播放器两种情况都按小端解
        app = _strip_nul(body.decode("utf-16-le", "replace"))
        be = _strip_nul(body.decode("utf-16-be", "replace"))
        if body[:2] == b"\xfe\xff" or (not CJK_RE.search(app) and CJK_RE.search(be)):
            return app, be, True, "UTF-16 大端标签：播放器按小端解，会乱码"
        return app, app, False, ""

    app = _strip_nul(body.decode("utf-8", "replace"))
    try:
        body.decode("utf-8")
        return app, app, False, ""
    except UnicodeDecodeError:
        pass

    best = None
    for codec, label in (("gb18030", "GBK/GB18030"), ("big5", "Big5"), ("shift_jis", "Shift-JIS")):
        try:
            guess = _strip_nul(body.decode(codec))
        except (UnicodeDecodeError, LookupError):
            continue
        if not CJK_RE.search(guess):
            continue
        score = _score(guess)
        if best is None or score > best[0]:
            best = (score, guess, label)
    if best is not None:
        return app, best[1], True, "编码像 %s，播放器会显示乱码" % best[2]
    return app, None, True, "标签不是合法 UTF-8，播放器会显示乱码"


# ---------------------------------------------------------------------------
# ID3v2 / MP3
# ---------------------------------------------------------------------------
def _synchsafe(b, o):
    return ((b[o] & 0x7F) << 21) | ((b[o + 1] & 0x7F) << 14) | ((b[o + 2] & 0x7F) << 7) | (b[o + 3] & 0x7F)


def _be32(b, o):
    return int.from_bytes(b[o:o + 4], "big")


def _le32(b, o):
    return int.from_bytes(b[o:o + 4], "little")


def parse_id3v2(data):
    """返回 dict(version, readable, frames) 或 None（完全没有 ID3v2 时）。"""
    if len(data) < 10 or data[:3] != b"ID3":
        return None
    major, flags = data[3], data[5]
    tag_size = _synchsafe(data, 6)
    end = min(10 + tag_size, len(data))
    frames = {}
    if major == 2:
        return {"version": 2, "readable": False, "frames": frames}
    pos = 10
    if flags & 0x40 and pos + 4 <= end:
        ext = _synchsafe(data, pos) if major >= 4 else _be32(data, pos)
        pos = min(pos + max(ext, 4), end)
    while pos + 10 <= end:
        if data[pos] == 0:
            break
        fid = data[pos:pos + 4]
        size = _synchsafe(data, pos + 4) if major >= 4 else _be32(data, pos + 4)
        pos += 10
        if size <= 0 or pos + size > end:
            break
        if fid not in frames:
            frames[fid] = data[pos:pos + size]
        pos += size
    return {"version": major, "readable": True, "frames": frames}


def find_image(data):
    """在 APIC / PICTURE 数据里搜 JPEG / PNG 魔数（播放器也是这么找的）。"""
    if not data:
        return None
    cands = []
    jpeg = data.find(b"\xff\xd8")
    png = data.find(b"\x89PNG")
    if jpeg >= 0:
        cands.append((jpeg, "JPEG"))
    if png >= 0:
        cands.append((png, "PNG"))
    if not cands:
        return None
    off, kind = min(cands)
    return {"kind": kind, "size": len(data) - off}


# ---------------------------------------------------------------------------
# FLAC / OGG / Vorbis comment
# ---------------------------------------------------------------------------
def parse_vorbis_comments(body):
    """vendorLen + vendor + count + count×(len + "KEY=value")，键名大小写不敏感。"""
    out = {}
    if not body or len(body) < 8:
        return out
    vendor = _le32(body, 0)
    p = 4 + vendor
    if p + 4 > len(body):
        return out
    count = _le32(body, p)
    p += 4
    if count > 4096:
        count = 4096
    for _ in range(count):
        if p + 4 > len(body):
            break
        n = _le32(body, p)
        p += 4
        if n < 0 or p + n > len(body):
            break
        comment = body[p:p + n].decode("utf-8", "replace")
        p += n
        eq = comment.find("=")
        if eq > 0:
            key = comment[:eq].upper()
            if key not in out:
                out[key] = comment[eq + 1:].strip()
    return out


def parse_flac(data):
    if len(data) < 8 or data[:4] != b"fLaC":
        return None
    pos = 4
    comment = None
    picture = None
    while pos + 4 <= len(data):
        head = data[pos]
        last = head & 0x80
        btype = head & 0x7F
        size = int.from_bytes(data[pos + 1:pos + 4], "big")
        pos += 4
        if size < 0 or pos + size > len(data):
            break
        block = data[pos:pos + size]
        if btype == 4:
            comment = block
        elif btype == 6 and picture is None and len(block) > 32:
            mime_len = _be32(block, 4)
            o = 8 + mime_len
            if o + 4 <= len(block):
                desc_len = _be32(block, o)
                o = o + 4 + desc_len + 16
                if o + 4 <= len(block):
                    picture = {"kind": "FLAC PICTURE", "size": _be32(block, o)}
        pos += size
        if last:
            break
    return {"comment": comment, "picture": picture}


def parse_ogg(data):
    """找 Ogg 里的 comment header（\\x03vorbis）再解 Vorbis comment。"""
    if not data:
        return {}
    i = data.find(b"\x03vorbis")
    if i < 0:
        return {}
    return parse_vorbis_comments(data[i + 7:])


# ---------------------------------------------------------------------------
# 单个文件体检
# ---------------------------------------------------------------------------
def check_file(path):
    ext = os.path.splitext(path)[1].lower()
    data = read_prefix(path)
    row = {
        "file": path, "name": os.path.basename(path), "format": ext.lstrip(".").upper(),
        "title": "", "artist": "", "album": "", "cover": "", "lyrics": "",
        "status": "", "notes": [], "missing": [],
    }
    notes = row["notes"]
    broken = False

    if ext == ".mp3":
        id3 = parse_id3v2(data)
        tail = read_tail(path, 128)
        has_v1 = len(tail) >= 128 and tail[:3] == b"TAG"
        if id3 is None:
            notes.append("只有 ID3v1 标签（播放器不读，会退回文件名）" if has_v1
                         else "没有 ID3v2 标签（播放器会拿文件名当标题）")
        elif not id3["readable"]:
            notes.append("ID3v2.2 标签：帧格式太老，播放器读不了，建议用 Picard 重存")
        else:
            frames = id3["frames"]
            for fid, field in ((b"TIT2", "title"), (b"TPE1", "artist"), (b"TALB", "album")):
                if fid in frames:
                    app, fixed, bad, note = decode_id3_text(frames[fid])
                    # 乱码时表格里显示「正确内容」并加个 ⚠，说明栏再写清楚播放器实际会显示什么
                    row[field] = ("⚠ " + fixed) if (bad and fixed) else app
                    if bad:
                        broken = True
                        suffix = "（正确内容应为「%s」）" % fixed if fixed else ""
                        notes.append("%s：%s（播放器显示：%s）%s" % (field, note, app or "空", suffix))
            if b"APIC" in frames:
                img = find_image(frames[b"APIC"])
                if img is None:
                    row["cover"] = "有(格式不支持)"
                    notes.append("内嵌封面不是 JPEG/PNG：播放器认不出来")
                elif img["size"] > MAX_ART:
                    row["cover"] = "有(%s >1MB)" % img["kind"]
                    notes.append("内嵌封面超过 1MB：播放器会忽略，请压到 1MB 以内")
                else:
                    row["cover"] = "有(%s)" % img["kind"]
            lyric = ""
            if b"USLT" in frames:
                lyric = decode_id3_text(frames[b"USLT"])[0]
            if not lyric.strip() and b"TXXX" in frames:
                txxx = decode_id3_text(frames[b"TXXX"])[0]
                if "lyrics" in txxx.lower():
                    lyric = txxx
            if lyric.strip():
                row["lyrics"] = "有"
            if has_v1 and not row["title"] and not row["artist"] and not row["album"]:
                notes.append("ID3v2 里没有歌名/歌手/专辑，只有文件尾的 ID3v1（播放器不读）")

    elif ext == ".flac":
        flac = parse_flac(data)
        if flac is None:
            notes.append("不是标准 FLAC 头，播放器可能读不到标签")
        else:
            fields = parse_vorbis_comments(flac["comment"])
            row["title"] = fields.get("TITLE", "")
            row["artist"] = fields.get("ARTIST", "")
            row["album"] = fields.get("ALBUM", "")
            if fields.get("LYRICS"):
                row["lyrics"] = "有"
            pic = flac["picture"]
            if pic:
                if pic["size"] > MAX_ART:
                    row["cover"] = "有(>1MB)"
                    notes.append("内嵌封面超过 1MB：播放器会忽略")
                else:
                    row["cover"] = "有(%s)" % pic["kind"]

    elif ext in (".ogg", ".oga", ".opus"):
        fields = parse_ogg(data)
        row["title"] = fields.get("TITLE", "")
        row["artist"] = fields.get("ARTIST", "")
        row["album"] = fields.get("ALBUM", "")
        if fields.get("LYRICS"):
            row["lyrics"] = "有"
        if fields.get("METADATA_BLOCK_PICTURE") or fields.get("COVERART"):
            row["cover"] = "有(OGG 内嵌)"

    elif ext == ".wav":
        notes.append("WAV：播放器不读标签，只用文件名")

    missing = []
    if not row["title"]:
        missing.append("标题")
        row["title"] = "（缺，用文件名）"
    if not row["artist"]:
        missing.append("歌手")
        row["artist"] = "（缺 → Local）"
    if not row["album"]:
        missing.append("专辑")
        row["album"] = "（缺 → Unknown）"
    if not row["cover"]:
        missing.append("封面")
        row["cover"] = "无"
    if not row["lyrics"]:
        missing.append("歌词")
        row["lyrics"] = "无"

    if broken:
        row["status"] = "乱码"
    elif len(missing) >= 4:
        row["status"] = "建议整理"
    elif missing:
        row["status"] = "可改善"
    else:
        row["status"] = "完整"
    if missing:
        notes.insert(0, "缺：" + " / ".join(missing))
    row["missing"] = missing
    return row


def collect_paths(paths, max_files=2000):
    """把「文件 + 文件夹」混合的输入展开成音频文件列表。

    返回 (files, skipped_unsupported)。拖进来的单个音频文件会原样收下，
    文件夹则递归展开；不支持的扩展名（m4a / wma …）只计入 skipped。
    """
    files = []
    others = 0
    seen = set()

    def add(path):
        nonlocal others
        if len(files) >= max_files:
            return
        key = os.path.normcase(os.path.abspath(path))
        if key in seen:
            return
        ext = os.path.splitext(path)[1].lower()
        if ext in AUDIO_EXT:
            seen.add(key)
            files.append(path)
        elif ext in OTHER_EXT:
            others += 1

    for item in paths:
        if os.path.isdir(item):
            for base, _dirs, names in os.walk(item):
                for n in sorted(names):
                    add(os.path.join(base, n))
                if len(files) >= max_files:
                    break
        elif os.path.isfile(item):
            add(item)
    return files[:max_files], others


def scan_paths(paths, max_files=2000, progress=None, should_stop=None):
    """体检一批「文件 + 文件夹」，返回 (rows, 播放器不支持的格式数量)。"""
    files, others = collect_paths(paths, max_files)
    _debug_log("scan_paths: %d 个音频文件（不支持格式 %d）" % (len(files), others))
    rows = []
    total = max(1, len(files))
    for i, path in enumerate(files, 1):
        if should_stop is not None and should_stop():
            break
        try:
            rows.append(check_file(path))
        except Exception as exc:                      # 单个文件出错不影响整体
            rows.append({"file": path, "name": os.path.basename(path), "format": "?",
                         "title": "", "artist": "", "album": "", "cover": "", "lyrics": "",
                         "status": "读取失败", "notes": [str(exc)], "missing": []})
        if progress is not None:
            progress(i, total, os.path.basename(path))
    return rows, others


def scan_folder(root, max_files=2000, progress=None, should_stop=None):
    """兼容旧调用：只扫一个文件夹。"""
    return scan_paths([root], max_files, progress, should_stop)


def summarize(rows):
    def count(pred):
        return sum(1 for r in rows if pred(r))
    return {
        "total": len(rows),
        "ok": count(lambda r: r["status"] == "完整"),
        "bad": count(lambda r: r["status"] != "完整"),
        "garbled": count(lambda r: r["status"] == "乱码"),
        "no_title": count(lambda r: r["title"].startswith("（缺")),
        "no_artist": count(lambda r: r["artist"].startswith("（缺")),
        "no_album": count(lambda r: r["album"].startswith("（缺")),
        "no_cover": count(lambda r: r["cover"] == "无"),
        "no_lyrics": count(lambda r: r["lyrics"] == "无"),
    }


def write_csv(rows, path):
    import csv
    with open(path, "w", newline="", encoding="utf-8-sig") as f:
        w = csv.writer(f)
        w.writerow(["文件", "格式", "标题", "歌手", "专辑", "封面", "歌词", "状态", "说明"])
        for r in rows:
            w.writerow([r["file"], r["format"], r["title"], r["artist"], r["album"],
                        r["cover"], r["lyrics"], r["status"], "；".join(r["notes"])])


def write_m3u8(rows, path):
    with open(path, "w", encoding="utf-8") as f:
        f.write("#EXTM3U\n")
        for r in rows:
            if r["status"] != "完整":
                f.write(r["file"] + "\n")


# ---------------------------------------------------------------------------
# 命令行模式
# ---------------------------------------------------------------------------
def run_cli(argv):
    import argparse
    ap = argparse.ArgumentParser(description="YUNYIN 标签体检（命令行模式）")
    ap.add_argument("--cli", metavar="路径", nargs="+", required=True,
                    help="要检查的音乐文件夹或音频文件（可以给多个）")
    ap.add_argument("--csv", help="导出明细 CSV")
    ap.add_argument("--m3u8", help="导出 Picard 待修清单")
    ap.add_argument("--only-problems", action="store_true", help="只列需要处理的")
    ap.add_argument("--max-files", type=int, default=2000)
    args = ap.parse_args(argv)

    paths = [p for p in args.cli if os.path.exists(p)]
    if not paths:
        _log("路径不存在：" + " ".join(args.cli))
        return 1

    def progress(i, total, _name):
        if i % 25 == 0 or i == total:
            _log("  ...%d/%d" % (i, total))

    rows, others = scan_paths(paths, args.max_files, progress)
    s = summarize(rows)
    _log("")
    _log("YUNYIN 标签体检  " + (paths[0] if len(paths) == 1 else "(%d 项输入) %s …" % (len(paths), paths[0])))
    _log("扫描 %d 首%s" % (s["total"], ("（另有 %d 个播放器不支持的格式已跳过）" % others) if others else ""))
    _log("")
    for r in rows:
        if args.only_problems and r["status"] == "完整":
            continue
        _log("[%s] %s" % (r["status"], r["name"]))
        if r["status"] != "完整":
            _log("        " + "；".join(r["notes"]))
    _log("")
    _log("完整 %d 首 / 需处理 %d 首%s" % (s["ok"], s["bad"],
                                        ("（其中乱码 %d 首）" % s["garbled"]) if s["garbled"] else ""))
    _log("缺标题 %d · 缺歌手 %d · 缺专辑 %d · 无封面 %d · 无歌词 %d"
         % (s["no_title"], s["no_artist"], s["no_album"], s["no_cover"], s["no_lyrics"]))
    if args.csv:
        write_csv(rows, args.csv)
        _log("明细已导出：" + os.path.abspath(args.csv))
    if args.m3u8:
        write_m3u8(rows, args.m3u8)
        _log("待修清单已导出：" + os.path.abspath(args.m3u8))
    return 0


# ---------------------------------------------------------------------------
# 图形界面（tkinter，Python 自带，不需要额外安装）
# ---------------------------------------------------------------------------
STATUS_COLOR = {"完整": "#1a7f37", "可改善": "#9a6700", "建议整理": "#9a6700",
                "乱码": "#cf222e", "读取失败": "#cf222e"}


def open_url(url):
    try:
        if os.name == "nt":
            os.startfile(url)          # noqa: S606 （Windows 上用默认浏览器打开）
        else:
            import webbrowser
            webbrowser.open(url)
    except Exception:
        _log(url)


def _all_widgets(widget):
    yield widget
    for child in widget.winfo_children():
        yield from _all_widgets(child)


def enable_windows_drag_drop(root, on_drop):
    """把 Windows 资源管理器的文件拖放（WM_DROPFILES）接到窗口上。

    只用 ctypes 调 shell32 / user32，不依赖第三方库，也不需要额外打包 DLL。
    非 Windows 或失败时返回 False —— 界面上的「选择文件夹 / 选择文件」照旧可用。
    """
    if os.name != "nt":
        return False
    try:
        import ctypes
        from ctypes import wintypes

        user32 = ctypes.WinDLL("user32", use_last_error=True)
        shell32 = ctypes.WinDLL("shell32", use_last_error=True)

        WM_DROPFILES = 0x0233
        GWLP_WNDPROC = -4
        LRESULT = ctypes.c_ssize_t
        WNDPROC = ctypes.WINFUNCTYPE(LRESULT, wintypes.HWND, ctypes.c_uint,
                                     wintypes.WPARAM, wintypes.LPARAM)

        shell32.DragAcceptFiles.argtypes = [wintypes.HWND, wintypes.BOOL]
        shell32.DragQueryFileW.restype = ctypes.c_uint
        shell32.DragQueryFileW.argtypes = [wintypes.HANDLE, ctypes.c_uint,
                                           wintypes.LPWSTR, ctypes.c_uint]
        shell32.DragFinish.argtypes = [wintypes.HANDLE]

        set_long = getattr(user32, "SetWindowLongPtrW", None) or user32.SetWindowLongW
        set_long.restype = ctypes.c_void_p
        set_long.argtypes = [wintypes.HWND, ctypes.c_int, WNDPROC]
        call_prev = user32.CallWindowProcW
        call_prev.restype = LRESULT
        call_prev.argtypes = [ctypes.c_void_p, wintypes.HWND, ctypes.c_uint,
                              wintypes.WPARAM, wintypes.LPARAM]

        def take_paths(hdrop):
            paths = []
            count = shell32.DragQueryFileW(hdrop, 0xFFFFFFFF, None, 0)
            for i in range(count):
                need = shell32.DragQueryFileW(hdrop, i, None, 0) + 1
                buf = ctypes.create_unicode_buffer(need)
                shell32.DragQueryFileW(hdrop, i, buf, need)
                if buf.value:
                    paths.append(buf.value)
            shell32.DragFinish(hdrop)
            return paths

        keep_alive = []
        old_procs = {}

        def make_proc(hwnd):
            def proc(h, msg, wparam, lparam):
                if msg == WM_DROPFILES:
                    try:
                        paths = take_paths(wparam)
                        _debug_log("drop: " + " | ".join(paths))
                        # 注意：窗口过程里不要直接调 Tkinter（after/bind 都可能不出队），
                        # 只把结果丢进队列，让界面线程自己取。
                        on_drop(paths)
                    except Exception as exc:            # 拖放失败不能拖垮界面
                        _debug_log("drop failed: %r" % (exc,))
                    return 0
                return call_prev(old_procs[hwnd], h, msg, wparam, lparam)
            return WNDPROC(proc)

        for widget in _all_widgets(root):
            try:
                hwnd = user32.GetParent(widget.winfo_id()) or widget.winfo_id()
            except Exception:
                continue
            if not hwnd or hwnd in old_procs:
                continue
            shell32.DragAcceptFiles(wintypes.HWND(hwnd), True)
            proc = make_proc(hwnd)
            old = set_long(wintypes.HWND(hwnd), GWLP_WNDPROC, proc)
            old_procs[hwnd] = old
            keep_alive.append(proc)
        root._tagcheck_dnd = keep_alive             # 挂在 root 上，防止被 GC 回收
        _debug_log("drag&drop 已启用（%d 个窗口: %s）"
                   % (len(keep_alive), ",".join(str(h) for h in old_procs)))
        return True
    except Exception as exc:
        _debug_log("drag&drop 不可用: %r" % (exc,))
        return False


def run_gui():
    import tkinter as tk
    from tkinter import ttk, filedialog, messagebox

    root = tk.Tk()
    root.title("YUNYIN 标签体检 —— 一键检查曲库标签是否规范")
    # 按屏幕大小开窗并居中（高分屏缩放时也不会把列挤出窗口）
    sw, sh = root.winfo_screenwidth(), root.winfo_screenheight()
    win_w = max(900, min(1280, int(sw * 0.78)))
    win_h = max(520, min(760, int(sh * 0.78)))
    root.geometry("%dx%d+%d+%d" % (win_w, win_h,
                                   max(0, (sw - win_w) // 2), max(0, (sh - win_h) // 3)))
    root.minsize(880, 500)

    def fit_window():
        """按控件实际需要的尺寸再定一次窗口大小（高分屏缩放时内容会更高）。"""
        root.update_idletasks()
        w = min(max(win_w, root.winfo_reqwidth() + 24), max(900, sw - 40))
        h = min(max(win_h, root.winfo_reqheight() + 24), max(520, sh - 80))
        root.geometry("%dx%d+%d+%d" % (w, h, max(0, (sw - w) // 2), max(0, (sh - h) // 3)))
        root.update_idletasks()
        _debug_log("layout: window=%dx%d req=%dx%d table=%dx%d bottom=%d (窗高 %d)"
                   % (w, h, root.winfo_reqwidth(), root.winfo_reqheight(),
                      tree.winfo_width(), tree.winfo_height(),
                      buttons.winfo_y() + buttons.winfo_height(), h))

    state = {"rows": [], "root_dir": "", "inputs": []}
    msg_queue = queue.Queue()
    item_rows = {}          # Treeview 行 → 体检结果，双击时按行取，避免同名文件串台

    # ---- 顶部：文件夹 + 开始 ---------------------------------------------
    top = ttk.Frame(root, padding=10)
    top.pack(fill="x")
    ttk.Label(top, text="音乐文件夹：").pack(side="left")
    path_var = tk.StringVar()
    entry = ttk.Entry(top, textvariable=path_var)
    entry.pack(side="left", fill="x", expand=True, padx=(0, 8))

    def choose_dir():
        d = filedialog.askdirectory(title="选择音乐文件夹")
        if d:
            path_var.set(d)

    ttk.Button(top, text="选择文件夹…", command=choose_dir).pack(side="left")
    ttk.Button(top, text="选择文件…", command=lambda: choose_files()).pack(side="left", padx=(6, 0))
    start_btn = ttk.Button(top, text="开始检查")
    start_btn.pack(side="left", padx=6)

    def set_inputs(paths):
        """记录本次要检查的输入（文件或文件夹，可多个）。"""
        state["inputs"] = list(paths)
        if not paths:
            path_var.set("")
        elif len(paths) == 1:
            path_var.set(paths[0])
        else:
            path_var.set("%s  等 %d 项" % (paths[0], len(paths)))

    def choose_files():
        d = filedialog.askopenfilenames(
            title="选择音频文件（可多选）",
            filetypes=[("音频文件", "*.mp3 *.flac *.ogg *.oga *.opus *.wav"), ("所有文件", "*.*")])
        if d:
            set_inputs(list(d))

    def on_drop(paths):
        """拖进来：文件夹递归展开，单个音频文件原样收下，可一次拖多个。"""
        try:
            _debug_log("on_drop: %d 项" % len(paths))
            set_inputs(paths)
            stat_var.set("已接收 %d 项，正在开始检查…" % len(paths))
            start_scan()
        except Exception as exc:
            _debug_log("on_drop failed: %r" % (exc,))

    entry.bind("<Key>", lambda _e: state.__setitem__("inputs", []))   # 手打路径时以输入框为准

    opts = ttk.Frame(root, padding=(10, 0))
    opts.pack(fill="x")
    only_bad = tk.BooleanVar(value=True)
    check_btn = ttk.Checkbutton(opts, text="只看有问题的", variable=only_bad)
    check_btn.pack(side="left")
    progress = ttk.Progressbar(opts, mode="determinate", length=280)
    progress.pack(side="right")

    # ---- 结果表 ----------------------------------------------------------
    # 说明单独放在表格下方（选中哪行显示哪行），表格只留短字段，
    # 这样列宽固定也不会被系统缩放挤出窗口。
    columns = ("status", "name", "title", "artist", "album", "cover", "lyrics")
    heads = {"status": ("状态", 56), "name": ("文件", 175), "title": ("标题", 130),
             "artist": ("歌手", 110), "album": ("专辑", 110), "cover": ("封面", 76),
             "lyrics": ("歌词", 44)}
    frame = ttk.Frame(root, padding=(10, 6))
    frame.pack(fill="both", expand=True)
    tree = ttk.Treeview(frame, columns=columns, show="headings", selectmode="browse")
    for c in columns:
        text, width = heads[c]
        tree.heading(c, text=text)
        tree.column(c, width=width, minwidth=40, anchor="w", stretch=False)
    vs = ttk.Scrollbar(frame, orient="vertical", command=tree.yview)
    tree.configure(yscrollcommand=vs.set)
    tree.pack(side="left", fill="both", expand=True)
    vs.pack(side="right", fill="y")
    for status, color in STATUS_COLOR.items():
        tree.tag_configure(status, foreground=color)

    # ---- 底部：统计 + 导出 ----------------------------------------------
    detail_var = tk.StringVar(value="选中上面任意一行，这里会显示它的完整说明。")
    detail = tk.Label(root, textvariable=detail_var, justify="left", anchor="w",
                      wraplength=1100, height=3, bg="#f6f8fa", fg="#24292f",
                      relief="solid", borderwidth=1, padx=8, pady=4)
    detail.pack(fill="x", padx=10, pady=(0, 6))

    bottom = ttk.Frame(root, padding=(10, 0))
    bottom.pack(fill="x")
    stat_var = tk.StringVar(
        value="选文件夹 / 选文件 / 把文件或文件夹拖进窗口都可以（可多选）。规则和 Vita 上的播放器完全一致。")
    ttk.Label(bottom, textvariable=stat_var, justify="left", wraplength=1060).pack(anchor="w")

    buttons = ttk.Frame(root, padding=(10, 8, 10, 10))
    buttons.pack(fill="x")
    csv_btn = ttk.Button(buttons, text="导出明细 CSV", state="disabled")
    csv_btn.pack(side="left")
    m3u_btn = ttk.Button(buttons, text="导出 Picard 待修清单", state="disabled")
    m3u_btn.pack(side="left", padx=6)
    ttk.Button(buttons, text="打开 Picard 官网", command=lambda: open_url(PICARD_URL)).pack(side="left")
    ttk.Label(buttons, text="流程：导出待修清单 → 拖进 Picard → 全选 Lookup → Save",
              foreground="#666").pack(side="right")

    # ---- 表格刷新 / 详情 --------------------------------------------------
    def refresh_table():
        tree.delete(*tree.get_children())
        item_rows.clear()
        rows = state["rows"]
        for r in rows:
            if only_bad.get() and r["status"] == "完整":
                continue
            item = tree.insert("", "end", values=(r["status"], r["name"], r["title"], r["artist"],
                                                  r["album"], r["cover"], r["lyrics"]),
                               tags=(r["status"],))
            item_rows[item] = r
        if rows:
            s = summarize(rows)
            stat_var.set(
                "完整 %d 首 / 需处理 %d 首%s        缺标题 %d · 缺歌手 %d · 缺专辑 %d · 无封面 %d · 无歌词 %d"
                % (s["ok"], s["bad"], ("（其中乱码 %d 首）" % s["garbled"]) if s["garbled"] else "",
                   s["no_title"], s["no_artist"], s["no_album"], s["no_cover"], s["no_lyrics"]))

    check_btn.configure(command=refresh_table)

    def show_detail(_event=None):
        sel = tree.selection()
        if not sel or sel[0] not in item_rows:
            return
        r = item_rows[sel[0]]
        messagebox.showinfo("%s · %s" % (r["status"], r["name"]),
                            "标题：%s\n歌手：%s\n专辑：%s\n封面：%s\n歌词：%s\n\n%s\n\n%s"
                            % (r["title"], r["artist"], r["album"], r["cover"], r["lyrics"],
                               "；".join(r["notes"]) or "（没发现问题）", r["file"]))

    tree.bind("<Double-1>", show_detail)

    def show_selected(_event=None):
        sel = tree.selection()
        if not sel or sel[0] not in item_rows:
            detail_var.set("选中上面任意一行，这里会显示它的完整说明。")
            return
        r = item_rows[sel[0]]
        detail_var.set("[%s] %s\n%s\n%s"
                       % (r["status"], r["name"], "；".join(r["notes"]) or "（没发现问题）", r["file"]))

    tree.bind("<<TreeviewSelect>>", show_selected)

    # ---- 扫描（后台线程 + 队列，界面不卡） --------------------------------
    def scan_thread(paths):
        def progress_cb(i, total, name):
            msg_queue.put(("progress", i, total, name))

        rows, others = scan_paths(paths, progress=progress_cb)
        msg_queue.put(("done", rows, others))

    def pump():
        try:
            while True:
                msg = msg_queue.get_nowait()
                if msg[0] == "drop":
                    _debug_log("pump: 收到 %d 项拖放" % len(msg[1]))
                    on_drop(msg[1])
                elif msg[0] == "progress":
                    _, i, total, name = msg
                    progress["value"] = 100.0 * i / total
                    stat_var.set("正在读取 %d/%d：%s" % (i, total, name))
                else:
                    _, rows, others = msg
                    state["rows"] = rows
                    _debug_log("scan done: %d 行（不支持格式 %d）" % (len(rows), others))
                    progress["value"] = 0
                    start_btn["state"] = "normal"
                    csv_btn["state"] = "normal"
                    m3u_btn["state"] = "normal"
                    refresh_table()
                    s = summarize(rows)
                    if others:
                        stat_var.set(stat_var.get() + "        （另有 %d 个播放器不支持的格式已跳过）" % others)
                    if not os.environ.get("TAGCHECK_DEBUG_LOG"):     # 自动测试时不弹窗
                        if s["bad"] == 0:
                            messagebox.showinfo("体检完成", "太好了，所有歌曲的标签都能被播放器正常读取。")
                        else:
                            messagebox.showinfo(
                                "体检完成",
                                "有 %d 首要处理%s。\n\n下一步：点「导出 Picard 待修清单」，"
                                "把生成的 m3u8 拖进 Picard，全选 → Lookup → Save 即可。"
                                % (s["bad"], ("（其中 %d 首乱码）" % s["garbled"]) if s["garbled"] else ""))
        except queue.Empty:
            pass
        root.after(120, pump)

    def start_scan():
        _debug_log("start_scan: inputs=%d" % len(state["inputs"]))
        inputs = [p for p in state["inputs"] if os.path.exists(p)]
        if not inputs:
            typed = path_var.get().strip().strip('"')
            if typed and os.path.exists(typed):
                inputs = [typed]
        if not inputs:
            messagebox.showwarning("还没有选择音乐",
                                   "点「选择文件夹…」或「选择文件…」，也可以直接把文件/文件夹拖进窗口。")
            return
        state["inputs"] = inputs
        first = inputs[0]
        state["root_dir"] = first if os.path.isdir(first) else os.path.dirname(first)
        state["rows"] = []
        tree.delete(*tree.get_children())
        start_btn["state"] = "disabled"
        csv_btn["state"] = "disabled"
        m3u_btn["state"] = "disabled"
        stat_var.set("开始扫描 %d 项输入…" % len(inputs) if len(inputs) > 1 else "开始扫描…")
        threading.Thread(target=scan_thread, args=(inputs,), daemon=True).start()

    start_btn.configure(command=start_scan)
    entry.bind("<Return>", lambda _e: start_scan())

    # ---- 导出 ------------------------------------------------------------
    def export_csv():
        if not state["rows"]:
            return
        path = filedialog.asksaveasfilename(title="导出明细 CSV", defaultextension=".csv",
                                           initialfile="tag-report.csv",
                                           initialdir=state["root_dir"] or os.getcwd(),
                                           filetypes=[("CSV 文件", "*.csv")])
        if not path:
            return
        write_csv(state["rows"], path)
        messagebox.showinfo("已导出", "明细已导出到：\n" + path)

    def export_m3u():
        if not state["rows"]:
            return
        path = filedialog.asksaveasfilename(title="导出 Picard 待修清单", defaultextension=".m3u8",
                                           initialfile="music-to-fix.m3u8",
                                           initialdir=state["root_dir"] or os.getcwd(),
                                           filetypes=[("播放列表", "*.m3u8")])
        if not path:
            return
        write_m3u8(state["rows"], path)
        messagebox.showinfo(
            "已导出",
            "待修清单已导出：\n%s\n\n把它拖进 MusicBrainz Picard，全选 → Lookup → Save。" % path)

    csv_btn.configure(command=export_csv)
    m3u_btn.configure(command=export_m3u)

    # 所有控件建好之后再挂拖放，否则表格、按钮区收不到拖进来的文件。
    # 传入的只是「把路径塞进队列」，真正的界面操作由 pump 在界面线程里做。
    fit_window()
    enable_windows_drag_drop(root, lambda paths: msg_queue.put(("drop", paths)))

    root.after(120, pump)
    root.mainloop()
    return 0


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--cli":
        return run_cli(sys.argv[1:])
    try:
        return run_gui()
    except ImportError as exc:
        _log("图形界面需要 tkinter（Python 安装时勾选 tcl/tk）：%s" % exc)
        _log("可先用命令行模式：python tagcheck.py --cli 音乐文件夹")
        return 2


if __name__ == "__main__":
    sys.exit(main())
