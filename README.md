# YUNYIN 云音

<img src="screenshots/icon0.png" width="104" align="right" alt="YUNYIN">

给 **PS Vita** 用的本地音乐播放器：深色界面、中文歌名不乱码、歌词逐句滚动、
专辑封面滑动浏览，四套主题随便换。

**下载**：<https://github.com/ATSPwang618/YUNYIN/releases>

## 怎么用

1. 把 `yunyin-cn.vpk`（中文曲库）或 `yunyin-jp.vpk`（日文曲库）用 VitaShell 装到 PS Vita。
2. 音乐放进 `ux0:/data/yunyin/music`（可以分子文件夹，最多 240 首）。
3. 打开「云音」，第一次进入会自动扫描曲库，扫完就能听。
4. 首页按 **○** 播放；**←→** 切按钮/切歌，**△** 返回，**↑↓** 换左侧页面。
5. 设置页可以换主题（LIGHT / DARK / PURE / ANIME）、开关音效和震动。

支持格式：MP3 / OGG / WAV / FLAC / OPUS。

> 歌名、歌手、专辑、封面、歌词**全都来自音频文件里的内嵌标签**。
> 建议先用 [MusicBrainz Picard](https://picard.musicbrainz.org/) 把标签补全，界面才好看。

## 歌名/封面/歌词不显示？用配套工具修

Release 里还有一个 **`YUNYIN-TagCheck.exe`**（Windows，免安装），专治各种标签不规范：

1. **双击打开** → 把音乐文件夹（或几个文件）**直接拖进窗口**，或点「选择文件夹…」→ 点「开始检查」。
   它会按播放器完全相同的规则读一遍，告诉你每首歌缺什么、哪些是乱码（乱码的还会写出正确内容）。
2. 点 **「联网匹配并修复…」** 一键补全：去 iTunes / Deezer / TheAudioDB / MusicBrainz 搜歌名、歌手、
   专辑、封面，再去 **LRCLIB 找带时间轴的歌词**，一起写进文件里。

   - 结果先列出来给你看，相似度太低的不建议写入（阈值可调），双击某行还能换下一个候选
   - 默认只补「缺失或乱码」的字段，不动你已经写好的标签
3. 免费库查不到的（翻唱、伴奏、冷门歌）点 **「导出待修文件夹」**，把导出的文件夹拖进 Picard 手动补。

拖错文件了？选中那几行点「**移除选中**」（或按 Delete 键），或者「**清空列表**」重来。

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

## 两个字体版本

- `yunyin-cn.vpk` —— 中文优先：Noto Sans SC，汉字用简体字形
- `yunyin-jp.vpk` —— 日文优先：MS Mincho，假名和汉字用日文字形

两个版本界面完全一样，只是字体和字形集不同，按曲库语言挑一个装就行。

## 给开发者：自己打包

需要 WSL2（`pocket-ubuntu`）里的 VitaSDK、bun 和 PocketJS 源码：

```powershell
# 只打默认版（中文）→ dist/yunyin-main.vpk
wsl -d pocket-ubuntu -u root bash -lc 'cd /mnt/d/AI-PSVITA/yunyin && python3 scripts/build-vpk.py'

# 一次打中文 + 日文两个版本
wsl -d pocket-ubuntu -u root bash /mnt/d/AI-PSVITA/yunyin/scripts/build-variants.sh
```

重新打标签工具：`powershell -ExecutionPolicy Bypass -File scripts\build-tagcheck-exe.ps1`
（需要 Python 3.10+，脚本会自己装 pyinstaller 和 mutagen）

目录：`app/` 前端源码（`app.tsx` 界面、`colors.json` 文字配色）· `asset/ui/<主题>/` 皮肤图 ·
`native/` 音频解码与标签读取 · `fonts/` 字体 · `scripts/` 打包脚本。

## 写在最后

- 主题文字颜色在 `app/colors.json` 里改，改完重新打包即可。
- 日文版用的 MS Mincho 来自 Windows 自带字体，对外分发前请确认授权；
  `fonts/chinese/NotoSansSC-Medium.ttf` 是 Noto Sans SC（SIL OFL）。
- 界面参考了 PocketJS 官方 `library` / `gallery` / `music` / `launcher` 模板。
