/* 曲库：本地扫描 / mock 数据 / 派生表 / 歌词解析 / 在线曲目（零行为搬迁）。 */
import type { Album, FsEntry, LyricLine, Track } from "./types";
import { DEFAULT_COVER_CLS, makeAlbumId, makeTrackId, slug } from "./util";
import { AUDIO_RE, LIB_MOUNTS, collectAudio, media } from "./media";

export const getCoverClass = (coverId: string, fallbackCls?: string): string => {
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
export const buildAlbums = (tracks: Track[]): Album[] => {
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
export const buildTrackById = (tracks: Track[]): Record<string, Track> => {
  const map: Record<string, Track> = {};

  for (const song of tracks) {
    map[song.id] = song;
  }

  return map;
};

/* Album.id -> Album 的查找表 */
export const buildAlbumById = (albums: Album[]): Record<string, Album> => {
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
export const sanitizeIds = (
  ids: string[],
  byId: Record<string, unknown>,
): string[] => {
  return ids.filter((id) => byId[id] !== undefined);
};

/*
 * 获取安全的 Track duration，防止 0 / 负数 / NaN / Infinity
 * 导致进度计算异常。
 */
export const getTrackDuration = (song: Track): number => {
  const duration = song.durationMs;

  if (!Number.isFinite(duration)) {
    return 1;
  }

  return Math.max(1, duration);
};

export const parseTimestamp = (raw: string): number | null => {
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
export const parseLyrics = (raw: string | undefined, song: Track): LyricLine[] => {
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

  out.sort((left, right) => left.time - right.time);

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


/* 示范曲：从 I Will Be.mp3 的 lyrics-eng 抽出，扫描接入前作为 LRC fixture */
export const FIXTURE_LRC_I_WILL_BE = `[00:00.10]歌曲名 I Will Be
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
export const MOCK_TRACKS: Track[] = [
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

export function buildRealTrack(entry: FsEntry, index: number): Track {
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

export function scanLibrary(): Track[] {
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
export const ONLINE_ALBUM = "在线歌曲";
export const ONLINE_ARTIST = "在线";

/* 从 URL 或 `netease:<id>` 条目里挑出 song id（没有就用序号），
 * 只为了让默认名字有点辨识度。 */
export function onlineIdHint(url: string): string {
  const m = /[?&]id=(\d+)/.exec(url) || /^netease:(\d+)$/.exec(url);
  return m ? m[1] : "";
}

export function buildOnlineTrack(
  url: string,
  title: string,
  index: number,
  playlist = "",
): Track {
  const name =
    (title || "").trim() ||
    `[在线] ${onlineIdHint(url) || String(index + 1)}`;
  const artist = ONLINE_ARTIST;
  const album = ONLINE_ALBUM;
  return {
    /* 在线条目的身份是 URL 本身：同名歌曲（本地文件 / 两条在线记录）不能再撞
     * id，否则列表里两首会共用同一条"当前曲目"，点哪首放到的是另一首。 */
    id: `${makeTrackId({ artist, album, title: name, durationMs: 0 })}-${slug(url)}`,
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
    /* 所属歌单（playlist.json 里的 name；旧格式为空 → 界面上归到"在线歌曲"） */
    playlist: playlist || undefined,
  };
}

/* 读一遍在线曲目清单。宿主没有 netplay（旧版/电脑模拟）时返回空数组。 */
export function scanOnlineTracks(): Track[] {
  const api = media();
  if (!api || !api.netplay) return [];
  let list: { url?: string; title?: string; playlist?: string }[] = [];
  try {
    const parsed = JSON.parse(api.netplay() || "[]");
    list = Array.isArray(parsed) ? parsed : [];
  } catch {
    list = [];
  }
  const entries = list.filter(
    (e): e is { url: string; title?: string; playlist?: string } =>
      !!e && typeof e.url === "string" && e.url.length > 0,
  );
  /*
   * 同一首歌在 JSON 里既写了 `id` 又写了 `outer/url` 直链时，只留 `netease:` 那条 ——
   * 它走加密解析、链接过期能自动换新；两条都显示的话用户分不清点的是哪条
   * （00.89 真机日志里就是这么踩到的）。
   */
  const idEntries = new Set<string>();
  for (const e of entries) {
    const m = /^netease:(\d+)$/.exec(e.url);
    if (m) idEntries.add(m[1]);
  }
  const kept = entries.filter((e) => {
    if (/^netease:/.test(e.url)) return true;
    if (!/^https?:\/\/music\.163\.com\/song\/media\/outer\/url\?/i.test(e.url)) {
      return true;
    }
    const m = /[?&]id=(\d+)/.exec(e.url);
    return !(m && idEntries.has(m[1]));
  });
  return kept.map((e, i) =>
    buildOnlineTrack(e.url, e.title || "", i, (e.playlist || "").trim()),
  );
}

