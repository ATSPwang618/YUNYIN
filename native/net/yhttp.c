/*
 * Phase 0 网络探针 —— 接口约定见 yhttp.h。
 *
 * 只按 VitaSDK 头文件写（psp2/net/{net,netctl,http}.h、psp2/libssl.h、
 * psp2/sysmodule.h），不出现任何别的平台的 API（§23 的要求）。每个失败路径都
 * 同时记录"卡在哪一步"和"原始 Vita 错误码" —— 真机上区分 DNS 失败 / TLS 失败 /
 * 被取消，正是这件事的意义所在。
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

/* 超时单位是微秒。Phase 0 希望"失败得快且看得懂"，所以取值偏短，
 * 一次启动内能跑完多轮尝试。 */
#define YHTTP_RESOLVE_TIMEOUT_US (5 * 1000 * 1000)
#define YHTTP_CONNECT_TIMEOUT_US (10 * 1000 * 1000)
#define YHTTP_RECV_TIMEOUT_US    (10 * 1000 * 1000)
#define YHTTP_HEADER_MAX         (16 * 1024)

static yhttp_log_fn g_log;
static int g_inited;
/* 0 = 没人初始化，1 = 正在初始化，2 = 已完成。
 * 探针线程和在线播放线程会同时进来（真机上抓到过两个线程并发 init，
 * 第二个 sceNetInit 返回 0x80410110 EBUSY），所以这里必须串行化。 */
static volatile int g_init_lock;
static int g_ssl_inited;
static int g_http_inited;
static int g_ca_loaded;
/* 根证书只值得试一次：这台机器上任何池大小都装不进去，反复重试只会反复
 * 重建网络栈（见 yhttp_load_ca 的说明）。 */
static int g_ca_tried;
/* 正在使用的在线流数量。装载根证书要先 sceHttpTerm/sceSslTerm 再 Init ——
 * 只要还有流在用这套栈，那就等于把音频线程脚下的地板抽掉。 */
static volatile int g_streams_open;
static void *g_net_pool;
static unsigned int g_ssl_pool = YHTTP_SSL_POOL;
static unsigned int g_http_pool = YHTTP_HTTP_POOL;
static int g_verify_flags;

/* ------------------------------------------------------------------ 日志 -- */

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

/* ------------------------------------------------------------ 生命周期 -- */

int yhttp_init(void) {
    SceNetInitParam param;
    int ret;

    for (;;) {
        int v = g_init_lock;
        if (v == 2) return 0;                       /* 已经好了 */
        if (v == 0 && __sync_bool_compare_and_swap(&g_init_lock, 0, 1)) break;
        sceKernelDelayThread(1000);                 /* 别人正在初始化，等它 */
    }
    if (g_inited) {
        g_init_lock = 2;
        return 0;
    }

    ret = sceSysmoduleLoadModule(SCE_SYSMODULE_NET);
    yh_logf("yhttp: sysmodule NET -> 0x%08X\n", (unsigned)ret);

    /* SceNetInit 会把这块内存留到整个生命周期结束，所以它必须活得比所有请求久
     * （不能用栈上的缓冲区）。 */
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
    /* EBUSY（0x80410110）= 网络栈已被初始化过：当成成功。 */
    if (ret < 0 && (unsigned)ret != 0x80410110u) {
        g_init_lock = 0; /* 让后来者能重试 */
        return ret;
    }

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
    if (ret < 0) {
        /* 0x80435020 = SSL ALREADY_INITED；HTTP 侧同类错误也一并容忍。 */
        unsigned u = (unsigned)ret;
        if (u != 0x80435020u && u != 0x80431012u) {
            g_init_lock = 0;
            return ret;
        }
    }
    g_http_inited = 1;

    g_inited = 1;
    g_init_lock = 2;
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
    /* §27：系统库有自己的内存池，所以诚实的口径是"我们给了多少、它们实际用了多少"。 */
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
 * 与证书校验相关（§23）。
 *
 * 实测：256 KiB 的 SceHttp 池装不下那 47 张根证书（sceHttpsLoadCert 返回
 * 0x80431022 OUT_OF_MEMORY），所以逐档放大池子重试，并把结果记下来。
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
    if (g_ca_tried) return -1; /* 试过了：这台机器装不进去，别再来一遍 */
    /*
     * 装根证书要先把 SceHttp/SceSsl 关掉、调大池子、再打开。
     *
     * 真机上"在线播放刚开个头就解码失败"就是这么来的：探针线程跑 verify 目标
     * 时调用这里，把整个网络栈重建了一遍，而音频线程手里正拿着同一个栈在
     * 读 AAC/MP3 字节 —— 请求当场全部作废，解码器只能报"打不开"。
     *
     * 所以：只要有在线流在用这套栈，就推迟（不置 g_ca_tried，等空闲时再说）。
     * 证书校验本来就不靠这一步（固件自带根证书库一直在验），跳过不会让校验失效。
     */
    if (g_streams_open > 0) {
        yh_logf("yhttp: 根证书装载推迟（有 %d 个在线流正在用网络栈）\n",
                g_streams_open);
        return -1;
    }
    g_ca_tried = 1;
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
 * 把"校验"打开。
 *
 * 有两个机制，它们**不是一回事**：
 *
 *   sceHttpsEnableOption(SCE_HTTPS_FLAG_*)  打开各项检查
 *       （服务器校验、CN、有效期、已知 CA）；没有 id 参数，是进程级设置。
 *   sceHttpsLoadCert()                      只是"额外注册根证书"。
 *
 * 根证书装载在真机上失败（任何池大小都 OOM），但这**不代表校验是关的**：
 * 固件本身用自带根证书库在验。所以先打开开关，真假交给"自签名证书"那个目标来判。
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
    /* 真正打开校验的是这些 flag；加载根证书只是"额外加分项"，
     * 这台固件拒绝它，不能因此让 verify 模式失败。 */
    g_verify_flags = yh_enable_verify_flags();
    (void)yhttp_load_ca();
    return 0;
}

