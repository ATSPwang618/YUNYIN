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
#include <psp2/io/fcntl.h>
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

/* 随包发的根证书放在 app0:/certs/（打包脚本从仓库 certs/ 拷进去）。
 * 上限 16 KB：一张 X.509 根证书 PEM ≈ 1.4 KB，DER ≈ 0.9 KB，够用了。 */
#define YHTTP_CA_DIR "app0:/certs"
#define YHTTP_CA_MAX (16 * 1024)

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
/*
 * 正在飞的 POST / 探针请求数量。
 *
 * 为什么必须有它：网络栈重建（见 yhttp_net_reset）会 sceHttpTerm 掉整个栈，
 * 如果此时**别的线程**正拿着一个 request/connection 对象（yhttp_post / yhttp_probe 里），
 * 那些对象当场失效 —— 下一次内部调用就会跳到空指针上。
 * 真机现场就是 `Stop reason: Prefetch abort, PC: 0x0`（一进应用就崩），
 * 而模拟器因为网络足够快、根本没触发重建，所以看起来没问题。
 * 现在：只要有一笔请求在飞，重建一律推迟（返回 -1），由调用方稍后重试。
 */
static volatile int g_inflight;
static void *g_net_pool;
static unsigned int g_ssl_pool = YHTTP_SSL_POOL;
static unsigned int g_http_pool = YHTTP_HTTP_POOL;
static int g_verify_flags;
/* verify 的几个开关是进程级设置，开一次就够；每次请求都调会刷日志、也多一次
 * 系统调用（登录页每秒轮询时特别明显）。 */
static int g_verify_done;

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

    /*
     * 尽力而为：把随包发的根证书注册进 SceSsl（失败只记日志，不影响后面请求）。
     * 放在这里而不是外面，是因为 sceHttps* 要求 HTTPS 系统模块已加载 + 栈已 Init。
     */
    yhttp_load_ca();

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

/*
 * 网络栈整体重建（解析器卡死的恢复路径）。
 *
 * 为什么需要：真机日志里出现过 DNS 解析器"拒服务"
 * （`0x80436009 SCE_HTTP_ERROR_RESOLVER_ESERVERREFUSED`）之后**所有**请求都失败 ——
 * 连二维码都取不回来，只有重启应用才恢复。这里把 SceNet/SceSsl/SceHttp 全关掉重开，
 * 让下一次请求拿到干净的解析器。
 *
 * 纪律：有在线流正拿着这套栈时**绝不能**重建（那会把正在播的歌打断），
 * 这种情况返回 -1，让调用方稍后再试。
 */
int yhttp_net_reset(void) {
    if (g_inflight > 0) {
        yh_logf("yhttp: 网络栈重建推迟（有 %d 笔请求在飞）\n", g_inflight);
        return -1;
    }
    if (g_streams_open > 0) {
        yh_logf("yhttp: 网络栈重建推迟（有 %d 个在线流在用）\n", g_streams_open);
        return -1;
    }
    yh_logf("yhttp: 重建网络栈（解析器恢复）\n");
    yhttp_term();
    g_init_lock = 0;
    return yhttp_init();
}

