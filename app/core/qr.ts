/* QR display bridge.
 *
 * Rust owns URL encoding, Reed-Solomon, matrix construction and RGBA painting
 * on a native worker. JS only waits for the finished native texture handle
 * and registers it with the renderer. No QR algorithm or byte-sized image
 * construction belongs in the guest frame.
 */

import { getOps, registerTexture } from "@pocketjs/framework";
import { media } from "./media";

let frameNo = 0;
let lastQrHandle = -1;

type QrUploadJob = {
  key: string;
  text: string;
  afterFrame: number;
  onReady: () => void;
  onError: () => void;
};

let pendingQr: QrUploadJob | undefined;

/** Queue a native QR result for the next safe frame window. */
export function scheduleQrTexture(
  key: string,
  text: string,
  onReady: () => void,
  onError: () => void,
): void {
  pendingQr = {
    key,
    text,
    afterFrame: frameNo + 2,
    onReady,
    onError,
  };
}

/** Drop a queued job when the account page is unmounted or superseded. */
export function cancelQrTexture(key: string, text?: string): void {
  if (pendingQr?.key !== key) return;
  if (text !== undefined && pendingQr.text !== text) return;
  pendingQr = undefined;
}

/** Poll only the cheap Rust-ready bridge and register one new texture handle. */
export function pumpQrTexture(): void {
  frameNo += 1;
  const job = pendingQr;
  if (!job || frameNo < job.afterFrame) return;

  const handle = media()?.netLoginQr?.() ?? -1;
  if (handle < 0) return; /* Rust worker is still encoding. */

  pendingQr = undefined;
  try {
    const ops = getOps();
    if (lastQrHandle >= 0) {
      ops.freeTexture?.(lastQrHandle);
    }
    lastQrHandle = handle;
    registerTexture(job.key, handle);
    job.onReady();
  } catch {
    job.onError();
  }
}
