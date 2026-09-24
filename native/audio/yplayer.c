#include "yplayer.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ctype.h>

#include <mpg123.h>
#include <vorbis/vorbisfile.h>
#define DR_WAV_IMPLEMENTATION
#include "vendor/dr_wav.h"
#define DR_FLAC_IMPLEMENTATION
#include "vendor/dr_flac.h"
#include "vendor/opus/opusfile.h"
#include "ym4a.h"
#include "yaac.h"
#include "yp_io.h"
#include "host/yunyin_log.h"

/* 一帧 AAC 解出来是 1024 个采样（带 SBR 时 2048）；硬件解码器单帧输出
 * 绝不会超过 SCE_AUDIODEC_AAC_MAX_SAMPLES。 */
#define YP_AAC_MAX_FRAMES 2048

/* Opus 交给我们的永远是 48 kHz 立体声。opusfile 官方文档推荐一次 120 ms，
 * 携带缓冲区就按这个尺寸开。 */
#define YP_OPUS_CARRY_FRAMES 5760

typedef struct yp_state {
  int fmt;   /* 0 none, 1 mp3, 2 ogg, 3 wav, 4 flac, 5 opus, 6 m4a */
  int rate;  /* native source rate */
  int ch;    /* native source channels (1 or 2) */
  const yp_io *io;   /* 当前输入（Phase 1 起所有格式都从这里读） */
  int owns_io;       /* 1 = yp_close() 负责关闭这份 io */
  long long duration_hint_ms; /* 调用方给的时长提示（§37），为 0 表示没有 */
  mpg123_handle *mp3;
  OggVorbis_File vf;
  int vf_ok;
  drwav wav;
  int wav_ok;
  unsigned long long wav_frames;
  drflac *flac;
  unsigned long long flac_frames;
  OggOpusFile *of;
  /* Opus 携带缓冲区：op_read/op_read_stereo 一次最多给一个完整 packet，
   * 所以这里一次解一大块，再用好几个输出缓冲区慢慢放（见 yp_opus_decode）。 */
  short opus_pcm[YP_OPUS_CARRY_FRAMES * 2];
  int opus_len;
  int opus_pos;
  long long opus_played; /* frames already handed to the output */
  /* M4A：解复用器一帧一帧给裸 AAC，硬件解码器把它变成 PCM，
   * 播放器再用几个输出缓冲区把这些 PCM 放完。 */
  short m4a_pcm[YP_AAC_MAX_FRAMES * 2];
  int m4a_pcm_len;
  int m4a_pcm_pos;
  long long m4a_pos; /* last reported position, for the gap before a decode */
  unsigned char *cover;
  size_t cover_len;
} yp_state;

static yp_state g;

/* ---------------------------------------------------------------- 输入层 -- */
/*
 * 五个库各有自己的回调签名，这里统一转发到同一个 yp_io（任务书 §34）。
 * 只做转发，不做等待、不做重试 —— "数据够不够"由上层 Gate 决定（§15/§65）。
 */

static long long io_read_at(const yp_io *io, void *dst, unsigned long long n) {
  if (!io || !io->read) return -1;
  return io->read(io->ctx, dst, n);
}

static long long io_seek_to(const yp_io *io, long long off, int whence) {
  if (!io || !io->seek) return -1;
  return io->seek(io->ctx, off, whence);
}

/* mpg123：读回调返回实际字节数（0 = EOF），lseek 返回新位置 */
static ssize_t mp3_io_read(void *ctx, void *buf, size_t n) {
  return (ssize_t)io_read_at((const yp_io *)ctx, buf, (unsigned long long)n);
}

static off_t mp3_io_lseek(void *ctx, off_t off, int whence) {
  return (off_t)io_seek_to((const yp_io *)ctx, (long long)off, whence);
}

