"""皮肤 PNG 归一化（2 的幂 + 512 上限）与 app/images.json 清单维护。"""

import json
import math
import struct
import zlib
from pathlib import Path


# --- skin art normalisation ----------------------------------------------
# Every skin PNG is baked into a GPU texture, and the PocketJS texture format
# requires power-of-two dims with a hard 512px cap per side (contracts/spec
# TEX_MAX_DIM, re-checked by the engine when the texture is uploaded).  The
# art is authored at whatever size the drawing tool produced, so oversized /
# odd-sized skin PNGs are downscaled to the closest allowed size while
# *staging* — the project's own files are never touched.  Skin images are
# drawn stretched over their rect (`absolute inset-0 w-full h-full`), so the
# target aspect only decides how many texels the GPU gets, not the geometry.
TEX_MAX_DIM = 512


_POW2_DIMS = [1 << i for i in range(TEX_MAX_DIM.bit_length() - 1, -1, -1)]


def _is_pow2(n):
    return n > 0 and (n & (n - 1)) == 0


def _png_decode(path):
    """Reader for the format the skin art uses: 8-bit RGBA, non-interlaced.
    Returns (w, h, rgba bytearray)."""
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"not a PNG: {path}")
    pos, idat, hdr = 8, bytearray(), None
    while pos + 12 <= len(data):
        ln = int.from_bytes(data[pos:pos + 4], "big")
        typ = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + ln]
        pos += 12 + ln
        if typ == b"IHDR":
            hdr = struct.unpack(">IIBBBBB", body)
        elif typ == b"IDAT":
            idat += body
        elif typ == b"IEND":
            break
    if hdr is None:
        raise ValueError(f"no IHDR: {path}")
    w, h, depth, ctype, _comp, _filt, interlace = hdr
    if (depth, ctype, interlace) != (8, 6, 0):
        raise ValueError(f"unsupported PNG (depth={depth} ctype={ctype} "
                         f"interlace={interlace}): {path}")
    raw = zlib.decompress(bytes(idat))
    stride = w * 4
    out = bytearray(w * h * 4)
    prev = bytes(stride)
    p = 0
    for y in range(h):
        f = raw[p]
        p += 1
        line = bytearray(raw[p:p + stride])
        p += stride
        if f == 1:
            for i in range(4, stride):
                line[i] = (line[i] + line[i - 4]) & 0xFF
        elif f == 2:
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif f == 3:
            for i in range(stride):
                a = line[i - 4] if i >= 4 else 0
                line[i] = (line[i] + ((a + prev[i]) >> 1)) & 0xFF
        elif f == 4:
            for i in range(stride):
                a = line[i - 4] if i >= 4 else 0
                b = prev[i]
                c = prev[i - 4] if i >= 4 else 0
                pa = abs(b - c)
                pb = abs(a - c)
                pc = abs(a + b - 2 * c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[i] = (line[i] + pr) & 0xFF
        elif f != 0:
            raise ValueError(f"bad PNG filter {f}: {path}")
        out[y * stride:(y + 1) * stride] = line
        prev = line
    return w, h, out


def _png_encode(path, w, h, rgba):
    """Write 8-bit RGBA in a single filter-0 IDAT (what the baker reads back)."""
    stride = w * 4
    raw = bytearray()
    for y in range(h):
        raw.append(0)
        raw += rgba[y * stride:(y + 1) * stride]

    def chunk(typ, body):
        return (struct.pack(">I", len(body)) + typ + body +
                struct.pack(">I", zlib.crc32(typ + body) & 0xFFFFFFFF))

    ihdr = struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)
    path.write_bytes(b"\x89PNG\r\n\x1a\n" +
                     chunk(b"IHDR", ihdr) +
                     chunk(b"IDAT", zlib.compress(bytes(raw), 9)) +
                     chunk(b"IEND", b""))


