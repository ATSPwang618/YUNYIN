import { createMemo } from "solid-js";
import { Text, View } from "@pocketjs/framework/components";
import { type Track } from "../core/types";
import { clipW } from "../core/util";
import { pTxt } from "../core/theme";
import { TrackRow } from "../components/rows";
import { FocusIndicator } from "../components/focus_indicator";

/* 通用曲目列表（本地音乐 / 在线歌曲 / 我喜欢的 / 专辑详情 共用）。
 * 只渲染可见的 5 行 + 一行计数；焦点行由上层 cursor/active 决定。 */

/* 一屏显示几行（行高 30 + 行距 4）：内容区 256 高，
 * 6 行 = 180，加上页签行 / 子页头 / 计数行刚好不溢出。 */
export const LIST_WINDOW = 6;
/* 行高 30 + 行距 4（容器 gap-1）：焦点指示器每次移动的步长。 */
const ROW_PITCH = 34;

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
}) {
  /* 只取"当前窗口"这一小段：先算起点/终点，再逐条问 id —— 列表多长都一样。 */
  const visible = createMemo(() => {
    const from = props.start();
    const total = props.count();
    const out: { i: number; id: string }[] = [];
    for (let i = from; i < Math.min(from + LIST_WINDOW, total); i += 1) {
      out.push({ i, id: props.idAt(i) });
    }
    return out;
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
    <View class="relative flex-col w-full grow gap-1 overflow-hidden">
      {/* 焦点指示器画在行**下面**（先画 = 下层），行走的是透明底，所以看得见它。 */}
      <FocusIndicator y={indicatorY} visible={props.active} />
      {visible().map(({ i: idx, id }) => {
        const song = props.getTrack(id);
        if (!song) {
          return null;
        }
        return (
          <TrackRow
            index={idx + 1}
            title={clipW(song.title, 26)}
            artist={clipW(song.artist, 30)}
            current={props.currentId() === id}
            off={song.off}
            vip={song.vip}
            vipped={song.fee === 1}
          />
        );
      })}

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