/* vorbisfile */
static size_t ogg_io_read(void *ptr, size_t size, size_t nmemb, void *ctx) {
  unsigned long long want = (unsigned long long)size * nmemb;
  long long got = io_read_at((const yp_io *)ctx, ptr, want);
  if (got <= 0) return 0;
  return (size_t)got / (size ? size : 1);
}

static int ogg_io_seek(void *ctx, ogg_int64_t off, int whence) {
  return io_seek_to((const yp_io *)ctx, (long long)off, whence) < 0 ? -1 : 0;
}

static int ogg_io_close(void *ctx) { (void)ctx; return 0; /* io 由播放器关 */ }

static long ogg_io_tell(void *ctx) {
  const yp_io *io = (const yp_io *)ctx;
  if (!io || !io->tell) return -1;
  return (long)io->tell(io->ctx);
}

/* dr_wav / dr_flac：读回调签名相同，seek 回调各自有自己的枚举类型 */
static size_t dr_io_read(void *user, void *dst, size_t n) {
  long long got = io_read_at((const yp_io *)user, dst, (unsigned long long)n);
  return got <= 0 ? 0 : (size_t)got;
}

/* dr_wav/dr_flac 只区分 start 与 current（"跳到结尾"用 current + 0x7FFFFFFF 表达） */
static drwav_bool32 drwav_io_seek(void *user, int off, drwav_seek_origin origin) {
  int whence = origin == drwav_seek_origin_current ? SEEK_CUR : SEEK_SET;
  return io_seek_to((const yp_io *)user, (long long)off, whence) < 0 ? 0 : 1;
}

static drflac_bool32 drflac_io_seek(void *user, int off,
                                    drflac_seek_origin origin) {
  int whence = origin == drflac_seek_origin_current ? SEEK_CUR : SEEK_SET;
  return io_seek_to((const yp_io *)user, (long long)off, whence) < 0 ? 0 : 1;
}

/* opusfile */
static int opus_io_read(void *ctx, unsigned char *ptr, int nbytes) {
  long long got = io_read_at((const yp_io *)ctx, ptr, (unsigned long long)nbytes);
  return (int)got;
}

static int opus_io_seek(void *ctx, opus_int64 off, int whence) {
  return io_seek_to((const yp_io *)ctx, (long long)off, whence) < 0 ? -1 : 0;
}

static opus_int64 opus_io_tell(void *ctx) {
  const yp_io *io = (const yp_io *)ctx;
  if (!io || !io->tell) return -1;
  return (opus_int64)io->tell(io->ctx);
}

static int opus_io_close(void *ctx) { (void)ctx; return 0; /* io 由播放器关 */ }

/* ------------------------------------------------------------------ 嗅探 -- */
/*
 * 按字节判断容器（任务书 §38/§39）：网络流没有可用后缀，本地文件也可能被改名。
 * 返回本文件内部的 fmt 编号；0 = 认不出来（那时才退回按后缀判断）。
 */
static int yp_sniff(const unsigned char *b, int n) {
  if (n >= 4 && b[0] == 'f' && b[1] == 'L' && b[2] == 'a' && b[3] == 'C')
    return 4;  /* FLAC */
  if (n >= 4 && b[0] == 'O' && b[1] == 'g' && b[2] == 'g' && b[3] == 'S') {
    int i, limit = n < 64 ? n : 64;
    for (i = 0; i + 8 <= limit; i++) {
      if (memcmp(b + i, "OpusHead", 8) == 0) return 5;
      if (memcmp(b + i, "vorbis", 6) == 0) return 2;
    }
    return 2; /* 认不出编码名时按 Ogg/Vorbis 处理 */
  }
  if (n >= 12 && b[4] == 'f' && b[5] == 't' && b[6] == 'y' && b[7] == 'p')
    return 6;  /* MP4 家族：M4A */
  if (n >= 12 && b[8] == 'W' && b[9] == 'A' && b[10] == 'V' && b[11] == 'E')
    return 3;  /* RIFF/WAVE */
  if (n >= 3 && b[0] == 'I' && b[1] == 'D' && b[2] == '3') return 1; /* MP3 */
  if (n >= 2 && b[0] == 0xFF && (b[1] & 0xE0) == 0xE0) return 1;     /* MP3 帧同步 */
  return 0;
}