def _tex_fit(w, h):
    """Closest allowed texture size: power-of-two per side, <= TEX_MAX_DIM and
    never upscaled.  Picked by aspect so the resample stays near-isotropic."""
    src_ar = w / h
    best = None
    for tw in _POW2_DIMS:
        if tw > w:
            continue
        for th in _POW2_DIMS:
            if th > h:
                continue
            key = (abs(math.log2((tw / th) / src_ar)), -(tw * th))
            if best is None or key < best[0]:
                best = (key, tw, th)
    return (best[1], best[2]) if best else (w, h)


def _box_downscale(rgba, w, h, tw, th):
    """Area-average downscale — the right low-pass for these scale factors.
    Averages premultiplied alpha so alpha edges don't pick up a dark fringe."""
    cols = [(x * w // tw, max(x * w // tw + 1, (x + 1) * w // tw))
            for x in range(tw)]
    rows = [(y * h // th, max(y * h // th + 1, (y + 1) * h // th))
            for y in range(th)]
    out = bytearray(tw * th * 4)
    for ty in range(th):
        y0, y1 = rows[ty]
        orow = ty * tw * 4
        for tx in range(tw):
            x0, x1 = cols[tx]
            r = g = b = a = n = 0
            for y in range(y0, y1):
                base = (y * w + x0) * 4
                for i in range(base, base + (x1 - x0) * 4, 4):
                    al = rgba[i + 3]
                    r += rgba[i] * al
                    g += rgba[i + 1] * al
                    b += rgba[i + 2] * al
                    a += al
                    n += 1
            o = orow + tx * 4
            out[o] = r // a if a else 0
            out[o + 1] = g // a if a else 0
            out[o + 2] = b // a if a else 0
            out[o + 3] = a // n if n else 0
    return out


def normalize_assets(root):
    """Downscale every staged skin PNG the Vita texture format can't take."""
    if not root.is_dir():
        return
    fixed = []
    for p in sorted(root.rglob("*.png")):
        try:
            w, h, rgba = _png_decode(p)
        except ValueError as exc:
            print(f"[build-vpk] asset left as-is: {exc}")
            continue
        if w <= TEX_MAX_DIM and h <= TEX_MAX_DIM and _is_pow2(w) and _is_pow2(h):
            continue
        tw, th = _tex_fit(w, h)
        if (tw, th) == (w, h):
            continue
        _png_encode(p, tw, th, _box_downscale(rgba, w, h, tw, th))
        fixed.append(f"{p.relative_to(root).as_posix()}: {w}x{h} -> {tw}x{th}")
    if fixed:
        print(f"[build-vpk] resized {len(fixed)} skin image(s) to the "
              f"{TEX_MAX_DIM}px power-of-two texture limit:")
        for line in fixed:
            print("           ", line)


def refresh_image_manifest(app_dst):
    """Keep the staged images.json in sync with the skin art.

    images.json only supplies per-image *metadata* (the linear flag the baker
    needs for art drawn scaled, and optional psm), so a missing or stale entry
    silently falls back to nearest sampling.  Every PNG shipped in the app is
    skinned art drawn scaled/rotated, so it wants linear sampling: add any
    asset that is missing, drop asset/ entries whose file is gone (the file
    used to carry stale `asset/ui/pur/...` and Windows-style `ui\\dark\\...`
    keys) and write the paths with forward slashes.
    """
    manifest = app_dst / "images.json"
    try:
        meta = json.loads(manifest.read_text(encoding="utf-8")) if manifest.exists() else {}
    except Exception as exc:
        print(f"[build-vpk] images.json unreadable ({exc}); rebuilding it")
        meta = {}
    out, dropped = {}, []
    for key, val in meta.items():
        name = key.replace("\\", "/")
        if name.startswith("asset/") and not (app_dst / name).exists():
            dropped.append(key)
            continue
        out[name] = val
    added = []
    for p in sorted((app_dst / "asset").rglob("*.png")):
        name = p.relative_to(app_dst).as_posix()
        if name not in out:
            out[name] = {"linear": True}
            added.append(name)
    manifest.write_text(json.dumps(out, ensure_ascii=False, indent=2) + "\n",
                        encoding="utf-8")
    print(f"[build-vpk] images.json: {len(out)} entries "
          f"(+{len(added)} added, -{len(dropped)} stale)")
