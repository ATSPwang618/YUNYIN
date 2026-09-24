#ifndef YUNYIN_YM4A_H
#define YUNYIN_YM4A_H

#include "yp_io.h"

#ifdef __cplusplus
extern "C" {
#endif

/*
 * M4A / MP4 音频解复用器（"拆盒子"的那一层）。
 *
 * M4A 只是盒子（容器），里面 `mdat` 里装的才是真正的音频数据 AAC。本模块只负责
 * 搬运：走 ftyp/moov/trak/mdia/minf/stbl，挑出 `soun` 音轨，从 `esds` 读出 AAC
 * 参数，重建样本表（stts/stsc/stsz/stco），让调用方可以**一次取一帧裸 AAC**，
 * 而不必把整个 `mdat` 读进内存。
 *
 * 它刻意不碰解码、也不依赖 Vita：硬件解码在 `yaac.c` 里。因此本文件在电脑上也能
 * 用普通 gcc（不需要 VitaSDK）编译并测试。
 *
 * 用法：
 *   if (ym4a_open(path) != 0) fail;
 *   while ((n = ym4a_next_sample(au, sizeof au)) > 0) decode(au, n);
 *   ym4a_close();
 */

/* ym4a_open 的返回码。 */
#define YM4A_OK            0
#define YM4A_ERR_IO       -1  /* missing / unreadable / truncated */
#define YM4A_ERR_FORMAT   -2  /* not an MP4 container (no moov / no stbl) */
#define YM4A_ERR_NO_AUDIO -3  /* no `soun` track */
#define YM4A_ERR_CODEC    -4  /* audio track is not AAC */
#define YM4A_ERR_MEMORY   -5  /* sample table did not fit */
#define YM4A_ERR_TABLE    -6  /* sample table is inconsistent */

/* 走 yp_io 打开（Phase 1 起的主入口）；owns_io != 0 表示由 ym4a_close() 负责关闭它。 */
int  ym4a_open_io(const yp_io *io, int owns_io);

/* 本地文件便利入口：自己开文件 IO。成功返回 0（YM4A_OK）。 */
int  ym4a_open(const char *path);
void ym4a_close(void);
int  ym4a_ready(void);

int  ym4a_rate(void);                    /* decoder output rate (Hz) */
int  ym4a_channels(void);                /* 1 or 2 */
int  ym4a_object_type(void);             /* AAC AudioObjectType (2 = LC) */
int  ym4a_sbr(void);                     /* 1 when the config signals SBR */

long long ym4a_total_frames(void);       /* PCM frames per channel, whole track */
int  ym4a_sample_count(void);            /* AAC access units in `mdat` */
int  ym4a_cur_sample(void);              /* index of the last sample read, -1 before */

/* 第 idx 帧 AAC 对应的 PCM 起始帧号（按容器声明的时间轴算）。 */
long long ym4a_sample_start_frame(int idx);
/* 容器声明这一帧 AAC 含多少 PCM 帧（媒体时间刻度）。 */
int  ym4a_sample_frames(int idx);

/*
 * 读下一帧 AAC（裸数据，没有 ADTS 头）到 `dst`。
 * 返回字节数；0 表示读完；负数是错误（读失败 / 超过 cap）。
 */
int  ym4a_next_sample(unsigned char *dst, int cap);

/* 把读游标移到"包含 PCM 帧号 frame"的那一帧 AAC 上。 */
int  ym4a_seek_frame(long long frame);

/*
 * AudioSpecificConfig：解码器要知道采样率/声道/SBR，而这些**不在**裸帧里。
 * 最多拷 `cap` 字节，返回长度。
 */
int  ym4a_asc(unsigned char *dst, int cap);
const char *ym4a_codec_name(void);

#ifdef __cplusplus
}
#endif

#endif /* YUNYIN_YM4A_H */
