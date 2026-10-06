/* 二维码：把登录 URL 变成一张 GPU 贴图。
 *
 * 用 vendor/ 里的 MIT 单文件编码器（qrcode-generator，不引 npm 依赖）编出矩阵，
 * 在 JS 里铺成 RGBA 位图，走 PocketJS 的 uploadTexture + registerTexture，
 * 页面里用 <Image src={key}> 显示 —— 和专辑封面同一条通道。
 *
 * 二维码是登录页里唯一的动态 GPU 贴图；它不能沿用“每次刷新都生成一个
 * 新 key”的写法，否则每次刷新都会把旧句柄留在渲染器里。 */

import { getOps, registerTexture } from "@pocketjs/framework";
import qrcode from "../vendor/qrcode.js";

/* 贴图边长：2 的幂、≤512。二维码实际显示 124 逻辑像素，128 足够且更适合
 * Vita 的小显存/动态纹理路径。显示端是点采样，放大后仍保持黑白模块边界。 */
const TEX = 128;
/* 静区：规格要求 4 个模块宽，少了手机对不上焦。 */
const QUIET = 4;
let lastQrHandle = -1;

/** 把 `text` 编成二维码、上传成贴图并注册到 `key`；返回实际绘制边长（像素）。 */
export function uploadQrTexture(key: string, text: string): number {
  const ops = getOps();
  /* 先回收上一张二维码。native 侧会把 GPU 镜像放入安全的回收队列，
   * 不是在当前场景里直接销毁。 */
  if (lastQrHandle >= 0) {
    ops.freeTexture?.(lastQrHandle);
    lastQrHandle = -1;
  }

  const qr = qrcode(0, "L"); /* 0 = 自动挑版本；L 纠错同尺寸容量最大 */
  qr.addData(text);
  qr.make();

  const count = qr.getModuleCount();
  const modules = count + QUIET * 2;
  const scale = Math.max(1, Math.floor(TEX / modules));
  const drawn = modules * scale;
  const offset = Math.floor((TEX - drawn) / 2);

  const rgba = new Uint8Array(TEX * TEX * 4);
  rgba.fill(255); /* 白底（含静区） */

  for (let row = 0; row < count; row += 1) {
    for (let col = 0; col < count; col += 1) {
      if (!qr.isDark(row, col)) continue;
      const x0 = offset + (col + QUIET) * scale;
      const y0 = offset + (row + QUIET) * scale;
      for (let y = 0; y < scale; y += 1) {
        let at = ((y0 + y) * TEX + x0) * 4;
        for (let x = 0; x < scale; x += 1) {
          rgba[at] = 0;
          rgba[at + 1] = 0;
          rgba[at + 2] = 0;
          rgba[at + 3] = 255;
          at += 4;
        }
      }
    }
  }

  const handle = ops.uploadTexture(rgba, TEX, TEX, 3);
  if (handle < 0) throw new Error("二维码贴图上传失败");
  lastQrHandle = handle;
  registerTexture(key, handle);
  return drawn;
}
