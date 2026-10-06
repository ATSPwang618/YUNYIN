/* 跨页面共用的小状态（不属于主题 / 曲库 / 播放引擎的那一类）。 */
import { createSignal } from "solid-js";

/* PS 键锁状态（显示在 About 页）：LOCKED / UNLOCKED / FAIL 0x… */
export const [psLockInfo, setPsLockInfo] = createSignal("PS KEY  UNLOCKED");
