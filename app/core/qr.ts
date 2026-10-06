/* 二维码：把登录 URL 变成一张 GPU 贴图。
 *
 * 用 vendor/ 里的 MIT 单文件编码器（qrcode-generator，不引 npm 依赖）编出矩阵，
 * 在 JS 里铺成 RGBA 位图，走 PocketJS 的 uploadTexture + registerTexture，
 * 页面里用 <Image src={key}> 显示 —— 和专辑封面同一条通道。 */

import { getOps, registerTexture } from "@pocketjs/framework";
import qrcode from "../vendor/qrcode.js";

/* 贴图边长：2 的幂、≤512。256 在 480×272 的逻辑屏上显示 160 逻辑像素够扫。 */
const TEX = 256;
/* 静区：规格要求 4 个模块宽，少了手机对不上焦。 */
const QUIET = 4;

/** 把 `text` 编成二维码、上传成贴图并注册到 `key`；返回实际绘制边长（像素）。 */
export function uploadQrTexture(key: string, text: string): number {
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

  const handle = getOps().uploadTexture(rgba, TEX, TEX, 3);
  if (handle < 0) throw new Error("二维码贴图上传失败");
  registerTexture(key, handle);
  return drawn;
}