/* -------------------------------------------------------------- 响应头 -- */

/*
 * 响应头的处理。
 *
 * `sceHttpGetAllResponseHeaders` 返回的是"库自己池里的指针 + 字节数"，
 * **不是调用方拥有的 C 字符串**：
 *
 *   - 绝对不能用 free() 释放（那会破坏 SceHttp 的堆 —— 第一次真机探针就是因为
 *     这个崩在 newlib 的 _svfprintf_r 里）；
 *   - 也不能当成 NUL 结尾的字符串：用 "%.150s" 打印会读过量，越过这个块的尾巴。
 *
 * 所以这里所有访问都以 `size` 为界，取值一律用"指针 + 长度"返回，而不是字符串。
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
    /* Content-Range: bytes 0-65535/12345678   （长度未知时是 "/" 加 "*"） */
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

/* ---------------------------------------------------------------- 请求 -- */

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
    /* 校验状态要在"打开校验"之后再读一次才上报（之前先读，日志里就出现
     * 成功码旁边写着 flags=0x0 的怪现象）。 */
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

    /* §22：重定向、超时、响应头上限都属于契约的一部分。 */
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
        /* 用带长度的 %.*s：永远不会读过头块。 */
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
    /* `headers` 属于 SceHttp 的内存池：释放它会破坏那个池。 */
    if (req >= 0) sceHttpDeleteRequest(req);
    if (conn >= 0) sceHttpDeleteConnection(conn);
    if (tmpl >= 0) sceHttpDeleteTemplate(tmpl);
    return res->err_code;
}

/* ------------------------------------------------------------ 流式读取 -- */

struct yhttp_stream {
    char *url;
    char *referer;
    int tls_mode;
    long long size;      /* -1 = 未知 */
    unsigned char *win;  /* 当前窗口 */
    int win_cap;
    long long win_start; /* 窗口在文件里的起始偏移 */
    int win_len;         /* 窗口里有效字节数 */
    int eof;             /* 已经到流末尾 */
    int err;             /* 最近一次错误码（0 = 没有） */
    int opened;          /* 是否已计入 g_streams_open（成功取到第一个窗口后为 1） */
    int last_status;     /* 最近一次 HTTP 状态码（诊断用） */
    volatile int cancelled;
};

/* 一个窗口最多重试几次。网络抖动/连接被回收是常态，一次失败就判定"这条流坏了"
 * 会把整首歌判死（旧版本的 s->err 是永久粘住的）。 */
#define YHTTP_FETCH_TRIES 3
#define YHTTP_RETRY_DELAY_US (200 * 1000)

/*
 * 抓一个窗口到 s->win（覆盖 [off, off+want)）。
 *
 * 复用探针那套已经真机验证过的用法：每次请求单独建 template/conn/req、
 * 开自动重定向、响应头按 headerSize 限界、库返回的指针只读不 free。
 */
