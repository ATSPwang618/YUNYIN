/*
 * M4A / MP4 音频解复用器 —— M4A 链路里"搬运工"的那一半。
 *
 * M4A 是盒子（MP4 家族），`mdat` 里装的音乐才是 AAC。本文件找到各个盒子、挑出
 * `soun` 音轨、从 `esds` 读出 AAC 参数、重建样本表，让播放器可以**一次取一帧裸 AAC**。
 * 这里不做任何解码：硬件解码在 `yaac.c`。
 *
 * 不用 FFmpeg、不用容器库、不依赖 Vita —— 同一份源码在电脑上也能编译，
 * 所以没有主机也能拿真实文件测解析。
 */

#include "ym4a.h"
#include "yp_io.h"
#include "host/yunyin_log.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* 上限：够放一小时的曲子，又足够紧，坏文件会当场报错而不是把 Vita 内存吃光。 */
#define YM4A_MAX_TABLE_BYTES (4 * 1024 * 1024)
#define YM4A_MAX_SAMPLES     4000000
#define YM4A_MAX_CHUNKS      4000000
#define YM4A_MAX_STTS        8192
#define YM4A_MAX_STSC        8192
#define YM4A_MAX_ASC         64

typedef struct {
  long long start; /* offset of the box header */
  long long body;  /* offset of the first payload byte */
  long long total; /* whole box size, header included */
} ym4a_box;

typedef struct {
  int ready;
  const yp_io *io;
  int owns_io;   /* 1 = 这份 IO 由 ym4a_close() 关闭 */
  long long file_size;
  long long mdat_body;
  long long mdat_size;

  int rate;      /* output (PCM) sample rate */
  int ch;        /* 1 or 2 */
  int aot;       /* AAC AudioObjectType, 2 = LC */
  int sbr;       /* 1 when the config signals SBR */
  int timescale; /* media timescale of the audio track */
  char codec[5]; /* fourcc of the sample entry we accepted */

  unsigned char asc[YM4A_MAX_ASC];
  int asc_len;

  /* stts：游程编码的（个数, 时长）对 */
  unsigned int *stts_count;
  unsigned int *stts_dur;
  int stts_n;

  /* stsc：chunk 游程。first_chunk 从 1 开始，和盒子里一样。 */
  unsigned int *stsc_first;
  unsigned int *stsc_spc;
  int stsc_n;

  /* stsz：每帧大小 */
  unsigned int *sizes;
  int sample_n;

  /* stco / co64：每个 chunk 的文件偏移 */
  long long *chunk_off;
  int chunk_n;

  int *chunk_first; /* chunk_first[c] = first sample index of chunk c */

  /* 读游标 */
  int cur;  /* next sample index to read */
  int last; /* index returned by the previous read, -1 if none */

  /* 顺序读的偏移缓存：(cc, ci, co) = chunk、下一个样本号、它的偏移 */
  int cc;
  int ci;
  long long co;
} ym4a_t;

static ym4a_t g;

/* ------------------------------------------------------------------ 字节 -- */

static unsigned int be32(const unsigned char *p) {
  return ((unsigned int)p[0] << 24) | ((unsigned int)p[1] << 16) |
         ((unsigned int)p[2] << 8) | (unsigned int)p[3];
}

static unsigned long long be64(const unsigned char *p) {
  return ((unsigned long long)be32(p) << 32) | (unsigned long long)be32(p + 4);
}

/* 按绝对偏移读 n 字节：先 seek 再 read，两个回调都由调用方提供。 */
static int seek_to(const yp_io *io, long long off) {
  if (!io || !io->seek || off < 0) return -1;
  return io->seek(io->ctx, off, SEEK_SET) < 0 ? -1 : 0;
}

static int rd(const yp_io *io, long long off, void *buf, long long n) {
  if (!io || !io->read) return -1;
  if (n < 0 || seek_to(io, off) != 0) return -1;
  return io->read(io->ctx, buf, (unsigned long long)n) == n ? 0 : -1;
}

static unsigned char *slurp(const yp_io *io, long long off, long long n) {
  unsigned char *p;
  if (n <= 0 || n > YM4A_MAX_TABLE_BYTES) {
    yunyin_log("ym4a: table too large\n");
    return NULL;
  }
  p = (unsigned char *)malloc((size_t)n);
  if (!p) return NULL;
  if (rd(io, off, p, n) != 0) {
    free(p);
    return NULL;
  }
  return p;
}

