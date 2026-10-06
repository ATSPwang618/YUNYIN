/* 主题：扁平化配色（浅色 / 深色 / 浅蓝 / 浅绿 / 浅紫）+ 文字与底色角色表。
 *
 * 新 UI 是扁平化的：面板/卡片/焦点都用类名直接画（bgCls），
 * 皮肤资产只剩播放器那 6 个控制图标 —— 背景/卡片/行贴图已全部删掉。
 * 颜色定义在 app/colors.json（构建时把里面的类字面量烘焙进样式表）。
 * 浅蓝/浅绿/浅紫三套是从浅色派生出来的（scripts/tools/gen-flat-themes.py），
 * 只换强调色和整屏底色，布局与字号完全一致。
 */
import { createSignal } from "solid-js";
import THEME_COLORS from "../colors.json";

export const UI_THEMES = ["light", "dark", "blue", "green", "purple"] as const;
export type UiSkin = (typeof UI_THEMES)[number];

/* 主题显示名（设置页那一行）。 */
const THEME_LABELS: Record<UiSkin, string> = {
  light: "浅色",
  dark: "深色",
  blue: "浅蓝",
  green: "浅绿",
  purple: "浅紫",
};

/* 播放器控制图标 —— 每个动作两个状态：N = 常态，F = 聚焦（贴图自带圆圈高亮，
 * 所以焦点态不再用类名画圆底）。暗色主题用暗色那套。 */
export type UiSkinKey =
  | "play" | "playF"
  | "pause" | "pauseF"
  | "prev" | "prevF"
  | "next" | "nextF"
  | "hon" | "honF"
  | "hoff" | "hoffF"
  | "shuf" | "shufF"
  | "rep1" | "rep1F";
export type SkinMap = Record<UiSkinKey, string>;

/*
 * 图标路径必须是**字面量**：打包器按源码里出现的路径字符串收素材，
 * 用模板串拼路径它看不见 → 运行时找不到贴图（真机上按钮直接空白）。
 * 每套主题一套自己的图（彩色那三套由 scripts/tools/gen-theme-icons.py 换色派生）。
 */
const LIGHT_ICONS: SkinMap = {
  play: "asset/ui/light/icon_playN.png",
  playF: "asset/ui/light/icon_playF.png",
  pause: "asset/ui/light/icon_pauseN.png",
  pauseF: "asset/ui/light/icon_pauseF.png",
  prev: "asset/ui/light/icon_prevN.png",
  prevF: "asset/ui/light/icon_prevF.png",
  next: "asset/ui/light/icon_nextN.png",
  nextF: "asset/ui/light/icon_nextF.png",
  hon: "asset/ui/light/icon_honN.png",
  honF: "asset/ui/light/icon_honF.png",
  hoff: "asset/ui/light/icon_hoffN.png",
  hoffF: "asset/ui/light/icon_hoffF.png",
  shuf: "asset/ui/light/icon_shufN.png",
  shufF: "asset/ui/light/icon_shufF.png",
  rep1: "asset/ui/light/icon_rep1N.png",
  rep1F: "asset/ui/light/icon_rep1F.png",
};

const DARK_ICONS: SkinMap = {
  play: "asset/ui/dark/icon_playN.png",
  playF: "asset/ui/dark/icon_playF.png",
  pause: "asset/ui/dark/icon_pauseN.png",
  pauseF: "asset/ui/dark/icon_pauseF.png",
  prev: "asset/ui/dark/icon_prevN.png",
  prevF: "asset/ui/dark/icon_prevF.png",
  next: "asset/ui/dark/icon_nextN.png",
  nextF: "asset/ui/dark/icon_nextF.png",
  hon: "asset/ui/dark/icon_honN.png",
  honF: "asset/ui/dark/icon_honF.png",
  hoff: "asset/ui/dark/icon_hoffN.png",
  hoffF: "asset/ui/dark/icon_hoffF.png",
  shuf: "asset/ui/dark/icon_shufN.png",
  shufF: "asset/ui/dark/icon_shufF.png",
  rep1: "asset/ui/dark/icon_rep1N.png",
  rep1F: "asset/ui/dark/icon_rep1F.png",
};

