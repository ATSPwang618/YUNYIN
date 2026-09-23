/*
 * Hardware AAC decoding for YUNYIN (SceAudiodec).
 *
 * Contract notes that cost real debugging time upstream (see the reference
 * implementations — wiliwili's `vitadec_audio.c` and
 * `vita-hw-decoder/src/internal/vita_aac_decoder.c`):
 *
 *  - MP4/M4A hands us *raw* AAC access units, so `isAdts = 0`; the decoder
 *    does NOT read a header, it needs channels + rate from the container
 *    (that is what `ym4a.c` parses out of `esds`).
 *  - The ES and PCM buffers must be 0x100-aligned *and* come from a 4 KiB
 *    aligned block, otherwise sceAudiodecCreateDecoder fails with a bare
 *    error.  Uncached memory is used, as the hardware block DMAs into them.
 *  - `isSbr` cannot be derived from the access unit; the public references
 *    always pass 1.  With plain AAC-LC the decoder still emits 1024 frames
 *    per unit, so the value is a capability hint, not a forced upsample.
 *  - Output size is whatever the decoder reports (outputPcmSize); we never
 *    assume 1024 samples per frame.
 */

#include "yaac.h"
#include "host/yunyin_log.h"

#include <stdio.h>
#include <string.h>

#ifdef __vita__

#include <psp2/audiodec.h>
#include <psp2/kernel/sysmem.h>
#include <psp2/sysmodule.h>

static struct {
  int ready;
  int library_open;
  int module_open;
  int ch;
  int rate;
  int pcm_bytes;
  int last_status;
  SceUID es_uid;
  SceUID pcm_uid;
  unsigned char *es;
  short *pcm;
  SceAudiodecCtrl ctrl;
  SceAudiodecInfo info;
} a = {.es_uid = -1, .pcm_uid = -1};

/* Round up to what sceKernelAllocMemBlock accepts (4 KiB) while keeping the
 * sizes handed to the decoder logical. */
static void *alloc_uncached(const char *name, int size, SceUID *uid) {
  int block = (SCE_AUDIODEC_ROUND_UP(size) + 0xFFF) & ~0xFFF;
  void *base = NULL;
  SceUID mem = sceKernelAllocMemBlock(name, SCE_KERNEL_MEMBLOCK_TYPE_USER_RW_UNCACHE,
                                      block, NULL);
  if (mem < 0) return NULL;
  if (sceKernelGetMemBlockBase(mem, &base) < 0 || !base) {
    sceKernelFreeMemBlock(mem);
    return NULL;
  }
  memset(base, 0, (size_t)block);
  *uid = mem;
  return base;
}

static void free_es(void) {
  if (a.es_uid >= 0) {
    sceKernelFreeMemBlock(a.es_uid);
    a.es_uid = -1;
  }
  a.es = NULL;
}

static void free_pcm(void) {
  if (a.pcm_uid >= 0) {
    sceKernelFreeMemBlock(a.pcm_uid);
    a.pcm_uid = -1;
  }
  a.pcm = NULL;
}

void yaac_close(void) {
  if (a.ready) {
    sceAudiodecDeleteDecoder(&a.ctrl);
    a.ready = 0;
  }
  free_es();
  free_pcm();
  if (a.library_open) {
    sceAudiodecTermLibrary(SCE_AUDIODEC_TYPE_AAC);
    a.library_open = 0;
  }
  if (a.module_open) {
    sceSysmoduleUnloadModule(SCE_SYSMODULE_AVCDEC);
    a.module_open = 0;
  }
  a.ch = 0;
  a.rate = 0;
  a.pcm_bytes = 0;
  a.last_status = 0;
}

