//! `HttpRangeSource` — the network side of the seam (Phase 2, task book §78).
//!
//! 当前状态：只有接口与设计。在 `net::http` 能在 Vita 上真正发 Range 请求之前，
//! 每个方法都返回 `Unsupported`。
//!
//! 形态照抄 cspot 的 `CDNAudioFile`（§7 指定的参考实现）—— 同样的硬件、同样的问题，
//! 它已经解过一遍：
//!
//! ```text
//! open  -> Range: bytes=0-8191        头部窗口：解码器在这里嗅探魔数
//!       -> Range: bytes=-12288        尾部窗口：Ogg/Opus 的 seek 需要文件尾
//! read  -> 先从字节缓存取；取不到就按当前位置发一次约 14 KiB 的 Range
//!          （绝不做"解码器每读一次就发一次请求"）
//! seek  -> 移动游标、丢掉窗口；下一次 read 重新取，并留一点 margin，
//!          这样小幅回退不用重新请求（§54）
//! ```
//!
//! §15 的两条推论：
//!   - 解码器永远看不到 socket：它只看到缓存里的字节，或者 `WouldBlock`；
//!   - 这里的 seek 以**字节**为单位，"时间 → 字节"由解码器自己换算。
#![allow(dead_code)]

use super::cache::{ByteCache, CacheConfig};
use super::{AudioSource, SourceError, SourceKind};
use alloc::string::String;

/// 一次取数请求：从哪开始、要多少。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FetchRequest {
    pub offset: u64,
    pub len: usize,
}

/// 交给网络线程的接口：`HttpRangeSource` 只申明"我要什么"，
/// 网络线程负责真正传输，并把字节推回来（§9）。
pub trait RangeFetcher {
    /// 从 `offset` 开始传；后一次调用会顶掉前一次（§56）。
    fn request(&mut self, req: FetchRequest) -> Result<(), SourceError>;
    /// 取消正在进行的传输（切歌时用）。
    fn cancel(&mut self) -> Result<(), SourceError>;
}

/// 尺寸取自参考实现。两个值都故意取小 —— Vita 上稀缺的是网络缓冲（§66）。
pub const HEADER_WINDOW: usize = 8 * 1024;
pub const FOOTER_WINDOW: usize = 12 * 1024;
pub const SEEK_MARGIN: u64 = 4 * 1024;

pub struct HttpRangeSource {
    url: String,
    cache: ByteCache,
    pos: u64,
    /// 服务器给了 Content-Length 时的流总长度。
    size: Option<u64>,
    error: Option<SourceError>,
}

impl HttpRangeSource {
    /// 绑定到已解析出的 URL。URL 的生命周期归调用方（Provider）：
    /// CDN 回 403/404 时由 Provider 重新解析并重建这个对象
    /// —— 这就是 `SourceError::Expired` 存在的原因（§52）。
    pub fn new(url: &str, cfg: CacheConfig) -> Self {
        Self {
            url: String::from(url),
            cache: ByteCache::new(cfg),
            pos: 0,
            size: None,
            error: None,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// 网络线程下一次该取什么；窗口已经在 refill 水位之上时返回 `None`。
    pub fn next_fetch(&self) -> Option<FetchRequest> {
        if self.cache.is_eof() {
            return None;
        }
        if !self.cache.wants_refill() && self.cache.available() > 0 {
            return None;
        }
        Some(FetchRequest {
            offset: self.cache.start() + self.cache.available() as u64,
            len: self.cache.config().refill,
        })
    }

    pub fn cache(&self) -> &ByteCache {
        &self.cache
    }

    /// 把下载好的字节交给窗口（由网络线程调用）。
    pub fn deliver(&mut self, data: &[u8]) {
        self.cache.push(data);
    }
}

impl AudioSource for HttpRangeSource {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, SourceError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        if self.cache.available() > 0 {
            let n = self.cache.read(buf);
            self.pos += n as u64;
            return Ok(n);
        }
        if self.cache.is_eof() {
            return Ok(0); /* real end of stream */
        }
        /* 缓存空了：Gate 会在窗口回到 `decode_margin` 之上前挡住解码器，
         * 所以"请稍后再来"才是诚实的回答。 */
        Err(SourceError::WouldBlock)
    }

    fn seek(&mut self, pos: u64) -> Result<(), SourceError> {
        self.cache.seek(pos);
        self.pos = pos;
        Ok(())
    }

    fn tell(&self) -> u64 {
        self.pos
    }

    fn size(&self) -> Option<u64> {
        self.size
    }

    fn available(&self) -> usize {
        self.cache.available()
    }

    fn is_eof(&self) -> bool {
        self.cache.is_eof() && self.cache.available() == 0
    }

    fn error(&self) -> Option<SourceError> {
        self.error.clone()
    }

    fn kind(&self) -> SourceKind {
        SourceKind::HttpRange
    }
}
