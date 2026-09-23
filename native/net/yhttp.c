/*
 * Phase 0 network probe — see yhttp.h for the contract.
 *
 * Written against the VitaSDK headers only (psp2/net/{net,netctl,http}.h,
 * psp2/libssl.h, psp2/sysmodule.h); no other platform's API appears here, as
 * §23 requires.  Every error path records both the calling stage and the raw
 * Vita error code, because on hardware the difference between "DNS failed",
 * "TLS failed" and "aborted" is the whole point of the exercise.
 */

#include "yhttp.h"
#include "host/yunyin_log.h"

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>

#ifdef __vita__

#include <psp2/kernel/threadmgr.h>
#include <psp2/libssl.h>
#include <psp2/net/http.h>
#include <psp2/net/net.h>
#include <psp2/net/netctl.h>
#include <psp2/sysmodule.h>

#define YHTTP_USER_AGENT \
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 " \
    "(KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"

/* Timeouts in microseconds; Phase 0 wants failures to be *legible*, so these
 * are short enough to fit several attempts in one boot. */
#define YHTTP_RESOLVE_TIMEOUT_US (5 * 1000 * 1000)
#define YHTTP_CONNECT_TIMEOUT_US (10 * 1000 * 1000)
#define YHTTP_RECV_TIMEOUT_US    (10 * 1000 * 1000)
#define YHTTP_HEADER_MAX         (16 * 1024)

static yhttp_log_fn g_log;
static int g_inited;
static int g_ssl_inited;
static int g_http_inited;
static int g_ca_loaded;
static void *g_net_pool;
static unsigned int g_ssl_pool = YHTTP_SSL_POOL;
static unsigned int g_http_pool = YHTTP_HTTP_POOL;
static int g_verify_flags;

/* ------------------------------------------------------------------ log -- */

static void yh_logf(const char *fmt, ...) {
    char line[256];
    va_list ap;
    int n;
    if (!g_log) return;
    va_start(ap, fmt);
    n = vsnprintf(line, sizeof line, fmt, ap);
    va_end(ap);
    if (n < 0) return;
    if (n >= (int)sizeof line) n = (int)sizeof line - 1;
    g_log(line, (unsigned int)n);
}

void yhttp_set_log(yhttp_log_fn fn) { g_log = fn; }

/* ----------------------------------------------------------- lifecycle -- */

int yhttp_init(void) {
    SceNetInitParam param;
    int ret;

    if (g_inited) return 0;

    ret = sceSysmoduleLoadModule(SCE_SYSMODULE_NET);
    yh_logf("yhttp: sysmodule NET -> 0x%08X\n", (unsigned)ret);

    /* SceNetInit keeps this block for its whole lifetime, so it must outlive
     * every request (and cannot be a stack buffer). */
    g_net_pool = malloc(YHTTP_NET_POOL);
    if (!g_net_pool) {
        yh_logf("yhttp: net pool malloc(%d) failed\n", YHTTP_NET_POOL);
        return -1;
    }
    param.memory = g_net_pool;
    param.size = YHTTP_NET_POOL;
    param.flags = 0;
    ret = sceNetInit(&param);
    yh_logf("yhttp: sceNetInit(%d) -> 0x%08X\n", YHTTP_NET_POOL, (unsigned)ret);
    if (ret < 0) return ret;

    ret = sceNetCtlInit();
    yh_logf("yhttp: sceNetCtlInit -> 0x%08X\n", (unsigned)ret);

    ret = sceSysmoduleLoadModule(SCE_SYSMODULE_SSL);
    yh_logf("yhttp: sysmodule SSL -> 0x%08X\n", (unsigned)ret);
    ret = sceSslInit(YHTTP_SSL_POOL);
    yh_logf("yhttp: sceSslInit(%d) -> 0x%08X\n", YHTTP_SSL_POOL, (unsigned)ret);
    if (ret >= 0) g_ssl_inited = 1;

    ret = sceSysmoduleLoadModule(SCE_SYSMODULE_HTTPS);
    yh_logf("yhttp: sysmodule HTTPS -> 0x%08X\n", (unsigned)ret);
    ret = sceHttpInit(YHTTP_HTTP_POOL);
    yh_logf("yhttp: sceHttpInit(%d) -> 0x%08X\n", YHTTP_HTTP_POOL, (unsigned)ret);
    if (ret < 0) return ret;
    g_http_inited = 1;

    g_inited = 1;
    return 0;
}

void yhttp_term(void) {
    if (g_http_inited) sceHttpTerm();
    if (g_ssl_inited) sceSslTerm();
    sceSysmoduleUnloadModule(SCE_SYSMODULE_HTTPS);
    sceSysmoduleUnloadModule(SCE_SYSMODULE_SSL);
    sceNetCtlTerm();
    sceNetTerm();
    sceSysmoduleUnloadModule(SCE_SYSMODULE_NET);
    free(g_net_pool);
    g_net_pool = NULL;
    g_http_inited = g_ssl_inited = g_inited = 0;
}

int yhttp_online(void) {
    SceNetCtlInfo info;
    int state = 0;
    int ret = sceNetCtlInetGetState(&state);
    if (ret < 0) {
        yh_logf("yhttp: sceNetCtlInetGetState -> 0x%08X\n", (unsigned)ret);
        return 0;
    }
    yh_logf("yhttp: netctl state=%d (3=connected)\n", state);
    if (state != SCE_NETCTL_STATE_CONNECTED) return 0;
    memset(&info, 0, sizeof info);
    if (sceNetCtlInetGetInfo(SCE_NETCTL_INFO_GET_IP_ADDRESS, &info) >= 0) {
        yh_logf("yhttp: ip=%s\n", info.ip_address);
    }
    return 1;
}

int yhttp_memory(unsigned int *pool, unsigned int *in_use, unsigned int *peak) {
    SceHttpMemoryPoolStats st;
    SceSslMemoryPoolStats ssl;
    int ret = sceHttpGetMemoryPoolStats(&st);
    if (ret < 0) {
        yh_logf("yhttp: sceHttpGetMemoryPoolStats -> 0x%08X\n", (unsigned)ret);
        return ret;
    }
    if (pool) *pool = st.poolSize;
    if (in_use) *in_use = st.currentInuseSize;
    if (peak) *peak = st.maxInuseSize;
    /* §27: the console libraries keep their own pools, so the honest memory
     * figure is "what did we hand out, and how much did they actually use". */
    yh_logf("yhttp: http pool %u used=%u peak=%u\n", st.poolSize,
            st.currentInuseSize, st.maxInuseSize);
    memset(&ssl, 0, sizeof ssl);
    if (sceSslGetMemoryPoolStats(&ssl) >= 0) {
        yh_logf("yhttp: ssl pool %u used=%u peak=%u\n", ssl.poolSize,
                ssl.currentInuseSize, ssl.maxInuseSize);
    }
    return 0;
}

/*
 * Certificate validation against the console's own CA store (§23).
 *
 * A 256 KiB SceHttp pool is not enough for the 47 root certificates (measured
 * on hardware: sceHttpsLoadCert -> 0x80431022 OUT_OF_MEMORY), so the pool pair
 * is grown until the store fits.  The result is reported and remembered.
 */
static int yh_try_load_ca(unsigned int ssl_pool, unsigned int http_pool) {
    SceHttpsCaList list;
    int ret;

    if (g_http_inited) sceHttpTerm();
    if (g_ssl_inited) sceSslTerm();
    ret = sceSslInit(ssl_pool);
    if (ret < 0) {
        yh_logf("yhttp: sceSslInit(%u) -> 0x%08X\n", ssl_pool, (unsigned)ret);
        g_ssl_inited = 0;
        return ret;
    }
    g_ssl_inited = 1;
    ret = sceHttpInit(http_pool);
    if (ret < 0) {
        yh_logf("yhttp: sceHttpInit(%u) -> 0x%08X\n", http_pool, (unsigned)ret);
        g_http_inited = 0;
        return ret;
    }
    g_http_inited = 1;

    memset(&list, 0, sizeof list);
    ret = sceHttpsGetCaList(&list);
    if (ret < 0) {
        yh_logf("yhttp: sceHttpsGetCaList -> 0x%08X\n", (unsigned)ret);
        return ret;
    }
    ret = sceHttpsLoadCert(list.caNum, (const SceHttpsData **)list.caCerts,
                           NULL, NULL);
    yh_logf("yhttp: sceHttpsLoadCert(%d) with ssl=%u http=%u -> 0x%08X\n",
            list.caNum, ssl_pool, http_pool, (unsigned)ret);
    sceHttpsFreeCaList(&list);
    if (ret >= 0) {
        g_ssl_pool = ssl_pool;
        g_http_pool = http_pool;
        return 0;
    }
    return ret;
}

int yhttp_load_ca(void) {
    static const unsigned int ladder[][2] = {
        {256 * 1024, 256 * 1024}, /* the default pair; known too small */
        {512 * 1024, 512 * 1024},
        {1024 * 1024, 1024 * 1024},
        {1024 * 1024, 2048 * 1024},
    };
    unsigned int i;
    if (g_ca_loaded) return 0;
    for (i = 0; i < sizeof ladder / sizeof ladder[0]; i++) {
        if (yh_try_load_ca(ladder[i][0], ladder[i][1]) == 0) {
            g_ca_loaded = 1;
            yh_logf("yhttp: CA store loaded with ssl=%u http=%u\n",
                    g_ssl_pool, g_http_pool);
            return 0;
        }
    }
    yh_logf("yhttp: CA store could not be loaded with any pool pair\n");
    return -1;
}

unsigned int yhttp_ca_http_pool(void) { return g_http_pool; }
unsigned int yhttp_ca_ssl_pool(void) { return g_ssl_pool; }

/*
 * Turning verification on.
 *
 * Two mechanisms exist and they are not the same thing:
 *
 *   sceHttpsEnableOption(SCE_HTTPS_FLAG_*)  switches the *checks* on
 *       (server verify, CN, validity window, known-CA).  No id argument: it is
 *       a process-wide setting.
 *   sceHttpsLoadCert()                      registers *additional* roots.
 *
 * The CA store could not be loaded on hardware (OUT_OF_MEMORY at every pool
 * size), which does not by itself mean verification is off — the firmware may
 * verify against its own store once the flags are enabled.  So: enable the
 * flags first and let the self-signed-certificate target decide the truth.
 */
static int yh_enable_verify_flags(void) {
    unsigned int flags = SCE_HTTPS_FLAG_SERVER_VERIFY |
                         SCE_HTTPS_FLAG_CN_CHECK |
                         SCE_HTTPS_FLAG_NOT_AFTER_CHECK |
                         SCE_HTTPS_FLAG_NOT_BEFORE_CHECK |
                         SCE_HTTPS_FLAG_KNOWN_CA_CHECK;
    int ret = sceHttpsEnableOption(flags);
    yh_logf("yhttp: sceHttpsEnableOption(0x%02X) -> 0x%08X\n", flags,
            (unsigned)ret);
    return ret;
}

static int yh_load_system_ca(void) {
    /* Flags are what actually switch verification on; the CA load is a bonus
     * that this firmware refuses, so its failure must not abort verify mode. */
    g_verify_flags = yh_enable_verify_flags();
    (void)yhttp_load_ca();
    return 0;
}

/* -------------------------------------------------------------- headers -- */

/*
 * Header block handling.
 *
 * `sceHttpGetAllResponseHeaders` hands back a pointer into the library's own
 * pool plus a byte count — it is NOT a caller-owned C string:
 *
 *   - it must never be free()d (that corrupts the SceHttp pool; doing so is what
 *     crashed the first on-device probe run, inside _svfprintf_r), and
 *   - it is not safe to treat as NUL-terminated: printing "%.150s" from a value
 *     inside it walks past the end of the block.
 *
 * So every access here is bounded by `size`, and values are returned as
 * pointer+length pairs rather than strings.
 */
typedef struct {
    const char *ptr;
    int len;
} yh_hdr;

static yh_hdr yh_header_find(const char *headers, unsigned int size,
                             const char *name) {
    size_t name_len = strlen(name);
    unsigned int i = 0;
    yh_hdr none;
    none.ptr = NULL;
    none.len = 0;
    while (i < size) {
        unsigned int line_start = i;
        unsigned int line_end = i;
        unsigned int len;
        while (line_end < size && headers[line_end] != '\n' &&
               headers[line_end] != '\r')
            line_end++;
        len = line_end - line_start;
        if (len > name_len && headers[line_start + name_len] == ':' &&
            strncasecmp(headers + line_start, name, name_len) == 0) {
            unsigned int v = line_start + (unsigned int)name_len + 1;
            while (v < line_end && (headers[v] == ' ' || headers[v] == '\t')) v++;
            none.ptr = headers + v;
            none.len = (int)(line_end - v);
            return none;
        }
        if (line_end == i) break; /* no progress: malformed block */
        i = line_end;
        while (i < size && (headers[i] == '\r' || headers[i] == '\n')) i++;
    }
    return none;
}

static void yh_parse_content_range(const yh_hdr *h, yhttp_result *res) {
    /* Content-Range: bytes 0-65535/12345678   (or "/" "*" for unknown) */
    unsigned long long start = 0, end = 0, total = 0;
    char tmp[64];
    int n;
    if (!h->ptr || h->len <= 5) return;
    if (strncasecmp(h->ptr, "bytes", 5) != 0) return;
    n = h->len - 5;
    if (n > (int)sizeof tmp - 1) n = (int)sizeof tmp - 1;
    memcpy(tmp, h->ptr + 5, (size_t)n);
    tmp[n] = 0;
    if (sscanf(tmp, " %llu-%llu/%llu", &start, &end, &total) >= 2) {
        res->range_start = (long long)start;
        res->range_end = (long long)end;
        res->range_total = total;
    }
}

/* ---------------------------------------------------------------- probe -- */

int yhttp_probe(const char *url, const char *range, const char *referer,
                const char *cookie, int tls_mode, int auto_redirect,
                unsigned char *out, int out_cap, yhttp_result *res) {
    int tmpl = -1, conn = -1, req = -1;
    char *headers = NULL;
    unsigned int headers_size = 0;
    long long t0, t1;
    int ret = 0;

    if (!res) return -1;
    memset(res, 0, sizeof *res);
    res->tls_mode = tls_mode;
    res->content_length = -1;
    res->range_start = res->range_end = -1;

    if (yhttp_init() < 0) {
        res->err_at = 0;
        return -1;
    }
    if (tls_mode == YHTTP_TLS_VERIFY) yh_load_system_ca();
    /* Read the verification state *after* the attempt to arm it: reporting it
     * before was why the log showed flags=0x0 next to a success code. */
    res->ca_loaded = g_ca_loaded;
    res->verify_flags = g_verify_flags;
    res->http_pool = g_http_pool;
    res->ssl_pool = g_ssl_pool;

    tmpl = sceHttpCreateTemplate(YHTTP_USER_AGENT, SCE_HTTP_VERSION_1_1,
                                 SCE_HTTP_PROXY_AUTO);
    if (tmpl < 0) { res->err_code = tmpl; res->err_at = 1; return tmpl; }
    conn = sceHttpCreateConnectionWithURL(tmpl, url, 1);
    if (conn < 0) { res->err_code = conn; res->err_at = 1; goto done; }
    req = sceHttpCreateRequestWithURL(conn, SCE_HTTP_METHOD_GET, url, 0);
    if (req < 0) { res->err_code = req; res->err_at = 1; goto done; }

    /* §22: redirects, timeouts and header budget are all part of the contract. */
    sceHttpSetAutoRedirect(req, auto_redirect ? 1 : 0);
    sceHttpSetResolveTimeOut(req, YHTTP_RESOLVE_TIMEOUT_US);
    sceHttpSetConnectTimeOut(req, YHTTP_CONNECT_TIMEOUT_US);
    sceHttpSetRecvTimeOut(req, YHTTP_RECV_TIMEOUT_US);
    sceHttpSetResponseHeaderMaxSize(req, YHTTP_HEADER_MAX);
    if (range && *range)
        sceHttpAddRequestHeader(req, "Range", range, SCE_HTTP_HEADER_ADD);
    if (referer && *referer)
        sceHttpAddRequestHeader(req, "Referer", referer, SCE_HTTP_HEADER_ADD);
    if (cookie && *cookie)
        sceHttpAddRequestHeader(req, "Cookie", cookie, SCE_HTTP_HEADER_ADD);

    t0 = sceKernelGetSystemTimeWide();
    ret = sceHttpSendRequest(req, NULL, 0);
    if (ret < 0) { res->err_code = ret; res->err_at = 2; goto done; }

    ret = sceHttpGetStatusCode(req, &res->status);
    if (ret < 0) { res->err_code = ret; res->err_at = 3; goto done; }

    {
        unsigned long long clen = 0;
        if (sceHttpGetResponseContentLength(req, &clen) >= 0)
            res->content_length = (long long)clen;
    }
    if (sceHttpGetAllResponseHeaders(req, &headers, &headers_size) >= 0 &&
        headers) {
        yh_hdr cr = yh_header_find(headers, headers_size, "Content-Range");
        yh_hdr ct = yh_header_find(headers, headers_size, "Content-Type");
        yh_hdr loc = yh_header_find(headers, headers_size, "Location");
        res->headers_len = (int)headers_size;
        yh_parse_content_range(&cr, res);
        if (ct.ptr && ct.len >= 6 && strncasecmp(ct.ptr, "audio/", 6) == 0)
            res->content_type_audio = 1;
        /* %.*s with an explicit length: never reads past the block. */
        if (cr.ptr) yh_logf("yhttp: Content-Range: %.*s\n", cr.len, cr.ptr);
        if (ct.ptr) yh_logf("yhttp: Content-Type: %.*s\n", ct.len, ct.ptr);
        if (loc.ptr) {
            res->redirected = 1;
            yh_logf("yhttp: Location: %.*s\n", loc.len, loc.ptr);
        }
    }

    if (out && out_cap > 0) {
        int want = out_cap;
        int got = 0;
        while (got < want) {
            ret = sceHttpReadData(req, out + got, (unsigned int)(want - got));
            if (ret == 0) break;            /* end of body */
            if (ret < 0) { res->err_code = ret; res->err_at = 4; break; }
            got += ret;
        }
        res->bytes_read = got;
    }

    t1 = sceKernelGetSystemTimeWide();
    res->took_ms = (unsigned int)((t1 - t0) / 1000);

    if (tls_mode == YHTTP_TLS_VERIFY) {
        int ssl_err = 0;
        unsigned int detail = 0;
        if (sceHttpsGetSslError(req, &ssl_err, &detail) >= 0) {
            res->ssl_error = ssl_err;
            res->ssl_detail = detail;
        }
    }

done:
    /* `headers` belongs to SceHttp's pool: freeing it corrupts that pool. */
    if (req >= 0) sceHttpDeleteRequest(req);
    if (conn >= 0) sceHttpDeleteConnection(conn);
    if (tmpl >= 0) sceHttpDeleteTemplate(tmpl);
    return res->err_code;
}

/* ---------------------------------------------------------------- abort -- */

typedef struct {
    int req;
    volatile int started;   /* SendRequest returned */
    volatile int stopped;   /* the read loop exited */
    volatile int bytes;     /* how much the worker has transferred */
    int send_rc;
    int read_rc;
} yh_abort_ctx;

/*
 * Static on purpose: sceKernelStartThread() *copies* its argument bytes onto
 * the new thread's stack, so passing the address of a stack struct gave the
 * worker a private copy while this thread watched the original (all zeroes —
 * why the first two abort runs always reported bytes_in_flight=0).
 */
static yh_abort_ctx g_abort;

static int yh_abort_worker(unsigned int args, void *argp) {
    /* sceKernelStartThread() copies its argument bytes onto the new thread's
     * stack, so we deliberately start with no argument and share the one
     * static context instead — reading `argp` here is what wrote to NULL. */
    yh_abort_ctx *ctx = &g_abort;
    unsigned char scratch[4096];
    (void)args;
    (void)argp;
    ctx->send_rc = sceHttpSendRequest(ctx->req, NULL, 0);
    if (ctx->send_rc >= 0) {
        ctx->started = 1;
        /* Chase the body in small chunks: the abort has to land inside a
         * transfer that is genuinely in flight, not between requests. */
        for (;;) {
            int n = sceHttpReadData(ctx->req, scratch, sizeof scratch);
            if (n <= 0) {
                ctx->read_rc = n;
                break;
            }
            ctx->bytes += n;
            if (ctx->bytes > 64 * 1024 * 1024) break; /* runaway guard */
        }
    }
    ctx->stopped = 1;
    return 0;
}

int yhttp_abort_probe(const char *url, const char *referer, int tls_mode,
                      unsigned int wait_ms, yhttp_result *res) {
    int tmpl = -1, conn = -1, req = -1;
    SceUID thid;
    long long t0, t1;
    int ret;

    if (!res) return -1;
    memset(res, 0, sizeof *res);
    res->tls_mode = tls_mode;
    res->content_length = -1;
    res->range_start = res->range_end = -1;

    if (yhttp_init() < 0) { res->err_at = 0; return -1; }
    if (tls_mode == YHTTP_TLS_VERIFY) yh_load_system_ca();

    tmpl = sceHttpCreateTemplate(YHTTP_USER_AGENT, SCE_HTTP_VERSION_1_1,
                                 SCE_HTTP_PROXY_AUTO);
    if (tmpl < 0) { res->err_code = tmpl; res->err_at = 1; return tmpl; }
    conn = sceHttpCreateConnectionWithURL(tmpl, url, 1);
    if (conn < 0) { res->err_code = conn; res->err_at = 1; goto done; }
    req = sceHttpCreateRequestWithURL(conn, SCE_HTTP_METHOD_GET, url, 0);
    if (req < 0) { res->err_code = req; res->err_at = 1; goto done; }
    sceHttpSetAutoRedirect(req, 1);
    sceHttpSetResolveTimeOut(req, YHTTP_RESOLVE_TIMEOUT_US);
    sceHttpSetConnectTimeOut(req, YHTTP_CONNECT_TIMEOUT_US);
    sceHttpSetRecvTimeOut(req, YHTTP_RECV_TIMEOUT_US);
    if (referer && *referer)
        sceHttpAddRequestHeader(req, "Referer", referer, SCE_HTTP_HEADER_ADD);

    memset(&g_abort, 0, sizeof g_abort);
    g_abort.req = req;
    thid = sceKernelCreateThread("yunyin-net-abort", yh_abort_worker,
                                 0x10000100, 0x4000, 0, 0, NULL);
    if (thid < 0) {
        res->err_code = thid;
        res->err_at = 1;
        goto done;
    }
    ret = sceKernelStartThread(thid, 0, NULL);   /* worker reads g_abort */
    if (ret < 0) {
        res->err_code = ret;
        res->err_at = 1;
        sceKernelDeleteThread(thid);
        goto done;
    }

    /*
     * Wait until a transfer is really in flight (SendRequest done *and* some
     * bytes received), then abort and measure how long the blocked read takes
     * to give up.  Waiting a fixed time instead — as the first version did —
     * only measured whether the worker's whole loop had finished (§24).
     */
    {
        unsigned int spin = 0;
        while (!g_abort.started && spin < 8000) {      /* up to 8 s to connect */
            sceKernelDelayThread(1000);
            spin++;
        }
        spin = 0;
        while (g_abort.bytes < 16384 && !g_abort.stopped && spin < 8000) {
            sceKernelDelayThread(1000);
            spin++;
        }
    }
    res->bytes_read = g_abort.bytes;   /* how much was in flight at abort time */
    (void)wait_ms;                  /* kept for API compatibility */

    t0 = sceKernelGetSystemTimeWide();
    ret = sceHttpAbortRequest(req);
    {
        unsigned int spin = 0;
        while (!g_abort.stopped && spin < 5000) {   /* up to 5 s */
            sceKernelDelayThread(1000);
            spin++;
        }
    }
    t1 = sceKernelGetSystemTimeWide();

    res->abort_took_ms = (unsigned int)((t1 - t0) / 1000);
    res->aborted = g_abort.stopped ? 1 : 0;
    if (ret < 0) { res->err_code = ret; res->err_at = 5; }
    else res->err_code = g_abort.read_rc;   /* expect SCE_HTTP_ERROR_ABORTED */
    yh_logf("yhttp: abort -> rc=0x%08X, stopped=%d, bytes_in_flight=%d, "
            "send=0x%08X read=0x%08X, stopped in %u ms\n",
            (unsigned)ret, g_abort.stopped, g_abort.bytes,
            (unsigned)g_abort.send_rc, (unsigned)g_abort.read_rc,
            res->abort_took_ms);
    sceKernelWaitThreadEnd(thid, NULL, NULL);
    sceKernelDeleteThread(thid);

done:
    if (!g_abort.stopped) {
        /* The worker is still inside a read on this request; deleting it would
         * pull the rug out from under that thread.  Leaking one request beats
         * crashing the app, and the next probe run re-creates everything. */
        yh_logf("yhttp: abort worker never stopped; leaving request %d alive\n",
                req);
        return res->err_code;
    }
    if (req >= 0) sceHttpDeleteRequest(req);
    if (conn >= 0) sceHttpDeleteConnection(conn);
    if (tmpl >= 0) sceHttpDeleteTemplate(tmpl);
    return res->err_code;
}

#else /* host build: the probe is Vita-only, keep the sources linkable */

void yhttp_set_log(yhttp_log_fn fn) { (void)fn; }
int yhttp_init(void) { return -1; }
void yhttp_term(void) {}
int yhttp_online(void) { return 0; }
int yhttp_memory(unsigned int *pool, unsigned int *in_use, unsigned int *peak) {
    (void)pool; (void)in_use; (void)peak;
    return -1;
}
int yhttp_probe(const char *url, const char *range, const char *referer,
                const char *cookie, int tls_mode, int auto_redirect,
                unsigned char *out, int out_cap, yhttp_result *res) {
    (void)url; (void)range; (void)referer; (void)cookie; (void)tls_mode;
    (void)auto_redirect; (void)out; (void)out_cap;
    if (res) memset(res, 0, sizeof *res);
    return -1;
}
int yhttp_abort_probe(const char *url, const char *referer, int tls_mode,
                      unsigned int wait_ms, yhttp_result *res) {
    (void)url; (void)referer; (void)tls_mode; (void)wait_ms;
    if (res) memset(res, 0, sizeof *res);
    return -1;
}

int yhttp_load_ca(void) { return -1; }
unsigned int yhttp_ca_http_pool(void) { return 0; }
unsigned int yhttp_ca_ssl_pool(void) { return 0; }

#endif /* __vita__ */