const BLUE_ICONS: SkinMap = {
  play: "asset/ui/blue/icon_playN.png",
  playF: "asset/ui/blue/icon_playF.png",
  pause: "asset/ui/blue/icon_pauseN.png",
  pauseF: "asset/ui/blue/icon_pauseF.png",
  prev: "asset/ui/blue/icon_prevN.png",
  prevF: "asset/ui/blue/icon_prevF.png",
  next: "asset/ui/blue/icon_nextN.png",
  nextF: "asset/ui/blue/icon_nextF.png",
  hon: "asset/ui/blue/icon_honN.png",
  honF: "asset/ui/blue/icon_honF.png",
  hoff: "asset/ui/blue/icon_hoffN.png",
  hoffF: "asset/ui/blue/icon_hoffF.png",
  shuf: "asset/ui/blue/icon_shufN.png",
  shufF: "asset/ui/blue/icon_shufF.png",
  rep1: "asset/ui/blue/icon_rep1N.png",
  rep1F: "asset/ui/blue/icon_rep1F.png",
};

const GREEN_ICONS: SkinMap = {
  play: "asset/ui/green/icon_playN.png",
  playF: "asset/ui/green/icon_playF.png",
  pause: "asset/ui/green/icon_pauseN.png",
  pauseF: "asset/ui/green/icon_pauseF.png",
  prev: "asset/ui/green/icon_prevN.png",
  prevF: "asset/ui/green/icon_prevF.png",
  next: "asset/ui/green/icon_nextN.png",
  nextF: "asset/ui/green/icon_nextF.png",
  hon: "asset/ui/green/icon_honN.png",
  honF: "asset/ui/green/icon_honF.png",
  hoff: "asset/ui/green/icon_hoffN.png",
  hoffF: "asset/ui/green/icon_hoffF.png",
  shuf: "asset/ui/green/icon_shufN.png",
  shufF: "asset/ui/green/icon_shufF.png",
  rep1: "asset/ui/green/icon_rep1N.png",
  rep1F: "asset/ui/green/icon_rep1F.png",
};

const PURPLE_ICONS: SkinMap = {
  play: "asset/ui/purple/icon_playN.png",
  playF: "asset/ui/purple/icon_playF.png",
  pause: "asset/ui/purple/icon_pauseN.png",
  pauseF: "asset/ui/purple/icon_pauseF.png",
  prev: "asset/ui/purple/icon_prevN.png",
  prevF: "asset/ui/purple/icon_prevF.png",
  next: "asset/ui/purple/icon_nextN.png",
  nextF: "asset/ui/purple/icon_nextF.png",
  hon: "asset/ui/purple/icon_honN.png",
  honF: "asset/ui/purple/icon_honF.png",
  hoff: "asset/ui/purple/icon_hoffN.png",
  hoffF: "asset/ui/purple/icon_hoffF.png",
  shuf: "asset/ui/purple/icon_shufN.png",
  shufF: "asset/ui/purple/icon_shufF.png",
  rep1: "asset/ui/purple/icon_rep1N.png",
  rep1F: "asset/ui/purple/icon_rep1F.png",
};

export const SKINS: Record<UiSkin, SkinMap> = {
  light: LIGHT_ICONS,
  dark: DARK_ICONS,
  blue: BLUE_ICONS,
  green: GREEN_ICONS,
  purple: PURPLE_ICONS,
};

/* 启动默认皮肤：设计稿是浅色版，默认跟着设计走。 */
export const [uiTheme, setUiTheme] = createSignal<UiSkin>("light");
export const useSkin = (): SkinMap => SKINS[uiTheme()];



/* 文字角色 —— 每套主题一套完整类字面量（构建时烘焙）。 */
export const PANEL_TXT = THEME_COLORS.ui as Record<UiSkin, Record<string, string>>;
export const pTxt = (key: string) => PANEL_TXT[uiTheme()][key] ?? "";

/* 底色/描边角色 —— 面板、卡片、焦点行、进度条、二维码底。 */
export const PANEL_BG = THEME_COLORS.bg as Record<UiSkin, Record<string, string>>;
export const bgCls = (key: string) => PANEL_BG[uiTheme()][key] ?? "";

/* 主题切换顺序（设置页用）。 */
/* 主题轮换：设置页按 ○ 依次切 浅色 → 深色 → 浅蓝 → 浅绿 → 浅紫 → 浅色。 */
export const nextTheme = (): UiSkin => {
  const i = UI_THEMES.indexOf(uiTheme());
  return UI_THEMES[(i + 1) % UI_THEMES.length];
};
export const themeLabel = (): string => THEME_LABELS[uiTheme()];

/* 软件版本：About 页展示，和 param.sfo APP_VER 对齐。 */
export const APP_VERSION = "1.10";
export const APP_VER_SFO = "01.10";
export const POCKETJS_VERSION = "0.13.0";
