import { Text, View } from "@pocketjs/framework/components";
import { bgCls, pTxt } from "../core/theme";
import { clipW } from "../core/util";

/* 通用行组件：菜单行 / 卡片 / 曲目行 / 专辑行 / 居中提示。
 * 所有 class 都是**整串**取自主题表或源码字面量（框架按整串烘焙样式）。 */

export function MenuRow(props: {
  title: string;
  value?: string;
  focused?: boolean; /* 列表面板统一用焦点指示器；这个 prop 只留给设置页那种小列表 */
}) {
  return (
    <View class={props.focused ? bgCls("rowFocus") : bgCls("row")}>
      <View class="flex-1 min-w-0">
        <Text class={props.focused ? pTxt("accent") : pTxt("rowTitle")}>
          {(props.focused ? "› " : "") + clipW(props.title, 26)}
        </Text>
      </View>
      <View class="shrink-0">
        <Text class={props.focused ? pTxt("accent") : pTxt("rowValue")}>
          {clipW(props.value, 12)}
        </Text>
      </View>
    </View>
  );
}

export function CardRow(props: {
  title: string;
  sub: string;
  focused?: boolean;
}) {
  return (
    <View class={props.focused ? bgCls("cardFocus") : bgCls("card")}>
      <Text class={pTxt("cardTitle")}>{clipW(props.title, 36)}</Text>
      <Text class={pTxt("cardSub")}>{clipW(props.sub, 38)}</Text>
    </View>
  );
}

/* 曲目行：序号 + 歌名/歌手；focused 时整行红描边，右端给"正在播放"标记。 */
export function TrackRow(props: {
  index: number;
  title: string;
  artist: string;
  /*
   * 焦点视觉**不再由行自己承担**（任务书《PocketJS 低性能优化》§7）：
   * 上下移动时如果每行都换 class + 换文字（'›' ↔ 序号），一次按键就要动 2~12 个节点。
   * 现在焦点是一个**独立指示器**（`tracks.tsx` 里那个 translateY 滑动的 View），
   * 行本身保持静止 —— 所以这个 prop 变成可选（列表面板不再传它）。
   */
  focused?: boolean;
  current?: boolean;
  /* 下架 / 无版权：整行淡化，右端写"下架"（不显示 ▶）。 */
  off?: boolean;
  /* 需要会员 / 暂无版权：同样淡化，右端写"会员"。 */
  vip?: boolean;
  /* VIP 歌曲（`fee == 1`）：**只打标签**，不影响能不能播 */
  vipped?: boolean;
}) {
  const blocked = () => props.off || props.vip;
  const label = () =>
    props.off
      ? "下架"
      : props.vip
        ? "会员"
        : props.current
          ? "▶"
          : props.vipped
            ? "VIP"
            : "";
  return (
    /* 曲目行**永远是"普通行"样式**：焦点由父级指示器表达（评审 §8）。
     * 这样上下移动时行本身不换 class、不换文字，一次按键只动指示器一个节点。 */
    <View class={bgCls("row")}>
      <View class="flex-1 min-w-0 flex-row items-center gap-2 overflow-hidden">
        <View class="shrink-0">
          <Text class={pTxt("index")}>{String(props.index)}</Text>
        </View>
        <View class="flex-1 min-w-0 flex-col overflow-hidden">
          <Text class={blocked() ? pTxt("hint") : pTxt("listTitle")}>
            {clipW(props.title, 26)}
          </Text>
          <Text class={pTxt("listSub")}>{clipW(props.artist, 30)}</Text>
        </View>
      </View>
      <View class="shrink-0">
        <Text class={blocked() || !props.current ? pTxt("rowValue") : pTxt("accent")}>
          {label()}
        </Text>
      </View>
    </View>
  );
}

/* 专辑行：专辑名 + 歌手，右侧曲目数（专辑页是**纯文字列表**，不用封面网格）。 */
export function AlbumRow(props: {
  title: string;
  artist: string;
  count: number;
  focused?: boolean;
}) {
  return (
    <View class={props.focused ? bgCls("rowFocus") : bgCls("row")}>
      <View class="flex-1 min-w-0 flex-col overflow-hidden">
        <Text class={props.focused ? pTxt("accent") : pTxt("listTitle")}>
          {props.title}
        </Text>
        <Text class={pTxt("listSub")}>{props.artist}</Text>
      </View>
      <View class="shrink-0">
        <Text class={props.focused ? pTxt("accent") : pTxt("rowValue")}>
          {props.count} 首
        </Text>
      </View>
    </View>
  );
}

export function EmptyHint(props: { text: string }) {
  return (
    <View class="grow w-full items-center justify-center">
      {/* 占位/加载文字用 **baked keyframe**（官方 animate-pulse，编译期烘进 style table，
       * 由 native core 按固定 tick 推进）："正在同步歌单…" 这类文案会轻微呼吸，
       * 但 JS 一帧都不参与 —— 这正是"固定动画直接 Bake"的用法。 */}
      <Text class={pTxt("pulse")}>{props.text}</Text>
    </View>
  );
}