static int yh_stream_fetch(yhttp_stream *s, long long off, int want) {
    int tmpl = -1, conn = -1, req = -1;
    char *headers = NULL;
    unsigned int headers_size = 0;
    char range[64];
    unsigned long long clen = 0;
    yh_hdr cr;
    int total = 0, start = 0;
    int got = 0;
    int ret;

    if (s->cancelled) return -1;
    if (s->size > 0 && off >= s->size) {
        s->eof = 1;
        return 0;
    }

    tmpl = sceHttpCreateTemplate(YHTTP_USER_AGENT, SCE_HTTP_VERSION_1_1,
                                 SCE_HTTP_PROXY_AUTO);
    if (tmpl < 0) { s->err = tmpl; return tmpl; }
    conn = sceHttpCreateConnectionWithURL(tmpl, s->url, 1);
    if (conn < 0) { s->err = conn; goto done; }
    req = sceHttpCreateRequestWithURL(conn, SCE_HTTP_METHOD_GET, s->url, 0);
    if (req < 0) { s->err = req; goto done; }

    sceHttpSetAutoRedirect(req, 1);
    sceHttpSetResolveTimeOut(req, YHTTP_RESOLVE_TIMEOUT_US);
    sceHttpSetConnectTimeOut(req, YHTTP_CONNECT_TIMEOUT_US);
    sceHttpSetRecvTimeOut(req, YHTTP_RECV_TIMEOUT_US);
    sceHttpSetResponseHeaderMaxSize(req, YHTTP_HEADER_MAX);
    snprintf(range, sizeof range, "bytes=%lld-%lld", off,
             off + (long long)want - 1);
    sceHttpAddRequestHeader(req, "Range", range, SCE_HTTP_HEADER_ADD);
    if (s->referer && *s->referer)
        sceHttpAddRequestHeader(req, "Referer", s->referer, SCE_HTTP_HEADER_ADD);

    ret = sceHttpSendRequest(req, NULL, 0);
    if (ret < 0) { s->err = ret; goto done; }
    ret = sceHttpGetStatusCode(req, &total);
    if (ret < 0) { s->err = ret; goto done; }
    s->last_status = total;
    /*
     * 416 = 请求的区间落在文件末尾之外。这不是错误，是"到尾了"：
     * 解码器（mpg123 打开时会 seek 到很远问长度）完全可能问到一个合法的
     * 越界偏移，旧版本把它当成硬错误写进 s->err，之后每一次读都失败，
     * 整首歌就再也放不出来了。
     */
    if (total == 416) {
        s->eof = 1;
        s->win_start = off;
        s->win_len = 0;
        return 0;
    }
    if (total != 200 && total != 206) {
        s->err = -1;
        yh_logf("yhttp: stream status=%d (需要 206/200) off=%lld\n", total, off);
        goto done;
    }
    if (sceHttpGetResponseContentLength(req, &clen) < 0) clen = 0;

    /* Content-Range 告诉我们这段是从哪开始的、全长多少（§19）。 */
    if (sceHttpGetAllResponseHeaders(req, &headers, &headers_size) >= 0 &&
        headers) {
        cr = yh_header_find(headers, headers_size, "Content-Range");
        if (cr.ptr && cr.len > 0) {
            char tmp[64];
            int n = cr.len < (int)sizeof tmp - 1 ? cr.len : (int)sizeof tmp - 1;
            unsigned long long a = 0, b = 0, t = 0;
            memcpy(tmp, cr.ptr, (size_t)n);
            tmp[n] = 0;
            if (sscanf(tmp, "bytes %llu-%llu/%llu", &a, &b, &t) >= 2) {
                start = (int)a;
                s->size = (long long)t;
            }
        }
    }
    /* 服务器无视 Range（200）：这一坨是从 0 开始的整段流。 */
    if (total == 200 && off > 0) {
        start = 0;
        yh_logf("yhttp: stream 服务器忽略 Range，退回整段读\n");
    }

    {
        int cap = s->win_cap;
        if (clen > 0 && (long long)clen < (long long)cap) cap = (int)clen;
        while (got < cap) {
            ret = sceHttpReadData(req, s->win + got, (unsigned int)(cap - got));
            if (ret == 0) break;
            if (ret < 0) { s->err = ret; goto done; }
            got += ret;
        }
    }
    s->win_start = (long long)start;
    s->win_len = got;
    if (got == 0) s->eof = 1;
    else if (s->size > 0 && (long long)start + got >= s->size) s->eof = 1;
    yh_logf("yhttp: stream 窗口 %lld..%lld（%d 字节，总长 %lld）\n",
            s->win_start, s->win_start + got, got, s->size);

done:
    if (req >= 0) sceHttpDeleteRequest(req);
    if (conn >= 0) sceHttpDeleteConnection(conn);
    if (tmpl >= 0) sceHttpDeleteTemplate(tmpl);
    return s->err;
}

