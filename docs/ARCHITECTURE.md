# YUNYIN 播放引擎架构（目标形态）

本文描述「本地 + 在线」统一播放引擎的目标架构。它是重构任务书的落地版本，
并已按**当前实际代码**修正（任务书写于 M4A 支持之前，见文末差异一节）。

## 一条链路

```text
        PocketJS UI (app/app.tsx)
                │  vitaMedia.*
                ▼
        Source Resolver            ← provider/{netease} 只在这一层之上
                │
        ┌───────┴────────┐
        ▼                ▼
 LocalFileSource   NetEaseProvider → AudioInfo(URL,格式,时长,过期)
        │                │
        └───────┬────────┘
                ▼
          AudioSource              ← 唯一接缝：上层不认识 HTTP，下层不认识网易云
                │
        ┌───────┴────────┐
        ▼                ▼
  (本地：直接读)   HttpRangeSource ── Network Thread ── Byte Cache
                │
                ▼
        Audio Thread: Gate → Decoder → sceAudioOutOutput (现有 BGM 口)
```

## 六个格式，一条输入层

解码器**不更换**，只把「输入」从路径换成抽象 IO：

| 格式 | 解码器 | 现在的打开方式 | 回调式打开（已确认存在） |
| --- | --- | --- | --- |
| MP3 | mpg123 | `mpg123_open(path)` | `mpg123_replace_reader_handle()` / `mpg123_reader64` |
| OGG | vorbisfile | `ov_fopen(path)` | `ov_open_callbacks()`（还接受初始头部缓冲） |
| WAV | dr_wav | `drwav_init_file(path)` | `drwav_init(wav, onRead, onSeek, pUserData)` |
| FLAC | dr_flac | `drflac_open_file(path)` | `drflac_open(onRead, onSeek, pUserData)` |
| OPUS | opusfile | `op_open_file(path)` | `op_open_callbacks()` |
| **M4A** | `ym4a.c` + `yaac.c` | `ym4a_open(path)`（内部 `FILE*`） | 自己实现，改走同一份 `yp_io` |

结论：**六个格式都能只换输入层**。`yaac.c`（SceAudiodec 硬件解码）不碰 IO，
不受影响；需要改造的是 `ym4a.c` 的文件读取部分。

## 两个线程

```text
Network Thread                       Audio Thread（现有，保持 960 帧模型）
──────────────                       ────────────────────────────────
HttpRangeSource 的补数据请求           Gate：缓存不足 → 输出静音，不调 Decoder
HTTP / TLS / Range / Cookie          Decoder：六个格式的解码循环
取消（切歌即取消）                     sceAudioOutOutput
断网恢复 / URL 过期重取
```

为什么不是 PCM Ring：PCM 环要几 MB 内存，而解码器自己已有缓冲；压缩字节窗口
只要 1 MiB，而且 seek 直接变成「换一个 Range 请求」。

## AudioSource 契约

```text
read() == 0   仅代表真正 EOF
available()   不阻塞即可读出的字节数（Gate 与水位看它）
WouldBlock    暂时没数据，但流还活着 —— 与 Eof 严格区分
```

这条规则是整个设计的地基：一旦「暂时没数据」被误报成 EOF，解码器就会提前结束
播放；反过来，如果让解码器自己等网络，音频线程就会被网络卡住。

## Byte Cache 水位（初始值，可调）

```text
capacity       1 MiB
high_water   768 KiB     以上网络线程可以闲着
refill       512 KiB     低于此值网络线程开始补
decode_margin 64 KiB     低于此值音频线程不再调 Decoder，直接静音
low_water    128 KiB
```

`read()` 永远不返回「比真实缓存更多」的字节：空缓存必须看起来就是空的。

## 格式识别不看后缀

`provider::AudioFormat::sniff(head)` 按魔数判断，已覆盖六种格式：

```text
fLaC                 → flac
OggS + OpusHead      → opus
OggS + vorbis        → ogg
????ftyp             → m4a      （MP4 家族；ftyp 不在 0 偏移）
RIFF????WAVE         → wav
ID3 / 0xFFEx         → mp3
```

网易云的 CDN 链接没有可用后缀，识别只能靠字节。

## 模块职责

目录也是按这个分工分的：

```text
native/
├── audio/     yplayer.c/.h（六个格式的解码循环与对外 API）
│              ym4a.c/.h（M4A 解复用）  yaac.c/.h（SceAudiodec 硬件 AAC）
├── host/      yunyin_listdir.c（目录列举） yunyin_image.c（图片解码）
│              yunyin_log.h（唯一一份日志实现）
├── net/       yhttp.c/.h（Phase 0 薄传输层：SceNet + SceSsl + SceHttp）
├── vendor/    stb_image.h  dr_wav.h  dr_flac.h  opus/
├── libs/      预编译静态库（mpg123 / vorbisfile / vorbis / ogg / opusfile / opus）
└── rs/        Rust 宿主：bgm.rs decoder.rs bridge.rs tags.rs
              + source/ net/ provider/（引擎接缝）
              + platform/（电源·键锁·文件·日志·设置）
              + ui/（流式 CJK·字体图集·跳帧）
```

| 模块 | 职责 | 现在 |
| --- | --- | --- |
| `rs/source/` | AudioSource 接缝、本地实现、HTTP Range 源、字节缓存 | 本地/缓存/接口已实现并测试；HTTP 源为 Phase 2 桩 |
| `rs/net/` | 传输层（HTTP/TLS/Range/Cookie/取消）+ Phase 0 探针驱动 | 类型/接缝已就位；探针可实机运行，生产路径待 Phase 2 |
| `rs/provider/` | Provider 接缝 + `AudioInfo` + 格式嗅探 | 已实现 |
| `rs/provider/netease/` | 网易云 API/加密/账号/URL 缓存 | URL 缓存策略已实现，API 待 Phase 3 |
| `rs/bgm.rs` | BGM 口生命周期 + 音频线程 | **不动** |
| `rs/decoder.rs` | 调 `yplayer.c` 的 FFI | Phase 1 加 handle/IO/duration |
| `rs/platform/` `rs/ui/` | 系统服务 / 界面支撑 | **本重构不动**（仅归位） |
| `audio/yplayer.c` | 六个格式的解码循环 | **解码循环不动**，只换输入层 |

## 与重构任务书的差异（因为新增了 M4A）

1. **六个解码器，不是五个**：M4A(AAC) 走 `ym4a.c` + 硬件 `yaac.c`。
   任务书里「五个 decoder」「五个 decode loop」的表述按六个理解。
2. **`ym4a.c` 也要 IO 化**：它是自己写的解复用器，用 `FILE*` 读文件；
   流式化时必须改走同一份 `yp_io`（任务书未涉及，因为当时还没有 M4A）。
3. **格式嗅探多了 `ftyp`**：M4A 的容器标记在偏移 4，不在 0。
4. **本地兼容性多一项**：M4A 已经在真机跑通（含硬件解码），
   重构时必须保持（任务书的「本地播放必须保持兼容」清单里加上 M4A）。
5. **URL 缓存/格式提示**：M4A 的时长来自容器样本表（不是解码器猜测），
   所以 `duration_ms` 对 M4A 是可靠的，可以作为 duration hint 的对照组。
