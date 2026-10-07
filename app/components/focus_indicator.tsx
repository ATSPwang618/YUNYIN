//! 共用焦点指示器（官方推荐做法：整个列表只有它一个节点在动）。
//!
//! 三个列表（曲目 / 菜单 / 专辑）现在共用同一套机制：
//!   上下移动 → 只把指示器的 `translateY` 补间到目标 → native core 推进。
//! 行本身不再换 class、不再换文字，所以一次按键的 UI 更新量是**一个属性**。
//!
//! 两种高度：普通行 32px（`rowIndicator`）/ 卡片行 44px（`cardIndicator`）；
//! 焦点不在列表里时切到 `*Off`（同一个框 + `opacity-0` + `transition-opacity`），
//! 避免"两个光标"（真机踩过）。

import { createEffect } from "solid-js";
import { View } from "@pocketjs/framework/components";
import type { NodeMirror } from "@pocketjs/framework/renderer";
import { animate, jump } from "@pocketjs/framework/animation";
import { bgCls } from "../core/theme";
import { MOTION } from "../core/motion";

export function FocusIndicator(props: {
  /** 目标位移（px）：由调用方按"行高 + 间距"累加算出。 */
  y: () => number;
  /** 当前焦点行是不是卡片行（44px）。 */
  tall?: () => boolean;
  /** 焦点是否在这个列表里（false 时淡出，避免和其它焦点框同时出现）。 */
  visible: () => boolean;
}) {
  let node: NodeMirror | undefined;
  let placed = false;
  createEffect(() => {
    const y = props.y();
    if (!node) return;
    if (!placed) {
      /* 首次就位用 jump（瞬间到位，避免从上一页的位置滑一下）。 */
      placed = true;
      jump(node, "translateY", y);
      return;
    }
    animate(node, "translateY", y, { dur: MOTION.micro, easing: "out" });
  });

  const cls = () => {
    const tall = props.tall?.() ?? false;
    if (!props.visible()) {
      return tall ? bgCls("cardIndicatorOff") : bgCls("rowIndicatorOff");
    }
    return tall ? bgCls("cardIndicator") : bgCls("rowIndicator");
  };

  return (
    <View
      ref={(n: NodeMirror) => {
        node = n;
      }}
      class={cls()}
    />
  );
}
