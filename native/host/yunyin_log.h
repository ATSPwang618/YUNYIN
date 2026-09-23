#ifndef YUNYIN_LOG_H
#define YUNYIN_LOG_H

/*
 * 可选开启的原生日志：卡里 `ux0:/data/yunyin/` 下存在空文件 `debug` 时才写，
 * 正式版默认完全静默；有那个文件的卡会得到可读的 ux0:/data/yunyin.log。
 *
 * 整个 C 侧共用这一份实现（host/yunyin_listdir.c、host/yunyin_image.c、
 * audio/ym4a.c、audio/yaac.c）。不在 Vita 工具链里（未定义 `__vita__`）时，
 * 这两个函数编译成空操作，好让同样的源码在电脑上也能编、能跑。
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
