import { Text, View } from "@pocketjs/framework/components";
import { bgCls, pTxt } from "../core/theme";

/* 顶部四页签：选中 = 红字 + 红下划线；焦点（手柄在这里时）= 浅红底。 */

export const TAB_LABELS = ["发现", "榜单", "歌单", "我的"] as const;

export function TabBar(props: {
  active: () => number;
  cursor: () => number;
  focused: () => boolean;
}) {
  return (
    <View class="flex-row items-center justify-between w-full h-[24]">
      {TAB_LABELS.map((label, i) => (
        <View
          class={
            props.focused() && props.cursor() === i
              ? bgCls("tabFocus")
              : bgCls("tabItem")
          }
        >
          <Text class={props.active() === i ? pTxt("tabActive") : pTxt("tab")}>
            {label}
          </Text>
          {props.active() === i ? (
            <View class={bgCls("underlinePos")} />
          ) : null}
        </View>
      ))}
    </View>
  );
}