/* ------------------------------------------------------------------ 盒子 -- */

/* 0 = 正常，-1 = 盒子结构坏了。 */
static int box_header(const yp_io *io, long long off, long long end, char type[4],
                      long long *hdr, long long *total) {
  unsigned char b[16];
  unsigned int size;
  long long want = (end - off) < 16 ? (end - off) : 16;
  if (want < 8) return -1;
  if (rd(io, off, b, want) != 0) return -1;
  size = be32(b);
  memcpy(type, b + 4, 4);
  if (size == 1) {
    if (off + 16 > end) return -1;
    *hdr = 16;
    *total = (long long)be64(b + 8);
  } else if (size == 0) {
    *hdr = 8;
    *total = end - off;
  } else {
    *hdr = 8;
    *total = (long long)size;
  }
  if (*total < *hdr || off + *total > end) return -1;
  return 0;
}

/* 在 [from,end) 里往后找第一个 `type` 类型的盒子。
 * 0 = 找到，1 = 没找到，-1 = 结构坏了。 */
static int box_find(const yp_io *io, long long from, long long end, const char type[4],
                    ym4a_box *out) {
  long long off = from;
  while (off + 8 <= end) {
    char t[4];
    long long hdr, total;
    if (box_header(io, off, end, t, &hdr, &total) != 0) return -1;
    if (memcmp(t, type, 4) == 0) {
      out->start = off;
      out->body = off + hdr;
      out->total = total;
      return 0;
    }
    off += total;
  }
  return 1;
}

static long long box_payload(const ym4a_box *b) {
  return b->total - (b->body - b->start);
}

/* ------------------------------------------------------------ 音频参数 -- */

typedef struct {
  const unsigned char *p;
  int n;
  int pos;
} bitreader;

static int br_read(bitreader *b, int nbits) {
  int v = 0;
  int i;
  for (i = 0; i < nbits; i++) {
    int byte = b->pos >> 3;
    int bit;
    if (byte >= b->n) {
      b->pos += nbits - i;
      return -1;
    }
    bit = (b->p[byte] >> (7 - (b->pos & 7))) & 1;
    v = (v << 1) | bit;
    b->pos++;
  }
  return v;
}

static int aac_rate_from_index(int idx) {
  static const int tab[13] = {96000, 88200, 64000, 48000, 44100, 32000, 24000,
                              22050, 16000, 12000, 11025, 8000,  7350};
  return (idx >= 0 && idx < 13) ? tab[idx] : 0;
}

/*
 * AudioSpecificConfig → 编码类型(AOT)、采样率、声道、SBR。
 *
 * 普通 AAC-LC 前两个字节就够了（当初对着的网易云样本是 `12 10` = LC / 44100 / 立体声）。
 * 显式 SBR（AOT 5 / 29）把输出采样率放在 extensionSamplingFrequencyIndex；
 * 隐式 SBR 则藏在 GASpecificConfig 之后的 0x2B7 同步扩展里 —— 所以这里必须逐位解析。
 */
static void parse_asc(const unsigned char *asc, int len, int *aot, int *rate,
                      int *ch, int *sbr) {
  bitreader b;
  int a, sfi, base, channels, out_rate, sbr_flag;

  b.p = asc;
  b.n = len;
  b.pos = 0;
  out_rate = 0;
  sbr_flag = 0;
  channels = 0;
  base = 0;

  a = br_read(&b, 5);
  if (a == 31) {
    int ext = br_read(&b, 6);
    if (ext < 0) return;
    a = 32 + ext;
  }
  sfi = br_read(&b, 4);
  base = aac_rate_from_index(sfi);
  channels = br_read(&b, 4);
  out_rate = base;

  if (a == 5 || a == 29) {
    int ext_rate = aac_rate_from_index(br_read(&b, 4));
    if (ext_rate > 0) {
      out_rate = ext_rate;
      sbr_flag = 1;
    }
  } else if (a == 2 || a == 1 || a == 3 || a == 4) {
    /* GASpecificConfig：frameLengthFlag、dependsOnCoreCoder（可能再跟 14 位）、extensionFlag */
    int depends = br_read(&b, 1);
    int sync_pos;
    (void)br_read(&b, 1); /* frameLengthFlag: not needed here */
    if (depends == 1) br_read(&b, 14);
    (void)br_read(&b, 1); /* extensionFlag */
    sync_pos = b.pos;
    if (br_read(&b, 11) == 0x2B7) {
      if (br_read(&b, 5) == 5 && br_read(&b, 1) == 1) {
        int ext_rate = aac_rate_from_index(br_read(&b, 4));
        if (ext_rate > 0) {
          out_rate = ext_rate;
          sbr_flag = 1;
        }
      }
    } else {
      b.pos = sync_pos;
    }
  }

  if (a > 0) *aot = a;
  if (out_rate > 0) *rate = out_rate;
  if (channels > 2) channels = 2; /* the decoder handles at most 2 */
  if (channels > 0) *ch = channels;
  if (sbr_flag) *sbr = 1;
}

