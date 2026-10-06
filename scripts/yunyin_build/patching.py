"""通用文本补丁助手：往 PocketJS 源码里插代码前，先确认锚点还在。"""


def patch(text, find, inject, desc):
    if inject in text:
        return text
    if find not in text:
        raise SystemExit(f"[build-vpk] pattern not found for {desc}: {find[:60]!r}")
    print(f"[build-vpk] patch: {desc}")
    return text.replace(find, find + inject)
