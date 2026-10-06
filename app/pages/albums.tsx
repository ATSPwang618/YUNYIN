import { createMemo } from "solid-js";
import { Text, View } from "@pocketjs/framework/components";
import { type Album } from "../core/types";
import { clipW } from "../core/util";
import { pTxt } from "../core/theme";
import { AlbumRow } from "../components/rows";
import { FocusIndicator } from "../components/focus_indicator";
import { LIST_WINDOW } from "./tracks";

/* 专辑页：**纯文字列表**（专辑名 + 歌手 + 曲目数）。
 * 封面网格已砍 —— 一屏三张大图换来的性能开销不值得。 */

export function AlbumListPage(props: {
  albums: Album[];
  cursor: () => number;
  start: () => number;
  active: () => boolean;
}) {
  const visible = createMemo(() =>
    props.albums.slice(props.start(), props.start() + LIST_WINDOW),
  );
  /* 焦点指示器（评审 §7）：专辑行都是 30px 行，位移就是"焦点行 × 34"。 */
  const indicatorY = () =>
    Math.max(0, props.cursor() - props.start()) * 34;

  return (
    <View class="relative flex-col w-full grow gap-1 overflow-hidden">
      <FocusIndicator y={indicatorY} visible={props.active} />
      {visible().map((album) => (
        <AlbumRow
          title={clipW(album.title, 26)}
          artist={clipW(album.artist, 30)}
          count={album.trackIds.length}
        />
      ))}

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
