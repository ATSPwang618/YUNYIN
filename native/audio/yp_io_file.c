/*
 * yp_io 的"本地文件"实现（任务书 §32/§16）。
 *
 * 就是一层 stdio 包装 —— 和 Phase 1 之前六个解码器各自 fopen 的行为等价，
 * 只是把"谁来读"收拢到同一处，好让 Phase 2 的网络源用同样的接口插进来。
 */

#include "yp_io.h"
#include "host/yunyin_log.h"

#include <stdio.h>
#include <stdlib.h>

typedef struct {
    FILE *f;
    long long size;
} yp_file_ctx;

static long long yp_file_read(void *ctx, void *dst, unsigned long long n) {
    yp_file_ctx *c = (yp_file_ctx *)ctx;
    size_t got;
    if (!c || !c->f) return -1;
    if (n == 0) return 0;
    got = fread(dst, 1, (size_t)n, c->f);
    return (long long)got; /* 0 = 读到结尾 */
}

static long long yp_file_seek(void *ctx, long long off, int whence) {
    yp_file_ctx *c = (yp_file_ctx *)ctx;
    int w = SEEK_SET;
    if (!c || !c->f) return -1;
    if (whence == SEEK_CUR) w = SEEK_CUR;
    else if (whence == SEEK_END) w = SEEK_END;
    if (fseeko(c->f, (off_t)off, w) != 0) return -1;
    return (long long)ftello(c->f);
}

static long long yp_file_tell(void *ctx) {
    yp_file_ctx *c = (yp_file_ctx *)ctx;
    if (!c || !c->f) return -1;
    return (long long)ftello(c->f);
}

static long long yp_file_size(void *ctx) {
    yp_file_ctx *c = (yp_file_ctx *)ctx;
    if (!c) return -1;
    return c->size;
}

static int yp_file_close(void *ctx) {
    yp_file_ctx *c = (yp_file_ctx *)ctx;
    if (!c) return -1;
    if (c->f) fclose(c->f);
    free(c);
    return 0;
}

int yp_io_file_open(yp_io *io, const char *path) {
    yp_file_ctx *c;
    long long size;
    FILE *f;

    if (!io || !path || !*path) return -1;
    f = fopen(path, "rb");
    if (!f) {
        yunyin_log("yp_io: fopen failed\n");
        return -1;
    }
    if (fseeko(f, 0, SEEK_END) != 0) {
        fclose(f);
        return -1;
    }
    size = (long long)ftello(f);
    if (fseeko(f, 0, SEEK_SET) != 0) {
        fclose(f);
        return -1;
    }
    c = (yp_file_ctx *)malloc(sizeof *c);
    if (!c) {
        fclose(f);
        return -1;
    }
    c->f = f;
    c->size = size;

    io->ctx = c;
    io->read = yp_file_read;
    io->seek = yp_file_seek;
    io->tell = yp_file_tell;
    io->size = yp_file_size;
    io->close = yp_file_close;
    return 0;
}