/* esds → AudioSpecificConfig。描述符按 0x03(ES) → 0x04 → 0x05 逐层进。 */
static int parse_esds(const unsigned char *p, int n, unsigned char *asc,
                      int *asc_len) {
  int i = 4; /* version + flags */
  int guard = 0;

  while (i < n && guard++ < 16) {
    int tag;
    int len = 0;
    int b;
    tag = p[i++];
    do {
      if (i >= n) return -1;
      b = p[i++];
      len = (len << 7) | (b & 0x7F);
    } while (b & 0x80);

    if (tag == 0x03) {
      int flags;
      if (i + 3 > n) return -1;
      i += 2; /* ES_ID */
      flags = p[i++];
      if (flags & 0x80) i += 2; /* streamDependenceFlag */
      if (flags & 0x40) {       /* URL_Flag */
        if (i >= n) return -1;
        i += p[i] + 1;
      }
      if (flags & 0x20) i += 2; /* OCRstreamFlag */
      continue;
    }
    if (tag == 0x04) {
      i += 13; /* object type, stream type, buffer size, bitrates */
      if (i > n) return -1;
      continue;
    }
    if (tag == 0x05) {
      if (len <= 0 || len > YM4A_MAX_ASC || i + len > n) return -1;
      memcpy(asc, p + i, (size_t)len);
      *asc_len = len;
      return 0;
    }
    i += len;
  }
  return -1;
}

/* ---------------------------------------------------------- 样本表解析 -- */

/*
 * 样本表的载荷布局是：version/flags(4) → 表项个数(4) → 表项。
 * 注意个数在 +4 而不是 +8（这里踩过一次坑）。
 */

static int read_stts(const yp_io *io, const ym4a_box *box) {
  unsigned char *p;
  long long size = box_payload(box);
  unsigned int count, i;
  long long seen = 0;
  if (size < 8) return -1;
  p = slurp(io, box->body, size);
  if (!p) return -1;
  count = be32(p + 4);
  if (count == 0 || count > YM4A_MAX_STTS || 8 + (long long)count * 8 > size) {
    free(p);
    return -1;
  }
  g.stts_count = (unsigned int *)malloc(sizeof(unsigned int) * count);
  g.stts_dur = (unsigned int *)malloc(sizeof(unsigned int) * count);
  if (!g.stts_count || !g.stts_dur) {
    free(p);
    return -1;
  }
  for (i = 0; i < count; i++) {
    g.stts_count[i] = be32(p + 8 + i * 8);
    g.stts_dur[i] = be32(p + 8 + i * 8 + 4);
    seen += (long long)g.stts_count[i];
    if (seen > YM4A_MAX_SAMPLES) {
      free(p);
      return -1;
    }
  }
  g.stts_n = (int)count;
  free(p);
  return 0;
}

static int read_stsc(const yp_io *io, const ym4a_box *box) {
  unsigned char *p;
  long long size = box_payload(box);
  unsigned int count, i;
  if (size < 8) return -1;
  p = slurp(io, box->body, size);
  if (!p) return -1;
  count = be32(p + 4);
  if (count == 0 || count > YM4A_MAX_STSC ||
      8 + (long long)count * 12 > size) {
    free(p);
    return -1;
  }
  g.stsc_first = (unsigned int *)malloc(sizeof(unsigned int) * count);
  g.stsc_spc = (unsigned int *)malloc(sizeof(unsigned int) * count);
  if (!g.stsc_first || !g.stsc_spc) {
    free(p);
    return -1;
  }
  for (i = 0; i < count; i++) {
    g.stsc_first[i] = be32(p + 8 + i * 12);
    g.stsc_spc[i] = be32(p + 8 + i * 12 + 4);
  }
  g.stsc_n = (int)count;
  free(p);
  return 0;
}