static void yp_clear(void) {
  if (g.mp3) { mpg123_close(g.mp3); mpg123_delete(g.mp3); g.mp3 = NULL; }
  if (g.vf_ok) { ov_clear(&g.vf); g.vf_ok = 0; }
  if (g.wav_ok) { drwav_uninit(&g.wav); g.wav_ok = 0; }
  if (g.flac) { drflac_close(g.flac); g.flac = NULL; }
  if (g.of) { op_free(g.of); g.of = NULL; }
  yaac_close();
  ym4a_close();
  /* 输入层：只有"由我们打开"的那份 IO 才由我们关闭（见 yp_open_io 的 owns_io）。 */
  if (g.owns_io && g.io && g.io->close) g.io->close(g.io->ctx);
  g.io = NULL;
  g.owns_io = 0;
  g.duration_hint_ms = 0;
  if (g.cover) { free(g.cover); g.cover = NULL; }
  g.cover_len = 0;
  g.rate = 0;
  g.ch = 0;
  g.fmt = 0;
  g.wav_frames = 0;
  g.flac_frames = 0;
  g.opus_len = 0;
  g.opus_pos = 0;
  g.opus_played = 0;
  g.m4a_pcm_len = 0;
  g.m4a_pcm_pos = 0;
  g.m4a_pos = 0;
}

/* -------- MP3 (mpg123) -------- */
static int mp3_open(void) {
  static int inited = 0;
  long long size;
  if (!inited) { mpg123_init(); inited = 1; }
  int err = 0;
  g.mp3 = mpg123_new(NULL, &err);
  if (!g.mp3) return -1;
  size = g.io->size ? g.io->size(g.io->ctx) : -1;
  /*
   * §36：本地文件（长度已知）保留 FORCE_SEEKABLE，seek 才是"真跳转"；
   * 长度未知的流（以后的网络源）绝不能设它 —— 那会让 mpg123 扫完整条流求长度。
   */
  if (size > 0) {
    mpg123_param(g.mp3, MPG123_FLAGS,
                 MPG123_FORCE_SEEKABLE | MPG123_FUZZY | MPG123_GAPLESS |
                     MPG123_PICTURE, 0.0);
  } else {
    mpg123_param(g.mp3, MPG123_FLAGS,
                 MPG123_FUZZY | MPG123_GAPLESS | MPG123_PICTURE, 0.0);
  }
  if (mpg123_replace_reader_handle(g.mp3, mp3_io_read, mp3_io_lseek, NULL) !=
      MPG123_OK)
    return -1;
  if (mpg123_open_handle(g.mp3, (void *)g.io) != MPG123_OK) return -1;
  if (size > 0) mpg123_set_filesize(g.mp3, (off_t)size);
  long r = 0;
  int ch = 0, enc = 0;
  if (mpg123_getformat(g.mp3, &r, &ch, &enc) != MPG123_OK) return -1;
  g.rate = (int)r;
  g.ch = 2; /* always expose stereo; mpg123 downmixes mono/multichannel */
  mpg123_format_none(g.mp3);
  mpg123_format(g.mp3, r, 2, MPG123_ENC_SIGNED_16);

  /* 内嵌封面（type 3 = 正面封面，0 = 其他）。 */
  mpg123_id3v1 *v1 = NULL;
  mpg123_id3v2 *v2 = NULL;
  if (mpg123_id3(g.mp3, &v1, &v2) == MPG123_OK && v2) {
    for (size_t i = 0; i < v2->pictures; i++) {
      mpg123_picture *pic = &v2->picture[i];
      if ((pic->type == 3 || pic->type == 0) && pic->data && pic->size > 0) {
        g.cover = (unsigned char *)malloc(pic->size);
        if (g.cover) {
          memcpy(g.cover, pic->data, pic->size);
          g.cover_len = pic->size;
        }
        break;
      }
    }
  }
  g.fmt = 1;
  return 0;
}

