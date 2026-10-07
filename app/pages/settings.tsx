import { View } from "@pocketjs/framework/components";
import { MenuRow } from "../components/rows";
import { themeLabel } from "../core/theme";

/* 设置页（子页，外壳由 app.tsx 的 SubPage 提供）：
 * 主题 / 按键说明 / 关于 / 字库。
 *
 * 这里**没有**"账号"行：登录入口在「我的 → 账号」，摆两份只会让人以为是两个地方。 */

export function SettingPage(props: {
  /* 只展示当前固定的 Vita2D 原生字体后端，不再提供 PJFA 模式切换。 */
  fontMode: string;
  cursor: () => number;
  active: () => boolean;
}) {
  const on = (i: number) => props.active() && props.cursor() === i;
  return (
    <View class="flex-col w-full grow gap-1 overflow-hidden">
      <MenuRow title="主题" value={themeLabel()} focused={on(0)} />
      <MenuRow title="按键说明" value="INFO" focused={on(1)} />
      <MenuRow title="关于" value="INFO" focused={on(2)} />
      <MenuRow title="字库" value={props.fontMode} focused={on(3)} />
    </View>
  );
}