static int read_stsz(const yp_io *io, const ym4a_box *box) {
  unsigned char *p;
  long long size = box_payload(box);
  unsigned int sample_size, count, i;
  if (size < 12) return -1;
  p = slurp(io, box->body, size);
  if (!p) return -1;
  sample_size = be32(p + 4);
  count = be32(p + 8);
  if (count == 0 || count > YM4A_MAX_SAMPLES) {
    free(p);
    return -1;
  }
  g.sizes = (unsigned int *)malloc(sizeof(unsigned int) * count);
  if (!g.sizes) {
    free(p);
    return -1;
  }
  if (sample_size != 0) {
    for (i = 0; i < count; i++) g.sizes[i] = sample_size;
  } else {
    if (12 + (long long)count * 4 > size) {
      free(p);
      return -1;
    }
    for (i = 0; i < count; i++) g.sizes[i] = be32(p + 12 + i * 4);
  }
  g.sample_n = (int)count;
  free(p);
  return 0;
}

static int read_stco(const yp_io *io, const ym4a_box *box, int wide) {
  unsigned char *p;
  long long size = box_payload(box);
  long long need;
  unsigned int count, i;
  if (size < 8) return -1;
  p = slurp(io, box->body, size);
  if (!p) return -1;
  count = be32(p + 4);
  need = 8 + (long long)count * (wide ? 8 : 4);
  if (count == 0 || count > YM4A_MAX_CHUNKS || need > size) {
    free(p);
    return -1;
  }
  g.chunk_off = (long long *)malloc(sizeof(long long) * count);
  if (!g.chunk_off) {
    free(p);
    return -1;
  }
  for (i = 0; i < count; i++) {
    g.chunk_off[i] = wide ? (long long)be64(p + 8 + i * 8)
                          : (long long)be32(p + 8 + i * 4);
  }
  g.chunk_n = (int)count;
  free(p);
  return 0;
}

/* stsc → 每个 chunk 的首个样本号。按规范，最后一段游程的"每 chunk 样本数"
 * 会继续适用于剩余的 chunk。 */
static int build_chunk_index(void) {
  int c, e = 0;
  if (!g.chunk_n || !g.stsc_n) return -1;
  g.chunk_first = (int *)malloc(sizeof(int) * (g.chunk_n + 1));
  if (!g.chunk_first) return -1;
  g.chunk_first[0] = 0;
  for (c = 0; c < g.chunk_n; c++) {
    while (e + 1 < g.stsc_n && (unsigned int)(c + 1) >= g.stsc_first[e + 1]) e++;
    g.chunk_first[c + 1] = g.chunk_first[c] + (int)g.stsc_spc[e];
  }
  if (g.chunk_first[g.chunk_n] < g.sample_n) {
    yunyin_log("ym4a: chunk runs cover fewer samples than stsz\n");
    return -1;
  }
  /* 最后一个 chunk 出现"游程比样本数还长"是正常的。 */
  g.chunk_first[g.chunk_n] = g.sample_n;
  return 0;
}

/* ------------------------------------------------------------ 编号换算 -- */

static int chunk_of_sample(int idx) {
  int lo = 0, hi = g.chunk_n;
  while (lo + 1 < hi) {
    int mid = (lo + hi) / 2;
    if (g.chunk_first[mid] <= idx)
      lo = mid;
    else
      hi = mid;
  }
  return lo;
}

/* 第 idx 帧的文件偏移。顺序读时游标会往前走，所以整首歌只走一遍，
 * 而不是每取一帧就从头数一遍。 */
static long long offset_of(int idx) {
  int c;
  if (idx < 0 || idx >= g.sample_n) return -1;
  c = chunk_of_sample(idx);
  if (c != g.cc || g.ci < g.chunk_first[c] || g.ci > idx) {
    g.cc = c;
    g.ci = g.chunk_first[c];
    g.co = g.chunk_off[c];
  }
  while (g.ci < idx) {
    g.co += g.sizes[g.ci];
    g.ci++;
  }
  return g.co;
}

/* stts 数的是"媒体时间刻度"，播放器数的是 `rate` 下的 PCM 帧。
 * AAC 通常两者相同；只有当年轨道的 timescale 用的是 SBR 的**核心**采样率时才会差比例。 */
