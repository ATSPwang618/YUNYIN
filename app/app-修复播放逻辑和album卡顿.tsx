import {
  Grid,
  FocusScope,
  Image,
  Text,
  View,
  type NodeMirror,
} from "@pocketjs/framework/components";

import { animate, jump } from "@pocketjs/framework/animation";
import { registerTexture } from "@pocketjs/framework";
import { onButtonPress, onFrame } from "@pocketjs/framework/lifecycle";
import { BTN, focusNode } from "@pocketjs/framework/input";

import {
  createSignal,
  createEffect,
  createMemo,
  onMount,
  Index,
  For,
} from "solid-js";

/* =========================================================
 * NATIVE MEDIA BRIDGE (globalThis.vitaMedia)
 *
 * native/media.rs 暴露：list / roots / play / pause / resume /
 * stop / state / cover / tags。state() 返回
 * { playing, paused, path, pos, dur, rate, dec }，pos/dur 为毫秒。
 * ======================================================= */

type VitaMedia = {
  list?(path: string): string;
  roots?(): string;
  play?(path: string): void;
  pause?(): void;
  resume?(): void;
  stop?(): void;
  state?(): string;
  cover?(path: string): number;
  tags?(path: string): string;
};

const media = (): VitaMedia | undefined =>
  (globalThis as unknown as { vitaMedia?: VitaMedia }).vitaMedia;

/* 写 ux0:data/yunyin.log 的原生日志入口（与 scanLibrary 的 log 共用）。 */
const logMsg = (m: string): void => {
  try {
    (media() as unknown as { logMsg?(s: string): void })?.logMsg?.(m);
  } catch {
    /* ignore */
  }
};

/* 只扫描 ux0:/data/music。系统自带的 ux0:/music 被 Vita 的 SceIo 隐藏，
 * homebrew 打不开；而 ux0:/data 是普通可访问目录，所以把歌曲放到
 * ux0:/data/music（如 E:\data\music）即可被扫描到。不再扫整张卡。 */
const LIB_MOUNTS = ["ux0:/data/music"];

/* 音频扩展名：mp3/ogg/wav + 原生支持的 flac/opus/ogg(x)。 */
const AUDIO_RE = /\.(mp3|ogg|wav|flac|opus|oga)$/i;

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
type Theme = "INDIGO" | "EMERALD" | "AMBER" | "ROSE";

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