yhttp_stream *yhttp_stream_open(const char *url, const char *referer,
                                int tls_mode, long long *size_out, int *err_out) {
    yhttp_stream *s;
    if (!url || !*url) return NULL;
    if (yhttp_init() < 0) {
        if (err_out) *err_out = -1;
        return NULL;
    }
    if (tls_mode == YHTTP_TLS_VERIFY) yh_load_system_ca();

    s = (yhttp_stream *)calloc(1, sizeof *s);
    if (!s) return NULL;
    s->win_cap = YHTTP_WINDOW;
    s->win = (unsigned char *)malloc((size_t)s->win_cap);
    s->url = (char *)malloc(strlen(url) + 1);
    s->referer = (char *)malloc(referer ? strlen(referer) + 1 : 1);
    if (!s->win || !s->url || !s->referer) {
        yhttp_stream_close(s);
        return NULL;
    }
    strcpy(s->url, url);
    if (referer) strcpy(s->referer, referer);
    else s->referer[0] = 0;
    s->tls_mode = tls_mode;
    s->size = -1;

    /* 第一次取窗口：顺带知道总长度（Content-Range 的 "/total"）。 */
    if (yh_stream_fetch(s, 0, s->win_cap) < 0) {
        if (err_out) *err_out = s->err;
        yhttp_stream_close(s);
        return NULL;
    }
    /* 到这里这条流才开始"占着网络栈"（yhttp_load_ca 靠这个计数决定能不能重建栈）。 */
    s->opened = 1;
    g_streams_open++;
    if (size_out) *size_out = s->size;
    if (err_out) *err_out = 0;
    return s;
}

long long yhttp_stream_read(yhttp_stream *s, long long off, void *dst,
                            long long n) {
    long long done = 0;
    unsigned char *out = (unsigned char *)dst;
    if (!s || !dst || n <= 0) return -1;
    if (s->cancelled) return -1;
    if (s->size > 0 && off >= s->size) return 0; /* 真正结束 */

    while (done < n) {
        long long want = off + done;
        /* 命中窗口就直接拷，不命中就把窗口挪过去（一次 Range 请求）。 */
        if (want < s->win_start || want >= s->win_start + s->win_len) {
            int attempt;
            if (s->eof && s->win_len == 0) break;
            /*
             * 取窗口允许重试：一次连接抖动不该让整首歌判死。
             * s->err 只表示"最近一次失败"，成功后立刻清掉，不再是永久粘住的状态。
             */
            for (attempt = 0; attempt < YHTTP_FETCH_TRIES; attempt++) {
                s->err = 0;
                if (yh_stream_fetch(s, want, s->win_cap) >= 0) break;
                if (s->cancelled) break;
                yh_logf("yhttp: stream 取窗口失败 0x%08X（%d/%d），稍后重试\n",
                        (unsigned)s->err, attempt + 1, YHTTP_FETCH_TRIES);
                sceKernelDelayThread(YHTTP_RETRY_DELAY_US);
            }
            if (s->cancelled) return -1;
            if (s->err) {
                yh_logf("yhttp: stream 放弃 off=%lld 错误 0x%08X\n", want,
                        (unsigned)s->err);
                return done > 0 ? done : s->err;
            }
            if (s->win_len == 0) break;
            if (want < s->win_start || want >= s->win_start + s->win_len) {
                yh_logf("yhttp: stream 窗口没盖住请求偏移 %lld\n", want);
                s->err = -1;
                return done > 0 ? done : -1;
            }
        }
        {
            long long avail = s->win_start + s->win_len - want;
            long long take = n - done;
            if (take > avail) take = avail;
            if (take <= 0) break;
            memcpy(out + done, s->win + (want - s->win_start), (size_t)take);
            done += take;
        }
    }
    return done; /* 0 = 真正结束 */
}

void yhttp_stream_cancel(yhttp_stream *s) {
    if (s) s->cancelled = 1;
}

void yhttp_stream_close(yhttp_stream *s) {
    if (!s) return;
    yhttp_stream_cancel(s);
    if (s->opened) {
        s->opened = 0;
        if (g_streams_open > 0) g_streams_open--;
    }
    free(s->win);
    free(s->url);
    free(s->referer);
    free(s);
}

