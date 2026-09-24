import {
  Grid,
  FocusScope,
  Image,
  Text,
  View,
  type NodeMirror,
} from "@pocketjs/framework/components";

import { animate, jump } from "@pocketjs/framework/animation";
import { after } from "@pocketjs/framework/clock";
import { registerTexture } from "@pocketjs/framework";
import { onButtonPress, onFrame } from "@pocketjs/framework/lifecycle";
import { BTN, focusNode } from "@pocketjs/framework/input";
import {
  openFontArchive,
  type TextResource,
} from "@pocketjs/framework/fonts";

import {
  createSignal,
  createEffect,
  createMemo,
  onMount,
  onCleanup,
  untrack,
  For,
  Show,
} from "solid-js";

/* 主题配色配置文件：改 app/colors.json 即可调整各主题文字颜色，重新打包生效。 */
import THEME_COLORS from "./colors.json";
import ThemeSeed from "./theme-seed";

/* =========================================================
 * NATIVE MEDIA BRIDGE (globalThis.vitaMedia)
 *
 * native/media.rs 暴露：list / roots / play / pause / resume /
 * stop / state / cover / tags。state() 返回
 * { playing, paused, path, pos, dur, rate, dec }，pos/dur 为毫秒。
 * ======================================================= */

type VitaMedia = {
  list?: (path: string) => string;
  roots?: () => string;
  play?: (path: string) => void;
  pause?: () => void;
  resume?: (path?: string) => void;
  stop?: () => void;
  state?: () => string;
  cover?: (path: string) => number;
  tags?: (path: string) => string;
  /* 在线曲目清单（卡里 ux0:/data/yunyin/netplay.url 描述的歌），
   * 返回 [{"url":..,"title":..,"referer":..}]；没有这个文件时是 "[]"。 */
  netplay?: () => string;
  setPsLock?: (on: boolean) => number;
  logEnabled?: () => number;
  store_get?: (key: string) => string;
  store_set?: (key: string, value: string) => void;
};

const media = (): VitaMedia | undefined =>
  (globalThis as unknown as { vitaMedia?: VitaMedia }).vitaMedia;

/* 写 ux0:data/yunyin.log 的原生日志入口（与 scanLibrary 的 log 共用）。 */
const logMsg = (m: string): void => {
  try {
    (media() as unknown as { logMsg?: (s: string) => void })?.logMsg?.(m);
  } catch {
    /* ignore */
  }
};

/* 只扫描 ux0:/data/music。系统自带的 ux0:/music 被 Vita 的 SceIo 隐藏，
 * homebrew 打不开；而 ux0:/data 是普通可访问目录，所以把歌曲放到
 * ux0:/data/music（如 E:\data\music）即可被扫描到。不再扫整张卡。 */
const LIB_MOUNTS = ["ux0:/data/yunyin/music"];

/* 音频扩展名：mp3/ogg/wav + 原生支持的 flac/opus/ogg(x) + m4a（里面是 AAC）。 */
const AUDIO_RE = /\.(mp3|ogg|wav|flac|opus|oga|m4a)$/i;

type FsEntry = { name: string; path: string; dir: boolean };

function collectAudio(
  path: string,
  depth: number,
  cap: number,
  out: FsEntry[],
): void {
  if (depth > 5 || out.length >= cap) return;
  const api = media();
  if (!api || !api.list) return;
  let items: FsEntry[] = [];
  try {
    items = JSON.parse(api.list(path) || "[]") as FsEntry[];
  } catch {
    items = [];
  }
  logMsg(
    `SCAN ${path} -> ${items.length} item(s): ` +
      items
        .slice(0, 24)
        .map((e) => `${e.dir ? "[D]" : "[F]"}${e.name}`)
        .join(", "),
  );
  for (const e of items) {
    if (out.length >= cap) return;
    if (e.dir) collectAudio(e.path, depth + 1, cap, out);
    else if (AUDIO_RE.test(e.name)) out.push(e);
    else if (!e.name.includes(".")) collectAudio(e.path, depth + 1, cap, out);
  }
}

type Screen = "home" | "list" | "album" | "loved" | "setting";
type FocusZone = "nav" | "content";
type PlaybackMode = "sequence" | "repeat-one";

/* =========================================================
 * TRACK / ALBUM DATA MODEL
 *
 * 设计原则：一切以 id 为主键。
 *
 * - Track.id 是稳定标识，不随扫描顺序变化。
 * - Album.trackIds 存 Track.id 数组，而不是数组下标。
 * - 当前播放 / 当前选中的 Album，都用 id 记录，
 *   而不是"它现在排在第几个"。
 *
 * 这样无论底层 tracks 数组是 mock 数据、
 * 还是以后真实扫描出来的、顺序会变的数据，
 * 上层状态都不会因为数组重新排序/增删而错位。
 * ======================================================= */

interface Track {
  /* 稳定主键：artist + album + title + durationMs，不随扫描顺序/文件名变化 */
  id: string;

  title: string;
  artist: string;
  album: string;

  /*
   * pak 内 WAV key（保留字段，预留给 audio.pcm 资源）。
   * 空字符串表示还没有可播放资源，走墙钟模拟进度。
   */
  wav: string;

  /* 沙盒/本地路径，扫描接入后使用；有值则走 Vita 原生解码。 */
  audioPath: string;

  /* 本地路径引用；有 audioPath 时优先走 Vita 原生播放 */
  audioRef: string;

  /* Cover Cache 的唯一 key。将来对应抽出的封面文件，不对应 CSS */
  coverId: string;

  /* 无封面时的占位渐变。只做兜底，UI 优先认 coverId */
  coverCls: string;

  /* 内嵌封面纹理 key（registerTexture 后的 key），有则优先显示真封面 */
  cover?: string;

  /* 歌曲真实时长，单位 milliseconds */
  durationMs: number;

  /* Track -> Album，artist + album 生成，避免同名专辑合并 */
  albumId: string;

  /* 原始 LRC / USLT 文本。没有则歌词页只显示歌名 */
  lyrics?: string;

  /*
   * 在线曲目标记。true 表示 audioPath 是 URL（不是卡里的文件）：
   * 封面/标签都不去读它（读网络地址没意义还慢），播放时原生侧认 http(s) 前缀。
   * 这类歌单独归到「在线歌曲」专辑，绝不占用本地歌曲的位置。
   */
  online?: boolean;
}

interface Album {
  /* Album 唯一 ID */
  id: string;

  title: string;
  artist: string;

  /*
   * Album -> Track。
   *
   * 存 Track.id，而不是数组下标。
   * 曲库重新扫描、排序变化时依然正确。
   */
  trackIds: string[];

  coverId: string;
  coverCls: string;
}

interface LyricLine {
  /* milliseconds */
  time: number;
  text: string;
}

/* =========================================================
 * HELPERS — 纯函数，不依赖任何响应式状态
 * ======================================================= */