const MOCK_TRACKS: Track[] = [
  {
    id: "track-avril-lavigne-i-will-be-i-will-be-239726",
    title: "I Will Be",
    artist: "Avril Lavigne",
    album: "I Will Be",
    wav: "",
    audioPath: "",
    audioRef: "",
    coverId: "avril-lavigne-i-will-be",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-sky-400 to-blue-800 border-sky-300",
    durationMs: 239726,
    albumId: "avril-lavigne-i-will-be",
    lyrics: FIXTURE_LRC_I_WILL_BE,
  },

  {
    id: "track-sync-pulse-night-drive-midnight-replay-180000",
    title: "MIDNIGHT REPLAY",
    artist: "SYNC PULSE",
    album: "NIGHT DRIVE",
    wav: "midnight-replay",
    audioPath: "",
    audioRef: "midnight-replay",
    coverId: "sync-pulse-night-drive",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-blue-500 to-blue-700 border-blue-300",
    durationMs: 180000,
    albumId: "sync-pulse-night-drive",
  },
  {
    id: "track-amber-tide-glass-horizon-glass-horizon-180000",
    title: "GLASS HORIZON",
    artist: "AMBER TIDE",
    album: "GLASS HORIZON",
    wav: "glass-horizon",
    audioPath: "",
    audioRef: "glass-horizon",
    coverId: "amber-tide-glass-horizon",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-amber-400 to-amber-700 border-amber-300",
    durationMs: 180000,
    albumId: "amber-tide-glass-horizon",
  },
  {
    id: "track-neon-drifters-night-signals-static-bloom-180000",
    title: "STATIC BLOOM",
    artist: "NEON DRIFTERS",
    album: "NIGHT SIGNALS",
    wav: "static-bloom",
    audioPath: "",
    audioRef: "static-bloom",
    coverId: "neon-drifters-night-signals",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-cyan-500 to-cyan-700 border-cyan-300",
    durationMs: 180000,
    albumId: "neon-drifters-night-signals",
  },
  {
    id: "track-vector-blue-city-lights-city-lights-180000",
    title: "CITY LIGHTS",
    artist: "VECTOR BLUE",
    album: "CITY LIGHTS",
    wav: "city-lights",
    audioPath: "",
    audioRef: "city-lights",
    coverId: "vector-blue-city-lights",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-violet-500 to-violet-700 border-violet-300",
    durationMs: 180000,
    albumId: "vector-blue-city-lights",
  },
  {
    id: "track-vector-blue-city-lights-neon-rain-180000",
    title: "NEON RAIN",
    artist: "VECTOR BLUE",
    album: "CITY LIGHTS",
    wav: "neon-rain",
    audioPath: "",
    audioRef: "neon-rain",
    coverId: "vector-blue-city-lights",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-indigo-500 to-indigo-700 border-indigo-300",
    durationMs: 180000,
    albumId: "vector-blue-city-lights",
  },
  {
    id: "track-vector-blue-city-lights-after-image-180000",
    title: "AFTER IMAGE",
    artist: "VECTOR BLUE",
    album: "CITY LIGHTS",
    wav: "after-image",
    audioPath: "",
    audioRef: "after-image",
    coverId: "vector-blue-city-lights",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-purple-500 to-purple-700 border-purple-300",
    durationMs: 180000,
    albumId: "vector-blue-city-lights",
  },
  {
    id: "track-lunar-mode-silver-dust-silver-dust-180000",
    title: "SILVER DUST",
    artist: "LUNAR MODE",
    album: "SILVER DUST",
    wav: "silver-dust",
    audioPath: "",
    audioRef: "silver-dust",
    coverId: "lunar-mode-silver-dust",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-slate-400 to-slate-700 border-slate-300",
    durationMs: 180000,
    albumId: "lunar-mode-silver-dust",
  },
  {
    id: "track-lunar-mode-silver-dust-low-gravity-180000",
    title: "LOW GRAVITY",
    artist: "LUNAR MODE",
    album: "SILVER DUST",
    wav: "low-gravity",
    audioPath: "",
    audioRef: "low-gravity",
    coverId: "lunar-mode-silver-dust",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-slate-500 to-slate-800 border-slate-300",
    durationMs: 180000,
    albumId: "lunar-mode-silver-dust",
  },
  {
    id: "track-chrome-heart-midnight-run-midnight-run-180000",
    title: "MIDNIGHT RUN",
    artist: "CHROME HEART",
    album: "MIDNIGHT RUN",
    wav: "midnight-run",
    audioPath: "",
    audioRef: "midnight-run",
    coverId: "chrome-heart-midnight-run",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-rose-500 to-rose-700 border-rose-300",
    durationMs: 180000,
    albumId: "chrome-heart-midnight-run",
  },
  {
    id: "track-chrome-heart-midnight-run-black-wires-180000",
    title: "BLACK WIRES",
    artist: "CHROME HEART",
    album: "MIDNIGHT RUN",
    wav: "black-wires",
    audioPath: "",
    audioRef: "black-wires",
    coverId: "chrome-heart-midnight-run",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-zinc-600 to-zinc-900 border-zinc-300",
    durationMs: 180000,
    albumId: "chrome-heart-midnight-run",
  },
  {
    id: "track-dayframe-echo-park-echo-park-180000",
    title: "ECHO PARK",
    artist: "DAYFRAME",
    album: "ECHO PARK",
    wav: "echo-park",
    audioPath: "",
    audioRef: "echo-park",
    coverId: "dayframe-echo-park",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-emerald-500 to-emerald-700 border-emerald-300",
    durationMs: 180000,
    albumId: "dayframe-echo-park",
  },
  {
    id: "track-dayframe-echo-park-rain-window-180000",
    title: "RAIN WINDOW",
    artist: "DAYFRAME",
    album: "ECHO PARK",
    wav: "rain-window",
    audioPath: "",
    audioRef: "rain-window",
    coverId: "dayframe-echo-park",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-teal-500 to-teal-700 border-teal-300",
    durationMs: 180000,
    albumId: "dayframe-echo-park",
  },
  {
    id: "track-static-arc-orbital-orbital-180000",
    title: "ORBITAL",
    artist: "STATIC ARC",
    album: "ORBITAL",
    wav: "orbital",
    audioPath: "",
    audioRef: "orbital",
    coverId: "static-arc-orbital",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-sky-500 to-sky-700 border-sky-300",
    durationMs: 180000,
    albumId: "static-arc-orbital",
  },
  {
    id: "track-static-arc-orbital-zero-signal-180000",
    title: "ZERO SIGNAL",
    artist: "STATIC ARC",
    album: "ORBITAL",
    wav: "zero-signal",
    audioPath: "",
    audioRef: "zero-signal",
    coverId: "static-arc-orbital",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-cyan-600 to-cyan-800 border-cyan-300",
    durationMs: 180000,
    albumId: "static-arc-orbital",
  },
  {
    id: "track-polaris-last-summer-last-summer-180000",
    title: "LAST SUMMER",
    artist: "POLARIS",
    album: "LAST SUMMER",
    wav: "last-summer",
    audioPath: "",
    audioRef: "last-summer",
    coverId: "polaris-last-summer",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-yellow-400 to-orange-600 border-yellow-300",
    durationMs: 180000,
    albumId: "polaris-last-summer",
  },
  {
    id: "track-polaris-last-summer-sunset-drive-180000",
    title: "SUNSET DRIVE",
    artist: "POLARIS",
    album: "LAST SUMMER",
    wav: "sunset-drive",
    audioPath: "",
    audioRef: "sunset-drive",
    coverId: "polaris-last-summer",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-orange-400 to-red-600 border-orange-300",
    durationMs: 180000,
    albumId: "polaris-last-summer",
  },
  {
    id: "track-north-signal-deep-blue-deep-blue-180000",
    title: "DEEP BLUE",
    artist: "NORTH SIGNAL",
    album: "DEEP BLUE",
    wav: "deep-blue",
    audioPath: "",
    audioRef: "deep-blue",
    coverId: "north-signal-deep-blue",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-blue-600 to-indigo-900 border-blue-300",
    durationMs: 180000,
    albumId: "north-signal-deep-blue",
  },
  {
    id: "track-north-signal-deep-blue-cold-current-180000",
    title: "COLD CURRENT",
    artist: "NORTH SIGNAL",
    album: "DEEP BLUE",
    wav: "cold-current",
    audioPath: "",
    audioRef: "cold-current",
    coverId: "north-signal-deep-blue",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-cyan-500 to-blue-800 border-cyan-300",
    durationMs: 180000,
    albumId: "north-signal-deep-blue",
  },
  {
    id: "track-motion-arc-glass-city-glass-city-180000",
    title: "GLASS CITY",
    artist: "MOTION ARC",
    album: "GLASS CITY",
    wav: "glass-city",
    audioPath: "",
    audioRef: "glass-city",
    coverId: "motion-arc-glass-city",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-fuchsia-500 to-purple-800 border-fuchsia-300",
    durationMs: 180000,
    albumId: "motion-arc-glass-city",
  },
  {
    id: "track-motion-arc-glass-city-slow-motion-180000",
    title: "SLOW MOTION",
    artist: "MOTION ARC",
    album: "GLASS CITY",
    wav: "slow-motion",
    audioPath: "",
    audioRef: "slow-motion",
    coverId: "motion-arc-glass-city",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-pink-500 to-violet-700 border-pink-300",
    durationMs: 180000,
    albumId: "motion-arc-glass-city",
  },
  {
    id: "track-red-frame-fade-out-fade-out-180000",
    title: "FADE OUT",
    artist: "RED FRAME",
    album: "FADE OUT",
    wav: "fade-out",
    audioPath: "",
    audioRef: "fade-out",
    coverId: "red-frame-fade-out",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-red-500 to-red-800 border-red-300",
    durationMs: 180000,
    albumId: "red-frame-fade-out",
  },
  {
    id: "track-red-frame-fade-out-final-signal-180000",
    title: "FINAL SIGNAL",
    artist: "RED FRAME",
    album: "FADE OUT",
    wav: "final-signal",
    audioPath: "",
    audioRef: "final-signal",
    coverId: "red-frame-fade-out",
    coverCls:
      "w-14 h-14 rounded-xl shadow-md items-center justify-center bg-gradient-to-b from-red-600 to-black border-red-300",
    durationMs: 180000,
    albumId: "red-frame-fade-out",
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
      (media() as unknown as { logMsg?(s: string): void })?.logMsg?.(m);
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

  /* 真实扫描时 durationMs 初始为 0，makeTrackId 会把它们都归一成同一个占位值，
   * artist+album+title 完全相同的曲目会撞 id。这里做一次去重，
   * 撞到的后续曲目追加 -dup2 / -dup3 后缀，保证 id 全局唯一，
   * 同时不影响没有撞车的曲目的 id 稳定性（重命名/重扫不受影响）。 */
  const seenIds = new Map<string, number>();
  for (const t of tracks) {
    const n = (seenIds.get(t.id) || 0) + 1;
    seenIds.set(t.id, n);
    if (n > 1) {
      t.id = `${t.id}-dup${n}`;
    }
  }

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
  return tracks;
}

/* =========================================================
 * AUDIO BACKEND
 *
 * UI 仍然认 position / playing / finishTrack。
 * onFrame 只从 backend 读当前位置。
 *
 * 优先级：
 *   1) 有 audioPath 且 host 挂了 vitaMedia：走 Vita 原生解码 (MP3/OGG/WAV)
 *   2) 否则墙钟模拟
 * ======================================================= */

const audioEngine = {
  loadedPath: "",
  mode: "clock" as "clock" | "vita",
  sampleRate: 44100,
  clockOriginMs: 0,
  clockOffsetMs: 0,
  running: false,

  /* 起播时间戳。用于给 native "playing" 状态一个宽限期：
   * vm.play() 是异步起播的，调用后的头几帧 vm.state() 可能还没跟上
   * （返回上一首的残留状态或尚未开始播放），如果立刻信任它去同步
   * UI 的 playing()，会出现"刚按下播放又被打回暂停"的抖动。 */
  startedAtMs: 0,

  load(song: Track) {
    const realPath = song.audioPath || "";
    this.loadedPath = realPath;
    this.clockOffsetMs = 0;
    this.clockOriginMs = Date.now();
    this.running = false;

    const vm = media();
    this.mode =
      realPath && vm && vm.play && vm.state ? "vita" : "clock";
  },

  play() {
    this.running = true;
    this.clockOriginMs = Date.now();
    this.startedAtMs = Date.now();
    const vm = media();

    if (this.mode === "vita" && vm && vm.play) {
      try {
        vm.play(this.loadedPath);
      } catch {
        this.mode = "clock";
      }
    }
  },

  pause() {
    if (this.running) {
      this.clockOffsetMs = this.positionMs();
    }

    this.running = false;
    const vm = media();

    if (this.mode === "vita" && vm && vm.pause) {
      try {
        vm.pause();
      } catch {
        /* ignore */
      }
    }
  },

  stop() {
    this.running = false;
    this.clockOffsetMs = 0;
    this.clockOriginMs = Date.now();
    const vm = media();

    if (this.mode === "vita" && vm && vm.stop) {
      try {
        vm.stop();
      } catch {
        /* ignore */
      }
    }
  },

  pump() {
    /* vitaMedia 自行驱动；墙钟无需 pump */
  },

  snapshot(): {
    posMs: number;
    durMs: number;
    playing: boolean;
  } {
    const vm = media();
    if (this.mode === "vita" && vm && vm.state) {
      try {
        const st = JSON.parse(vm.state() || "{}") as {
          playing?: boolean;
          paused?: boolean;
          pos?: number;
          dur?: number;
        };
        return {
          posMs: Math.max(0, Number(st.pos) || 0),
          durMs: Math.max(0, Number(st.dur) || 0),
          playing: !!st.playing && !st.paused,
        };
      } catch {
        /* fall through to clock */
      }
    }

    return {
      posMs: this.clockFallback(),
      durMs: 0,
      playing: this.running,
    };
  },

  clockFallback(): number {
    if (!this.running) {
      return this.clockOffsetMs;
    }

    return this.clockOffsetMs + Math.max(0, Date.now() - this.clockOriginMs);
  },

  positionMs(): number {
    return this.snapshot().posMs;
  },
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
      class="w-96 h-48"
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

  const [listCursor, setListCursor] = createSignal(0);
  const [listStart, setListStart] = createSignal(0);

  const [albumCursor, setAlbumCursor] = createSignal(0);
  const [albumStart, setAlbumStart] = createSignal(0);

  /* 专辑封面缓存：albumId -> 已上传的封面纹理 key（懒加载，仅当前可见的 3 张） */
  const [albumCovers, setAlbumCovers] = createSignal<Record<string, string>>({});

  /* 封面加载队列：一帧只处理一个 albumId，避免连续同步 IO/解码/上传
   * 在同一帧里堆叠导致的卡顿。见 onFrame 里的消费逻辑。 */
  const [coverQueue, setCoverQueue] = createSignal<string[]>([]);

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

  /* 默认不自动播放：进入 App 先停在暂停态，用户按 ● 才开始播放。
   * 原来默认 true 会导致 onMount 里立刻 audioEngine.play()，
   * 配合 durationMs=0 的真实曲目会被瞬间判定"播放完毕"，
   * 表现为"进去好像就自动放了一首"。 */
  const [playing, setPlaying] = createSignal(false);

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
  const [favorites, setFavorites] = createSignal<string[]>([]);

  const [sfx, setSfx] = createSignal(true);
  const [vibration, setVibration] = createSignal(false);
  const [brightness, setBrightness] = createSignal(3);
  const [theme, setTheme] = createSignal<Theme>("INDIGO");

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
    if (!cur || cur.cover || !cur.audioPath) return;
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

  /* 当前可见专辑需要补封面时，只把 albumId 排进队列；
   * 真正的 IO/解码/上传工作挪到 onFrame 里一帧处理一个（见下方 onFrame）。
   * 这样连续切换/翻页多张专辑，不会在同一帧里堆叠多次同步开销。 */
  createEffect(() => {
    if (screen() !== "album" || selectedAlbumId() !== null) return;
    const api = media();
    if (!api || !api.cover) return;

    const visible = albums().slice(albumStart(), albumStart() + 3);
    const covers = albumCovers();
    const queued = coverQueue();

    const need = visible
      .map((a) => a.id)
      .filter((id) => !covers[id] && !queued.includes(id));

    if (need.length > 0) {
      setCoverQueue((prev) => [...prev, ...need]);
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
    const current = theme();

    if (current === "EMERALD") {
      return "flex-col w-full h-full p-2 gap-2 bg-gradient-to-b from-emerald-50 to-slate-100";
    }

    if (current === "AMBER") {
      return "flex-col w-full h-full p-2 gap-2 bg-gradient-to-b from-amber-50 to-slate-100";
    }

    if (current === "ROSE") {
      return "flex-col w-full h-full p-2 gap-2 bg-gradient-to-b from-rose-50 to-slate-100";
    }

    return "flex-col w-full h-full p-2 gap-2 bg-gradient-to-b from-indigo-50 to-slate-100";
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
      setCurrentTrackId(realTracks[0].id);
      setQueueIds(realTracks.map((song) => song.id));
    }

    focusNav();
    audioEngine.load(track());

    /* 不再默认自动播放；playing 初始为 false，这里只做 load，
     * 等用户主动按 ● 才会调用 togglePlay() -> audioEngine.play()。 */
  });

  /* =======================================================
   * RESET
   * ======================================================= */

  const resetHome = () => {
    setHomeCursor(1);
    setLyricsVisible(false);
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

  /*
   * 一首歌自然播放结束时调用（由 onFrame 里的进度/状态检测触发）。
   *
   * - repeat-one：重新加载当前曲目从头播放。
   * - sequence：顺序播放下一首；如果当前已经是队列最后一首，
   *   停止播放而不是绕回第一首，避免整张专辑放完后静默循环。
   *   如果你想要的是"放完自动回到第一首循环"，把最后一块
   *   "最后一首" 的分支删掉，直接用 jumpInQueue(1) 即可。
   */
  const finishTrack = () => {
    const list = activeQueueIds();

    if (list.length === 0) {
      setPlaying(false);
      return;
    }

    if (playbackMode() === "repeat-one") {
      const current = track();
      audioEngine.stop();
      audioEngine.load(current);
      setPosition(0);
      setBarsFrame(0);
      setPlaying(true);
      audioEngine.play();
      return;
    }

    const currentIndex = list.indexOf(currentTrackId());

    if (currentIndex === -1) {
      setPlaying(false);
      return;
    }

    if (currentIndex >= list.length - 1) {
      /* 顺序模式播完最后一首：停止，而不是静默绕回第一首 */
      audioEngine.stop();
      setPosition(getTrackDuration(track()));
      setPlaying(false);
      return;
    }

    const nextId = list[currentIndex + 1];

    if (!nextId) {
      setPlaying(false);
      return;
    }

    startTrack(nextId);
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

    if (currentFavorites.includes(currentId)) {
      setFavorites(currentFavorites.filter((id) => id !== currentId));
      return;
    }

    setFavorites([...currentFavorites, currentId]);
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
      setSfx(!sfx());
      return;
    }

    if (index === 1) {
      setVibration(!vibration());
      return;
    }

    if (index === 2) {
      setBrightness(brightness() >= 5 ? 1 : brightness() + 1);
      return;
    }

    const themes: Theme[] = ["INDIGO", "EMERALD", "AMBER", "ROSE"];
    const current = themes.indexOf(theme());
    setTheme(themes[(current + 1) % themes.length]);
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
        return;
      }

      return;
    }

    if (screen() === "setting") {
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
  });

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
  });

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
      moveSettingRight();
      return;
    }

    moveListRight();
  });

  /* =======================================================
   * BUTTON: CIRCLE
   * ======================================================= */

  onButtonPress(BTN.CIRCLE, () => {
    if (focusZone() === "nav") {
      openSelectedNav();
      return;
    }

    activateContent();
  });

  /* =======================================================
   * BUTTON: TRIANGLE
   * ======================================================= */

  onButtonPress(BTN.TRIANGLE, () => {
    if (focusZone() === "nav") {
      return;
    }

    if (screen() === "home" && lyricsVisible()) {
      setLyricsVisible(false);
      setHomeCursor(5);
      focusContent();
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
  });

  /* =======================================================
   * OPTIMIZED FRAME LOOP
   * ======================================================= */

  onFrame(() => {
    audioEngine.pump();

    /* -----------------------------------------------------
     * 封面加载队列：每帧最多处理一个 albumId。
     * 把同步的 IO/解码/纹理上传开销分摊到多帧里，
     * 避免进入 Album 页或翻页时一次性处理 3 张封面导致的卡顿。
     * ----------------------------------------------------- */
    const queue = coverQueue();
    if (queue.length > 0) {
      const id = queue[0];
      setCoverQueue((prev) => prev.slice(1));

      if (!albumCovers()[id]) {
        const api = media();
        const targetAlbum = albumById()[id];
        const firstTrackId = targetAlbum?.trackIds[0];
        const first = firstTrackId ? trackById()[firstTrackId] : undefined;

        if (api && api.cover && first && first.audioPath) {
          let h = -1;
          try {
            h = api.cover(first.audioPath);
          } catch {
            /* fallback to gradient */
          }
          if (typeof h === "number" && h >= 0) {
            const key = "emb:" + first.audioPath;
            registerTexture(key, h);
            setAlbumCovers((prev) => ({ ...prev, [id]: key }));
          }
        }
      }
    }

    const snap = audioEngine.snapshot();
    const currentTrack = track();

    /* 原生返回真实时长后，把当前曲目的 durationMs 补一次，
     * 这样 UI 的进度/总时长用真实值（避免真实曲库初始 0 的问题）。 */
    if (snap.durMs > 0 && currentTrack.id) {
      if (currentTrack.durationMs !== snap.durMs) {
        setTracks((prev) =>
          prev.map((t) =>
            t.id === currentTrack.id ? { ...t, durationMs: snap.durMs } : t,
          ),
        );
      }
    }

    /* -----------------------------------------------------
     * NATIVE PLAY STATE -> UI PLAY STATE
     *
     * vm.play() 是异步起播的，刚调用完的头几帧 vm.state() 可能还没
     * 跟上（残留上一首状态 / 尚未真正开始），所以起播后 300ms 内
     * 不信任 native 汇报的 playing=false，避免"刚按播放又跳回暂停"。
     * 过了宽限期之后，只要 native 说没在播（正常播完 / 异常停止），
     * UI 就必须跟着变成暂停，这样图标不会卡死在"播放中"的状态。
     * ----------------------------------------------------- */
    if (audioEngine.mode === "vita") {
      const pastGracePeriod = Date.now() - audioEngine.startedAtMs > 300;
      if (pastGracePeriod && playing() !== snap.playing) {
        setPlaying(snap.playing);
      }
    }

    /* -----------------------------------------------------
     * 是否播放结束的判断。
     *
     * 只信任 > 1000ms 的真实时长，绝不使用 getTrackDuration() 的
     * 1ms 兜底值 —— 真实曲库刚扫描出来 durationMs 是 0，如果拿 0
     * 走 Math.max(1, 0) 会变成 1ms，导致刚开始播放就被误判"放完"，
     * 表现为"歌曲刚放一下就自动跳下一首"。
     * ----------------------------------------------------- */
    const trustedDuration =
      currentTrack.durationMs > 1000
        ? currentTrack.durationMs
        : snap.durMs > 1000
          ? snap.durMs
          : 0;

    if (
      trustedDuration > 0 &&
      audioEngine.mode === "vita" &&
      snap.posMs >= trustedDuration - 100
    ) {
      finishTrack();
      return;
    }

    if (!playing()) {
      return;
    }

    if (snap.posMs !== position()) {
      setPosition(snap.posMs);
    }

    /* 律动条不需要 60fps：每两帧才推进一次动画相位，宿主写操作直接减半。 */
    barsFrameTick += 1;
    if ((barsFrameTick & 1) === 0) {
      setBarsFrame((value) => value + 1);
    }
  });

  /* =======================================================
   * ROOT
   * ======================================================= */

  return (
    <View debugName="MusicScreen" class={rootClass()}>
      {/* HEADER */}

      <View class="flex-row items-center justify-between h-6">
        <View class="flex-row items-center gap-1">
          <Text class="text-xs text-blue-600 font-bold">
            云音 for vita
          </Text>
        </View>

        <Text class="text-xs text-slate-500">
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

            {screen() === "home" && !lyricsVisible() && (
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

            {screen() === "home" && lyricsVisible() && (
              <PageEnter dir={1}>
                <LyricsPage
                  track={track}
                  playing={playing}
                  lines={lyrics}
                  position={position}
                  percent={percent}
                />
              </PageEnter>
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
                <SettingPage
                  cursor={settingCursor}
                  sfx={sfx}
                  vibration={vibration}
                  brightness={brightness}
                  theme={theme}
                />
              </PageEnter>
            )}
          </View>
        </FocusScope>
      </View>

      {/* FOOTER */}

      <View class="flex-row items-center justify-between">
      <Text class="text-xs text-slate-500 font-bold">↑↓ MOVE</Text>
      <Text class="text-xs text-slate-500 font-bold">←→ SELECT</Text>
      <Text class="text-xs text-slate-500 font-bold">● OK</Text>
      <Text class="text-xs text-slate-500 font-bold">▲ BACK</Text>
      <Text class="text-xs text-slate-500 font-bold">L/R PAGE</Text>
      </View>
    </View>
  );
}

/* =========================================================
 * NAV ITEM
 * ======================================================= */

function NavItem(props: {
  label: string;
  index: number;
  cursor(): number;
  active: boolean;
  refNode(node: NodeMirror): void;
}) {
  const isCursor = () => props.cursor() === props.index;

  return (
    <View
      ref={props.refNode}
      focusable
      class={
        isCursor()
          ? "w-14 h-9 rounded-xl shadow-md items-center justify-center bg-red-100 border-2 border-red-600"
          : props.active
            ? "w-14 h-9 rounded-xl shadow-md items-center justify-center bg-red-50 border-red-300"
            : "w-14 h-9 rounded-xl shadow-md items-center justify-center bg-white border-slate-300"
      }
    >
      <Text
        class={
          isCursor()
            ? "text-xs text-red-700 font-bold"
            : props.active
              ? "text-xs text-red-600 font-bold"
              : "text-xs text-slate-600 font-bold"
        }
      >
        {props.label}
      </Text>
    </View>
  );
}

/* =========================================================
 * HOME PAGE
 * ======================================================= */

function Bars(props: {
  count: number;
  playing(): boolean;
  frame(): number;
}) {
  /* 把 5 个高度合到一个 memo：frame/playing 一变才重算，且每次只对变化的 bar 写宿主，
   * 避免 5 个独立响应式节点各自重算。 */
  const heights = createMemo(() => {
    const f = props.frame();
    if (!props.playing()) {
      return Array.from({ length: props.count }, () => 5);
    }
    return Array.from({ length: props.count }, (_, i) => {
      const value = Math.abs(Math.sin(f * 0.9 + i * 1.7));
      return 5 + Math.round(value * 25);
    });
  });

  return (
    <View class="absolute inset-0 flex-row items-end justify-center gap-2 pb-4">
      {Array.from({ length: props.count }, (_, i) => (
        <View
          class="w-2 rounded-md bg-white"
          style={{ height: heights()[i] }}
        />
      ))}
    </View>
  );
}

function HomePage(props: {
  track(): Track;
  playing(): boolean;
  position(): number;
  percent(): number;
  barsFrame(): number;
  cursor(): number;
  playbackMode(): PlaybackMode;
  favorite(): boolean;
}) {
  const buttonClass = (index: number, round: boolean) => {
    if (round) {
      return props.cursor() === index
        ? "w-[35] h-[35] rounded-full shadow-md items-center justify-center bg-red-50 border-2 border-red-600"
        : "w-[35] h-[35] rounded-full shadow-md items-center justify-center bg-white border-slate-300";
    }

    return props.cursor() === index
      ? "w-[35] h-[35] rounded-xl shadow-md items-center justify-center bg-red-50 border-2 border-red-600"
      : "w-[35] h-[35] rounded-xl shadow-md items-center justify-center bg-white border-slate-300";
  };

  const lyricsButtonClass = () =>
    props.cursor() === 5
      ? "w-20 h-7 rounded-xl shadow-md items-center justify-center bg-red-50 border-2 border-red-600"
      : "w-20 h-7 rounded-xl shadow-md items-center justify-center bg-white border-slate-300";

  const progressWidth = () => {
    const duration = getTrackDuration(props.track());
    const currentPosition = Math.min(
      duration,
      Math.max(0, props.position()),
    );

    return Math.min(180, (currentPosition / duration) * 180);
  };

  return (
    <View class="flex-row w-96 h-48 gap-2">
      {/* COVER AREA */}

      <View class="w-44 h-48 items-center justify-center">
        <View class="relative w-[168] h-[168]">
          {props.track().cover ? (
            <Image
              src={props.track().cover}
              class="absolute inset-0 w-[168] h-[168] rounded-xl"
            />
          ) : (
            <View
              class={getCoverClass(props.track().coverId, props.track().coverCls)}
              style={{ width: 168, height: 168 }}
            />
          )}
          <Bars count={5} playing={props.playing} frame={props.barsFrame} />
        </View>
      </View>

      {/* INFO AREA */}

      <View class="flex-col justify-center gap-2 w-48 h-48">
        <Text class="text-xs text-blue-500">{clip(props.track().album, 24)}</Text>
        <Text class="text-xl text-slate-950 font-bold">
          {clip(props.track().title, 18)}
        </Text>
        <Text class="text-xs text-slate-500">{clip(props.track().artist, 30)}</Text>

        {/* PROGRESS */}

        <View class="flex-row items-center gap-1">
          <View class="w-45 h-2 rounded-md bg-slate-200 overflow-hidden">
            <View
              class="w-0 h-2 rounded-md bg-blue-600"
              style={{ width: progressWidth() }}
            />
          </View>

          <Text class="text-xs text-slate-400">{props.percent()}%</Text>
        </View>

        {/* PLAYER BUTTONS */}

        <View class="flex-row items-center gap-1">
          <View class={buttonClass(0, true)}>
            <Text class="text-xs text-slate-700 font-bold">{"|◀"}</Text>
          </View>

          <View class={buttonClass(1, true)}>
            <Text class="text-lg text-slate-950 font-bold">
              {props.playing() ? "Ⅱ" : "▶"}
            </Text>
          </View>

          <View class={buttonClass(2, true)}>
            <Text class="text-xs text-slate-700 font-bold">{"▶|"}</Text>
          </View>

          <View class={buttonClass(3, false)}>
            <Text
              class={
                props.playbackMode() === "repeat-one"
                  ? "text-xs text-red-600 font-bold"
                  : "text-xs text-slate-700 font-bold"
              }
            >
              {props.playbackMode() === "repeat-one" ? "ONE" : "SEQ"}
            </Text>
          </View>

          <View class={buttonClass(4, false)}>
            <Text
              class={
                props.favorite()
                  ? "text-lg text-red-600 font-bold"
                  : "text-lg text-slate-700 font-bold"
              }
            >
              {props.favorite() ? "♥" : "♡"}
            </Text>
          </View>
        </View>

        {/* STATUS */}

        <View class="flex-row items-center gap-2">
          <Text class="text-xs text-slate-500">MODE</Text>
          <Text class="text-xs text-slate-950 font-bold">
            {props.playbackMode() === "sequence" ? "LIST" : "ONE"}
          </Text>
          <Text class="text-xs text-slate-500">
            {props.favorite() ? "LOVED" : ""}
          </Text>
        </View>

        {/* LYRICS BUTTON */}

        <View class="flex-row items-center gap-2">
          <Text class="text-xs text-slate-500">VIEW</Text>

          <View class={lyricsButtonClass()}>
            <Text
              class={
                props.cursor() === 5
                  ? "text-xs text-red-700 font-bold"
                  : "text-xs text-slate-700 font-bold"
              }
            >
              LYRICS
            </Text>
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
  track(): Track;
  playing(): boolean;
  lines(): LyricLine[];
  position(): number;
  percent(): number;
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

  const windowed = createMemo(() => {
    const idx = active();
    const ls = props.lines();
    const prev = idx - 1 >= 0 ? ls[idx - 1]?.text ?? "" : "";
    const cur = ls[idx]?.text ?? "";
    const next = idx + 1 < ls.length ? ls[idx + 1]?.text ?? "" : "";
    return [prev, cur, next];
  });

  return (
    <View class="flex-col w-96 h-48 p-2 gap-1 rounded-xl bg-slate-100 border-slate-300">
      <View class="flex-row items-center justify-between h-7">
        <View class="flex-col">
          <Text class="text-sm text-slate-950 font-bold">LYRICS</Text>
          <Text class="text-xs text-slate-400">{clip(props.track().title, 24)}</Text>
        </View>

        <Text class="text-xs text-slate-400">{props.percent()}%</Text>
      </View>

      <View class="flex-row items-center justify-between h-5 gap-2 overflow-hidden">
        <Text class="text-xs text-blue-500">
          {clip(props.track().artist, 34)}
        </Text>

        <Text
          class={
            props.playing()
              ? "text-xs text-red-500 font-bold"
              : "text-xs text-slate-400 font-bold"
          }
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
          <Text class="text-xs text-slate-400">{clip(windowed()[0], 44)}</Text>
          <Text class="text-sm text-red-600 font-bold">
            {clip(windowed()[1], 44)}
          </Text>
          <Text class="text-xs text-slate-400">{clip(windowed()[2], 44)}</Text>
        </View>
      </View>

      <View class="flex-row items-center justify-between h-5">
        <Text class="text-xs text-slate-400"> </Text>
        <Text class="text-xs text-slate-400">△ BACK</Text>
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
  getTrack(id: string): Track | undefined;
  cursor(): number;
  start(): number;
}) {
  const visible = createMemo(() =>
    props.trackIds.slice(props.start(), props.start() + 3),
  );

  return (
    <View class="flex-col w-96 h-48 p-2 gap-1 rounded-xl bg-slate-100 border-slate-300">
      <View class="flex-row items-center justify-between h-7">
        <Text class="text-sm text-slate-950 font-bold">{clip(props.title, 30)}</Text>
        <Text class="text-xs text-slate-400">{props.subtitle}</Text>
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
                ? "flex-row items-center justify-between w-full h-10 rounded-xl px-2 bg-red-50 border-red-600"
                : "flex-row items-center justify-between w-full h-10 rounded-xl px-2 bg-white border-slate-300"
            }
          >
            <View class="flex-row items-center gap-2">
              <Text
                class={
                  current
                    ? "text-xs text-red-600 font-bold"
                    : "text-xs text-slate-300"
                }
              >
                {current ? ">" : ""}
              </Text>

              <Text class="text-xs text-slate-400">
                {globalPosition + 1}
              </Text>

              <View class="flex-col">
                <Text class="text-xs text-slate-950 font-bold">
                  {clip(song.title, 24)}
                </Text>
                <Text class="text-xs text-slate-500">{clip(song.artist, 32)}</Text>
              </View>
            </View>

            <Text
              class={
                current
                  ? "text-xs text-red-600 font-bold"
                  : "text-xs text-slate-400"
              }
            >
              {current ? "PLAY" : ""}
            </Text>
          </View>
        );
      })}

      <View class="flex-row items-center justify-end">
        <Text class="text-xs text-slate-400">
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
  albums(): Album[];
  cursor(): number;
  start(): number;
  covers(): Record<string, string>;
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

  /* 把"可见的 3 张专辑 + 各自封面"合并成一个响应式数组：
   * 封面 signal 一更新，这个 memo 会重算并产出新的数组引用，
   * <Index each=...> 内部会按下标 diff、只更新真正变化的那张卡片，
   * 不需要靠外层重建整个 AlbumSlider 来刷新封面。 */
  const visibleTiles = createMemo(() =>
    props.albums()
      .slice(props.start(), props.start() + 3)
      .map((a) => ({ album: a, coverKey: props.covers()[a.id] })),
  );

  /*
   * 注意：这里不再用 sliderKey 包一层 <For keyed> 强制销毁重建
   * AlbumSlider。之前的写法是：
   *
   *   const sliderKey = () => `${start}:${cursor}:${coverStamp}`;
   *   <For each={[sliderKey()]} keyed>{() => <AlbumSlider .../>}</For>
   *
   * cursor 每变化一次（哪怕还在同一页 3 张专辑内左右移动），整个
   * <Grid> + 3 张 <Index> 卡片 + <Image> 纹理节点都会被整体卸载再
   * 重新创建 —— 这是 Album 页最大的卡顿来源。
   *
   * AlbumSlider 内部的 createEffect 已经会在 cursor 变化时自己重放
   * 滑动动画，完全不需要外层重建整棵树来"触发"动画。直接常驻
   * AlbumSlider，只让 props（tiles/cursor/start/dir）随信号更新即可。
   */
  return (
    <View class="flex-col w-96 h-48">
      <View class="flex-row items-center justify-between h-7 px-2">
        <View class="flex-col">
          <Text class="text-sm text-slate-950 font-bold">ALBUMS</Text>
          <Text class="text-xs text-slate-400">YOUR MUSIC COLLECTION</Text>
        </View>

        <Text class="text-xs text-slate-400">
          {props.cursor() + 1}/{props.albums().length}
        </Text>
      </View>

      <View class="grow items-center justify-center">
        <AlbumSlider
          dir={dir()}
          tiles={visibleTiles}
          cursor={props.cursor}
          start={props.start()}
        />
      </View>

      <View class="flex-row items-center justify-between h-5 px-2">
        <Text class="text-xs text-slate-400">
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
  tiles(): { album: Album; coverKey: string | undefined }[];
  cursor(): number;
  start: number;
}) {
  let gridRef: NodeMirror | undefined;

  /* 每次光标变化（无论是否翻到新的一页）都把整个滑块先弹回反方向偏移，
   * 再滑回原位：让左右切换有明显的"一个一个换"的感觉。
   * 这个组件现在长期挂载（不再被外层强制销毁重建），所以这里的
   * createEffect 是唯一驱动滑动动画的地方，行为和之前完全一致。 */
  createEffect(() => {
    /* 读一次 cursor 订阅变化，光标每动一次就重放一次滑动动画。 */
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
    gridRef = node;}}
    /* 调整专辑的3个框的上下位置，改那个11就行了*/
    style={{translateX: props.dir * 12,translateY: 11,}}
    class="flex-row items-start justify-center w-[300]">
      <Grid
        columns={3}
        gap={12}
        class="flex-row flex-wrap items-start justify-center w-[300]"
      >
        <Index each={props.tiles()}>
          {(item, localIndex) => {
          const tile = item();
          const album = tile.album;
          const index = props.start + localIndex;
          const current = props.cursor() === index;
          const coverKey = tile.coverKey;

          return (
            <View class="flex-col items-center gap-2 w-[90] overflow-hidden">
              {coverKey ? (
                <Image
                  src={coverKey}
                  class={
                    current
                      ? "w-[82] h-[82] rounded-xl shadow-md border-2 border-red-600"
                      : "w-[82] h-[82] rounded-xl shadow-md border-slate-300"
                  }
                />
              ) : (
                <View
                  class={
                    current
                      ? "w-[82] h-[82] rounded-xl shadow-md items-center justify-center bg-red-50 border-2 border-red-600"
                      : "w-[82] h-[82] rounded-xl shadow-md items-center justify-center bg-white border-slate-300"
                  }
                  style={{ width: 80, height: 80 }}
                >
                  <Text
                    class={
                      current
                        ? "text-lg text-red-700 font-bold"
                        : "text-lg text-slate-500 font-bold"
                    }
                  >
                    {index + 1}
                  </Text>
                </View>
              )}

              <Text
                class={
                  current
                    ? "text-xs text-red-600 font-bold"
                    : "text-xs text-slate-900 font-bold"
                }
              >
                {clip(album.title, 7)}
              </Text>

              <Text class="text-xs text-slate-700">{clip(album.artist, 8)}</Text>
            </View>
          );
          }}
        </Index>
      </Grid>
    </View>
  );
}