static int mp3_decode(short *buf, int max_frames) {
  size_t done = 0;
  if (mpg123_read(g.mp3, buf, (size_t)max_frames * g.ch * 2, &done) != MPG123_OK)
    return 0;
  return (int)(done / ((size_t)g.ch * 2));
}

/* -------- OGG (vorbisfile) -------- */
static int ogg_open(void) {
  ov_callbacks cb;
  cb.read_func = ogg_io_read;
  cb.seek_func = ogg_io_seek;
  cb.close_func = ogg_io_close;
  cb.tell_func = ogg_io_tell;
  if (ov_open_callbacks((void *)g.io, &g.vf, NULL, 0, cb) != 0) return -1;
  vorbis_info *vi = ov_info(&g.vf, -1);
  if (!vi) { ov_clear(&g.vf); return -1; }
  g.vf_ok = 1;
  g.rate = vi->rate;
  g.ch = vi->channels >= 2 ? 2 : 1;
  g.fmt = 2;
  return 0;
}

/* ov_read 一次只吐"当前 packet"的数据，短块（瞬态处）可能只有 128 / 576 个
 * 采样点。以前只读一次就返回，上层把剩下的全部填静音 —— 这个 live 录音里
 * 有 4332 个包短于一个 960 帧缓冲区，等于每三分之一不到的缓冲区漏一段，
 * 听感就是"特别卡"。这里循环补齐整个缓冲区（和 mpg123 / dr_wav 一致）。 */
static int ogg_decode(short *buf, int max_frames) {
  if (max_frames <= 0) return 0;
  if (g.ch == 1) {
    short mono[2048];
    int frames = 0;
    while (frames < max_frames) {
      int bits = 0;
      int want = max_frames - frames;
      if (want > (int)(sizeof mono / sizeof mono[0])) want = (int)(sizeof mono / sizeof mono[0]);
      long n = ov_read(&g.vf, (char *)mono, want * 2, 0, 2, 1, &bits);
      if (n <= 0) break;
      int got = (int)(n / 2);
      for (int i = 0; i < got; i++) {
        buf[(frames + i) * 2] = mono[i];
        buf[(frames + i) * 2 + 1] = mono[i];
      }
      frames += got;
    }
    return frames;
  }
  int frames = 0;
  while (frames < max_frames) {
    int bits = 0;
    long n = ov_read(&g.vf, (char *)(buf + frames * 2),
                     (max_frames - frames) * 4, 0, 2, 1, &bits);
    if (n <= 0) break;
    frames += (int)(n / 4);
  }
  return frames;
}

/* -------- WAV (dr_wav) -------- */
static int wav_open(const char *p) {
  (void)p;
  if (!drwav_init(&g.wav, dr_io_read, drwav_io_seek, (void *)g.io)) return -1;
  g.wav_ok = 1;
  g.rate = (int)g.wav.sampleRate;
  g.ch = g.wav.channels >= 2 ? 2 : 1;
  g.wav_frames = 0;
  g.fmt = 3;
  return 0;
}

static int wav_decode(short *buf, int max_frames) {
  if (g.ch == 1) {
    short mono[1024];
    drwav_uint64 read = drwav_read_pcm_frames_s16(&g.wav, (drwav_uint64)max_frames, mono);
    for (drwav_uint64 i = 0; i < read; i++) { buf[i * 2] = mono[i]; buf[i * 2 + 1] = mono[i]; }
    g.wav_frames += read;
    return (int)read;
  }
  drwav_uint64 read = drwav_read_pcm_frames_s16(&g.wav, (drwav_uint64)max_frames, buf);
  g.wav_frames += read;
  return (int)read;
}

