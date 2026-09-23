# 重构第一轮：现状审计与修改计划

对应重构任务书 §92–§97（第一轮只产出审计、设计、计划、风险、Phase 0）。
本轮的**代码改动只有一件事**：把模块骨架与接缝建起来并验证；
媒体行为一行未变（本地五个格式 + M4A 的播放路径完全照旧）。

## 一、现状 vs 目标

```text
现在                                    目标
────                                    ────
Audio Thread                            Network Thread
 ├── Decoder（按扩展名分派六个格式）        └── HttpRangeSource + Byte Cache
 └── sceAudioOutOutput（BGM 口，960 帧）
                                        Audio Thread（保持 960 帧模型）
输入只有「文件路径」                      ├── Gate（缓存不足则静音）
没有网络层、没有 Source、没有 Provider     ├── Decoder（六个 decode loop 不变）
                                        └── sceAudioOutOutput
```

## 二、文件级表格（按实际源码，已逐项对照）

| 文件 | 当前职责 | 问题 | 计划修改 | 不修改 |
| --- | --- | --- | --- | --- |
| `native/rs/bgm.rs` | BGM 口生命周期、音频线程、960 帧块、暂停/停止 | 入口只有 `path` | 加 Source 播放入口、duration hint、buffer gate | BGM port lifecycle、960 帧模型、输出调用 |
| `native/rs/decoder.rs` | `yplayer.c` 的 FFI（open/rate/decode/seek/…） | 只能传路径 | handle 化 + `yp_open_io` + duration hint + source gate | decode/position 语义 |
| `native/audio/yplayer.c` | 六个格式的解码循环 + 输出缓冲补齐 | 输入只能用路径 | `yp_io` 回调、handle、format hint、stream mode | **六个 decode loop**（mp3/ogg/wav/flac/opus/m4a） |
| `native/audio/ym4a.c` | M4A 解复用（moov/stbl/esds/样本表） | 内部 `FILE*` | 改走同一份 `yp_io` | 样本表/标签/单位换算逻辑 |
| `native/audio/yaac.c` | SceAudiodec 硬件 AAC | 无 | **不动**（不碰 IO） | 全部 |
| `native/rs/tags.rs` | 本地曲库标签/封面（含 M4A ilst） | 只服务本地文件 | 不动；在线曲目元数据由 Provider 提供 | 全部 |
| `native/rs/bridge.rs` | QuickJS 绑定（`vitaMedia.*`） | 无远程/缓存状态、panic 曾穿透 JS | 加 remote resolve/play、buffer state、cookie；已加 `catch_unwind` 防护 | 现有方法语义 |
| `native/rs/platform/power.rs` | 电源 tick | 无 | **不动** | 全部 |
| `native/rs/platform/ps_lock.rs` | 播放期间锁 PS 键 | 无 | **不动** | 全部 |
| `native/rs/ui/*` `native/rs/platform/{fs,log,store}.rs` | CJK/字体/跳帧/I/O/日志/设置 | 无 | **不动**（本次仅按职责归位到 `ui/` 与 `platform/`） | 全部 |
| `native/host/*` | 目录列举、图片解码、日志 | 日志曾有两份实现 | 合并为 `host/yunyin_log.h` 一份 | 对外行为 |
| `native/rs/source/*` | — | 不存在 | **新增**：AudioSource 接缝、本地源、字节缓存、HTTP 源 | — |
| `native/rs/net/*` | — | 不存在 | **新增**：HTTP 类型 + 传输接缝（`yhttp.c` 的 Rust 面） | — |
| `native/rs/provider/*` | — | 不存在 | **新增**：Provider 接缝、`AudioInfo`、格式嗅探、网易云 | — |
| `native/yhttp.c/.h` | — | 不存在 | Phase 0/2 新增（上一次实验的残留已备份，未污染仓库） | — |
| `scripts/build-vpk.py` | 构建 VPK | 曾把任意失败当成 SCE 对齐问题重试 24 次 | 已修：先探 ELF，非对齐失败立刻报错 | 构建主流程 |
| `app/app.tsx` | UI + 曲库扫描 | 无在线入口 | Phase 5 加在线曲库/缓冲状态 | 现有本地流程与 UI |