int yhttp_stream_error(const yhttp_stream *s) { return s ? s->err : -1; }

/* ---------------------------------------------------------------- 取消 -- */

typedef struct {
    int req;
    volatile int started;   /* SendRequest returned */
    volatile int stopped;   /* the read loop exited */
    volatile int bytes;     /* how much the worker has transferred */
    int send_rc;
    int read_rc;
} yh_abort_ctx;

/*
 * 故意用静态变量：sceKernelStartThread() 会把参数**拷贝**到新线程的栈上，
 * 以前传栈上结构体的地址，等于给 worker 发了一份副本，而主线程盯的是原件
 * （永远全是 0 —— 这就是前两次取消测试一直报 bytes_in_flight=0 的原因）。
 */
static yh_abort_ctx g_abort;

static int yh_abort_worker(unsigned int args, void *argp) {
    /* sceKernelStartThread() 会把参数拷贝到新线程栈上，所以这里故意不传参数、
     * 共用一个静态结构体；以前在这里解引用 `argp` 就是往 NULL 写。 */
    yh_abort_ctx *ctx = &g_abort;
    unsigned char scratch[4096];
    (void)args;
    (void)argp;
    ctx->send_rc = sceHttpSendRequest(ctx->req, NULL, 0);
    if (ctx->send_rc >= 0) {
        ctx->started = 1;
        /* 用小片持续读：取消必须落在"真的在传数据"的过程中，而不是两次请求之间。 */
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
     * 先等到"真的在传数据"（SendRequest 已返回**并且**已收到一些字节），再取消，
     * 然后量被阻塞的读多久放弃。第一版只是等固定时间，那测的其实是 worker 整个
     * 循环有没有跑完（§24 要的不是这个）。
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
    (void)wait_ms;                  /* 保留参数只为接口兼容 */

    t0 = sceKernelGetSystemTimeWide();
    ret = sceHttpAbortRequest(req);
    {
        unsigned int spin = 0;
        while (!g_abort.stopped && spin < 5000) {   /* 最多等 5 秒 */
            sceKernelDelayThread(1000);
            spin++;
        }
    }
    t1 = sceKernelGetSystemTimeWide();

    res->abort_took_ms = (unsigned int)((t1 - t0) / 1000);
    res->aborted = g_abort.stopped ? 1 : 0;
    if (ret < 0) { res->err_code = ret; res->err_at = 5; }
    else res->err_code = g_abort.read_rc;   /* 预期是取消相关的错误码 */
    yh_logf("yhttp: abort -> rc=0x%08X, stopped=%d, bytes_in_flight=%d, "
            "send=0x%08X read=0x%08X, stopped in %u ms\n",
            (unsigned)ret, g_abort.stopped, g_abort.bytes,
            (unsigned)g_abort.send_rc, (unsigned)g_abort.read_rc,
            res->abort_took_ms);
    sceKernelWaitThreadEnd(thid, NULL, NULL);
    sceKernelDeleteThread(thid);

done:
    if (!g_abort.stopped) {
        /* worker 还在读这个请求，删掉它等于抽掉对方脚下的地板。
         * 泄漏一个请求好过把整个应用带崩，下次探针会重新建。 */
        yh_logf("yhttp: abort worker never stopped; leaving request %d alive\n",
                req);
        return res->err_code;
    }
    if (req >= 0) sceHttpDeleteRequest(req);
    if (conn >= 0) sceHttpDeleteConnection(conn);
    if (tmpl >= 0) sceHttpDeleteTemplate(tmpl);
    return res->err_code;
}

#else /* 电脑上的构建：探针只在 Vita 上有效，这里留桩让源码可链接 */

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
yhttp_stream *yhttp_stream_open(const char *url, const char *referer,
                                int tls_mode, long long *size_out,
                                int *err_out) {
    (void)url; (void)referer; (void)tls_mode;
    if (size_out) *size_out = -1;
    if (err_out) *err_out = -1;
    return NULL;
}
long long yhttp_stream_read(yhttp_stream *s, long long off, void *dst,
                            long long n) {
    (void)s; (void)off; (void)dst; (void)n;
    return -1;
}
void yhttp_stream_cancel(yhttp_stream *s) { (void)s; }
void yhttp_stream_close(yhttp_stream *s) { (void)s; }
int yhttp_stream_error(const yhttp_stream *s) { (void)s; return -1; }

#endif /* __vita__ */
