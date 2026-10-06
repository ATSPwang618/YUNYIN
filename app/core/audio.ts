/* 播放后端：Vita 原生解码优先，主机没挂 vitaMedia 时退回墙钟模拟。
 * 零行为搬迁 —— 只把 media() 的引用换成模块导入。 */
import { media } from "./media";
import type { Track } from "./types";

/* =========================================================
 * AUDIO BACKEND
 *
 * UI 仍然认 position / playing / finishTrack。
 * onFrame 只从 backend 读当前位置。
 *
 * 优先级：
 *   1) 有 audioPath 且 host 挂了 vitaMedia：走 Vita 原生解码 (MP3/OGG/WAV/FLAC/OPUS/M4A)
 *   2) 否则墙钟模拟
 * ======================================================= */

export const audioEngine = {
  loadedPath: "",
  mode: "clock" as "clock" | "vita",
  sampleRate: 44100,
  clockOriginMs: 0,
  clockOffsetMs: 0,
  running: false,

  /* Native state() 约 10Hz 读取；UI 仍由 onFrame 60Hz 更新。 */
  statePollFrames: 0,
  statePollIntervalFrames: 6,
  nativePosMs: 0,
  nativeDurMs: 0,
  nativePlaying: false,
  nativePaused: false,
  nativeSampleAtMs: 0,
  nativeSampleValid: false,
  nativePath: "",
  /* 原生侧最近一次在线打开失败的原因（空串 = 没有）。界面拿它把
   * "没声音还卡着"变成一行明确提示。 */
  nativeError: "",

  load(song: Track) {
    const realPath = song.audioPath || "";
    this.loadedPath = realPath;
    this.clockOffsetMs = 0;
    this.clockOriginMs = Date.now();
    this.running = false;
    this.statePollFrames = 0;
    this.nativePosMs = 0;
    this.nativeDurMs = 0;
    this.nativePlaying = false;
    this.nativePaused = false;
    this.nativeSampleAtMs = Date.now();
    this.nativeSampleValid = false;
    this.nativeError = "";

    const vm = media();
    this.mode = realPath && vm && vm.play && vm.state ? "vita" : "clock";
  },

  play() {
    this.running = true;
    this.clockOriginMs = Date.now();
    this.statePollFrames = 0;
    const vm = media();

    if (this.mode === "vita" && vm && vm.play) {
      try {
        /* 暂停后恢复播放：必须走原生 resume。
         * vm.play() 会重新打开文件、另起一个解码线程，等于从 0 重播 —— 
         * 这正是“点暂停再点播放会从头开始”的原因。带上路径是为了极端情况下
         * （暂停时正好放到结尾、解码线程已经退出）还能退化成重新开一首。 */
        if (this.nativePaused && vm.resume) {
          vm.resume(this.loadedPath);
        } else {
          vm.play(this.loadedPath);
        }
        this.nativeSampleValid = false;
      } catch {
        this.mode = "clock";
      }
    }
  },

  pause() {
    if (this.running) this.clockOffsetMs = this.positionMs();
    this.running = false;
    const vm = media();

    if (this.mode === "vita" && vm && vm.pause) {
      try { vm.pause(); } catch { /* ignore */ }
    }

    if (this.mode === "vita") {
      this.nativePlaying = false;
      this.nativePaused = true;
      this.nativePosMs = this.clockOffsetMs;
      this.nativeSampleAtMs = Date.now();
      this.nativeSampleValid = true;
    }
  },

  stop() {
    this.running = false;
    this.clockOffsetMs = 0;
    this.clockOriginMs = Date.now();
    this.statePollFrames = 0;
    this.nativePosMs = 0;
    this.nativeDurMs = 0;
    this.nativePlaying = false;
    this.nativePaused = false;
    this.nativeSampleAtMs = Date.now();
    this.nativeSampleValid = false;
    const vm = media();

    if (this.mode === "vita" && vm && vm.stop) {
      try { vm.stop(); } catch { /* ignore */ }
    }
  },

  pump() {},

  refreshNativeState(): boolean {
    const vm = media();
    if (this.mode !== "vita" || !vm || !vm.state) return false;

    try {
      const st = JSON.parse(vm.state() || "{}") as {
        playing?: boolean; paused?: boolean; pos?: number; dur?: number;
      };
      this.nativePosMs = Math.max(0, Number(st.pos) || 0);
      this.nativeDurMs = Math.max(0, Number(st.dur) || 0);
      this.nativePlaying = !!st.playing && !st.paused;
      this.nativePaused = !!st.paused;
      this.nativePath = String((st as { path?: string }).path || "");
      this.nativeError = String((st as { err?: string }).err || "");
      this.nativeSampleAtMs = Date.now();
      this.nativeSampleValid = true;
      return true;
    } catch {
      return false;
    }
  },

  snapshot(force = false): { posMs: number; durMs: number; playing: boolean; path: string; error: string } {
    if (this.mode === "vita") {
      this.statePollFrames += 1;
      if (force || !this.nativeSampleValid || this.statePollFrames >= this.statePollIntervalFrames) {
        this.statePollFrames = 0;
        this.refreshNativeState();
      }

      if (this.nativeSampleValid) {
        let pos = this.nativePosMs;
        if (this.nativePlaying) pos += Math.max(0, Date.now() - this.nativeSampleAtMs);
        return {
          posMs: Math.max(0, pos),
          durMs: this.nativeDurMs,
          playing: this.nativePlaying,
          path: this.nativePath,
          error: this.nativeError,
        };
      }
    }

    return {
      posMs: this.clockFallback(),
      durMs: 0,
      playing: this.running,
      path: this.loadedPath,
      error: "",
    };
  },

  clockFallback(): number {
    if (!this.running) return this.clockOffsetMs;
    return this.clockOffsetMs + Math.max(0, Date.now() - this.clockOriginMs);
  },

  positionMs(): number { return this.snapshot(true).posMs; },
};

