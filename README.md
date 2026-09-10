# YUNYIN 云音

<img src="screenshots/icon0.png" width="112" align="right" alt="YUNYIN">

一款用 [PocketJS](https://pocketjs.dev) 开发
（Solid 前端 + Rust ，跑在 Vita 原生进程里），专为psvita开发的一款本地音乐播放器。受不了现有音乐播放器的古法UI，想要一个现代化、可自定义主题、支持中文歌词的本地音乐播放器，于是就自己写了。

|  |  |
| --- | --- |
| 曲库目录 | `ux0:/data/yunyin/music` |
| 音频格式 | MP3（最推荐格式！！） / OGG / WAV / FLAC / OPUS |
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

## 标签规范（重要）

歌名、歌手、专辑、封面、歌词**全部来自音频的内嵌标签**。标签不规范时，界面上就会出现
文件名、`Local` / `Unknown`、问号或者空白。最常见的三类问题：

| 情况 | 在播放器上的表现 |
| --- | --- |
| MP3 标签编码字节写着 Latin-1，里面却塞 GBK / Big5 字节（国内工具常见） | 中文全变乱码 |
| 只有文件尾那种老式 ID3v1，或者压根没有标签 | 只显示文件名，歌手 `Local`、专辑 `Unknown` |
| 封面不是内嵌的 JPEG / PNG，或体积超过 1MB | 显示默认封面 |

### 先体检：YUNYIN 标签体检（图形界面）

下载 Release 附件里的 **`YUNYIN-TagCheck.exe`**，双击打开，然后任选一种方式：

- 点「选择文件夹…」（整个曲库）
- 点「选择文件…」（可多选，只查这几首）
- **直接把文件或文件夹拖进窗口**（可一次拖多个，混着拖也行）

点「开始检查」即可。它用的是**和播放器完全相同的标签读取规则**，所以报告出来的就是播放器真实会看到的结果：

- 每首歌的状态（完整 / 可改善 / 建议整理 / 乱码）+ 缺哪几项，红色标出有问题的
- 乱码的歌直接写出「正确内容应该是『…』」，方便确认检测准不准
- 选中某一行，窗口下方会显示这首歌的完整说明和文件路径，双击还能看大图（弹窗详情）

界面上两个导出按钮：

- **导出明细 CSV** —— 全量报告，Excel 直接打开
- **导出待修文件夹** —— 把所有待修曲目「硬链接」到一个文件夹里（不占额外空间），
  整包拖进 Picard 就行。**注意 Picard 不认 m3u8 播放列表**，所以用它而不是清单文件。

### 也可以不用 Picard：联网直接修复

点 **「联网匹配并修复…」**，它会用现有标签（乱码或缺失时用文件名）去
**iTunes + MusicBrainz** 搜索，把匹配到的歌名 / 歌手 / 专辑 / 年份 / 封面直接写进文件：

- 结果先列出来给你看：相似度低于阈值（默认 0.72，可改）的不建议写入
- 双击某一行可以**换下一个候选**
- 默认**只补「缺失或乱码」的字段**，不动你已经写好的标签；勾「覆盖已有标签」才会全覆盖
- 封面来自 Cover Art Archive / iTunes，会自动选 500–600px 的版本（远小于播放器 1MB 上限）

局限要说清楚：免费数据库对**主流歌曲**命中率很高，但**翻唱、伴奏、Remix、军乐、冷门
中文歌**经常查不到——这种情况还是得靠 Picard 或手动补。

源码就是 `scripts/tagcheck.py`（纯 Python 标准库 + tkinter，不依赖任何第三方库），也能当命令行用：

```
python scripts/tagcheck.py                     # 图形界面
python scripts/tagcheck.py --cli "D:\Music" --csv report.csv --m3u8 music-to-fix.m3u8
```

要自己重新打包 exe：`powershell -ExecutionPolicy Bypass -File scripts\build-tagcheck-exe.ps1`
（需要 Python 3.10+ 且勾选了 tcl/tk；脚本会自动装 pyinstaller）

### 再修：MusicBrainz Picard

1. 装 [MusicBrainz Picard](https://picard.musicbrainz.org/)（免费）
2. 打开 `music-to-fix.m3u8`，或直接把音乐文件夹拖进去
3. 全选 → **Lookup**（按声学指纹匹配，不认识中文也能自动认出来）
4. **Save** —— 标签会被重写成规范的 UTF-8 / UTF-16，乱码一并解决
5. 想要封面和歌词：Options → Metadata 里勾上 Cover Art、Lyrics

### 自己写标签时的规范建议

- MP3：ID3v2.3 / 2.4，文字用 **UTF-8**（或带 BOM 的 UTF-16）
- FLAC / OGG / OPUS：Vorbis comment（UTF-8），键名用 TITLE / ARTIST / ALBUM / LYRICS
- 封面：内嵌 JPEG 或 PNG，建议 500×500 以内、**不要超过 1MB**
- 歌词：内嵌（ID3 的 USLT、Vorbis 的 LYRICS），带时间轴更佳（LRC 格式）
- 文件名随意：播放器优先用标签，读不到标签才退回文件名

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
