# YUNYIN 云音

<img src="screenshots/icon0.png" width="112" align="right" alt="YUNYIN">

一款用 [PocketJS](https://pocketjs.dev) 开发
（Solid 前端 + Rust 原生宿主，跑在 Vita 原生进程里），专为psvita开发的一款本地音乐播放器。受不了现有音乐播放器的古法UI，想要一个现代化、可自定义主题、支持中文歌词的本地音乐播放器，于是就自己写了。

|  |  |
| --- | --- |
| 曲库目录 | `ux0:/data/yunyin/music` |
| 音频格式 | MP3 / OGG / WAV / FLAC / OPUS |
| 界面 | 首页播放器 · 全部曲目 · 专辑 · 收藏 · 设置 |
| 主题 | LIGHT / DARK / PURE / ANIME 四套，**默认 DARK** |

> 建议先用 [MusicBrainz Picard](https://picard.musicbrainz.org/) 给音乐补全标签再放进去。
> 歌名、歌手、专辑、封面、歌词全都来自音频内嵌标签，标签越全，界面越好看。

## 界面截图

| 首页播放器 |
 ![首页播放器](screenshots/1.png) 


| 歌词页 |
 ![歌词页](screenshots/2.png) 

| 专辑 |
![专辑](screenshots/4.png) 

| 全部曲目 |
| ![全部曲目](screenshots/3.png) 

| 设置 |
| ![设置](screenshots/5.png) 

## 功能特性

- **曲库扫描**：启动时扫描 `ux0:/data/yunyin/music`（支持子文件夹，最深 5 层、最多 240 首），
  按 `artist + album + title + durationMs` 归类并缓存。改标签、重命名、重新扫描都不会丢收藏；
  专辑名留空的归到 `Singles`，重复条目自动去重。
- **本地解码播放**：MP3、OGG(Vorbis)、WAV、FLAC、OPUS，全部在 Vita 上原生解码播放。
- **内嵌封面**：从 ID3 / FLAC / OGG 标签取内嵌封面，懒加载 + 原生缩放；
  专辑页三张封面横向滑动切换，卡片封面跟着可见专辑实时刷新。
- **歌词**：读取音频内嵌歌词（ID3 USLT / OGG LYRICS / FLAC），逐行显示，当前行高亮放大。
- **四套主题**：LIGHT / DARK / PURE / ANIME，各有独立的背景图、面板、按钮图标与文字配色；
  设置页里随时切换，即时生效。
- **中文字形烘焙**：把曲库标签 + UI 文案里出现过的汉字烘焙进字体图集，
- **Vita 按键操作**：左侧导航 + 右侧内容双区焦点，界面底部有按键提示。
- **流畅动画**：页面进出场、焦点缩放、封面滑动、频谱柱跳动，在 Vita 上保持 60FPS。

## 操作

| 按键 | 作用 |
| --- | --- |
| ↑ / ↓ | 左侧导航上下移动（首页 / 列表 / 专辑 / 收藏 / 设置） |
| ← / → | 右侧内容区左右移动（曲目、专辑卡片、设置卡片、播放按钮） |
| ○ | 确定：进入页面 / 播放曲目 / 切换开关 |
| △ | 返回：退出歌词、关闭 About、返回上级 |

按键提示也画在界面底部（○ SELECT / △ BACK / L PREV / R NEXT / ◎ MENU）。

## 安装与使用

1. 把 `dist/yunyin-cn.vpk`（或 `yunyin-jp.vpk`）拷到记忆卡，用 VitaShell 选中安装。
2. 音乐文件放进 `ux0:/data/yunyin/music`（可以分子文件夹）。
3. 启动「云音」，首次进入会自动扫描曲库，扫描完在首页按 ○ 播放。。

## 两个字体版本（中文 / 日文）

同一份代码可以打包成两个字体版本，界面完全一致，只有字体文件与字形集不同：

```powershell
wsl -d pocket-ubuntu -u root bash /mnt/d/AI-PSVITA/yunyin/scripts/build-variants.sh
```

- `dist/yunyin-cn.vpk` —— 中文优先：Noto Sans SC，补 CJK 标点 + 全角字符，简体字形。
- `dist/yunyin-jp.vpk` —— 日文优先：MS Mincho，补平假名 / 片假名 / 半角假名 /
  CJK 标点 / 全角字符，假名与汉字用日文字形。

只打其中一个：

```powershell
wsl -d pocket-ubuntu -u root bash -lc 'cd /mnt/d/AI-PSVITA/yunyin && YUNYIN_FONT=japanese YUNYIN_OUT=yunyin-jp python3 scripts/build-vpk.py'
```

字形集 = 曲库标签收割（歌名/歌手/专辑）+ UI 文案 +
`fonts/<字体主题>/chars.txt` 里声明的字符区间。要补字符就改那个 txt：每行写
`U+XXXX-U+YYYY` 区间或直接写字符，`#` 后面是注释。字形图集有尺寸上限，
一次塞太多字会烘焙失败。

## 打包方法

PowerShell（整套流水线跑在 WSL2 里）：

```powershell
wsl -d pocket-ubuntu -u root bash -lc 'cd /mnt/d/AI-PSVITA/yunyin && python3 scripts/build-vpk.py'
```

输出 `dist/yunyin-main.vpk`（TITLE_ID `PF2A47F97`，等同中文版）。

`scripts/build-vpk.py` 的流程：暂存前端源码与原生补丁 → 把皮肤图缩放到合法贴图尺寸 →
烘焙字体图集 → 调 PocketJS 打包 → 用本项目 TITLE_ID 重新封装 VPK。

构建依赖（WSL2 的 `pocket-ubuntu` 发行版内）：

- VitaSDK（`/opt/vitasdk`）
- bun（`/root/.bun/bin/bun`）
- PocketJS 框架源码（`/root/pocketjs`）

## 目录结构

| 路径 | 说明 |
| --- | --- |
| `app/` | 前端：`app.tsx` 界面、`colors.json` 文字配色、`images.json` 贴图清单 |
| `asset/ui/<主题>/` | 四套皮肤的贴图（背景、面板、卡片、行、图标） |
| `native/` | 原生部分：音频解码与元数据读取（`media.rs`、`yplayer.c` 等） |
| `fonts/` | 字体文件与字形表（`fonts/chinese/`、`fonts/japanese/`） |
| `scripts/` | 打包脚本（`build-vpk.py`、`build-variants.sh`） |
| `dist/` | 打包产物（VPK） |

## 备注

- 主题文字配色集中在 `app/colors.json`，改完重新打包即可生效。
- 皮肤原图可以放高分辨率，打包时会自动缩放到 Vita 贴图上限（2 的幂、单边 ≤512）。
- 日文版用的 MS Mincho 来自 Windows 自带字体，对外分发前请自行确认授权；
  `fonts/chinese/NotoSansSC-Medium.ttf` 是 Noto Sans SC（SIL OFL）。
- 界面与交互参考了 PocketJS 官方 `library` / `gallery` / `music` / `launcher` 模板。
