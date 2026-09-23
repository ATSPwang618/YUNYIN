//! `HttpRangeSource` — the network side of the seam (Phase 2, task book §78).
//!
//! Phase 0/1 status: interface + design only.  Every method reports
//! `Unsupported` until `net::http` can do real range requests on the Vita.
//!
//! The shape is taken from cspot's `CDNAudioFile` (the reference in §7), which
//! is the same problem solved on the same hardware:
//!
//! ```text
//! open  -> Range: bytes=0-8191        (header window: decoders sniff magic here)
//!       -> Range: bytes=-12288        (footer window: Ogg/Opus seek needs the tail)
//! read  -> serve from the byte cache; on a miss, one range request of ~14 KiB
//!          at the current position (never one request per decoder read)
//! seek  -> move the cursor and drop the window; the next read refetches,
//!          keeping a small margin so a short backward seek stays in cache (§54)
//! ```
//!
//! Two consequences carried over from §15:
//!   - the decoders never see a socket: they see cached bytes or `WouldBlock`;
//!   - a seek is expressed in *bytes* here, and the decoder is responsible for
//!     mapping time to bytes.
#![allow(dead_code)]

use super::cache::{ByteCache, CacheConfig};
use super::{AudioSource, SourceError, SourceKind};
use alloc::string::String;

/// Where a fetch asked the network thread to start, and how much it wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FetchRequest {
    pub offset: u64,
    pub len: usize,
}

/// Network thread handoff.  `HttpRangeSource` publishes what it needs; the
/// network thread performs the transfer and pushes bytes back (§9).
pub trait RangeFetcher {
    /// Start a transfer at `offset`.  Later calls supersede earlier ones (§56).
    fn request(&mut self, req: FetchRequest) -> Result<(), SourceError>;
    /// Cancel whatever is in flight (track change).
    fn cancel(&mut self) -> Result<(), SourceError>;
}

/// Sizes from the reference implementation; both are small on purpose, because
/// the Vita's network buffers are the scarce resource (§66).
pub const HEADER_WINDOW: usize = 8 * 1024;
pub const FOOTER_WINDOW: usize = 12 * 1024;
pub const SEEK_MARGIN: u64 = 4 * 1024;

pub struct HttpRangeSource {
    url: String,
    cache: ByteCache,
    pos: u64,
    /// Total stream size from Content-Length, when the server sent one.
    size: Option<u64>,
    error: Option<SourceError>,
}

impl HttpRangeSource {
    /// Bind to a resolved URL.  The caller (a provider) owns URL lifetime; when
    /// the CDN answers 403/404 the provider re-resolves and this object is
    /// rebuilt, which is why `SourceError::Expired` exists (§52).
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

    /// What the network thread should fetch next, or `None` when the window is
    /// already above the refill watermark.
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

    /// Hand downloaded bytes to the window (called by the network thread).
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
        /* Out of cached bytes: the gate keeps the decoder away until the window
         * is back above `decode_margin`, so asking again is the honest answer. */
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
