/* qrcode-generator（MIT，见 qrcode.js 头部）的最小类型声明。 */
export interface QrCode {
  addData(data: string): void;
  make(): void;
  getModuleCount(): number;
  isDark(row: number, col: number): boolean;
}

declare function qrcode(typeNumber: number, errorCorrectionLevel: string): QrCode;

export default qrcode;