/* -------- FLAC (dr_flac) -------- */
static int flac_open(const char *p) {
  (void)p;
  g.flac = drflac_open(dr_io_read, drflac_io_seek, (void *)g.io);
  if (!g.flac) return -1;
  g.rate = (int)g.flac->sampleRate;
  g.ch = g.flac->channels >= 2 ? 2 : 1;
  g.flac_frames = 0;
  g.fmt = 4;
  return 0;
}

static int flac_decode(short *buf, int max_frames) {
  if (g.ch == 1) {
    short mono[1024];
    drflac_uint64 read = drflac_read_pcm_frames_s16(g.flac, (drflac_uint64)max_frames, mono);
    for (drflac_uint64 i = 0; i < read; i++) { buf[i * 2] = mono[i]; buf[i * 2 + 1] = mono[i]; }
    g.flac_frames += read;
    return (int)read;
  }
  drflac_uint64 read = drflac_read_pcm_frames_s16(g.flac, (drflac_uint64)max_frames, buf);
  g.flac_frames += read;
  return (int)read;
}

/* -------- OPUS (opusfile) -------- */
static int opus_open(const char *p) {
  int err = 0;
  OpusFileCallbacks cb;
  (void)p;
  cb.read = opus_io_read;
  cb.seek = opus_io_seek;
  cb.tell = opus_io_tell;
  cb.close = opus_io_close;
  g.of = op_open_callbacks((void *)g.io, &cb, NULL, 0, &err);
  if (!g.of) return -1;
  g.rate = 48000; /* opus decodes at 48 kHz; BGM port opens at this native rate */
  /* 一律输出立体声：op_read_stereo 会把单声道/多声道下混成两声道，
   * 参考的 Vita 后端也是这么做的。 */
  g.ch = 2;
  g.opus_len = 0;
  g.opus_pos = 0;
  g.opus_played = 0;
  g.fmt = 5;
  return 0;
}

/*
 * Opus 解码，形态照参考的 Vita 后端。
 *
 * opusfile 的接口有两处很容易写错，而且都会真的出问题：
 *
 *   - `_buf_size` 的单位是"值"（帧 × 声道），返回值才是"帧"。把帧数当缓冲区
 *     容量传进去，每次调用都会变得极小 —— 一个输出块的尾巴上甚至只剩一个值。
 *   - 缓冲区装不下一整帧时，libopusfile 返回 0。上层（bgm.rs）会把块里剩下的
 *     部分补静音，于是变成"每隔一小段掉一个采样"—— 也就是这里修掉的"颤音"。
 *
 * 所以：一次调用拉满 120 ms 到携带缓冲区，再用任意多个输出缓冲区把它放完。
 */
static int opus_fill(void) {
  int n = op_read_stereo(g.of, g.opus_pcm, YP_OPUS_CARRY_FRAMES * 2);
  if (n <= 0) return 0;
  if (n > YP_OPUS_CARRY_FRAMES) n = YP_OPUS_CARRY_FRAMES;
  g.opus_len = n;
  g.opus_pos = 0;
  return 1;
}

static int yp_opus_decode(short *buf, int max_frames) {
  int done = 0;
  if (max_frames <= 0) return 0;
  while (done < max_frames) {
    int avail, want;
    if (g.opus_pos >= g.opus_len && !opus_fill()) break;
    avail = g.opus_len - g.opus_pos;
    want = max_frames - done;
    if (want > avail) want = avail;
    if (want <= 0) break;
    memcpy(buf + done * 2, g.opus_pcm + g.opus_pos * 2, (size_t)want * 4);
    g.opus_pos += want;
    done += want;
  }
  g.opus_played += done;
  return done;
}

/* -------- M4A / AAC（ym4a 解复用 + SceAudiodec 硬件解码） -------- */

