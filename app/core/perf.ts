/* 性能日志：回答"到底是什么操作让画面卡了一下"。
 *
 * 三条纪律（真机上写日志本身就是文件 IO，不能刷屏）：
 *   1. **只在慢的时候写**：单帧间隔 ≥120ms / 单次操作 ≥30ms 才落一行；
 *   2. 每 ~10 秒给一行汇总（平均 fps、慢帧次数、最慢一帧）；
 *   3. 名字要能直接对应到用户的操作（切页 / 读清单 / 起播 / 打开歌单）。
 *
 * 和宿主那两条配合看：
 *   `HOST: guest 帧耗时 Nms`   这一帧是**被原生调用拖慢**的
 *   `HOST: render 耗时 Nms`    卡在渲染/呈现里（不是 JS 的锅）
 *   `perf: …`                  是我们自己的 JS 代码哪一段慢
 */
import { logMsg } from "./media";

/** 一帧超过这个间隔就算"掉帧"（60fps 是 16.7ms，120ms 肉眼已经很卡）。 */
const SLOW_FRAME_MS = 120;
/** 单次操作超过这个时长才值得记一行（30ms ≈ 两帧）。 */
const SLOW_SPAN_MS = 30;
/** 每多少帧给一行汇总（≈10 秒 @60fps）。 */
const SUMMARY_FRAMES = 600;

let lastFrameAt = 0;
let summaryAt = 0;
let frameCount = 0;
let slowFrames = 0;
let worstGapMs = 0;
let frameSerial = 0;
let activeFrame = 0;

/*
 * 慢帧账本：这一帧里每个阶段各花了多久。
 *
 * 为什么需要它：模拟器上一切都很顺，实机才会卡；而现有日志只能告诉我们
 * "这一帧 404ms"，说不出是**谁**吃的。账本记下同帧内的具名操作，
 * 慢帧时一起写出去 —— 下次实机日志就能直接点名（例如
 * `perf: 慢帧 404ms ← 切页面 → 榜单 210ms + netPlaylistTracks 150ms`）。
 *
 * 开销自觉：只在 ≥20ms 时记一笔、最多 8 笔，不吃性能。
 */
let frameBeginAt = 0;
let ledger: string[] = [];

/*
 * 结算**本帧 JS 自己**花的时间。
 *
 * 为什么必须"帧内结算"而不是"下一帧开头结算"：下一帧开头量到的是两帧之间的
 * 墙钟间隔，里面混着宿主渲染 / present / 模拟器的帧间隔等待。上一版就是这么写的，
 * 结果日志里出现 `慢帧 1001ms ← （无具名操作）` 这种**假慢帧** —— JS 明明没干活。
 * 现在只量 JS：帧开始 → 帧回调结束。
 */
function flushFrame(): void {
  if (frameBeginAt === 0) return;
  const dur = Date.now() - frameBeginAt;
  const id = activeFrame;
  frameBeginAt = 0;
  if (dur >= SLOW_FRAME_MS) {
    logMsg(
      `perf: JS 帧 id=${id} ${dur}ms ← ${ledger.length > 0 ? ledger.join(" + ") : "（无具名操作：都在 JS 逻辑/组件重算里）"}`,
    );
  }
}

/** 每帧最前面调一次：开一本新账（上一帧若忘了结算，这里兜底结算）。 */
export function perfFrameBegin(): void {
  flushFrame();
  frameSerial += 1;
  activeFrame = frameSerial;
  frameBeginAt = Date.now();
  if (ledger.length > 0) ledger = [];
}

/** 记一笔"某段花了多久"（≥20ms 才记，最多 8 笔）。 */
export function perfNote(name: string, ms: number): void {
  if (ms < 20 || ledger.length >= 8) return;
  ledger.push(`${name} ${Math.round(ms)}ms`);
}

/** 想手动结算时用（正常路径由下一帧的 perfFrameBegin 结算）。 */
export function perfFrameEnd(): void {
  flushFrame();
}

/** 每帧调一次（放在帧循环最前面）。 */
export function perfFrame(): void {
  const now = Date.now();
  if (lastFrameAt === 0) {
    lastFrameAt = now;
    summaryAt = now;
    return;
  }
  const gap = now - lastFrameAt;
  lastFrameAt = now;
  frameCount += 1;
  if (gap > worstGapMs) worstGapMs = gap;
  if (gap >= SLOW_FRAME_MS) {
    slowFrames += 1;
    logMsg(`perf: 帧间隔 id=${frameSerial} ${gap}ms（第 ${frameCount} 帧起）`);
    /* 一次掉帧就把汇总窗口重置，免得后面几十行都把同一段算进去 */
    if (gap >= 400) {
      summaryAt = now;
      frameCount = 0;
      slowFrames = 1;
      worstGapMs = gap;
      return;
    }
  }
  if (frameCount >= SUMMARY_FRAMES) {
    const span = Math.max(1, now - summaryAt);
    const fps = Math.round((frameCount * 1000) / span);
    logMsg(
      `perf: ${frameCount} 帧 / 平均 ${fps} fps / ≥${SLOW_FRAME_MS}ms 慢帧 ${slowFrames} 次 / 最慢 ${worstGapMs}ms`,
    );
    summaryAt = now;
    frameCount = 0;
    slowFrames = 0;
    worstGapMs = 0;
  }
}

/** 量一段同步操作的耗时；只有 ≥30ms 才写一行。 */
export function perfSpan<T>(name: string, body: () => T): T {
  const t0 = Date.now();
  const out = body();
  const dt = Date.now() - t0;
  if (dt >= SLOW_SPAN_MS) logMsg(`perf: ${name} id=${activeFrame} ${dt}ms`);
  /* 顺手记进慢帧账本：慢帧时就能看出"是哪一段"和"哪一帧"是同一件事。 */
  perfNote(name, dt);
  return out;
}
