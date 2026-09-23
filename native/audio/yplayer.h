#ifndef YUNYIN_YPLAYER_H
#define YUNYIN_YPLAYER_H

#include <stddef.h>

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
 * Formats: MP3 (mpg123), OGG (vorbisfile), WAV (dr_wav),
 *          FLAC (dr_flac), OPUS (opusfile), M4A/AAC (ym4a.c + yaac.c).
 *
 * M4A 是容器不是编码：`ym4a.c` 走 MP4 盒子、把 `mdat` 里的裸 AAC 帧交出来，
 * `yaac.c` 用 Vita 的硬件 AAC 块解码。这里不需要 FFmpeg。
 */
int  yp_open(const char *path);
int  yp_rate(void);
int  yp_channels(void);
int  yp_decode(short *buf, int max_frames);
int  yp_seek(long long frame);
long long yp_position(void);
long long yp_length(void);
void yp_close(void);
const unsigned char *yp_cover(int *len);

#ifdef __cplusplus
}
#endif

#endif
