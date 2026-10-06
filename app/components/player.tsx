import { Image, Text, View } from "@pocketjs/framework/components";
import { createMemo } from "solid-js";
import { createEffect } from "solid-js";
import type { NodeMirror } from "@pocketjs/framework/renderer";
import { type Track, type PlaybackMode } from "../core/types";
import { clipW, formatMs } from "../core/util";
import { getTrackDuration } from "../core/library";
import { bgCls, pTxt, useSkin } from "../core/theme";
import { StreamText } from "../core/cjk";
import { MotionHandle, trackChange } from "../core/motion";

/* 左侧常驻播放器面板：封面（无封面时画一张唱片）+ 问候 + 曲目 + 进度 + 控制。
 * 6 个焦点位：0 上一首 / 1 播放暂停 / 2 下一首 / 3 循环模式 / 4 收藏 / 5 歌词。 */

const BAR_PX = 104;
/* 6 个焦点位并成一排：顺序模式 · 上一首 · 播放暂停 · 下一首 · 喜爱 · 词（歌词页）。
 * app.tsx 的按键路由直接按这个顺序取动作，别再写死下标。 */
export const CTRL_ORDER = ["mode", "prev", "play", "next", "fav", "lyric"] as const;
/* 有贴图的动作：模式键现在也走贴图（shuf / rep1），只剩"词"是文字按钮。 */
type IconSlot = Exclude<(typeof CTRL_ORDER)[number], "lyric">;

