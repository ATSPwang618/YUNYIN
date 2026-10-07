/* 流式 CJK 字库（PJFA）：模式切换 + 诊断日志 + StreamText 组件。
 * 这块在任务书的「不可动清单」里 —— 本次只做零行为搬迁，逻辑一字未改。 */
import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import { Text } from "@pocketjs/framework/components";
import { openFontArchive, type PreparedText, type TextResource } from "@pocketjs/framework/fonts";
import type { ResourceState } from "@pocketjs/framework/resource";
import { logMsg, media } from "./media";

export type CjkMode = "baked" | "stream";
export const [cjkMode, setCjkMode] = createSignal<CjkMode>(
  (() => {
    try {
      /*
       * 默认走**内置**（烘进主图的 GB2312 一级 3755 字）：启动不用开字库文件、
       * 不占额外内存，冷启动最稳。想显示生僻字/日文名的，去设置页切"流式"
       * （按需从 fonts/cjk.pjfa 取字形），卡里的 store 记住选择。
       */
      return media()?.store_get?.("cjkMode") === "stream" ? "stream" : "baked";
    } catch {
      return "baked";
    }
  })(),
);
const [cjkStatus, setCjkStatus] = createSignal<"off" | "opening" | "ready" | "error">("off");
export const [cjkEpoch, setCjkEpoch] = createSignal(0);
let cjkFont: ReturnType<typeof openFontArchive> | undefined;

/*
 * 字体资源不属于某个 View/item：TextResource 只是一次字体批次的句柄，
 * 真正的字形和布局缓存由 PocketJS core 持有。这里再加一层很小的共享
 * lease cache，避免 Recycler 槽位回收时立刻 dispose，再在下一帧 prepare
 * 同一段文字。lease 计数归零后仍保留一段时间，超过上限才按 LRU 释放。
 * 这样 item 的生命周期和字体缓存生命周期彻底解耦。
 */
type SharedText = {
  key: string;
  text: string;
  slot: number;
  resource?: TextResource;
  resourceUnsubscribe?: () => void;
  error?: unknown;
  /* Always keep the framework subscribers on this wrapper.  Subscribing
   * directly to TextResource after prepare races with the resource becoming
   * ready, and can leave a Text boundary on its fallback forever. */
  subscribers: Set<() => void>;
  lastStateSignature?: string;
  refs: number;
  used: number;
};
/* PocketJS core 的 TextResource batch 上限是 32；共享缓存不能把已释放
 * 的 batch 长时间留满，否则新页面会收到“Text batch count exceeds budget”。 */
const SHARED_TEXT_LIMIT = 24;
const sharedTexts = new Map<string, SharedText>();
const sharedPrepareQueue: SharedText[] = [];
let sharedTextClock = 0;
let sharedTextHitsWindow = 0;
let sharedTextMissesWindow = 0;
let sharedTextEvictionsWindow = 0;
let sharedPrepareQueuedWindow = 0;
let sharedPreparePumpedWindow = 0;
let sharedPrepareQueuePeakWindow = 0;

const pendingTextState = (): ResourceState<never> => ({ status: "pending" });

const sharedTextState = (entry: SharedText): ResourceState<PreparedText> =>
  entry.resource?.state() ?? (entry.error
    ? { status: "error", error: entry.error }
    : pendingTextState());

/*
 * TextResource notifications are edge-triggered.  A notification can be
 * missed while a resource is handed from the prepare queue to the framework
 * boundary, especially during page exit/re-entry.  Keep a small level-triggered
 * check as well: the frame pump compares the current state and wakes the
 * subscribers whenever pending -> ready/error is observed.  This is cheap
 * (one state read per live shared text) and prevents a permanent loading item.
 */
const reconcileSharedText = (entry: SharedText, force = false): void => {
  const state = sharedTextState(entry);
  const signature = state.status === "error"
    ? `error:${String(state.error)}`
    : state.status;
  if (!force && entry.lastStateSignature === signature) return;
  entry.lastStateSignature = signature;
  for (const subscriber of [...entry.subscribers]) subscriber();
};

const reconcileSharedTexts = (): void => {
  for (const entry of sharedTexts.values()) reconcileSharedText(entry);
};

/*
 * `prepareText()` 会做字形收集、去重、描述符分配，并可能触发 native
 * admission。它虽然不是网络请求，但在冷启动时一屏几十个文本一起调用，
 * 会把整批工作挤进同一个 drawlist 帧。这里把它改成显式的 frame pump：
 * View 只拿一个 pending lease，真正的 prepare 每帧最多推进一个。
 *
 * 这不是把工作丢给一个不存在的 JS 子线程，而是把主线程上的同步工作
 * 切成可调度的小批次；native 字形请求本身仍由 PocketJS 的异步 offload
 * 处理。这样布局可以先保持固定 advance，字体准备完成后再通知 Text 刷新。
 */