static long long ticks_to_frames(unsigned long long ticks) {
  if (g.timescale <= 0 || g.rate <= 0) return (long long)ticks;
  if (g.timescale == g.rate) return (long long)ticks;
  return (long long)((ticks * (unsigned long long)g.rate +
                      (unsigned long long)g.timescale / 2) /
                     (unsigned long long)g.timescale);
}

static long long frames_before_sample(int idx, int *inside_index) {
  int i, seen = 0;
  unsigned long long ticks = 0;
  for (i = 0; i < g.stts_n; i++) {
    int cnt = (int)g.stts_count[i];
    if (idx < seen + cnt) {
      if (inside_index) *inside_index = i;
      ticks += (unsigned long long)(idx - seen) * g.stts_dur[i];
      return ticks_to_frames(ticks);
    }
    ticks += (unsigned long long)cnt * g.stts_dur[i];
    seen += cnt;
  }
  if (inside_index) *inside_index = g.stts_n > 0 ? g.stts_n - 1 : -1;
  return ticks_to_frames(ticks);
}

static int sample_of_frame(long long frame) {
  int i, seen = 0;
  long long acc = 0;
  if (!g.stts_n) return 0;
  if (frame <= 0) return 0;
  for (i = 0; i < g.stts_n; i++) {
    int cnt = (int)g.stts_count[i];
    long long run = ticks_to_frames((unsigned long long)cnt * g.stts_dur[i]);
    if (frame < acc + run && run > 0) {
      long long per = ticks_to_frames(g.stts_dur[i]);
      int k = per > 0 ? (int)((frame - acc) / per) : 0;
      if (k >= cnt) k = cnt - 1;
      if (k < 0) k = 0;
      return seen + k;
    }
    acc += run;
    seen += cnt;
  }
  return g.sample_n > 0 ? g.sample_n - 1 : 0;
}

/* ------------------------------------------------------------------ 打开 -- */

static void reset_state(void) {
  free(g.stts_count);
  free(g.stts_dur);
  free(g.stsc_first);
  free(g.stsc_spc);
  free(g.sizes);
  free(g.chunk_off);
  free(g.chunk_first);
  /* 只有"由我们打开"的 IO 才由我们关闭（见 ym4a_open_io 的 owns_io）。 */
  if (g.owns_io && g.io && g.io->close) g.io->close(g.io->ctx);
  memset(&g, 0, sizeof(g));
  g.last = -1;
  g.cc = -1;
}

void ym4a_close(void) { reset_state(); }

int ym4a_ready(void) { return g.ready; }

/* 读 mp4a 样本描述（含 esds）与音轨的各个表。
 * 0 = 音轨解析成功，1 = 这不是我们能处理的音轨。 */