int yhttp_online(void) {
    SceNetCtlInfo info;
    int state = 0;
    /*
     * 查联网状态之前**必须先把网络栈初始化起来**。
     *
     * 真机现场（一进应用就 `Prefetch abort, PC: 0x0`）：启动日志停在"运行环境"那一行之前，
     * 而中间只执行了这一句 `sceNetCtlInetGetState()` —— 在 `sceNetCtlInit()` 之前调用它，
     * 固件里那条路径尚未就绪，间接调用直接跳到空指针。
     * `yhttp_init()` 内部有锁 + 幂等（已初始化会直接返回 0），这里先调一次最稳。
     */
    if (yhttp_init() < 0) return 0;
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
 * 证书校验相关的接口（§23）。
 *
 * 老版本这里是"拆栈 + 逐档放大池子重装固件根证书"，实测（真机 + 模拟器）在任何
 * 池大小下都是 0x80431022 OUT_OF_MEMORY，代价却是先把整个网络栈 sceHttpTerm() +
 * sceSslTerm() 拆掉 —— 并发在飞的请求全部作废，模拟器上重建之后 TLS 会一直
 * 0x80431075（"网络明明是好的，歌单一个都同步不了"）。那套动作已经删掉。
 *
 * 现在这条只做一件事：把**随包发的**根证书注册进 SceSsl（见下面的注释）。
 * 校验本身由 sceHttpsEnableOption() 的几个 flag 生效，注册只是让"这台机器缺根"
 * 不再成为问题；注册失败也只是少一条可信根，不影响原有行为。
 */
int yhttp_load_ca(void) {
    /*
     * 随包发一张根证书（app0:/certs/），开机注册进 SceSsl。
     *
     * 为什么值得做：SceSsl 默认只认**固件自带**的根证书库，那个库跟着系统版本
     * 走 —— 老机器、模拟器上可能缺新根。网易云整条链（music.163.com 和
     * *.music.126.net CDN）都挂在 **DigiCert Global Root G2** 下面，把这一张
     * 带上，就不用赌用户机器上的库全不全。
     *
     * 注意三件事：
     *   * 这是"**额外注册**"（sceHttpsLoadCert），不是替换固件那份 —— 成功只是多
     *     一条可信根，原来的行为不变；
     *   * 失败一律只记日志：Vita3K 里这个 API 是 UNIMPLEMENTED（返回 -1），真机上
     *     历史上报 0x80431022 OUT_OF_MEMORY；两者都不影响后面用固件根库继续跑；
     *   * PEM / DER 各试一次：SceHttpsData 只声明了 ptr+size，没写格式，两边都试
     *     是最省事的确定办法 —— 哪次成功会直接写进日志。
     */
    static unsigned char ca_buf[YHTTP_CA_MAX];
    static const struct {
        const char *file;
        const char *what;
    } cands[] = {
        { "digicert-global-root-g2.pem", "PEM" },
        { "digicert-global-root-g2.der", "DER" },
    };
    char path[64];
    unsigned int i;

    if (g_ca_tried) return g_ca_loaded ? 0 : -1;
    g_ca_tried = 1;

    for (i = 0; i < sizeof cands / sizeof cands[0]; i++) {
        SceUID fd;
        int n, rc;
        SceHttpsData data;
        const SceHttpsData *list[1];

        snprintf(path, sizeof path, "%s/%s", YHTTP_CA_DIR, cands[i].file);
        fd = sceIoOpen(path, SCE_O_RDONLY, 0);
        if (fd < 0) {
            yh_logf("yhttp: 内置根证书 %s 打不开（%s，0x%08X）\n",
                    cands[i].what, path, (unsigned)fd);
            continue;
        }
        n = sceIoRead(fd, ca_buf, sizeof ca_buf);
        sceIoClose(fd);
        if (n <= 0) {
            yh_logf("yhttp: 内置根证书 %s 读出来是空的（%s）\n", cands[i].what, path);
            continue;
        }
        data.ptr = (char *)ca_buf;
        data.size = (unsigned)n;
        list[0] = &data;
        rc = sceHttpsLoadCert(1, list, NULL, NULL);
        yh_logf("yhttp: 内置根证书 %s（%d 字节）sceHttpsLoadCert -> 0x%08X\n",
                cands[i].what, n, (unsigned)rc);
        if (rc >= 0) {
            g_ca_loaded = 1;
            yh_logf("yhttp: 根证书 = 固件库 + 内置 DigiCert Global Root G2\n");
            return 0;
        }
    }
    yh_logf("yhttp: 内置根证书没装上（这台机器不支持/内存不够），继续用固件根库\n");
    return -1;
}

/*
 * 关于"关掉证书校验"（sceHttpsDisableOption）：**故意不提供**。
 *
 * 模拟器上它返回 0x8043506B（关不掉，救不了任何东西）；真机上它会关掉
 * **进程级**校验，连后面带会话 Cookie 的账号接口也一起不校验 —— 一旦被劫持
 * 就是把登录凭证送出去。收益为零，所以这条路不做，也不留接口。
 */

unsigned int yhttp_ca_http_pool(void) { return g_http_pool; }
unsigned int yhttp_ca_ssl_pool(void) { return g_ssl_pool; }
int yhttp_inflight(void) { return g_inflight; }

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
    /*
     * 打开校验靠的是这些 flag（进程级，一次就够）。
     *
     * 这里**不再**调用 yhttp_load_ca()：那一步既从来没成功过，又要拆掉整个
     * 网络栈重建，会把并发在飞的请求和后续 TLS 一起搞坏（见上面的说明）。
     */
    if (!g_verify_done) {
        g_verify_flags = yh_enable_verify_flags();
        g_verify_done = 1;
    }
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

static int yhttp_probe_impl(const char *url, const char *range, const char *referer,
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

/*
 * Phase 3：POST 表单 + 读回 JSON 正文。
 *
 * 与 yhttp_probe 的差别只有三处：方法、请求体、Content-Type 头；超时与响应头
 * 上限沿用同一套常量。**不自动跟随重定向** —— API 调用要如实看到 3xx。
 */
/*
 * 把响应里所有 `Set-Cookie` 的 `name=value` 部分拼成一条 Cookie 头
 * （`MUSIC_U=…; __csrf=…`）。网易云登录成功后的 MUSIC_U 就在这里，
 * 后面所有需要登录的请求都靠它。
 */
static void yh_collect_cookies(const char *headers, unsigned int size,
                               char *out, int cap) {
    unsigned int i = 0;
    int used = 0;
    if (!out || cap <= 0) return;
    out[0] = 0;
    while (i < size) {
        unsigned int line_start = i, line_end = i, v, e, len;
        while (line_end < size && headers[line_end] != '\n' &&
               headers[line_end] != '\r')
            line_end++;
        len = line_end - line_start;
        if (len > 11 &&
            strncasecmp(headers + line_start, "Set-Cookie:", 11) == 0) {
            v = line_start + 11;
            while (v < line_end && (headers[v] == ' ' || headers[v] == '\t')) v++;
            e = v;
            while (e < line_end && headers[e] != ';') e++;
            if (e > v) {
                int n = (int)(e - v);
                if (used + n + 3 < cap) {
                    if (used) {
                        memcpy(out + used, "; ", 2);
                        used += 2;
                    }
                    memcpy(out + used, headers + v, (size_t)n);
                    used += n;
                    out[used] = 0;
                }
            }
        }
        if (line_end == i) break; /* no progress: malformed block */
        i = line_end;
        while (i < size && (headers[i] == '\r' || headers[i] == '\n')) i++;
    }
}

/*
 * 下载进度：给界面显示"正在同步歌单… 32%"用。
 *
 * 为什么需要：歌单详情是一整包几 MB 的 JSON，慢就慢在**收正文**这一段，
 * 而我们以前只报"已经等了 38 秒"。既然 Content-Length 就在响应头里，
 * 收到的字节数 / 总字节数就是一个真实百分比，不是猜的。
 */
static volatile int g_dl_active;
static volatile long long g_dl_got;
static volatile long long g_dl_total;

int yhttp_dl_active(void) { return g_dl_active; }
long long yhttp_dl_got(void) { return g_dl_got; }
long long yhttp_dl_total(void) { return g_dl_total; }

static int yhttp_post_impl(const char *url, const char *body, const char *content_type,
               const char *referer, const char *cookie, int tls_mode,
               unsigned char *out, int out_cap, int *status_out, int *len_out,
               char *set_cookie_out, int set_cookie_cap) {
    int tmpl = -1, conn = -1, req = -1;
    int ret = 0;
    const char *stage = "init";
    unsigned int body_len = body ? (unsigned int)strlen(body) : 0;

    if (status_out) *status_out = 0;
    if (len_out) *len_out = 0;
    if (set_cookie_out && set_cookie_cap > 0) set_cookie_out[0] = 0;
    if (!url || !*url) return -1;

    if (yhttp_init() < 0) return -1;
    if (tls_mode == YHTTP_TLS_VERIFY) yh_load_system_ca();

    tmpl = sceHttpCreateTemplate(YHTTP_USER_AGENT, SCE_HTTP_VERSION_1_1,
                                 SCE_HTTP_PROXY_AUTO);
    if (tmpl < 0) return tmpl;
    stage = "create-conn";
    conn = sceHttpCreateConnectionWithURL(tmpl, url, 1);
    if (conn < 0) { ret = conn; goto done; }
    stage = "create-req";
    req = sceHttpCreateRequestWithURL(conn, SCE_HTTP_METHOD_POST, url,
                                      (unsigned long long)body_len);
    if (req < 0) { ret = req; goto done; }

    /* 关掉栈自己的 cookie jar：网易云登录的会话标识（NMTID）在 Set-Cookie 里，
     * 真机实测开着 jar 时它会把这个头收走、响应头和 jar 两条路都拿不到；
     * 关掉之后它就是一个普通响应头，我们自己解析（cookie 头我们本来也自己发）。 */
    sceHttpSetCookieEnabled(req, 0);
    sceHttpSetAutoRedirect(req, 0);
    sceHttpSetResolveTimeOut(req, YHTTP_RESOLVE_TIMEOUT_US);
    sceHttpSetConnectTimeOut(req, YHTTP_CONNECT_TIMEOUT_US);
    sceHttpSetRecvTimeOut(req, YHTTP_RECV_TIMEOUT_US);
    sceHttpSetResponseHeaderMaxSize(req, YHTTP_HEADER_MAX);
    if (content_type && *content_type)
        sceHttpAddRequestHeader(req, "Content-Type", content_type,
                                SCE_HTTP_HEADER_OVERWRITE);
    if (referer && *referer)
        sceHttpAddRequestHeader(req, "Referer", referer, SCE_HTTP_HEADER_ADD);
    if (cookie && *cookie)
        sceHttpAddRequestHeader(req, "Cookie", cookie, SCE_HTTP_HEADER_ADD);

    stage = "send";
    ret = sceHttpSendRequest(req, body_len ? body : NULL, body_len);
    if (ret < 0) goto done;

    {
        int st = 0;
        stage = "status";
        ret = sceHttpGetStatusCode(req, &st);
        if (ret < 0) goto done;
        if (status_out) *status_out = st;
    }

    if (set_cookie_out && set_cookie_cap > 0) {
        char *headers = NULL;
        unsigned int headers_size = 0;
        if (sceHttpGetAllResponseHeaders(req, &headers, &headers_size) >= 0 && headers)
            yh_collect_cookies(headers, headers_size, set_cookie_out,
                               set_cookie_cap);
        if (set_cookie_out[0] == 0) {
            /* 响应头里没有：再试一次从 jar 导出（万一固件行为与预期不同）。 */
            unsigned int clen = (unsigned int)set_cookie_cap;
            if (sceHttpGetCookie(url, set_cookie_out, &clen, 1, 1) < 0)
                set_cookie_out[0] = 0;
            else
                yh_logf("yhttp: cookie jar -> %u 字节\n", clen);
        }
        if (set_cookie_out[0] == 0) {
            /* 诊断：头里到底有没有 cookie 字样（大小写无关找 "ookie"）。 */
            unsigned int k = 0;
            int seen = 0;
            if (headers && headers_size >= 5) {
                for (k = 0; k + 5 <= headers_size; k++) {
                    if (strncasecmp(headers + k, "ookie", 5) == 0) { seen = 1; break; }
                }
            }
            yh_logf("yhttp: 无 Set-Cookie（headers=%u 字节, 含 ookie=%d）\n",
                    headers_size, seen);
        }
    }

    if (out && out_cap > 0) {
        int want = out_cap - 1;   /* 留一个字节用来探"还有没有" */
        int got = 0;
        /* 正文开收之前先把进度归零，并把 Content-Length 记下来当分母。 */
        {
            unsigned long long clen = 0;
            g_dl_got = 0;
            g_dl_total = (sceHttpGetResponseContentLength(req, &clen) >= 0 &&
                          clen > 0)
                             ? (long long)clen
                             : -1;
            g_dl_active = 1;
        }
        stage = "read";
        while (got < want) {
            ret = sceHttpReadData(req, out + got, (unsigned int)(want - got));
            if (ret == 0) break;              /* 正文结束 */
            if (ret < 0) goto done;
            got += ret;
            g_dl_got = got;
        }
        if (got == want) {
            unsigned char probe;
            ret = sceHttpReadData(req, &probe, 1);
            if (ret < 0) goto done;
            if (ret > 0) { ret = -2; goto done; } /* 超出缓冲，别让调用方解析半截 */
        }
        out[got] = 0;
        if (len_out) *len_out = got;
    }
    ret = 0;

done:
    g_dl_active = 0;
    /* 大响应记一行：以后"这次到底慢在哪一段"有据可查（小于 64 KiB 不写）。 */
    if (ret >= 0 && g_dl_total >= 65536) {
        yh_logf("yhttp: 收完 %lld/%lld 字节 url=%s\n", g_dl_got, g_dl_total,
                url ? url : "");
    }
    /* 失败时把"哪一步失败"写清楚：以前只有错误码，排查全靠猜。 */
    if (ret < 0) {
        yh_logf("yhttp: POST 失败 stage=%s code=0x%08X url=%s\n", stage,
                (unsigned)ret, url ? url : "(null)");
    }
    /* `headers` 属于 SceHttp 的内存池：释放它会破坏那个池。 */
    if (req >= 0) sceHttpDeleteRequest(req);
    if (conn >= 0) sceHttpDeleteConnection(conn);
    if (tmpl >= 0) sceHttpDeleteTemplate(tmpl);
    return ret;
}

/* ------------------------------------------------------------ 流式读取 -- */

struct yhttp_stream {
    char *url;
    char *referer;
    /*
     * 开流时带的 Cookie（登录会话 MUSIC_U 等）。
     *
     * 为什么必须有：网易云给登录用户的播放地址里带 `authSecret=…`，
     * 那种地址的 CDN 会校 Cookie —— 不带就是 **403**（真机日志：
     * `yhttp: stream status=403`，表现成"切歌之后没声音还卡住"）。
     */
    char *cookie;
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
    /*
     * 复用的模板与连接。
     *
     * 一首歌要抓好多个 1 MiB 窗口，以前每个窗口都 CreateTemplate + CreateConnection
     * 再全删掉 —— 真机实测一次 Range 首包 1.8–2.7 秒，其中很大一块就是重复的
     * TCP/TLS 建连。现在整条流只建一次，每个窗口只新建/销毁 Request。
     * 抓取失败时这两个一起丢掉（下次重试重建），避免复用坏连接。
     */
    int tmpl;
    int conn;
    volatile int cancelled;
};

/* 一个窗口最多重试几次。网络抖动/连接被回收是常态，一次失败就判定"这条流坏了"
 * 会把整首歌判死（旧版本的 s->err 是永久粘住的）。 */
#define YHTTP_FETCH_TRIES 3
#define YHTTP_RETRY_DELAY_US (200 * 1000)

/* 丢掉这条流复用的模板/连接（失败时、关闭时）。 */
static void yh_stream_drop_conn(yhttp_stream *s) {
    if (s->conn >= 0) {
        sceHttpDeleteConnection(s->conn);
        s->conn = -1;
    }
    if (s->tmpl >= 0) {
        sceHttpDeleteTemplate(s->tmpl);
        s->tmpl = -1;
    }
}

/*
 * 抓一个窗口到 s->win（覆盖 [off, off+want)）。
 *
 * template / connection 挂在流对象上复用（见 yhttp_stream 里的注释），
 * 每次只新建/销毁 Request；抓取失败时把连接一起丢掉，下次重建。
 * 响应头按 headerSize 限界、库返回的指针只读不 free（探针那套已验证）。
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
        /*
         * 请求的位置已经在流末尾之外（解码器探测长度时会这么问）。
         * 这里必须把窗口一并清空：否则窗口还是上一次的旧内容，"请求的偏移
         * 没被窗口盖住"那条分支会把它当成错误（s->err=-1）抛回去 —— 解码器
         * 把负返回值当硬错误，就再也不回开头重读了。
         * 清空窗口 = "这个位置已经没有数据"，read() 会以短读正常收工。
         */
        s->eof = 1;
        s->win_start = off;
        s->win_len = 0;
        return 0;
    }

    /* 模板/连接只建一次，整条流复用（弱网首包的大头就是重复建连）。 */
    if (s->tmpl < 0) {
        s->tmpl = sceHttpCreateTemplate(YHTTP_USER_AGENT, SCE_HTTP_VERSION_1_1,
                                        SCE_HTTP_PROXY_AUTO);
        if (s->tmpl < 0) {
            int e = s->tmpl;
            s->tmpl = -1;
            s->err = e;
            return e;
        }
    }
    if (s->conn < 0) {
        s->conn = sceHttpCreateConnectionWithURL(s->tmpl, s->url, 1);
        if (s->conn < 0) {
            s->err = s->conn;
            s->conn = -1;
            goto done;
        }
    }
    conn = s->conn;
    tmpl = s->tmpl;
    req = sceHttpCreateRequestWithURL(conn, SCE_HTTP_METHOD_GET, s->url, 0);
    if (req < 0) { s->err = req; goto done; }

    sceHttpSetAutoRedirect(req, 1);
    /*
     * 关掉栈自己的 cookie jar —— 和 POST 那条路同一个原因：
     * 开着 jar 时 SceHttp 会把我们手写的 `Cookie` 头收走（POST 侧真机实测过），
     * 于是 CDN 看到的是"没带登录会话"，带 authSecret 的地址一律回 403。
     */
    sceHttpSetCookieEnabled(req, 0);
    sceHttpSetResolveTimeOut(req, YHTTP_RESOLVE_TIMEOUT_US);
    sceHttpSetConnectTimeOut(req, YHTTP_CONNECT_TIMEOUT_US);
    sceHttpSetRecvTimeOut(req, YHTTP_RECV_TIMEOUT_US);
    sceHttpSetResponseHeaderMaxSize(req, YHTTP_HEADER_MAX);
    snprintf(range, sizeof range, "bytes=%lld-%lld", off,
             off + (long long)want - 1);
    sceHttpAddRequestHeader(req, "Range", range, SCE_HTTP_HEADER_ADD);
    if (s->referer && *s->referer)
        sceHttpAddRequestHeader(req, "Referer", s->referer, SCE_HTTP_HEADER_ADD);
    if (s->cookie && *s->cookie)
        sceHttpAddRequestHeader(req, "Cookie", s->cookie, SCE_HTTP_HEADER_ADD);

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
    /*
     * 注意：s->eof 只是**这一个窗口**的说明，不是"整条流结束了"。
     * 解码器探测完文件尾巴以后一定会 seek 回开头，那时候当然还有数据。
     * 以前 read() 用 `eof && win_len == 0` 当"整条流到底"的门，结果探测过一次
     * 尾巴之后，连 off=0 的读都直接返回 0（假 EOF）—— 真机上"解码器打不开
     * 在线流"就是这么来的。现在这个标志只用来记日志，不参与任何判断。
     */
    s->eof = (got == 0) || (s->size > 0 && (long long)start + got >= s->size);
    yh_logf("yhttp: stream 窗口 %lld..%lld（%d 字节，总长 %lld）\n",
            s->win_start, s->win_start + got, got, s->size);

done:
    if (req >= 0) sceHttpDeleteRequest(req);
    /* 模板/连接整条流复用，这里只删 Request；抓取失败就把连接丢掉，下次重建。 */
    if (s->err != 0) yh_stream_drop_conn(s);
    return s->err;
}