/* =========================================================
 * SETTINGS
 * ======================================================= */

function SettingPage(props: {
  cursor(): number;
  sfx(): boolean;
  vibration(): boolean;
  brightness(): number;
  theme(): Theme;
}) {
  const cardClass = (index: number) => {
    if (props.cursor() === index) {
      return "flex-col items-center justify-center w-[82] h-[82] rounded-xl shadow-md bg-red-50 border-2 border-red-600";
    }

    return "flex-col items-center justify-center w-[82] h-[82] rounded-xl shadow-md bg-white border-slate-300";
  };

  return (
    <View class="flex-col w-96 h-48 p-2 gap-2 rounded-xl bg-slate-100 border-slate-300">
      <View class="flex-row items-center justify-between h-7">
        <View class="flex-col">
          <Text class="text-sm text-slate-950 font-bold">
            MUSIC SETTINGS
          </Text>
          <Text class="text-xs text-slate-400">SYSTEM CONFIGURATION</Text>
        </View>

        <Text class="text-xs text-slate-400">4 OPTIONS</Text>
      </View>

      <View class="flex-row items-center justify-center gap-3 grow">
        <View class={cardClass(0)}>
          <Text
            class={
              props.cursor() === 0
                ? "text-xs text-red-700 font-bold"
                : "text-xs text-slate-700 font-bold"
            }
          >
            SFX
          </Text>
          <Text
            class={
              props.sfx()
                ? "text-xs text-red-600 font-bold"
                : "text-xs text-slate-400 font-bold"
            }
          >
            {props.sfx() ? "ON" : "OFF"}
          </Text>
        </View>

        <View class={cardClass(1)}>
          <Text
            class={
              props.cursor() === 1
                ? "text-xs text-red-700 font-bold"
                : "text-xs text-slate-700 font-bold"
            }
          >
            VIB
          </Text>
          <Text
            class={
              props.vibration()
                ? "text-xs text-red-600 font-bold"
                : "text-xs text-slate-400 font-bold"
            }
          >
            {props.vibration() ? "ON" : "OFF"}
          </Text>
        </View>

        <View class={cardClass(2)}>
          <Text
            class={
              props.cursor() === 2
                ? "text-xs text-red-700 font-bold"
                : "text-xs text-slate-700 font-bold"
            }
          >
            BR
          </Text>
          <Text class="text-xs text-slate-500 font-bold">
            {props.brightness()}
          </Text>
        </View>

        <View class={cardClass(3)}>
          <Text
            class={
              props.cursor() === 3
                ? "text-xs text-red-700 font-bold"
                : "text-xs text-slate-700 font-bold"
            }
          >
            THEME
          </Text>
          <Text class="text-xs text-slate-500 font-bold">
            {props.theme()}
          </Text>
        </View>
      </View>
    </View>
  );
}