export function PlayerPanel(props: {
  track: () => Track;
  playing: () => boolean;
  position: () => number;
  playbackMode: () => PlaybackMode;
  favorite: () => boolean;
  cursor: () => number;
  focused: () => boolean;
  /* 当前播放曲目在清单里的位置，形如 "2 / 6"（没选歌时是空串）。 */
  queuePos: () => string;
  netStatus: () => string;
  /* 状态词要不要用告警色（红）：缓冲 / 网络故障 = true；
   * 播放中 / 暂停中 / 待定 = false（灰字，不吓人）。 */
  netAlert: () => boolean;
  /* 已缓冲进度（0..1）；在线歌才有意义，0 就是不画那根浅色条。 */
  buffered: () => number;
  /** 启动进入动画用：把根节点交给上层（任务书 §3 的左栏滑入）。 */
  nodeRef?: (n: NodeMirror) => void;
}) {
  /* 空位（还没选歌）两边就是纯文字 00:00，不显示横线也不显示假时长。 */
  const hasSong = () => !!props.track().id;
  /*
   * 切歌动画（任务书 §13）：封面容器先"轻微退出"（scale 0.96 / opacity 0.55）
   * 再补回 1 —— 用的是官方 animate()，由 native core 推进，不进帧循环。
   * 只在**曲目 id 变化**时触发一次；暂停/播放/进度变化都不动它。
   */
  let cover: NodeMirror | undefined;
  const coverAnim = new MotionHandle();
  let lastTrackId = "";
  createEffect(() => {
    const id = props.track().id;
    if (id === lastTrackId) return;
    lastTrackId = id;
    if (id) trackChange(cover, coverAnim);
  });
  const posLabel = createMemo(() =>
    hasSong() ? formatMs(props.position()) : "00:00",
  );
  const durLabel = createMemo(() =>
    hasSong() ? formatMs(getTrackDuration(props.track())) : "00:00",
  );

  const fillWidth = () => {
    if (!hasSong()) return 0;
    const duration = getTrackDuration(props.track());
    const pos = Math.min(duration, Math.max(0, props.position()));
    return Math.min(BAR_PX, (pos / duration) * BAR_PX);
  };

  /*
   * 控制按钮**直接用素材 PNG**（每个动作 N = 常态 / F = 聚焦两张）。
   *
   * 为什么不再用类名画圆底：素材自带的圆圈高亮比类名圆底好看，而且省掉一层
   * 容器；更重要的是字形（↻ / ↻1）依赖字库，缺字时真机上就是一个方框 ——
   * 现在模式键也走贴图（顺序 = shuf，单曲循环 = rep1），不再有缺字风险。
   */
  const iconFor = (slot: Exclude<IconSlot, never>, on: boolean): string => {
    const skin = useSkin();
    if (slot === "play") {
      if (props.playing()) return on ? skin.pauseF : skin.pause;
      return on ? skin.playF : skin.play;
    }
    if (slot === "prev") return on ? skin.prevF : skin.prev;
    if (slot === "next") return on ? skin.nextF : skin.next;
    if (slot === "fav") {
      return props.favorite()
        ? (on ? skin.honF : skin.hon)
        : (on ? skin.hoffF : skin.hoff);
    }
    /* 顺序模式 = 整份清单循环（shuf 图标），单曲循环 = rep1 */
    const repeatOne = props.playbackMode() === "repeat-one";
    if (repeatOne) return on ? skin.rep1F : skin.rep1;
    return on ? skin.shufF : skin.shuf;
  };

  return (
    <View
      ref={(n: NodeMirror) => {
        props.nodeRef?.(n);
      }}
      class={bgCls("playerBox")}
    >
      {/* 封面（有真封面画真封面，没有就画一张红唱片）。
          原来右下角那行"第几首/总数"已经并到下面的信息行里了。 */}
      <View
        ref={(n: NodeMirror) => {
          cover = n;
        }}
        class="flex-col items-end w-[108]"
      >
        <View class={bgCls("coverBox")}>
          {props.track().cover ? (
            <Image
              src={props.track().cover ?? ""}
              class={"relative w-[108] h-[108]"}
            />
          ) : (
            <>
              <View class={bgCls("coverDisk")} />
              <View class={bgCls("coverLabel")} />
              <View class={bgCls("coverHole")} />
            </>
          )}
        </View>
      </View>

      <StreamText
        class={pTxt("playerTitle")}
        text={clipW(props.track().title, 28)}
      />
      <StreamText
        class={pTxt("playerArtist")}
        text={clipW(props.track().artist, 28)}
      />

      {/* 信息行：「清单进度 + 状态 + 词」并成一排 —— 左中右各一格，
          左右两格等宽，中间那格才是真居中。
          没选歌时中间写「待定」；有歌时写 播放中 / 暂停中 / 缓冲中 / 网络故障。
          顺序：**信息行在上、进度行在下**（用户要求两行对调）。 */}
      <View class="flex-row items-center justify-between w-full h-[20] overflow-hidden">
        {/* 左右两格**等宽 56**：以前是 30，`6 / 6` 刚好放得下、`7 / 527` 就
            从左边溢出去（真机截图反馈）。加宽后仍保持等宽 —— 中间那格才是真居中。
            文本按界面规范用 clipW 截到 9 个显示单位（12px 下约 54px），
            三位数歌单（"527 / 527"）刚好完整显示，再长也不会出界。 */}
        <View class="w-[56] items-start justify-center overflow-hidden">
          <Text class={pTxt("time")}>{clipW(props.queuePos(), 9)}</Text>
        </View>
        <View class="grow items-center justify-center">
          {/* "缓冲中"用 **baked keyframe** 呼吸（官方 animate-pulse，native core 推进）：
           * 它是最需要"看起来还活着"的状态，而 JS 完全不用管它。 */}
          <Text
            class={
              props.netStatus() === "缓冲中"
                ? pTxt("statusPulse")
                : props.netAlert()
                  ? pTxt("status")
                  : pTxt("hint")
            }
          >
            {props.netStatus()}
          </Text>
        </View>
        <View class="w-[56] items-center justify-center">
          {/* "词"是唯一的文字按钮：**纯文字、不套圆圈、不用强调色**。
              聚焦只靠"提亮"表达（灰 → 标题色），跟贴图按钮的聚焦圈各管各的，
              不会在面板里多出一颗突兀的彩色圆。 */}
          <Text
            class={
              props.focused() && props.cursor() === CTRL_ORDER.indexOf("lyric")
                ? pTxt("rowTitle")
                : pTxt("lyricBtn")
            }
          >
            词
          </Text>
        </View>
      </View>

      {/* 时间 + 进度条 + 时间：**永远**左边 0:00、右边总长，几何固定。
          状态词在上一行，绝不顶替这里的文本 ——
          以前直接顶替左边的时间，字符串一变长就把进度条挤到边上。 */}
      <View class="flex-row items-center justify-between w-full">
        <Text class={pTxt("time")}>{posLabel()}</Text>
        <View class={bgCls("progressTrack")}>
          {/* 缓存条（浅色）画在下面，播放进度条（红）盖在上面 */}
          <View
            class={bgCls("bufferFill")}
            style={{ width: Math.min(BAR_PX, Math.max(0, props.buffered()) * BAR_PX) }}
          />
          <View class={bgCls("progressFill")} style={{ width: fillWidth() }} />
        </View>
        <Text class={pTxt("time")}>{durLabel()}</Text>
      </View>

      {/* 控制行：顺序 / 上一首 · 播放 · 下一首 / 喜爱 —— 左右各 2 个，
          "词"搬到了上面信息行，所以中间那颗大的播放键天然落在中线上。
          贴图直径 24（大键 36），间距 8：4×24 + 36 + 4×8 = 164 ≤ 180。 */}
      <View class="flex-row items-center justify-center gap-2 w-full">
        {CTRL_ORDER.map((slot, i) => {
          if (slot === "lyric") return null; /* 在上面信息行里 */
          const big = slot === "play";
          const on = props.focused() && props.cursor() === i;
          return (
            <Image
              src={iconFor(slot, on)}
              class={big ? "relative w-[36] h-[36]" : "relative w-[24] h-[24]"}
            />
          );
        })}
      </View>
    </View>
  );
}