export const pumpCjkPrepare = (budget = 1): number => {
  if (budget < 1 || !cjkFont) return 0;
  let pumped = 0;
  while (pumped < budget && sharedPrepareQueue.length > 0) {
    const entry = sharedPrepareQueue.shift()!;
    if (entry.refs === 0 || sharedTexts.get(entry.key) !== entry) continue;
    try {
      const t0 = Date.now();
      entry.resource = cjkFont.prepareText(entry.text, { slot: entry.slot });
      /* 关键：prepareText 返回时通常仍是 pending。必须把底层资源的
       * ready/error 通知桥接到 wrapper 的订阅者，否则 native 已上传字体
       * 但 Text 永远停在 fallback，直到其它 UI 状态碰巧触发重算。 */
      entry.resourceUnsubscribe = entry.resource.subscribe(() => {
        reconcileSharedText(entry);
      });
      streamPrepareWindow += 1;
      streamPrepareMsWindow += Date.now() - t0;
      sharedPreparePumpedWindow += 1;
      reconcileSharedText(entry, true);
    } catch (error) {
      const message = String(error);
      if (message.includes("Text batch count exceeds budget")) {
        /* 这是可恢复的容量竞争，不是字体错误：先驱逐一个 refs=0 的
         * lease，当前项重新排队，等已有 item 释放后再提交。 */
        let victim: SharedText | undefined;
        for (const candidate of sharedTexts.values()) {
          if (candidate !== entry && candidate.refs === 0 &&
              (!victim || candidate.used < victim.used)) victim = candidate;
        }
        if (victim) {
          sharedTexts.delete(victim.key);
          victim.resourceUnsubscribe?.();
          victim.resource?.dispose();
          sharedTextEvictionsWindow += 1;
        } else {
          /* Do not drop the lease when every existing batch is still in use.
           * The old early return removed the entry from the queue permanently,
           * so its item stayed on "加载中…" even after another page released
           * the capacity.  Keep it queued and retry on the next pump. */
          sharedPrepareQueue.unshift(entry);
          return pumped;
        }
        sharedPrepareQueue.push(entry);
        continue;
      }
      entry.error = error;
      streamPrepareErrorsWindow += 1;
      reconcileSharedText(entry, true);
    }
    pumped += 1;
  }
  reconcileSharedTexts();
  return pumped;
};

export type CjkWarmupItem = { text: string; slot: number };
export type CjkWarmupTicket = {
  ready: () => boolean;
  pending: () => number;
  dispose: () => void;
};

/* 页面 loading 使用的预热票据：先把可见窗口登记进共享缓存，页面暂不挂
 * 真实 Text 节点；frame pump 慢慢准备，全部 ready/error 后才揭示列表。 */
export const beginCjkWarmup = (items: readonly CjkWarmupItem[]): CjkWarmupTicket => {
  if (cjkMode() !== "stream" || !cjkFont) {
    return { ready: () => true, pending: () => 0, dispose: () => {} };
  }
  const unique = new Map<string, CjkWarmupItem>();
  for (const item of items) {
    if (!item.text) continue;
    unique.set(`${item.slot}\u0000${item.text}`, item);
  }
  const leases = [...unique.values()].map((item) => acquireSharedText(item.text, item.slot));
  let disposed = false;
  const pending = () => leases.reduce((n, lease) => n + (lease.state().status === "pending" ? 1 : 0), 0);
  return {
    ready: () => pending() === 0,
    pending,
    dispose: () => {
      if (disposed) return;
      disposed = true;
      for (const lease of leases) lease.dispose();
    },
  };
};

const trimSharedTexts = (): void => {
  while (sharedTexts.size > SHARED_TEXT_LIMIT) {
    let victim: SharedText | undefined;
    for (const entry of sharedTexts.values()) {
      if (entry.refs !== 0) continue;
      if (!victim || entry.used < victim.used) victim = entry;
    }
    if (!victim) return; /* 所有资源仍被 View 使用，不能强行回收。 */
    sharedTexts.delete(victim.key);
    victim.resourceUnsubscribe?.();
    victim.resource?.dispose();
    sharedTextEvictionsWindow += 1;
  }
};

const clearSharedTexts = (): void => {
  sharedPrepareQueue.length = 0;
  for (const entry of sharedTexts.values()) {
    entry.resourceUnsubscribe?.();
    entry.resource?.dispose();
  }
  sharedTexts.clear();
};