int yaac_open(int channels, int rate, int is_adts, int is_sbr) {
  SceAudiodecInitParam init;
  int ret;
  int es_size;
  char msg[128];

  yaac_close();
  if (channels < 1 || channels > 2 || rate <= 0) return YAAC_ERR_STATE;
  a.es_uid = -1;
  a.pcm_uid = -1;
  a.ch = channels;
  a.rate = rate;

  /* Best effort: the module is usually already up because the player itself
   * was started through the same system codec path.  Its failure is not fatal,
   * sceAudiodecInitLibrary below is the real judge. */
  ret = sceSysmoduleLoadModule(SCE_SYSMODULE_AVCDEC);
  if (ret >= 0) a.module_open = 1;
  snprintf(msg, sizeof msg, "yaac: sceSysmoduleLoadModule(AVCDEC) -> 0x%08X\n",
           (unsigned)ret);
  yunyin_log(msg);

  memset(&init, 0, sizeof init);
  init.aac.size = sizeof init.aac;
  init.aac.totalStreams = 1;
  ret = sceAudiodecInitLibrary(SCE_AUDIODEC_TYPE_AAC, &init);
  if (ret < 0) {
    a.last_status = ret;
    snprintf(msg, sizeof msg, "yaac: sceAudiodecInitLibrary -> 0x%08X\n",
             (unsigned)ret);
    yunyin_log(msg);
    yaac_close();
    return YAAC_ERR_INIT;
  }
  a.library_open = 1;

  es_size = SCE_AUDIODEC_ROUND_UP(YAAC_ES_CAP);
  a.es = (unsigned char *)alloc_uncached("YunyinAacEs", es_size, &a.es_uid);
  if (!a.es) {
    yunyin_log("yaac: ES buffer allocation failed\n");
    yaac_close();
    return YAAC_ERR_MEMORY;
  }
  a.pcm_bytes = SCE_AUDIODEC_ROUND_UP(channels * SCE_AUDIODEC_AAC_MAX_SAMPLES *
                                      (int)sizeof(short));
  a.pcm = (short *)alloc_uncached("YunyinAacPcm", a.pcm_bytes, &a.pcm_uid);
  if (!a.pcm) {
    yunyin_log("yaac: PCM buffer allocation failed\n");
    yaac_close();
    return YAAC_ERR_MEMORY;
  }

  memset(&a.info, 0, sizeof a.info);
  a.info.aac.size = sizeof a.info.aac;
  a.info.aac.isAdts = is_adts ? 1 : 0;
  a.info.aac.ch = (SceUInt32)channels;
  a.info.aac.samplingRate = (SceUInt32)rate;
  a.info.aac.isSbr = is_sbr ? 1 : 0;

  memset(&a.ctrl, 0, sizeof a.ctrl);
  a.ctrl.size = sizeof a.ctrl;
  a.ctrl.pEs = a.es;
  a.ctrl.maxEsSize = (SceUInt32)es_size;
  a.ctrl.pPcm = a.pcm;
  a.ctrl.maxPcmSize = (SceUInt32)a.pcm_bytes;
  a.ctrl.wordLength = SCE_AUDIODEC_WORD_LENGTH_16BITS;
  a.ctrl.pInfo = &a.info;

  ret = sceAudiodecCreateDecoder(&a.ctrl, SCE_AUDIODEC_TYPE_AAC);
  if (ret < 0) {
    a.last_status = ret;
    snprintf(msg, sizeof msg, "yaac: sceAudiodecCreateDecoder -> 0x%08X\n",
             (unsigned)ret);
    yunyin_log(msg);
    yaac_close();
    return YAAC_ERR_INIT;
  }
  a.ready = 1;
  snprintf(msg, sizeof msg,
           "yaac: decoder ready ch=%d rate=%d adts=%d sbr=%d es=%d pcm=%d\n",
           channels, rate, is_adts, is_sbr, es_size, a.pcm_bytes);
  yunyin_log(msg);
  return 0;
}

int yaac_decode(const unsigned char *au, int len, short *out, int out_cap_frames) {
  int ret;
  int frames;
  if (!a.ready) return YAAC_ERR_STATE;
  if (!au || len <= 0 || !out || out_cap_frames <= 0) return YAAC_ERR_STATE;
  if (len > (int)a.ctrl.maxEsSize) {
    yunyin_log("yaac: access unit exceeds the ES buffer\n");
    return YAAC_ERR_TOO_BIG;
  }
  memcpy(a.es, au, (size_t)len);
  a.ctrl.inputEsSize = (SceUInt32)len;
  a.ctrl.outputPcmSize = 0;

  ret = sceAudiodecDecode(&a.ctrl);
  a.last_status = ret;
  if (ret < 0) {
    char msg[96];
    snprintf(msg, sizeof msg, "yaac: sceAudiodecDecode -> 0x%08X\n",
             (unsigned)ret);
    yunyin_log(msg);
    return YAAC_ERR_DECODE;
  }
  if (a.ctrl.outputPcmSize == 0) return 0;

  frames = (int)(a.ctrl.outputPcmSize / (SceUInt32)(a.ch * (int)sizeof(short)));
  if (frames > out_cap_frames) frames = out_cap_frames;
  memcpy(out, a.pcm, (size_t)frames * (size_t)a.ch * sizeof(short));
  return frames;
}

int yaac_ready(void) { return a.ready; }
int yaac_channels(void) { return a.ch; }
int yaac_rate(void) { return a.rate; }
int yaac_last_status(void) { return a.last_status; }

#else /* host build: no hardware decoder, stubs keep the sources linkable */

void yaac_close(void) {}
int yaac_open(int channels, int rate, int is_adts, int is_sbr) {
  (void)channels;
  (void)rate;
  (void)is_adts;
  (void)is_sbr;
  return YAAC_ERR_INIT;
}
int yaac_decode(const unsigned char *au, int len, short *out,
                int out_cap_frames) {
  (void)au;
  (void)len;
  (void)out;
  (void)out_cap_frames;
  return YAAC_ERR_STATE;
}
int yaac_ready(void) { return 0; }
int yaac_channels(void) { return 0; }
int yaac_rate(void) { return 0; }
int yaac_last_status(void) { return 0; }

#endif