static int parse_audio_trak(const yp_io *io, const ym4a_box *trak) {
  ym4a_box mdia, minf, stbl, hdlr, mdhd, stsd, box;
  long long end = trak->start + trak->total;
  long long mdia_end, stbl_end;
  unsigned char *p;
  unsigned int entry_count, i;
  long long entry_off;
  int found_codec = 0;
  int seen_asc = 0;

  if (box_find(io, trak->body, end, "mdia", &mdia) != 0) return 1;
  mdia_end = mdia.start + mdia.total;
  if (box_find(io, mdia.body, mdia_end, "hdlr", &hdlr) != 0) return 1;
  {
    unsigned char b[4];
    if (rd(io, hdlr.body + 8, b, 4) != 0) return 1;
    if (memcmp(b, "soun", 4) != 0) return 1; /* video / text track */
  }
  if (box_find(io, mdia.body, mdia_end, "minf", &minf) != 0) return 1;
  if (box_find(io, minf.body, minf.start + minf.total, "stbl", &stbl) != 0)
    return 1;
  stbl_end = stbl.start + stbl.total;

  if (box_find(io, mdia.body, mdia_end, "mdhd", &mdhd) == 0) {
    unsigned char b[24];
    if (rd(io, mdhd.body, b, sizeof b) == 0) {
      g.timescale = (int)(b[0] == 1 ? be32(b + 20) : be32(b + 12));
    }
  }

  if (box_find(io, stbl.body, stbl_end, "stsd", &stsd) != 0) return 1;
  p = slurp(io, stsd.body, 8);
  if (!p) return 1;
  entry_count = be32(p + 4);
  free(p);
  if (entry_count == 0) return 1;

  entry_off = stsd.body + 8;
  for (i = 0; i < entry_count && i < 8; i++) {
    unsigned char hdrb[8];
    long long esize, entry_end;
    if (rd(io, entry_off, hdrb, 8) != 0) return 1;
    esize = (long long)be32(hdrb);
    if (esize < 8 || entry_off + esize > stsd.start + stsd.total) return 1;
    entry_end = entry_off + esize;
    memcpy(g.codec, hdrb + 4, 4);
    g.codec[4] = 0;
    if (memcmp(hdrb + 4, "mp4a", 4) == 0) {
      unsigned char v[2];
      unsigned char sb[12];
      int version = 0;
      if (rd(io, entry_off + 16, v, 2) == 0) version = (v[0] << 8) | v[1];
      if (rd(io, entry_off + 24, sb, 12) == 0) {
        int ch = (sb[0] << 8) | sb[1];
        int rate = (int)(be32(sb + 8) >> 16);
        if (ch > 0 && ch <= 2) g.ch = ch;
        if (rate > 0) g.rate = rate;
      }
      if (box_find(io, entry_off + 36 + (version == 1 ? 16 : 0), entry_end,
                   "esds", &box) == 0) {
        long long n = box_payload(&box);
        unsigned char *ed = slurp(io, box.body, n);
        if (ed) {
          int asc_len = 0;
          if (parse_esds(ed, (int)n, g.asc, &asc_len) == 0) {
            g.asc_len = asc_len;
            seen_asc = 1;
          }
          free(ed);
        }
      }
      found_codec = 1;
      break;
    }
    entry_off = entry_end;
  }
  if (!found_codec) {
    yunyin_log("ym4a: audio track is not mp4a/AAC\n");
    return 1;
  }

  if (seen_asc) {
    int aot = 0, rate = 0, ch = 0, sbr = 0;
    parse_asc(g.asc, g.asc_len, &aot, &rate, &ch, &sbr);
    if (aot > 0) g.aot = aot;
    if (rate > 0) g.rate = rate;
    if (ch > 0) g.ch = ch;
    if (sbr) g.sbr = 1;
  }
  if (g.aot == 0) g.aot = 2;
  if (g.rate <= 0) g.rate = 44100;
  if (g.ch <= 0) g.ch = 2;
  if (g.timescale <= 0) g.timescale = g.rate;

  if (box_find(io, stbl.body, stbl_end, "stts", &box) != 0) return 1;
  if (read_stts(io, &box) != 0) return 1;
  if (box_find(io, stbl.body, stbl_end, "stsc", &box) != 0) return 1;
  if (read_stsc(io, &box) != 0) return 1;
  if (box_find(io, stbl.body, stbl_end, "stsz", &box) != 0) return 1;
  if (read_stsz(io, &box) != 0) return 1;
  if (box_find(io, stbl.body, stbl_end, "stco", &box) == 0) {
    if (read_stco(io, &box, 0) != 0) return 1;
  } else if (box_find(io, stbl.body, stbl_end, "co64", &box) == 0) {
    if (read_stco(io, &box, 1) != 0) return 1;
  } else {
    return 1;
  }
  return build_chunk_index() == 0 ? 0 : 1;
}

/*
 * 走 yp_io 打开（Phase 1 起的主入口）。
 *   owns_io != 0 时，ym4a_close() 会调用 io->close() 释放它；
 *   否则只借用（网络源由 Phase 2 自己持有）。
 * size() 允许返回负数：长度未知时跳过所有"越界校验"，其余逻辑照旧。
 */
