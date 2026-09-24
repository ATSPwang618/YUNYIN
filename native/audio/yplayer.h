#ifndef YUNYIN_YPLAYER_H
#define YUNYIN_YPLAYER_H

#include <stddef.h>
#include "yp_io.h"

#ifdef __cplusplus
extern "C" {
#endif

/*
 * 流式音频播放器（拉模式，和 ElevenMPV 一样）：
 *   - open(path) 探测格式并初始化对应的解码器
 *   - decode(buf, max_frames) 返回这次实际产出多少帧
 *   - position() / length() 单位是源采样帧（采样率看 rate()）
 *   - cover() 返回内嵌 JPEG/PNG 的指针（没有则为 0）
 *
 * 六个格式：MP3 (mpg123)、OGG (vorbisfile)、WAV (dr_wav)、
 *           FLAC (dr_flac)、OPUS (opusfile)、M4A/AAC (ym4a.c + yaac.c 硬件解码)。
 *
 * M4A 是容器不是编码：`ym4a.c` 走 MP4 盒子、把 `mdat` 里的裸 AAC 帧交出来，
 * `yaac.c` 用 Vita 的硬件 AAC 块解码。这里不需要 FFmpeg。
 *
 * Phase 1 起输入层是 `yp_io`（任务书 §32）：六个格式**都从同一组回调读字节**，
 * 解码循环本身一行没动。本地文件用 `yp_open(path)`；网络源（Phase 2）把
 * HttpRangeSource 的回调传给 `yp_open_io()` 即可。
 *
 * 目前实现是**单实例**（同一时刻放一首歌，和播放器一致），handle 只是把接口
 * 形状定下来；Phase 2 若要同时开两路源，才需要改成多实例。
 */

typedef struct yp_state yp_player; /* 不透明：调用方只当指针用 */

/* 本地文件入口：自己开文件 IO。成功返回 handle，失败返回 NULL。 */
yp_player *yp_open(const char *path);

/*
 * 通用入口（Phase 2 的网络源走这里）：
 *   io           要读的字节流（本地文件 / HTTP Range 实现同一组回调）
 *   owns_io      1 = yp_close() 负责调用 io->close()
 *   path_hint    可选，仅在嗅不出格式时用于按后缀兜底
 *   format_hint  0 = 自动（先嗅字节，再按后缀）；1..6 见 yplayer.c 的 fmt 表
 *   duration_ms  可选时长提示（§37），-1 = 没有
 */
yp_player *yp_open_io(const yp_io *io, int owns_io, const char *path_hint,
                      int format_hint, long long duration_hint_ms);

int  yp_rate(const yp_player *p);
int  yp_channels(const yp_player *p);
int  yp_decode(yp_player *p, short *buf, int max_frames);
int  yp_seek(yp_player *p, long long frame);
long long yp_position(const yp_player *p);
long long yp_length(const yp_player *p);
void yp_close(yp_player *p);
const unsigned char *yp_cover(yp_player *p, int *len);

#ifdef __cplusplus
}
#endif

#endif
