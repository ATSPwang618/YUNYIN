/* 纯函数工具：不依赖任何响应式状态，任何模块都能安全引用。 */

export const slug = (value: string): string => {
  return value
    .trim()
    .toLowerCase()
    /* 保留中英文/数字/日文假名，只把空格和标点转成 "-"，避免中文全部丢掉 */
    .replace(/[^a-z0-9\u3040-\u30ff\u3400-\u9fff]+/g, "-")
    .replace(/^-+|-+$/g, "");
};

/*
 * 专辑 ID = artist + album。
 * 只 hash 专辑名时，Avril 这种 album 标签写成歌名的文件会污染专辑页。
 */
export const makeAlbumId = (album: string, artist = ""): string => {
  const albumSlug = slug(album) || "unknown-album";
  const artistSlug = slug(artist);

  return artistSlug ? `${artistSlug}-${albumSlug}` : albumSlug;
};

/*
 * 曲目 ID = artist + album + title + durationMs。
 * 不使用文件名、inode、数组下标，重命名/重扫不会丢掉收藏。
 */
export const makeTrackId = (input: {
  artist: string;
  album: string;
  title: string;
  durationMs: number;
}): string => {
  const duration = Number.isFinite(input.durationMs)
    ? Math.max(1, Math.round(input.durationMs))
    : 1;

  return [
    "track",
    slug(input.artist) || "unknown",
    slug(input.album) || "unknown",
    slug(input.title) || "unknown",
    String(duration),
  ].join("-");
};

export const DEFAULT_COVER_CLS =
  "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-slate-300 to-slate-500 border-slate-300";

/* 截断工具：超长文本用省略号，避免溢出/互相叠字。 */
export function clip(s: string | undefined, n: number): string {
  const t = s || "";
  return t.length <= n ? t : t.slice(0, Math.max(1, n - 1)) + "…";
}

/*
 * 按**显示宽度**截断：CJK / 全角算 2 个单位，其它算 1。
 *
 * 为什么不能按字符数截：列表行是固定宽度（右栏 260、播放器 196），
 * 12px 字下 1 个单位 ≈ 6px。一串中文按"20 个字符"截出来有 240px 宽，
 * 直接顶出屏幕（真机截图里 发现页 的歌单名就是这样）。
 */
export function clipW(s: string | undefined, units: number): string {
  const t = s || "";
  let w = 0;
  let i = 0;
  for (; i < t.length; i++) {
    const c = t.charCodeAt(i);
    const wide =
      (c >= 0x1100 && c <= 0x115f) ||
      (c >= 0x2e80 && c <= 0xa4cf) ||
      (c >= 0xac00 && c <= 0xd7a3) ||
      (c >= 0xf900 && c <= 0xfaff) ||
      (c >= 0xfe30 && c <= 0xfe6f) ||
      (c >= 0xff00 && c <= 0xff60) ||
      (c >= 0xffe0 && c <= 0xffe6);
    const cost = wide ? 2 : 1;
    if (w + cost > units) break;
    w += cost;
  }
  return i >= t.length ? t : t.slice(0, Math.max(1, i - 1)) + "…";
}

export function formatMs(ms: number): string {
  const t = Math.max(0, Math.floor((ms || 0) / 1000));
  const m = Math.floor(t / 60);
  const s = t % 60;
  return m + ":" + (s < 10 ? "0" : "") + s;
}
