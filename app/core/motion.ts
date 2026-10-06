//! YUNYIN 动效层（任务书《UI 动画改造任务》§2/§29/§30）。
//!
//! 两条路线，都走 **PocketJS 官方**：
//!   1. **声明式**：把 `transition-* / duration-* / ease-*` 与 `translate-* / scale-*`
//!      写进 `app/colors.json` 的角色字面量里 —— 类一换，native core 自己补间，
//!      JS 一帧都不参与（首选，最省）。
//!   2. **事件驱动**：一次性动作（切页、切歌、页面进入）用官方
//!      `animate() / spring() / jump() / cancelAnim()`（`@pocketjs/framework/animation`），
//!      由 native core 按 vblank 推进；**绝不用 onFrame + setSignal 每帧驱动**。
//!
//! 这里只放常量和 helper，不放页面逻辑（`app.tsx` 保持单文件入口）。

import { animate, cancelAnim, jump, spring } from "@pocketjs/framework/animation";
import type { NodeMirror } from "@pocketjs/framework/renderer";

/** 统一动效规范（任务书 §2）：所有时长只从这里取，别在组件里散写魔法数字。 */
export const MOTION = {
  /** 按钮 / 焦点反馈。 */
  micro: 110,
  /** 轻量状态变化。 */
  fast: 150,
  /** 列表 / 状态转换。 */
  normal: 190,
  /** 页面进入 / 切换。 */
  page: 230,
  slow: 280,

  /** 页面进出的位移。 */
  pageOffset: 16,
  /* ---- Tab 切换（两段式：旧页滑出 → 换页 → 新页滑入）----
   * 真机反馈"太快要卡" ⇒ 退场短一点、进场慢一点，位移都收小（8px 在 480 宽屏幕上已经够）。
   */
  /** 旧页滑出 + 淡出。 */
  tabExit: 160,
  /** 新页滑入 + 淡入（比退场慢，形成"接管"的感觉）。 */
  tabEnter: 280,
  /** 进出场的位移（≤8px，大幅横移在这个尺寸上会显得廉价）。 */
  tabOffset: 8,
  /** 子页退场：向右滑出 + 淡出（比进场快，和 Tab 退场同一个节奏）。 */
  subExit: 160,
} as const;

/**
 * 一次性的动画句柄：**每个属性各留一个 animation id**。
 *
 * 为什么不是"一个 id"：以前 `to()` 一进来就 `cancel()` 掉上一个 id，
 * 于是 `trackChange()` 里
 * ```
 * handle.to(node, "scale", 1, …)     // 建了 scale 的 tween
 * handle.to(node, "opacity", 1, …)   // 立刻 cancel 掉 scale，只剩 opacity
 * ```
 * **两个属性根本没并行**（评审指出的实际 bug）。
 * 现在按属性分别取消：同属性重播会取消旧的（不打架），不同属性互不影响（能并行）。
 */
export class MotionHandle {
  private ids: Record<string, number> = {};

  /** 从"当前值"补间到 `to`（同一属性上的旧动画会先取消）。 */
  to(
    node: NodeMirror | undefined,
    prop: string,
    value: number | string,
    dur: number,
    easing = "out",
  ): void {
    if (!node) return;
    this.cancelProp(prop);
    this.ids[prop] = animate(node, prop as never, value, { dur, easing: easing as never });
  }

  /** 弹簧（播放键/重点交互用）；同样按属性取消。 */
  spring(node: NodeMirror | undefined, prop: string, value: number | string, bouncy = false): void {
    if (!node) return;
    this.cancelProp(prop);
    this.ids[prop] = spring(node, prop as never, value, bouncy ? "bouncy" : "default");
  }

  /** 只取消某一个属性的动画。 */
  cancelProp(prop: string): void {
    const id = this.ids[prop];
    if (id) {
      cancelAnim(id);
      this.ids[prop] = 0;
    }
  }

  /** 取消全部（切换对象/离开页面时用）。 */
  cancel(): void {
    for (const prop of Object.keys(this.ids)) this.cancelProp(prop);
  }
}

/**
 * 页面 / 内容进入：从 `dir` 指的那一侧滑一点点进来 + 淡入（任务书 §10）。
 *
 * `dir = "right"` 表示"新页从右边进来"（前进），"left" 是返回方向。
 */
export function pageEnter(
  node: NodeMirror | undefined,
  dir: "left" | "right",
  dur: number = MOTION.page,
): void {
  if (!node) return;
  const from = dir === "right" ? MOTION.pageOffset : -MOTION.pageOffset;
  jump(node, "translateX", from);
  jump(node, "opacity", 0);
  animate(node, "translateX", 0, { dur, easing: "out" });
  animate(node, "opacity", 1, { dur, easing: "out" });
}

/*
 * 焦点反馈**故意不做成 JS 动画**：
 *
 * 真机反馈"上下键那个动效掉帧"。原因是每按一次上下，行要同时 spawn 多个补间
 * （translateX + bgColor + borderColor + borderWidth + textColor）。现在换成
 * 纯**声明式颜色过渡**（`transition-colors duration-110 ease-out`，写在 colors.json
 * 的角色字面量里）：类一换、native core 只补颜色，**零 JS、零 transform、零布局**，
 * 110ms 又刚好在"按钮反馈"的甜蜜区（任务书 §2）。
 *
 * 所以这里不再提供 focusIn/focusOut —— 焦点的一切都在主题字面量里，
 * 这也是官方最推荐的路线（能声明式就别写 JS）。
 */

/** 切歌：封面/信息"轻微退出再进来"（任务书 §13）。 */
export function trackChange(node: NodeMirror | undefined, handle: MotionHandle): void {
  if (!node) return;
  jump(node, "scale", 0.96);
  jump(node, "opacity", 0.55);
  handle.to(node, "scale", 1, MOTION.normal, "out");
  handle.to(node, "opacity", 1, MOTION.normal, "out");
}
