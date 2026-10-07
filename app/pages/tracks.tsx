import { createEffect, createMemo, createSignal, Show, untrack } from "solid-js";
import { Text, View } from "@pocketjs/framework/components";
import { type Track } from "../core/types";
import { clipW } from "../core/util";
import { pTxt } from "../core/theme";
import { TrackRow } from "../components/rows";
import { FocusIndicator } from "../components/focus_indicator";
import { logEnabled, logMsg } from "../core/media";

/* 通用曲目列表（本地音乐 / 在线歌曲 / 我喜欢的 / 专辑详情 共用）。
 * 只渲染可见的 5 行 + 一行计数；焦点行由上层 cursor/active 决定。 */

/* 一屏显示几行（行高 30 + 行距 4）：内容区 256 高，
 * 6 行 = 180，加上页签行 / 子页头 / 计数行刚好不溢出。 */
export const LIST_WINDOW = 6;
/* 行高 30 + 行距 4（容器 gap-1）：焦点指示器每次移动的步长。 */
const ROW_PITCH = 34;
/* 多留一行前后缓冲。槽位本身固定不变，滚动一行只把离开窗口的槽位
 * 重新绑定到新进入的索引，和 RecyclerView 的 ViewHolder 回收一致。 */
const RECYCLE_POOL = LIST_WINDOW + 2;

type TrackSlot = {
  index: () => number;
  setIndex: (value: number) => void;
};

function RecycledTrackSlot(props: {
  slot: TrackSlot;
  count: () => number;
  start: () => number;
  idAt: (index: number) => string;
  getTrack: (id: string) => Track | undefined;
  currentId: () => string;
}) {
  const entry = createMemo(() => {
    const index = props.slot.index();
    if (index < 0 || index >= props.count()) return undefined;
    const id = props.idAt(index);
    const song = id ? props.getTrack(id) : undefined;
    return song ? { index, id, song } : undefined;
  });

  return (
    <Show when={entry()}>
      {(value) => (
        <View
          style={{
            posType: 1,
            insetT: (props.slot.index() - props.start()) * ROW_PITCH,
            insetL: 0,
            insetR: 0,
            height: 30,
          }}
        >
          <TrackRow
            index={value().index + 1}
            title={clipW(value().song.title, 26)}
            artist={clipW(value().song.artist, 30)}
            current={props.currentId() === value().id}
            off={value().song.off}
            vip={value().song.vip}
            vipped={value().song.fee === 1}
          />
        </View>
      )}
    </Show>
  );
}

export function TrackListPage(props: {
  /** 列表总条数（O(1)，**不要**为此物化整表）。 */
  count: () => number;
  /** 第 i 条的 id —— 只算这一条（大歌单靠它避免 O(n) 重建）。 */
  idAt: (i: number) => string;
  getTrack: (id: string) => Track | undefined;
  currentId: () => string;
  cursor: () => number;
  start: () => number;
  active: () => boolean;
  debugName?: string;
}) {
  const slots: TrackSlot[] = Array.from({ length: RECYCLE_POOL }, () => {
    const [index, setIndex] = createSignal(-1);
    return { index, setIndex };
  });
  let lastStart = -1;
  let lastTotal = -1;
  let lastTrace = "";
  createEffect(() => {
    const t0 = Date.now();
    const total = props.count();
    const requested = props.start();
    const from = Math.max(0, Math.min(requested, Math.max(0, total - LIST_WINDOW)));
    const step = from - lastStart;
    const canRecycle = lastStart >= 0 && total === lastTotal && Math.abs(step) === 1;

    if (canRecycle) {
      /* 向前滚：旧的最左槽位离开，补到最右；向后滚相反。 */
      const leaving = step > 0 ? lastStart : lastStart + RECYCLE_POOL - 1;
      const entering = step > 0 ? from + RECYCLE_POOL - 1 : from;
      const slot = slots.find((candidate) => untrack(candidate.index) === leaving);
      if (slot) slot.setIndex(entering);
    } else if (from !== lastStart || total !== lastTotal) {
      for (let i = 0; i < slots.length; i += 1) {
        const index = from + i;
        slots[i].setIndex(index < total ? index : -1);
      }
    }

    lastStart = from;
    lastTotal = total;
    if (logEnabled()) {
      const ids: string[] = [];
      for (let i = from; i < Math.min(from + LIST_WINDOW, total); i += 1) ids.push(props.idAt(i));
      const trace = `${from}:${total}:${ids.join(",")}`;
      if (trace !== lastTrace) {
        lastTrace = trace;
        logMsg(
          `perf: track_window name=${props.debugName ?? "track"} start=${from} ` +
            `total=${total} visible=${ids.length} recycle=${canRecycle ? 1 : 0} ` +
            `derive_ms=${Date.now() - t0} ids=${ids.join(",")}`,
        );
      }
    }
  });

  /*
   * 焦点指示器（官方推荐做法 §7）：整个列表只有它一个节点在动。
   *   D-pad ↓ → cursor 变 → indicator 的 translateY 补间到 n*34 → native core 推进。
   * 行本身不换 class、不换文字，所以一次按键的 UI 更新量从"2~12 个节点"
   * 降到"1 个属性"。首次布局用 jump（瞬间就位，避免从 0 滑一下）。
   */
  /* 三个列表现在共用同一个指示器组件（评审 §7/§8）。 */
  const indicatorY = () =>
    Math.max(0, Math.min(LIST_WINDOW - 1, props.cursor() - props.start())) * ROW_PITCH;

  return (
    <View class="relative flex-col w-full grow overflow-hidden">
      {/* viewport 固定；槽位绝对定位，滚动只改变 translate/layout 的位置，
          不再让 Solid 销毁并重建整组 TrackRow。 */}
      <View class="relative w-full h-[204] overflow-hidden">
        {/* 焦点指示器画在行**下面**（先画 = 下层），行走的是透明底，所以看得见它。 */}
        <FocusIndicator y={indicatorY} visible={props.active} />
        {slots.map((slot) => (
          <RecycledTrackSlot
            slot={slot}
            count={props.count}
            start={props.start}
            idAt={props.idAt}
            getTrack={props.getTrack}
            currentId={props.currentId}
          />
        ))}
      </View>

      <View class="flex-row items-center justify-end h-[16] pr-1">
        <Text class={pTxt("hint")}>
          {props.count() > 0
            ? `${props.cursor() + 1} / ${props.count()}`
            : "0 / 0"}
        </Text>
      </View>
    </View>
  );
}
