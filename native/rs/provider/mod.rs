//! Provider 与"格式判定"（任务书 §41/§42）。
//!
//! Provider 只回答一个问题："给我一个歌曲 ID，字节在哪、是什么？" —— 然后返回
//! `AudioInfo`。它永远不返回 socket，解码器也永远看不到 Provider 的对象（§15）。
//!
//! `AudioFormat::sniff` 也放在这里：§38/§39 明确要求格式**只能由字节判断**，
//! 不能靠 URL 后缀 —— 网易云 CDN 链接根本没有什么有用的后缀。
#![allow(dead_code)]

pub mod netease;

use alloc::format;
use alloc::string::String;

/// 音质请求；各 Provider 自己映射到自家的档位（§47）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    /// 账号权限允许的最好音质。
    Auto,
    Low,
    Medium,
    High,
    Lossless,
}

/// 我们**确实能解**的容器/编码。
///
/// 目前六大类：五个库解码器，加上 M4A(AAC) —— 由 `ym4a.c` 解复用、
/// `yaac.c` 交给硬件解码块。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioFormat {
    Mp3,
    OggVorbis,
    Opus,
    Wav,
    Flac,
    M4a,
    Unknown,
}

impl AudioFormat {
    /// 传给 `yplayer.c` 的格式名：让它直接挑对解码器，不必对网络流再嗅一次。
    pub fn name(&self) -> &'static str {
        match self {
            AudioFormat::Mp3 => "mp3",
            AudioFormat::OggVorbis => "ogg",
            AudioFormat::Opus => "opus",
            AudioFormat::Wav => "wav",
            AudioFormat::Flac => "flac",
            AudioFormat::M4a => "m4a",
            AudioFormat::Unknown => "unknown",
        }
    }

    /// 当前播放器能不能解这个格式。
    pub fn is_playable(&self) -> bool {
        !matches!(self, AudioFormat::Unknown)
    }

    /// 用开头几个字节判断容器类型（§39）。
    ///
    /// 顺序是按"容易被搞混的排前面"定的：Ogg 要判两次（Vorbis / Opus），
    /// MP4 的 `ftyp` 不在偏移 0，而在前面 4 字节的长度字段之后。
    pub fn sniff(head: &[u8]) -> Self {
        if head.len() >= 4 && &head[0..4] == b"fLaC" {
            return AudioFormat::Flac;
        }
        if head.len() >= 4 && &head[0..4] == b"OggS" {
            /* Ogg 既能装 Vorbis 也能装 Opus；编码名就在第一个 Ogg 页里、
             * 靠前的位置。 */
            let probe = &head[..head.len().min(64)];
            if contains(probe, b"OpusHead") {
                return AudioFormat::Opus;
            }
            if contains(probe, b"vorbis") {
                return AudioFormat::OggVorbis;
            }
            return AudioFormat::OggVorbis; /* Ogg without either tag: treat as Vorbis */
        }
        if head.len() >= 12 && &head[4..8] == b"ftyp" {
            return AudioFormat::M4a;
        }
        if head.len() >= 12 && &head[8..12] == b"WAVE" {
            return AudioFormat::Wav;
        }
        if head.len() >= 3 && &head[0..3] == b"ID3" {
            return AudioFormat::Mp3;
        }
        if head.len() >= 2 && head[0] == 0xFF && (head[1] & 0xE0) == 0xE0 {
            return AudioFormat::Mp3; /* MPEG frame sync, tag-less MP3 */
        }
        AudioFormat::Unknown
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// 播放一条流所需要的**全部**信息，仅此而已。
///
/// 多出来的字段（§41 的 `source`/`song_id`/`quality`/`expires_at`）是给界面和
/// "URL 过期后重新解析"用的；**解码器不许读它们**。
#[derive(Clone, Debug)]
pub struct AudioInfo {
    pub url: String,
    pub format: AudioFormat,
    /// Provider 声称的时长。未知时是 0 —— 那就以解码器报的为准（§37）。
    pub duration_ms: u32,
    pub bitrate: u32,
    pub size: Option<u64>,
    pub source: String,
    pub song_id: String,
    pub quality: Quality,
    /// 超过这个绝对时间（毫秒时间戳）后 `url` 必须重新解析。
    pub expires_at: u64,
}

impl AudioInfo {
    /// 本地文件永不过期，背后也没有 Provider。
    pub fn local(path: &str, format: AudioFormat, size: Option<u64>) -> Self {
        Self {
            url: String::from(path),
            format,
            duration_ms: 0,
            bitrate: 0,
            size,
            source: String::from("local"),
            song_id: String::new(),
            quality: Quality::Auto,
            expires_at: 0,
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "{} {} {}ms bitrate={} size={:?} src={}",
            self.source,
            self.format.name(),
            self.duration_ms,
            self.bitrate,
            self.size,
            self.song_id
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderError {
    /// 还没有 Provider 能处理这个 ID。
    Unsupported,
    Network(String),
    /// 缺少或无效的 Cookie/会话（§48）。
    Auth(String),
    /// 账号权限不够，拿不到这个音质（§47）。
    VipRequired,
    NotFound,
    /// CDN URL 失效了，重新解析后再试（§52）。
    Expired,
    Cancelled,
}

/// Provider 接缝。以后要加 QQ 音乐，只需新增一个实现，不用改播放器（§43）。
pub trait MusicProvider {
    fn name(&self) -> &'static str;
    fn resolve(&self, song_id: &str, quality: Quality) -> Result<AudioInfo, ProviderError>;
}