## 三、不可动清单（改动必须写明原因）

```text
bgm.rs        BGM 口生命周期、960 帧块、输出调用
power.rs      全部
ps_lock.rs    全部
yplayer.c     六个 decoder 的核心 decode loop
yaac.c        SceAudiodec 调用（硬件解码）
tags.rs       本地标签/封面解析（含 M4A ilst）
CJK / font / UI
```

## 四、风险矩阵

状态只有三种：**已确认**（本轮读了源码/头文件/实测）、**待实机验证**、
**待源码确认**。

| 项目 | 状态 | 依据 / 待办 |
| --- | --- | --- |
| Decoder callback IO | **已确认** | 六个格式均有回调式打开 API（见 `ARCHITECTURE.md` 表格，含头文件行号） |
| callback 表达 WouldBlock | **已确认：不能** | 各库 read 回调返回 0/短读即 EOF 或错误：opusfile `0/负值=错误`、vorbisfile `0=EOF`、dr_wav/dr_flac 短读视为结束、mpg123 `0=EOF` |
| HTTP / Range / 206 | 待实机验证 | Phase 0 冒烟测试（`GET` + `Range: bytes=0-1023`，必须 206） |
| HTTPS / TLS | 待实机验证 | 需确认 SceSsl 证书链与 SNI；CDN 证书有效期 |
| Cookie / Referer | 待实机验证 | 网易云无会话 cookie 时多数歌曲拿不到 URL |
| Redirect | 待实机验证 | 策略已定：只允许同 scheme 跳转（`net/http.rs::redirect_allowed`，已测） |
| 网络权限 / `ATTRIBUTE2` | 待实机验证 | 现包 `ATTRIBUTE2=12` + authid `0x2800000000000001`；Phase 0 实测 socket 是否可用 |
| MP3 seek（远端） | 待源码确认 | `MPG123_FORCE_SEEKABLE` 会强制扫描全流求长度；远端应改用 `mpg123_set_filesize()`（Content-Length）并保留 seek 回调 |
| FLAC/OGG/Opus/M4A seek（远端） | 待实机验证 | 都需要「头部 + 尾部」两段 Range 才能正常 seek |
| URL 过期 | 已实现策略，待实机验证 | `UrlCache` + 3 分钟提前量 + 403/404/410 失效重取 |
| 内存 | 待实机验证 | 预算：字节缓存 1 MiB + 头/尾窗口 20 KiB + TLS 缓冲；Phase 0 用日志测量 |
| 线程同步 | 已设计 | 只有「Network 写缓存 / Audio 读缓存」一处共享点，用现有 `ps_lock`/`Mutex` 风格 |
| LiveArea 撕页退出 | 已确认（现状保留） | 声音在自身进程 BGM 口，撕页即停；网络线程同样属本进程 |
| M4A 本地播放 | 已确认（真机 + 电脑双向验证） | 重构不得回退此项 |

## 五、十个关键问题（答案都来自源码或实测，不猜）

1. **六个 decoder 是否支持 callback IO？** 是。mpg123：`mpg123_replace_reader_handle`；
   vorbisfile：`ov_open_callbacks`；dr_wav：`drwav_init(onRead,onSeek,userdata)`；
   dr_flac：`drflac_open(onRead,onSeek,userdata)`；opusfile：`op_open_callbacks`；
   M4A：`ym4a.c` 是自己写的，需要改成 `yp_io`。
2. **callback 能表达 WouldBlock 吗？** 不能。各库的读回调以 0/短读表示
   EOF 或错误，没有「稍后再来」的语义。
3. **那怎么避免 decoder 看到「暂时没数据」？** 用 **Gate**：`available() < decode_margin`
   时音频线程不调用 decoder，直接输出静音；缓存补到水位以上才恢复。这样
   decoder 永远不会因为「网络慢」而结束播放。
4. **`MPG123_FORCE_SEEKABLE` 在远端怎么办？** 它会让 mpg123 扫描整条流来确定长度
   （头文件原文：无法确定大小时「stream is assumed as non-seekable unless overridden」）。
   远端做法：不设该 flag，改为 `mpg123_set_filesize(Content-Length)`，seek 交给我们的
   回调完成；本地文件保持现状。
