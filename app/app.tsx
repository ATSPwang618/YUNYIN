import { View } from "@pocketjs/framework/components";
import { createSignal, createMemo, createEffect, onMount, Show } from "solid-js";

import { registerTexture } from "@pocketjs/framework";
import { onButtonPress, onFrame } from "@pocketjs/framework/lifecycle";
import { BTN } from "@pocketjs/framework/input";

import ThemeSeed from "./theme-seed";

import type { PlaybackMode, Track } from "./core/types";
import {
  MOCK_TRACKS,
  buildAlbumById,
  buildAlbums,
  buildOnlineTrack,
  buildTrackById,
  getTrackDuration,
  onlineIdHint,
  parseLyrics,
  sanitizeIds,
  scanLibrary,
} from "./core/library";
import { audioEngine } from "./core/audio";
import { logEnabled, logMsg, media } from "./core/media";
import {
  catalogIds,
  catalogMenu,
  catalogPage,
  catalogVersion,
  type CatalogPage,
  type CatalogPlaylist,
  type CatalogSong,
} from "./core/catalog";
import { bgCls, nextTheme, setUiTheme } from "./core/theme";
import {
  applyCjkMode,
  beginCjkWarmup,
  cjkEpoch,
  cjkMode,
  logCjkStats,
  pumpCjkPrepare,
  type CjkWarmupTicket,
} from "./core/cjk";
import { setPsLockInfo } from "./core/ui-state";
import { perfFrame, perfFrameBegin, perfFrameEnd, perfSpan } from "./core/perf";
import type { NodeMirror } from "@pocketjs/framework/renderer";
import { animate, jump } from "@pocketjs/framework/animation";
import { MOTION, MotionHandle } from "./core/motion";

import { TabBar, TAB_LABELS } from "./components/tabbar";
import { CTRL_ORDER, PlayerPanel } from "./components/player";
import { SubPage } from "./components/subpage";

import { MenuList, type MenuRowData } from "./pages/menu";
import { TrackListPage, LIST_WINDOW } from "./pages/tracks";
import { AlbumListPage } from "./pages/albums";
import { PlaceholderPage } from "./pages/placeholder";
import { SettingPage } from "./pages/settings";
import { KeyGuidePage } from "./pages/keys";
import { AboutPage } from "./pages/about";
import { AccountPage, type LoginSnapshot } from "./pages/account";
import { LyricsPage } from "./pages/lyrics";
import { pumpQrTexture } from "./core/qr";

/* =========================================================
 * YUNYIN —— 流媒体播放器外壳
 *
 * 布局：左播放器常驻 + 右内容（四页签 + 子页）。
 * 按键：↑↓ 列表 / ←→ 页签或进播放器 / ○ 确认 / △ 返回 / L·R 切歌 / START 息屏。
 * ======================================================= */

type Sub =
  | null
  | "local"
  | "online"
  | "favorites"
  | "favoritesOnline"
  | "albums"
  | "album"
  | "playlist"
  | "settings"
  | "keys"
  | "about"
  | "account"
  | "lyrics";

const PLAYER_LAST = 5; /* 播放器 6 个焦点位：0..5 */
/* 默认落在"我的"页签（打开应用就是它）。 */
const MINE_TAB = 3;

/* 榜单：网易云的排行榜本身就是歌单，id 固定（与原生 lists.rs 里的一致）。 */
const TOPLISTS: { id: string; name: string }[] = [
  { id: "3778678", name: "热歌榜" },
  { id: "19723756", name: "飙升榜" },
  { id: "3779629", name: "新歌榜" },
  { id: "2884035", name: "原创榜" },
];

/* list/ 文件里的歌曲节点（现在由 Rust catalog bridge 提供）。 */
type ListSong = CatalogSong;

/* 空位占位：进应用还没选歌时播放器停在这里（只有从列表点歌才开始放）。 */
const EMPTY_TRACK: Track = {
  id: "",
  title: "未选择歌曲",
  artist: "从列表里选一首开始",
  album: "",
  wav: "",
  audioPath: "",
  audioRef: "",
  coverId: "",
  coverCls: "",
  durationMs: 0,
  albumId: "",
  lyrics: "",
};

