//! Byte cache — the compressed-byte window the network thread fills and the
//! decoders drain through `AudioSource` (task book §12/§13).
//!
//! Why compressed bytes and not PCM (§10): a PCM ring costs megabytes of RAM
//! and duplicates work the decoders already buffer.  A byte window is ~1 MiB and
//! keeps seek cheap, because a seek is just another range request.
//!
//! Watermarks (configurable, §13 — these are the documented starting values):
//!
//! ```text
//! capacity      1 MiB
//! high_water  768 KiB   above this the network thread can idle
//! refill      512 KiB   below this the network thread starts fetching
//! decode_margin 64 KiB  below this the audio thread stops calling the decoder
//! low_water   128 KiB
//! ```
//!
//! `available()` never reports more than what is really buffered: an empty cache
//! must look empty so the gate can mute instead of letting a decoder see EOF.
#![allow(dead_code)]

use alloc::vec::Vec;

/// Tunables for one cache.  Defaults follow §13; they live here so nobody
/// hard-codes a number in a second place.
#[derive(Clone, Copy, Debug)]
pub struct CacheConfig {
    pub capacity: usize,
    pub high_water: usize,
    pub refill: usize,
    pub decode_margin: usize,
    pub low_water: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            capacity: 1024 * 1024,
            high_water: 768 * 1024,
            refill: 512 * 1024,
            decode_margin: 64 * 1024,
            low_water: 128 * 1024,
        }
    }
}

/// A contiguous window of the stream: `start` is the file offset of `bytes[0]`.
#[derive(Default)]
pub struct ByteCache {
    cfg: CacheConfig,
    bytes: Vec<u8>,
    start: u64,
    eof: bool,
}

impl ByteCache {
    pub fn new(cfg: CacheConfig) -> Self {
        Self {
            /* Grown as data arrives: `capacity` is the budget the window may
             * reach, not memory we take up front. */
            bytes: Vec::new(),
            cfg,
            start: 0,
            eof: false,
        }
    }

    /// Drop everything and forget EOF — used on seek and on track change.
    pub fn reset(&mut self) {
        self.bytes.clear();
        self.start = 0;
        self.eof = false;
    }

    pub fn config(&self) -> CacheConfig {
        self.cfg
    }

    /// Where the window currently begins.
    pub fn start(&self) -> u64 {
        self.start
    }

    /// How many bytes can be handed to a decoder right now.
    pub fn available(&self) -> usize {
        self.bytes.len()
    }

    pub fn mark_eof(&mut self) {
        self.eof = true;
    }

    pub fn is_eof(&self) -> bool {
        self.eof
    }

    /// Should the network thread fetch?  (§13: below `refill`, up to
    /// `high_water`.)
    pub fn wants_refill(&self) -> bool {
        !self.eof && self.bytes.len() < self.cfg.refill
    }

    /// Is it safe to call the decoder?  (§65: never let a decoder probe the
    /// network, so it only runs with `decode_margin` bytes in hand.)
    pub fn can_decode(&self) -> bool {
        self.bytes.len() >= self.cfg.decode_margin || self.eof
    }

    /// Append freshly downloaded bytes that continue the window.
    pub fn push(&mut self, data: &[u8]) -> usize {
        let room = self.cfg.capacity.saturating_sub(self.bytes.len());
        let n = room.min(data.len());
        self.bytes.extend_from_slice(&data[..n]);
        n
    }

    /// Read from the window, consuming it.  Returns the number of bytes copied.
    pub fn read(&mut self, dst: &mut [u8]) -> usize {
        let n = dst.len().min(self.bytes.len());
        dst[..n].copy_from_slice(&self.bytes[..n]);
        self.bytes.drain(..n);
        self.start += n as u64;
        n
    }

    /// Move the window to an absolute offset (§53 seek): the caller then refills
    /// with a range request that starts at `pos`.
    pub fn seek(&mut self, pos: u64) {
        self.reset();
        self.start = pos;
    }
}