static int m4a_open(const char *p) {
  int rc;
  (void)p;
  rc = ym4a_open_io(g.io, 0); /* IO 归播放器持有，解复用器只借用 */
  if (rc != YM4A_OK) {
    char msg[64];
    snprintf(msg, sizeof msg, "yplayer: ym4a_open rc=%d\n", rc);
    yunyin_log(msg);
    return -1;
  }
  g.rate = ym4a_rate();
  g.ch = ym4a_channels(); /* 1 or 2; mono is doubled up in m4a_decode */

  /* 一律传 isSbr=1：裸帧里没有 SBR 标志位，两个公开的 Vita 参考实现也都传 1。
   * 对 AAC-LC，解码器每帧仍然只给 1024 个采样，所以它只是"能力提示"、
   * 不是强制上采样 —— 真实输出长度以解码器回报为准。 */
  if (yaac_open(ym4a_channels(), ym4a_rate(), 0, 1) != 0) {
    yunyin_log("yplayer: yaac_open failed\n");
    ym4a_close();
    return -1;
  }
  {
    char msg[128];
    snprintf(msg, sizeof msg,
             "yplayer: m4a %s rate=%d ch=%d aot=%d sbr=%d samples=%d\n",
             ym4a_codec_name(), g.rate, g.ch, ym4a_object_type(), ym4a_sbr(),
             ym4a_sample_count());
    yunyin_log(msg);
  }
  g.m4a_pcm_len = 0;
  g.m4a_pcm_pos = 0;
  g.m4a_pos = 0;
  g.fmt = 6;
  return 0;
}

/* 当前在 PCM 缓冲区里的那一帧 AAC 对应的 PCM 起始帧号，加上已经被放掉的量。 */
static long long m4a_position(void) {
  int idx = ym4a_cur_sample();
  if (idx < 0) return g.m4a_pos;
  return ym4a_sample_start_frame(idx) + (long long)g.m4a_pcm_pos;
}

/* 从容器取一帧并解码。解不出来的帧记一条日志后跳过（给一点重试预算）：
 * 坏一帧只该损失几毫秒音频，不该毁掉整首歌。 */
static int m4a_fill(void) {
  unsigned char au[YAAC_ES_CAP];
  int n, got, failures = 0;
  while (failures < 4) {
    n = ym4a_next_sample(au, (int)sizeof au);
    if (n <= 0) return 0; /* end of track (or unreadable sample) */
    got = yaac_decode(au, n, g.m4a_pcm, YP_AAC_MAX_FRAMES);
    if (got < 0) {
      failures++;
      continue;
    }
    if (got == 0) continue; /* decoder had nothing to emit for this frame */
    /* 最后一帧通常是补齐过的：按容器声明的时长裁掉多余部分，
     * 让进度和总长对得上。只裁最后一帧 —— 中间的输出原样信任。 */
    if (ym4a_cur_sample() == ym4a_sample_count() - 1) {
      int declared = ym4a_sample_frames(ym4a_cur_sample());
      if (declared > 0 && declared < got) got = declared;
    }
    g.m4a_pcm_len = got;
    g.m4a_pcm_pos = 0;
    return 1;
  }
  yunyin_log("yplayer: AAC decode gave up on this track\n");
  return 0;
}

static int m4a_decode(short *buf, int max_frames) {
  int done = 0;
  if (max_frames <= 0) return 0;
  while (done < max_frames) {
    int avail, want, i;
    if (g.m4a_pcm_pos >= g.m4a_pcm_len && !m4a_fill()) break;
    avail = g.m4a_pcm_len - g.m4a_pcm_pos;
    want = max_frames - done;
    if (want > avail) want = avail;
    if (want <= 0) break;
    if (g.ch == 1) {
      for (i = 0; i < want; i++) {
        short s = g.m4a_pcm[g.m4a_pcm_pos + i];
        buf[(done + i) * 2] = s;
        buf[(done + i) * 2 + 1] = s;
      }
    } else {
      memcpy(buf + done * 2, g.m4a_pcm + g.m4a_pcm_pos * 2,
             (size_t)want * 4);
    }
    g.m4a_pcm_pos += want;
    done += want;
  }
  if (done > 0) g.m4a_pos = m4a_position();
  return done;
}

/* -------- public -------- */

