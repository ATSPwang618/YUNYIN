/* 流式 CJK 字库（PJFA）：模式切换 + 诊断日志 + StreamText 组件。
 * 这块在任务书的「不可动清单」里 —— 本次只做零行为搬迁，逻辑一字未改。 */
import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import { Text } from "@pocketjs/framework/components";
import { openFontArchive, type TextResource } from "@pocketjs/framework/fonts";
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
        ",err:" + (st?.error ?? "") + "} want=" + want,
    );
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
 *   1024 条 × 26×36 × 2 槽 ≈ 1.83 MiB，落在预算内（留了约 10% 余量）。
 *
 * 为什么不能写死一个大数：配置超预算时引擎直接拒绝 → 整个流式字库进 error，
 * 表现就是"字还是方框"。所以被拒就退到下一档；384 是老版本的可用值，做最后兜底。
 * 列表页一屏（十几行标题+歌手）动辄两三百个不同的字，384 那点容量正是
 * "有些行整行空白"的来源之一。
 */
const CAPACITY_LADDER = [1024, 768, 512, 384];
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
 * - 字形还在取的时候，先按同样的布局画一段**淡色**的烘焙文字占位：早期这里
 *   用的是"全透明"占位，本意是避免先闪 口 再换字；但真机反馈是"一屏空白"
 *   （字形要一帧四个地取回来，列表页第一屏几百个字要等一两秒）——
 *   宁可先给个淡色底稿，也别让整行看不见字。字齐了整体换成清晰字形，布局不变；
 * - 真的取不到（字库没有这个字 / 出错）才退回**可见**的烘焙文字 —— 那时看到
 *   的 口 就是字库确实缺字，不是没加载完。 */
export function StreamText(props: { class: string; text: string }) {
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
            /* 0.45：够看清"这里有字"，又明显比正式字形淡（一眼能看出还没加载完）。 */
            <Text class={props.class} style={{ opacity: 0.45 }}>{props.text}</Text>
          )}
          errorFallback={() => <Text class={props.class}>{props.text}</Text>}
        />
      )}
    </Show>
  );
}
