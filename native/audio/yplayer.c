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
#include "host/yunyin_log.h"

/* One decoded AAC frame is 1024 samples (2048 with SBR); the hardware decoder
 * never emits more than SCE_AUDIODEC_AAC_MAX_SAMPLES per access unit. */
#define YP_AAC_MAX_FRAMES 2048

/* Opus is always handed to us as stereo at 48 kHz.  opusfile's own docs
 * recommend a 120 ms block, which is what the carry buffer is sized for. */
#define YP_OPUS_CARRY_FRAMES 5760

typedef struct {
  int fmt;   /* 0 none, 1 mp3, 2 ogg, 3 wav, 4 flac, 5 opus, 6 m4a */
  int rate;  /* native source rate */
  int ch;    /* native source channels (1 or 2) */
  mpg123_handle *mp3;
  OggVorbis_File vf;
  int vf_ok;
  drwav wav;
  int wav_ok;
  unsigned long long wav_frames;
  drflac *flac;
  unsigned long long flac_frames;
  OggOpusFile *of;
  /* Opus carry buffer: op_read/op_read_stereo give out at most a full packet
   * per call, so we decode a big block once and drain it over several output
   * buffers (see yp_opus_decode). */
  short opus_pcm[YP_OPUS_CARRY_FRAMES * 2];
  int opus_len;
  int opus_pos;
  long long opus_played; /* frames already handed to the output */
  /* M4A: the demuxer hands out one AAC access unit, the hardware decoder turns
   * it into PCM, and the player drains that PCM over several output buffers. */
  short m4a_pcm[YP_AAC_MAX_FRAMES * 2];
  int m4a_pcm_len;
  int m4a_pcm_pos;
  long long m4a_pos; /* last reported position, for the gap before a decode */
  unsigned char *cover;
  size_t cover_len;
} yp_state;

static yp_state g;

