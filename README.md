# YUNYIN 云音

<img src="screenshots/icon0.png" width="104" align="right" alt="YUNYIN">

用 [PocketJS](https://pocketjs.dev)（Solid 前端 + Rust 宿主，整个播放器就跑在 Vita 的一个进程里）写的 PS Vita 本地音乐播放器。

我受不了现有音乐播放器的古法 UI，一直想要一个现代化、能换主题、能看中文歌词的本地播放器，于是这个项目就诞生了。

## 它好在哪

- **播放期间 PS 键被锁住。** 正在放歌时按 PS 不会回到桌面 —— 想离开应用，先按 **○** 暂停（暂停即解锁）。这样就不用去赌"切后台到底还能不能出声"：解码和出声都在云音自己的进程里，音频走系统 BGM 口，MP3 / OGG / WAV / FLAC / OPUS 一视同仁。
- **退出就停。** 暂停后按 PS 回桌面、在 LiveArea 上把云音撕掉，进程结束，声音立刻断。
- **黑屏播放 + 肩键换曲。** 播放中按 **START** 关掉画面，声音继续；黑屏下 **L / R** 切上一首 / 下一首，屏幕保持黑着；按其他任意键（或再按一次 START）回到画面。
- **进应用默认是暂停**，按 **○** 才开始放。
- 四套主题、逐句同步的歌词，封面 / 歌手 / 专辑全都读音频内嵌标签。
- 中日两套字形，外加**流式 CJK**：常用字烘焙进包里，生僻字按需从后台加载。

|  |  |
| --- | --- |
| 当前版本 | **0.61** —— [下载 / 历史版本](https://github.com/ATSPwang618/YUNYIN/releases) |
| 曲库目录 | `ux0:/data/yunyin/music`（可以分子文件夹，最多 240 首） |
| 音频格式 | MP3（最推荐） / OGG / WAV / FLAC / OPUS |
| 界面 | 首页播放器 · 全部曲目 · 专辑 · 收藏 · 设置 |
| 主题 | LIGHT / DARK / PURE / ANIME 四套，**默认 DARK** |

> 歌名、歌手、专辑、封面、歌词全都来自音频的内嵌标签，标签越全，界面越好看。
> 标签乱的话，先用下面那个 `YUNYIN-TagCheck.exe` 一键修。

## 实机安装

1. 用 VitaShell 把 `yunyin-cn.vpk`（中文曲库）或 `yunyin-jp.vpk`（日文曲库）装到 PS Vita。
2. 音乐放进 `ux0:/data/yunyin/music`。
3. 打开「云音」，第一次进入会自动扫描曲库，扫完就能听。
4. 设置页可以换主题、开关音效和震动；About 页显示版本号，按 **○** 切换 CJK 烘焙 / 流式。
5. **播放期间 PS 键是锁着的**：想退出应用，先按 **○** 暂停（暂停即解锁），再按 PS 回桌面；把云音撕页关掉，声音立刻停。

### 按键

| 键 | 作用 |
| --- | --- |
| ← → ↑ ↓ | 移动光标（左边栏切页面，右边栏切项目） |
| ○ | 确认。首页上按光标位置分别是：上一首 / 播放暂停 / 下一首 / 循环模式 / 收藏 / 歌词页 |
| △ | 返回 |
| L / R | 上一首 / 下一首（黑屏时照样有效） |
| START | 关屏继续播放（再按一次或按别的键回来） |
| PS | 播放期间被锁（想离开先暂停）；暂停后正常回桌面 / 长按弹快捷菜单 |

> PS 键锁只在**播放中**生效，暂停 / 停止 / 一首放完立刻自动解锁。
> 这是刻意的：按 PS 会把应用切到后台，而后台继续出声需要 Vita 宿主那套支持（上游 PocketJS 没做，实测切后台就断），
> 所以干脆不让你在放歌时误退出去。万一应用真的卡死，长按电源键约 30 秒可以强制关机。

## 界面截图

**首页播放器**（内嵌封面 + 频谱柱 + 播放控制）

![首页播放器](screenshots/1.png)

**歌词页**（逐句同步，当前行高亮）

![歌词页](screenshots/2.png)

**全部曲目**（序号 / 歌名 / 歌手 / 播放状态）

![全部曲目](screenshots/3.png)

**专辑**（封面一张张滑动切换）

![专辑](screenshots/4.png)

**设置**（音效 / 震动 / 关于 / 主题）

![设置](screenshots/5.png)

## 两个字体版本 + 流式 CJK

- `yunyin-cn.vpk` —— 中文优先：Noto Sans SC，汉字用简体字形
- `yunyin-jp.vpk` —— 日文优先：MS Mincho，假名和汉字用日文字形

两个包界面完全一样，按曲库语言挑一个装就行。

界面上的字有两套来源，About 页按 **○** 可以随时切：

- **CJK BAKED**（默认）：常用字直接烘焙进包里，启动最快，不读盘。
- **CJK STREAM**：常用字照样烘焙，生僻字（人名、地名、冷门歌词）在后台按需从 `app0:/fonts/cjk.pjfa` 取。
  显示范围大得多，代价是多一点内存和后台读盘。

歌名或歌词里出现生僻字、显示成方框的时候，切成 STREAM 就好。

## 歌名 / 封面 / 歌词不显示？用配套工具修

Release 里有一个 **`YUNYIN-TagCheck.exe`**（Windows，免安装），把 mp3 拖进去就能联网修标签：

1. **双击打开** → 把音乐文件夹（或几个文件）**直接拖进窗口**，或点「选择文件夹…」→ 点「开始检查」。
   它会按播放器完全相同的规则读一遍，告诉你每首歌缺什么、哪些是乱码（乱码的还会写出正确内容）。
2. 点 **「联网匹配并修复…」** 一键补全：去 iTunes / Deezer / TheAudioDB / MusicBrainz 搜歌名、歌手、
   专辑、封面，再去 **LRCLIB 找带时间轴的歌词**，一起写进文件里。

   - 结果先列出来给你看，相似度太低的不建议写入（阈值可以自己调），双击某行还能换下一个候选
   - 默认只补「缺失或乱码」的字段，不动你已经写好的标签
3. 免费库查不到的（翻唱、伴奏、冷门歌）点 **「导出待修文件夹」**，把导出的文件夹拖进 Picard 手动补。

拖错文件了？选中那几行点「**移除选中**」（或按 Delete 键），或者「**清空列表**」重来。

## 不规范标签可能导致出现的情况

| 情况 | 在播放器上的表现 |
| --- | --- |
| MP3 标签编码字节写着 Latin-1，里面却塞 GBK / Big5 字节（国内工具常见） | 中文全变乱码 |
| 只有文件尾那种老式 ID3v1，或者压根没有标签 | 只显示文件名，歌手 `Local`、专辑 `Unknown` |
| 封面不是内嵌的 JPEG / PNG，或体积超过 1MB | 显示默认封面 |

### 自己想写标签时的规范建议

- MP3：ID3v2.3 / 2.4，文字用 **UTF-8**（或带 BOM 的 UTF-16）
- FLAC / OGG / OPUS：Vorbis comment（UTF-8），键名用 TITLE / ARTIST / ALBUM / LYRICS
- 封面：内嵌 JPEG 或 PNG，建议 500×500 以内、**不要超过 1MB**
- 歌词：内嵌（ID3 的 USLT、Vorbis 的 LYRICS），带时间轴更佳（LRC 格式）
- 文件名随意：播放器优先用标签，读不到标签才退回文件名

## 抓日志（出问题时用）

正式版默认不写日志。要抓日志：

1. 在卡里 `ux0:/data/yunyin/` 下建一个**空文件**，名字就叫 `debug`（不要扩展名）。
2. 重新打开云音，日志会写进 `ux0:data/yunyin.log`。
3. 抓完把 `debug` 删掉，下次启动就又不写了。

## 自行构建

在 WSL2（`pocket-ubuntu`）里准备 VitaSDK、bun，以及 **PocketJS v0.12.0** 的源码放在 `/root/pocketjs`。
（这一版必须用 0.12：流式 CJK 用到 `engine/core/src/font_stream.rs`、`framework/src/fonts.ts`，0.11 里没有这些文件。）

```powershell
# 只打默认版（中文）→ dist/yunyin-main.vpk
wsl -d pocket-ubuntu -u root bash -lc 'cd /mnt/d/AI-PSVITA/yunyin && python3 scripts/build-vpk.py'

# 一次打中文 + 日文两个版本 → dist/yunyin-cn.vpk / dist/yunyin-jp.vpk
wsl -d pocket-ubuntu -u root bash /mnt/d/AI-PSVITA/yunyin/scripts/build-variants.sh
```

- 版本号（`param.sfo` 里的 `APP_VER`，VitaShell 里能看到）在 `scripts/build-vpk.py` 顶部，默认 `00.61`；
  也可以用环境变量 `YUNYIN_APP_VER=00.62` 覆盖。
- PocketJS 装在别的地方：`POCKETJS_ROOT=/你的路径 python3 scripts/build-vpk.py`。
- 流式字库 `fonts/chinese/cjk.pjfa` 由 `scripts/bake-cjk-archive.ts` 烘出来，构建时会直接用缓存；
  换了字体或改了 `fonts/chinese/cjk-stream.txt`，把这个文件删掉让它重烘。

## 最后

- 特别感谢 [PocketJS](https://pocketjs.dev) 团队的努力付出！！
- 播放后端对照 [ElevenMPVScrobbling](https://github.com/patchyfluffy/ElevenMPVScrobbling) 的"本进程 BGM 口"做法（不是 libShellAudio / SceShell）。
- 主题文字颜色在 `app/colors.json` 里改，改完重新打包即可。
- 日文版用的 MS Mincho 来自 Windows 自带字体，对外分发前请确认授权；
  `fonts/chinese/NotoSansSC-Medium.ttf` 是 Noto Sans SC（SIL OFL）。
- 界面参考了 PocketJS 官方 `library` / `gallery` / `music` / `launcher` 模板。