const acquireSharedText = (text: string, slot: number): TextResource => {
  const key = `${slot}\u0000${text}`;
  let entry = sharedTexts.get(key);
  if (entry) {
    entry.refs += 1;
    entry.used = ++sharedTextClock;
    sharedTextHitsWindow += 1;
  } else {
    if (!cjkFont) throw new Error("Font archive is not ready");
    entry = {
      key,
      text,
      slot,
      refs: 1,
      used: ++sharedTextClock,
      subscribers: new Set(),
    };
    sharedTexts.set(key, entry);
    sharedPrepareQueue.push(entry);
    sharedPrepareQueuedWindow += 1;
    sharedPrepareQueuePeakWindow = Math.max(sharedPrepareQueuePeakWindow, sharedPrepareQueue.length);
    sharedTextMissesWindow += 1;
    trimSharedTexts();
  }
  let released = false;
  return {
    state: () => sharedTextState(entry!),
    subscribe(fn) {
      entry!.subscribers.add(fn);
      return () => entry!.subscribers.delete(fn);
    },
    dispose() {
      if (released) return;
      released = true;
      entry!.refs = Math.max(0, entry!.refs - 1);
      entry!.used = ++sharedTextClock;
      trimSharedTexts();
    },
  };
};

/* 每个统计窗口累计 StreamText 的同步工作量。不要在每个字上写日志：日志本身
 * 会放大卡顿；由帧循环每 60 帧汇总一次，和 host 的 atlas 上传日志对照。 */
let streamPrepareWindow = 0;
let streamPrepareMsWindow = 0;
let streamReleaseWindow = 0;
let streamReleaseMsWindow = 0;
let streamPrepareErrorsWindow = 0;

/* 供歌词页预取复用同一个字库句柄（只读访问器，避免导出可变绑定）。 */
export const fontArchive = () => cjkFont;
/* STREAM 模式的诊断日志（只有卡里有 ux0:/data/yunyin/debug 时才真的写）。
 * host 里：resident = 常驻字形数，inked = 其中真的有墨的个数
 * （inked 一直是 0 就说明字库读出来的点阵是空的），pending = 还在等的请求，
 * rejected = 没空位被拒，unsupported = 字库里确实没有这个字；
 * js 里：loaded = JS 侧累计提交成功的字数，requests = 还在飞的请求数。 */
