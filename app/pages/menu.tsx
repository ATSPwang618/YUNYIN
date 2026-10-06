import { createMemo } from "solid-js";
import { Text, View } from "@pocketjs/framework/components";
import { CardRow, MenuRow } from "../components/rows";
import { FocusIndicator } from "../components/focus_indicator";
import { pTxt } from "../core/theme";
import { LIST_WINDOW } from "./tracks";

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

export function MenuList(props: {
  rows: MenuRowData[];
  cursor: () => number;
  start: () => number;
  active: () => boolean;
}) {
  const visible = createMemo(() =>
    props.rows.slice(props.start(), props.start() + LIST_WINDOW),
  );

  /*
   * 焦点指示器（评审 §7）：菜单里的行高**不统一**（卡片行 44 / 普通行 30），
   * 所以位移要把焦点行之前那些行的高度累加起来（间距 gap-1 = 4px）。
   * 这样上下移动同样只动指示器一个节点，行不再各自换样式。
   */
  const rowH = (kind: MenuRowData["kind"]) => (kind === "card" ? 44 : 30);
  const indicatorY = () => {
    let y = 0;
    const end = Math.min(props.cursor(), props.rows.length);
    for (let i = props.start(); i < end; i += 1) y += rowH(props.rows[i].kind) + 4;
    return y;
  };
  const indicatorTall = () => props.rows[props.cursor()]?.kind === "card";

  return (
    <View class="relative flex-col w-full grow gap-1 overflow-hidden">
      <FocusIndicator y={indicatorY} tall={indicatorTall} visible={props.active} />
      {visible().map((row, i) => {
        return row.kind === "card" ? (
          <CardRow title={row.name} sub={row.value} />
        ) : (
          <MenuRow title={row.name} value={row.value} />
        );
      })}
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