const slug = (value: string): string => {
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
const makeAlbumId = (album: string, artist = ""): string => {
  const albumSlug = slug(album) || "unknown-album";
  const artistSlug = slug(artist);

  return artistSlug ? `${artistSlug}-${albumSlug}` : albumSlug;
};

/*
 * 曲目 ID = artist + album + title + durationMs。
 * 不使用文件名、inode、数组下标，重命名/重扫不会丢掉收藏。
 */
const makeTrackId = (input: {
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

const DEFAULT_COVER_CLS =
  "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-slate-300 to-slate-500 border-slate-300";

/* =========================================================
 * UI SKIN PACK —— 主题皮肤档
 * 每套主题的所有纹理文件名都按字面量列出（build 会全部烘焙），
 * 运行时用 uiTheme() 选择当前主题的皮肤；文字/封面/状态仍动态。
 * 注意：字体是构建时烘焙的（每套主题可单独 build + 指定字体），
 * 无法在运行时切换，故皮肤档只切纹理/背景/配色。
 * ======================================================= */
const UI_THEMES = ["light", "dark", "pure", "anime"] as const;
type UiSkin = (typeof UI_THEMES)[number];
type UiSkinKey =
  | "screenBg" | "appBg" | "panel" | "lyricsPanel" | "card" | "cardFocus"
  | "nav" | "navActive" | "navFocus"
  | "navHome" | "navList" | "navAlbum" | "navLoved" | "navSet"
  | "row" | "rowFocus" | "coverDefault" | "coverGlow" | "settingBg" | "aboutBg"
  | "playN" | "playF" | "pauseN" | "pauseP"
  | "prevN" | "prevF" | "nextN" | "nextF"
  | "rep1N" | "rep1F" | "shufN" | "shufF"
  | "honN" | "honF" | "hoffN" | "hoffF";
type SkinMap = Record<UiSkinKey, string>;

const SKINS: Record<UiSkin, SkinMap> = {
  light: {
    screenBg: "asset/ui/light/screen_bg.png",
    appBg: "asset/ui/light/app_bg.png",
    panel: "asset/ui/light/panel.png",
    lyricsPanel: "asset/ui/light/lyrics_panel.png",
    card: "asset/ui/light/card.png",
    cardFocus: "asset/ui/light/card_focus.png",
    nav: "asset/ui/light/nav.png",
    navActive: "asset/ui/light/nav_active.png",
    navFocus: "asset/ui/light/nav_focus.png",
    navHome: "asset/ui/light/icon_nav_home.png",
    navList: "asset/ui/light/icon_nav_list.png",
    navAlbum: "asset/ui/light/icon_nav_album.png",
    navLoved: "asset/ui/light/icon_nav_loved.png",
    navSet: "asset/ui/light/icon_nav_set.png",
    row: "asset/ui/light/row.png",
    rowFocus: "asset/ui/light/row_focus.png",
    coverDefault: "asset/ui/light/cover_default.png",
    coverGlow: "asset/ui/light/cover_glow.png",
    settingBg: "asset/ui/light/setting_bg.png",
    aboutBg: "asset/ui/light/about_bg.png",
    playN: "asset/ui/light/icon_playN.png",
    playF: "asset/ui/light/icon_playF.png",
    pauseN: "asset/ui/light/icon_pauseN.png",
    pauseP: "asset/ui/light/icon_pauseP.png",
    prevN: "asset/ui/light/icon_prevN.png",
    prevF: "asset/ui/light/icon_prevF.png",
    nextN: "asset/ui/light/icon_nextN.png",
    nextF: "asset/ui/light/icon_nextF.png",
    rep1N: "asset/ui/light/icon_rep1N.png",
    rep1F: "asset/ui/light/icon_rep1F.png",
    shufN: "asset/ui/light/icon_shufN.png",
    shufF: "asset/ui/light/icon_shufF.png",
    honN: "asset/ui/light/icon_honN.png",
    honF: "asset/ui/light/icon_honF.png",
    hoffN: "asset/ui/light/icon_hoffN.png",
    hoffF: "asset/ui/light/icon_hoffF.png",
  },
  dark: {
    screenBg: "asset/ui/dark/screen_bg.png",
    appBg: "asset/ui/dark/app_bg.png",
    panel: "asset/ui/dark/panel.png",
    lyricsPanel: "asset/ui/dark/lyrics_panel.png",
    card: "asset/ui/dark/card.png",
    cardFocus: "asset/ui/dark/card_focus.png",
    nav: "asset/ui/dark/nav.png",
    navActive: "asset/ui/dark/nav_active.png",
    navFocus: "asset/ui/dark/nav_focus.png",
    navHome: "asset/ui/dark/icon_nav_home.png",
    navList: "asset/ui/dark/icon_nav_list.png",
    navAlbum: "asset/ui/dark/icon_nav_album.png",
    navLoved: "asset/ui/dark/icon_nav_loved.png",
    navSet: "asset/ui/dark/icon_nav_set.png",
    row: "asset/ui/dark/row.png",
    rowFocus: "asset/ui/dark/row_focus.png",
    coverDefault: "asset/ui/dark/cover_default.png",
    coverGlow: "asset/ui/dark/cover_glow.png",
    settingBg: "asset/ui/dark/setting_bg.png",
    aboutBg: "asset/ui/dark/about_bg.png",
    playN: "asset/ui/dark/icon_playN.png",
    playF: "asset/ui/dark/icon_playF.png",
    pauseN: "asset/ui/dark/icon_pauseN.png",
    pauseP: "asset/ui/dark/icon_pauseP.png",
    prevN: "asset/ui/dark/icon_prevN.png",
    prevF: "asset/ui/dark/icon_prevF.png",
    nextN: "asset/ui/dark/icon_nextN.png",
    nextF: "asset/ui/dark/icon_nextF.png",
    rep1N: "asset/ui/dark/icon_rep1N.png",
    rep1F: "asset/ui/dark/icon_rep1F.png",
    shufN: "asset/ui/dark/icon_shufN.png",
    shufF: "asset/ui/dark/icon_shufF.png",
    honN: "asset/ui/dark/icon_honN.png",
    honF: "asset/ui/dark/icon_honF.png",
    hoffN: "asset/ui/dark/icon_hoffN.png",
    hoffF: "asset/ui/dark/icon_hoffF.png",
  },
  pure: {
    screenBg: "asset/ui/pure/screen_bg.png",
    appBg: "asset/ui/pure/app_bg.png",
    panel: "asset/ui/pure/panel.png",
    lyricsPanel: "asset/ui/pure/lyrics_panel.png",
    card: "asset/ui/pure/card.png",
    cardFocus: "asset/ui/pure/card_focus.png",
    nav: "asset/ui/pure/nav.png",
    navActive: "asset/ui/pure/nav_active.png",
    navFocus: "asset/ui/pure/nav_focus.png",
    navHome: "asset/ui/pure/icon_nav_home.png",
    navList: "asset/ui/pure/icon_nav_list.png",
    navAlbum: "asset/ui/pure/icon_nav_album.png",
    navLoved: "asset/ui/pure/icon_nav_loved.png",
    navSet: "asset/ui/pure/icon_nav_set.png",
    row: "asset/ui/pure/row.png",
    rowFocus: "asset/ui/pure/row_focus.png",
    coverDefault: "asset/ui/pure/cover_default.png",
    coverGlow: "asset/ui/pure/cover_glow.png",
    settingBg: "asset/ui/pure/setting_bg.png",
    aboutBg: "asset/ui/pure/about_bg.png",
    playN: "asset/ui/pure/icon_playN.png",
    playF: "asset/ui/pure/icon_playF.png",
    pauseN: "asset/ui/pure/icon_pauseN.png",
    pauseP: "asset/ui/pure/icon_pauseP.png",
    prevN: "asset/ui/pure/icon_prevN.png",
    prevF: "asset/ui/pure/icon_prevF.png",
    nextN: "asset/ui/pure/icon_nextN.png",
    nextF: "asset/ui/pure/icon_nextF.png",
    rep1N: "asset/ui/pure/icon_rep1N.png",
    rep1F: "asset/ui/pure/icon_rep1F.png",
    shufN: "asset/ui/pure/icon_shufN.png",
    shufF: "asset/ui/pure/icon_shufF.png",
    honN: "asset/ui/pure/icon_honN.png",
    honF: "asset/ui/pure/icon_honF.png",
    hoffN: "asset/ui/pure/icon_hoffN.png",
    hoffF: "asset/ui/pure/icon_hoffF.png",
  },
  anime: {
    screenBg: "asset/ui/anime/screen_bg.png",
    appBg: "asset/ui/anime/app_bg.png",
    panel: "asset/ui/anime/panel.png",
    lyricsPanel: "asset/ui/anime/lyrics_panel.png",
    card: "asset/ui/anime/card.png",
    cardFocus: "asset/ui/anime/card_focus.png",
    nav: "asset/ui/anime/nav.png",
    navActive: "asset/ui/anime/nav_active.png",
    navFocus: "asset/ui/anime/nav_focus.png",
    navHome: "asset/ui/anime/icon_nav_home.png",
    navList: "asset/ui/anime/icon_nav_list.png",
    navAlbum: "asset/ui/anime/icon_nav_album.png",
    navLoved: "asset/ui/anime/icon_nav_loved.png",
    navSet: "asset/ui/anime/icon_nav_set.png",
    row: "asset/ui/anime/row.png",
    rowFocus: "asset/ui/anime/row_focus.png",
    coverDefault: "asset/ui/anime/cover_default.png",
    coverGlow: "asset/ui/anime/cover_glow.png",
    settingBg: "asset/ui/anime/setting_bg.png",
    aboutBg: "asset/ui/anime/about_bg.png",
    playN: "asset/ui/anime/icon_playN.png",
    playF: "asset/ui/anime/icon_playF.png",
    pauseN: "asset/ui/anime/icon_pauseN.png",
    pauseP: "asset/ui/anime/icon_pauseP.png",
    prevN: "asset/ui/anime/icon_prevN.png",
    prevF: "asset/ui/anime/icon_prevF.png",
    nextN: "asset/ui/anime/icon_nextN.png",
    nextF: "asset/ui/anime/icon_nextF.png",
    rep1N: "asset/ui/anime/icon_rep1N.png",
    rep1F: "asset/ui/anime/icon_rep1F.png",
    shufN: "asset/ui/anime/icon_shufN.png",
    shufF: "asset/ui/anime/icon_shufF.png",
    honN: "asset/ui/anime/icon_honN.png",
    honF: "asset/ui/anime/icon_honF.png",
    hoffN: "asset/ui/anime/icon_hoffN.png",
    hoffF: "asset/ui/anime/icon_hoffF.png",
  },
};

/* 启动默认皮肤（设置页里仍可切换，切换顺序 light → pure → anime → dark）。 */
const [uiTheme, setUiTheme] = createSignal<UiSkin>("dark");
const useSkin = (): SkinMap => SKINS[uiTheme()];

/* 主题化文字色：dark 皮肤上文字要浅色，light 上深色。 */
const isDarkish = () => uiTheme() !== "light";
/* 只有 dark 主题把面板/卡片也做成深色（其余主题面板仍是浅色），故面板文字以此判定。 */
const isDarkPanel = () => uiTheme() === "dark";

/* 每主题文字调色板（角色 → Tailwind 颜色类）。集中定义，UI 只按角色取用。 */
type Palette = {
  title: string; body: string; mute: string; faint: string;
  accent: string; focus: string; brand: string;
};
/* 背景文字配色 —— 来自 asset/ui/colors.json（每主题一套）。 */
const PALETTES = THEME_COLORS.bg as Record<UiSkin, Palette>;

const tTitle = () => PALETTES[uiTheme()].title;
const tBody = () => PALETTES[uiTheme()].body;
const tMute = () => PALETTES[uiTheme()].mute;
const tFaint = () => PALETTES[uiTheme()].faint;
const tAccent = () => PALETTES[uiTheme()].accent;
const tFocus = () => PALETTES[uiTheme()].focus;
const tBrand = () => PALETTES[uiTheme()].brand;

/* 面板/卡片文字——每套主题一套**独特**颜色（来自 asset/ui/colors.json）。
 * 值是完整类字面量，保证可被烘焙；UI 直接 pTxt(key) 取用。 */
const PANEL_TXT = THEME_COLORS.ui as Record<UiSkin, Record<string, string>>;
const pTxt = (key: string) => PANEL_TXT[uiTheme()][key] ?? "";

/* 软件版本：About 页展示，和 param.sfo APP_VER 00.63 对齐。 */
const APP_VERSION = "0.88";
/* param.sfo 里的 APP_VER（VitaShell 里显示的那串），和 APP_VERSION 一起改。 */
const APP_VER_SFO = "00.88";
const POCKETJS_VERSION = "0.12.0";

type CjkMode = "baked" | "stream";
const [cjkMode, setCjkMode] = createSignal<CjkMode>(
  (() => {
    try {
      return media()?.store_get?.("cjkMode") === "stream" ? "stream" : "baked";
    } catch {
      return "baked";
    }
  })(),
);
const [cjkStatus, setCjkStatus] = createSignal<"off" | "opening" | "ready" | "error">("off");
const [cjkEpoch, setCjkEpoch] = createSignal(0);
let cjkFont: ReturnType<typeof openFontArchive> | undefined;
/* PS 键锁状态（显示在 About 页）：LOCKED / UNLOCKED / FAIL 0x… */
const [psLockInfo, setPsLockInfo] = createSignal("PS KEY  UNLOCKED");

/* STREAM 模式的诊断日志（只有卡里有 ux0:/data/yunyin/debug 时才真的写）。
 * host 里：resident = 常驻字形数，inked = 其中真的有墨的个数
 * （inked 一直是 0 就说明字库读出来的点阵是空的），pending = 还在等的请求，
 * rejected = 没空位被拒，unsupported = 字库里确实没有这个字；
 * js 里：loaded = JS 侧累计提交成功的字数，requests = 还在飞的请求数。 */
const logCjkStats = (): void => {
  try {
    const g = globalThis as unknown as {
      ui?: { fontStreamStats?: () => string; fontStreamRequests?: () => string };
    };
    const host = g.ui?.fontStreamStats?.() ?? "n/a";
    let want = "?";
    try {
      const reqs = JSON.parse(g.ui?.fontStreamRequests?.() ?? "[]") as number[][];
      want =
        String(reqs.length) +
        ":" +
        reqs
          .slice(0, 6)
          .map((r) => "U+" + Number(r[2]).toString(16).toUpperCase())
          .join(",");
    } catch {
      /* ignore */
    }
    const st = cjkFont?.status() as
      | { state?: string; requests?: number; loaded?: number; error?: string }
      | undefined;
    logMsg(
      "cjk: mode=" + cjkMode() + " status=" + cjkStatus() + " host=" + host +
        " js={state:" + (st?.state ?? "-") +
        ",req:" + String(st?.requests ?? "-") +
        ",loaded:" + String(st?.loaded ?? "-") +
        ",err:" + (st?.error ?? "") + "} want=" + want,
    );
  } catch {
    /* ignore */
  }
};

let cjkDbgFrames = 0;

/* 日志没开（正式版默认）时，连诊断数据的采集都省掉。 */
const logEnabled = (): boolean => {
  try {
    return !!media()?.logEnabled?.();
  } catch {
    return false;
  }
};

const streamHostReady = (): boolean => {
  try {
    const g = globalThis as unknown as {
      ui?: { fontStreamConfigure?: unknown; fontStreamBatch?: unknown };
      offload?: { local?: unknown };
    };
    return !!(g.ui?.fontStreamConfigure && g.ui?.fontStreamBatch && g.offload?.local);
  } catch {
    return false;
  }
};

const slotFromClass = (cls: string): number => {
  const bold = /\bfont-bold\b/.test(cls);
  if (/\btext-sm\b/.test(cls)) return bold ? 8 : 1;
  return bold ? 7 : 0;
};

/* 设置页第一张卡片（CJK）上显示的短标签。 */
const cjkCardValue = (): string => {
  if (cjkMode() !== "stream") return "BAKED";
  if (cjkStatus() === "ready") return "STREAM";
  if (cjkStatus() === "opening") return "LOADING";
  if (cjkStatus() === "error") return "ERROR";
  return "STREAM";
};

const applyCjkMode = (next: CjkMode): void => {
  setCjkMode(next);
  try {
    media()?.store_set?.("cjkMode", next);
  } catch {
    /* ignore */
  }
  if (next !== "stream") {
    try {
      cjkFont?.dispose();
    } catch {
      /* ignore */
    }
    cjkFont = undefined;
    setCjkStatus("off");
    setCjkEpoch((n) => n + 1);
    return;
  }
  if (cjkFont) return;
  if (!streamHostReady()) {
    setCjkStatus("error");
    setCjkEpoch((n) => n + 1);
    return;
  }
  try {
    setCjkStatus("opening");
    cjkFont = openFontArchive({
      path: "fonts/cjk.pjfa",
      slots: [0, 7, 8],
      provider: "local",
      capacity: 384,
      maxBytes: 2 * 1024 * 1024,
      onChange: () => {
        const st = cjkFont?.status().state;
        if (st === "ready") setCjkStatus("ready");
        else if (st === "error") setCjkStatus("error");
        else if (st === "warming" || st === "opening") setCjkStatus("opening");
        logCjkStats();
      },
    });
    setCjkEpoch((n) => n + 1);
  } catch {
    cjkFont = undefined;
    setCjkStatus("error");
    setCjkEpoch((n) => n + 1);
  }
};

/* 流式文字：把一段文字交给 cjk.pjfa 按需补字形。
 *
 * - **只有「文字 + 字号槽」变了才重新申请字形资源**：光标移动 / 选中高亮只会
 *   换颜色类名（槽位、文字都没变），这时复用手里的资源，不再走一遍请求→提交；
 * - 字形还在取的时候，先按同样的布局画一段**透明**文字占位（占住行高，
 *   不闪 口 也不跳版），等齐了整体换上去；
 * - 真的取不到（字库没有这个字 / 出错）才退回**可见**的烘焙文字 —— 那时看到
 *   的 口 就是字库确实缺字，不是没加载完。 */
function StreamText(props: { class: string; text: string }) {
  const [res, setRes] = createSignal<TextResource | undefined>();
  let key = "";
  let current: TextResource | undefined;

  const release = () => {
    current?.dispose();
    current = undefined;
    setRes(undefined);
  };

  createEffect(() => {
    const mode = cjkMode();
    void cjkEpoch();
    const text = props.text;
    const slot = slotFromClass(props.class);
    const usable =
      mode === "stream" && !!cjkFont && (slot === 0 || slot === 7 || slot === 8);
    const next = usable ? slot + "\u0000" + text : "";
    /* 颜色 / 高亮变化：key 没变就直接复用，绝不重新申请字形。 */
    if (next === key && current) return;
    key = next;
    release();
    if (!usable || !cjkFont) return;
    try {
      current = cjkFont.prepareText(text, { slot });
      setRes(current);
    } catch {
      current = undefined;
      key = "";
    }
  });

  onCleanup(() => {
    key = "";
    release();
  });

  return (
    <Show
      when={res()}
      fallback={<Text class={props.class}>{props.text}</Text>}
    >
      {(r) => (
        <Text
          class={props.class}
          resource={r()}
          fallback={() => (
            <Text class={props.class} style={{ opacity: 0 }}>{props.text}</Text>
          )}
          errorFallback={() => <Text class={props.class}>{props.text}</Text>}
        />
      )}
    </Show>
  );
}

const getCoverClass = (coverId: string, fallbackCls?: string): string => {
  if (fallbackCls && fallbackCls.trim()) {
    return fallbackCls;
  }

  void coverId;
  return DEFAULT_COVER_CLS;
};

/*
 * 从 Track[] 建立 Album[]。
 *
 * 输入是任意一批 tracks（mock 或真实扫描结果都行），
 * 输出的 Album.trackIds 存的是 id，
 * 不依赖输入数组的顺序或长度。
 */
const buildAlbums = (tracks: Track[]): Album[] => {
  const groups: Record<string, Album> = {};

  for (const song of tracks) {
    if (!song) {
      continue;
    }

    const albumId = song.albumId || makeAlbumId(song.album, song.artist);

    if (!groups[albumId]) {
      groups[albumId] = {
        id: albumId,
        title: song.album,
        artist: song.artist,
        trackIds: [],
        coverId: song.coverId,
        coverCls: getCoverClass(song.coverId, song.coverCls),
      };
    }

    groups[albumId].trackIds.push(song.id);
  }

  return Object.keys(groups).map((key) => groups[key]);
};

/* Track.id -> Track 的查找表，供 O(1) 按 id 取歌曲 */
const buildTrackById = (tracks: Track[]): Record<string, Track> => {
  const map: Record<string, Track> = {};

  for (const song of tracks) {
    map[song.id] = song;
  }

  return map;
};

/* Album.id -> Album 的查找表 */
const buildAlbumById = (albums: Album[]): Record<string, Album> => {
  const map: Record<string, Album> = {};

  for (const item of albums) {
    map[item.id] = item;
  }

  return map;
};

/*
 * 清洗一批 id（favorites / 持久化数据都能用）。
 *
 * 曲库重新扫描后，之前存的某些 id 可能已经不存在了，
 * 用这个函数过滤掉找不到对应 Track 的野指针 id，
 * 避免渲染出 undefined。
 */
const sanitizeIds = (
  ids: string[],
  byId: Record<string, unknown>,
): string[] => {
  return ids.filter((id) => byId[id] !== undefined);
};

/*
 * 获取安全的 Track duration，防止 0 / 负数 / NaN / Infinity
 * 导致进度计算异常。
 */
const getTrackDuration = (song: Track): number => {
  const duration = song.durationMs;

  if (!Number.isFinite(duration)) {
    return 1;
  }

  return Math.max(1, duration);
};

const parseTimestamp = (raw: string): number | null => {
  const match = raw
    .trim()
    .match(/^(\d{1,3}):(\d{2})(?:\.(\d{1,3}))?$/);

  if (!match) {
    return null;
  }

  const minutes = Number(match[1]);
  const seconds = Number(match[2]);
  const fraction = match[3] ?? "";
  const millis = fraction
    ? Number(fraction.padEnd(3, "0").slice(0, 3))
    : 0;

  if (!Number.isFinite(minutes) || !Number.isFinite(seconds)) {
    return null;
  }

  return minutes * 60000 + seconds * 1000 + millis;
};

/*
 * 把 LRC / 内嵌 lyrics-eng 转成 LyricLine[]。
 * 优先级由数据层保证：同名 .lrc → ID3 USLT / lyrics-eng → 空。
 */
const parseLyrics = (raw: string | undefined, song: Track): LyricLine[] => {
  const fallback: LyricLine[] = [
    { time: 0, text: `${song.title} — ${song.artist}`.trim() },
  ];

  if (!raw || !raw.trim()) {
    return fallback;
  }

  const out: LyricLine[] = [];

  for (const line of raw.split(/\r?\n/)) {
    const stamps = [...line.matchAll(/\[([^\]]+)\]/g)];

    if (stamps.length === 0) {
      continue;
    }

    const text = line.replace(/\[[^\]]+\]/g, "").trim();

    if (!text) {
      continue;
    }

    /* 卡拉 OK 式 LRC 每行会给每个词各打一个时间戳，若全部采用会把整行复制到
     * 每个词的时刻，导致同一句紧挨着反复出现。这里只取该行第一个有效时间戳。 */
    let time: number | null = null;
    for (const stamp of stamps) {
      const t = parseTimestamp(stamp[1] ?? "");
      if (t !== null) {
        time = t;
        break;
      }
    }

    if (time === null) {
      continue;
    }

    out.push({ time, text });
  }

  if (out.length === 0) {
    return fallback;
  }

  out.sort((left, right) => left[0] - right[0]);

  /* 极少见的多行同时间戳：合并成一行 */
  const merged: LyricLine[] = [];
  for (const ln of out) {
    const last = merged[merged.length - 1];
    if (last && last.time === ln.time) {
      last.text = `${last.text} / ${ln.text}`;
    } else {
      merged.push({ time: ln.time, text: ln.text });
    }
  }

  return merged;
};

/*
 * 兜底 Track。
 *
 * 曲库为空时（比如扫描尚未完成、或全部被清空），
 * 用这个占位对象代替，防止播放器崩溃。
 */
const FALLBACK_TRACK: Track = {
  id: "",
  title: "NO TRACK",
  artist: "",
  album: "",
  wav: "",
  audioPath: "",
  audioRef: "",
  coverId: "",
  coverCls: DEFAULT_COVER_CLS,
  durationMs: 1,
  albumId: "",
  lyrics: "",
};


/* 示范曲：从 I Will Be.mp3 的 lyrics-eng 抽出，扫描接入前作为 LRC fixture */
const FIXTURE_LRC_I_WILL_BE = `[00:00.10]歌曲名 I Will Be
[00:00.20]歌手名 Avril Lavigne
[00:00.30]作词：Max Martin+Lukasz "Doctor Luke" Gottwald/Avril Lavigne
[00:00.40]作曲：Max Martin+Lukasz "Doctor Luke" Gottwald/Avril Lavigne
[00:03.84]There's nothing I could say to you
[00:06.88]Nothin' I could ever do to make you see
[00:12.92]What you mean to me
[00:16.56]All the pain the tears I cried
[00:19.70]Still you never said good-bye
[00:22.69]And now I know
[00:25.69]How far you'd go
[00:30.83]I know I let you down
[00:34.02]But it's not like that now
[00:37.41]This time I'll never let you go
[00:44.05]I will be all that you want
[00:50.23]And get myself together
[00:53.07]Cause you keep me from falling apart
[00:56.62]All my life
[00:59.56]I'll be with you forever
[01:03.05]To get you through the day
[01:05.95]And make everything okay
[01:14.23]I thought that I had everything
[01:17.12]I didn't know what life could bring
[01:20.31]But now I see
[01:23.61]Honestly
[01:27.00]You're the one thing I got right
[01:29.99]The only one I let inside
[01:33.08]Now I can breathe
[01:36.08]Cause you're here with me
[01:41.37]And if I let you down
[01:44.56]I'll turn it all around
[01:47.65]Cause I will never let you go
[01:54.34]I will be all that you want
[02:00.62]And get myself together
[02:03.51]Cause you keep me from falling apart
[02:06.91]All my life
[02:10.10]I'll be with you forever
[02:13.29]To get you through the day
[02:16.54]And make everything okay
[02:18.38]Cause without you
[02:19.73]I can't sleep
[02:21.27]I'm not gonna ever ever let you leave
[02:24.42]You're all I got
[02:25.96]You're all I want
[02:27.56]Yeah
[02:31.00]And without you I don't know what I'd do
[02:34.05]I could never ever live a day without you here
[02:38.59]With me
[02:40.18]Do you see
[02:41.93]You're all I need
[02:58.25]And I will be all that you want
[03:04.68]And get myself together
[03:07.58]Cause you keep me from falling apart
[03:11.07]All my life
[03:14.16]I'll be with you forever
[03:17.35]To get you through the day
[03:20.35]And make everything okay
[03:23.49]I will be all that you want
[03:30.13]And get myself together
[03:33.17]Cause you keep me from falling apart
[03:36.61]All my life
[03:39.55]I'll be with you forever
[03:42.90]To get you through the day
[03:45.99]And make everything okay
`;

/* =========================================================
 * MOCK TRACK DATA
 *
 * 这批数据只是"初始种子"。
 *
 * 未来真实扫描接入后，替换方式是：
 *
 *   setTracks(realScannedTracks)
 *
 * 不需要改动下面任何 UI / 交互逻辑。
 * ======================================================= */

/* 曲库为空时的占位数据：只留 4 条（test1..test4）。
 * 它们没有真实音频（audioPath 为空），播放时走时钟模拟，只为让界面不空。 */
const MOCK_TRACKS: Track[] = [
  {
    id: "test-1",
    title: "test1",
    artist: "test",
    album: "test",
    wav: "",
    audioPath: "",
    audioRef: "",
    coverId: "test-1",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-sky-400 to-blue-800 border-sky-300",
    durationMs: 180000,
    albumId: "test",
    lyrics: FIXTURE_LRC_I_WILL_BE,
  },
  {
    id: "test-2",
    title: "test2",
    artist: "test",
    album: "test",
    wav: "",
    audioPath: "",
    audioRef: "",
    coverId: "test-2",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-blue-500 to-blue-700 border-blue-300",
    durationMs: 180000,
    albumId: "test",
  },
  {
    id: "test-3",
    title: "test3",
    artist: "test",
    album: "test",
    wav: "",
    audioPath: "",
    audioRef: "",
    coverId: "test-3",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-amber-400 to-amber-700 border-amber-300",
    durationMs: 180000,
    albumId: "test",
  },
  {
    id: "test-4",
    title: "test4",
    artist: "test",
    album: "test",
    wav: "",
    audioPath: "",
    audioRef: "",
    coverId: "test-4",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-cyan-500 to-cyan-700 border-cyan-300",
    durationMs: 180000,
    albumId: "test",
  },
];

/* =========================================================
 * REAL LIBRARY (Vita native media)
 *
 * scanLibrary() 用 vitaMedia.list 扫挂载点，再用 tags / cover
 * 读出标题/歌手/专辑/内嵌封面，构造真实 Track。找不到时返回 []，
 * 上层会退回 MOCK_TRACKS，保证 UI 不空。
 * ======================================================= */

function buildRealTrack(entry: FsEntry, index: number): Track {
  const api = media();
  let title = "";
  let artist = "Local";
  let album = "Unknown";
  let ly = "";

  if (api && api.tags) {
    try {
      const t = JSON.parse(api.tags(entry.path) || "{}") as {
        title?: string;
        artist?: string;
        album?: string;
        lyrics?: string;
      };
      if (t.title) title = t.title;
      if (t.artist) artist = t.artist;
      if (t.album) album = t.album;
      ly =
        typeof t.lyrics === "string" && t.lyrics.trim()
          ? t.lyrics
          : "";
    } catch {
      /* keep defaults */
    }
  }

  const name = entry.name.replace(AUDIO_RE, "");
  const base = title || name;
  const albumLabel = album && album.trim() ? album : "Singles"; /* 空专辑归到 Singles，按歌手归并 */
  return {
    id: makeTrackId({ artist, album: albumLabel, title: base, durationMs: 0 }),
    title: base,
    artist,
    album: albumLabel,
    wav: "",
    audioPath: entry.path,
    audioRef: entry.path,
    coverId: slug(albumLabel) + "-" + slug(artist),
    coverCls: DEFAULT_COVER_CLS,
    cover: undefined,
    durationMs: 0,
    albumId: makeAlbumId(albumLabel, artist),
    lyrics: ly,
  };
}

function scanLibrary(): Track[] {
  const api = media();
  if (!api || !api.list) return [];
  const log = (m: string) => {
    try {
      (media() as unknown as { logMsg?: (s: string) => void })?.logMsg?.(m);
    } catch {
      /* ignore */
    }
  };
  const found: FsEntry[] = [];
  const seen = new Set<string>();
  const scanMounts = (mounts: string[]) => {
    for (const mount of mounts) {
      const bucket: FsEntry[] = [];
      collectAudio(mount, 0, 240, bucket);
      log(`mount ${mount} -> ${bucket.length} audio files`);
      for (const e of bucket) {
        if (seen.has(e.path)) {
          log(`  DUP-PATH ${e.path}`);
          continue; /* 同一物理文件只扫一次 */
        }
        seen.add(e.path);
        found.push(e);
      }
    }
  };
  scanMounts(LIB_MOUNTS);
  log(`total unique files: ${found.length}`);
  const tracks = found.map((entry, index) => buildRealTrack(entry, index));
  const byTitle = new Map<string, number>();
  for (const t of tracks) byTitle.set(t.title, (byTitle.get(t.title) || 0) + 1);
  for (const [title, n] of byTitle) if (n > 1) log(`  DUP-TITLE "${title}" x${n}`);
  const byAlbum = new Map<string, number>();
  for (const t of tracks) byAlbum.set(t.album, (byAlbum.get(t.album) || 0) + 1);
  log(`album groups: ${byAlbum.size}`);
  for (const [album, n] of byAlbum) log(`  ALBUM "${album}" -> ${n} tracks`);
  const sample = tracks.slice(0, 30);
  for (const t of sample)
    log(
      `  SAMPLE [${t.artist}] "${t.title}" album="${t.album}" lyrics=${t.lyrics ? t.lyrics.length : 0} path=${t.audioPath}`,
    );
  log(`unique tracks: ${tracks.length}`);

  /*
   * 在线曲目接在本地曲库**后面**，单独成一组（专辑「在线歌曲」）。
   *
   * 以前在线歌是"藏在本地第一首底下偷偷加载"的 —— 界面显示第一首本地歌、
   * 按播放却是另一首，这就是分不清的原因。现在它就是一个正常的条目：
   * 选中它、按 ○，才走网络播放；本地歌的位置一个都没被动过。
   */
  const online = scanOnlineTracks();
  if (online.length) {
    log(`online tracks: ${online.length}`);
    for (const t of online) log(`  ONLINE "${t.title}" url=${t.audioPath}`);
  }
  return tracks.concat(online);
}

/* 在线歌统一归到这一组，界面上和本地专辑明显分开 */
const ONLINE_ALBUM = "在线歌曲";
const ONLINE_ARTIST = "在线";

/* 从 URL 里挑出 song id（没有就用序号），只为了让默认名字有点辨识度。 */
function onlineIdHint(url: string): string {
  const m = /[?&]id=(\d+)/.exec(url);
  return m ? m[1] : "";
}

function buildOnlineTrack(
  url: string,
  title: string,
  index: number,
): Track {
  const name =
    (title || "").trim() ||
    `[在线] ${onlineIdHint(url) || String(index + 1)}`;
  const artist = ONLINE_ARTIST;
  const album = ONLINE_ALBUM;
  return {
    id: makeTrackId({ artist, album, title: name, durationMs: 0 }),
    title: name,
    artist,
    album,
    wav: "",
    /* 在线曲目的 audioPath 就是 URL：原生侧按 http(s) 前缀分流，
     * 界面这一层不需要知道"本地 / 在线"的区别。 */
    audioPath: url,
    audioRef: url,
    coverId: slug(album) + "-" + slug(artist),
    coverCls: DEFAULT_COVER_CLS,
    cover: undefined,
    durationMs: 0,
    albumId: makeAlbumId(album, artist),
    lyrics: "",
    online: true,
  };
}

/* 读一遍在线曲目清单。宿主没有 netplay（旧版/电脑模拟）时返回空数组。 */
function scanOnlineTracks(): Track[] {
  const api = media();
  if (!api || !api.netplay) return [];
  let list: { url?: string; title?: string }[] = [];
  try {
    const parsed = JSON.parse(api.netplay() || "[]");
    list = Array.isArray(parsed) ? parsed : [];
  } catch {
    list = [];
  }
  return list
    .filter((e) => e && typeof e.url === "string" && e.url.length > 0)
    .map((e, i) => buildOnlineTrack(e.url as string, e.title || "", i));
}

/* =========================================================
 * AUDIO BACKEND
 *
 * UI 仍然认 position / playing / finishTrack。
 * onFrame 只从 backend 读当前位置。
 *
 * 优先级：
 *   1) 有 audioPath 且 host 挂了 vitaMedia：走 Vita 原生解码 (MP3/OGG/WAV/FLAC/OPUS/M4A)
 *   2) 否则墙钟模拟
 * ======================================================= */

const audioEngine = {
  loadedPath: "",
  mode: "clock" as "clock" | "vita",
  sampleRate: 44100,
  clockOriginMs: 0,
  clockOffsetMs: 0,
  running: false,

  /* Native state() 约 10Hz 读取；UI 仍由 onFrame 60Hz 更新。 */
  statePollFrames: 0,
  statePollIntervalFrames: 6,
  nativePosMs: 0,
  nativeDurMs: 0,
  nativePlaying: false,
  nativePaused: false,
  nativeSampleAtMs: 0,
  nativeSampleValid: false,
  nativePath: "",

  load(song: Track) {
    const realPath = song.audioPath || "";
    this.loadedPath = realPath;
    this.clockOffsetMs = 0;
    this.clockOriginMs = Date.now();
    this.running = false;
    this.statePollFrames = 0;
    this.nativePosMs = 0;
    this.nativeDurMs = 0;
    this.nativePlaying = false;
    this.nativePaused = false;
    this.nativeSampleAtMs = Date.now();
    this.nativeSampleValid = false;

    const vm = media();
    this.mode = realPath && vm && vm.play && vm.state ? "vita" : "clock";
  },

  play() {
    this.running = true;
    this.clockOriginMs = Date.now();
    this.statePollFrames = 0;
    const vm = media();

    if (this.mode === "vita" && vm && vm.play) {
      try {
        /* 暂停后恢复播放：必须走原生 resume。
         * vm.play() 会重新打开文件、另起一个解码线程，等于从 0 重播 —— 
         * 这正是“点暂停再点播放会从头开始”的原因。带上路径是为了极端情况下
         * （暂停时正好放到结尾、解码线程已经退出）还能退化成重新开一首。 */
        if (this.nativePaused && vm.resume) {
          vm.resume(this.loadedPath);
        } else {
          vm.play(this.loadedPath);
        }
        this.nativeSampleValid = false;
      } catch {
        this.mode = "clock";
      }
    }
  },

  pause() {
    if (this.running) this.clockOffsetMs = this.positionMs();
    this.running = false;
    const vm = media();

    if (this.mode === "vita" && vm && vm.pause) {
      try { vm.pause(); } catch { /* ignore */ }
    }

    if (this.mode === "vita") {
      this.nativePlaying = false;
      this.nativePaused = true;
      this.nativePosMs = this.clockOffsetMs;
      this.nativeSampleAtMs = Date.now();
      this.nativeSampleValid = true;
    }
  },

  stop() {
    this.running = false;
    this.clockOffsetMs = 0;
    this.clockOriginMs = Date.now();
    this.statePollFrames = 0;
    this.nativePosMs = 0;
    this.nativeDurMs = 0;
    this.nativePlaying = false;
    this.nativePaused = false;
    this.nativeSampleAtMs = Date.now();
    this.nativeSampleValid = false;
    const vm = media();

    if (this.mode === "vita" && vm && vm.stop) {
      try { vm.stop(); } catch { /* ignore */ }
    }
  },

  pump() {},

  refreshNativeState(): boolean {
    const vm = media();
    if (this.mode !== "vita" || !vm || !vm.state) return false;

    try {
      const st = JSON.parse(vm.state() || "{}") as {
        playing?: boolean; paused?: boolean; pos?: number; dur?: number;
      };
      this.nativePosMs = Math.max(0, Number(st.pos) || 0);
      this.nativeDurMs = Math.max(0, Number(st.dur) || 0);
      this.nativePlaying = !!st.playing && !st.paused;
      this.nativePaused = !!st.paused;
      this.nativePath = String((st as { path?: string }).path || "");
      this.nativeSampleAtMs = Date.now();
      this.nativeSampleValid = true;
      return true;
    } catch {
      return false;
    }
  },

  snapshot(force = false): { posMs: number; durMs: number; playing: boolean; path: string } {
    if (this.mode === "vita") {
      this.statePollFrames += 1;
      if (force || !this.nativeSampleValid || this.statePollFrames >= this.statePollIntervalFrames) {
        this.statePollFrames = 0;
        this.refreshNativeState();
      }

      if (this.nativeSampleValid) {
        let pos = this.nativePosMs;
        if (this.nativePlaying) pos += Math.max(0, Date.now() - this.nativeSampleAtMs);
        return {
          posMs: Math.max(0, pos),
          durMs: this.nativeDurMs,
          playing: this.nativePlaying,
          path: this.nativePath,
        };
      }
    }

    return {
      posMs: this.clockFallback(),
      durMs: 0,
      playing: this.running,
      path: this.loadedPath,
    };
  },

  clockFallback(): number {
    if (!this.running) return this.clockOffsetMs;
    return this.clockOffsetMs + Math.max(0, Date.now() - this.clockOriginMs);
  },

  positionMs(): number { return this.snapshot(true).posMs; },
};

/* =========================================================
 * PAGE TRANSITION
 * ======================================================= */

function PageEnter(props: { dir?: number; children: any }) {
  let el: NodeMirror | undefined;
  const dir = props.dir ?? 1;

  onMount(() => {
    if (!el) {
      return;
    }

    animate(el, "opacity", 1, { dur: 200, easing: "out" });
    animate(el, "translateX", 0, {
      dur: 300,
      easing: "out-back",
      delay: 20,
    });
  });

  return (
    <View
      ref={(node: NodeMirror) => {
        el = node;
      }}
      style={{ opacity: 0, translateX: dir * 44 }}
      class="w-96 h-48 items-center justify-center"
    >
      {props.children}
    </View>
  );
}

/* 弹出式子界面（About / Lyrics）：打开时从右侧滑入并淡入，关闭时向右侧滑出并淡出。
 * 关闭动画必须在卸载前播放，因此由 closing() 驱动离场，动画播完后经 onExitDone 通知宿主真正卸载。 */
function PageInOut(props: {
  dir?: number;
  closing: () => boolean;
  onExitDone: () => void;
  children: any;
}) {
  let el: NodeMirror | undefined;
  let disposed = false;
  let disposeExitTimer: (() => void) | undefined;
  const dir = props.dir ?? 1;

  onMount(() => {
    if (el) {
      animate(el, "opacity", 1, { dur: 200, easing: "out" });
      animate(el, "scale", 1, { dur: 260, easing: "out-back", delay: 10 });
      animate(el, "translateX", 0, {
        dur: 300,
        easing: "out-back",
        delay: 20,
      });
    }
  });

  createEffect(() => {
    if (props.closing() && el) {
      animate(el, "opacity", 0, { dur: 180, easing: "in" });
      animate(el, "scale", 0.98, { dur: 240, easing: "in" });
      animate(el, "translateX", dir * 44, {
        dur: 240,
        easing: "in",
      });
      /* PocketJS 的 setTimeout 会退化成微任务（不会等 300ms），这里改用确定性的虚拟时钟
       * after()：0.3s 后（60Hz 下约 18 帧）真正卸载，保证离场动画播完再切回原界面。 */
      disposeExitTimer?.();
      disposeExitTimer = after(0.3, () => {
        if (!disposed) props.onExitDone();
      });
    }
  });

  onCleanup(() => {
    disposed = true;
    disposeExitTimer?.();
  });

  return (
    <View
      ref={(node: NodeMirror) => {
        el = node;
      }}
      style={{ opacity: 0, translateX: dir * 44, scale: 0.98 }}
      class="w-96 h-48 items-center justify-center"
    >
      {props.children}
    </View>
  );
}

/* =========================================================
 * MAIN APP
 * ======================================================= */

/* 截断工具：超长文本用省略号，避免溢出/互相叠字。 */
function clip(s: string | undefined, n: number): string {
  const t = s || "";
  return t.length <= n ? t : t.slice(0, Math.max(1, n - 1)) + "…";
}

function formatMs(ms: number): string {
  const t = Math.max(0, Math.floor((ms || 0) / 1000));
  const m = Math.floor(t / 60);
  const s = t % 60;
  return m + ":" + (s < 10 ? "0" : "") + s;
}

export default function Music() {
  /* =======================================================
   * DATA SOURCE — 未来接入真实曲库的唯一入口
   *
   * 现在传入 MOCK_TRACKS。
   * 以后真实扫描完成后，调用 setTracks(realTracks) 即可，
   * 其余状态 / UI 全部自动跟着重新派生，不用改。
   * ======================================================= */

  const [tracks, setTracks] = createSignal<Track[]>(MOCK_TRACKS);

  /* 派生数据：完全由 tracks() 计算得出，永远保持同步 */
  const albums = createMemo(() => buildAlbums(tracks()));
  const trackById = createMemo(() => buildTrackById(tracks()));
  const albumById = createMemo(() => buildAlbumById(albums()));

  /* =======================================================
   * UI / NAVIGATION STATE
   * ======================================================= */

  const [screen, setScreen] = createSignal<Screen>("home");
  const [focusZone, setFocusZone] = createSignal<FocusZone>("nav");
  const [navIndex, setNavIndex] = createSignal(0);

  const [homeCursor, setHomeCursor] = createSignal(1);
  const [lyricsVisible, setLyricsVisible] = createSignal(false);
  const [lyricsClosing, setLyricsClosing] = createSignal(false);
  /* 设置页里的子页面：about（关于）/ keys（按键说明）/ null（没开）。
   * 两者共用同一套进出动画和"△ 返回"，都只是一页静态文字。 */
  const [subPage, setSubPage] = createSignal<"about" | "keys" | null>(null);
  const [subClosing, setSubClosing] = createSignal(false);

  const [listCursor, setListCursor] = createSignal(0);
  const [listStart, setListStart] = createSignal(0);

  const [albumCursor, setAlbumCursor] = createSignal(0);
  const [albumStart, setAlbumStart] = createSignal(0);

  /* 专辑封面缓存：albumId -> 已上传的封面纹理 key（懒加载，仅当前可见的 3 张） */
  const [albumCovers, setAlbumCovers] = createSignal<Record<string, string>>({});

  const [settingCursor, setSettingCursor] = createSignal(0);

  /*
   * 当前选中查看的 Album。
   *
   * 存 id，不存下标：Album 列表顺序变化时依然指向同一张专辑。
   */
  const [selectedAlbumId, setSelectedAlbumId] = createSignal<
    string | null
  >(null);

  /*
   * 当前播放歌曲。
   *
   * 存 id，不存下标：曲库刷新/重排时，
   * 正在播放的歌不会被"偷换"成别的歌。
   */
  const [currentTrackId, setCurrentTrackId] = createSignal<string>(
    MOCK_TRACKS[0]?.id ?? "",
  );

  /* 进应用默认是暂停状态：等用户按播放键才出声（按 PS 回桌面后的后台
   * 播放由原生侧接管，和这里的状态无关）。 */
  const [playing, setPlaying] = createSignal(false);
  /* 息屏中（Start）：画面盖一层纯黑遮罩、帧循环歇着，少占 CPU/GPU；
   * 屏幕没真的关，按键还在，所以黑屏下 L/R 一样能换曲。 */
  const [displayOff, setDisplayOff] = createSignal(false);

  /* 当前 mock 播放位置(ms)。Native Audio 接入后直接接 native position */
  const [position, setPosition] = createSignal(0);

  /* Visualizer phase，和 position 分开 */
  const [barsFrame, setBarsFrame] = createSignal(0);
  let barsFrameTick = 0; /* 节流计数：律动条约 30fps 更新 */

  const [playbackMode, setPlaybackMode] =
    createSignal<PlaybackMode>("sequence");

  /*
   * 收藏列表。
   *
   * 存 track.id（稳定标识），可安全跨会话持久化，
   * 曲库重新扫描也不会指错歌。
   */
  /* 收藏：从磁盘读回，跨会话保留。 */
  const [favorites, setFavorites] = createSignal<string[]>(
    (() => {
      try {
        const raw = media()?.store_get?.("favorites") || "[]";
        const arr = JSON.parse(raw);
        return Array.isArray(arr) ? arr.filter((x) => typeof x === "string") : [];
      } catch {
        return [];
      }
    })(),
  );


  const navRefs: (NodeMirror | undefined)[] = [];
  let contentRef: NodeMirror | undefined;

  /*
   * 当前播放队列。存 Track.id。
   * List / Album / Loved 点歌时写入，next/prev/finish 只走这里。
   */
  const [queueIds, setQueueIds] = createSignal<string[]>(
    MOCK_TRACKS.map((song) => song.id),
  );

  /* =======================================================
   * MEMOIZED CURRENT DATA
   * ======================================================= */

  /* 当前 Track。曲库为空时兜底为 FALLBACK_TRACK，不会崩 */
  const track = createMemo(
    () => trackById()[currentTrackId()] ?? tracks()[0] ?? FALLBACK_TRACK,
  );

  /* 当前曲目内嵌封面：只在切到该曲目时懒加载，避免扫描时把整库封面都传进显存。 */
  createEffect(() => {
    const cur = track();
    /* 在线曲目的 audioPath 是 URL：封面要真的下载才拿得到，先不做（也不该在这里做）。 */
    if (!cur || cur.cover || !cur.audioPath || cur.online) return;
    const api = media();
    if (!api || !api.cover) return;
    let handle = -1;
    try {
      handle = api.cover(cur.audioPath);
    } catch {
      /* fallback to gradient */
    }
    if (typeof handle === "number" && handle >= 0) {
      const key = "emb:" + cur.audioPath;
      registerTexture(key, handle);
      setTracks((prev) =>
        prev.map((t) => (t.id === cur.id ? { ...t, cover: key } : t)),
      );
    }
  });

  /* 当前可见专辑的封面懒加载：只给 3 张专辑的第一首歌取内嵌封面 */
  createEffect(() => {
    if (screen() !== "album" || selectedAlbumId() !== null) return;
    const api = media();
    if (!api || !api.cover) return;
    const visible = albums().slice(albumStart(), albumStart() + 3);
    const log = (m: string) => {
      /* 诊断日志默认关：关着连字符串都不拼，省掉每次重跑的这点开销。 */
      if (!logEnabled()) return;
      try {
        (media() as unknown as { logMsg?: (s: string) => void })?.logMsg?.(m);
      } catch { /* ignore */ }
    };
    log(`albumcover screen=${screen()} vis=${visible.length}`);
    for (const a of visible) {
      if (albumCovers()[a.id]) continue;
      const first = a.trackIds[0] ? trackById()[a.trackIds[0]] : undefined;
      if (!first || !first.audioPath || first.online) {
        log(`  ac ${a.id} -> no first/audioPath`);
        continue;
      }
      let h = -1;
      try {
        h = api.cover(first.audioPath);
      } catch {
        /* ignore */
      }
      if (typeof h === "number" && h >= 0) {
        const key = "emb:" + first.audioPath;
        registerTexture(key, h);
        setAlbumCovers((prev) => ({ ...prev, [a.id]: key }));
        log(`  ac ${a.id} -> cover ok handle=${h}`);
      } else {
        log(`  ac ${a.id} -> cover FAIL handle=${h}`);
      }
    }
  });

  /* 当前 Album（在专辑详情页时使用） */
  const album = createMemo(() => {
    const id = selectedAlbumId();

    if (id === null) {
      return null;
    }

    return albumById()[id] ?? null;
  });

  /* 当前歌曲歌词：解析 Track.lyrics（LRC / USLT），切歌才重建 */
  const lyrics = createMemo(() => parseLyrics(track().lyrics, track()));

  /*
   * 当前列表的 track id 数组。
   *
   * List / Loved / Album 都统一从这里派生，
   * 内容一律是 Track.id，不是下标。
   */
  const currentListTrackIds = createMemo(() => {
    const currentScreen = screen();

    if (currentScreen === "list") {
      return tracks().map((song) => song.id);
    }

    if (currentScreen === "loved") {
      /* 过滤掉曲库里已经不存在的收藏 id */
      return sanitizeIds(favorites(), trackById());
    }

    if (currentScreen === "album" && selectedAlbumId() !== null) {
      return album()?.trackIds ?? [];
    }

    return [];
  });

  /* 播放队列：清洗掉已经不存在的 id，空队列回退到整库 */
  const activeQueueIds = createMemo(() => {
    const cleaned = sanitizeIds(queueIds(), trackById());

    if (cleaned.length > 0) {
      return cleaned;
    }

    return tracks().map((song) => song.id);
  });

  /* 收藏状态，用当前 track.id 判断 */
  const isFavorite = createMemo(() => favorites().includes(track().id));

  /* 当前播放百分比，完全由 position / durationMs 计算 */
  const percent = createMemo(() => {
    const currentTrack = track();
    const duration = getTrackDuration(currentTrack);
    const currentPosition = Math.min(
      duration,
      Math.max(0, position()),
    );

    return Math.min(100, Math.round((currentPosition / duration) * 100));
  });

  /*
   * 头部 "N/Total" 计数。
   *
   * 通过在当前 tracks() 里查找 currentTrackId() 的位置来算，
   * 而不是直接持有一个下标信号，
   * 这样曲库顺序变化时这个数字依然正确。
   */
  const trackPositionLabel = createMemo(() => {
    const list = activeQueueIds();
    const idx = list.indexOf(currentTrackId());

    return {
      pos: idx >= 0 ? idx + 1 : 0,
      total: list.length,
    };
  });

  /* Root theme */
  const rootClass = createMemo(() => {
    if (uiTheme() === "pure") {
      return "flex-col w-full h-full p-2 gap-2 bg-gradient-to-b from-orange-200 to-amber-50";
    }
    if (uiTheme() === "anime") {
      return "flex-col w-full h-full p-2 gap-2 bg-gradient-to-b from-fuchsia-950 to-rose-900";
    }
    return uiTheme() === "dark"
      ? "flex-col w-full h-full p-2 gap-2 bg-gradient-to-b from-slate-950 to-indigo-950"
      : "flex-col w-full h-full p-2 gap-2 bg-gradient-to-b from-emerald-50 to-teal-50";
  });

  /*
   * 按 id 查一首歌的展示信息。
   *
   * 传给列表类组件用，避免它们直接依赖任何全局常量。
   */
  const getTrack = (id: string): Track | undefined => trackById()[id];

  /* =======================================================
   * FOCUS
   * ======================================================= */

  const focusNav = () => {
    const node = navRefs[navIndex()];

    if (node) {
      focusNode(node);
    }
  };

  const focusContent = () => {
    if (contentRef) {
      focusNode(contentRef);
    }
  };

  onMount(() => {
    // 优先加载 Vita 上的真实曲库（ux0:music 等）。找不到则保持 MOCK_TRACKS。
    const realTracks = scanLibrary();
    if (realTracks.length) {
      setTracks(realTracks);
      setQueueIds(realTracks.map((song) => song.id));
      /*
       * 有在线曲目时，进来就停在第一首在线歌上：界面显示的是它，按 ○ 放的
       * 也是它 —— 不会再出现"播放页写着第一首本地歌、按下去却是另一首"。
       * 没有在线曲目时照旧停在第一首本地歌。
       */
      const firstOnline = realTracks.find((song) => song.online);
      setCurrentTrackId((firstOnline ?? realTracks[0]).id);
    }

    focusNav();
    audioEngine.load(track());
    if (cjkMode() === "stream") {
      applyCjkMode("stream");
    }
  });

  /* =======================================================
   * RESET
   * ======================================================= */

  const resetHome = () => {
    setHomeCursor(1);
    setLyricsVisible(false);
    setLyricsClosing(false);
  };

  const resetList = () => {
    setListCursor(0);
    setListStart(0);
  };

  const resetAlbum = () => {
    setAlbumCursor(0);
    setAlbumStart(0);
  };

  const resetSetting = () => {
    setSettingCursor(0);
  };

  /* =======================================================
   * FOCUS ZONE
   * ======================================================= */

  const enterContent = () => {
    setFocusZone("content");
    focusContent();
  };

  const leaveContent = () => {
    setFocusZone("nav");
    focusNav();
  };

  /* =======================================================
   * NAVIGATION
   * ======================================================= */

  const openSelectedNav = () => {
    const index = navIndex();
    setSubPage(null);
    setSubClosing(false);

    if (index === 0) {
      setScreen("home");
      setSelectedAlbumId(null);
      resetHome();
      resetList();
      enterContent();
      return;
    }

    if (index === 1) {
      setScreen("list");
      setSelectedAlbumId(null);
      setLyricsVisible(false);
      resetList();
      enterContent();
      return;
    }

    if (index === 2) {
      setScreen("album");
      setSelectedAlbumId(null);
      setLyricsVisible(false);
      resetAlbum();
      resetList();
      enterContent();
      return;
    }

    if (index === 3) {
      setScreen("loved");
      setSelectedAlbumId(null);
      setLyricsVisible(false);
      resetList();
      enterContent();
      return;
    }

    setScreen("setting");
    setSelectedAlbumId(null);
    setLyricsVisible(false);
    resetSetting();
    enterContent();
  };

  const openAlbum = (id: string) => {
    if (!albumById()[id]) {
      return;
    }

    setSelectedAlbumId(id);
    resetList();
    setLyricsVisible(false);
    setScreen("album");
    enterContent();
  };

  /* =======================================================
   * TRACK CONTROL — 全部以 id 为准
   * ======================================================= */

  const startTrack = (id: string) => {
    const song = trackById()[id];

    if (!song) {
      return;
    }

    audioEngine.stop();
    audioEngine.load(song);
    setCurrentTrackId(id);
    setPosition(0);
    setBarsFrame(0);
    setPlaying(true);
    audioEngine.play();
  };

  const adoptQueue = (ids: string[]) => {
    const cleaned = sanitizeIds(ids, trackById());
    setQueueIds(cleaned.length > 0 ? cleaned : tracks().map((song) => song.id));
  };

  const playTrack = (id: string) => {
    const listIds = currentListTrackIds();
    adoptQueue(listIds.length > 0 ? listIds : tracks().map((song) => song.id));
    startTrack(id);

    setSelectedAlbumId(null);
    setScreen("home");
    setNavIndex(0);
    setLyricsVisible(false);

    resetHome();
    resetList();
    enterContent();
  };

  const findQueuePosition = (id: string): number => {
    return activeQueueIds().indexOf(id);
  };

  const jumpInQueue = (delta: number) => {
    const list = activeQueueIds();

    if (list.length === 0) {
      return;
    }

    const currentIdx = findQueuePosition(currentTrackId());
    const safeIdx = currentIdx === -1 ? 0 : currentIdx;
    const nextId = list[(safeIdx + delta + list.length) % list.length];

    if (!nextId) {
      return;
    }

    startTrack(nextId);

    setScreen("home");
    setSelectedAlbumId(null);
    setNavIndex(0);
    setLyricsVisible(false);

    resetHome();
    resetList();
    enterContent();
  };

  const nextTrack = () => {
    jumpInQueue(1);
  };

  const prevTrack = () => {
    jumpInQueue(-1);
  };

  const finishTrack = () => {
    const list = activeQueueIds();

    if (list.length === 0) {
      return;
    }

    /* 单曲循环 */
    if (playbackMode() === "repeat-one") {
      audioEngine.stop();
      audioEngine.load(track());
      setPosition(0);
      setBarsFrame(0);
      setPlaying(true);
      audioEngine.play();
      return;
    }

    jumpInQueue(1);
  };

  const togglePlay = () => {
    const next = !playing();
    setPlaying(next);

    if (next) {
      audioEngine.play();
      return;
    }

    audioEngine.pause();
  };

  const togglePlaybackMode = () => {
    setPlaybackMode(
      playbackMode() === "sequence" ? "repeat-one" : "sequence",
    );
  };

  const toggleFavorite = () => {
    const currentId = track().id;
    const currentFavorites = favorites();
    const next = currentFavorites.includes(currentId)
      ? currentFavorites.filter((id) => id !== currentId)
      : [...currentFavorites, currentId];

    setFavorites(next);
    try {
      media()?.store_set?.("favorites", JSON.stringify(next));
    } catch {
      /* ignore */
    }
  };

  /* =======================================================
   * HOME MOVEMENT
   * ======================================================= */

  const moveHomeLeft = (): boolean => {
    if (homeCursor() <= 0) {
      return false;
    }

    setHomeCursor(homeCursor() - 1);
    return true;
  };

  const moveHomeRight = () => {
    if (homeCursor() >= 5) {
      return;
    }

    setHomeCursor(homeCursor() + 1);
  };

  /* =======================================================
   * LIST MOVEMENT
   * ======================================================= */

  const moveListLeft = (): boolean => {
    if (listCursor() <= 0) {
      return false;
    }

    const next = listCursor() - 1;
    setListCursor(next);

    if (next < listStart()) {
      setListStart(next);
    }

    return true;
  };

  const moveListRight = () => {
    const count = currentListTrackIds().length;

    if (listCursor() >= count - 1) {
      return;
    }

    const next = listCursor() + 1;
    setListCursor(next);

    if (next >= listStart() + 3) {
      setListStart(next - 2);
    }
  };

  /* =======================================================
   * ALBUM MOVEMENT
   * ======================================================= */

  const moveAlbumLeft = (): boolean => {
    if (albumCursor() <= 0) {
      return false;
    }

    if (albumCursor() > albumStart()) {
      setAlbumCursor(albumCursor() - 1);
      return true;
    }

    if (albumStart() <= 0) {
      return false;
    }

    setAlbumStart(albumStart() - 1);
    setAlbumCursor(albumCursor() - 1);
    return true;
  };

  const moveAlbumRight = () => {
    const last = albums().length - 1;

    if (albumCursor() >= last) {
      return;
    }

    if (albumCursor() < albumStart() + 2) {
      setAlbumCursor(albumCursor() + 1);
      return;
    }

    if (albumStart() < last - 2) {
      setAlbumStart(albumStart() + 1);
    }

    setAlbumCursor(albumCursor() + 1);
  };

  /* =======================================================
   * SETTINGS MOVEMENT
   * ======================================================= */

  const moveSettingLeft = (): boolean => {
    if (settingCursor() <= 0) {
      return false;
    }

    setSettingCursor(settingCursor() - 1);
    return true;
  };

  const moveSettingRight = () => {
    if (settingCursor() >= 3) {
      return;
    }

    setSettingCursor(settingCursor() + 1);
  };

  /* =======================================================
   * SETTINGS ACTION
   * ======================================================= */

  const activateSetting = () => {
    const index = settingCursor();

    if (index === 0) {
      /* CJK：切换 BAKED（只用烘焙字集）/ STREAM（生僻字按需从 cjk.pjfa 取）。 */
      applyCjkMode(cjkMode() === "stream" ? "baked" : "stream");
      return;
    }

    if (index === 1) {
      /* KEYS：按键操作说明（和 About 一样的静态文字页，△ 返回）。 */
      setSubPage("keys");
      setSubClosing(false);
      focusContent();
      return;
    }

    if (index === 2) {
      /* ABOUT：打开 About Us 静态文字界面，△ 返回设置页。 */
      setSubPage("about");
      setSubClosing(false);
      focusContent();
      return;
    }

    setUiTheme(uiTheme() === "light" ? "pure" : uiTheme() === "pure" ? "anime" : uiTheme() === "anime" ? "dark" : "light");
  };

  /* =======================================================
   * CONTENT ACTION
   * ======================================================= */

  const activateContent = () => {
    if (screen() === "home") {
      if (lyricsVisible()) {
        return;
      }

      const cursor = homeCursor();

      if (cursor === 0) {
        prevTrack();
        return;
      }

      if (cursor === 1) {
        togglePlay();
        return;
      }

      if (cursor === 2) {
        nextTrack();
        return;
      }

      if (cursor === 3) {
        togglePlaybackMode();
        return;
      }

      if (cursor === 4) {
        toggleFavorite();
        return;
      }

      if (cursor === 5) {
        setLyricsVisible(true);
        setLyricsClosing(false);
        return;
      }

      return;
    }

    if (screen() === "setting") {
      if (subPage()) {
        /* 子页面是纯文字，○ 在这里不做任何事（△ 返回）。 */
        return;
      }
      activateSetting();
      return;
    }

    if (screen() === "album" && selectedAlbumId() === null) {
      const list = albums();
      const chosen = list[albumCursor()];

      if (chosen) {
        openAlbum(chosen.id);
      }

      return;
    }

    const ids = currentListTrackIds();
    const selectedId = ids[listCursor()];

    if (selectedId !== undefined) {
      playTrack(selectedId);
    }
  };

  /* =======================================================
   * BUTTON: UP
   * ======================================================= */

  /* 息屏（黑屏遮罩）期间：方向键 / ○ / △ 不做事，只由下面的通配处理器
   * 负责亮屏；这样"按一下键亮屏"不会顺手在界面上点出一个动作。 */
  const screenOn = () => !displayOff();

  onButtonPress(BTN.UP, () => {
    if (focusZone() !== "nav") {
      return;
    }

   const current = navIndex();
   const next = Math.max(0, current - 1);

   if (next !== current) {
     setNavIndex(next);
     focusNav();
   }
  }, { active: screenOn });

  /* =======================================================
   * BUTTON: DOWN
   * ======================================================= */

  onButtonPress(BTN.DOWN, () => {
    if (focusZone() !== "nav") {
      return;
    }

   const current = navIndex();
   const next = Math.min(4, current + 1);

   if (next !== current) {
     setNavIndex(next);
     focusNav();
   }
  }, { active: screenOn });

  /* =======================================================
   * BUTTON: LEFT
   * ======================================================= */

  onButtonPress(BTN.LEFT, () => {
    if (focusZone() !== "content") {
      return;
    }

    if (screen() === "home" && lyricsVisible()) {
      return;
    }

    if (screen() === "home") {
      if (moveHomeLeft()) {
        return;
      }

      leaveContent();
      return;
    }

    if (screen() === "album" && selectedAlbumId() === null) {
      if (moveAlbumLeft()) {
        return;
      }

      leaveContent();
      return;
    }

    if (screen() === "setting") {
      if (subPage()) {
        return;
      }
      if (moveSettingLeft()) {
        return;
      }

      leaveContent();
      return;
    }

    if (moveListLeft()) {
      return;
    }

    leaveContent();
  }, { active: screenOn });

  /* =======================================================
   * BUTTON: L / R —— 上一首 / 下一首
   *
   * 故意不做焦点判断：关屏（应用仍在后台播放）时也要能直接切歌，
   * 这是 PSP 上那套"合盖换曲"的习惯用法。
   * ======================================================= */

  onButtonPress(BTN.LTRIGGER, () => {
    prevTrack();
  });

  onButtonPress(BTN.RTRIGGER, () => {
    nextTrack();
  });

  /* =======================================================
   * BUTTON: START —— 息屏（只盖黑屏遮罩，不真的关屏）
   *
   * 真的调 scePowerRequestDisplayOff 关屏之后，系统不再把按键报给应用
   * （实机日志里关屏后一条按键记录都没有），黑屏换曲就没法做。
   * 所以这里只是盖一层纯黑遮罩 + 停止画面更新：看起来一样是黑的，
   * 但应用还在前台跑，按键照常进来（黑屏 L/R 换曲、任意键亮屏）。
   * ======================================================= */

  onButtonPress(BTN.START, () => {
    const next = !displayOff();
    setDisplayOff(next);
    logMsg(next ? "screen: soft off" : "screen: on");
  });

  /* 息屏中按任意键亮屏；L / R 只换曲，不亮屏（合盖换曲就得屏幕一直黑着）。 */
  onButtonPress(0xffff, (pressed) => {
    if (displayOff() && pressed & ~(BTN.LTRIGGER | BTN.RTRIGGER | BTN.START)) {
      setDisplayOff(false);
    }
  });

  /* =======================================================
   * BUTTON: RIGHT
   * ======================================================= */

  onButtonPress(BTN.RIGHT, () => {
    if (focusZone() === "nav") {
      enterContent();
      return;
    }

    if (screen() === "home" && lyricsVisible()) {
      return;
    }

    if (screen() === "home") {
      moveHomeRight();
      return;
    }

    if (screen() === "album" && selectedAlbumId() === null) {
      moveAlbumRight();
      return;
    }

    if (screen() === "setting") {
      if (subPage()) {
        return;
      }
      moveSettingRight();
      return;
    }

    moveListRight();
  }, { active: screenOn });

  /* =======================================================
   * BUTTON: CIRCLE
   * ======================================================= */

  onButtonPress(BTN.CIRCLE, () => {
    if (focusZone() === "nav") {
      openSelectedNav();
      return;
    }

    activateContent();
  }, { active: screenOn });

  /* =======================================================
   * BUTTON: TRIANGLE
   * ======================================================= */

  onButtonPress(BTN.TRIANGLE, () => {
    if (focusZone() === "nav") {
      return;
    }

    if (screen() === "home" && lyricsVisible()) {
      setLyricsClosing(true);
      setHomeCursor(5);
      /* 焦点在离场动画播完后，由 PageInOut.onExitDone 归还到内容区。 */
      return;
    }

    if (screen() === "setting" && subPage()) {
      setSubClosing(true);
      /* 焦点在离场动画播完后，由 PageInOut.onExitDone 归还到内容区。 */
      return;
    }

    if (screen() === "album" && selectedAlbumId() !== null) {
      setSelectedAlbumId(null);
      resetList();
      resetAlbum();
      focusContent();
      return;
    }

    leaveContent();
  }, { active: screenOn });

  /* =======================================================
   * OPTIMIZED FRAME LOOP
   * ======================================================= */

  /* =======================================================
   * PS 键锁：播放期间按 PS 出不去
   *
   * 按 PS 会回 LiveArea 把应用切后台；而后台继续出声需要 Vita 宿主那套
   * 后台音频支持（上游 PocketJS 没做，实测切后台就断）。所以换个思路：
   * **播放中锁住 PS 键**，想离开应用必须先暂停 —— 暂停 / 停止 / 放完立刻解锁。
   * ======================================================= */
  createEffect(() => {
    const on = playing();
    let ret = 0;
    try {
      ret = media()?.setPsLock?.(on) ?? 0;
    } catch {
      ret = -1;
    }
    /* 0 = 系统调用成功；非 0 在 About 页显示成 FAIL 0x…，方便确认锁有没有生效。 */
    setPsLockInfo(
      on
        ? ret === 0
          ? "PS KEY  LOCKED"
          : "PS KEY  FAIL 0x" + (ret >>> 0).toString(16).toUpperCase()
        : "PS KEY  UNLOCKED",
    );
    logMsg("ps lock: request=" + String(on) + " ret=" + String(ret));
  });

  let frameCounter = 0;
  let lastFrameMs = 0;

  onFrame(() => {
    audioEngine.pump();

    /* STREAM 诊断：约每秒把流式字库的状态写一行到日志。
     * 日志默认关；关着的时候连统计都不采集，正式版零额外开销。 */
    if (cjkMode() === "stream") {
      cjkDbgFrames += 1;
      if (cjkDbgFrames % 60 === 0 && logEnabled()) logCjkStats();
    }

    /* 息屏中又被切到后台（PS → LiveArea）再回来，帧循环会空一大段；
     * 这时自动亮屏，免得回来面对一片黑还以为卡死了。 */
    const nowMs = Date.now();
    const frameGapMs = nowMs - lastFrameMs;
    lastFrameMs = nowMs;
    if (displayOff() && frameGapMs > 1500) {
      logMsg("screen: on (back from background)");
      setDisplayOff(false);
    }

    /* 息屏期间：黑屏遮罩已经盖上，UI 这一帧就不用算了（省 CPU/GPU）。 */
    if (displayOff()) return;
    frameCounter += 1;

    /* Native state() 约 10Hz；两次采样之间由 audioEngine 平滑预测。 */
    const snap = audioEngine.snapshot(frameCounter === 1);

    if (snap.durMs > 0) {
      const cur = track();
      if (cur && cur.durationMs !== snap.durMs) {
        setTracks((prev) =>
          prev.map((t) => t.id === cur.id ? { ...t, durationMs: snap.durMs } : t),
        );
      }
    }

    if (!playing()) return;

    const currentTrack = track();
    /* 息屏期间可能由原生侧（L/R 肩键）换过歌：回到前台时把 UI 同步过来。 */
    if (snap.path && snap.path !== currentTrack.audioPath) {
      const matched = tracks().find((song) => song.audioPath === snap.path);
      if (matched && matched.id !== currentTrack.id) {
        setCurrentTrackId(matched.id);
      }
    }
    const duration = currentTrack.durationMs || snap.durMs || getTrackDuration(currentTrack);
    const next = snap.posMs;

    if (duration > 0 && next >= duration) {
      finishTrack();
      return;
    }

    /* 进度/律动条统一降到约 30fps：位置信号每变一次就要重排整棵受影响子树，
     * 60Hz 对这个"慢慢爬的进度条 + 五根柱子"没有可见收益，30Hz 已经把
     * 每帧的刷新点砍掉一半（歌词高亮 33ms 精度也完全够）。 */
    barsFrameTick += 1;
    if ((barsFrameTick & 1) === 0) {
      if (next !== position()) setPosition(next);
      setBarsFrame((value) => value + 1);
    }
  });

  /* =======================================================
   * ROOT
   * ======================================================= */

  return (
    <View debugName="MusicScreen" class={rootClass()}>
      <ThemeSeed />
      {/* 全屏背景（按主题，铺满整个画面） */}
      <Image src={useSkin().screenBg} class="absolute inset-0 w-full h-full" />
      {/* HEADER */}

      <View class="flex-row items-center justify-between h-6">
        <View class="flex-row items-center gap-1">
          <Text class={pTxt("brand")}>
            YUNYIN
          </Text>
        </View>

        <Text class={pTxt("footer")}>
          {trackPositionLabel().pos}/{trackPositionLabel().total}
        </Text>
      </View>

      {/* MAIN */}

      <View class="flex-row grow">
        {/* LEFT NAV */}

        <FocusScope
          active={() => focusZone() === "nav"}
          autoFocus={false}
          restoreFocus={false}
          class="w-18 h-full"
        >
          <View class="flex-col items-center justify-center gap-2 w-45 h-45">
            <NavItem
              label="HOME"
              index={0}
              cursor={navIndex}
              active={screen() === "home"}
              refNode={(node: NodeMirror) => {
                navRefs[0] = node;
              }}
            />

            <NavItem
              label="LIST"
              index={1}
              cursor={navIndex}
              active={screen() === "list"}
              refNode={(node: NodeMirror) => {
                navRefs[1] = node;
              }}
            />

            <NavItem
              label="ALBUM"
              index={2}
              cursor={navIndex}
              active={screen() === "album"}
              refNode={(node: NodeMirror) => {
                navRefs[2] = node;
              }}
            />

            <NavItem
              label="LOVED"
              index={3}
              cursor={navIndex}
              active={screen() === "loved"}
              refNode={(node: NodeMirror) => {
                navRefs[3] = node;
              }}
            />

            <NavItem
              label="SET"
              index={4}
              cursor={navIndex}
              active={screen() === "setting"}
              refNode={(node: NodeMirror) => {
                navRefs[4] = node;
              }}
            />
          </View>
        </FocusScope>

        {/* RIGHT CONTENT */}

        <FocusScope
          active={() => focusZone() === "content"}
          autoFocus={false}
          restoreFocus={false}
          class="w-96 h-48"
        >
          <View
            ref={(node: NodeMirror) => {
              contentRef = node;
            }}
            focusable
            class="w-96 h-48"
          >
            {/* HOME PLAYER */}

            {screen() === "home" && !lyricsVisible() && !lyricsClosing() && (
              <PageEnter dir={-1}>
                <HomePage
                  track={track}
                  playing={playing}
                  position={position}
                  percent={percent}
                  barsFrame={barsFrame}
                  cursor={homeCursor}
                  playbackMode={playbackMode}
                  favorite={isFavorite}
                />
              </PageEnter>
            )}

            {/* HOME LYRICS */}

            {screen() === "home" && (lyricsVisible() || lyricsClosing()) && (
              <PageInOut
                dir={1}
                closing={() => lyricsClosing()}
                onExitDone={() => {
                  setLyricsVisible(false);
                  setLyricsClosing(false);
                  focusContent();
                }}
              >
                <LyricsPage
                  track={track}
                  playing={playing}
                  lines={lyrics}
                  position={position}
                  percent={percent}
                />
              </PageInOut>
            )}

            {/* ALL TRACKS */}

            {screen() === "list" && (
              <PageEnter dir={1}>
                <MusicListPage
                  title="ALL TRACKS"
                  subtitle={`${tracks().length} SONGS`}
                  trackIds={currentListTrackIds()}
                  getTrack={getTrack}
                  cursor={listCursor}
                  start={listStart}
                />
              </PageEnter>
            )}

            {/* ALBUM GRID */}

            {screen() === "album" && selectedAlbumId() === null && (
              <PageEnter dir={1}>
                <AlbumGrid
                  albums={albums}
                  cursor={albumCursor}
                  start={albumStart}
                  covers={() => albumCovers()}
                />
              </PageEnter>
            )}

            {/* ALBUM TRACKS */}

            {screen() === "album" && selectedAlbumId() !== null && (
              <PageEnter dir={1}>
                <MusicListPage
                  title={album()?.title ?? "ALBUM"}
                  subtitle={`${album()?.trackIds.length ?? 0} TRACKS`}
                  trackIds={currentListTrackIds()}
                  getTrack={getTrack}
                  cursor={listCursor}
                  start={listStart}
                />
              </PageEnter>
            )}

            {/* LOVED */}

            {screen() === "loved" && (
              <PageEnter dir={1}>
                <MusicListPage
                  title="LOVED TRACKS"
                  subtitle={`${favorites().length} FAVORITES`}
                  trackIds={currentListTrackIds()}
                  getTrack={getTrack}
                  cursor={listCursor}
                  start={listStart}
                />
              </PageEnter>
            )}

            {/* SETTINGS */}

            {screen() === "setting" && (
              <PageEnter dir={1}>
                {subPage() ? (
                  <PageInOut
                    dir={1}
                    closing={() => subClosing()}
                    onExitDone={() => {
                      setSubPage(null);
                      setSubClosing(false);
                      focusContent();
                    }}
                  >
                    {subPage() === "keys" ? <KeyGuidePage /> : <AboutPage />}
                  </PageInOut>
                ) : (
                  <SettingPage cursor={settingCursor} />
                )}
              </PageEnter>
            )}
          </View>
        </FocusScope>
      </View>

      {/* FOOTER */}

      <View class="flex-row items-center justify-between">
      <Text class={pTxt("footer")}>○ SELECT</Text>
      <Text class={pTxt("footer")}>△ BACK</Text>
      <Text class={pTxt("footer")}>L PREV | R NEXT</Text>
      <Text class={pTxt("footer")}>◎ MENU</Text>
      </View>

      {/* 息屏（Start）：一层纯黑遮罩盖住整个画面。屏幕其实还亮着，
          所以按键照常进得来 —— 黑屏下 L/R 换曲靠的就是这一点。 */}
      {displayOff() && <View class="absolute inset-0 z-50 bg-black" />}
    </View>
  );
}

/* =========================================================
 * NAV ITEM
 * ======================================================= */

/* label → 皮肤键。图标路径必须来自 SKINS 里的完整字面量：打包器只烘焙源码
 * 中出现的字面量路径，`asset/ui/${theme}/icon_nav_${label}.png` 这种运行时
 * 拼出来的路径不会被烘焙，于是图标全都空着（上一版的问题）。 */
const NAV_ICON_KEY: Record<string, UiSkinKey> = {
  HOME: "navHome",
  LIST: "navList",
  ALBUM: "navAlbum",
  LOVED: "navLoved",
  SET: "navSet",
};

function NavItem(props: {
  label: string;
  index: number;
  cursor: () => number;
  active: boolean;
  refNode: (node: NodeMirror) => void;
}) {
  const isCursor = () => props.cursor() === props.index;
  const icon = () => useSkin()[NAV_ICON_KEY[props.label] ?? "navHome"];

  return (
    <View
      ref={props.refNode}
      focusable
      class={
        isCursor()
          ? "relative w-16 h-8 overflow-hidden flex-col items-center justify-center transition-transform duration-150 ease-out-back scale-110"
          : "relative w-16 h-8 overflow-hidden flex-col items-center justify-center transition-transform duration-150 ease-out-back scale-100"
      }
    >
      <Image
        src={isCursor() ? useSkin().navFocus : props.active ? useSkin().navActive : useSkin().nav}
        class="absolute inset-0 w-full h-full"
      />
      <Image
        src={icon()}
        class="relative w-5 h-5"
      />
    </View>
  );
}

/* =========================================================
 * HOME PAGE
 * ======================================================= */

function Bars(props: {
  count: number;
  playing: () => boolean;
  frame: () => number;
}) {
  /* 把 5 个高度合到一个 memo：frame/playing 一变才重算，且每次只对变化的 bar 写宿主，
   * 避免 5 个独立响应式节点各自重算。 */
  const heights = createMemo(() => {
    const f = props.frame();
    /* 柱子尺寸：高 4–20px、宽 5px（原来 5–30px / 8px，整体细一圈小一圈）。 */
    if (!props.playing()) {
      return Array.from({ length: props.count }, () => 4);
    }
    return Array.from({ length: props.count }, (_, i) => {
      const value = Math.abs(Math.sin(f * 0.9 + i * 1.7));
      return 4 + Math.round(value * 16);
    });
  });

  return (
    <View class="absolute inset-0 flex-row items-end justify-center gap-[6] pb-4">
      {Array.from({ length: props.count }, (_, i) => (
        <View
          class="w-[5] rounded-md bg-white"
          style={{ height: heights()[i] }}
        />
      ))}
    </View>
  );
}

function HomePage(props: {
  track: () => Track;
  playing: () => boolean;
  position: () => number;
  percent: () => number;
  barsFrame: () => number;
  cursor: () => number;
  playbackMode: () => PlaybackMode;
  favorite: () => boolean;
}) {
  const buttonClass = (index: number, big: boolean) => {
    /* 注意：这里必须返回**完整**的类字面量。PocketJS 的样式表是按源码里
     * 出现的字面量烘焙的，`box + " scale-110"` 这种拼接在运行时拼出来的
     * 字符串匹配不到任何样式，View 就没有宽高（配合 overflow-hidden 直接
     * 把图标裁没了）——上一版就是这么把前一首/后一首/循环/爱心弄丢的。 */
    const focused = props.cursor() === index;
    if (big) {
      return focused
        ? "relative w-[44] h-[44] overflow-hidden flex-col items-center justify-center transition-transform duration-150 ease-out-back scale-110"
        : "relative w-[44] h-[44] overflow-hidden flex-col items-center justify-center transition-transform duration-150 ease-out-back scale-100";
    }
    return focused
      ? "relative w-[32] h-[32] overflow-hidden flex-col items-center justify-center transition-transform duration-150 ease-out-back scale-110"
      : "relative w-[32] h-[32] overflow-hidden flex-col items-center justify-center transition-transform duration-150 ease-out-back scale-100";
  };

  const lyricsButtonClass = () =>
    props.cursor() === 5
      ? "w-16 h-6 rounded-xl shadow-md items-center justify-center bg-orange-100 border-2 border-orange-500 transition duration-150 ease-out-back scale-110"
      : "w-16 h-6 rounded-xl shadow-md items-center justify-center bg-white border-slate-300 transition duration-150 ease-out-back scale-100";

  const progressWidth = () => {
    const duration = getTrackDuration(props.track());
    const currentPosition = Math.min(
      duration,
      Math.max(0, props.position()),
    );

    return Math.min(180, (currentPosition / duration) * 180);
  };

  /* 时间文字按秒刷新：位置信号每帧都在动，但 m:ss 只在秒变化时才需要重排。
   * （memo 的值不变就不会通知下游，比每次渲染重新拼字符串省一个刷新点。） */
  const posLabel = createMemo(() => formatMs(props.position()));
  const durLabel = createMemo(() => formatMs(getTrackDuration(props.track())));

  return (
    <View class="relative w-[368] h-48 mx-[8] rounded-xl overflow-hidden">
      <Image src={useSkin().appBg} class="absolute inset-0 w-full h-full" />

      <View class="flex-row w-[368] h-48 gap-1 relative pl-3">
        {/* COVER AREA */}
        <View class="w-[168] h-48 items-center justify-center">
          <View class="relative w-[168] h-[168]">
            <Image src={useSkin().coverGlow} class="absolute inset-0 w-[168] h-[168]" />
            <Image
              src={props.track().cover || useSkin().coverDefault}
              class="absolute inset-0 w-[168] h-[168] rounded-xl"
            />
            <Bars count={5} playing={props.playing} frame={props.barsFrame} />
          </View>
        </View>

        {/* INFO AREA — 深色磨砂玻璃面板 */}
        <View class="relative w-[184] h-48 rounded-2xl">
          <Image src={useSkin().panel} class="absolute inset-0 w-full h-full" />
          <View class="relative flex-col justify-center gap-1 w-[184] h-48 p-1">
            <StreamText class={pTxt("album")} text={clip(props.track().album, 24)} />
            <StreamText class={pTxt("title")} text={clip(props.track().title, 16)} />
            <StreamText class={pTxt("artist")} text={clip(props.track().artist, 28)} />

            {/* PROGRESS */}
            <View class="flex-col gap-0">
              <View class="w-45 h-1 rounded-md bg-slate-200 overflow-hidden">
                <View class="w-0 h-1 rounded-md bg-orange-500" style={{ width: progressWidth() }} />
              </View>
              <View class="flex-row items-center justify-between w-45">
                <Text class={pTxt("percent")}>{posLabel()}</Text>
                <Text class={pTxt("percent")}>{durLabel()}</Text>
              </View>
            </View>

            {/* PLAYER BUTTONS — 车机风格：暂停键更大 */}
            <View class="flex-row items-center justify-center gap-1">
              <View class={buttonClass(0, false)}>
                <Image src={props.cursor() === 0 ? useSkin().prevF : useSkin().prevN} class="absolute inset-0 w-full h-full" />
              </View>
              <View
                class={
                  props.playing()
                    ? (props.cursor() === 1
                        ? "relative w-[44] h-[44] overflow-hidden flex-col items-center justify-center transition-transform duration-150 ease-out-back scale-110 animate-pulse"
                        : "relative w-[44] h-[44] overflow-hidden flex-col items-center justify-center transition-transform duration-150 ease-out-back scale-100 animate-pulse")
                    : (props.cursor() === 1
                        ? "relative w-[44] h-[44] overflow-hidden flex-col items-center justify-center transition-transform duration-150 ease-out-back scale-110"
                        : "relative w-[44] h-[44] overflow-hidden flex-col items-center justify-center transition-transform duration-150 ease-out-back scale-100")
                }
              >
                <Image
                  src={
                    props.playing()
                      ? (props.cursor() === 1 ? useSkin().pauseP : useSkin().pauseN)
                      : (props.cursor() === 1 ? useSkin().playF : useSkin().playN)
                  }
                  class="absolute inset-0 w-full h-full"
                />
              </View>
              <View class={buttonClass(2, false)}>
                <Image src={props.cursor() === 2 ? useSkin().nextF : useSkin().nextN} class="absolute inset-0 w-full h-full" />
              </View>
              <View class={buttonClass(3, false)}>
                <Image
                  src={
                    props.playbackMode() === "repeat-one"
                      ? (props.cursor() === 3 ? useSkin().rep1F : useSkin().rep1N)
                      : (props.cursor() === 3 ? useSkin().shufF : useSkin().shufN)
                  }
                  class="absolute inset-0 w-full h-full"
                />
              </View>
              <View class={buttonClass(4, false)}>
                <Image
                  src={
                    props.favorite()
                      ? (props.cursor() === 4 ? useSkin().honF : useSkin().honN)
                      : (props.cursor() === 4 ? useSkin().hoffF : useSkin().hoffN)
                  }
                  class="absolute inset-0 w-full h-full"
                />
              </View>
            </View>

            {/* STATUS + LYRICS — 合并成一行，给封面/进度留空 */}
            <View class="flex-row items-center gap-2">
              <Text class={pTxt("label")}>MODE</Text>
              <Text class={pTxt("status")}>
                {props.playbackMode() === "sequence" ? "LIST" : "ONE"}
              </Text>
              <Text class={pTxt("label")}>{props.favorite() ? "LOVED" : ""}</Text>
              <View class={lyricsButtonClass()}>
                <Text
                  class={
                    props.cursor() === 5
                      ? "text-xs text-orange-600 font-bold"
                      : "text-xs text-slate-700 font-bold"
                  }
                >
                  LYRICS
                </Text>
              </View>
            </View>
          </View>
        </View>
      </View>
    </View>
  );
}

/* =========================================================
 * LYRICS PAGE
 * ======================================================= */

function LyricsPage(props: {
  track: () => Track;
  playing: () => boolean;
  lines: () => LyricLine[];
  position: () => number;
  percent: () => number;
}) {
  const LYR_LINE = 16; /* 单行歌词高度 px，用于滚动位移 */
  let listRef: NodeMirror | undefined;
  let prevIndex = 0;

  /* 当前活动歌词行下标（根据播放进度推进） */
  const active = createMemo(() => {
    const pos = props.position();
    const ls = props.lines();
    let idx = 0;
    for (let i = 0; i < ls.length; i += 1) {
      if (pos >= ls[i].time) idx = i;
    }
    return idx;
  });

  /* 滚动效果：活动行前进整块上滑一行，后退则下滑一行 */
  createEffect(() => {
    const idx = active();
    if (!listRef) return;
    if (idx === prevIndex) return;
    const dir = idx > prevIndex ? 1 : -1;
    prevIndex = idx;
    jump(listRef, "translateY", dir * LYR_LINE);
    animate(listRef, "translateY", 0, { dur: 170, easing: "out" });
  });

  /* 三行歌词拆成三个**字符串** memo：之前 windowed() 每次重算都返回新数组，
   * 数组 !== 旧数组，于是每帧都通知下游（三个 StreamText 跟着每帧重跑）。
   * 拆开之后只有"这一行真的换了"才通知。 */
  const prevLine = createMemo(() => {
    const idx = active();
    return idx > 0 ? props.lines()[idx - 1]?.text ?? "" : "";
  });
  const curLine = createMemo(() => props.lines()[active()]?.text ?? "");
  const nextLine = createMemo(() => {
    const idx = active() + 1;
    const ls = props.lines();
    return idx < ls.length ? ls[idx]?.text ?? "" : "";
  });

  /* 预取：把后面几行歌词的字形先取回来 —— 滚到下一行时不再"先缺字、后补齐"。
   * 可见的三行由 StreamText 申请，这里补 idx+2..idx+5；每次换行先释放旧的
   * 预取（别的租约还持有的字形不会被踢掉），离开歌词页/切模式时一并释放。 */
  let prefetched: TextResource[] = [];
  const dropPrefetch = () => {
    for (const r of prefetched) r.dispose();
    prefetched = [];
  };
  createEffect(() => {
    const mode = cjkMode();
    void cjkEpoch();
    const idx = active();
    const ls = props.lines();
    const slot = slotFromClass(pTxt("lyricCur"));
    const usable =
      mode === "stream" && !!cjkFont && (slot === 0 || slot === 7 || slot === 8);
    dropPrefetch();
    if (!usable || !cjkFont) return;
    for (let i = idx + 2; i <= idx + 5 && i < ls.length; i += 1) {
      const text = clip(ls[i]?.text ?? "", 44).trim();
      if (!text) continue;
      try {
        prefetched.push(cjkFont.prepareText(text, { slot }));
      } catch {
        /* 超出预算就算了，不影响正常显示 */
      }
    }
  });
  onCleanup(dropPrefetch);

  return (
    <View class="relative overflow-hidden flex-col w-96 h-48 p-2 gap-1 rounded-xl">
      <Image src={useSkin().lyricsPanel} class="absolute inset-0 w-full h-full" />
      <View class="flex-row items-center justify-between h-7">
        <View class="flex-col">
          <Text class={pTxt("aboutTitle")}>LYRICS</Text>
          <StreamText class={pTxt("lyricOther")} text={clip(props.track().title, 24)} />
        </View>

        <Text class={pTxt("percent")}>{props.percent()}%</Text>
      </View>

      <View class="flex-row items-center justify-between h-5 gap-2 overflow-hidden">
        <StreamText
          class={pTxt("detailArtist")}
          text={clip(props.track().artist, 34)}
        />

        <Text
          class={props.playing() ? pTxt("navActive") : pTxt("lyricOther")}
        >
          {props.playing() ? "PLAYING" : "PAUSED"}
        </Text>
      </View>

      <View class="flex-col items-center justify-center grow overflow-hidden">
        <View
          ref={(node: NodeMirror) => {
            listRef = node;
          }}
          style={{ translateY: 0 }}
          class="flex-col items-center gap-1 overflow-hidden"
        >
          <StreamText class={pTxt("lyricOther")} text={clip(prevLine(), 44)} />
          <StreamText class={pTxt("lyricCur")} text={clip(curLine(), 44)} />
          <StreamText class={pTxt("lyricOther")} text={clip(nextLine(), 44)} />
        </View>
      </View>

      <View class="flex-row items-center justify-between h-5">
        <Text class={pTxt("lyricOther")}> </Text>
        <Text class={pTxt("lyricOther")}>△ BACK</Text>
      </View>
    </View>
  );
}

/* =========================================================
 * MUSIC LIST
 *
 * 完全数据无关：只认 trackIds + getTrack。
 * List / Loved / Album 三处共用，谁也不依赖全局常量。
 * ======================================================= */

function MusicListPage(props: {
  title: string;
  subtitle: string;
  trackIds: string[];
  getTrack: (id: string) => Track | undefined;
  cursor: () => number;
  start: () => number;
}) {
  const visible = createMemo(() =>
    props.trackIds.slice(props.start(), props.start() + 3),
  );

  return (
    /* 这张卡片和设计稿一样是浅色的，所以标题/副标题要用「浅底版」文字色：
     * listTitle / listSub 是给专辑页那种深色背景配的（dark / anime 下是浅色
     * 字），直接拿来用在这个浅色卡片上，标题就整行看不见了。 */
    <View class="flex-col w-96 h-48 p-2 gap-1 rounded-xl bg-slate-100 border-slate-300">
      <View class="flex-row items-center justify-between h-7">
        <Text class={pTxt("listPanelTitle")}>{clip(props.title, 30)}</Text>
        <Text class={pTxt("listPanelSub")}>{props.subtitle}</Text>
      </View>

      {visible().map((trackId, localIndex) => {
        const globalPosition = props.start() + localIndex;
        const current = props.cursor() === globalPosition;
        const song = props.getTrack(trackId);

        if (!song) {
          return null;
        }

        return (
          <View
            class={
              current
                ? "relative flex-row items-center justify-between w-full h-10 overflow-hidden rounded-xl px-2 transition-transform duration-150 ease-out-back scale-105"
                : "relative flex-row items-center justify-between w-full h-10 overflow-hidden rounded-xl px-2 transition-transform duration-150 ease-out-back scale-100"
            }
          >
            <Image
              src={current ? useSkin().rowFocus : useSkin().row}
              class="absolute inset-0 w-full h-full"
            />
            <View class="flex-row items-center gap-2">
              <Text
                class={current ? pTxt("navActive") : pTxt("listIndex")}
              >
                {current ? ">" : ""}
              </Text>

              <Text class={pTxt("listIndex")}>
                {globalPosition + 1}
              </Text>

              <View class="flex-col">
                <StreamText class={pTxt("status")} text={clip(song.title, 24)} />
                <StreamText class={pTxt("detailArtist")} text={clip(song.artist, 32)} />
              </View>
            </View>

            <Text
              class={current ? pTxt("navActive") : pTxt("listSub")}
            >
              {current ? "PLAY" : ""}
            </Text>
          </View>
        );
      })}

      <View class="flex-row items-center justify-end">
        <Text class={pTxt("listPanelSub")}>
          {props.trackIds.length > 0
            ? `${props.cursor() + 1} / ${props.trackIds.length}`
            : "0 / 0"}
        </Text>
      </View>
    </View>
  );
}

/* =========================================================
 * ALBUM GRID
 * ======================================================= */

function AlbumGrid(props: {
  albums: () => Album[];
  cursor: () => number;
  start: () => number;
  covers: () => Record<string, string>;
}) {
  const [dir, setDir] = createSignal(1);
  let prevCursor = props.cursor();

  createEffect(() => {
    const current = props.cursor();
    if (current !== prevCursor) {
      setDir(current > prevCursor ? 1 : -1);
      prevCursor = current;
    }
  });

  /*
   * 这里故意只在 start 改变时重建 AlbumSlider。
   *
   * 原来的 sliderKey 同时包含 cursor，导致每按一次左右键都会把整个
   * AlbumSlider 销毁/重建，容易造成卡顿。
   *
   * 但 PocketJS 的 children/Index 对这种动态数组的更新并不可靠，完全
   * 常驻 AlbumSlider 又会出现“数字变了，但 3 张图永远不换”的问题。
   *
   * 所以采用折中方案：
   *   - cursor 改变：不重建 AlbumSlider，只刷新对应 AlbumTile
   *   - start 改变：只重建一次 AlbumSlider，让可见的 3 张专辑换组
   *   - cover 改变：由 AlbumTile 自己局部 keyed 刷新
   */
  const visibleTiles = createMemo(() =>
    props.albums()
      .slice(props.start(), props.start() + 3)
      .map((a) => ({ album: a })),
  );

  return (
    <View class="flex-col w-96 h-48">
      <View class="flex-row items-center justify-between h-7 px-2">
        <View class="flex-col">
          <Text class={pTxt("listTitle")}>ALBUMS</Text>
          <Text class={pTxt("listSub")}>YOUR MUSIC COLLECTION</Text>
        </View>
        <Text class={pTxt("listSub")}>
          {props.cursor() + 1}/{props.albums().length}
        </Text>
      </View>

      <View class="grow items-center justify-center">
        {/* 关键：key 只跟 start 绑定，不跟 cursor 绑定。 */}
        <For each={[props.start()]} keyed>
          {() => (
            <AlbumSlider
              dir={dir()}
              tiles={visibleTiles}
              cursor={props.cursor}
              start={props.start()}
              covers={props.covers}
            />
          )}
        </For>
      </View>

      <View class="flex-row items-center justify-between h-5 px-2">
        <Text class={pTxt("listSub")}>
          {props.start() + 1}-
          {Math.min(props.start() + 3, props.albums().length)}/
          {props.albums().length}
        </Text>
      </View>
    </View>
  );
}

/* =========================================================
 * ALBUM SLIDER
 * ======================================================= */

function AlbumSlider(props: {
  dir: number;
  tiles: () => { album: Album }[];
  cursor: () => number;
  start: number;
  covers: () => Record<string, string>;
}) {
  let gridRef: NodeMirror | undefined;

  /* cursor 变化时只重放滑动动画，不销毁整个 AlbumSlider。 */
  createEffect(() => {
    props.cursor();

    if (!gridRef) {
      return;
    }

    const offset = (props.dir || 1) * 24;
    jump(gridRef, "translateX", offset);
    animate(gridRef, "translateX", 0, { dur: 200, easing: "out" });
  });

  return (
    <View
      ref={(node: NodeMirror) => {
        gridRef = node;
      }}
      /* 调整专辑的3个框的上下位置，改那个11就行了 */
      style={{ translateX: props.dir * 12, translateY: 11 }}
      class="flex-row items-start justify-center w-[300]"
    >
      <Grid
        columns={3}
        gap={12}
        class="flex-row flex-wrap items-start justify-center w-[300]"
      >
        {props.tiles().map((tile, localIndex) => (
          <AlbumTile
            tile={tile}
            localIndex={localIndex}
            cursor={props.cursor}
            start={props.start}
            covers={props.covers}
          />
        ))}
      </Grid>
    </View>
  );
}

/* =========================================================
 * ALBUM TILE
 * ======================================================= */

function AlbumTile(props: {
  tile: { album: Album };
  localIndex: number;
  cursor: () => number;
  start: number;
  covers: () => Record<string, string>;
}) {
  const index = props.start + props.localIndex;
  const album = props.tile.album;

  /*
   * PocketJS 对普通 JSX children 的动态更新比较严格，所以这里用一个
   * 很小的 keyed For 包住“单张卡片内容”。
   *
   * key 只包含：
   *   - current：光标换到另一张时，只有旧/新两张卡片刷新
   *   - coverKey：异步封面上传完成时，只刷新这一张卡片
   *
   * 不会重新创建整个 AlbumSlider。
   */
  const tileKey = () => {
    const current = props.cursor() === index;
    const coverKey = props.covers()[album.id] ?? "_";
    return `${current ? "1" : "0"}|${coverKey}`;
  };

  return (
    <For each={[tileKey()]} keyed>
      {() => {
        const current = props.cursor() === index;
        const coverKey = props.covers()[album.id];

        return (
          <View class="flex-col items-center gap-2 w-[90] overflow-hidden">
            <Image
              src={coverKey || useSkin().coverDefault}
              class={
                current
                  ? "w-[82] h-[82] rounded-xl shadow-md border-2 border-red-600 transition-transform duration-150 ease-out-back scale-110"
                  : "w-[82] h-[82] rounded-xl shadow-md border-slate-300 transition-transform duration-150 ease-out-back scale-100"
              }
            />

            <StreamText
              class={
                current ? pTxt("navActive") : pTxt("status")
              }
              text={clip(album.title, 7)}
            />

            <StreamText class={pTxt("detailArtist")} text={clip(album.artist, 8)} />
          </View>
        );
      }}
    </For>
  );
}

/* =========================================================
 * ABOUT US — static text, opened from the SETTINGS page.
 * △ BACK returns to settings (handled by the host key handler).
 * ======================================================= */

function AboutPage() {
  return (
    <View class="relative overflow-hidden flex-col w-96 h-48 p-3 gap-1 rounded-xl">
      <Image src={useSkin().aboutBg} class="absolute inset-0 w-full h-full" />
      <View class="relative flex-col w-96 h-48 p-3 gap-1">
        <View class="flex-row items-center justify-between h-7">
          <Text class={pTxt("aboutTitle")}>ABOUT US</Text>
          <Text class={pTxt("aboutSub")}>YUNYIN</Text>
        </View>
        <View class="grow flex-col items-center justify-center gap-1">
          <Text class={pTxt("aboutTitle")}>YUNYIN 云音 for vita</Text>
          <Text class={pTxt("aboutSub")}>
            VER {APP_VERSION}  ·  APP {APP_VER_SFO}  ·  PJ {POCKETJS_VERSION}
          </Text>
          <Text class={pTxt("aboutSub")}>made by 阡陌</Text>
          <Text class={pTxt("aboutSub")}>致谢：PocketJS 团队 · ElevenMPV-A</Text>
          {/* 播放期间锁 PS 键 —— 这行就是给你确认锁没锁上的（LOCKED / FAIL 会亮起来） */}
          <Text class={psLockInfo() !== "PS KEY  UNLOCKED" ? pTxt("aboutTitle") : pTxt("aboutSub")}>
            {psLockInfo()}
          </Text>
        </View>
        <View class="flex-row items-center justify-center h-5">
          <Text class={pTxt("aboutSub")}>△ BACK 返回</Text>
        </View>
      </View>
    </View>
  );
}

/* =========================================================
 * KEY GUIDE —— 按键操作说明
 *
 * 设置页 KEYS 卡片进入，版式和 About 一样（静态文字 + △ 返回）。
 * ======================================================= */

const KEY_GUIDE_ROWS: [string, string][] = [
  ["← → ↑ ↓", "切换页面 / 移动光标"],
  ["○", "确认 · 播放 / 暂停"],
  ["△", "返回"],
  ["L / R", "上一首 / 下一首"],
  ["START", "关屏继续播放"],
  ["PS", "播放中锁定 · 先暂停再退出"],
];

function KeyGuidePage() {
  return (
    <View class="relative overflow-hidden flex-col w-96 h-48 p-3 gap-1 rounded-xl">
      <Image src={useSkin().aboutBg} class="absolute inset-0 w-full h-full" />
      <View class="relative flex-col w-96 h-48 p-3 gap-1">
        <View class="flex-row items-center justify-between h-7">
          <Text class={pTxt("aboutTitle")}>KEY GUIDE</Text>
          <Text class={pTxt("aboutSub")}>操作说明</Text>
        </View>
        <View class="grow flex-col items-center justify-center">
          <For each={KEY_GUIDE_ROWS}>
            {(row) => (
              <View class="flex-row items-center justify-between w-full">
                <Text class={pTxt("aboutTitle")}>{row[0]}</Text>
                <Text class={pTxt("aboutSub")}>{row[1]}</Text>
              </View>
            )}
          </For>
        </View>
        <View class="flex-row items-center justify-center h-5">
          <Text class={pTxt("aboutSub")}>△ BACK 返回</Text>
        </View>
      </View>
    </View>
  );
}

/* =========================================================
 * SETTINGS
 * ======================================================= */

function SettingPage(props: {
  cursor: () => number;
}) {
  const cardClass = (index: number) => {
    return props.cursor() === index
      ? "relative w-[82] h-[82] overflow-hidden flex-col items-center justify-center transition-transform duration-150 ease-out-back scale-110"
      : "relative w-[82] h-[82] overflow-hidden flex-col items-center justify-center transition-transform duration-150 ease-out-back scale-100";
  };

  const cardImg = (index: number) =>
    props.cursor() === index ? useSkin().cardFocus : useSkin().card;

  return (
    <View class="relative overflow-hidden flex-col w-96 h-48 p-2 gap-2 rounded-xl">
      <Image src={useSkin().settingBg} class="absolute inset-0 w-full h-full" />
      <View class="relative flex-col w-96 h-48 p-2 gap-2">
        <View class="flex-row items-center justify-between h-7">
          <View class="flex-col">
            <Text class={pTxt("setHeader")}>MUSIC SETTINGS</Text>
            <Text class={pTxt("setSub")}>SYSTEM CONFIGURATION</Text>
          </View>
          <Text class={pTxt("setSub")}>4 OPTIONS</Text>
        </View>

        <View class="flex-row items-center justify-center gap-3 grow">
          {[0, 1, 2, 3].map((i) => (
            <View class={cardClass(i)} key={i}>
              <Image src={cardImg(i)} class="absolute inset-0 w-full h-full" />
              {i === 0 && (
                <>
                  <Text class={props.cursor() === 0 ? pTxt("navActive") : pTxt("setTitle")}>CJK</Text>
                  <Text class={cjkMode() === "stream" ? pTxt("navActive") : pTxt("setVal")}>
                    {cjkCardValue()}
                  </Text>
                </>
              )}
              {i === 1 && (
                <>
                  <Text class={props.cursor() === 1 ? pTxt("navActive") : pTxt("setTitle")}>KEYS</Text>
                  <Text class={pTxt("setInfo")}>INFO</Text>
                </>
              )}
              {i === 2 && (
                <>
                  <Text class={props.cursor() === 2 ? pTxt("navActive") : pTxt("setTitle")}>ABOUT</Text>
                  <Text class={pTxt("setInfo")}>INFO</Text>
                </>
              )}
              {i === 3 && (
                <>
                  <Text class={props.cursor() === 3 ? pTxt("navActive") : pTxt("setTitle")}>THEME</Text>
                  <Text class={pTxt("setVal")}>{uiTheme() === "light" ? "LIGHT" : uiTheme() === "dark" ? "DARK" : uiTheme() === "pure" ? "PURE" : "ANIME"}</Text>
                </>
              )}
            </View>
          ))}
        </View>
      </View>
    </View>
  );
}
