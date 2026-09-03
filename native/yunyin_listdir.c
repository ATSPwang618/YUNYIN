/*
 * yunyin_listdir.c
 *
 * Directory listing for Yunyin, implemented directly on SceIo the same way
 * mature Vita homebrew does it (ElevenMPV / most apps use sceIoDopen/Dread).
 * Rust's std::fs::read_dir is unreliable on the real hardware for device
 * paths, so we enumerate directories ourselves and build the JSON array the
 * JS side already expects.
 *
 * We also try the path in a couple of accepted spellings (as-is, trailing
 * slash, and "device:/path") because the real SceIo is stricter than Vita3K
 * about the mount form, and we log which form worked along with the result.
 */
#include <psp2/io/dirent.h>
#include <psp2/io/fcntl.h>
#include <psp2/io/stat.h>

#include <stddef.h>
#include <stdio.h>
#include <string.h>

#ifndef SCE_SEEK_END
#define SCE_SEEK_END 2
#endif

static void yunyin_log(const char *msg) {
    SceUID f = sceIoOpen("ux0:data/yunyin.log",
                         SCE_O_WRONLY | SCE_O_CREAT, 0777);
    if (f >= 0) {
        sceIoLseek(f, 0, SCE_SEEK_END);
        sceIoWrite(f, msg, strlen(msg));
        sceIoClose(f);
    }
}

/* Append a JSON-escaped string `s` to buf (at *off). Returns 0 on success. */
static int append_json_str(char *buf, int cap, size_t *off, const char *s) {
    const unsigned char *p = (const unsigned char *)s;
    for (; *p; p++) {
        char tmp[8];
        int t = 0;
        unsigned char c = *p;
        if (c == '"' || c == '\\') {
            tmp[t++] = '\\';
            tmp[t++] = (char)c;
        } else if (c == '\n') {
            tmp[t++] = '\\';
            tmp[t++] = 'n';
        } else if (c == '\r') {
            tmp[t++] = '\\';
            tmp[t++] = 'r';
        } else if (c == '\t') {
            tmp[t++] = '\\';
            tmp[t++] = 't';
        } else if (c < 0x20) {
            t = snprintf(tmp, sizeof tmp, "\\u%04x", c);
        } else {
            tmp[t++] = (char)c;
        }
        if (*off + (size_t)t >= (size_t)cap) {
            return -1;
        }
        memcpy(buf + *off, tmp, (size_t)t);
        *off += (size_t)t;
    }
    return 0;
}

/* Fill `out` with a JSON array describing the directory at `path`.
 * Returns the number of bytes written (excluding the NUL) or -1 on error. */
int yunyin_list_dir(const char *path, char *out, int out_cap) {
    char c[8][512];
    const char *cands[8];
    int nc = 0;

    /* as-is */
    {
        int i = nc;
        snprintf(c[i], sizeof c[i], "%s", path);
        cands[nc++] = c[i];
    }

    size_t plen = strlen(path);
    if (plen && path[plen - 1] != '/') {
        int i = nc;
        snprintf(c[i], sizeof c[i], "%s/", path);
        cands[nc++] = c[i];
    }

    const char *colon = strchr(path, ':');
    if (colon) {
        int d = (int)(colon - path);
        const char *rest = colon + 1;
        if (*rest == '/') {
            /* device:/music -> also try device:music (canonical form).
             * The Vita's system folders (music/picture/video/app) are hidden
             * from readdir of the parent, but can often be opened directly
             * via the device:path spelling without the slash. */
            if (rest[1]) {
                int i = nc;
                snprintf(c[i], sizeof c[i], "%.*s:%s", d, path, rest + 1);
                cands[nc++] = c[i];
                i = nc;
                snprintf(c[i], sizeof c[i], "%.*s:%s/", d, path, rest + 1);
                cands[nc++] = c[i];
            }
        } else if (*rest) {
            int i = nc;
            snprintf(c[i], sizeof c[i], "%.*s:/%s", d, path, rest);
            cands[nc++] = c[i];
        }
    }

    size_t off = 0;
    out[off++] = '[';
    SceUID dir;
    const char *chosen = NULL;
    unsigned int count = 0;

    for (int i = 0; i < nc; i++) {
        dir = sceIoDopen(cands[i]);
        if (dir < 0) {
            continue;
        }
        chosen = cands[i];
        int first = 1;
        SceIoDirent ent;
        while (sceIoDread(dir, &ent) > 0) {
            const char *name = ent.d_name;
            if (name[0] == '.') {
                continue;
            }
            if (count >= 240) {
                break;
            }
            int isdir = SCE_S_ISDIR(ent.d_stat.st_mode);

            size_t chosen_len = strlen(chosen);
            const char *sep = (chosen[chosen_len - 1] == '/') ? "" : "/";
            char p[512];
            snprintf(p, sizeof p, "%s%s%s", chosen, sep, name);

            if (!first && off + 1 < (size_t)out_cap) {
                out[off++] = ',';
            }
            first = 0;

            if (off + 1 < (size_t)out_cap) {
                out[off++] = '{';
            }
            if (off + 8 < (size_t)out_cap) {
                memcpy(out + off, "\"name\":\"", 8);
                off += 8;
            }
            if (append_json_str(out, out_cap, &off, name) == 0 &&
                off + 3 < (size_t)out_cap) {
                memcpy(out + off, "\",", 2);
                off += 2;
            }
            if (off + 8 < (size_t)out_cap) {
                memcpy(out + off, "\"path\":\"", 8);
                off += 8;
            }
            if (append_json_str(out, out_cap, &off, p) == 0 &&
                off + 3 < (size_t)out_cap) {
                memcpy(out + off, "\",", 2);
                off += 2;
            }
            {
                char tmp[64];
                int t = snprintf(tmp, sizeof tmp, "\"dir\":%s,\"size\":%lld}",
                                 isdir ? "true" : "false",
                                 (long long)ent.d_stat.st_size);
                if (off + (size_t)t < (size_t)out_cap) {
                    memcpy(out + off, tmp, (size_t)t);
                    off += (size_t)t;
                }
            }
            count++;
        }
        sceIoDclose(dir);
        break;
    }

    if (off + 1 < (size_t)out_cap) {
        out[off++] = ']';
    }
    if (off < (size_t)out_cap) {
        out[off] = 0;
    }

    if (chosen == NULL) {
        char msg[512];
        snprintf(msg, sizeof msg, "YUNYIN_LIST %s -> ERR\n", path);
        yunyin_log(msg);
    } else {
        char msg[512];
        snprintf(msg, sizeof msg, "YUNYIN_LIST %s (via %s) -> %u entries\n",
                 path, chosen, count);
        yunyin_log(msg);
    }

    return (int)off;
}
