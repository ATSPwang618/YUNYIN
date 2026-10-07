import { createEffect, createSignal, Show, untrack } from "solid-js";
import { Text, View } from "@pocketjs/framework/components";
import { CardRow, MenuRow } from "../components/rows";
import { FocusIndicator } from "../components/focus_indicator";
import { pTxt } from "../core/theme";
import { LIST_WINDOW } from "./tracks";
import { logEnabled, logMsg } from "../core/media";

/* 行驱动菜单：发现 / 榜单 / 歌单 / 我的 四个页签共用。
 * 行数据由 app.tsx 组装（读 list/ 里的 JSON 文件 + 本地状态），这里只负责画。 */

export type MenuRowData = {
  kind: "card" | "item" | "hint";
  name: string;
  value: string;
  /** 网易云歌单/榜单 id（"item" 用） */
  id?: string;
  /** 对应的 list/ 文件名（打开时先读它） */
  file?: string;
  /** 本地清单（`playlist.json` 里的分组）：子页直接按曲库分组显示，没有文件要读 */
  local?: boolean;
};

const rowH = (kind: MenuRowData["kind"]) => (kind === "card" ? 44 : 32);
const MENU_POOL = LIST_WINDOW + 2;
type MenuSlot = { index: () => number; setIndex: (value: number) => void };

function menuTop(from: number, index: number, rows: MenuRowData[]): number {
  let y = 0;
  for (let i = from; i < index; i += 1) y += rowH(rows[i]?.kind ?? "item") + 4;
  return y;
}

function RecycledMenuSlot(props: {
  slot: MenuSlot;
  rows: () => MenuRowData[];
  start: () => number;
}) {
  const row = () => props.rows()[props.slot.index()];
  return (
    /* Recycled slots must remount only the entering row. A non-keyed Show
       keeps the old Text node identity while its native TEXT_RUN payload
       changes; that path can leave a blank PVF run after a discover recycle. */
    <Show when={row()} keyed>
      {(value) => (
        <View
          style={{
            posType: 1,
            insetT: menuTop(props.start(), props.slot.index(), props.rows()),
            insetL: 0,
            insetR: 0,
            height: rowH(value.kind),
          }}
        >
          {value.kind === "card" ? (
            <CardRow title={value.name} sub={value.value} />
          ) : (
            <MenuRow title={value.name} value={value.value} />
          )}
        </View>
      )}
    </Show>
  );
}

export function MenuList(props: {
  rows: MenuRowData[];
  cursor: () => number;
  start: () => number;
  active: () => boolean;
  debugName?: string;
}) {
  const slots: MenuSlot[] = Array.from({ length: MENU_POOL }, () => {
    const [index, setIndex] = createSignal(-1);
    return { index, setIndex };
  });
  let lastStart = -1;
  let lastTotal = -1;
  let lastTrace = "";
  createEffect(() => {
    const t0 = Date.now();
    const from = props.start();
    const total = props.rows.length;
    const step = from - lastStart;
    const canRecycle = lastStart >= 0 && total === lastTotal && Math.abs(step) === 1;
    if (canRecycle) {
      const leaving = step > 0 ? lastStart : lastStart + MENU_POOL - 1;
      const entering = step > 0 ? from + MENU_POOL - 1 : from;
      const slot = slots.find((candidate) => untrack(candidate.index) === leaving);
      /* Retain an out-of-range logical index.  The row stays hidden through
       * row(), but remains discoverable when the user scrolls back. */
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
      const rows = props.rows.slice(from, from + LIST_WINDOW);
      const trace = `${from}:${total}:${rows.map((row) => row.kind).join("")}`;
      if (trace !== lastTrace) {
        lastTrace = trace;
        logMsg(
          `perf: menu_window name=${props.debugName ?? "menu"} start=${from} ` +
            `total=${total} visible=${rows.length} recycle=${canRecycle ? 1 : 0} ` +
            `derive_ms=${Date.now() - t0} kinds=${rows.map((row) => row.kind).join(",")} ` +
            `names=${rows.map((row) => row.name.slice(0, 20)).join("|")}`,
        );
      }
    }
  });

  /*
   * 焦点指示器（评审 §7）：菜单里的行高**不统一**（卡片行 44 / 普通行 30），
   * 所以位移要把焦点行之前那些行的高度累加起来（间距 gap-1 = 4px）。
   * 这样上下移动同样只动指示器一个节点，行不再各自换样式。
   */
  const indicatorY = () => {
    let y = 0;
    const end = Math.min(props.cursor(), props.rows.length);
    for (let i = props.start(); i < end; i += 1) y += rowH(props.rows[i].kind) + 4;
    return y;
  };
  const indicatorTall = () => props.rows[props.cursor()]?.kind === "card";

  return (
    <View class="relative flex-col w-full grow overflow-hidden">
      <View class="relative w-full h-[216] overflow-hidden">
        <FocusIndicator y={indicatorY} tall={indicatorTall} visible={props.active} />
        {slots.map((slot) => (
          <RecycledMenuSlot slot={slot} rows={() => props.rows} start={props.start} />
        ))}
      </View>
      <View class="flex-row items-center justify-end h-[16] pr-1">
        <Text class={pTxt("hint")}>
          {props.rows.length > LIST_WINDOW
            ? `${props.cursor() + 1} / ${props.rows.length}`
            : ""}
        </Text>
      </View>
    </View>
  );
}