export const logCjkStats = (): void => {
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
        ",err:" + (st?.error ?? "") +
        ",prepare:" + streamPrepareWindow +
        ",prepare_ms:" + streamPrepareMsWindow +
        ",release:" + streamReleaseWindow +
        ",release_ms:" + streamReleaseMsWindow +
        ",prepare_err:" + streamPrepareErrorsWindow +
        ",prepare_queue:" + sharedPrepareQueue.length +
        ",prepare_queued:" + sharedPrepareQueuedWindow +
        ",prepare_pumped:" + sharedPreparePumpedWindow +
        ",prepare_peak:" + sharedPrepareQueuePeakWindow +
        ",cache_hit:" + sharedTextHitsWindow +
        ",cache_miss:" + sharedTextMissesWindow +
        ",cache_evict:" + sharedTextEvictionsWindow +
        "} want=" + want,
    );
    streamPrepareWindow = 0;
    streamPrepareMsWindow = 0;
    streamReleaseWindow = 0;
    streamReleaseMsWindow = 0;
    streamPrepareErrorsWindow = 0;
    sharedPrepareQueuedWindow = 0;
    sharedPreparePumpedWindow = 0;
    sharedPrepareQueuePeakWindow = 0;
    sharedTextHitsWindow = 0;
    sharedTextMissesWindow = 0;
    sharedTextEvictionsWindow = 0;
  } catch {
    /* ignore */
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

export const slotFromClass = (cls: string): number => {
  const bold = /\bfont-bold\b/.test(cls);
  if (/\btext-sm\b/.test(cls)) return bold ? 8 : 1;
  return bold ? 7 : 0;
};

export const applyCjkMode = (next: CjkMode): void => {
  setCjkMode(next);
  try {
    media()?.store_set?.("cjkMode", next);
  } catch {
    /* ignore */
  }
  if (next !== "stream") {
    clearSharedTexts();
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
  capacityIdx = 0; /* 用户主动切过来：从最大容量重新试 */
  openStreamArchive();
};

/*
 * 预留字形条数（容量）阶梯 —— 从大到小试。
 *
 * 容量 = 这个字库同时能"钉"住多少个字形。引擎的源位图预算是**共享 2 MiB**
 * （两个槽共同分），按 26×36 的格子算最多约 1120 条：
 *   1120 条 × 26×36 × 2 槽 ≈ 1.999 MiB，刚好落在 2 MiB 预算内。
 *
 * 为什么不能写死一个大数：配置超预算时引擎直接拒绝 → 整个流式字库进 error，
 * 表现就是"字还是方框"。所以被拒就退到下一档；384 是老版本的可用值，做最后兜底。
 * 列表页一屏（十几行标题+歌手）动辄两三百个不同的字，384 那点容量正是
 * "有些行整行空白"的来源之一。
 */
const CAPACITY_LADDER = [1120, 1024, 768, 512, 384];
let capacityIdx = 0;

/* 真正开字库：容量从阶梯里取；被引擎拒绝（超源位图预算）就自动退到下一档。 */
const openStreamArchive = (): void => {
  try {
    setCjkStatus("opening");
    cjkFont = openFontArchive({
      path: "fonts/cjk.pjfa",
      /*
       * 只要 0（常规）和 7（加粗）两个槽。
       *
       * 为什么去掉 8：界面已经全部用 12px（text-xs），slot 8（14px 加粗）在引擎里
       * 没有对应尺寸的字模，真机上 openFontArchive 直接报
       * `Font slot 8 incompatible or exceeds residency budget` → 整个流式字库起不来
       * （表现就是字还是方框）。pjfa 也要按同一组槽位重烘（scripts/bake-cjk-archive.ts）。
       */
      slots: [0, 7],
      provider: "local",
      capacity: CAPACITY_LADDER[capacityIdx],
      maxBytes: 2 * 1024 * 1024,
      onChange: () => {
        const st = cjkFont?.status().state;
        if (st === "ready") setCjkStatus("ready");
        else if (st === "error") {
          /*
           * 顺序要紧：**先把这个作废的句柄摘下来再 dispose** —— dispose 内部
           * 会回调 onChange，那时候不能再动已经作废的句柄。
           */
          const dead = cjkFont;
          const next = capacityIdx + 1;
          clearSharedTexts();
          cjkFont = undefined;
          try {
            dead?.dispose();
          } catch {
            /* ignore */
          }
          if (next < CAPACITY_LADDER.length) {
            logMsg(
              "cjk: 容量 " + CAPACITY_LADDER[capacityIdx] +
                " 被拒（超源位图预算），回退 " + CAPACITY_LADDER[next],
            );
            capacityIdx = next;
            openStreamArchive();
            return;
          }
          logMsg(
            "cjk: 容量 " + CAPACITY_LADDER[capacityIdx] + " 仍被拒，流式字库关闭：" +
              String(dead?.status().error ?? ""),
          );
          setCjkStatus("error");
        } else if (st === "warming" || st === "opening") setCjkStatus("opening");
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
 * - 字形还在取的时候，只保留同样的文字和字体度量，但不绘制烘焙字形：这样布局
 *   不跳动，也不会先闪出 GID 0 的"口"。字形缓存齐了之后整体换成清晰字形；
 * - 真的取不到（字库没有这个字 / 出错）才退回**可见**的烘焙文字 —— 那时看到
 *   的 口 就是字库确实缺字，不是没加载完。 */
export function StreamText(props: { class: string; text: string }) {
  const [res, setRes] = createSignal<TextResource | undefined>();
  let key = "";
  let current: TextResource | undefined;

  const release = () => {
    if (current) {
      const t0 = Date.now();
      current.dispose();
      streamReleaseWindow += 1;
      streamReleaseMsWindow += Date.now() - t0;
    }
    current = undefined;
    setRes(undefined);
  };

  createEffect(() => {
    const mode = cjkMode();
    void cjkEpoch();
    const text = props.text;
    const slot = slotFromClass(props.class);
    const usable =
      mode === "stream" && !!cjkFont && (slot === 0 || slot === 7);
    const next = usable ? slot + "\u0000" + text : "";
    /* 颜色 / 高亮变化：key 没变就直接复用，绝不重新申请字形。 */
    if (next === key && current) return;
    key = next;
    release();
    if (!usable || !cjkFont) return;
    try {
      const t0 = Date.now();
      current = acquireSharedText(text, slot);
      streamPrepareWindow += 1;
      streamPrepareMsWindow += Date.now() - t0;
      setRes(current);
    } catch {
      streamPrepareErrorsWindow += 1;
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
            /* pending 只替换当前 Text，不动外层 list / Recycler。分页时
             * 槽位仍然存在，只显示 item 级 loading，ready 后由订阅回调
             * 原地换成真正文字。 */
            <Text class={props.class}>加载中…</Text>
          )}
          errorFallback={() => <Text class={props.class}>{props.text}</Text>}
        />
      )}
    </Show>
  );
}
