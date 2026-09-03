#ifndef YUNYIN_YPLAYER_H
#define YUNYIN_YPLAYER_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Streaming audio player (pull-style, like ElevenMPV):
 *   - open(path) detects the format and initializes the decoder
 *   - decode(buf, max_frames) returns how many frames were produced
 *   - position()/length() are in source frames (rate = get_rate())
 *   - cover() returns a pointer to embedded JPEG/PNG bytes (0 if none)
 *
 * Formats: MP3 (mpg123), OGG (vorbisfile), WAV (dr_wav),
 *          FLAC (dr_flac), OPUS (opusfile).
 */
int  yp_open(const char *path);
int  yp_rate(void);
int  yp_channels(void);
int  yp_decode(short *buf, int max_frames);
long long yp_position(void);
long long yp_length(void);
void yp_close(void);
const unsigned char *yp_cover(int *len);

#ifdef __cplusplus
}
#endif

#endif
