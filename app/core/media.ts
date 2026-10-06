/* 原生桥与文件扫描：App 里所有跟 globalThis.vitaMedia 打交道的入口都在这里。
 * 只做搬运（零行为变化），类型定义见 ./types.ts。 */
import type { FsEntry, VitaMedia } from "./types";

/* =========================================================
 * NATIVE MEDIA BRIDGE (globalThis.vitaMedia)
 *
 * native/media.rs 暴露：list / roots / play / pause / resume /
 * stop / state / cover / tags。state() 返回
 * { playing, paused, path, pos, dur, rate, dec }，pos/dur 为毫秒。
 * ======================================================= */


export const media = (): VitaMedia | undefined =>
  (globalThis as unknown as { vitaMedia?: VitaMedia }).vitaMedia;

/* 写 ux0:data/yunyin.log 的原生日志入口（与 scanLibrary 的 log 共用）。 */
export const logMsg = (m: string): void => {
  try {
    (media() as unknown as { logMsg?: (s: string) => void })?.logMsg?.(m);
  } catch {
    /* ignore */
  }
};

/* 只扫描 ux0:/data/yunyin/music（卡上就是 E:\data\yunyin\music）。
 * 系统自带的 ux0:/music 被 Vita 的 SceIo 隐藏、homebrew 打不开；
 * ux0:/data 是普通可访问目录。不再扫整张卡，也不扫旧的 data/music。 */
export const LIB_MOUNTS = ["ux0:/data/yunyin/music"];

/* 音频扩展名：mp3/ogg/wav + 原生支持的 flac/opus/ogg(x) + m4a（里面是 AAC）。 */
export const AUDIO_RE = /\.(mp3|ogg|wav|flac|opus|oga|m4a)$/i;


export function collectAudio(
  path: string,
  depth: number,
  cap: number,
  out: FsEntry[],
): void {
  if (depth > 5 || out.length >= cap) return;
  const api = media();
  if (!api || !api.list) return;
  let items: FsEntry[] = [];
  try {
    items = JSON.parse(api.list(path) || "[]") as FsEntry[];
  } catch {
    items = [];
  }
  logMsg(
    `SCAN ${path} -> ${items.length} item(s): ` +
      items
        .slice(0, 24)
        .map((e) => `${e.dir ? "[D]" : "[F]"}${e.name}`)
        .join(", "),
  );
  for (const e of items) {
    if (out.length >= cap) return;
    if (e.dir) collectAudio(e.path, depth + 1, cap, out);
    else if (AUDIO_RE.test(e.name)) out.push(e);
    else if (!e.name.includes(".")) collectAudio(e.path, depth + 1, cap, out);
  }
}

/* 日志没开（正式版默认）时，连诊断数据的采集都省掉。 */
export const logEnabled = (): boolean => {
  try {
    return !!media()?.logEnabled?.();
  } catch {
    return false;
  }
};