5. **网络时长不可靠时怎么给 duration hint？** Provider 的 `AudioInfo.duration_ms` 先给
   UI 用；解码器就绪后以其自报时长覆盖。M4A 的时长来自容器样本表，天然可靠，
   可作为对照。
6. **没有扩展名如何识别格式？** `AudioFormat::sniff(head)`（已实现、已用六个真实文件
   验证，含 M4A 的 `ftyp`）。
7. **URL 过期如何重新获取？** `UrlCache` + `EXPIRY_MARGIN_MS`；传输层拿到
   403/404/410 时 `invalidate(song_id)`，重取一次并重试。
8. **切歌如何取消 HTTP？** `HttpClient::cancel` + `RangeFetcher::cancel`
   + `SourceError::Cancelled`；用会话序号（generation）丢弃迟到的响应。
9. **Vita 实际网络内存需要多少？** 待实机验证：Phase 0 打印 socket/TLS 缓冲与
   可用堆，再决定 512 KiB / 1 MiB / 2 MiB。
10. **`ATTRIBUTE2=12` 是否足够网络播放？** 待实机验证（Phase 0 第一件事）。

## 六、Phase 计划

| Phase | 内容 | 验收 |
| --- | --- | --- |
| **0** | HTTP 冒烟测试：`GET` + `Range` + 206 + cookie + 取消；顺带测网络内存与权限 | 真机日志给出 206 与完整响应头 |
| **1** | Decoder IO 抽象：`yp_io` + `yp_open_io` + handle 化（六个格式）+ duration hint + Gate | **本地六个格式播放不回退**（本文件第一条铁律） |
| **2** | `HttpRangeSource` 落地：头部/尾部窗口、按需 Range、seek、断网恢复 | 远端文件可以 seek、暂停、断网 30 秒后继续 |
| **3** | `NetEaseProvider`：API + 加密 + 账号 + URL 缓存 | songId → 能播的 `AudioInfo` |
| **4** | 账号/歌单/歌词，扫码登录 | 歌单能整列表播放 |
| **5** | UI：在线曲库、缓冲状态、错误提示 | UI 不出现「假播放」 |

## 七、本轮实际改动

新增（骨架，**不接入运行时**，但已编译并测试）：

```text
native/rs/source/{mod,local,http,cache}.rs    AudioSource 接缝 + 本地源 + 字节缓存 + HTTP 源（Phase 2 桩）
native/rs/net/{mod,http}.rs                   HTTP 类型 + 传输接缝 + 跳转策略
native/rs/provider/{mod}.rs                   Provider 接缝 + AudioInfo + 格式嗅探
native/rs/provider/netease/{mod,api,crypto,account,resolve}.rs
docs/ARCHITECTURE.md docs/REFACTOR_PLAN.md
```

目录归位（纯移动 + 引用更新，行为不变）：

```text
native/{yplayer,ym4a,yaac}.{c,h}   → native/audio/
native/{yunyin_image,yunyin_listdir}.c, yunyin_log.h → native/host/
native/stb_image.h                 → native/vendor/
native/rs/{power,ps_lock,fs,log,store}.rs            → native/rs/platform/
native/rs/{cjk_host,font_gpu,frame_skip,offload_local}.rs → native/rs/ui/
```

`yunyin_listdir.c` 里那份重复的日志实现已删除，全 C 侧共用 `host/yunyin_log.h`。

验证方式（都在电脑上针对**要发布的源码**跑）：

* 六个真实文件的格式嗅探全部正确；合成魔数（ID3/帧同步/OggS+Vorbis/OggS+Opus/fLaC/RIFF/fTyp）全部正确；
* `LocalFileSource` 的 read/seek/tell/size/available/EOF 语义，含「末尾短读不算 EOF」；
* `ByteCache` 水位（0 / 32 KiB / 557 KiB / EOF）与 seek 重置；
* `UrlCache` 的 3 分钟提前失效、按 (song_id, quality) 命中、403 失效；
* VPK 照常打包（骨架模块编入宿主，未接入播放路径）。