static void yp_clear(void) {
  if (g.mp3) { mpg123_close(g.mp3); mpg123_delete(g.mp3); g.mp3 = NULL; }
  if (g.vf_ok) { ov_clear(&g.vf); g.vf_ok = 0; }
  if (g.wav_ok) { drwav_uninit(&g.wav); g.wav_ok = 0; }
  if (g.flac) { drflac_close(g.flac); g.flac = NULL; }
  if (g.of) { op_free(g.of); g.of = NULL; }
  yaac_close();
  ym4a_close();
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
static int mp3_open(const char *p) {
  static int inited = 0;
  if (!inited) { mpg123_init(); inited = 1; }
  int err = 0;
  g.mp3 = mpg123_new(NULL, &err);
  if (!g.mp3) return -1;
  mpg123_param(g.mp3, MPG123_FLAGS,
               MPG123_FORCE_SEEKABLE | MPG123_FUZZY | MPG123_GAPLESS | MPG123_PICTURE, 0.0);
  if (mpg123_open(g.mp3, p) != MPG123_OK) return -1;
  long r = 0;
  int ch = 0, enc = 0;
  if (mpg123_getformat(g.mp3, &r, &ch, &enc) != MPG123_OK) return -1;
  g.rate = (int)r;
  g.ch = 2; /* always expose stereo; mpg123 downmixes mono/multichannel */
  mpg123_format_none(g.mp3);
  mpg123_format(g.mp3, r, 2, MPG123_ENC_SIGNED_16);

  /* Embedded cover (type 3 = front cover, 0 = other). */
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
static int ogg_open(const char *p) {
  if (ov_fopen(p, &g.vf) != 0) return -1;
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
  if (!drwav_init_file(&g.wav, p)) return -1;
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
  g.flac = drflac_open_file(p);
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
  g.of = op_open_file(p, &err);
  if (!g.of) return -1;
  g.rate = 48000; /* opus decodes at 48 kHz; BGM port opens at this native rate */
  /* Always stereo out: op_read_stereo downmixes mono and multichannel sources,
   * which is also what the reference Vita backend does. */
  g.ch = 2;
  g.opus_len = 0;
  g.opus_pos = 0;
  g.opus_played = 0;
  g.fmt = 5;
  return 0;
}

/*
 * Opus decoding, shaped like the reference Vita backend.
 *
 * Two things about the opusfile API are easy to get wrong, and both have real
 * consequences:
 *
 *   - `_buf_size` counts VALUES (frames x channels); the return value counts
 *     frames.  Passing a frame count as the buffer size makes every call a
 *     tiny one — down to a single value at the tail of an output block.
 *   - A call whose buffer cannot hold one full frame answers 0.  The caller
 *     (bgm.rs) fills the rest of the block with silence, so that becomes a
 *     one-sample hole at an audible cadence — the "warble" this fixes.
 *
 * So: pull a whole 120 ms block into the carry buffer with one call, and drain
 * it across however many output buffers that takes.
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

/* -------- M4A / AAC (ym4a + hardware SceAudiodec) -------- */

static int m4a_open(const char *p) {
  int rc = ym4a_open(p);
  if (rc != YM4A_OK) {
    char msg[64];
    snprintf(msg, sizeof msg, "yplayer: ym4a_open rc=%d\n", rc);
    yunyin_log(msg);
    return -1;
  }
  g.rate = ym4a_rate();
  g.ch = ym4a_channels(); /* 1 or 2; mono is doubled up in m4a_decode */

  /* isSbr=1 unconditionally: the access unit carries no SBR flag, and both
   * public Vita references pass 1.  With AAC-LC the decoder still returns 1024
   * frames per unit, so this is a capability hint rather than a forced
   * upsample — and the real output size is read back from the decoder. */
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

/* PCM frame of the sample currently in the PCM buffer, plus what has already
 * been drained out of it. */
static long long m4a_position(void) {
  int idx = ym4a_cur_sample();
  if (idx < 0) return g.m4a_pos;
  return ym4a_sample_start_frame(idx) + (long long)g.m4a_pcm_pos;
}

/* Pull one access unit from the container and decode it.  A frame the decoder
 * rejects is skipped and logged, with a small retry budget: one damaged AAC
 * frame should cost a few milliseconds of audio, not the rest of the song. */
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
    /* The last unit is usually padded out: trim it to the duration the
     * container declares so position and length agree.  Only the final unit is
     * trimmed — mid-track output is trusted as-is. */
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
int yp_open(const char *path) {
  yp_clear();
  if (!path || !*path) return -1;
  size_t L = strlen(path);
  char lo[1024];
  if (L >= sizeof lo) return -1;
  for (size_t i = 0; i < L; i++) lo[i] = (char)tolower((unsigned char)path[i]);
  lo[L] = 0;

  if (strstr(lo, ".mp3")) return mp3_open(path);
  if (strstr(lo, ".ogg")) return ogg_open(path);
  if (strstr(lo, ".wav")) return wav_open(path);
  if (strstr(lo, ".flac")) return flac_open(path);
  if (strstr(lo, ".opus") || strstr(lo, ".oga")) return opus_open(path);
  /* M4A is the container; the audio inside is AAC.  `.mp4`/`.m4b` are accepted
   * too — the demuxer always picks the `soun` track. */
  if (strstr(lo, ".m4a") || strstr(lo, ".m4b") || strstr(lo, ".mp4"))
    return m4a_open(path);
  return -1;
}

int yp_rate(void) { return g.rate; }
int yp_channels(void) { return g.ch; }

int yp_decode(short *buf, int max_frames) {
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

/* Jump to an absolute source frame. Decoder stays on the current frame
 * during pause (callback fills silence); seek is for explicit jumps. */
int yp_seek(long long frame) {
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

long long yp_position(void) {
  if (g.fmt == 1 && g.mp3) return (long long)mpg123_tell(g.mp3);
  if (g.fmt == 2 && g.vf_ok) return (long long)ov_pcm_tell(&g.vf);
  if (g.fmt == 3 && g.wav_ok) return (long long)g.wav_frames;
  if (g.fmt == 4 && g.flac) return (long long)g.flac_frames;
  /* Played frames, not op_pcm_tell: the carry buffer runs up to 120 ms ahead
   * of what the output has actually consumed. */
  if (g.fmt == 5) return g.opus_played;
  if (g.fmt == 6) return m4a_position();
  return 0;
}

long long yp_length(void) {
  if (g.fmt == 1 && g.mp3) return (long long)mpg123_length(g.mp3);
  if (g.fmt == 2 && g.vf_ok) return (long long)ov_pcm_total(&g.vf, -1);
  if (g.fmt == 3 && g.wav_ok) return (long long)g.wav.totalPCMFrameCount;
  if (g.fmt == 4 && g.flac) return (long long)g.flac->totalPCMFrameCount;
  if (g.fmt == 5 && g.of) return (long long)op_pcm_total(g.of, -1);
  if (g.fmt == 6) return ym4a_total_frames();
  return 0;
}

const unsigned char *yp_cover(int *len) {
  if (len) *len = (int)g.cover_len;
  return g.cover_len ? g.cover : NULL;
}

void yp_close(void) { yp_clear(); }
