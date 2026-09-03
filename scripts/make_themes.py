#!/usr/bin/env python3
# Generates the 4-theme skin assets + updates app/images.json mapping.
import os, shutil, json, glob
from PIL import Image

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
UI = os.path.join(ROOT, "asset", "ui")
LIGHT = os.path.join(UI, "light")
DARK = os.path.join(UI, "dark")
PURE = os.path.join(UI, "pure")
ANIME = os.path.join(UI, "anime")

# 1) rename pur -> pure (idempotent). Keep the existing purple assets.
PUR_SRC = os.path.join(UI, "pur")
if os.path.isdir(PUR_SRC):
    shutil.rmtree(PURE, ignore_errors=True)
    shutil.copytree(PUR_SRC, PURE)
print("pure assets from pur:", len([f for f in os.listdir(PURE) if f.endswith(".png")]))

def gradient(path, top, bottom):
    w, h = 256, 128
    im = Image.new("RGB", (w, h))
    px = im.load()
    for y in range(h):
        t = y / (h - 1)
        c = tuple(int(top[i] + (bottom[i] - top[i]) * t) for i in range(3))
        for x in range(w):
            px[x, y] = c
    im.save(path)

# 2) light -> genuine light background (soft indigo-tinted, matches dark text)
gradient(os.path.join(LIGHT, "screen_bg.png"), (228, 232, 248), (250, 251, 255))

# recolor helper for surfaces
def recolor(path, tint):
    im = Image.open(path).convert("RGBA")
    px = im.load()
    w, h = im.size
    for y in range(h):
        for x in range(w):
            r, g, b, a = px[x, y]
            if a == 0:
                continue
            L = (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0
            nr = int(min(255, round(L * 255 * tint[0])))
            ng = int(min(255, round(L * 255 * tint[1])))
            nb = int(min(255, round(L * 255 * tint[2])))
            px[x, y] = (nr, ng, nb, a)
    im.save(path)

# 4) DARK theme -> true dark surfaces (dark slate panels/cards/nav/rows) for "Dark=浅色文字".
def dark_surface(path, base, edge):
    w, h = 256, 128
    im = Image.new("RGB", (w, h))
    px = im.load()
    for y in range(h):
        t = y / (h - 1)
        c = tuple(int(base[i] + (edge[i] - base[i]) * t) for i in range(3))
        for x in range(w):
            px[x, y] = c
    im.save(path)

DARK_TINT = {
    "panel.png": (30, 36, 48), "app_bg.png": (24, 30, 42),
    "lyrics_panel.png": (30, 36, 48), "setting_bg.png": (28, 34, 46),
    "about_bg.png": (28, 34, 46),
}
for fn, (r, g, b) in DARK_TINT.items():
    p = os.path.join(DARK, fn)
    if os.path.exists(p):
        dark_surface(p, (r, g, b), (min(r + 14, 255), min(g + 16, 255), min(b + 18, 255)))

# Cards / nav / rows: dark with slightly lighter edge for depth
for fn, base, edge in [
    ("card.png", (40, 46, 60), (52, 60, 74)),
    ("card_focus.png", (52, 60, 74), (64, 72, 86)),
    ("nav.png", (44, 50, 64), (56, 64, 78)),
    ("nav_active.png", (58, 62, 74), (70, 74, 86)),
    ("nav_focus.png", (64, 60, 60), (76, 72, 72)),
    ("row.png", (40, 46, 60), (50, 58, 72)),
    ("row_focus.png", (56, 52, 56), (68, 64, 68)),
    ("cover_glow.png", (30, 36, 48), (44, 50, 62)),
]:
    p = os.path.join(DARK, fn)
    if os.path.exists(p):
        dark_surface(p, base, edge)

# 3) anime skin from light surfaces, recolored warm/coral; icons kept.
os.makedirs(ANIME, exist_ok=True)
for fn in sorted(os.listdir(LIGHT)):
    if not fn.endswith(".png"):
        continue
    src = os.path.join(LIGHT, fn)
    dst = os.path.join(ANIME, fn)
    shutil.copy2(src, dst)

# Anime surfaces -> pink/coral tint; bg -> vibrant anime gradient
TINT_FILES = {"app_bg.png","panel.png","lyrics_panel.png","card.png","card_focus.png",
              "nav.png","nav_active.png","nav_focus.png","row.png","row_focus.png",
              "cover_default.png","cover_glow.png","setting_bg.png","about_bg.png"}
for fn in TINT_FILES:
    p = os.path.join(ANIME, fn)
    if os.path.exists(p):
        recolor(p, (1.05, 0.70, 0.95))          # warm pink/violet
gradient(os.path.join(ANIME, "screen_bg.png"), (56, 22, 84), (232, 84, 118))  # violet -> coral

print("anime assets:", len([f for f in os.listdir(ANIME) if f.endswith(".png")]))

# 4) refresh app/images.json: include every asset/ui/**/*.png as linear
ij = os.path.join(ROOT, "app", "images.json")
d = json.load(open(ij, encoding="utf-8"))
# drop stale pur/ entries (replaced by pure/)
for k in [x for x in list(d) if x.startswith("asset/ui/pur/")]:
    del d[k]
added = 0
for rel in sorted(glob.glob("asset/ui/*/*.png", recursive=True)):
    key = rel.replace("\\", "/")
    if key not in d:
        d[key] = {"linear": True}
        added += 1
# ensure linear for the recolorable core surfaces
json.dump(d, open(ij, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
print("images.json: total", len(d), "added", added)