yhttp_stream *yhttp_stream_open(const char *url, const char *referer,
                                const char *cookie, int tls_mode,
                                long long *size_out, int *err_out,
                                int *status_out) {
    yhttp_stream *s;
    if (status_out) *status_out = 0;
    if (!url || !*url) return NULL;
    if (yhttp_init() < 0) {
        if (err_out) *err_out = -1;
        return NULL;
    }
    if (tls_mode == YHTTP_TLS_VERIFY) yh_load_system_ca();

    s = (yhttp_stream *)calloc(1, sizeof *s);
    if (!s) return NULL;
    /* calloc 出来是 0，这两个要置 -1 才是"还没建"（0 是合法的句柄值）。 */
    s->tmpl = -1;
    s->conn = -1;
    s->win_cap = YHTTP_WINDOW;
    s->win = (unsigned char *)malloc((size_t)s->win_cap);
    s->url = (char *)malloc(strlen(url) + 1);
    s->referer = (char *)malloc(referer ? strlen(referer) + 1 : 1);
    s->cookie = (char *)malloc(cookie ? strlen(cookie) + 1 : 1);
    if (!s->win || !s->url || !s->referer || !s->cookie) {
        yhttp_stream_close(s);
        return NULL;
    }
    strcpy(s->url, url);
    if (referer) strcpy(s->referer, referer);
    else s->referer[0] = 0;
    if (cookie) strcpy(s->cookie, cookie);
    else s->cookie[0] = 0;
    s->tls_mode = tls_mode;
    s->size = -1;

    /* 第一次取窗口：顺带知道总长度（Content-Range 的 "/total"）。 */
    if (yh_stream_fetch(s, 0, s->win_cap) < 0) {
        if (err_out) *err_out = s->err;
        if (status_out) *status_out = s->last_status; /* 403/404/410 → 地址被拒 */
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
            /*
             * 这里**不能**用"之前到过流末尾"来提前收工：解码器探测完尾巴一定会
             * seek 回开头，那时候必须照常发新的 Range 请求。真正"到底了"的信号
             * 只有一条 —— 这次取回来的窗口是空的（下面那个 win_len == 0）。
             *
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
    yh_stream_drop_conn(s);
    free(s->win);
    free(s->url);
    free(s->referer);
    free(s->cookie);
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

/*
 * 对外入口：只做"在飞请求计数"，然后转发给实现。
 *
 * 为什么用包装而不是在每个 return 前手写减一：这两个函数里分支很多
 * （建连接失败 / 发请求失败 / 读失败 / 太长…），漏一处就会让计数器永远 >0，
 * 于是网络栈再也无法重建。包装一层就天然平衡。
 */
int yhttp_probe(const char *url, const char *range, const char *referer,
                const char *cookie, int tls_mode, int auto_redirect,
                unsigned char *out, int out_cap, yhttp_result *res) {
    g_inflight++;
    int rc = yhttp_probe_impl(url, range, referer, cookie, tls_mode,
                              auto_redirect, out, out_cap, res);
    g_inflight--;
    return rc;
}

int yhttp_post(const char *url, const char *body, const char *content_type,
               const char *referer, const char *cookie, int tls_mode,
               unsigned char *out, int out_cap, int *status_out, int *len_out,
               char *set_cookie_out, int set_cookie_cap) {
    g_inflight++;
    int rc = yhttp_post_impl(url, body, content_type, referer, cookie, tls_mode,
                             out, out_cap, status_out, len_out, set_cookie_out,
                             set_cookie_cap);
    g_inflight--;
    return rc;
}

#else /* 电脑上的构建：探针只在 Vita 上有效，这里留桩让源码可链接 */

void yhttp_set_log(yhttp_log_fn fn) { (void)fn; }
int yhttp_init(void) { return -1; }
void yhttp_term(void) {}
int yhttp_online(void) { return 0; }
int yhttp_net_reset(void) { return -1; }
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

int yhttp_post(const char *url, const char *body, const char *content_type,
               const char *referer, const char *cookie, int tls_mode,
               unsigned char *out, int out_cap, int *status_out, int *len_out,
               char *set_cookie_out, int set_cookie_cap) {
    (void)url; (void)body; (void)content_type; (void)referer; (void)cookie;
    (void)tls_mode; (void)out; (void)out_cap; (void)status_out; (void)len_out;
    (void)set_cookie_out; (void)set_cookie_cap;
    return -1;
}

int yhttp_load_ca(void) { return -1; }
int yhttp_inflight(void) { return 0; }
int yhttp_dl_active(void) { return 0; }
long long yhttp_dl_got(void) { return 0; }
long long yhttp_dl_total(void) { return -1; }
unsigned int yhttp_ca_http_pool(void) { return 0; }
unsigned int yhttp_ca_ssl_pool(void) { return 0; }
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
long long yhttp_stream_read(yhttp_stream *s, long long off, void *dst,
                            long long n) {
    (void)s; (void)off; (void)dst; (void)n;
    return -1;
}
void yhttp_stream_cancel(yhttp_stream *s) { (void)s; }
void yhttp_stream_close(yhttp_stream *s) { (void)s; }
int yhttp_stream_error(const yhttp_stream *s) { (void)s; return -1; }

#endif /* __vita__ */
