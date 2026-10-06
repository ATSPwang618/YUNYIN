import { For } from "solid-js";
import { View } from "@pocketjs/framework/components";
import { MenuRow } from "../components/rows";

/* 按键说明页（子页）：静态行列表。 */

export const KEY_GUIDE_ROWS: [string, string][] = [
  ["↑ / ↓", "列表上下移动"],
  ["← / →", "切换页签 · 进播放器"],
  ["○", "确认 · 播放 / 暂停"],
  ["△", "返回"],
  ["L / R", "上一首 / 下一首"],
  ["START", "关屏继续播放"],
];

export function KeyGuidePage() {
  return (
    <View class="flex-col w-full grow gap-1 overflow-hidden">
      <For each={KEY_GUIDE_ROWS}>
        {(row) => <MenuRow title={row[0]} value={row[1]} focused={false} />}
      </For>
    </View>
  );
}
