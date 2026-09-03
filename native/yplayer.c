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

typedef struct {
  int fmt;   /* 0 none, 1 mp3, 2 ogg, 3 wav, 4 flac, 5 opus */
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
  if (g.cover) { free(g.cover); g.cover = NULL; }
  g.cover_len = 0;
  g.rate = 0;
  g.ch = 0;
  g.fmt = 0;
  g.wav_frames = 0;
  g.flac_frames = 0;
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

static int ogg_decode(short *buf, int max_frames) {
  int bits = 0;
  if (g.ch == 1) {
    short mono[1024 * 2];
    long n = ov_read(&g.vf, (char *)mono, max_frames * 2, 0, 2, 1, &bits);
    if (n <= 0) return 0;
    int frames = n / 2;
    for (int i = 0; i < frames; i++) { buf[i * 2] = mono[i]; buf[i * 2 + 1] = mono[i]; }
    return frames;
  }
  long n = ov_read(&g.vf, (char *)buf, max_frames * 4, 0, 2, 1, &bits);
  if (n <= 0) return 0;
  return n / 4;
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
  g.rate = 48000; /* opus decodes at 48k; round to 44100 upstream */
  g.ch = op_channel_count(g.of, -1) >= 2 ? 2 : 1;
  g.fmt = 5;
  return 0;
}

static int yp_opus_decode(short *buf, int max_frames) {
  /* op_read returns samples per channel; we request stereo into pcm[2*max] */
  short pcm[1024 * 2];
  int cap = max_frames;
  if (cap > 1024) cap = 1024;
  int n = op_read(g.of, pcm, cap, NULL);
  if (n <= 0) return 0;
  if (g.ch == 1) {
    for (int i = 0; i < n; i++) { buf[i * 2] = pcm[i]; buf[i * 2 + 1] = pcm[i]; }
  } else {
    memcpy(buf, pcm, (size_t)n * 4);
  }
  return n;
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
    default: return 0;
  }
}

long long yp_position(void) {
  if (g.fmt == 1 && g.mp3) return (long long)mpg123_tell(g.mp3);
  if (g.fmt == 2 && g.vf_ok) return (long long)ov_pcm_tell(&g.vf);
  if (g.fmt == 3 && g.wav_ok) return (long long)g.wav_frames;
  if (g.fmt == 4 && g.flac) return (long long)g.flac_frames;
  if (g.fmt == 5 && g.of) return (long long)op_pcm_tell(g.of);
  return 0;
}

long long yp_length(void) {
  if (g.fmt == 1 && g.mp3) return (long long)mpg123_length(g.mp3);
  if (g.fmt == 2 && g.vf_ok) return (long long)ov_pcm_total(&g.vf, -1);
  if (g.fmt == 3 && g.wav_ok) return (long long)g.wav.totalPCMFrameCount;
  if (g.fmt == 4 && g.flac) return (long long)g.flac->totalPCMFrameCount;
  if (g.fmt == 5 && g.of) return (long long)op_pcm_total(g.of, -1);
  return 0;
}

const unsigned char *yp_cover(int *len) {
  if (len) *len = (int)g.cover_len;
  return g.cover_len ? g.cover : NULL;
}

void yp_close(void) { yp_clear(); }
