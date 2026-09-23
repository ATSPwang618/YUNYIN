#ifndef YUNYIN_LOG_H
#define YUNYIN_LOG_H

/*
 * Opt-in native log.  Nothing is written unless an empty file named `debug`
 * exists in ux0:/data/yunyin — the shipped build stays silent, and a card with
 * that file gets a readable ux0:/data/yunyin.log.
 *
 * Shared by the whole C side (host/yunyin_listdir.c, host/yunyin_image.c,
 * audio/ym4a.c, audio/yaac.c) — there is exactly one implementation.  Outside
 * the Vita toolchain (`__vita__`) the helpers compile to no-ops so the same
 * sources can be built and exercised on a PC.
 */

#if defined(__GNUC__)
#define YUNYIN_UNUSED __attribute__((unused))
#else
#define YUNYIN_UNUSED
#endif

#ifdef __vita__

#include <psp2/io/fcntl.h>
#include <string.h>

#define YUNYIN_LOG_FLAG "ux0:data/yunyin/debug"
#define YUNYIN_LOG_PATH "ux0:data/yunyin.log"

static YUNYIN_UNUSED int yunyin_log_enabled(void) {
  SceUID flag = sceIoOpen(YUNYIN_LOG_FLAG, SCE_O_RDONLY, 0);
  if (flag >= 0) {
    sceIoClose(flag);
    return 1;
  }
  return 0;
}

static YUNYIN_UNUSED void yunyin_log(const char *msg) {
  if (!msg || !yunyin_log_enabled()) {
    return;
  }
  SceUID f = sceIoOpen(YUNYIN_LOG_PATH, SCE_O_WRONLY | SCE_O_CREAT, 0777);
  if (f >= 0) {
    sceIoLseek(f, 0, SCE_SEEK_END);
    sceIoWrite(f, msg, strlen(msg));
    sceIoClose(f);
  }
}

#else /* host build: no device, but keep the diagnostics on stderr */

#include <stdio.h>

static YUNYIN_UNUSED int yunyin_log_enabled(void) { return 1; }
static YUNYIN_UNUSED void yunyin_log(const char *msg) { fputs(msg, stderr); }

#endif

#endif /* YUNYIN_LOG_H */
