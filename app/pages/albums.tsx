import { createEffect, createSignal, Show, untrack } from "solid-js";
import { Text, View } from "@pocketjs/framework/components";
import { type Album } from "../core/types";
import { clipW } from "../core/util";
import { pTxt } from "../core/theme";
import { AlbumRow } from "../components/rows";
import { FocusIndicator } from "../components/focus_indicator";
import { LIST_WINDOW } from "./tracks";
import { logEnabled, logMsg } from "../core/media";

/* 专辑页：**纯文字列表**（专辑名 + 歌手 + 曲目数）。
 * 封面网格已砍 —— 一屏三张大图换来的性能开销不值得。 */

export function AlbumListPage(props: {
  albums: Album[];
  cursor: () => number;
  start: () => number;
  active: () => boolean;
  debugName?: string;
}) {
  const ALBUM_POOL = LIST_WINDOW + 2;
  const ROW_PITCH = 36;
  type AlbumSlot = { index: () => number; setIndex: (value: number) => void };
  const slots: AlbumSlot[] = Array.from({ length: ALBUM_POOL }, () => {
    const [index, setIndex] = createSignal(-1);
    return { index, setIndex };
  });
  let lastStart = -1;
  let lastTotal = -1;
  let lastTrace = "";

  createEffect(() => {
    const total = props.albums.length;
    const requested = props.start();
    const from = Math.max(0, Math.min(requested, Math.max(0, total - LIST_WINDOW)));
    const step = from - lastStart;
    const canRecycle = lastStart >= 0 && total === lastTotal && Math.abs(step) === 1;

    if (canRecycle) {
      const leaving = step > 0 ? lastStart : lastStart + ALBUM_POOL - 1;
      const entering = step > 0 ? from + ALBUM_POOL - 1 : from;
      const slot = slots.find((candidate) => untrack(candidate.index) === leaving);
      /* Do not discard the holder at the end of the list; reverse recycling
       * needs its out-of-range index to locate the slot again. */
      if (slot) slot.setIndex(entering);
    } else if (from !== lastStart || total !== lastTotal) {
      for (let i = 0; i < slots.length; i += 1) {
        const index = from + i;
        slots[i].setIndex(index < total ? index : -1);
      }
    }

    lastStart = from;
    lastTotal = total;
    if (logEnabled()) {
      const visibleIds = props.albums
        .slice(from, from + LIST_WINDOW)
        .map((album) => album.id)
        .join(",");
      const trace = `${from}:${total}:${visibleIds}`;
      if (trace !== lastTrace) {
        lastTrace = trace;
        logMsg(
          `perf: album_window name=${props.debugName ?? "albums"} start=${from} ` +
            `total=${total} visible=${Math.min(LIST_WINDOW, Math.max(0, total - from))} ` +
            `recycle=${canRecycle ? 1 : 0} ids=${visibleIds}`,
        );
      }
    }
  });

  const rowAt = (slot: AlbumSlot) => {
    const index = slot.index();
    return index >= 0 && index < props.albums.length ? props.albums[index] : undefined;
  };
  /* 焦点指示器（评审 §7）：专辑行都是 32px 行，间距 4px，位移就是"焦点行 × 36"。 */
  const indicatorY = () =>
    Math.max(0, props.cursor() - props.start()) * 36;

  return (
    <View class="relative flex-col w-full grow overflow-hidden">
      <View class="relative w-full h-[216] overflow-hidden">
        <FocusIndicator y={indicatorY} visible={props.active} />
        {slots.map((slot) => (
          <Show when={rowAt(slot)} keyed>
            {(album) => (
              <View
                style={{
                  posType: 1,
                  insetT: (slot.index() - props.start()) * ROW_PITCH,
                  insetL: 0,
                  insetR: 0,
                  height: 32,
                }}
              >
                <AlbumRow
                  title={clipW(album.title, 26)}
                  artist={clipW(album.artist, 30)}
                  count={album.trackIds.length}
                />
              </View>
            )}
          </Show>
        ))}
      </View>

      <View class="flex-row items-center justify-end h-[16] pr-1">
        <Text class={pTxt("hint")}>
          {props.albums.length > 0
            ? `${props.cursor() + 1} / ${props.albums.length}`
            : "0 / 0"}
        </Text>
      </View>
    </View>
  );
}
