/*
 * YUNYIN Vita HTTP transport.
 *
 * The first implementation used SceHttp/SceSsl directly. That makes HTTPS
 * depend on the firmware's TLS implementation: older physical Vitas reject
 * current NetEase endpoints before an HTTP response exists, even when the
 * application supplies the right Root CA.
 *
 * This implementation keeps the same small C ABI, but uses VitaSDK's
 * libcurl/OpenSSL port. The CA bundle is loaded from app0:/certs/ and passed
 * to libcurl as an in-memory PEM blob, so neither the Vita firmware CA store
 * nor a host filesystem path is involved in verification.
 */

#include "yhttp.h"

/* Keep the small UI compatibility symbol in this same archive member so it is
 * pulled together with the locally rebuilt libcurl before the linker scans it. */
#include "openssl_compat.c"
#include "host/yunyin_log.h"

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>

#ifdef __vita__

#include <curl/curl.h>
#include <psp2/io/fcntl.h>
#include <psp2/kernel/threadmgr.h>
#include <psp2/net/net.h>
#include <psp2/net/netctl.h>
#include <psp2/sysmodule.h>

#define YHTTP_USER_AGENT \
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 " \
    "(KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"

/*
 * 连接超时 4 秒（原来 10 秒）。
 *
 * 为什么改：真机上 m704/m804 这类 CDN 域从 Vita 连不上，每次都要等满 10 秒才失败 ——
 * 界面"点一次在线歌"要等 12 秒 × 3 次重试，用户得反复点几次才碰上一个能连的域名。
 * PC 上同一个地址 0.4 秒就连上，4 秒对正常网络足够宽裕；失败更快 = 更早换解析结果。
 */
#define YHTTP_CONNECT_TIMEOUT_MS 4000L
#define YHTTP_TOTAL_TIMEOUT_MS 30000L
#define YHTTP_CA_MAX (256 * 1024)

#define YHTTP_ERR_NETWORK ((int)0x80431063u)
#define YHTTP_ERR_SSL ((int)0x80431075u)
#define YHTTP_ERR_TIMEOUT ((int)0x80431068u)
#define YHTTP_ERR_ABORTED ((int)0x80431080u)
#define YHTTP_ERR_RESOLVE ((int)0x80436007u)

static yhttp_log_fn g_log;
static volatile int g_init_lock;
static int g_inited;
static int g_curl_inited;
static void *g_net_pool;
static volatile int g_inflight;
static volatile int g_streams_open;
static volatile int g_dl_active;
static volatile long long g_dl_got;
static volatile long long g_dl_total = -1;
static unsigned int g_http_pool = YHTTP_HTTP_POOL;
static unsigned int g_ssl_pool;

/* Loaded once and kept alive for every easy handle using CURLOPT_CAINFO_BLOB. */
static unsigned char g_ca_buf[YHTTP_CA_MAX];
static size_t g_ca_len;
static int g_ca_loaded;
static int g_ca_tried;
static const char *g_ca_path;

/* ------------------------------------------------------------------ logging */

static void yh_logf(const char *fmt, ...) {
    char line[320];
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

/* ------------------------------------------------------------ certificate */

static int yh_read_file(const char *path) {
    SceUID fd;
    int total = 0;

    fd = sceIoOpen(path, SCE_O_RDONLY, 0);
    if (fd < 0) return fd;
    while (total < (int)sizeof g_ca_buf) {
        int n = sceIoRead(fd, g_ca_buf + total,
                          (unsigned int)(sizeof g_ca_buf - (size_t)total));
        if (n <= 0) break;
        total += n;
    }
    sceIoClose(fd);
    if (total <= 0) return -1;
    if (total == (int)sizeof g_ca_buf) {
        yh_logf("yhttp: CA bundle exceeds %u bytes", (unsigned)sizeof g_ca_buf);
        return -1;
    }
    g_ca_len = (size_t)total;
    return 0;
}

int yhttp_load_ca(void) {
    /* The Vita OpenSSL build rejects some otherwise valid certificates when a
     * large modern bundle is imported through PEM_X509_INFO_read_bio. NetEase
     * currently chains to DigiCert Global Root G2, so use that small, stable
     * trust anchor first. Keep the complete bundle as a fallback for other
     * endpoints and future CDN chains. */
    static const char *const paths[] = {
        "app0:/certs/digicert-global-root-g2.pem",
        "app0:/certs/ca-bundle.pem",
    };
    unsigned int i;

    if (g_ca_tried) return g_ca_loaded ? 0 : -1;
    g_ca_tried = 1;
    for (i = 0; i < sizeof paths / sizeof paths[0]; i++) {
        if (yh_read_file(paths[i]) == 0) {
            g_ca_loaded = 1;
            g_ca_path = paths[i];
            yh_logf("yhttp: CA bundle loaded from %s (%u bytes)",
                    paths[i], (unsigned)g_ca_len);
            return 0;
        }
    }
    yh_logf("yhttp: no app CA bundle could be loaded");
    return -1;
}

/* ------------------------------------------------------------ lifecycle */

int yhttp_init(void) {
    SceNetInitParam param;
    int ret;

    for (;;) {
        int v = g_init_lock;
        if (v == 2) return 0;
        if (v == 0 && __sync_bool_compare_and_swap(&g_init_lock, 0, 1)) break;
        sceKernelDelayThread(1000);
    }
    if (g_inited) {
        g_init_lock = 2;
        return 0;
    }

    ret = sceSysmoduleLoadModule(SCE_SYSMODULE_NET);
    yh_logf("yhttp: sysmodule NET -> 0x%08X", (unsigned)ret);
    if (ret < 0 && (unsigned)ret != 0x8002D013u) {
        g_init_lock = 0;
        return ret;
    }

    g_net_pool = malloc(YHTTP_NET_POOL);
    if (!g_net_pool) {
        g_init_lock = 0;
        return -1;
    }
    param.memory = g_net_pool;
    param.size = YHTTP_NET_POOL;
    param.flags = 0;
    ret = sceNetInit(&param);
    yh_logf("yhttp: sceNetInit(%d) -> 0x%08X", YHTTP_NET_POOL, (unsigned)ret);
    if (ret < 0 && (unsigned)ret != 0x80410110u) {
        free(g_net_pool);
        g_net_pool = NULL;
        g_init_lock = 0;
        return ret;
    }

    ret = sceNetCtlInit();
    yh_logf("yhttp: sceNetCtlInit -> 0x%08X", (unsigned)ret);
    if (ret < 0 && (unsigned)ret != 0x80410110u)
        yh_logf("yhttp: continuing after NetCtl init error");

    ret = curl_global_init(CURL_GLOBAL_DEFAULT);
    yh_logf("yhttp: curl_global_init -> %d", ret);
    if (ret != CURLE_OK) {
        sceNetCtlTerm();
        sceNetTerm();
        free(g_net_pool);
        g_net_pool = NULL;
        g_init_lock = 0;
        return YHTTP_ERR_NETWORK;
    }
    g_curl_inited = 1;
    yhttp_load_ca();
    yh_logf("yhttp: TLS backend = libcurl/OpenSSL, min=TLS1.2 max=TLS1.2");
    g_inited = 1;
    g_init_lock = 2;
    return 0;
}

void yhttp_term(void) {
    if (g_curl_inited) {
        curl_global_cleanup();
        g_curl_inited = 0;
    }
    sceNetCtlTerm();
    sceNetTerm();
    sceSysmoduleUnloadModule(SCE_SYSMODULE_NET);
    free(g_net_pool);
    g_net_pool = NULL;
    g_inited = 0;
    g_init_lock = 0;
}

int yhttp_net_reset(void) {
    if (g_inflight > 0 || g_streams_open > 0) {
        yh_logf("yhttp: network reset postponed (inflight=%d streams=%d)",
                g_inflight, g_streams_open);
        return -1;
    }
    yh_logf("yhttp: rebuilding libcurl/SceNet stack");
    yhttp_term();
    return yhttp_init();
}

int yhttp_online(void) {
    int state = 0;
    SceNetCtlInfo info;
    int ret;

    if (yhttp_init() < 0) return 0;
    ret = sceNetCtlInetGetState(&state);
    if (ret < 0) {
        yh_logf("yhttp: sceNetCtlInetGetState -> 0x%08X", (unsigned)ret);
        return 0;
    }
    yh_logf("yhttp: netctl state=%d (3=connected)", state);
    if (state != SCE_NETCTL_STATE_CONNECTED) return 0;
    memset(&info, 0, sizeof info);
    if (sceNetCtlInetGetInfo(SCE_NETCTL_INFO_GET_IP_ADDRESS, &info) >= 0)
        yh_logf("yhttp: ip=%s", info.ip_address);
    return 1;
}

int yhttp_memory(unsigned int *pool, unsigned int *in_use, unsigned int *peak) {
    if (pool) *pool = g_http_pool;
    if (in_use) *in_use = 0;
    if (peak) *peak = 0;
    yh_logf("yhttp: curl backend pool http=%u ssl=OpenSSL", g_http_pool);
    return 0;
}

unsigned int yhttp_ca_http_pool(void) { return g_http_pool; }
unsigned int yhttp_ca_ssl_pool(void) { return g_ssl_pool; }
int yhttp_inflight(void) { return g_inflight; }
int yhttp_dl_active(void) { return g_dl_active; }
long long yhttp_dl_got(void) { return g_dl_got; }
long long yhttp_dl_total(void) { return g_dl_total; }

/* ------------------------------------------------------------ curl helpers */

typedef struct {
    unsigned char *dst;
    size_t cap;
    size_t got;
    int overflow;
} yh_body_sink;

typedef struct {
    int headers_len;
    int range_valid;
    long long range_start;
    long long range_end;
    unsigned long long range_total;
    char *cookie_out;
    int cookie_cap;
    int cookie_len;
} yh_header_sink;

static size_t yh_write_body(char *ptr, size_t size, size_t nmemb, void *opaque) {
    yh_body_sink *sink = (yh_body_sink *)opaque;
    size_t n = size * nmemb;
    if (!sink->dst) {
        sink->got += n;
        g_dl_got = (long long)sink->got;
        return n;
    }
    if (sink->got + n > sink->cap) {
        size_t keep = sink->cap > sink->got ? sink->cap - sink->got : 0;
        if (keep) memcpy(sink->dst + sink->got, ptr, keep);
        sink->got += keep;
        sink->overflow = 1;
        g_dl_got = (long long)sink->got;
        return 0;
    }
    memcpy(sink->dst + sink->got, ptr, n);
    sink->got += n;
    g_dl_got = (long long)sink->got;
    return n;
}

static void yh_append_cookie(yh_header_sink *sink, const char *value, int len) {
    int i = 0;
    int end = len;
    int need;
    while (i < end && (value[i] == ' ' || value[i] == '\t')) i++;
    while (end > i && (value[end - 1] == '\r' || value[end - 1] == '\n')) end--;
    for (int j = i; j < end; j++) {
        if (value[j] == ';') {
            end = j;
            break;
        }
    }
    if (end <= i || !sink->cookie_out || sink->cookie_cap <= 1) return;
    need = end - i;
    if (sink->cookie_len > 0) need += 2;
    if (sink->cookie_len + need >= sink->cookie_cap)
        need = sink->cookie_cap - sink->cookie_len - 1;
    if (need <= 0) return;
    if (sink->cookie_len > 0) {
        sink->cookie_out[sink->cookie_len++] = ';';
        if (sink->cookie_len < sink->cookie_cap - 1)
            sink->cookie_out[sink->cookie_len++] = ' ';
        need -= 2;
    }
    if (need > 0) {
        memcpy(sink->cookie_out + sink->cookie_len, value + i, (size_t)need);
        sink->cookie_len += need;
        sink->cookie_out[sink->cookie_len] = 0;
    }
}

static size_t yh_header_cb(char *ptr, size_t size, size_t nmemb, void *opaque) {
    yh_header_sink *sink = (yh_header_sink *)opaque;
    size_t n = size * nmemb;
    const char *p = ptr;
    if (n > 0) sink->headers_len += (int)n;
    if (n >= 5 && !strncasecmp(p, "HTTP/", 5)) {
        sink->range_valid = 0;
    } else if (n >= 12 && !strncasecmp(p, "Set-Cookie:", 11)) {
        yh_append_cookie(sink, p + 11, (int)n - 11);
    } else if (n >= 14 && !strncasecmp(p, "Content-Range:", 14)) {
        unsigned long long start = 0, end = 0, total = 0;
        char tmp[128];
        int m = (int)n - 14;
        if (m >= (int)sizeof tmp) m = (int)sizeof tmp - 1;
        memcpy(tmp, p + 14, (size_t)m);
        tmp[m] = 0;
        if (sscanf(tmp, " bytes %llu-%llu/%llu", &start, &end, &total) >= 2) {
            sink->range_start = (long long)start;
            sink->range_end = (long long)end;
            sink->range_total = total;
            sink->range_valid = 1;
        }
    }
    return n;
}

static int yh_progress_cb(void *opaque, curl_off_t dltotal, curl_off_t dlnow,
                          curl_off_t ultotal, curl_off_t ulnow) {
    volatile int *cancelled = (volatile int *)opaque;
    (void)dltotal;
    (void)dlnow;
    (void)ultotal;
    (void)ulnow;
    return cancelled && *cancelled ? 1 : 0;
}

static struct curl_slist *yh_add_header(struct curl_slist *list,
                                        const char *name, const char *value) {
    size_t n;
    char *line;
    struct curl_slist *out;
    if (!value || !*value) return list;
    n = strlen(name) + 2 + strlen(value) + 1;
    line = (char *)malloc(n);
    if (!line) return list;
    snprintf(line, n, "%s: %s", name, value);
    out = curl_slist_append(list, line);
    free(line);
    return out;
}

static void yh_configure_common(CURL *easy, const char *url, int tls_mode,
                                int follow, struct curl_slist *headers,
                                volatile int *cancelled) {
    long verify = (tls_mode == YHTTP_TLS_VERIFY || tls_mode == YHTTP_TLS_DEFAULT) ? 1L : 0L;
    curl_easy_setopt(easy, CURLOPT_URL, url);
    curl_easy_setopt(easy, CURLOPT_USERAGENT, YHTTP_USER_AGENT);
    curl_easy_setopt(easy, CURLOPT_HTTP_VERSION, CURL_HTTP_VERSION_1_1);
    curl_easy_setopt(easy, CURLOPT_FOLLOWLOCATION, follow ? 1L : 0L);
    curl_easy_setopt(easy, CURLOPT_MAXREDIRS, 6L);
    curl_easy_setopt(easy, CURLOPT_NOSIGNAL, 1L);
    curl_easy_setopt(easy, CURLOPT_CONNECTTIMEOUT_MS, YHTTP_CONNECT_TIMEOUT_MS);
    curl_easy_setopt(easy, CURLOPT_TIMEOUT_MS, YHTTP_TOTAL_TIMEOUT_MS);
    curl_easy_setopt(easy, CURLOPT_SSL_VERIFYPEER, verify);
    curl_easy_setopt(easy, CURLOPT_SSL_VERIFYHOST, verify ? 2L : 0L);
    /* The VitaSDK 2026.08 libcurl/OpenSSL archive is built without TLS 1.3
     * support (curl_easy_perform returns CURLE_NOT_BUILT_IN if the TLS 1.3
     * upper bound is requested).  TLS 1.2 is supported by the current
     * NetEase endpoints and is the newest protocol this target actually
     * contains, so request TLS 1.2 or newer without naming the unavailable
     * TLS 1.3 maximum. */
    curl_easy_setopt(easy, CURLOPT_SSLVERSION, (long)CURL_SSLVERSION_TLSv1_2);
    if (verify && g_ca_loaded) {
        struct curl_blob blob;
        CURLcode ca_blob_rc;
        CURLcode ca_path_rc = CURLE_OK;

        blob.data = g_ca_buf;
        blob.len = g_ca_len;
        blob.flags = CURL_BLOB_NOCOPY;
        /* The full bundle was rejected by the Vita OpenSSL PEM importer. The
         * selected small anchor is imported from memory first, which avoids
         * any dependency on host-style paths. Fall back to app0:/ only if the
         * target curl build does not expose CAINFO_BLOB. */
        ca_blob_rc = curl_easy_setopt(easy, CURLOPT_CAINFO_BLOB, &blob);
        if (ca_blob_rc != CURLE_OK)
            ca_path_rc = curl_easy_setopt(easy, CURLOPT_CAINFO, g_ca_path);
        if (ca_blob_rc != CURLE_OK || ca_path_rc != CURLE_OK)
            yh_logf("yhttp: CA setopt blob=%d path=%d", (int)ca_blob_rc,
                    (int)ca_path_rc);
    }
    if (headers) curl_easy_setopt(easy, CURLOPT_HTTPHEADER, headers);
    if (cancelled) {
        curl_easy_setopt(easy, CURLOPT_NOPROGRESS, 0L);
        curl_easy_setopt(easy, CURLOPT_XFERINFOFUNCTION, yh_progress_cb);
        curl_easy_setopt(easy, CURLOPT_XFERINFODATA, (void *)cancelled);
    }
}

static int yh_map_curl_error(CURLcode code) {
    switch (code) {
    case CURLE_OPERATION_TIMEDOUT:
        return YHTTP_ERR_TIMEOUT;
    case CURLE_COULDNT_RESOLVE_HOST:
        return YHTTP_ERR_RESOLVE;
    case CURLE_SSL_CONNECT_ERROR:
    case CURLE_PEER_FAILED_VERIFICATION:
    case CURLE_SSL_CACERT_BADFILE:
    case CURLE_USE_SSL_FAILED:
        return YHTTP_ERR_SSL;
    case CURLE_ABORTED_BY_CALLBACK:
        return YHTTP_ERR_ABORTED;
    default:
        return YHTTP_ERR_NETWORK;
    }
}

static void yh_fill_result(CURL *easy, yh_header_sink *headers,
                           yh_body_sink *body, yhttp_result *res) {
    long status = 0;
    long redirects = 0;
    curl_off_t content_len = -1;
    char *content_type = NULL;
    if (!res) return;
    if (curl_easy_getinfo(easy, CURLINFO_RESPONSE_CODE, &status) == CURLE_OK)
        res->status = (int)status;
    if (curl_easy_getinfo(easy, CURLINFO_REDIRECT_COUNT, &redirects) == CURLE_OK)
        res->redirected = redirects > 0 ? 1 : 0;
    if (curl_easy_getinfo(easy, CURLINFO_CONTENT_LENGTH_DOWNLOAD_T, &content_len) == CURLE_OK)
        res->content_length = content_len >= 0 ? (long long)content_len : -1;
    if (curl_easy_getinfo(easy, CURLINFO_CONTENT_TYPE, &content_type) == CURLE_OK &&
        content_type && !strncasecmp(content_type, "audio/", 6))
        res->content_type_audio = 1;
    if (headers) {
        res->headers_len = headers->headers_len;
        if (headers->range_valid) {
            res->range_start = headers->range_start;
            res->range_end = headers->range_end;
            res->range_total = headers->range_total;
        }
    }
    if (body) res->bytes_read = (int)body->got;
    res->ca_loaded = g_ca_loaded;
    /* Zero is the report's "enable option succeeded" value. The actual
     * verification is libcurl/OpenSSL's peer+hostname check above. */
    res->verify_flags = 0;
    res->http_pool = g_http_pool;
    res->ssl_pool = g_ssl_pool;
}

static int yh_get(const char *url, const char *range, const char *referer,
                  const char *cookie, int tls_mode, int follow,
                  yh_body_sink *body, yh_header_sink *headers,
                  yhttp_result *res, volatile int *cancelled) {
    CURL *easy;
    struct curl_slist *list = NULL;
    CURLcode code;
    long long t0, t1;
    int ret;

    easy = curl_easy_init();
    if (!easy) return YHTTP_ERR_NETWORK;
    list = yh_add_header(list, "Accept", "*/*");
    list = yh_add_header(list, "Referer", referer);
    list = yh_add_header(list, "Cookie", cookie);
    list = yh_add_header(list, "Range", range);
    yh_configure_common(easy, url, tls_mode, follow, list, cancelled);
    curl_easy_setopt(easy, CURLOPT_WRITEFUNCTION, yh_write_body);
    curl_easy_setopt(easy, CURLOPT_WRITEDATA, body);
    curl_easy_setopt(easy, CURLOPT_HEADERFUNCTION, yh_header_cb);
    curl_easy_setopt(easy, CURLOPT_HEADERDATA, headers);

    t0 = sceKernelGetSystemTimeWide();
    g_dl_active = 1;
    g_dl_got = 0;
    g_dl_total = -1;
    __sync_add_and_fetch(&g_inflight, 1);
    code = curl_easy_perform(easy);
    __sync_sub_and_fetch(&g_inflight, 1);
    {
        curl_off_t total = -1;
        if (curl_easy_getinfo(easy, CURLINFO_CONTENT_LENGTH_DOWNLOAD_T, &total) == CURLE_OK)
            g_dl_total = total >= 0 ? (long long)total : -1;
    }
    g_dl_active = 0;
    t1 = sceKernelGetSystemTimeWide();
    if (res) res->took_ms = (unsigned int)((t1 - t0) / 1000);
    yh_fill_result(easy, headers, body, res);

    if (code == CURLE_OK && body && body->overflow) {
        ret = -2;
        if (res) {
            res->err_code = ret;
            res->err_at = 4;
        }
        yh_logf("yhttp: response exceeds buffer url=%s", url ? url : "(null)");
    } else if (code != CURLE_OK) {
        ret = yh_map_curl_error(code);
        if (res) {
            res->err_code = ret;
            res->err_at = 2;
            if (ret == YHTTP_ERR_SSL) {
                res->ssl_error = 0x20;
                res->ssl_detail = (unsigned int)code;
            }
        }
        {
            /*
             * 把"连的是哪个 IP、等了多久、系统错误码是多少"记下来。
             *
             * 真机上 m704/m804 会 10s 连接超时（三次重试间隔 ~12s），而 PC 上同一个
             * URL 0.4s 连上并返回 206 —— 说明 Vita 解析到的那个边缘地址可能根本不可达。
             * 没有这一行就只能猜是网络、CDN 还是解析。
             */
            char *peer_ip = NULL;
            long os_errno = 0;
            curl_easy_getinfo(easy, CURLINFO_PRIMARY_IP, &peer_ip);
            curl_easy_getinfo(easy, CURLINFO_OS_ERRNO, &os_errno);
            yh_logf("yhttp: GET failed curl=%d (%s) mapped=0x%08X stage=send ip=%s os_errno=%ld took=%ums url=%s",
                    (int)code, curl_easy_strerror(code), (unsigned)ret,
                    peer_ip ? peer_ip : "-", os_errno,
                    res ? res->took_ms : 0u,
                    url ? url : "(null)");
        }
    } else {
        ret = 0;
    }
    curl_slist_free_all(list);
    curl_easy_cleanup(easy);
    return ret;
}

/* ------------------------------------------------------------ GET/probe */

static int yhttp_probe_impl(const char *url, const char *range,
                            const char *referer, const char *cookie,
                            int tls_mode, int auto_redirect,
                            unsigned char *out, int out_cap,
                            yhttp_result *res) {
    yh_body_sink body;
    yh_header_sink headers;
    int ret;
    if (!res) return -1;
    memset(res, 0, sizeof *res);
    res->tls_mode = tls_mode;
    res->content_length = -1;
    res->range_start = res->range_end = -1;
    if (yhttp_init() < 0) {
        res->err_at = 0;
        return -1;
    }
    memset(&body, 0, sizeof body);
    body.dst = out;
    body.cap = out ? (out_cap > 0 ? (size_t)out_cap : 0) : 0;
    memset(&headers, 0, sizeof headers);
    ret = yh_get(url, range, referer, cookie, tls_mode, auto_redirect,
                 &body, &headers, res, NULL);
    if (out && out_cap > 0 && body.got < (size_t)out_cap)
        out[body.got] = 0;
    return ret;
}

int yhttp_probe(const char *url, const char *range, const char *referer,
                const char *cookie, int tls_mode, int auto_redirect,
                unsigned char *out, int out_cap, yhttp_result *res) {
    return yhttp_probe_impl(url, range, referer, cookie, tls_mode,
                            auto_redirect, out, out_cap, res);
}

/* ------------------------------------------------------------ POST */

static int yhttp_post_impl(const char *url, const char *body_text,
                           const char *content_type, const char *referer,
                           const char *cookie, int tls_mode,
                           unsigned char *out, int out_cap, int *status_out,
                           int *len_out, char *set_cookie_out,
                           int set_cookie_cap) {
    CURL *easy;
    struct curl_slist *list = NULL;
    yh_body_sink body;
    yh_header_sink headers;
    CURLcode code;
    long status = 0;
    size_t body_len = body_text ? strlen(body_text) : 0;
    int ret;

    if (status_out) *status_out = 0;
    if (len_out) *len_out = 0;
    if (set_cookie_out && set_cookie_cap > 0) set_cookie_out[0] = 0;
    if (yhttp_init() < 0) return -1;
    easy = curl_easy_init();
    if (!easy) return YHTTP_ERR_NETWORK;

    memset(&body, 0, sizeof body);
    body.dst = out;
    body.cap = out ? (out_cap > 0 ? (size_t)out_cap - 1 : 0) : 0;
    memset(&headers, 0, sizeof headers);
    headers.cookie_out = set_cookie_out;
    headers.cookie_cap = set_cookie_cap;

    list = yh_add_header(list, "Content-Type", content_type);
    list = yh_add_header(list, "Referer", referer);
    list = yh_add_header(list, "Cookie", cookie);
    yh_configure_common(easy, url, tls_mode, 0, list, NULL);
    curl_easy_setopt(easy, CURLOPT_POST, 1L);
    curl_easy_setopt(easy, CURLOPT_POSTFIELDS, body_text ? body_text : "");
    curl_easy_setopt(easy, CURLOPT_POSTFIELDSIZE_LARGE, (curl_off_t)body_len);
    curl_easy_setopt(easy, CURLOPT_WRITEFUNCTION, yh_write_body);
    curl_easy_setopt(easy, CURLOPT_WRITEDATA, &body);
    curl_easy_setopt(easy, CURLOPT_HEADERFUNCTION, yh_header_cb);
    curl_easy_setopt(easy, CURLOPT_HEADERDATA, &headers);

    g_dl_active = 1;
    g_dl_got = 0;
    g_dl_total = -1;
    __sync_add_and_fetch(&g_inflight, 1);
    code = curl_easy_perform(easy);
    __sync_sub_and_fetch(&g_inflight, 1);
    {
        curl_off_t total = -1;
        if (curl_easy_getinfo(easy, CURLINFO_CONTENT_LENGTH_DOWNLOAD_T, &total) == CURLE_OK)
            g_dl_total = total >= 0 ? (long long)total : -1;
    }
    g_dl_active = 0;
    if (curl_easy_getinfo(easy, CURLINFO_RESPONSE_CODE, &status) == CURLE_OK &&
        status_out)
        *status_out = (int)status;
    if (len_out) *len_out = (int)body.got;
    if (out && out_cap > 0) {
        size_t nul = body.got < (size_t)out_cap ? body.got : (size_t)out_cap - 1;
        out[nul] = 0;
    }

    if (code == CURLE_OK && !body.overflow) {
        ret = 0;
    } else if (body.overflow) {
        ret = -2;
        yh_logf("yhttp: POST response exceeds buffer url=%s", url ? url : "(null)");
    } else {
        ret = yh_map_curl_error(code);
        {
            /*
             * 把"连的是哪个 IP、等了多久、系统错误码是多少"记下来。
             *
             * 真机上 m704/m804 会 10s 连接超时（三次重试间隔 ~12s），而 PC 上同一个
             * URL 0.4s 连上并返回 206 —— 说明 Vita 解析到的那个边缘地址可能根本不可达。
             * 没有这一行就只能猜是网络、CDN 还是解析。
             */
            char *peer_ip = NULL;
            long os_errno = 0;
            curl_easy_getinfo(easy, CURLINFO_PRIMARY_IP, &peer_ip);
            curl_easy_getinfo(easy, CURLINFO_OS_ERRNO, &os_errno);
            yh_logf("yhttp: POST failed curl=%d (%s) mapped=0x%08X stage=send ip=%s os_errno=%ld url=%s",
                    (int)code, curl_easy_strerror(code), (unsigned)ret,
                    peer_ip ? peer_ip : "-", os_errno,
                    url ? url : "(null)");
        }
    }
    curl_slist_free_all(list);
    curl_easy_cleanup(easy);
    return ret;
}

int yhttp_post(const char *url, const char *body, const char *content_type,
               const char *referer, const char *cookie, int tls_mode,
               unsigned char *out, int out_cap, int *status_out,
               int *len_out, char *set_cookie_out, int set_cookie_cap) {
    return yhttp_post_impl(url, body, content_type, referer, cookie, tls_mode,
                           out, out_cap, status_out, len_out,
                           set_cookie_out, set_cookie_cap);
}

/* The old diagnostic API promises a cancellation check. libcurl's stream path
 * uses the xfer callback for real cancellation; this bounded GET cannot tear
 * down another request's handle. */
int yhttp_abort_probe(const char *url, const char *referer, int tls_mode,
                      unsigned int wait_ms, yhttp_result *res) {
    (void)wait_ms;
    return yhttp_probe_impl(url, "bytes=0-255", referer, NULL, tls_mode, 1,
                            NULL, 0, res);
}

/* ------------------------------------------------------------ range stream */

struct yhttp_stream {
    char *url;
    char *referer;
    char *cookie;
    int tls_mode;
    long long size;
    unsigned char *win;
    int win_cap;
    long long win_start;
    int win_len;
    int eof;
    int err;
    int opened;
    int last_status;
    volatile int cancelled;
};

static char *yh_strdup0(const char *s) {
    size_t n;
    char *p;
    if (!s) return NULL;
    n = strlen(s) + 1;
    p = (char *)malloc(n);
    if (p) memcpy(p, s, n);
    return p;
}

static int yh_stream_fetch(yhttp_stream *s, long long off, int want) {
    char range[64];
    yh_body_sink body;
    yh_header_sink headers;
    yhttp_result result;
    int ret;

    if (s->cancelled) return YHTTP_ERR_ABORTED;
    if (s->size > 0 && off >= s->size) {
        s->eof = 1;
        s->win_start = off;
        s->win_len = 0;
        return 0;
    }
    if (want <= 0) return 0;
    snprintf(range, sizeof range, "bytes=%lld-%lld", off,
             off + (long long)want - 1);
    memset(&body, 0, sizeof body);
    body.dst = s->win;
    body.cap = (size_t)want;
    memset(&headers, 0, sizeof headers);
    memset(&result, 0, sizeof result);
    result.content_length = -1;
    result.range_start = result.range_end = -1;
    ret = yh_get(s->url, range, s->referer, s->cookie, s->tls_mode, 1,
                 &body, &headers, &result, &s->cancelled);
    s->last_status = result.status;
    if (ret < 0) {
        s->err = ret;
        return ret;
    }
    if (result.status == 416) {
        s->eof = 1;
        s->win_start = off;
        s->win_len = 0;
        return 0;
    }
    if (result.status < 200 || result.status >= 300) {
        s->err = YHTTP_ERR_NETWORK;
        return s->err;
    }
    s->win_start = off;
    s->win_len = (int)body.got;
    s->eof = body.got == 0;
    if (headers.range_valid && headers.range_total > 0)
        s->size = (long long)headers.range_total;
    else if (result.content_length >= 0 && result.status == 200)
        s->size = result.content_length;
    if (!s->opened) {
        s->opened = 1;
        __sync_add_and_fetch(&g_streams_open, 1);
    }
    return 0;
}

yhttp_stream *yhttp_stream_open(const char *url, const char *referer,
                                const char *cookie, int tls_mode,
                                long long *size_out, int *err_out,
                                int *status_out) {
    yhttp_stream *s;
    int ret;
    if (size_out) *size_out = -1;
    if (err_out) *err_out = 0;
    if (status_out) *status_out = 0;
    if (yhttp_init() < 0) {
        if (err_out) *err_out = YHTTP_ERR_NETWORK;
        return NULL;
    }
    s = (yhttp_stream *)calloc(1, sizeof *s);
    if (!s) {
        if (err_out) *err_out = -1;
        return NULL;
    }
    s->url = yh_strdup0(url);
    s->referer = yh_strdup0(referer ? referer : "");
    s->cookie = yh_strdup0(cookie ? cookie : "");
    s->tls_mode = tls_mode;
    s->size = -1;
    s->win_cap = YHTTP_WINDOW;
    s->win = (unsigned char *)malloc((size_t)s->win_cap);
    if (!s->url || !s->referer || !s->cookie || !s->win) {
        yhttp_stream_close(s);
        if (err_out) *err_out = -1;
        return NULL;
    }
    ret = yh_stream_fetch(s, 0, s->win_cap);
    if (ret < 0 || s->win_len == 0) {
        if (err_out) *err_out = ret < 0 ? ret : YHTTP_ERR_NETWORK;
        if (status_out) *status_out = s->last_status;
        yhttp_stream_close(s);
        return NULL;
    }
    if (size_out) *size_out = s->size;
    if (status_out) *status_out = s->last_status;
    return s;
}

long long yhttp_stream_read(yhttp_stream *s, long long off, void *dst,
                            long long n) {
    int ret;
    long long avail;
    long long take;
    if (!s || !dst || n <= 0) return 0;
    if (s->cancelled) return YHTTP_ERR_ABORTED;
    if (s->err < 0) return s->err;
    if (s->size > 0 && off >= s->size) return 0;
    if (off < s->win_start || off >= s->win_start + s->win_len) {
        ret = yh_stream_fetch(s, off, s->win_cap);
        if (ret < 0) return ret;
        if (s->win_len == 0) return 0;
    }
    avail = (long long)s->win_start + s->win_len - off;
    if (avail <= 0) return 0;
    take = n < avail ? n : avail;
    memcpy(dst, s->win + (off - s->win_start), (size_t)take);
    return take;
}

void yhttp_stream_cancel(yhttp_stream *s) {
    if (s) s->cancelled = 1;
}

void yhttp_stream_close(yhttp_stream *s) {
    if (!s) return;
    s->cancelled = 1;
    if (s->opened) __sync_sub_and_fetch(&g_streams_open, 1);
    free(s->url);
    free(s->referer);
    free(s->cookie);
    free(s->win);
    free(s);
}

int yhttp_stream_error(const yhttp_stream *s) {
    return s ? s->err : -1;
}

#else

void yhttp_set_log(yhttp_log_fn fn) { (void)fn; }
int yhttp_init(void) { return -1; }
void yhttp_term(void) {}
int yhttp_net_reset(void) { return -1; }
int yhttp_online(void) { return 0; }
int yhttp_memory(unsigned int *pool, unsigned int *in_use, unsigned int *peak) {
    if (pool) *pool = 0;
    if (in_use) *in_use = 0;
    if (peak) *peak = 0;
    return -1;
}
int yhttp_probe(const char *url, const char *range, const char *referer,
                const char *cookie, int tls_mode, int auto_redirect,
                unsigned char *out, int out_cap, yhttp_result *res) {
    (void)url; (void)range; (void)referer; (void)cookie; (void)tls_mode;
    (void)auto_redirect; (void)out; (void)out_cap; (void)res;
    return -1;
}
int yhttp_post(const char *url, const char *body, const char *content_type,
               const char *referer, const char *cookie, int tls_mode,
               unsigned char *out, int out_cap, int *status_out, int *len_out,
               char *set_cookie_out, int set_cookie_cap) {
    (void)url; (void)body; (void)content_type; (void)referer; (void)cookie;
    (void)tls_mode; (void)out; (void)out_cap; (void)status_out; (void)len_out;
    (void)set_cookie_out; (void)set_cookie_cap;
    return -1;
}
int yhttp_abort_probe(const char *url, const char *referer, int tls_mode,
                      unsigned int wait_ms, yhttp_result *res) {
    (void)url; (void)referer; (void)tls_mode; (void)wait_ms; (void)res;
    return -1;
}
int yhttp_load_ca(void) { return -1; }
int yhttp_inflight(void) { return 0; }
int yhttp_dl_active(void) { return 0; }
long long yhttp_dl_got(void) { return 0; }
long long yhttp_dl_total(void) { return 0; }
unsigned int yhttp_ca_http_pool(void) { return 0; }
unsigned int yhttp_ca_ssl_pool(void) { return 0; }

struct yhttp_stream { int unused; };
yhttp_stream *yhttp_stream_open(const char *url, const char *referer,
                                const char *cookie, int tls_mode,
                                long long *size_out, int *err_out,
                                int *status_out) {
    (void)url; (void)referer; (void)cookie; (void)tls_mode;
    if (size_out) *size_out = -1;
    if (err_out) *err_out = -1;
    if (status_out) *status_out = 0;
    return NULL;
}
long long yhttp_stream_read(yhttp_stream *s, long long off, void *dst, long long n) {
    (void)s; (void)off; (void)dst; (void)n;
    return -1;
}
void yhttp_stream_cancel(yhttp_stream *s) { (void)s; }
void yhttp_stream_close(yhttp_stream *s) { (void)s; }
int yhttp_stream_error(const yhttp_stream *s) { (void)s; return -1; }

#endif