/* 按后缀兜底识别（只在"嗅字节认不出来"时用，比如空文件或未知容器）。 */
static int yp_format_from_path(const char *path) {
  size_t L, i;
  char lo[1024];
  if (!path || !*path) return 0;
  L = strlen(path);
  if (L >= sizeof lo) return 0;
  for (i = 0; i < L; i++) lo[i] = (char)tolower((unsigned char)path[i]);
  lo[L] = 0;
  if (strstr(lo, ".mp3")) return 1;
  if (strstr(lo, ".ogg") || strstr(lo, ".oga")) return 2;
  if (strstr(lo, ".wav")) return 3;
  if (strstr(lo, ".flac")) return 4;
  if (strstr(lo, ".opus")) return 5;
  if (strstr(lo, ".m4a") || strstr(lo, ".m4b") || strstr(lo, ".mp4")) return 6;
  return 0;
}

/* 读前 64 字节嗅探格式，然后把游标复位，让解码器从 0 开始。 */
static int yp_sniff_head(void) {
  unsigned char head[64];
  long long got;
  if (!g.io || !g.io->read || !g.io->seek) return 0;
  if (io_seek_to(g.io, 0, SEEK_SET) < 0) return 0;
  got = io_read_at(g.io, head, sizeof head);
  io_seek_to(g.io, 0, SEEK_SET);
  if (got <= 0) return 0;
  return yp_sniff(head, (int)got);
}

/* 按格式分派到对应的解码器（都从 g.io 读）。 */
static int yp_open_decoder(int fmt, const char *path_hint) {
  switch (fmt) {
    case 1: return mp3_open();
    case 2: return ogg_open();
    case 3: return wav_open(path_hint);
    case 4: return flac_open(path_hint);
    case 5: return opus_open(path_hint);
    case 6: return m4a_open(path_hint);
    default: return -1;
  }
}

/*
 * 主入口：把一份 yp_io 交给正确的解码器。
 *   owns_io != 0 时，yp_close() 会调用 io->close() 释放它；
 *   path_hint / format_hint / duration_hint_ms 都是"提示"，可以为空或 -1/0。
 *
 * 目前内部实现是**单实例**（同一时刻只放一首歌，和播放器一致），
 * 返回的 handle 就是那份状态；Phase 2 若要同时开两路源，这里才需要改成多实例。
 */
yp_player *yp_open_io(const yp_io *io, int owns_io, const char *path_hint,
                      int format_hint, long long duration_hint_ms) {
  int fmt;
  if (!io || !io->read) return NULL;
  yp_clear();
  g.io = io;
  g.owns_io = owns_io ? 1 : 0;
  g.duration_hint_ms = duration_hint_ms > 0 ? duration_hint_ms : 0;

  /* 格式判定顺序（§38/§39/§70）：调用方给的提示 → 嗅字节 → 后缀兜底。 */
  fmt = (format_hint > 0 && format_hint <= 6) ? format_hint : 0;
  if (fmt == 0) fmt = yp_sniff_head();
  if (fmt == 0) fmt = yp_format_from_path(path_hint);
  if (fmt == 0) {
    yunyin_log("yplayer: 认不出格式（既嗅不出魔数，也没有可用后缀）\n");
    yp_clear();
    return NULL;
  }
  if (yp_open_decoder(fmt, path_hint) != 0) {
    yp_clear();
    return NULL;
  }
  return &g;
}

/* 本地文件便利入口：自己开一份文件 IO，并交给 yp_close() 负责关闭。 */
yp_player *yp_open(const char *path) {
  static yp_io file_io; /* 单实例：与 yp_open_io 的实现一致 */
  if (!path || !*path) return NULL;
  if (yp_io_file_open(&file_io, path) != 0) return NULL;
  return yp_open_io(&file_io, 1, path, 0, -1);
}

int yp_rate(const yp_player *p) { (void)p; return g.rate; }
int yp_channels(const yp_player *p) { (void)p; return g.ch; }