int ym4a_open_io(const yp_io *io, int owns_io) {
  long long end, off, cur;
  ym4a_box moov, box;
  int i;
  int had_audio = 0;

  ym4a_close();
  if (!io || !io->read || !io->seek) return YM4A_ERR_IO;
  g.io = io;
  g.owns_io = owns_io ? 1 : 0;

  off = io->size ? io->size(io->ctx) : -1;
  if (off <= 0) {
    /* 长度未知（网络流）：用"能看到的最后位置"兜底，并放宽越界校验。 */
    g.file_size = 0;
    end = 0x7fffffffffffffffLL;
  } else {
    g.file_size = off;
    end = off;
  }

  if (box_find(io, 0, end, "moov", &moov) != 0) {
    yunyin_log("ym4a: no moov box\n");
    ym4a_close();
    return YM4A_ERR_FORMAT;
  }
  if (box_find(io, moov.start + moov.total, end, "mdat", &box) == 0 ||
      box_find(io, 0, moov.start, "mdat", &box) == 0) {
    g.mdat_body = box.body;
    g.mdat_size = box_payload(&box);
  }

  cur = moov.body;
  for (i = 0; i < 16; i++) {
    int rc = box_find(io, cur, moov.start + moov.total, "trak", &box);
    if (rc != 0) break;
    if (parse_audio_trak(io, &box) == 0) {
      char msg[176];
      g.ready = 1;
      g.cur = 0;
      g.last = -1;
      g.cc = -1;
      snprintf(msg, sizeof msg,
               "ym4a: codec=%s aot=%d rate=%d ch=%d sbr=%d ts=%d samples=%d "
               "dur=%lldms\n",
               g.codec, g.aot, g.rate, g.ch, g.sbr, g.timescale, g.sample_n,
               ym4a_total_frames() * 1000 / (g.rate ? g.rate : 1));
      yunyin_log(msg);
      return YM4A_OK;
    }
    had_audio = 1;
    cur = box.start + box.total;
  }

  ym4a_close();
  return had_audio ? YM4A_ERR_CODEC : YM4A_ERR_NO_AUDIO;
}

/* 本地文件：自己开一份文件 IO，并交给它负责关闭。 */
int ym4a_open(const char *path) {
  static yp_io file_io; /* 单例：同一时刻只开一个 M4A（与播放器一致） */
  if (!path || !*path) return YM4A_ERR_IO;
  if (yp_io_file_open(&file_io, path) != 0) return YM4A_ERR_IO;
  return ym4a_open_io(&file_io, 1);
}

/* -------------------------------------------------------------- 取值接口 -- */

int ym4a_rate(void) { return g.ready ? g.rate : 0; }
int ym4a_channels(void) { return g.ready ? g.ch : 0; }
int ym4a_object_type(void) { return g.aot; }
int ym4a_sbr(void) { return g.sbr; }
int ym4a_sample_count(void) { return g.sample_n; }
int ym4a_cur_sample(void) { return g.last; }
const char *ym4a_codec_name(void) { return g.codec; }

long long ym4a_total_frames(void) {
  int i;
  unsigned long long ticks = 0;
  if (!g.ready) return 0;
  for (i = 0; i < g.stts_n; i++) {
    ticks += (unsigned long long)g.stts_count[i] * g.stts_dur[i];
  }
  return ticks_to_frames(ticks);
}

long long ym4a_sample_start_frame(int idx) {
  if (!g.ready || idx <= 0) return 0;
  if (idx > g.sample_n) idx = g.sample_n;
  return frames_before_sample(idx, NULL);
}

int ym4a_sample_frames(int idx) {
  int inside = -1;
  if (!g.ready || idx < 0) return 0;
  frames_before_sample(idx, &inside);
  if (inside < 0 || inside >= g.stts_n) return 0;
  return (int)ticks_to_frames(g.stts_dur[inside]);
}

int ym4a_asc(unsigned char *dst, int cap) {
  if (!g.ready || !dst || cap <= 0 || g.asc_len <= 0 || g.asc_len > cap)
    return 0;
  memcpy(dst, g.asc, (size_t)g.asc_len);
  return g.asc_len;
}

int ym4a_next_sample(unsigned char *dst, int cap) {
  int idx;
  unsigned int n;
  long long off;
  if (!g.ready || !dst || cap <= 0) return -1;
  if (g.cur >= g.sample_n) return 0;
  idx = g.cur;
  n = g.sizes[idx];
  if ((int)n > cap) {
    yunyin_log("ym4a: access unit larger than the ES buffer\n");
    return -1;
  }
  off = offset_of(idx);
  /* 长度未知（网络流）时 file_size 为 0，此时只做最基本的下界校验。 */
  if (off < 0) return -1;
  if (g.file_size > 0 && off + (long long)n > g.file_size) return -1;
  if (rd(g.io, off, dst, (long long)n) != 0) return -1;
  g.last = idx;
  g.cur = idx + 1;
  return (int)n;
}

int ym4a_seek_frame(long long frame) {
  if (!g.ready) return -1;
  if (frame < 0) frame = 0;
  g.cur = sample_of_frame(frame);
  g.last = -1;
  g.cc = -1;
  g.ci = 0;
  g.co = 0;
  return 0;
}
