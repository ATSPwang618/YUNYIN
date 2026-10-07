import { createEffect, onMount } from "solid-js";
import { Text, View } from "@pocketjs/framework/components";
import type { NodeMirror } from "@pocketjs/framework/renderer";
import { bgCls, pTxt } from "../core/theme";
import { StreamText } from "../core/cjk";
import { animate, jump } from "@pocketjs/framework/animation";
import { pageEnter } from "../core/motion";

/* 子页外壳：← 标题（+ 右侧可选值） + 内容。
 * ← 是一个**可聚焦**的返回键：列表第一行再按 ↑ 聚焦它，○ 返回（△ 也能返回）。 */

export function SubPage(props: {
  title: string;
  right?: string;
  backFocused?: () => boolean;
  /** 进入方向：前进 = 从右侧滑入，返回 = 从左侧滑入（任务书 §10）。 */
  dir?: "left" | "right";
  /** 把根节点交给上层：**退场动画**要用同一个节点（先播退场，到点再真正关页）。 */
  nodeRef?: (n: NodeMirror) => void;
  children: any;
}) {
  const focused = () => props.backFocused?.() ?? false;
  let box: NodeMirror | undefined;
  let head: NodeMirror | undefined;
  /*
   * 子页进入动画（任务书 §10/§11）：整页 16px 滑入 + 淡入（220ms），
   * header（← + 标题）单独早 40ms、只滑 8px —— 层次感来自这个时差。
   * 一次性触发，不进帧循环；素材/布局都没变。
   */
  onMount(() => {
    pageEnter(box, props.dir ?? "right");
    if (head) {
      const from = props.dir === "left" ? -8 : 8;
      jump(head, "translateX", from);
      jump(head, "opacity", 0);
      animate(head, "translateX", 0, { dur: 180, easing: "out" });
      animate(head, "opacity", 1, { dur: 180, easing: "out" });
    }
  });
  /*
   * 同一个 SubPage 实例里**换页**（设置 → 按键说明 → 关于…）时也要有进场动画：
   * 那时 onMount 不会再触发，只有 title 变。首次挂载由上面那段负责，这里跳过。
   */
  let firstTitle = true;
  createEffect(() => {
    const t = props.title;
    void t;
    if (firstTitle) {
      firstTitle = false;
      return;
    }
    pageEnter(box, props.dir ?? "right", 200);
  });
  return (
    <View
      ref={(n: NodeMirror) => {
        box = n;
        props.nodeRef?.(n);
      }}
      class="flex-col w-full h-full gap-1 overflow-hidden"
    >
      <View
        ref={(n: NodeMirror) => {
          head = n;
        }}
        class="flex-row items-center justify-between w-full h-[24] px-1"
      >
        <View class="flex-row items-center gap-2">
          <View class={focused() ? bgCls("backBtnFocus") : bgCls("backBtn")}>
            <Text class={focused() ? pTxt("accent") : pTxt("pageTitle")}>←</Text>
          </View>
          <StreamText class={pTxt("pageTitle")} text={props.title} />
        </View>
        {props.right ? (
          <Text class={pTxt("rowValue")}>{props.right}</Text>
        ) : null}
      </View>
      {props.children}
    </View>
  );
}
