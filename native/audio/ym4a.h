#ifndef YUNYIN_YM4A_H
#define YUNYIN_YM4A_H

#ifdef __cplusplus
extern "C" {
#endif

/*
 * M4A / MP4 audio demuxer.
 *
 * M4A is the box ("container"): AAC is the actual audio inside `mdat`.  This
 * module is only the mover — it walks ftyp/moov/trak/mdia/minf/stbl, picks the
 * `soun` track, reads the AAC parameters out of `esds` and rebuilds the sample
 * table (stts/stsc/stsz/stco) so the caller can pull one raw AAC access unit at
 * a time without ever loading `mdat` into memory.
 *
 * Deliberately knows nothing about decoding and nothing about the Vita: the
 * hardware decoder lives in `yaac.c`, so this file also compiles and runs on a
 * PC (plain gcc, no VitaSDK) for parser testing.
 *
 * Usage:
 *   if (ym4a_open(path) != 0) fail;
 *   while ((n = ym4a_next_sample(au, sizeof au)) > 0) decode(au, n);
 *   ym4a_close();
 */

/* ym4a_open result codes. */
#define YM4A_OK            0
#define YM4A_ERR_IO       -1  /* missing / unreadable / truncated */
#define YM4A_ERR_FORMAT   -2  /* not an MP4 container (no moov / no stbl) */
#define YM4A_ERR_NO_AUDIO -3  /* no `soun` track */
#define YM4A_ERR_CODEC    -4  /* audio track is not AAC */
#define YM4A_ERR_MEMORY   -5  /* sample table did not fit */
#define YM4A_ERR_TABLE    -6  /* sample table is inconsistent */

int  ym4a_open(const char *path);        /* 0 (YM4A_OK) on success */
void ym4a_close(void);
int  ym4a_ready(void);

int  ym4a_rate(void);                    /* decoder output rate (Hz) */
int  ym4a_channels(void);                /* 1 or 2 */
int  ym4a_object_type(void);             /* AAC AudioObjectType (2 = LC) */
int  ym4a_sbr(void);                     /* 1 when the config signals SBR */

long long ym4a_total_frames(void);       /* PCM frames per channel, whole track */
int  ym4a_sample_count(void);            /* AAC access units in `mdat` */
int  ym4a_cur_sample(void);              /* index of the last sample read, -1 before */

/* PCM frame index where access unit `idx` starts. */
long long ym4a_sample_start_frame(int idx);
/* PCM frames an access unit is declared to contain (media timescale). */
int  ym4a_sample_frames(int idx);

/*
 * Read the next access unit (raw AAC, no ADTS header) into `dst`.
 * Returns the byte count, 0 at end of stream, negative on error.
 */
int  ym4a_next_sample(unsigned char *dst, int cap);

/* Move the read cursor to the access unit that contains PCM frame `frame`. */
int  ym4a_seek_frame(long long frame);

/*
 * AudioSpecificConfig (the decoder needs its rate/channels/SBR, which are NOT
 * in the raw access units).  Copies up to `cap` bytes; returns the length.
 */
int  ym4a_asc(unsigned char *dst, int cap);
const char *ym4a_codec_name(void);

#ifdef __cplusplus
}
#endif

#endif /* YUNYIN_YM4A_H */