export default function Music() {
  /* ---------------- 数据源 ---------------- */
  const [tracks, setTracks] = createSignal<Track[]>(MOCK_TRACKS);
  /* 专辑只收**本地**曲目：在线歌走「歌单 / 在线歌曲」入口，
   * 绝不混进本地专辑页（以前在线条目会凑出一个"在线歌曲"专辑）。 */
  const albums = createMemo(() =>
    buildAlbums(tracks().filter((song) => !song.online)),
  );
  const trackById = createMemo(() => buildTrackById(tracks()));
  const albumById = createMemo(() => buildAlbumById(albums()));

  /* ---------------- 播放状态 ---------------- */
  const [currentTrackId, setCurrentTrackId] = createSignal<string>("");
  const [playing, setPlaying] = createSignal(false);
  const [displayOff, setDisplayOff] = createSignal(false);
  const [position, setPosition] = createSignal(0);
  const [playbackMode, setPlaybackMode] = createSignal<PlaybackMode>("sequence");
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
  const [queueIds, setQueueIds] = createSignal<string[]>(
    MOCK_TRACKS.map((song) => song.id),
  );

  /* ---------------- 界面状态 ---------------- */
  const [tabIndex, setTabIndex] = createSignal(MINE_TAB);

  /* ---------------- 动效（任务书 §3/§4） ---------------- */
  let playerRef: NodeMirror | undefined;
  let contentRef: NodeMirror | undefined;
  const contentAnim = new MotionHandle();
  let lastTabForMotion = -1;

  /*
   * 启动分层进入（§3）：左栏从左侧 -12px 淡入（220ms），
   * 右栏稍晚 30ms 从 +12px 淡入（240ms）。只做一次，不进帧循环。
   */
  onMount(() => {
    if (playerRef) {
      jump(playerRef, "translateX", -12);
      jump(playerRef, "opacity", 0);
      animate(playerRef, "translateX", 0, { dur: 220, easing: "out" });
      animate(playerRef, "opacity", 1, { dur: 220, easing: "out" });
    }
    if (contentRef) {
      jump(contentRef, "translateX", 12);
      jump(contentRef, "opacity", 0);
      animate(contentRef, "translateX", 0, { dur: 240, easing: "out", delay: 30 });
      animate(contentRef, "opacity", 1, { dur: 240, easing: "out", delay: 30 });
    }
  });

  /*
   * Tab 切换（§4）：**两段式** —— 旧页先滑出淡出（160ms），换页，
   * 新页再从反方向滑入淡入（280ms）。位移只有 8px（480 宽屏幕上已经很明显）。
   *
   * 为什么不能"一次 animate 同时管进出"：内容是被框架整体替换的，
   * 旧页在新页挂上那一刻就没了 —— 想让它"退场"必须**推迟换页**。
   * 这里用一个一次性的截止时间（帧循环里检查），不引入 setTimeout，
   * 也不做每帧动画（动画仍然全部交给 native animate）。
   */
  let tabPending: { next: number; untilMs: number; startedMs: number } | null = null;
  /*
   * 内容列是不是正处在"退场后"的状态。
   *
   * 为什么要有这个标志：第一段退场会把内容 animate 到 opacity 0，第二段才换页 + 进场。
   * 一旦第二段因为任何原因没跑（真机反馈：**切页时右侧整块空白** —— 原因就是我把它
   * 放进了"正在播放"才执行的代码块里），内容就永远停在看不见的状态。
   * 现在用这个标志做兜底：只要"已经退场"却没有待处理切换，就把内容复位并记一行日志。
   */
  let contentFaded = false;

  /** 切 Tab 的唯一入口（按键/点标签都走这里）。 */
  const switchTab = (next: number) => {
    if (next < 0 || next >= TAB_LABELS.length) return;
    if (next === tabIndex()) return;
    if (!contentRef) {
      setTabIndex(next);
      return;
    }
    if (tabPending) {
      /* 上一次还没退完：直接把目标换成最新的（连按 Tab 不会排队抖动）。 */
      tabPending.next = next;
      return;
    }
    const forward = next > tabIndex();
    lastTabForMotion = tabIndex();
    contentAnim.cancel();
    contentFaded = true;
    logMsg(
      `perf: Tab 退场 → ${TAB_LABELS[next]}（exit ${MOTION.tabExit}ms，offset ${MOTION.tabOffset}px）`,
    );
    /* 第一段：旧页朝"离开方向"滑出 + 淡出。 */
    animate(contentRef, "translateX", forward ? -MOTION.tabOffset : MOTION.tabOffset, {
      dur: MOTION.tabExit,
      easing: "out",
    });
    animate(contentRef, "opacity", 0, { dur: MOTION.tabExit, easing: "out" });
    const now = Date.now();
    tabPending = { next, untilMs: now + MOTION.tabExit, startedMs: now };
  };

  /**
   * 帧循环里每帧问一次（**放在没有 early return 的位置**）：
   *   1. 退场到点了 → 换页 + 播进场；
   *   2. 兜底：已经退场却没有待处理切换（被别的路径打断）→ 把内容复位。
   */
  const pumpTabTransition = () => {
    if (!tabPending) {
      if (contentFaded && contentRef) {
        contentFaded = false;
        contentAnim.cancel();
        logMsg("perf: Tab 动画兜底复位（退场后没有待处理切换，内容重新可见）");
        jump(contentRef, "translateX", 0);
        animate(contentRef, "opacity", 1, { dur: MOTION.fast, easing: "out" });
      }
      return;
    }
    if (Date.now() < tabPending.untilMs) return;
    const { next, startedMs } = tabPending;
    tabPending = null;
    contentFaded = false;
    const forward = next > lastTabForMotion;
    setTabIndex(next);
    const exitTook = Date.now() - startedMs;
    logMsg(
      `perf: Tab 进场 ← ${TAB_LABELS[next]}（退场实际 ${exitTook}ms / 预期 ${MOTION.tabExit}ms，enter ${MOTION.tabEnter}ms）` +
        (exitTook > MOTION.tabExit + 120
          ? " ← 退场被拖慢：帧循环受阻，查上一帧的慢帧账本"
          : ""),
    );
    if (!contentRef) return;
    contentAnim.cancel();
    /* 第二段：新页从**反方向**滑入（先瞬移到位，再补间回 0 —— 只跑一次 animate）。 */
    jump(contentRef, "translateX", forward ? MOTION.tabOffset : -MOTION.tabOffset);
    jump(contentRef, "opacity", 0);
    animate(contentRef, "translateX", 0, { dur: MOTION.tabEnter, easing: "out" });
    animate(contentRef, "opacity", 1, { dur: MOTION.tabEnter, easing: "out" });
  };
  const [tabFocus, setTabFocus] = createSignal(false);
  const [sub, setSub] = createSignal<Sub>(null);
  const [playlistName, setPlaylistName] = createSignal("");
  const [selectedAlbumId, setSelectedAlbumId] = createSignal<string | null>(null);
  const [zone, setZone] = createSignal<"player" | "content">("content");
  const [playerCursor, setPlayerCursor] = createSignal(2); /* 2 = 播放/暂停 */
  const [cursor, setCursor] = createSignal(0);
  const [start, setStart] = createSignal(0);
  /* 子页顶部那个 ← 也是光标能落到的一格（↑ 从第一行进入，○ 返回）。 */
  const [headerFocus, setHeaderFocus] = createSignal(false);

  let frameCounter = 0;
  let lastFrameMs = 0;
  let cjkPrepareFrame = 0;
  let cjkDbgFrames = 0;

  /* ---------------- 登录 ---------------- */
  const [loginSnapshot, setLoginSnapshot] = createSignal<LoginSnapshot>({
    state: "idle",
    loggedIn: false,
    message: "",
    url: "",
  });
  const [rememberLogin, setRememberLogin] = createSignal(false);
  let lastLoginRaw = "";

  const refreshLogin = () => {
    const api = media();
    api?.netLoginTick?.();
    try {
      const raw = api?.netLoginState?.() || "";
      if (raw && raw !== "{}" && raw !== lastLoginRaw) {
        lastLoginRaw = raw;
        setLoginSnapshot(JSON.parse(raw) as LoginSnapshot);
      }
    } catch {
      /* 解析失败就当没发生 */
    }
  };

  const toggleRememberLogin = () => {
    const next = !rememberLogin();
    setRememberLogin(next);
    media()?.netLoginRemember?.(next ? 1 : 0);
  };

/* 在线歌"按下播放 → 出声"的等待起点：播放器面板据此显示 缓冲中 / 无网络。 */
  const [onlineWaitFrom, setOnlineWaitFrom] = createSignal(0);
  const [netHint, setNetHint] = createSignal("");
  /* 点了一下灰色（下架）歌曲时的一行提示，2 秒后自己消失。 */
  const [offNote, setOffNote] = createSignal("");
  let offNoteAt = 0;
  /* 在线打开彻底失败时的一行提示（保留到下一次点歌）。 */
  const [failNote, setFailNote] = createSignal("");
  /* 在线歌已缓冲比例（0..1）——进度条里那根浅色条。 */
  const [buffered, setBuffered] = createSignal(0);
  /* 清单子页"已经同步了几秒"（打开时置零，帧循环每秒递增）。 */
  const [syncElapsed, setSyncElapsed] = createSignal(0);
  /*
   * 同步进度文本（"3/7"）。原生侧只报"当前在跑的那件事"，
   * 只有 >1 步时才显示数字 —— 单步任务显示 "0/1" 反而像坏了。
   */
  const [syncProg, setSyncProg] = createSignal("");
  const syncProgText = () => (syncProg() ? ` ${syncProg()}` : "");
  let listOpenAt = 0;
  /* "账号歌单同步开始时刻" + 迟到标记：登录了却迟迟没有
   * account_playlists.json 时，界面要写"同步失败"，不能永远"同步中…"。
   * 基准是**这次同步开始的时间**（登录成功 / 刚启动），不是应用启动时间 ——
   * 否则"先玩三分钟再登录"会立刻被判成同步失败。 */
  let accountSyncSinceAt = Date.now();
  const [accountSyncLate, setAccountSyncLate] = createSignal(false);
  let lastAccountTraceStage = -1;
  let lastNativeErr = "";
  const [onlineInfoFilled, setOnlineInfoFilled] = createSignal<Set<string>>(new Set());
  /*
   * `nc:<网易云 id>` → 歌名/歌手/专辑（`netSongsInfo` 补回来的）。
   *
   * 为什么单独存一份：曲库里那份只覆盖"已经在曲库里的在线歌"；而**收藏的在线歌**
   * 是 `nc:` 条目、不进曲库，`getTrack()` 原先只能从"当前打开的那份清单"取元数据 ——
   * 所以收藏页看它们永远是空的（真机反馈：计数 3 首、列表 0/0）。
   * 现在收藏页也走这份表，补齐后就能正常成行。
   */
  const [cloudInfo, setCloudInfo] = createSignal<
    Record<string, { title?: string; artists?: string; album?: string; durationMs?: number }>
  >({});
  /* 当前打开的那个子页对应的清单：id 是网易云的歌单/榜单 id（本地清单为空），
   * file 是它在 list/ 下的文件名（先读文件，文件里没有才让原生去拉）。 */
  const [cloudOpen, setCloudOpen] = createSignal<{
    id: string;
    name: string;
    file: string;
    state: string;
    message: string;
  }>({ id: "", name: "", file: "", state: "idle", message: "" });
  /*
   * 在线目录由 Rust catalog worker 所有：Rust 后台解析整份 JSON，JS 只拿
   * 菜单摘要或当前可见窗口。这里的 signal 只保存小结果，不保存原始文档。
   */
  const [catalogDiscover, setCatalogDiscover] = createSignal<CatalogPlaylist[]>([]);
  const [catalogCharts, setCatalogCharts] = createSignal<CatalogPlaylist[]>([]);
  const [catalogDailyCount, setCatalogDailyCount] = createSignal(0);
  const [catalogAccount, setCatalogAccount] = createSignal<CatalogPlaylist[]>([]);
  const [catalogPages, setCatalogPages] = createSignal<Record<string, CatalogPage>>({});
  const [catalogQueues, setCatalogQueues] = createSignal<Record<string, string[]>>({});
  /* page 只是一扇窗口；歌曲元数据要按 id 合并保存，不能跟着当前窗口丢掉。 */
  const [catalogMeta, setCatalogMeta] = createSignal<Record<string, CatalogSong>>({});
  const CATALOG_FETCH_LIMIT = LIST_WINDOW + 2; /* 覆盖 Recycler 槽位的预加载行 */
  let lastCatalogVersion = "";
  const catalogPageRequests: Record<string, string> = {};
  const catalogIdsQueued: Record<string, boolean> = {};
  let deferredCatalogPage: { file: string; offset: number; dueFrame: number } | undefined;
  let catalogTrace = 0;
  let playlistWarmup: CjkWarmupTicket | undefined;
  let playlistWarmupKey = "";
  let playlistFontPrimedFile = "";
  let playlistFontPrimedEpoch = -1;
  const [playlistFontReady, setPlaylistFontReady] = createSignal(true);

  const mergeCatalogMeta = (file: string, songs: CatalogSong[]): void => {
    let changed = 0;
    setCatalogMeta((prev) => {
      let next = prev;
      for (const song of songs) {
        if (!song?.id) continue;
        const old = prev[song.id];
        if (
          old?.title === song.title && old?.artists === song.artists &&
          old?.album === song.album && old?.durationMs === song.durationMs &&
          old?.off === song.off && old?.vip === song.vip && old?.fee === song.fee
        ) continue;
        if (next === prev) next = { ...prev };
        next[song.id] = song;
        changed += 1;
      }
      return next;
    });
    if (changed > 0 && logEnabled()) {
      logMsg(`perf: list_meta_merge file=${file} rows=${songs.length} changed=${changed}`);
    }
  };

  const pullCatalog = () => {
    const version = catalogVersion();
    if (version === lastCatalogVersion) return;
    lastCatalogVersion = version;
    const discover = catalogMenu("discover");
    const charts = catalogMenu("charts");
    const account = catalogMenu("account");
    const accountRows = account.playlists ?? account.list ?? [];
    logMsg(
      `ui: catalog_pull version=${version} discover=${discover.state}/${discover.playlists?.length ?? 0} ` +
        `charts=${charts.state}/${charts.charts?.length ?? 0} ` +
        `account=${account.state}/${accountRows.length} list=${account.list?.length ?? 0} playlists=${account.playlists?.length ?? 0}`,
    );
    if (discover.state === "ready") setCatalogDiscover(discover.playlists ?? []);
    if (charts.state === "ready") {
      setCatalogCharts(charts.charts ?? []);
      setCatalogDailyCount(charts.dailyCount ?? 0);
    }
    if (account.state === "ready") {
      /* Native account menus historically used `list`; normalize both names. */
      setCatalogAccount(accountRows);
    }
    const open = cloudOpen();
    if (open.file) queueCatalogPage(open.file, start(), 1);
  };

  const queueCatalogPage = (file: string, offset: number, delayFrames = 1): void => {
    if (!file) return;
    deferredCatalogPage = { file, offset, dueFrame: frameCounter + Math.max(1, delayFrames) };
  };

  const queueCatalogIds = (file: string): void => {
    if (!file || catalogQueues()[file] || catalogIdsQueued[file]) return;
    catalogIdsQueued[file] = true;
  };

  const pumpCatalogIds = (): void => {
    const file = Object.keys(catalogIdsQueued).find((name) => catalogIdsQueued[name]);
    if (!file) return;
    delete catalogIdsQueued[file];
    const idsT0 = Date.now();
    const ids = perfSpan(`catalogIds ${file}`, () => catalogIds(file));
    if (logEnabled()) {
      logMsg(`perf: list_ids file=${file} count=${ids.length} bridge_ms=${Date.now() - idsT0} deferred=1`);
    }
    if (ids.length > 0) setCatalogQueues((prev) => ({ ...prev, [file]: ids }));
  };

  const loadCatalogPage = (
    file: string,
    offset: number,
    immediate = false,
  ): CatalogPage | undefined => {
    if (!file) return undefined;
    const key = `${file}:${offset}`;
    const current = catalogPages()[file];
    if (!immediate && current?.state === "ready" && current.offset === offset) return current;
    /* state=loading 只是 native worker 当时还没 publish；不能把这个 key
     * 当成永久完成。文件随后 publish_ok 后必须允许再次桥接读取。 */
    if (!immediate && catalogPageRequests[file] === key && current?.state === "ready") return current;
    catalogPageRequests[file] = key;
    const trace = ++catalogTrace;
    const t0 = Date.now();
    if (logEnabled()) {
      logMsg(
        `perf: list_page_begin trace=${trace} file=${file} offset=${offset} ` +
          `limit=${CATALOG_FETCH_LIMIT} immediate=${immediate ? 1 : 0}`,
      );
    }
    const page = perfSpan(
      `catalogPage ${file}@${offset}`,
      () => catalogPage(file, offset, CATALOG_FETCH_LIMIT),
    );
    const pageMs = Date.now() - t0;
    if (logEnabled()) {
      logMsg(
        `perf: list_page_end trace=${trace} file=${file} offset=${offset} ` +
          `state=${page.state} total=${page.total} songs=${page.songs.length} ` +
          `bridge_ms=${pageMs} name_len=${page.name.length}`,
      );
    }
    if (page.state === "ready") {
      mergeCatalogMeta(file, page.songs);
      setCatalogPages((prev) => ({ ...prev, [file]: page }));
      /* 全量 IDs 只用于后续队列/播放导航，不应和首屏 page 共用一次同步桥接。 */
      queueCatalogIds(file);
    }
    return page;
  };

  const pageData = (file: string): CatalogPage | undefined => catalogPages()[file];

  /* 歌单首屏的字体门闩：只预热当前可见窗口和标题，等 pending leases
   * 变成 ready/error 后再揭示 Recycler。这样冷启动的 prepare 不会和
   * TrackListPage 的第一次 drawlist 构建叠在一起。 */
  createEffect(() => {
    const mode = cjkMode();
    const epoch = cjkEpoch();
    const currentSub = sub();
    const open = cloudOpen();
    const file = currentSub === "playlist" ? open.file : "";
    const page = file ? pageData(file) : undefined;
    if (mode !== "stream" || !file || page?.state !== "ready") {
      playlistWarmup?.dispose();
      playlistWarmup = undefined;
      playlistWarmupKey = "";
      if (!playlistFontReady()) setPlaylistFontReady(true);
      /* 子页卸载后 TrackListPage 的 lease 会归零，菜单文本可能把这些
       * 资源从共享 LRU 中挤掉。不能只在切换字库模式时清 primed 标记，
       * 否则再次进入同一歌单会误以为字体仍在缓存，直接挂载所有 item，
       * 然后让它们逐个显示“加载中”并触发一串刷新。重新进入歌单时
       * 必须重新检查当前可见窗口；命中缓存时这一步是同步 ready 的。 */
      playlistFontPrimedFile = "";
      playlistFontPrimedEpoch = -1;
      return;
    }
    /* 同一份歌单已经完成过首次字体准备：后续分页不再替换整个 list，
     * 只让新进入的 item 自己显示 loading。 */
    if (playlistFontPrimedFile === file && playlistFontPrimedEpoch === epoch) {
      playlistWarmup?.dispose();
      playlistWarmup = undefined;
      playlistWarmupKey = "";
      if (!playlistFontReady()) setPlaylistFontReady(true);
      return;
    }
    /* 理论上可从非第一页开始，但那也属于分页语义，不能卡住整个列表。 */
    if (page.offset !== 0) {
      playlistWarmup?.dispose();
      playlistWarmup = undefined;
      playlistWarmupKey = "";
      if (!playlistFontReady()) setPlaylistFontReady(true);
      return;
    }
    const items = [
      { text: open.name || page.name, slot: 7 },
      ...page.songs.flatMap((song) => [
        { text: song.title || "", slot: 0 },
        { text: song.artists || "", slot: 0 },
      ]),
    ].filter((item) => item.text.length > 0);
    const key = `${file}:${page.offset}:${items.map((item) => `${item.slot}:${item.text}`).join("|")}`;
    if (key === playlistWarmupKey) return;
    playlistWarmup?.dispose();
    playlistWarmupKey = key;
    playlistWarmup = beginCjkWarmup(items);
    const ready = playlistWarmup.ready();
    setPlaylistFontReady(ready);
    if (logEnabled()) {
      logMsg(`perf: cjk_warmup file=${file} offset=${page.offset} items=${items.length} pending=${playlistWarmup.pending()} ready=${ready ? 1 : 0}`);
    }
  });

  /*
   * ---------------- 云端条目的"按需物化" ----------------
   *
   * 网易云清单（歌单 / 榜单 / 每日推荐）里的歌**不再一次性并进曲库**：
   * 清单文件里的 `songs[]` 就是数据源，只有"正在播的那首 + 屏幕上看得见的那几行"
   * 才临时造一个 Track 对象。1000 首的歌单因此在 JS 侧只占几行对象的开销 ——
   * 打开大歌单不再卡那一下（流畅度的关键）。
   *
   * 这些临时条目的 id 是 `nc:<网易云 id>`：队列、收藏、时长覆盖都按这个 id 走。
   */
  const cloudId = (nid: string) => "nc:" + nid;
  const cloudPlaceholder = "加载中…";
  const isCloudPlaceholder = (track: Track | undefined): boolean =>
    !!track && track.title === cloudPlaceholder;
  /* 时长覆盖表：懒加载的条目不在曲库里，原生报回的真实时长只能记在这。 */
  const [cloudDur, setCloudDur] = createSignal<Record<string, number>>({});

  /* page 返回的歌曲元数据已经在 loadCatalogPage() 中按网易云 id 合并到
   * catalogMeta；这里 O(1) 查全局小索引，不再只认当前 6/8 条窗口。
   * 这样 Recycler 槽位预加载到下一页时，也不会因为 page 切换丢失歌名。 */
  const cloudMetaFor = (id: string): ListSong | undefined => {
    if (!id.startsWith("nc:")) return undefined;
    return catalogMeta()[id.slice(3)];
  };

  const buildCloudTrack = (id: string, meta: ListSong): Track => {
    const base = buildOnlineTrack(`netease:${meta.id}`, meta.title || "", 0);
    return {
      ...base,
      id,
      title: meta.title || base.title,
      artist: meta.artists || base.artist,
      album: meta.album || base.album,
      durationMs: cloudDur()[id] || meta.durationMs || base.durationMs,
      cloud: true,
      off: !!meta.off,
      vip: !!meta.vip,
      fee: meta.fee ?? 0,
    };
  };

  /*
   * 懒加载条目的"保命缓存"。
   *
   * 为什么需要：`cloudMeta` 只认**当前打开的那份清单**。用户切到别的歌单后，
   * 正在播的那首 `nc:<id>` 就解析不出来了 —— 播放器退回"未选择歌曲"，而歌声还在响
   * （真机反馈的原话）。所以凡是用过的懒加载条目都留一份，正在播的那首永远解析得到。
   *
   * 为什么不是信号：它只是查找兜底，改它不该触发重渲染（每帧写信号就是新的卡顿）。
   * 为什么有上限：缓存不能无界，只留最近用过的 128 条。
   */
  const lazyKeep: Record<string, Track> = {};
  const lazyOrder: string[] = [];
  const keepLazy = (t: Track) => {
    if (!t.id.startsWith("nc:")) return;
    const old = lazyKeep[t.id];
    if (old && !isCloudPlaceholder(old)) return;
    if (old) {
      /* 允许真实元数据替换之前的占位对象，但不重复占用 LRU 名额。 */
      lazyKeep[t.id] = t;
      return;
    }
    lazyKeep[t.id] = t;
    lazyOrder.push(t.id);
    while (lazyOrder.length > 128) {
      const old = lazyOrder.shift();
      if (old) delete lazyKeep[old];
    }
  };

  /* ---------------- 派生 ---------------- */
  const getTrack = (id: string): Track | undefined => {
    const hit = trackById()[id];
    if (hit) return hit;
    const kept = lazyKeep[id];
    const meta = cloudMetaFor(id);
    const nid = id.startsWith("nc:") ? id.slice(3) : "";
    const info = nid ? cloudInfo()[nid] : undefined;
    if (meta || info) {
      if (kept && !isCloudPlaceholder(kept)) return kept;
      const built = buildCloudTrack(id, meta ?? {
        id: nid,
        title: info?.title || "",
        artists: info?.artists || "",
        album: info?.album || "",
        durationMs: info?.durationMs || 0,
      });
      keepLazy(built);
      return built;
    }
    if (kept) return kept;
    /*
     * 最后一道：`nc:<id>` 的元数据可能来自"补回来的在线信息"（收藏页就是这条路）。
     * 补到之前先给一首**占位行**（歌名写 id），这样列表不会空着 —— 用户能看见
     * 自己收了几首、也能点开；`netSongsInfo` 回来后歌名会自动替换。
     */
    if (id.startsWith("nc:")) {
      const nid = id.slice(3);
      const info = cloudInfo()[nid];
      const built = buildCloudTrack(id, {
        id: nid,
        title: cloudPlaceholder,
        artists: info?.artists || "",
        album: info?.album || "",
        durationMs: info?.durationMs || 0,
        off: 0,
        vip: 0,
        fee: 0,
      });
      return built;
    }
    return undefined;
  };
  /* 播放器只认这三处：曲库 → 保命缓存 → 当前清单文件。
   * 顺序很要紧：切歌单时前两者仍然让"正在播的那首"解析得到。 */
  const currentTrack = (): Track | undefined => {
    const id = currentTrackId();
    if (!id) return undefined;
    return trackById()[id] ?? lazyKeep[id] ?? getTrack(id);
  };
  const hasTrack = createMemo(() => !!currentTrack());
  const track = createMemo((): Track => {
    void cloudDur(); /* 时长回填 / 覆盖表更新后要重新取一遍 */
    return currentTrack() ?? EMPTY_TRACK;
  });
  const album = createMemo(() => {
    const id = selectedAlbumId();
    return id === null ? null : albumById()[id] ?? null;
  });
  const lyrics = createMemo(() => parseLyrics(track().lyrics, track()));
  const isFavorite = createMemo(() => favorites().includes(track().id));
  /* 队列里往前/往后找第一首**能播**的歌（下架的歌只跳过，不缩小清单本身，
   * 这样播放器右下角的 "N / M" 和列表里的序号始终对得上）。 */
  const stepPlayable = (list: string[], from: number, delta: number): string => {
    const n = list.length;
    if (n === 0) return "";
    for (let k = 1; k <= n; k++) {
      const idx = ((from + delta * k) % n + n) % n;
      const id = list[idx];
      const song = id ? getTrack(id) : undefined;
      if (song && !song.off && !song.vip) return id;
    }
    return "";
  };

  const percent = createMemo(() => {
    const duration = getTrackDuration(track());
    const pos = Math.min(duration, Math.max(0, position()));
    return Math.min(100, Math.round((pos / duration) * 100));
  });

  const localIds = createMemo(() =>
    tracks().filter((t) => !t.online).map((t) => t.id),
  );
  /* "在线歌曲"只认卡里 playlist.json 描述的那些：点开歌单/榜单临时并进来的
   * 歌（cloud）不算它的数量，也不混进它的列表。 */
  const onlineIds = createMemo(() =>
    tracks().filter((t) => t.online && !t.cloud).map((t) => t.id),
  );

  /* playlist.json 里的歌单（按清单出现顺序，带每张的数量）。 */
  const playlists = createMemo<{ name: string; count: number }[]>(() => {
    const counts = new Map<string, number>();
    for (const t of tracks()) {
      if (t.online && t.playlist) {
        counts.set(t.playlist, (counts.get(t.playlist) ?? 0) + 1);
      }
    }
    return [...counts.entries()].map(([name, count]) => ({ name, count }));
  });

  /* ------------------------------------------------------------------
   * 四个页签的行：全部由这里组装，页面只负责画（MenuList）。
   * 数据来源：list/ 里的 JSON 文件（原生后台刷）+ 本地状态。
   * ------------------------------------------------------------------ */
  let lastRowsPerfTrace = "";
  const rowsFor = createMemo<MenuRowData[]>(() => {
    const t = tabIndex();
    const rowsStartedAt = Date.now();
    const finishRows = (rows: MenuRowData[]): MenuRowData[] => {
      const trace = `${t}:${rows.length}:${rows.map((row) => `${row.kind}/${row.name}`).join("|")}`;
      if (trace !== lastRowsPerfTrace && logEnabled()) {
        lastRowsPerfTrace = trace;
        logMsg(
          `perf: menu_rows tab=${t} rows=${rows.length} build_ms=${Date.now() - rowsStartedAt} ` +
            `names=${rows.map((row) => row.name.slice(0, 20)).join("|")}`,
        );
      }
      return rows;
    };

    /* 发现：热门推荐（推荐歌单），点进去就是那张歌单 */
    if (t === 0) {
      const rows: MenuRowData[] = [];
      const list = catalogDiscover();
      if (list.length === 0) {
        rows.push({ kind: "card", name: "热门推荐", value: `同步中…${syncProgText()}` });
      }
      for (const p of list) {
        if (!p?.id || !p.name) continue;
        rows.push({
          kind: "item",
          name: p.name,
          value: `${p.count ?? 0} 首`,
          id: String(p.id),
          file: `playlist_${p.id}.json`,
        });
      }
      return finishRows(rows);
    }

    /* 榜单：每日推荐 + 热歌榜/飙升榜/新歌榜/原创榜 */
    if (t === 1) {
      const rows: MenuRowData[] = [];
      rows.push({
        kind: "card",
        name: "每日推荐",
        value:
          loginSnapshot().loggedIn && catalogDailyCount()
            ? `${catalogDailyCount()} 首`
            : "登录后同步",
        file: "daily.json",
      });
      for (const top of TOPLISTS) {
        const f = catalogCharts().find((chart) => chart.id === top.id);
        rows.push({
          kind: "item",
          name: f?.name || top.name,
          value: f?.count ? `${f.count} 首` : `同步中…${syncProgText()}`,
          id: top.id,
          file: `playlist_${top.id}.json`,
        });
      }
      return finishRows(rows);
    }

    /* 歌单：在线歌曲（playlist.json 的全部）→ 我的歌单（账号）→ 我喜欢的 */
    if (t === 2) {
      /* account_playlists.json can be left on the card from a previous
       * session. Never expose that snapshot as the current account while
       * logged out; the native worker will refresh it after login. */
      const clouds = loginSnapshot().loggedIn ? catalogAccount() : [];
      const cloudNames = new Set(clouds.map((p) => p.name));

      /* 「在线歌曲」那张卡片去掉了：它只是把卡里 playlist.json 的歌摊平一遍，
       * 和下面的歌单分组重复，用户反馈没用。 */
      const rows: MenuRowData[] = [];
      /* playlist.json 里的歌单（本地清单，没有 id，直接按曲库分组）。
       * 名字和网易云账号歌单重名的跳过 —— 那是同一张，下面按在线歌单列出即可。 */
      for (const p of playlists()) {
        if (cloudNames.has(p.name)) continue;
        rows.push({
          kind: "item",
          name: p.name,
          value: `${p.count} 首`,
          local: true,
        });
      }
      if (clouds.length === 0) {
        rows.push({
          kind: "hint",
          name: "网易云歌单",
          value: loginSnapshot().loggedIn
            ? (accountSyncLate() ? "同步失败 · 看日志" : `同步中…${syncProgText()}`)
            : "登录后同步",
        });
      }
      for (const p of clouds) {
        if (!p?.id || !p.name) continue;
        rows.push({
          kind: "item",
          name: p.name,
          value: `网易云 · ${p.count ?? 0} 首`,
          id: String(p.id),
          file: `playlist_${p.id}.json`,
        });
      }
      return finishRows(rows);
    }

    /* 我的：固定几行 */
    const favLocal = favorites().filter((id) => !id.startsWith("nc:") && !!getTrack(id));
    const favOnline = favorites().filter((id) => id.startsWith("nc:"));
    return finishRows([
      { kind: "item", name: "账号", value: loginSnapshot().loggedIn ? "已登录" : "未登录" },
      { kind: "item", name: "我喜欢的（本地）", value: `${favLocal.length} 首` },
      { kind: "item", name: "我喜欢的（在线）", value: `${favOnline.length} 首` },
      { kind: "hint", name: "最近播放", value: "即将接入" },
      { kind: "item", name: "本地音乐", value: `${localIds().length} 首` },
      { kind: "item", name: "专辑", value: `${albums().length} 张` },
      { kind: "item", name: "设置", value: "" },
    ]);
  });

  /* 当前子页的列表数据（本地/在线/收藏/专辑详情）。 */
  /*
   * 列表长度与"第 i 条是谁"：**窗口化**取用，不再整表物化。
   *
   * 为什么：大歌单（527 首）以前每次清单更新都会重建整份 id 数组
   * （527 次字符串拼接 + 数组扩容），再叠上整表 map 的元数据 ——
   * 真机日志里 `JS 帧 1021ms` 就是它。现在一屏 6 行，只算这 6 条：
   * 歌单直接按索引从文件里取（O(1)），不再预生成任何数组。
   */
  const listCount = createMemo<number>(() => {
    const s = sub();
    if (s === "local") return localIds().length;
    if (s === "online") return onlineIds().length;
    if (s === "favorites") {
      return favorites().filter((id) => !id.startsWith("nc:") && !!getTrack(id)).length;
    }
    if (s === "favoritesOnline") {
      return favorites().filter((id) => id.startsWith("nc:")).length;
    }
    if (s === "album") return album()?.trackIds.length ?? 0;
    if (s === "playlist") {
      const file = cloudOpen().file;
      const ids = file ? catalogQueues()[file] : undefined;
      /* catalogIds 已由 Rust 侧一次解析并缓存；列表按索引直接取，
       * 不再因为滚动去查当前 page 或扫描旧的曲库数组。 */
      if (ids?.length) return ids.length;
      const page = file ? pageData(file) : undefined;
      if (page?.state === "ready") return page.total;
      const name = playlistName();
      return onlineIds().filter((id) => trackById()[id]?.playlist === name).length;
    }
    return 0;
  });

  const listIdAt = (i: number): string => {
    const s = sub();
    if (s === "local") return localIds()[i] ?? "";
    if (s === "online") return onlineIds()[i] ?? "";
    if (s === "favorites") {
      return favorites().filter((id) => !id.startsWith("nc:") && !!getTrack(id))[i] ?? "";
    }
    if (s === "favoritesOnline") {
      return favorites().filter((id) => id.startsWith("nc:"))[i] ?? "";
    }
    if (s === "album") return album()?.trackIds[i] ?? "";
    if (s === "playlist") {
      const file = cloudOpen().file;
      const ids = file ? catalogQueues()[file] : undefined;
      if (ids?.[i]) return cloudId(ids[i]);
      const page = file ? pageData(file) : undefined;
      if (page?.state === "ready" && i >= page.offset && i < page.offset + page.songs.length) {
        const sid = page.songs[i - page.offset]?.id;
        if (sid) return cloudId(sid);
      }
      const name = playlistName();
      return onlineIds().filter((id) => trackById()[id]?.playlist === name)[i] ?? "";
    }
    return "";
  };

  const listIds = createMemo<string[]>(() => {
    const s = sub();
    if (s === "local") return localIds();
    if (s === "online") return onlineIds();
    /* 收藏里可能有懒加载的云端条目（nc:…），不能只按曲库过滤。 */
    /*
     * 「我喜欢的」不能只按曲库过滤：收藏里的在线歌是 `nc:<网易云 id>`，
     * 它们**不在本地曲库里**，而 `getTrack()` 只认"当前打开的那份清单"。
     * 以前这行直接把收藏的在线歌全滤掉 —— 计数写 3 首、打开却是 0/0（真机反馈）。
     * 现在：`nc:` 条目一律保留（行渲染会先用占位名，随后由 netSongsInfo 补齐），
     * 本地条目仍然按曲库过滤（文件被删掉的不该留在列表里）。
     */
    if (s === "favorites") {
      /* 本地那一组：真在曲库里的（文件被删掉的不留在列表里）。 */
      return favorites().filter((id) => !id.startsWith("nc:") && !!getTrack(id));
    }
    if (s === "favoritesOnline") {
      /* 在线那一组：`nc:` 条目本身就能成行（`getTrack` 会给占位名，
       * 元数据由 `netSongsInfo` 随后补齐）。 */
      return favorites().filter((id) => id.startsWith("nc:"));
    }
    if (s === "album") return album()?.trackIds ?? [];
    if (s === "playlist") {
      /* 有清单文件（网易云歌单/榜单/每日推荐）：**按文件里的顺序**还原列表。
       * 以前是拿曲库里的 playlist 标签反查，同名歌 / 同时打开两份清单时会错位，
       * 甚至整个清单只剩一首能对上。 */
      const file = cloudOpen().file;
      if (file) {
        const ids = catalogQueues()[file];
        if (ids?.length) return ids.map(cloudId);
        const songs = pageData(file)?.songs;
        if (songs?.length) return songs.map((song) => cloudId(song.id));
      }
      /* 卡里 playlist.json 描述的歌单：按曲库里的 playlist 分组。 */
      const name = playlistName();
      return onlineIds().filter((id) => trackById()[id]?.playlist === name);
    }
    return [];
  });

  /* 当前页有多少"行"（决定光标上限）。 */
  const rowCount = createMemo(() => {
    const s = sub();
    if (s === null) {
      return rowsFor().length;
    }
    if (s === "settings") return 4;
    if (s === "account") return 2;
    if (s === "albums") return albums().length;
    /* 歌单/榜单子页也是列表 —— 这里漏过一次 "playlist"，真机上表现为
     * "点进去只有第一首、↓ 和 ○ 都没反应"（光标上限一直是 0）。 */
    if (
      s === "local" || s === "online" || s === "favorites" ||
      s === "favoritesOnline" ||
      s === "album" || s === "playlist"
    ) {
      return listCount();
    }
    return 0;
  });

  /* 只在列表窗口真正变化时打一行，记录分页后的数据状态；这和
   * track_window / frame_skip 的 words 日志可以直接按时间顺序对照。 */
  let lastListViewTrace = "";
  createEffect(() => {
    const currentSub = sub();
    if (
      currentSub !== "local" && currentSub !== "online" &&
      currentSub !== "favorites" && currentSub !== "favoritesOnline" &&
      currentSub !== "album" && currentSub !== "playlist"
    ) return;
    const file = currentSub === "playlist" ? cloudOpen().file : "";
    const page = file ? pageData(file) : undefined;
    const offset = start();
    const total = listCount();
    const trace = `${currentSub}:${file}:${offset}:${total}:${page?.state ?? "none"}:${page?.songs.length ?? 0}`;
    if (trace === lastListViewTrace || !logEnabled()) return;
    lastListViewTrace = trace;
    logMsg(
      `ui: list_view kind=${currentSub} file=${file} start=${offset} total=${total} ` +
        `page_state=${page?.state ?? "none"} page_offset=${page?.offset ?? -1} ` +
        `page_songs=${page?.songs.length ?? 0}`,
    );
  });

  const subTitle = createMemo(() => {
    switch (sub()) {
      case "local": return "本地音乐";
      case "online": return "在线歌曲";
      case "favorites": return "我喜欢的";
      case "favoritesOnline": return "我喜欢的（在线）";
      case "albums": return "专辑";
      case "album": return album()?.title ?? "专辑";
      case "playlist": return playlistName() || "歌单";
      case "settings": return "设置";
      case "keys": return "按键说明";
      case "about": return "关于";
      case "account": return "扫码登录";
      case "lyrics": return "歌词";
      default: return "";
    }
  });

  const activeQueueIds = createMemo(() => {
    if (!hasTrack()) return [];
    /* 队列保持**整份清单**（含下架的歌）：序号 "N / M" 与列表一致；
     * 切歌时由 stepPlayable 跳过下架的。 */
    /* 队列里可能有懒加载的云端条目（nc:…）：按 getTrack 过滤，不按曲库。 */
    const cleaned = queueIds().filter((id) => !!getTrack(id));
    if (cleaned.length > 0) return cleaned;
    const cur = track();
    return cur.online ? onlineIds() : localIds();
  });

  /* 播放器右下角显示"当前播到清单第几首 / 清单总数"（没选歌时留空）。
   * 必须放在 activeQueueIds 之后：createMemo 会立刻求值一次。 */
  const queuePosLabel = createMemo(() => {
    const list = activeQueueIds();
    const idx = list.indexOf(currentTrackId());
    if (idx < 0 || list.length === 0) return "";
    return `${idx + 1} / ${list.length}`;
  });

  /*
   * 状态行的词（都 ≤4 字，面板那一格只有三十来像素宽）。
   *
   * 优先级：错误 > 灰色行提示 > 缓冲 > 播放/暂停 > 空位
   *   「网络故障」「需要会员」   ← 原生打开失败（failNote）
   *   「已下架」                ← 点了一下灰色行（offNote，两秒后自己消失）
   *   「缓冲中」「无网络」        ← 在线歌还没出声（netHint）
   *   「播放中」「暂停中」        ← 有歌并且没在等网络
   *   「待定」                  ← 还没选歌
   */
  const statusText = createMemo(() => {
    const note = failNote() || offNote();
    if (note) return note;
    if (!hasTrack()) return "待定";
    if (netHint()) return netHint();
    return playing() ? "播放中" : "暂停中";
  });
  /* 红字只给"出事了"的状态；播放中/暂停中/待定用灰字。 */
  const statusAlert = createMemo(() => !!(failNote() || netHint()));

  /* ---------------- 播放控制 ---------------- */
  const startTrack = (id: string) => {
    const song = getTrack(id);
    if (!song) return;
    const perfT0 = Date.now();
    setFailNote("");
    lastNativeErr = "";
    logMsg("ui: 停旧曲");
    audioEngine.stop();
    logMsg("ui: 停旧曲完成");
    audioEngine.load(song);
    setCurrentTrackId(id);
    setPosition(0);
    setPlaying(true);
    setOnlineWaitFrom(song.online ? Date.now() : 0);
    logMsg("ui: 起新曲 " + id);
    audioEngine.play();
    logMsg("ui: 起新曲完成");
    preloadNext(song);
    /* 起播这一段包含一次原生 stop + load + play（同步调用），慢了要能看见。 */
    const perfDt = Date.now() - perfT0;
    if (perfDt >= 30) logMsg(`perf: 起播（含原生调用）${perfDt}ms`);
  };

  /*
   * 预热**下一首**的播放地址（只解析 + 缓存地址，不开流、不下载音频）。
   *
   * 纪律（照 ClouDS-Music 那条"只预取一个目标"）：
   *   - 只挑队列里下一首**能播**的歌，最多一个；
   *   - 不扫队列、不并发、不预下载音频数据（Vita 的网络/SD 都很有限）；
   *   - 播放在线歌时才做；失败在原生侧静默记日志。
   * 用户按下一首时直接命中 URL 缓存，省掉一次加密 POST（弱网 2–3 秒）。
   */
  const preloadNext = (cur: Track) => {
    if (!cur.online) return;
    const list = activeQueueIds();
    const idx = list.indexOf(cur.id);
    if (idx < 0 || list.length < 2) return;
    /*
     * **最多往前看 16 首**（ClouDS 的 MAX_PREFETCH_SCAN）：找第一首能播的作为预热目标。
     * 以前直接调 stepPlayable，它会一直扫到队尾 —— 5000 首的歌单就是一次 5000 次循环。
     */
    /*
     * 这里以前会调 `netPreload`（只解析地址的第二套预热）。现在统一交给下面那个
     * `netPrefetchNext` effect —— 一条路：解析 + 整首灌进 cache.dat，且会给当前曲让路。
     * 保留这个函数的唯一作用：把"最多扫 16 首"的候选算出来（日志/调试用）。
     */
    const nextId = stepPlayableCapped(list, idx, 1, 16);
    if (!nextId || nextId === cur.id) return;
  };

  /* 往前/往后找第一首能播的歌，但**最多扫 `cap` 首**（预取、自动跳歌用）。 */
  const stepPlayableCapped = (
    list: string[],
    from: number,
    delta: number,
    cap: number,
  ): string => {
    const n = list.length;
    if (n === 0) return "";
    const limit = Math.min(cap, n);
    for (let k = 1; k <= limit; k++) {
      const idx = ((from + delta * k) % n + n) % n;
      const song = getTrack(list[idx]);
      if (song && !song.off && !song.vip) return list[idx];
    }
    return "";
  };

  const adoptQueue = (ids: string[]) => {
    /* 队列里可能有懒加载的云端条目：按 getTrack 过滤，不按曲库。 */
    const cleaned = ids.filter((id) => !!getTrack(id));
    setQueueIds(cleaned.length > 0 ? cleaned : tracks().map((song) => song.id));
  };

  /* 强制刷清单的节流：登录成功、或点开一个还没内容的清单时用，
   * 一分钟最多强制一次（原生那边同步本身就是 6 个请求）。 */
  let lastForceMs = 0;
  const forceListSync = () => {
    const now = Date.now();
    const age = now - lastForceMs;
    if (age < 60_000) {
      return;
    }
    lastForceMs = now;
    logMsg(
      `ui: list_sync_request force=1 logged_in=${loginSnapshot().loggedIn} ` +
        `catalog_account=${catalogAccount().length}`,
    );
    media()?.listSync?.(1);
  };

  /*
   * 清单文件的"新鲜度"。
   *
   * 榜单 / 歌单文件是**打开时**才落盘的：登录之前写下的那些会带一大堆下架
   * 标记（真机上出现过 200 首里 159 首标灰 —— 那是匿名会话拿到的权限结果）。
   * 文件超过 10 分钟就让原生重拉一次（原生自己有 60 秒 TTL，不会打爆接口）。
   * 文件时间戳由 Rust catalog worker 管理，guest 不再读取原始清单。
   */
  let lastStaleAskMs = 0;
  const refreshListIfStale = () => {
    const open = cloudOpen();
    if (!open.id) return;
    /* 拉取失败时 at 不会更新，别每 5 秒问一次原生（真正的节流点）。 */
    const now = Date.now();
    if (now - lastStaleAskMs < 60_000) return;
    lastStaleAskMs = now;
    logMsg("ui: 清单文件偏旧，让原生重拉 id=" + open.id);
    media()?.netPlaylistRequest?.(open.id);
  };

  const playTrack = (id: string) => {
    logMsg("ui: 点歌 " + id);
    const target = getTrack(id);
    const pool = listIds().length > 0 ? listIds() : tracks().map((s) => s.id);
    const scoped = pool.filter(
      /* 用 getTrack：懒加载的云端条目（nc:…）不在曲库里，查 trackById 会漏掉，
       * 于是"点歌"那一刻队列会被清空、L/R 切歌就乱了。 */
      (tid) => (getTrack(tid)?.online ?? false) === (target?.online ?? false),
    );
    adoptQueue(scoped);
    startTrack(id);
    /* 点歌后留在原列表（左栏一直在放），方便连着挑下一首。 */
  };

  const jumpInQueue = (delta: number) => {
    if (!hasTrack()) return; /* 空位时 L/R 不"顺手"开播，必须先从列表点歌 */
    const list = activeQueueIds();
    if (list.length === 0) return;
    const cur = list.indexOf(currentTrackId());
    /* 当前歌不在队列里（曲库重扫/队列被清洗）时的边界：
     * 下一首从头开始、上一首从尾巴开始；下架的（灰色）歌直接跳过 ——
     * 上一版把下架歌从队列里删掉，序号就和列表对不上了。 */
    const from = cur === -1 ? (delta > 0 ? -1 : 0) : cur;
    const nextId = stepPlayable(list, from, delta);
    if (!nextId) return;
    startTrack(nextId);
  };

  const nextTrack = () => jumpInQueue(1);
  const prevTrack = () => jumpInQueue(-1);

  /*
   * 下一首预取（任务书 §8/§30）：切歌之后告诉原生"下一首是谁"，
   * 它会在**不抢当前曲带宽**的前提下（缓冲 ≥20s 且网络 FAST）后台把整首备进
   * cache.dat；真到切过去时就是零网络请求启动。
   * 只对在线歌（`nc:<网易云 id>`）有意义；本地歌和队列末尾直接取消。
   */
  createEffect(() => {
    const api = media();
    const cur = currentTrackId();
    if (!api?.netPrefetchNext) return;
    if (!cur) {
      api.netPrefetchCancel?.();
      return;
    }
    const list = activeQueueIds();
    const from = list.indexOf(cur);
    const nextId = stepPlayable(list, from === -1 ? -1 : from, 1);
    if (!nextId || !nextId.startsWith("nc:")) {
      api.netPrefetchCancel?.();
      return;
    }
    api.netPrefetchNext(nextId.slice(3)); /* 去掉 "nc:" 前缀，传网易云 id */
  });

  const finishTrack = () => {
    if (playbackMode() === "repeat-one") {
      audioEngine.stop();
      audioEngine.load(track());
      setPosition(0);
      setPlaying(true);
      /* 在线歌单曲循环要重新解析地址：把加载提示的计时也重置，别静默等。 */
      setOnlineWaitFrom(track().online ? Date.now() : 0);
      audioEngine.play();
      return;
    }
    nextTrack();
  };

  const togglePlay = () => {
    if (!hasTrack()) return;
    const next = !playing();
    setPlaying(next);
    if (next) {
      audioEngine.play();
      return;
    }
    audioEngine.pause();
  };

  const togglePlaybackMode = () => {
    setPlaybackMode(playbackMode() === "sequence" ? "repeat-one" : "sequence");
  };

  const toggleFavorite = () => {
    if (!hasTrack()) return;
    const id = track().id;
    const cur = favorites();
    const next = cur.includes(id) ? cur.filter((x) => x !== id) : [...cur, id];
    setFavorites(next);
    try {
      media()?.store_set?.("favorites", JSON.stringify(next));
    } catch {
      /* ignore */
    }
  };

  /* ---------------- 页面导航 ---------------- */
  /* 子页进入方向：前进=从右滑入，返回=从左滑入（供 SubPage 的进入动画用）。 */
  let subDir: "left" | "right" = "right";
  /* 子页的根节点（退场动画要用）与"正在退场"的一次性状态。 */
  let subRef: NodeMirror | undefined;
  let subExiting: { untilMs: number; startedMs: number } | null = null;
  const openSub = (s: Sub) => {
    subDir = "right";
    subExiting = null; /* 上一次退场还没走完就又进来了：取消它，别把新页关了 */
    /* 切页整段量一次：JS 状态 + 界面重建都算在里面，超过 30ms 才落日志。 */
    perfSpan("切页面 → " + s, () => {
      setSub(s);
      setCursor(0);
      setStart(0);
      setTabFocus(false);
      setHeaderFocus(false);
      /* 从播放器点"词"进来时焦点还在左栏 —— 不挪过来，子页就按不动。 */
      setZone("content");
    });
  };

  const closeSub = () => {
    subDir = "left";
    perfSpan("返回（关子页）", () => {
      /*
       * 退场动画：**先让子页滑出去，到点再真的关**（真机反馈"设置/歌词进去有动画、出来没有"）。
       * 以前这里直接 setSub(null)，组件当场卸载 —— 根本没有退场那一段。
       * 位移/时长和 Tab 退场同节奏（16px / 160ms），所以整个界面的"退出感"是一致的。
       */
      if (sub() !== null && subRef) {
        const now = Date.now();
        subExiting = { untilMs: now + MOTION.subExit, startedMs: now };
        animate(subRef, "translateX", MOTION.pageOffset, {
          dur: MOTION.subExit,
          easing: "out",
        });
        animate(subRef, "opacity", 0, { dur: MOTION.subExit, easing: "out" });
        logMsg(`perf: 子页退场 → ${subTitle()}（exit ${MOTION.subExit}ms）`);
      } else {
        setSub(null);
      }
      setSelectedAlbumId(null);
      setCursor(0);
      setStart(0);
      setTabFocus(false);
      setHeaderFocus(false);
    });
  };

  /**
   * 帧首调用（和 Tab 切换一起）：退场到点了就真正关页。
   * 记"实际退场耗时"，被拖慢时直接点名（和 Tab 那两行一个格式，方便一起看）。
   */
  const pumpSubExit = () => {
    if (!subExiting) return;
    if (Date.now() < subExiting.untilMs) return;
    const took = Date.now() - subExiting.startedMs;
    subExiting = null;
    logMsg(
      `perf: 子页已退出（实际 ${took}ms / 预期 ${MOTION.subExit}ms）` +
        (took > MOTION.subExit + 120 ? " ← 被拖慢：查上一帧的慢帧账本" : ""),
    );
    setSub(null);
  };

  /* 打开一个"清单子页"（歌单 / 榜单 / 每日推荐）。
   * 关键：**先读文件、当场并进曲库**，子页一打开就有内容（断网也有上次的）；
   * 文件里没有、又知道网易云 id 的，才交给帧循环让原生去拉。 */
  const openListSub = (row: MenuRowData) => {
    let name = row.name;
    let state = row.id ? "loading" : "ready";
    let got = false;
    logMsg(
      `ui: open_list_row kind=${row.kind} name=${row.name} id=${row.id ?? ""} file=${row.file ?? ""}`,
    );
    if (row.file) {
      /* 先切到轻量 loading 页，再由下一帧读取清单。不要在按键回调里
       * 同步 catalogPage/catalogIds，否则字体预热和首屏 drawlist 会被同一
       * 次输入事件拖住。已有缓存只读，不做桥接。 */
      const page = catalogPages()[row.file];
      if (page?.name) name = page.name;
      if (page?.state === "ready" && page.total > 0) {
        state = "ready";
        got = true;
      }
    }
    setPlaylistName(name);
    setCloudOpen({
      id: row.id ?? "",
      name,
      file: row.file ?? "",
      state,
      message: "",
    });
    /* 文件里还没内容（榜单第一次打开 / 后台那轮还没跑完）：
     * **当场**就让原生去拉这一张（而不是等帧循环 5 秒后那一拍），
     * 拉完会落盘，下一拍就读到了。另外顺手让后台刷一遍清单。
     * 同步进度靠 listOpenAt + 帧循环里的秒数递增显示。 */
    listOpenAt = Date.now();
    setSyncElapsed(0);
    if (!got && row.id) {
      media()?.netPlaylistRequest?.(row.id);
      forceListSync();
    }
    openSub("playlist");
    if (row.file) queueCatalogPage(row.file, 0, 1);
    refreshListIfStale();
  };

  const activateRow = () => {
    const s = sub();
    const i = cursor();

    if (s === null) {
      const row = rowsFor()[i];
      if (!row || row.kind === "hint") return;

      /* "我的"页签是固定的六个入口（不是清单文件）。 */
      if (tabIndex() === 3) {
        if (i === 0) openSub("account");
        else if (i === 1) openSub("favorites");
        else if (i === 2) openSub("favoritesOnline");
        else if (i === 4) openSub("local");
        else if (i === 5) openSub("albums");
        else if (i === 6) openSub("settings");
        return;
      }

      /* 既不是清单文件、也没有 id / local 标记的行（例如"热门推荐 同步中…"占位）
       * 按下去什么都不做。 */
      if (row.file || row.id || row.local) openListSub(row);
      return;
    }

    if (
      s === "local" || s === "online" || s === "favorites" ||
      s === "favoritesOnline" ||
      s === "album" || s === "playlist"
    ) {
      const id = listIdAt(i); /* 窗口化：只算这一条 */
      if (!id) return;
      /* 网易云下架 / 无版权的歌（灰色行）：列出来但不播，按一下给个提示。 */
      const row = getTrack(id);
      if (row?.off || row?.vip) {
        setOffNote(row.off ? "已下架" : "需要会员");
        offNoteAt = Date.now();
        return;
      }
      playTrack(id);
      return;
    }
    if (s === "albums") {
      const a = albums()[i];
      if (a) {
        setSelectedAlbumId(a.id);
        openSub("album");
      }
      return;
    }
    if (s === "settings") {
      if (i === 0) setUiTheme(nextTheme());
      else if (i === 1) openSub("keys");
      else if (i === 2) openSub("about");
      else if (i === 3) applyCjkMode(cjkMode() === "stream" ? "baked" : "stream");
      return;
    }
    if (s === "account") {
      if (i === 0) media()?.netLoginStart?.();
      else if (i === 1) toggleRememberLogin();
    }
  };

  const activatePlayer = () => {
    /* 按控制排的实际顺序取动作（顺序模式 · 上一首 · 播放 · 下一首 · 喜爱 · 词）。 */
    const slot = CTRL_ORDER[playerCursor()] ?? "play";
    if (slot === "mode") togglePlaybackMode();
    else if (slot === "prev") prevTrack();
    else if (slot === "play") togglePlay();
    else if (slot === "next") nextTrack();
    else if (slot === "fav") toggleFavorite();
    else openSub("lyrics");
  };

  /* ---------------- 挂载 / 副作用 ---------------- */
  onMount(() => {
    const realTracks = scanLibrary();
    if (realTracks.length) {
      setTracks(realTracks);
      setQueueIds(realTracks.map((song) => song.id));
    }
    /* 进来**不预选歌**：播放器停在空位，等用户从本地/在线清单里点第一首才开播。 */
    if (cjkMode() === "stream") applyCjkMode("stream");

    /* Rust catalog worker 会在后台加载/解析已有清单；这里仅采样小摘要。 */
    pullCatalog();
    media()?.listSync?.();
  });

  /* 登录成功（或这次启动就带着上次的会话）→ 立刻强制刷一遍在线清单。
   * 每日推荐 / 我的歌单在登录前必定 Auth 失败，不强制刷就永远停在"同步中"
   * —— 这是真机上反馈的"登录了还显示同步中"的直接原因。 */
  let wasLoggedIn = false;
  createEffect(() => {
    const on = loginSnapshot().loggedIn;
    if (on && !wasLoggedIn) {
      wasLoggedIn = true;
      /* 计时从"这次登录"开始算，而不是应用启动那一刻。 */
      accountSyncSinceAt = Date.now();
      setAccountSyncLate(false);
      logMsg(
        `ui: session_transition logged_in=true catalog_version=${catalogVersion()} ` +
          `catalog_account=${catalogAccount().length}`,
      );
      forceListSync();
    } else if (!on) {
      wasLoggedIn = false;
    }
  });

  /* 当前曲目的内嵌封面（本地文件才有；在线曲目跳过）。 */
  createEffect(() => {
    const cur = track();
    if (!cur || cur.cover || !cur.audioPath || cur.online) return;
    const api = media();
    if (!api || !api.cover) return;
    let handle = -1;
    try {
      handle = api.cover(cur.audioPath);
    } catch {
      /* fallback：唱片占位 */
    }
    if (typeof handle === "number" && handle >= 0) {
      const key = "emb:" + cur.audioPath;
      registerTexture(key, handle);
      setTracks((prev) => prev.map((t) => (t.id === cur.id ? { ...t, cover: key } : t)));
    }
  });

  /* 播放中锁 PS 键（暂停即解锁）。 */
  createEffect(() => {
    const on = playing();
    let ret = 0;
    try {
      ret = media()?.setPsLock?.(on) ?? 0;
    } catch {
      ret = -1;
    }
    setPsLockInfo(
      on
        ? ret === 0
          ? "PS KEY  LOCKED"
          : "PS KEY  FAIL 0x" + (ret >>> 0).toString(16).toUpperCase()
        : "PS KEY  UNLOCKED",
    );
    logMsg("ps lock: request=" + String(on) + " ret=" + String(ret));
  });

  /* ---------------- 按键 ---------------- */
  const screenOn = () => !displayOff();

  onButtonPress(BTN.UP, () => {
    if (zone() === "player") return;
    if (headerFocus()) return;
    if (tabFocus() && sub() === null) return;
    if (cursor() > 0) {
      const n = cursor() - 1;
      setCursor(n);
      if (n < start()) setStart(n);
      return;
    }
    if (sub() === null) setTabFocus(true);
    else setHeaderFocus(true);
  }, { active: screenOn });

  onButtonPress(BTN.DOWN, () => {
    if (zone() === "player") return;
    if (headerFocus()) {
      setHeaderFocus(false);
      setCursor(0);
      return;
    }
    if (tabFocus() && sub() === null) {
      setTabFocus(false);
      setCursor(0);
      return;
    }
    const n = cursor() + 1;
    if (n < rowCount()) {
      setCursor(n);
      if (n >= start() + LIST_WINDOW) setStart(n - LIST_WINDOW + 1);
    }
  }, { active: screenOn });

  onButtonPress(BTN.LEFT, () => {
    if (zone() === "player") {
      setPlayerCursor(Math.max(0, playerCursor() - 1));
      return;
    }
    if (headerFocus()) {
      setZone("player");
      return;
    }
    if (tabFocus() && sub() === null) {
      if (tabIndex() > 0) {
        switchTab(tabIndex() - 1); /* 两段式：旧页退场 → 换页 → 新页进场 */
        setCursor(0);
        setStart(0);
      } else {
        setZone("player");
      }
      return;
    }
    setZone("player");
  }, { active: screenOn });

  onButtonPress(BTN.RIGHT, () => {
    if (zone() === "player") {
      if (playerCursor() < PLAYER_LAST) {
        setPlayerCursor(playerCursor() + 1);
      } else {
        setZone("content");
        setTabFocus(sub() === null);
      }
      return;
    }
    if (headerFocus()) {
      setHeaderFocus(false);
      setCursor(0);
      return;
    }
    if (tabFocus() && sub() === null) {
      switchTab(Math.min(TAB_LABELS.length - 1, tabIndex() + 1));
      setCursor(0);
      setStart(0);
    }
  }, { active: screenOn });

  onButtonPress(BTN.CIRCLE, () => {
    if (zone() === "player") {
      activatePlayer();
      return;
    }
    if (headerFocus()) {
      /* 歌词页的 ← 按要求跳回"我的"页签；其它子页照旧返回进来的那一页。 */
      const toMine = sub() === "lyrics";
      closeSub();
      if (toMine) switchTab(MINE_TAB);
      return;
    }
    if (tabFocus() && sub() === null) {
      setTabFocus(false);
      setCursor(0);
      return;
    }
    activateRow();
  }, { active: screenOn });

  onButtonPress(BTN.TRIANGLE, () => {
    if (zone() === "player") {
      setZone("content");
      return;
    }
    /* △ 在任意子页都直接退出。列表不再先把光标滚回第一首，否则用户
     * 连按时会看到列表不断向上滚，且无法把按键当作明确的返回操作。 */
    if (sub() !== null) closeSub();
  }, { active: screenOn });

  /* L / R：上一首 / 下一首（黑屏也有效）。 */
  onButtonPress(BTN.LTRIGGER, () => prevTrack());
  onButtonPress(BTN.RTRIGGER, () => nextTrack());

  /* START：息屏（黑屏遮罩，按键仍有效）。 */
  onButtonPress(BTN.START, () => {
    const next = !displayOff();
    setDisplayOff(next);
    logMsg(next ? "screen: soft off" : "screen: on");
  });

  onButtonPress(0xffff, (pressed) => {
    /* 任何按键都记一笔"用户刚操作过"：原生预取会据此在 3 秒内让路，
     * 保证上下切页面/按钮响应不被后台整首下载挤（真机反馈的卡顿来源）。 */
    media()?.netUserActive?.();
    if (displayOff() && pressed & ~(BTN.LTRIGGER | BTN.RTRIGGER | BTN.START)) {
      setDisplayOff(false);
    }
  });

  /* ---------------- 帧循环 ---------------- */
  onFrame(() => {
    /* 慢帧账本：开新账；帧内任何提前 return 都由下面的 finally 结算。 */
    perfFrameBegin();
    /*
     * Tab 两段式切换的推进放**帧首**：它必须无条件执行 ——
     * 之前放在"正在播放"的进度块里，没歌播放时切页就永远不会换页，
     * 内容停在退场后的 opacity 0（真机表现为"往左切换页面全空"）。
     */
    pumpTabTransition();
    pumpSubExit();
    /* 用 try/finally 保证"帧结束"一定被结算 —— 帧循环里有提前 return（息屏那条），
     * 漏结算就会退化成量"两帧之间的墙钟时间"，把宿主渲染也算成 JS 慢帧（踩过）。 */
    try {
    /*
     * QR 贴图上传可能进入宿主 GPU 路径，不能和收到网络结果的响应式
     * effect 放在同一帧。只在帧循环里一次性泵一个已排队的二维码。
     */
    pumpQrTexture();
    /* 流式字体的 prepare 是主线程上的同步 bookkeeping，但不能把一屏
     * 文本一次性塞进同一帧。每帧最多推进一个 lease；页面仍停在 loading
     * 占位态时，drawlist 不会和冷启动字形收集重叠。 */
    /* prepareText 单次可能占 30~60ms（日志已证实），即使每次只推进
     * 一个也不应连续霸占每个 vblank。错开到每 4 帧一次，空闲帧给
     * drawlist / 输入 / present 让路；页面 warmup 仍会继续轮询完成。 */
    cjkPrepareFrame += 1;
    if (cjkPrepareFrame % 4 === 1) perfSpan("cjkPrepare", () => pumpCjkPrepare(1));
    perfFrame(); /* 掉帧 / 帧率汇总（只在慢的时候写日志） */
    /* 音频泵也要入账：它是**原生调用**（SDL / sceAudioOutOutput），输出缓冲满时
     * 会按音频时钟阻塞 —— 那部分是"等音频"，不是卡顿，必须和 JS 逻辑分开看。 */
    perfSpan("audioPump", () => audioEngine.pump());

    if (cjkMode() === "stream") {
      cjkDbgFrames += 1;
      if (cjkDbgFrames % 60 === 0 && logEnabled()) logCjkStats();
    }

    const nowMs = Date.now();
    const frameGapMs = nowMs - lastFrameMs;
    lastFrameMs = nowMs;
    if (displayOff() && frameGapMs > 1500) {
      logMsg("screen: on (back from background)");
      setDisplayOff(false);
    }
    if (displayOff()) return;
    frameCounter += 1;

    /* 页面已经显示 loading 后才读 native catalog；即使桥接本身偶尔较慢，
     * 也不会和按键回调、列表第一次 mount、字体收集叠在同一个同步栈。 */
    if (deferredCatalogPage && deferredCatalogPage.dueFrame <= frameCounter) {
      const task = deferredCatalogPage;
      deferredCatalogPage = undefined;
      loadCatalogPage(task.file, task.offset);
    }
    if (playlistWarmup && !playlistFontReady() && playlistWarmup.ready()) {
      playlistFontPrimedFile = cloudOpen().file;
      playlistFontPrimedEpoch = cjkEpoch();
      setPlaylistFontReady(true);
      if (logEnabled()) {
        logMsg(`perf: cjk_warmup_ready pending=0`);
      }
    }
    /* catalogIds 只为后续播放队列服务，低优先级地一次处理一个文件。 */
    if (frameCounter % 2 === 0) pumpCatalogIds();

    /* 登录状态：登录页开着时半秒一推（扫码要跟手），其它页面 2 秒一推。
     * 以前只有登录页才会刷新，于是"上次登录过、这次直接开应用"时，
     * 歌单页 / 我的页一直显示"未登录 / 同步中"（真机反馈过）。 */
    if (frameCounter % 120 === 0 || (sub() === "account" && frameCounter % 60 === 0)) {
      refreshLogin();
    }

    /* 灰色歌（下架）提示：点了一下之后 2 秒自动收掉。 */
    if (offNote() && Date.now() - offNoteAt > 2000) setOffNote("");

    /* 在线歌已缓冲进度（进度条里那根浅色条）：约每 0.2 秒问一次原生，
     * 原生那边只是两次原子读，不碰网络也不碰盘。 */
    if (frameCounter % 12 === 0) {
      const cur = track();
      if (cur.online && media()?.netBuffer) {
        /* 包进 perfSpan：这一笔进了慢帧账本，真机上就知道"是不是它在吃帧"。 */
        const parts = perfSpan("netBuffer", () => media()?.netBuffer?.() || "").split(",");
        const done = Number(parts[0]) || 0;
        const total = Number(parts[1]) || 0;
        const ratio = total > 0 ? Math.min(1, done / total) : 0;
        if (Math.abs(ratio - buffered()) > 0.005) setBuffered(ratio);
      } else if (buffered() !== 0) {
        setBuffered(0);
      }
    }

    /* 在线歌真实信息：每 0.5 秒问一批（一次请求补多首），只问还没拿到的。
     * 以前只补"正在播的那首"，列表里其余全是一模一样的占位名，看着像重复条目。 */
    if (frameCounter % 30 === 0) {
      const filled = onlineInfoFilled();
      const pending: string[] = [];
      for (const t of tracks()) {
        if (!t.online) continue;
        const id = onlineIdHint(t.audioPath);
        if (id && !filled.has(id)) pending.push(id);
      }
      /* 收藏里的在线歌（`nc:<id>`）也要补：它们不进曲库，上面那个循环扫不到，
       * 而收藏页现在正是靠这份信息把歌名显示出来的。 */
      for (const fid of favorites()) {
        if (!fid.startsWith("nc:")) continue;
        const id = fid.slice(3);
        if (id && !filled.has(id)) pending.push(id);
      }
      if (pending.length > 0) {
        /* 一次最多 8 首：以前把整批（可能上百首）ID 一次性发下去，
         * 原生要建一份大 JSON、JS 再 parse 一遍，弱机上就是内存尖峰。
         * 剩下的下一拍继续（每 0.5 秒一轮，几秒内自然补齐）。 */
        const raw = media()?.netSongsInfo?.(pending.slice(0, 8).join(",")) || "[]";
        try {
          const list = JSON.parse(raw) as {
            id?: string;
            detail?: { title?: string; artists?: string; album?: string; durationMs?: number };
          }[];
          const byId = new Map<
            string,
            { title?: string; artists?: string; album?: string; durationMs?: number }
          >();
          for (const e of Array.isArray(list) ? list : []) {
            if (e?.id && e.detail?.title) byId.set(e.id, e.detail);
          }
          if (byId.size > 0) {
            setTracks((prev) =>
              prev.map((t) => {
                const id = t.online ? onlineIdHint(t.audioPath) : "";
                const d = id ? byId.get(id) : undefined;
                return d
                  ? {
                      ...t,
                      title: d.title || t.title,
                      artist: d.artists || t.artist,
                      album: d.album || t.album,
                      durationMs: d.durationMs || t.durationMs,
                    }
                  : t;
              }),
            );
            setOnlineInfoFilled((prev) => {
              const next = new Set(prev);
              for (const id of byId.keys()) next.add(id);
              return next;
            });
            /* 单独留一份"按网易云 id 索引"的元数据：收藏页的 `nc:` 条目靠它成行。 */
            setCloudInfo((prev) => {
              const next = { ...prev };
              for (const [id, d] of byId) next[id] = d;
              return next;
            });
          }
        } catch {
          /* 没拿到就下次再问 */
        }
      }
    }

    /* 清单文件由 Rust catalog worker 读取、解析并缓存；guest 每 0.5 秒只采样
     * 一个原子版本号，需要时再取菜单摘要/当前窗口，不再触碰文件 IO 或全量 JSON。 */
    if (frameCounter % 30 === 0) pullCatalog();
    if (frameCounter % 60 === 0) {
      /* 同步进度（"3/7"）：原生只报当前那件事，只有多步任务才显示数字。 */
      try {
        const raw = perfSpan("netSyncProgress", () => media()?.netSyncProgress?.() || "");
        const p = raw ? (JSON.parse(raw) as { kind?: string; done?: number; total?: number }) : null;
        const total = p?.total ?? 0;
        let txt = "";
        if (p?.kind === "download" && total > 0) {
          /* 真实下载百分比（已收字节 / Content-Length）。
           * 收完正文还要解析、落盘，所以封顶 99%，免得卡在 100% 看着像坏了。 */
          const pct = Math.min(99, Math.round(((p.done ?? 0) / total) * 100));
          txt = pct > 0 ? `${pct}%` : "";
        } else if (p?.kind && total > 1) {
          txt = `${p.done ?? 0}/${total}`;
        }
        if (txt !== syncProg()) {
          setSyncProg(txt);
          logMsg(`ui: list_progress kind=${p?.kind ?? ""} done=${p?.done ?? 0} total=${p?.total ?? 0} text=${txt}`);
        }
      } catch {
        /* 拿不到就当没有进度 */
      }
    }
    /* 登录了却一直没有 account_playlists.json：每分钟再催一次（forceListSync
     * 自带 60 秒节流），超过 60 秒还没来就在界面上写"同步失败"。
     * 以前这里只会一直显示"同步中…"，用户根本分不出是慢还是坏了。 */
    if (frameCounter % 300 === 0) {
      const hasAccount = catalogAccount().length > 0;
      if (hasAccount) {
        if (accountSyncLate()) setAccountSyncLate(false);
      } else if (loginSnapshot().loggedIn) {
        const waited = Date.now() - accountSyncSinceAt;
        const stage = waited > 60_000 ? 2 : waited > 15_000 ? 1 : 0;
        if (stage !== lastAccountTraceStage) {
          lastAccountTraceStage = stage;
          logMsg(
            `ui: account_check stage=${stage} logged_in=true catalog_account=0 waited_ms=${waited}`,
          );
        }
        if (waited > 60_000 && !accountSyncLate()) setAccountSyncLate(true);
        if (waited > 15_000) forceListSync();
      } else if (lastAccountTraceStage !== -1) {
        lastAccountTraceStage = -1;
        logMsg("ui: account_check logged_in=false");
      }
    }
    /* 正开着的清单子页：先读文件（断网/没刷新过也有上次的内容）；
     * 文件里还没歌、又知道网易云 id 的，兜底让原生去拉一次（拉到会落盘）。 */
    if (sub() === "playlist" && frameCounter % 60 === 0) {
      const open = cloudOpen();
      /* 同步计时：还在等内容就每秒 +1（界面显示"正在同步歌单… 3s"，
       * 免得用户以为卡死了）；拿到内容就停在 0。 */
      if (open.state !== "ready" && listOpenAt > 0) {
        const secs = Math.floor((Date.now() - listOpenAt) / 1000);
        if (secs !== syncElapsed()) setSyncElapsed(secs);
      }
      /* 每 5 秒看一次"这份清单文件是不是太旧了"（旧 = 可能是登录前的权限结果） */
      if (frameCounter % 300 === 0) refreshListIfStale();
      let name = open.name;
      let songs: ListSong[] | undefined;
      if (open.file) {
        queueCatalogPage(open.file, start(), 1);
        const page = pageData(open.file);
        if (page?.name) name = page.name;
        if (page?.state === "ready") songs = page.songs;
      }
      if (songs && pageData(open.file)?.state === "ready") {
        /* 文件没变就什么都不做（adoptListFile 自己判重）——
         * 以前每秒把整张榜单重并一遍，是卡顿的主因。 */
        const renamed = name !== open.name;
        if (renamed) setPlaylistName(name);
        if (renamed || open.state !== "ready") {
          setCloudOpen((p) => ({ ...p, name, state: "ready" }));
        }
      } else if (open.id && frameCounter % 60 === 0) {
        /* 只触发 native 请求；结果落盘后由 catalog worker 发布，整份响应不回 guest。 */
        media()?.netPlaylistRequest?.(open.id);
      }
    }

    const snap = audioEngine.snapshot(frameCounter === 1);

    /* 在线歌打开彻底失败（三次都没成）：别再装成"在播放"，
     * 停掉并把原因显示出来 —— 否则用户看到的就是"没声音还卡着"。 */
    if (snap.error !== lastNativeErr) {
      lastNativeErr = snap.error;
      if (snap.error) {
        logMsg("ui: 播放失败 " + snap.error);
        setPlaying(false);
        /* 原生给的就是短词（"网络故障"/"需要会员"）；这里再兜一道，
         * 保证状态行永远不会被长字符串撑爆。 */
        setFailNote(snap.error.slice(0, 6));
        /* 原生说"需要会员/暂无版权"：把这首歌标成不可播（灰行 + 跳歌时跳过），
         * 免得用户每次按下一首都要重新等一轮 403。 */
        if (snap.error.indexOf("会员") >= 0) {
          const id = currentTrackId();
          setTracks((prev) => prev.map((t) => (t.id === id ? { ...t, vip: true } : t)));
        }
      }
    }

    /* 时长同步只在"新曲真的出声"时做：在线歌解析/开流期间原生还报着
     * 上一首的时长，照抄会把旧时长写到新曲上（切歌后瞬间显示错时长）。 */
    if (snap.durMs > 0 && snap.playing) {
      const cur = track();
      if (cur && cur.durationMs !== snap.durMs) {
        if (cur.id.startsWith("nc:")) {
          /* 懒加载的云端条目不在曲库里，真实时长记在覆盖表里。 */
          setCloudDur((p) => ({ ...p, [cur.id]: snap.durMs }));
          /* 保命缓存里那一份也要跟着更新，否则显示的还是旧时长。 */
          const kept = lazyKeep[cur.id];
          if (kept) lazyKeep[cur.id] = { ...kept, durationMs: snap.durMs };
        } else {
          setTracks((prev) =>
            prev.map((t) => (t.id === cur.id ? { ...t, durationMs: snap.durMs } : t)),
          );
        }
      }
    }

    /* 在线歌加载提示：还没出声"缓冲中"，等太久"无网络"（只在变化时写信号）。 */
    {
      const cur = track();
      const from = onlineWaitFrom();
      let hint = "";
      if (cur?.online && from > 0 && playing()) {
        /*
         * 只有**真的出声**才算缓冲结束。
         *
         * 以前这里是 `snap.playing || snap.durMs > 0`，而解码器一接上流就知道时长了
         * （还没输出任何声音），于是"缓冲中"被提前收掉、状态显示成"播放中" ——
         * 真机反馈的原话："明明还没出声，写着播放中不太合理"。
         */
        if (snap.playing) {
          setOnlineWaitFrom(0);
        } else {
          hint = Date.now() - from > 20000 ? "无网络" : "缓冲中";
        }
      }
      if (hint !== netHint()) setNetHint(hint);
    }

    if (!playing()) return;

    const currentTrack = track();
    /* 息屏期间可能被原生侧换过歌：本地歌按路径同步回来（在线歌反查不了）。 */
    if (snap.path && !currentTrack.online && snap.path !== currentTrack.audioPath) {
      const matched = tracks().find((song) => song.audioPath === snap.path);
      if (matched && matched.id !== currentTrack.id) setCurrentTrackId(matched.id);
    }
    const duration = currentTrack.durationMs || snap.durMs || getTrackDuration(currentTrack);
    const next = snap.posMs;
    if (duration > 0 && next >= duration) {
      finishTrack();
      return;
    }

    /*
     * 进度条：只在**真的变了**的时候写信号。
     *
     * 这里原来还有个"柱状动画计数器"，每 2 帧自增一次（不分播放与否）——
     * 它让框架在待机时也不停重建绘制列表：真机待机 60 秒实测
     * `帧间隔≥120ms` 34 次、`HOST: guest 帧耗时` 平均 298ms。
     * 那份柱状动画已经删掉，计数器也一并移除；进度信号保留"值变才写"的守卫。
     */
    /*
     * 进度信号**限频到 ~10Hz**（评审 §13）：视觉上 10 次/秒已经够顺，
     * 而 60Hz 会让 PlayerPanel + 歌词整条依赖链每秒 replay 60 次
     * （PocketJS 官方：代价取决于 replay 次数，与 state 放在哪一层无关）。
     * 音频精度不受影响 —— 播放位置始终由原生侧维护，这里只是"界面采样"。
     */
    if (Math.abs(next - position()) >= 100) setPosition(next);

    } finally {
      /* 本帧 JS 侧到此结束：只有这一段算"JS 慢帧"，宿主渲染/等待不算。 */
      perfFrameEnd();
    }
  });

  /* ---------------- 渲染 ---------------- */
  const rowActive = () =>
    zone() === "content" && !tabFocus() && !headerFocus();

  return (
    <View debugName="MusicScreen" class={bgCls("appRoot")}>
      {/* 整屏壁纸：画在最前面，所有面板/行都是毛玻璃材质，会透出它 */}
      <ThemeSeed />

      <PlayerPanel
        nodeRef={(n) => {
          playerRef = n;
        }}
        track={track}
        playing={playing}
        position={position}
        playbackMode={playbackMode}
        favorite={isFavorite}
        cursor={playerCursor}
        focused={() => zone() === "player"}
        queuePos={queuePosLabel}
        netStatus={statusText}
        netAlert={statusAlert}
        buffered={buffered}
      />

      <View
        ref={(n) => {
          contentRef = n;
        }}
        class={bgCls("contentCol")}
      >
        <Show
          when={sub() === null}
          fallback={
            <SubPage
                nodeRef={(n) => {
                  subRef = n;
                }}
                title={subTitle()}
                right={sub() === "account" ? (loginSnapshot().loggedIn ? "SIGNED IN" : "未登录") : ""}
                dir={subDir}
                backFocused={() => zone() === "content" && headerFocus()}
              >
                <>
                 <Show
                   when={
                     sub() === "local" ||
                     sub() === "online" ||
                     sub() === "favorites" ||
                     sub() === "favoritesOnline" ||
                     sub() === "album"
                   }
                 >
                    <TrackListPage
                      debugName={sub() ?? "track"}
                      count={listCount}
                      idAt={listIdAt}
                      getTrack={getTrack}
                      currentId={currentTrackId}
                      cursor={cursor}
                      start={start}
                      active={rowActive}
                    />
                  </Show>
                  {/* 歌单子页：playlist.json 的歌单立刻就有内容；
                      网易云歌单要等接口，先显示"正在同步 / 同步失败"。 */}
                  <Show when={sub() === "playlist"}>
                    <Show
                      when={
                        (cloudOpen().id === "" || cloudOpen().state === "ready" || listCount() > 0) &&
                        (cjkMode() !== "stream" || playlistFontReady() || start() !== 0)
                      }
                      fallback={
                        <PlaceholderPage
                          text={
                            cjkMode() === "stream" &&
                            (cloudOpen().state === "ready" || listCount() > 0) &&
                            start() === 0 &&
                            !playlistFontReady()
                              ? `准备字体…${playlistWarmup?.pending() ? ` ${playlistWarmup.pending()}` : ""}`
                              : cloudOpen().state === "failed"
                              ? cloudOpen().message || "同步失败（检查登录状态）"
                              : syncElapsed() > 45
                                ? "同步超时（网络或登录）· 按 △ 返回"
                                : syncElapsed() > 0
                                  ? `正在同步歌单…${syncProgText()} ${syncElapsed()}s`
                                  : `正在同步歌单…${syncProgText()}`
                          }
                        />
                      }
                    >
                      <TrackListPage
                        debugName="playlist"
                        count={listCount}
                        idAt={listIdAt}
                        getTrack={getTrack}
                        currentId={currentTrackId}
                        cursor={cursor}
                        start={start}
                        active={rowActive}
                      />
                    </Show>
                  </Show>
                  <Show when={sub() === "albums"}>
                    <AlbumListPage
                      albums={albums()}
                      cursor={cursor}
                      start={start}
                      active={rowActive}
                    />
                  </Show>
                  <Show when={sub() === "settings"}>
                    <SettingPage
                      fontMode={cjkMode() === "stream" ? "流式" : "内置"}
                      cursor={cursor}
                      active={rowActive}
                    />
                  </Show>
                  <Show when={sub() === "keys"}>
                    <KeyGuidePage />
                  </Show>
                  <Show when={sub() === "about"}>
                    <AboutPage />
                  </Show>
                  <Show when={sub() === "account"}>
                    <AccountPage
                      snapshot={loginSnapshot}
                      remember={rememberLogin}
                      cursor={cursor}
                      active={rowActive}
                    />
                  </Show>
                  <Show when={sub() === "lyrics"}>
                    <LyricsPage track={track} lines={lyrics} position={position} />
                  </Show>
                </>
              </SubPage>
          }
        >
          <>
            <TabBar
              active={tabIndex}
              cursor={tabIndex}
              focused={() => zone() === "content" && tabFocus()}
            />
            {/* 四个页签共用一套行驱动菜单：行数据由 rowsFor() 组装。 */}
            <MenuList
              debugName={`tab-${tabIndex()}`}
              rows={rowsFor()}
              cursor={cursor}
              start={start}
              active={rowActive}
            />
          </>
        </Show>
      </View>

      {displayOff() && <View class="absolute inset-0 z-50 bg-black" />}
    </View>
  );
}
