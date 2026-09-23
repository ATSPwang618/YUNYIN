#ifndef YUNYIN_YHTTP_H
#define YUNYIN_YHTTP_H

/*
 * Phase 0 network probe — a very thin HTTP transport for the Vita.
 *
 * Responsibilities (task book §21): init, connection, request, headers, Range,
 * read, status, content length, abort, redirect, timeout, close.
 * Explicitly not here: NetEase, song ids, providers, decoders, cache, PCM.
 *
 * Everything is built on the console's own stack — SceNet + SceSsl + SceHttp —
 * with no third-party HTTP library (§20).  Phase 0 exists to prove on real
 * hardware that this stack can do: DNS, TLS handshake, certificate validation,
 * 302, Cookie, Referer, Range, 206 + Content-Range, and abort (§26).
 *
 * The C side never writes files: it reports through a log sink the Rust host
 * installs, which keeps the evidence in one place (ux0:data/yunyin-netprobe.log).
 */

#ifdef __cplusplus
extern "C" {
#endif

/* TLS policy.  `DEFAULT` is what a media CDN usually needs; `VERIFY` loads the
 * console's own CA store first, and is the mode a shipping build must use. */
#define YHTTP_TLS_DEFAULT 0
#define YHTTP_TLS_VERIFY  1

/* Pool sizes handed to the console libraries.  Deliberately small and reported
 * in the log: Phase 0 measures, Phase 2 tunes (§68). */
#define YHTTP_NET_POOL   (128 * 1024)
#define YHTTP_SSL_POOL   (256 * 1024)
#define YHTTP_HTTP_POOL  (256 * 1024)

typedef struct {
    /* --- request outcome --- */
    int  status;          /* HTTP status code, 0 when no response was seen */
    int  tls_mode;        /* YHTTP_TLS_* used for this attempt */
    int  err_code;        /* first negative Vita error, 0 when none */
    int  err_at;          /* 0=init 1=create 2=send 3=status 4=read 5=abort */
    int  ssl_error;       /* sceHttpsGetSslError: errNum */
    unsigned int ssl_detail;

    /* --- response shape --- */
    long long content_length;      /* Content-Length, -1 when absent */
    unsigned long long range_total; /* Content-Range "/total", 0 when absent */
    long long range_start;         /* Content-Range "bytes X-Y/Z" -> X, -1 none */
    long long range_end;           /* ... -> Y */
    int  redirected;               /* 1 when at least one redirect was followed */
    int  ca_loaded;                /* 1 when the console CA store was loaded */
    int  verify_flags;             /* sceHttpsEnableOption() result for verify mode */
    unsigned int http_pool;        /* SceHttp pool that worked (bytes) */
    unsigned int ssl_pool;         /* SceSsl pool in use (bytes) */
    int  headers_len;
    int  content_type_audio;       /* 1 when Content-Type is an audio type */

    /* --- transfer --- */
    int  bytes_read;
    unsigned int took_ms;          /* from SendRequest to last read */
    unsigned int abort_took_ms;    /* §24: how fast abort actually returned */
    int  aborted;                  /* 1 when the abort test cancelled in flight */
} yhttp_result;

/* Log sink installed by the Rust host (`yunyin_net_log`). */
typedef void (*yhttp_log_fn)(const char *line, unsigned int len);
void yhttp_set_log(yhttp_log_fn fn);

/* Loads SceNet/SceSsl/SceHttp and their sysmodules.  0 on success. */
int  yhttp_init(void);
void yhttp_term(void);

/* 1 when netctl reports CONNECTED.  Prints the console's IP in the log. */
int  yhttp_online(void);

/* sceHttpGetMemoryPoolStats -> pool size / in use / peak.  0 on success. */
int  yhttp_memory(unsigned int *pool, unsigned int *in_use, unsigned int *peak);

/*
 * One GET, optionally with a Range/Referer/Cookie header.
 *
 * Reads up to `out_cap` bytes into `out` (may be NULL) and fills `*res`.
 * Returns 0 when a response was received (even an error status), or the first
 * negative Vita error code.
 */
int yhttp_probe(const char *url, const char *range, const char *referer,
                const char *cookie, int tls_mode, int auto_redirect,
                unsigned char *out, int out_cap, yhttp_result *res);

/*
 * Cancellation test (§24): start the request on a worker thread, call
 * sceHttpAbortRequest() after `wait_ms`, and report how quickly the blocked
 * transfer actually stopped.
 */
int yhttp_abort_probe(const char *url, const char *referer, int tls_mode,
                      unsigned int wait_ms, yhttp_result *res);

/*
 * Certificate store bring-up (§23/§27).
 *
 * Loading the console's 47 root certificates needs more room than the default
 * pool: on hardware `sceHttpsLoadCert` answered OUT_OF_MEMORY with a 256 KiB
 * pool.  This walks a small ladder of pool sizes, reports each attempt, and
 * remembers the pair that worked so Phase 2 can init that way from the start.
 */
int yhttp_load_ca(void);
unsigned int yhttp_ca_http_pool(void);
unsigned int yhttp_ca_ssl_pool(void);

#ifdef __cplusplus
}
#endif

#endif /* YUNYIN_YHTTP_H */
