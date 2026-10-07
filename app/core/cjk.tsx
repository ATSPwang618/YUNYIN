/* Vita2D 原生字体适配层。
 *
 * 文字不再经过 PocketJS 的 PJFA/CJK stream、TextResource 或页面级预热；
 * core 安装 native measure provider 后会把普通文字编码成 TEXT_RUN，Vita
 * 宿主再统一交给 Vita2D 的 PVF 后端绘制。保留 StreamText 这个组件名
 * 只为了让现有页面保持稳定，组件本身不再拥有任何字体资源或刷新状态。
 */
import { Text } from "@pocketjs/framework/components";

export function StreamText(props: { class: string; text: string }) {
  return <Text class={props.class}>{props.text}</Text>;
}
