#define STB_IMAGE_IMPLEMENTATION
#define STBI_NO_STDIO
#define STBI_NO_HDR
#define STBI_NO_LINEAR
#define STBI_ONLY_JPEG
#define STBI_ONLY_PNG
#define STBI_NO_THREAD_LOCALS
#include "stb_image.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

int yunyin_image_decode(const uint8_t *data, int len, uint8_t **rgba, int *w, int *h) {
  if (!data || len <= 0) return -1;
  int cw = 0, ch = 0, comp = 0;
  unsigned char *px = stbi_load_from_memory(data, len, &cw, &ch, &comp, 4);
  if (!px || cw <= 0 || ch <= 0) {
    if (px) stbi_image_free(px);
    return -1;
  }
  *rgba = px;
  *w = cw;
  *h = ch;
  return 0;
}

void yunyin_image_free(uint8_t *p) {
  if (p) stbi_image_free(p);
}

int yunyin_image_resize(const uint8_t *src, int sw, int sh, uint8_t *dst, int dw, int dh) {
  if (!src || !dst || sw <= 0 || sh <= 0 || dw <= 0 || dh <= 0) return -1;
  for (int y = 0; y < dh; y++) {
    int sy = y * sh / dh;
    if (sy >= sh) sy = sh - 1;
    for (int x = 0; x < dw; x++) {
      int sx = x * sw / dw;
      if (sx >= sw) sx = sw - 1;
      const uint8_t *s = src + ((size_t)sy * (size_t)sw + (size_t)sx) * 4;
      uint8_t *d = dst + ((size_t)y * (size_t)dw + (size_t)x) * 4;
      d[0] = s[0];
      d[1] = s[1];
      d[2] = s[2];
      d[3] = s[3];
    }
  }
  return 0;
}
