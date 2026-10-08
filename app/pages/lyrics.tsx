import { createEffect, createMemo } from "solid-js";
import { Text, View, type NodeMirror } from "@pocketjs/framework/components";
import { animate, jump } from "@pocketjs/framework/animation";
import { type Track, type LyricLine } from "../core/types";
import { clipW } from "../core/util";
import { pTxt } from "../core/theme";

/* 歌词页（子页）：三行居中，当前行红色。骨架/预取逻辑沿用旧版（性能相关）。 */

export function LyricsPage(props: {
  track: () => Track;
  lines: () => LyricLine[];
  position: () => number;
}) {
  const LYR_LINE = 16;
  let listRef: NodeMirror | undefined;
  let prevIndex = 0;

  /*
   * 当前歌词行（评审 §11）：**游标推进 + 二分兜底**，不再每次从第 0 行扫到底。
   *
   * 播放时 position 只会向前：拿上一次的下标比一比，能前进就前进几步（O(1) 均摊）；
   * 一旦发生 seek（position 跳回去了），退化成一次二分查找 O(log N)。
   * 500 行歌词在 4 分钟里要更新上千次，这条把"每次 500 次比较"变成"每次 1~3 次"。
   */
  let lyricCursor = 0;
  const active = createMemo(() => {
    const pos = props.position();
    const ls = props.lines();
    if (ls.length === 0) return 0;
    let idx = lyricCursor;
    if (idx >= ls.length) idx = ls.length - 1;
    if (pos < ls[idx].time) {
      /* 往回 seek：二分找最后一个 time <= pos 的行。 */
      let lo = 0;
      let hi = ls.length - 1;
      while (lo < hi) {
        const mid = (lo + hi + 1) >> 1;
        if (ls[mid].time <= pos) lo = mid;
        else hi = mid - 1;
      }
      idx = lo;
    } else {
      /* 正常前进：只往前挪，最多挪到 time > pos 为止。 */
      while (idx + 1 < ls.length && ls[idx + 1].time <= pos) idx += 1;
    }
    lyricCursor = idx;
    return idx;
  });

  createEffect(() => {
    const idx = active();
    if (!listRef || idx === prevIndex) return;
    const dir = idx > prevIndex ? 1 : -1;
    prevIndex = idx;
    jump(listRef, "translateY", dir * LYR_LINE);
    animate(listRef, "translateY", 0, { dur: 170, easing: "out" });
  });

  /* 三行拆成三个字符串 memo：只有"这一行真换了"才通知下游。 */
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

  return (
    <View class="grow w-full flex-col items-center justify-center overflow-hidden">
      <View
        ref={(node: NodeMirror) => {
          listRef = node;
        }}
        style={{ translateY: 0 }}
        class="flex-col items-center gap-2 overflow-hidden"
      >
        <Text class={pTxt("lyricOther")}>{clipW(prevLine(), 38)}</Text>
        <Text class={pTxt("lyricCur")}>{clipW(curLine(), 40)}</Text>
        <Text class={pTxt("lyricOther")}>{clipW(nextLine(), 38)}</Text>
      </View>
    </View>
  );
}
