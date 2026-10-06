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
import { bgCls, nextTheme, setUiTheme } from "./core/theme";
import { applyCjkMode, cjkMode, logCjkStats } from "./core/cjk";
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

/* list/ 文件里的歌曲节点（原生写的统一结构）。 */
type ListSong = {
  id?: string;
  title?: string;
  artists?: string;
  album?: string;
  durationMs?: number;
  /** 1 = 网易云里已下架 / 无版权（标灰、不可播） */
  off?: number;
  /** 1 = 服务端 `privileges.pl == 0`（当前账号拿不到播放资源，多半是会员限定） */
  vip?: number;
  /** 服务端给的"这个账号能播的码率 / 音质档"（有就直接按它请求音质） */
  pl?: number;
  plLevel?: string;
  /** 网易云的 fee：1 = VIP 歌曲（只做标签，不锁播放） */
  fee?: number;
};

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
   * list/ 目录下的清单文件（原生每次启动后台刷一遍）：
   *   discover.json / daily.json / account_playlists.json / toplist_<id>.json
   * 界面读文件来显示 —— 一进应用就有内容，没网也能看上次的。
   */
  const [listFiles, setListFiles] = createSignal<
    /* `stamp` = 原生 listStat 给的"修改时间+大小"，直接当清单身份用。
     * 以前这里放的是**全文内容指纹**（hashOf 对整份原文逐字符算），
     * 实测真机日志 `perf: 指纹 toplist_3778678.json 335ms` —— 33KB 的文件
     * 要 300 多毫秒，四个榜单每 5 秒各来一次就是近 1 秒的纯浪费。
     * stat 戳本来就够用（变了就重读重解析，没变就跳过），指纹纯属多余。 */
    Record<string, { stamp: string; data: any }>
  >({});
  /* 清单文件的版本戳（见 loadList）：没变就不读文件。 */
  const listStamps: Record<string, string> = {};
  /* 内容指纹：只留一个 32 位整数，**不再把整份 raw 字符串留一份**
   * （榜单几十 KB × 好几份，parsed 对象之外再存一份原文是白占内存）。 */
  /*
   * 清单更新排队：解析完**不要立刻** setListFiles。
   *
   * 为什么：一次 `setListFiles` 会让所有依赖它的 memo（行列表 / listIds /
   * 可见行）在**同一帧**里全部重算。后台同步一口气写完 4 个榜单时，帧循环里
   * 连着 6 次 loadList ⇒ 一帧里最多 6 次级联 —— 真机日志里
   * `JS 帧 394ms ← 解析清单 toplist_3778678.json 29ms`（解析只 29ms，
   * 其余 365ms 全是级联）就是这么来的。
   *
   * 现在：解析结果先进队列，帧循环每 15 帧（≈0.25s）只应用一条。
   * 用户**手动打开**页面（openSub / 开歌单）时传 `immediate=true` 立即生效，
   * 该等的地方一秒都不多等。
   */
  const pendingList: { name: string; stamp: string; data: unknown }[] = [];
  const applyPendingList = () => {
    for (let i = 0; i < pendingList.length; i++) {
      const item = pendingList[i];
      if (listFiles()[item.name]?.stamp === item.stamp) {
        pendingList.splice(i, 1); /* 已经是这份内容了 */
        i -= 1;
        continue;
      }
      pendingList.splice(i, 1);
      setListFiles((p) => ({ ...p, [item.name]: { stamp: item.stamp, data: item.data } }));
      return; /* 一帧只应用一条：把级联摊到不同帧 */
    }
  };

  const loadList = (name: string, immediate = false, touched = false) => {
    if (!name) return;
    /*
     * `touched = true`：原生已经报过"这个文件刚被我们写过"，**跳过 stat 直接读**。
     * 真机上省下的不是几微秒 —— 一次 stat 是 35~86ms。
     */
    if (touched) {
      const raw = perfSpan("读清单 " + name, () => media()?.listRead?.(name) || "");
      if (!raw) return;
      const id = "t" + raw.length.toString();
      const prev = listFiles()[name];
      if (prev && prev.stamp === id) return;
      try {
        const data = perfSpan("解析清单 " + name, () => JSON.parse(raw));
        const dup = pendingList.findIndex((e) => e.name === name);
        if (dup >= 0) pendingList.splice(dup, 1);
        pendingList.push({ name, stamp: id, data });
      } catch {
        /* 坏文件当没有 */
      }
      return;
    }
    /* 先问版本戳（一次 stat，几微秒）：没变就直接返回，**不读整份文件**。
     * 以前每秒把发现页/歌单页的 JSON 从 SD 卡整份读出来再比字符串，
     * 榜单文件动辄几十 KB —— 这才是列表系统最大的一笔浪费。 */
    /* stat 也要计时：它每秒要对 6 个清单文件各来一次，而这段以前不在任何 span 里 ——
     * 真机日志里"慢帧几百毫秒却查不出谁花的"很可能就在这儿（尤其模拟器的文件层慢）。 */
    const stamp = perfSpan("listStat " + name, () => media()?.listStat?.(name) || "");
    if (stamp && stamp === listStamps[name]) return;
    const raw = perfSpan("读清单 " + name, () => media()?.listRead?.(name) || "");
    if (!raw) return;
    listStamps[name] = stamp || raw.length.toString();
    const prev = listFiles()[name];
    /* 身份就是 stat 戳（上面已经拿到了）。**不再算内容指纹** ——
     * 见 listFiles 的说明：那个逐字符哈希在真机上要几百毫秒，纯浪费。 */
    const id = stamp || raw.length.toString();
    if (prev && prev.stamp === id) return; /* 同一个版本，已经读过了 */
    try {
      const data = perfSpan("解析清单 " + name, () => JSON.parse(raw));
      if (immediate) {
        setListFiles((p) => ({ ...p, [name]: { stamp: id, data } }));
        return;
      }
      /* 同一个文件重复入队只留最新的一份 */
      const dup = pendingList.findIndex((e) => e.name === name);
      if (dup >= 0) pendingList.splice(dup, 1);
      pendingList.push({ name, stamp: id, data });
    } catch {
      /* 坏文件当没有 */
    }
  };
  const listData = <T,>(name: string): T | undefined =>
    listFiles()[name]?.data as T | undefined;

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
  /* 时长覆盖表：懒加载的条目不在曲库里，原生报回的真实时长只能记在这。 */
  const [cloudDur, setCloudDur] = createSignal<Record<string, number>>({});

  /*
   * 现在打开的那份清单：`nc:<id>` → 文件里的歌曲节点，**按需单查**。
   *
   * 以前这里是个 memo，一次把整份清单（527 首也照做）展开成一张 map ——
   * 清单一大，每次更新都要重建整张表，而界面一屏只看得到 6 行。
   * 现在只查"真的要渲染的那一条"，并把查过的记进小缓存；
   * 单查是 O(n) 的线性扫描，但每帧最多 6 次、且命中缓存后是 O(1)，
   * 比"每次重建 527 条"便宜得多（真机日志里那 900ms 级联就是它）。
   */
  /*
   * 歌单元数据索引：**每个歌单只建一次**，之后 O(1) 查。
   *
   * 走过的弯路（真机黑屏现场）：
   *   1) 最早是 memo，每次清单更新就把整份展开成 map（527 首 ⇒ 每 5 秒重建一次）；
   *   2) 改成"按需线性扫描"后，看起来省了，但**起播/换歌时会遍历整份列表逐个查**
   *      （`stepPlayable`/`getTrack` 每首一次）⇒ 527×527 次比较，一帧干 1~2 秒，
   *      直接顶爆 PocketJS 的 2 秒预算 → 黑屏（health.json: time budget exceeded）。
   * 正解是两头都要：**每个文件只建一次索引**，查的时候 O(1)；换歌单才重建。
   */
  let cloudMetaIndex: Record<string, ListSong> = {};
  let cloudMetaIndexFile = "";
  let cloudMetaIndexStamp = "";
  const cloudMetaFor = (id: string): ListSong | undefined => {
    const file = cloudOpen().file ?? "";
    const stamp = file ? listFiles()[file]?.stamp ?? "" : "";
    if (file !== cloudMetaIndexFile || stamp !== cloudMetaIndexStamp) {
      /* 换歌单、或这份清单被重新解析过：重建一次索引（O(n)，每次数据更新只一次）。 */
      const songs = file ? listData<{ songs?: ListSong[] }>(file)?.songs : undefined;
      const idx: Record<string, ListSong> = {};
      for (const s of songs ?? []) {
        if (s?.id) idx[cloudId(s.id)] = s;
      }
      cloudMetaIndex = idx;
      cloudMetaIndexFile = file;
      cloudMetaIndexStamp = stamp;
    }
    return cloudMetaIndex[id];
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
    if (!t.id.startsWith("nc:") || lazyKeep[t.id]) return;
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
    if (kept) return kept;
    const meta = cloudMetaFor(id);
    if (meta) {
      const built = buildCloudTrack(id, meta);
      keepLazy(built);
      return built;
    }
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
        title: info?.title || `[在线] ${nid}`,
        artists: info?.artists || "",
        album: info?.album || "",
        durationMs: info?.durationMs || 0,
        off: 0,
        vip: 0,
        fee: 0,
      });
      keepLazy(built);
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
  const rowsFor = createMemo<MenuRowData[]>(() => {
    const t = tabIndex();

    /* 发现：热门推荐（推荐歌单），点进去就是那张歌单 */
    if (t === 0) {
      const d = listData<{ playlists?: { id?: string; name?: string; count?: number }[] }>(
        "discover.json",
      );
      const rows: MenuRowData[] = [];
      const list = d?.playlists ?? [];
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
      return rows;
    }

    /* 榜单：每日推荐 + 热歌榜/飙升榜/新歌榜/原创榜 */
    if (t === 1) {
      const rows: MenuRowData[] = [];
      const daily = listData<{ songs?: ListSong[] }>("daily.json");
      rows.push({
        kind: "card",
        name: "每日推荐",
        value: daily?.songs?.length ? `${daily.songs.length} 首` : "登录后同步",
        file: "daily.json",
      });
      for (const top of TOPLISTS) {
        const f = listData<{ name?: string; songs?: ListSong[] }>(`toplist_${top.id}.json`);
        rows.push({
          kind: "item",
          name: f?.name || top.name,
          value: f?.songs?.length ? `${f.songs.length} 首` : `同步中…${syncProgText()}`,
          id: top.id,
          file: `playlist_${top.id}.json`,
        });
      }
      return rows;
    }

    /* 歌单：在线歌曲（playlist.json 的全部）→ 我的歌单（账号）→ 我喜欢的 */
    if (t === 2) {
      const acc = listData<{ list?: { id?: string; name?: string; count?: number }[] }>(
        "account_playlists.json",
      );
      const clouds = acc?.list ?? [];
      const cloudNames = new Set(clouds.map((p) => p?.name ?? ""));

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
      return rows;
    }

    /* 我的：固定几行 */
    const favLocal = favorites().filter((id) => !id.startsWith("nc:") && !!getTrack(id));
    const favOnline = favorites().filter((id) => id.startsWith("nc:"));
    return [
      { kind: "item", name: "账号", value: loginSnapshot().loggedIn ? "已登录" : "未登录" },
      { kind: "item", name: "我喜欢的（本地）", value: `${favLocal.length} 首` },
      { kind: "item", name: "我喜欢的（在线）", value: `${favOnline.length} 首` },
      { kind: "hint", name: "最近播放", value: "即将接入" },
      { kind: "item", name: "本地音乐", value: `${localIds().length} 首` },
      { kind: "item", name: "专辑", value: `${albums().length} 张` },
      { kind: "item", name: "设置", value: "" },
    ];
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
      if (file) {
        const songs = listData<{ songs?: ListSong[] }>(file)?.songs;
        if (songs?.length) return songs.length;
      }
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
      if (file) {
        const sid = listData<{ songs?: ListSong[] }>(file)?.songs?.[i]?.id;
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
        const songs = listData<{ songs?: ListSong[] }>(file)?.songs;
        if (songs?.length) {
          const out: string[] = [];
          for (const song of songs) {
            if (song?.id) out.push(cloudId(song.id));
          }
          if (out.length > 0) return out;
        }
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
    if (now - lastForceMs < 60_000) return;
    lastForceMs = now;
    logMsg("ui: 强制刷在线清单");
    media()?.listSync?.(1);
  };

  /*
   * 清单文件的"新鲜度"。
   *
   * 榜单 / 歌单文件是**打开时**才落盘的：登录之前写下的那些会带一大堆下架
   * 标记（真机上出现过 200 首里 159 首标灰 —— 那是匿名会话拿到的权限结果）。
   * 文件超过 10 分钟就让原生重拉一次（原生自己有 60 秒 TTL，不会打爆接口），
   * 拉到的会落盘，下一拍 loadList 就读到新的了。
   */
  const FILE_MAX_AGE_MS = 10 * 60 * 1000;
  let lastStaleAskMs = 0;
  const refreshListIfStale = () => {
    const open = cloudOpen();
    if (!open.id || !open.file) return;
    const at = Number((listData<{ at?: number }>(open.file) || {}).at) || 0;
    if (at > 0 && Date.now() - at < FILE_MAX_AGE_MS) return;
    /* 拉取失败时 at 不会更新，别每 5 秒问一次原生（真正的节流点）。 */
    const now = Date.now();
    if (now - lastStaleAskMs < 60_000) return;
    lastStaleAskMs = now;
    logMsg("ui: 清单文件偏旧，让原生重拉 id=" + open.id);
    media()?.netPlaylistTracks?.(open.id);
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
    if (row.file) {
      loadList(row.file, true); /* 用户刚点的：立即生效，别排队等 0.25s */
      const d = listData<{ name?: string; songs?: ListSong[] }>(row.file);
      if (d?.name) name = d.name;
      if (d?.songs?.length) {
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
      media()?.netPlaylistTracks?.(row.id);
      forceListSync();
    }
    openSub("playlist");
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

    /* list/ 清单：先把上次落下的文件读进界面，再让原生后台刷一遍
     * （热推荐 / 每日推荐 / 我的歌单 / 四个榜单，原生侧 10 分钟 TTL）。 */
    loadList("discover.json");
    loadList("daily.json");
    loadList("account_playlists.json");
    for (const top of TOPLISTS) loadList(`toplist_${top.id}.json`);
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
      logMsg("ui: 会话就绪，强制刷清单");
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
    /*
     * 长歌单里"按回去要按酸手"（真机反馈）：△ 先在**歌曲列表子页**里
     * 一跳回到第一首（光标 0 是顶部那个 ← ，第一首是 1）；
     * 已经在最上面（光标 ≤1 或光标停在标题栏）时，再按 △ 才是返回。
     * 也就是"△ 回顶，已在顶部时 △ 返回"。
     */
    const songListSub =
      sub() === "local" ||
      sub() === "online" ||
      sub() === "favorites" ||
      sub() === "favoritesOnline" ||
      sub() === "album" ||
      sub() === "playlist";
    if (songListSub && !headerFocus() && cursor() > 1) {
      setCursor(1);
      setStart(0);
      logMsg("list: △ 回到第一首");
      return;
    }
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

    /* list/ 清单文件：发现页、我的歌单每秒看一次；每日推荐 + 四个榜单每 5 秒看一次。
     * 原生只在启动时真刷一遍（10 分钟 TTL），这里只是把文件内容读进界面。 */
    /* 每 15 帧（≈0.25s）把队列里的一条清单更新应用到界面：把"一帧 6 次级联"
     * 摊成"6 帧各一次"，切页面/滚动就不再被后台同步的落盘打断。 */
    if (frameCounter % 15 === 0) applyPendingList();
    /*
     * **事件驱动**刷新清单（任务书 ⑦）：不再"每秒问 6 个文件变没变"。
     *
     * 真机实测：一次 `listStat` 要 **35~86ms**（SD 卡 metadata IO），每秒 6 次就是
     * 200ms+ 的固定开销，也是剩下那些 `JS 帧 130~390ms ← listStat/读清单/解析清单`
     * 的全部来源。文件本来就是我们原生侧自己写的 —— 写的时候顺手记一笔名字，
     * 界面每 0.5 秒取一次"被写过谁"，只有名单里的文件才去读（跳过 stat）。
     *
     * 另外保留一条**慢速扫查**（每 15 秒）兜底：用户从电脑往卡里丢文件时，
     * 那不是我们写的，只有扫查才能发现。代价从"每秒 6 次 stat"降到"每 15 秒 6 次"。
     */
    if (frameCounter % 30 === 0) {
      const touched = perfSpan("listTouched", () => media()?.listTouched?.() || "");
      if (touched) {
        for (const name of touched.split(",")) {
          const n = name.trim();
          if (n) loadList(n, false, true); /* 已知变过：跳过 stat，直接读 */
        }
      }
    }
    if (frameCounter % 900 === 0) {
      /* 慢速兜底扫查：只有这条路径还会 stat（每 15 秒一轮）。 */
      loadList("discover.json");
      loadList("account_playlists.json");
      loadList("daily.json");
      for (const top of TOPLISTS) loadList(`toplist_${top.id}.json`);
    }
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
        if (txt !== syncProg()) setSyncProg(txt);
      } catch {
        /* 拿不到就当没有进度 */
      }
    }
    /* 登录了却一直没有 account_playlists.json：每分钟再催一次（forceListSync
     * 自带 60 秒节流），超过 60 秒还没来就在界面上写"同步失败"。
     * 以前这里只会一直显示"同步中…"，用户根本分不出是慢还是坏了。 */
    if (frameCounter % 300 === 0) {
      const hasAccount = !!listData<{ list?: unknown[] }>("account_playlists.json");
      if (hasAccount) {
        if (accountSyncLate()) setAccountSyncLate(false);
      } else if (loginSnapshot().loggedIn) {
        const waited = Date.now() - accountSyncSinceAt;
        if (waited > 60_000 && !accountSyncLate()) setAccountSyncLate(true);
        if (waited > 15_000) forceListSync();
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
        /*
         * 打开中的歌单：以前**每秒**都 `loadList(open.file, true)` —— 真机一次 stat 35~86ms，
         * 这是"歌单选首后就一直卡"的一笔固定开销。文件若被原生写过，`listTouched`
         * 那条事件路径会把它读进来；这里只做**很慢的兜底扫查**（每 15 秒）。
         */
        if (frameCounter % 900 === 0) loadList(open.file, true);
        const d = listData<{ name?: string; songs?: ListSong[] }>(open.file);
        if (d?.name) name = d.name;
        if (d?.songs?.length) songs = d.songs;
      }
      if (songs) {
        /* 文件没变就什么都不做（adoptListFile 自己判重）——
         * 以前每秒把整张榜单重并一遍，是卡顿的主因。 */
        const renamed = name !== open.name;
        if (renamed) setPlaylistName(name);
        if (renamed || open.state !== "ready") {
          setCloudOpen((p) => ({ ...p, name, state: "ready" }));
        }
      } else if (open.id && frameCounter % 60 === 0) {
        /* 每秒问一次原生"拉到没有"（它自己 60 秒 TTL + 后台线程，问了不亏）。 */
        const raw = perfSpan("netPlaylistTracks", () => media()?.netPlaylistTracks?.(open.id) || "");
        try {
          const parsed = JSON.parse(raw) as {
            state?: string;
            name?: string;
            message?: string;
            songs?: ListSong[];
          };
          if (parsed?.state === "auth") {
            setCloudOpen((p) => ({ ...p, state: "failed", message: "登录后同步" }));
          } else if (parsed?.state === "failed") {
            setCloudOpen((p) => ({
              ...p,
              state: "failed",
              message: parsed.message || "",
            }));
          }
        } catch {
          /* 下次再问 */
        }
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
                      when={cloudOpen().id === "" || cloudOpen().state === "ready" || listCount() > 0}
                      fallback={
                        <PlaceholderPage
                          text={
                            cloudOpen().state === "failed"
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
