#ifndef YUNYIN_YAAC_H
#define YUNYIN_YAAC_H

#ifdef __cplusplus
extern "C" {
#endif

/*
 * 用 Vita 自带的硬件解码块解码 AAC（SceAudiodec）。
 *
 * 拆盒子的活在 `ym4a.c`；这里负责把 `mdat` 里的裸 AAC 帧真正变成 PCM。
 * 只用公开 API —— sceAudiodecInitLibrary / CreateDecoder / Decode / DeleteDecoder，
 * 不碰任何 `*Internal` 私有导入，也没有 FFmpeg。
 *
 * 不在 Vita 上时（未定义 `__vita__`）所有函数退化为桩，
 * 好让同一份源码在电脑上也能编译。
 */

#define YAAC_ERR_INIT     -1
#define YAAC_ERR_MEMORY   -2
#define YAAC_ERR_DECODE   -3
#define YAAC_ERR_TOO_BIG  -4
#define YAAC_ERR_STATE    -5

/* 能接受的单帧最大字节数。硬件公开的 AAC 上限是 1536 字节，
 * 这里留出余量，免得遇到偏大的帧就被判失败。 */
#define YAAC_ES_CAP 4096

/*
 * 为一条音轨启动解码器。
 *   channels 取 1~2；rate 是 Hz；M4A/MP4 用裸帧，所以 is_adts = 0；
 *   is_sbr 是"这条流可能有 SBR"的提示（帧头本身看不出来）。
 * 成功返回 0，失败返回负值。
 */
int yaac_open(int channels, int rate, int is_adts, int is_sbr);

/*
 * 解码一帧 AAC。把交错排列的 16 位 PCM 写进 `out`，返回**每声道帧数**；
 * 解码器没产出时返回 0，出错返回负的 YAAC_ERR_*。`out_cap_frames` 是 `out` 的容量。
 */
int yaac_decode(const unsigned char *au, int len, short *out, int out_cap_frames);

void yaac_close(void);
int yaac_ready(void);
int yaac_channels(void);
int yaac_rate(void);
/* 最近一次 sceAudiodec 的返回值（0 表示没有），用于写调试日志。 */
int yaac_last_status(void);

#ifdef __cplusplus
}
#endif

#endif /* YUNYIN_YAAC_H */
