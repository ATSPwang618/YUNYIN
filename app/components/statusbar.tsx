import { createSignal } from "solid-js";
import { Text, View } from "@pocketjs/framework/components";
import { pTxt } from "../core/theme";
import { media } from "../core/media";

/*
 * 左栏顶部状态栏（在播放器卡片**外面**）：电量数字 + 时间 + 在线/离线。
 *
 * 用纯文字（不再画图标）：这个布局引擎里"只有子元素有尺寸、父容器没显式高度"的
 * 小方块组会被算成 0 高、整组看不见；试过给父容器加 h-[12] 仍然不稳。文字最稳，
 * 而且和卡片左边缘天然对齐（不加额外 padding）。
 *
 * 两个约束：
 *   1. **运行期没有 setTimeout / setInterval**（用了就 ReferenceError 黑屏）：刷新由
 *      帧循环驱动，app.tsx 每 300 帧调一次 pollHostInfo()。
 *   2. 数据一次拿全（原生 hostInfo），别在帧循环里反复问系统。
 */

type HostInfo = { battery: number; charging: number; online: number; time: string };

const FALLBACK: HostInfo = { battery: -1, charging: 0, online: 0, time: "--:--" };
const [info, setInfo] = createSignal<HostInfo>(FALLBACK);

/// 帧循环里定期调用（不要用定时器）。
export function pollHostInfo(): void {
  const raw = (media() as unknown as { hostInfo?: () => string } | undefined)?.hostInfo?.();
  if (!raw) return;
  try {
    setInfo(JSON.parse(String(raw)) as HostInfo);
  } catch {
    /* 拿不到就维持上一次的值，别把状态栏闪成空 */
  }
}

export function StatusBar() {
  const pct = () => (info().battery >= 0 ? `${info().battery}%` : "--");
  return (
    /* 三段贴齐左栏（= 卡片）的左边缘 / 中间 / 右边缘：justify-between 铺满宽度。 */
    <View class="flex-row items-center justify-between w-full h-[14]">
      {/* 左：电量数字（充电时带 +）｜中：时间｜右：在线/离线 */}
      <Text class={pTxt("tab")}>{pct() + (info().charging ? "+" : "")}</Text>
      <Text class={pTxt("tab")}>{info().time}</Text>
      <Text class={pTxt("tab")}>{info().online ? "在线" : "离线"}</Text>
    </View>
  );
}