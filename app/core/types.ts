/* YUNYIN 数据类型：曲目 / 专辑 / 页面 / 原生桥。
 *
 * 只放类型与接口，不放任何运行时代码 —— 所有模块都可以安全地引用它们。
 */

/* =========================================================
 * NATIVE MEDIA BRIDGE (globalThis.vitaMedia)
 *
 * native/media.rs 暴露：list / roots / play / pause / resume /
 * stop / state / cover / tags。state() 返回
 * { playing, paused, path, pos, dur, rate, dec }，pos/dur 为毫秒。
 * ======================================================= */

export type VitaMedia = {
  list?: (path: string) => string;
  roots?: () => string;
  play?: (path: string) => void;
  pause?: () => void;
  resume?: (path?: string) => void;
  stop?: () => void;
  state?: () => string;
  cover?: (path: string) => number;
  tags?: (path: string) => string;
  /* 在线曲目清单（卡里 ux0:/data/yunyin/playlist.json 描述的歌单），
   * 返回 [{"url":..,"title":..,"referer":..}]；没有这个文件时是 "[]"。 */
  netplay?: () => string;
  /* Phase 4 扫码登录（全部非阻塞：界面只读状态，网络在后台线程）。 */
  netLoginStart?: () => void;
  netLoginTick?: () => void;
  netLoginState?: () => string;
  /** Native Rust QR worker result: texture handle, or -1 while not ready. */
  netLoginQr?: () => number;
  /**
   * 同步进度：`{"kind":"lists","done":3,"total":7}`（步数）
   * 或 `{"kind":"download","done":123,"total":456}`（正文下载字节数，界面换算成百分比）。
   * 空闲时 kind 为空串。
   */
  netSyncProgress?: () => string;
  /** 让原生后台预取下一首（传网易云歌曲 id；空串 = 只取消）。 */
  netPrefetchNext?: (id: string) => void;
  /** 取消预取（本地歌 / 队列到头 / 停止播放时调）。 */
  netPrefetchCancel?: () => void;
  /** 每次按键调一次：预取会据此在 3 秒内让路，保证按键响应不被后台下载挤。 */
  netUserActive?: () => void;
  netLoginRemember?: (on: number) => void;
  netLogout?: () => void;
  /* 在线歌曲详情（歌名/歌手/专辑/封面/时长），首次调用后台取数并缓存。 */
  netSongInfo?: (id: string) => string;
  /* 批量详情（逗号分隔的 ID）：一次问多首，回缓存里已有的那些。 */
  netSongsInfo?: (idsCsv: string) => string;
  /* 某张歌单的歌曲：{"state":"…","name":"…","songs":[{id,title,artists,album,durationMs}]}
   * 榜单/歌单的常规路径是读 list/ 里的文件；只有文件还没生成时才调它兜底。 */
  netPlaylistTracks?: (id: string) => string;
  /** 只触发 native 后台拉取，结果由 catalog version + page 读取。 */
  netPlaylistRequest?: (id: string) => void;
  /** Native-owned catalog snapshots; results are bounded to menu/page views. */
  netCatalogVersion?: () => string;
  netCatalogMenu?: (kind: string) => string;
  netCatalogPage?: (name: string, offset: string, limit: string) => string;
  netCatalogIds?: (name: string) => string;
  /* 读 ux0:/data/yunyin/list/ 下的清单 JSON（不存在返回空串）。 */
  listRead?: (name: string) => string;
  /* 清单文件的版本戳 "大小,修改时间ms"（不存在返回空串）：
   * 界面先用它判断变没变，变了才 listRead 读整份 JSON。 */
  listStat?: (name: string) => string;
  /**
   * 自上次调用后被写过的清单文件名（逗号分隔，没有变化时是空串）。
   *
   * 界面据此做**事件驱动**刷新：空串 ⇒ 一个文件操作都不做。
   * 真机实测一次 `listStat` 要 35~86ms（SD 卡 metadata IO），所以"每秒 6 次 stat"
   * 曾经是最大的卡顿来源。
   */
  listTouched?: () => string;
  /* 预热一首歌的播放地址（只解析+缓存，不播放）：
   * 当前歌在放时把"下一首"的地址先解析好，按下一首时省掉一次 POST + 首包。 */
  netPreload?: (songId: string, level?: string) => void;
  /* 触发一次后台清单同步（默认有 10 分钟 TTL；传 1 强制立刻刷，登录后用）。 */
  listSync?: (force?: number) => void;
  /* 当前在线流的缓冲进度："已缓冲字节,总字节"（总长未知时第二项是 0）。
   * 界面拿它画进度条里那根浅色的缓存条。 */
  netBuffer?: () => string;
  setPsLock?: (on: boolean) => number;
  logEnabled?: () => number;
  store_get?: (key: string) => string;
  store_set?: (key: string, value: string) => void;
};

export type FsEntry = { name: string; path: string; dir: boolean };

export type PlaybackMode = "sequence" | "repeat-one";

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

export interface Track {
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

  /* 所属在线歌单名（playlist.json 里的 name）。空 = 没写清单，归到"在线歌曲"。 */
  playlist?: string;

  /*
   * 从网易云歌单 / 榜单临时并进来的歌（点开某个清单才出现的那种）。
   * 这类歌**只属于当前打开的那份清单**：不进"歌单"页的清单分组，
   * 也不算进"在线歌曲 · 全部 N 首"（那两个入口只认卡里的 playlist.json）。
   */
  cloud?: boolean;

  /* 网易云里已下架 / 无版权：列表里照样显示，但标灰、不可播、不进队列。 */
  off?: boolean;

  /* 需要会员 / 暂无版权：CDN 用 403 拒掉了加密码地址（登录账号没这个权限）。
   * 和 off 一样标灰、跳歌时跳过；区别只是行尾写"会员"而不是"下架"。 */
  vip?: boolean;

  /* 网易云的 `fee`：1 = VIP 歌曲。**只作为标签**显示（ClouDS-Music 的做法），
   * 不参与"能不能播"的判断——VIP 账号对这类歌照样能播。 */
  fee?: number;
}

export interface Album {
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

export interface LyricLine {
  /* milliseconds */
  time: number;
  text: string;
}
