import { EmptyHint } from "../components/rows";

/* 占位页（榜单详情 / 我的歌单 / 最近播放）：居中一句"即将接入"。 */

export function PlaceholderPage(props: { text: string }) {
  return <EmptyHint text={props.text} />;
}
