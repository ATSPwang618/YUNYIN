#ifndef YUNYIN_YHTTP_H
#define YUNYIN_YHTTP_H

/*
 * Phase 0 网络探针 —— Vita 上的极薄 HTTP 传输层。
 *
 * 职责（任务书 §21）：初始化、建连、请求、请求头、Range、读取、状态码、
 * Content-Length、取消、重定向、超时、关闭。
 * 明确不属于这里：网易云、歌曲 ID、Provider、Decoder、Cache、PCM。
 *
 * 全部走机器自带的网络栈 —— SceNet + SceSsl + SceHttp，不引入第三方 HTTP 库（§20）。
 * Phase 0 的目的就是在真机上证明这套栈能做：DNS、TLS 握手、证书校验、302、
 * Cookie、Referer、Range、206 + Content-Range，以及取消（§26）。
 *
 * C 侧不写文件：它通过 Rust 宿主安装的日志出口上报，这样证据只落在一个地方
 * （ux0:data/yunyin-netprobe.log）。
 */

#ifdef __cplusplus
extern "C" {
#endif

/* TLS 策略。DEFAULT = 媒体 CDN 通常够用；VERIFY = 先打开校验开关（并尝试加载
 * 机器自带根证书），正式版应当用这个模式。 */
#define YHTTP_TLS_DEFAULT 0
#define YHTTP_TLS_VERIFY  1

/* 交给系统库的内存池大小。故意取小并写进日志：Phase 0 先测量，Phase 2 再调（§68）。 */
#define YHTTP_NET_POOL   (128 * 1024)
#define YHTTP_SSL_POOL   (256 * 1024)
#define YHTTP_HTTP_POOL  (256 * 1024)

typedef struct {
    /* --- 请求结果 --- */
    int  status;          /* HTTP 状态码；没拿到响应时为 0 */
    int  tls_mode;        /* 本次使用的 YHTTP_TLS_* */
    int  err_code;        /* 第一个负的 Vita 错误码；没有错误时为 0 */
    int  err_at;          /* 出错阶段：0=init 1=create 2=send 3=status 4=read 5=abort */
    int  ssl_error;       /* sceHttpsGetSslError 的 errNum */
    unsigned int ssl_detail;

    /* --- 响应形态 --- */
    long long content_length;       /* Content-Length；没有则为 -1 */
    unsigned long long range_total; /* Content-Range 里的总长度；没有则为 0 */
    long long range_start;          /* Content-Range "bytes X-Y/Z" 的 X；没有则 -1 */
    long long range_end;            /* 同上，Y */
    int  redirected;                /* 是否跟随过重定向（1 = 是） */
    int  ca_loaded;                 /* 机器自带根证书是否加载成功（1 = 成功） */
    int  verify_flags;              /* verify 模式下 sceHttpsEnableOption() 的返回值 */
    unsigned int http_pool;         /* 实际生效的 SceHttp 池大小（字节） */
    unsigned int ssl_pool;          /* 实际生效的 SceSsl 池大小（字节） */
    int  headers_len;
    int  content_type_audio;        /* Content-Type 是音频类型时为 1 */

    /* --- 传输 --- */
    int  bytes_read;
    unsigned int took_ms;           /* 从 SendRequest 到读完的耗时 */
    unsigned int abort_took_ms;     /* §24：取消后多久真正停下 */
    int  aborted;                   /* 取消测试是否成功打断进行中的传输 */
} yhttp_result;

/* 日志出口，由 Rust 宿主安装（`yunyin_net_log`）。 */
typedef void (*yhttp_log_fn)(const char *line, unsigned int len);
void yhttp_set_log(yhttp_log_fn fn);

/* 加载 SceNet/SceSsl/SceHttp 及其系统模块；成功返回 0。 */
int  yhttp_init(void);
void yhttp_term(void);

/* netctl 报告已连接时返回 1，并把主机 IP 写进日志。 */
int  yhttp_online(void);

/* 取内存池用量：总大小 / 当前 / 峰值；成功返回 0。 */
int  yhttp_memory(unsigned int *pool, unsigned int *in_use, unsigned int *peak);

/*
 * 发一次 GET，可选带 Range / Referer / Cookie 头。
 *
 * 最多读 `out_cap` 字节到 `out`（可为 NULL），并填充 `*res`。
 * 拿到响应（哪怕是错误状态码）返回 0，否则返回第一个负的 Vita 错误码。
 */
int yhttp_probe(const char *url, const char *range, const char *referer,
                const char *cookie, int tls_mode, int auto_redirect,
                unsigned char *out, int out_cap, yhttp_result *res);

/*
 * 一次 POST，给网易云 weapi 用。不走流式窗口，也不装载根证书
 * （装载会拆掉正在用的网络栈）。
 *
 * 成功拿到响应时返回读到的字节数（>=0），并把 HTTP 状态码写进 *status_out。
 * 连接失败返回负的 Vita 错误码。`body` 按 body_len 发送，不必以 NUL 结尾之外
 * 还保证里面没有提前的 0（表单是 ASCII）。
 */
int yhttp_post(const char *url, const char *body, int body_len,
               const char *referer, const char *cookie,
               unsigned char *out, int out_cap, int *status_out,
               char *set_cookie, int set_cookie_cap);

/*
 * 取消测试（§24）：在工作线程里发起请求，主线程等它真的在传数据之后调用
 * sceHttpAbortRequest()，并报告被阻塞的传输究竟多快停下。
 */
int yhttp_abort_probe(const char *url, const char *referer, int tls_mode,
                      unsigned int wait_ms, yhttp_result *res);

/*
 * 根证书库的装载（§23/§27）。
 *
 * 这台机器上 `sceHttpsLoadCert`（注册 47 张额外根证书）在任何池大小下都返回
 * OUT_OF_MEMORY，实测与默认证书校验无关（默认校验本来就是开着的）。这个函数
 * 逐档放大池子重试并记录结果，方便 Phase 2 一次性把参数定好。
 */
int yhttp_load_ca(void);
unsigned int yhttp_ca_http_pool(void);
unsigned int yhttp_ca_ssl_pool(void);

/* ------------------------------------------------------------ 流式读取 -- */
/*
 * Phase 2：把 HTTP 当"可随机读取的文件"来用（任务书 §17-§19）。
 *
 * 内部按 Range 维护一个窗口（默认 256 KiB）：落在窗口里的读直接命中，
 * 窗口外的读发一次新的 Range 请求把窗口挪过去。这样解码器"读一次发一次请求"
 * 的情况不会发生 —— 平均每个窗口只发一次。
 *
 * 与 yp_io 的约定一致：read 返回 0 只代表**流真的结束**；出错返回负值。
 * 取消（切歌/退出）用 yhttp_stream_cancel()，之后所有 read 立刻返回负数。
 */

typedef struct yhttp_stream yhttp_stream;

#define YHTTP_WINDOW (256 * 1024)

/*
 * 打开流并取到头部窗口（顺带拿到总长度）。
 * `size_out` 可为 NULL；拿到 Content-Range/Length 时写入总字节数（未知为 -1）。
 * 失败返回 NULL，*err_out 写入 Vita 错误码（可为 NULL）。
 */
yhttp_stream *yhttp_stream_open(const char *url, const char *referer,
                                int tls_mode, long long *size_out,
                                int *err_out);

/* 从绝对偏移 off 读最多 n 字节；返回读到的字节数，0 = 结束，负数 = 错误。 */
long long yhttp_stream_read(yhttp_stream *s, long long off, void *dst,
                            long long n);

/* 取消：正在进行的请求会被打断，之后的读一律失败（切歌、退出时调用）。 */
void yhttp_stream_cancel(yhttp_stream *s);

/* 释放（内部会先取消）。 */
void yhttp_stream_close(yhttp_stream *s);

/* 最近一次失败的错误码（0 = 没有）。 */
int yhttp_stream_error(const yhttp_stream *s);

/* ------------------------------------------------------------ 日志服务 -- */
/*
 * 在后台线程开一个极小的 HTTP 服务，把 `path` 这个文件原样回给任何来访者。
 * 真机排障时电脑上一条命令就能取日志，不用再手动拷：
 *
 *     curl http://<Vita 的 IP>:1337/ -o yunyin.log
 *
 * 只在日志开关打开时启动；失败只写一行日志，绝不影响播放。
 * 成功返回 0，失败返回负值。
 */
int yhttp_logserve_start(const char *path, unsigned short port);

#ifdef __cplusplus
}
#endif

#endif /* YUNYIN_YHTTP_H */