int yp_decode(yp_player *p, short *buf, int max_frames) {
  (void)p;
  switch (g.fmt) {
    case 1: return mp3_decode(buf, max_frames);
    case 2: return ogg_decode(buf, max_frames);
    case 3: return wav_decode(buf, max_frames);
    case 4: return flac_decode(buf, max_frames);
    case 5: return yp_opus_decode(buf, max_frames);
    case 6: return m4a_decode(buf, max_frames);
    default: return 0;
  }
}

/* 跳到某个绝对源帧。暂停时解码器停在当前帧不动（上层补静音）；
 * seek 只用于用户明确的跳转。 */
int yp_seek(yp_player *p, long long frame) {
  (void)p;
  if (frame < 0) frame = 0;
  switch (g.fmt) {
    case 1:
      if (!g.mp3) return -1;
      return mpg123_seek(g.mp3, (off_t)frame, SEEK_SET) < 0 ? -1 : 0;
    case 2:
      if (!g.vf_ok) return -1;
      return ov_pcm_seek(&g.vf, (ogg_int64_t)frame) == 0 ? 0 : -1;
    case 3:
      if (!g.wav_ok) return -1;
      if (!drwav_seek_to_pcm_frame(&g.wav, (drwav_uint64)frame)) return -1;
      g.wav_frames = (unsigned long long)frame;
      return 0;
    case 4:
      if (!g.flac) return -1;
      if (!drflac_seek_to_pcm_frame(g.flac, (drflac_uint64)frame)) return -1;
      g.flac_frames = (unsigned long long)frame;
      return 0;
    case 5:
      if (!g.of) return -1;
      if (op_pcm_seek(g.of, (ogg_int64_t)frame) != 0) return -1;
      g.opus_len = 0;
      g.opus_pos = 0;
      g.opus_played = frame;
      return 0;
    case 6:
      if (ym4a_seek_frame(frame) != 0) return -1;
      g.m4a_pcm_len = 0;
      g.m4a_pcm_pos = 0;
      g.m4a_pos = frame;
      return 0;
  }
  return -1;
}

long long yp_position(const yp_player *p) {
  (void)p;
  if (g.fmt == 1 && g.mp3) return (long long)mpg123_tell(g.mp3);
  if (g.fmt == 2 && g.vf_ok) return (long long)ov_pcm_tell(&g.vf);
  if (g.fmt == 3 && g.wav_ok) return (long long)g.wav_frames;
  if (g.fmt == 4 && g.flac) return (long long)g.flac_frames;
  /* 用"已播帧数"，而不是 op_pcm_tell：携带缓冲区最多领先实际输出 120 ms。 */
  if (g.fmt == 5) return g.opus_played;
  if (g.fmt == 6) return m4a_position();
  return 0;
}

long long yp_length(const yp_player *p) {
  (void)p;
  if (g.fmt == 1 && g.mp3) return (long long)mpg123_length(g.mp3);
  if (g.fmt == 2 && g.vf_ok) return (long long)ov_pcm_total(&g.vf, -1);
  if (g.fmt == 3 && g.wav_ok) return (long long)g.wav.totalPCMFrameCount;
  if (g.fmt == 4 && g.flac) return (long long)g.flac->totalPCMFrameCount;
  if (g.fmt == 5 && g.of) return (long long)op_pcm_total(g.of, -1);
  if (g.fmt == 6) return ym4a_total_frames();
  /* 容器/解码器都报不出长度时（例如网络 MP3 还没扫完），用调用方给的时长提示。 */
  if (g.duration_hint_ms > 0 && g.rate > 0)
    return (long long)g.duration_hint_ms * g.rate / 1000;
  return 0;
}

const unsigned char *yp_cover(yp_player *p, int *len) {
  (void)p;
  if (len) *len = (int)g.cover_len;
  return g.cover_len ? g.cover : NULL;
}

void yp_close(yp_player *p) {
  (void)p;
  yp_clear();
}
