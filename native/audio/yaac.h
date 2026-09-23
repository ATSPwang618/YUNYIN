#ifndef YUNYIN_YAAC_H
#define YUNYIN_YAAC_H

#ifdef __cplusplus
extern "C" {
#endif

/*
 * AAC decoding on the Vita's own hardware decoder (SceAudiodec).
 *
 * The container work lives in `ym4a.c`; this is the part that actually turns
 * the raw AAC access units inside `mdat` into PCM.  Public API only —
 * sceAudiodecInitLibrary / CreateDecoder / Decode / DeleteDecoder — no private
 * `*Internal` imports, no FFmpeg.
 *
 * Off the Vita (`__vita__` undefined) every function degrades to a stub, so the
 * sources still compile in a host test build.
 */

#define YAAC_ERR_INIT     -1
#define YAAC_ERR_MEMORY   -2
#define YAAC_ERR_DECODE   -3
#define YAAC_ERR_TOO_BIG  -4
#define YAAC_ERR_STATE    -5

/* Largest raw access unit accepted (the hardware's documented AAC limit is
 * 1536 bytes; the extra room just avoids tripping on a fat frame). */
#define YAAC_ES_CAP 4096

/*
 * Start a decoder for one track.
 *   channels 1..2, rate in Hz, is_adts 0 for M4A/MP4 (raw frames),
 *   is_sbr hints that the stream may be SBR (the header cannot tell us).
 * Returns 0 on success, negative on failure.
 */
int yaac_open(int channels, int rate, int is_adts, int is_sbr);

/*
 * Decode one access unit.  Writes interleaved 16-bit PCM into `out` and returns
 * the number of frames per channel, 0 when the decoder produced nothing, or a
 * negative YAAC_ERR_* code.  `out_cap_frames` is the capacity of `out`.
 */
int yaac_decode(const unsigned char *au, int len, short *out, int out_cap_frames);

void yaac_close(void);
int yaac_ready(void);
int yaac_channels(void);
int yaac_rate(void);
/* Last sceAudiodec return value, or 0 — surfaced in the debug log. */
int yaac_last_status(void);

#ifdef __cplusplus
}
#endif

#endif /* YUNYIN_YAAC_H */
