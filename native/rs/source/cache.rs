//! 字节缓存 —— 网络线程往里填、解码器经 `AudioSource` 往外取的"压缩字节窗口"
//! （任务书 §12/§13）。
//!
//! 为什么缓存压缩字节而不是 PCM（§10）：PCM 环要几 MB 内存，而且重复了解码器
//! 自己已经有的缓冲。字节窗口只要约 1 MiB，而且 seek 很便宜 —— 换一个 Range 请求就行。
//!
//! 水位（可配置，§13 的初始取值）：
//!
//! ```text
//! capacity      1 MiB
//! high_water  768 KiB   高于它，网络线程可以闲着
//! refill      512 KiB   低于它，网络线程开始补数据
//! decode_margin 64 KiB  低于它，音频线程不再调解码器（只输出静音）
//! low_water   128 KiB
//! ```
//!
//! `available()` 绝不虚报：空缓存必须看起来就是空的，这样 Gate 才能"静音"而不是
//! 让解码器误以为遇到 EOF。
#![allow(dead_code)]

use alloc::vec::Vec;

/// 一个缓存的全部可调参数。默认值照 §13；集中在这里，避免别处再硬编码一份。
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

/// 流上的一段连续窗口：`start` 是 `bytes[0]` 在文件里的偏移。
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
             * 的预算，不是一开始就占掉的内存。 */
            bytes: Vec::new(),
            cfg,
            start: 0,
            eof: false,
        }
    }

    /// 清空并忘掉 EOF —— seek 和切歌时用。
    pub fn reset(&mut self) {
        self.bytes.clear();
        self.start = 0;
        self.eof = false;
    }

    pub fn config(&self) -> CacheConfig {
        self.cfg
    }

    /// 当前窗口的起始偏移。
    pub fn start(&self) -> u64 {
        self.start
    }

    /// 现在能交给解码器的字节数。
    pub fn available(&self) -> usize {
        self.bytes.len()
    }

    pub fn mark_eof(&mut self) {
        self.eof = true;
    }

    pub fn is_eof(&self) -> bool {
        self.eof
    }

    /// 网络线程该去补数据了吗？（§13：低于 refill 就去补，补到 high_water 为止。）
    pub fn wants_refill(&self) -> bool {
        !self.eof && self.bytes.len() < self.cfg.refill
    }

    /// 现在可以调解码器吗？（§65：绝不让解码器自己去碰网络，
    /// 所以手里至少有 `decode_margin` 字节才调。）
    pub fn can_decode(&self) -> bool {
        self.bytes.len() >= self.cfg.decode_margin || self.eof
    }

    /// 把刚下载的、紧接窗口末尾的字节追加进来。
    pub fn push(&mut self, data: &[u8]) -> usize {
        let room = self.cfg.capacity.saturating_sub(self.bytes.len());
        let n = room.min(data.len());
        self.bytes.extend_from_slice(&data[..n]);
        n
    }

    /// 从窗口读走数据（消费掉），返回拷贝的字节数。
    pub fn read(&mut self, dst: &mut [u8]) -> usize {
        let n = dst.len().min(self.bytes.len());
        dst[..n].copy_from_slice(&self.bytes[..n]);
        self.bytes.drain(..n);
        self.start += n as u64;
        n
    }

    /// 把窗口移到某个绝对偏移（§53 的 seek）：之后调用方从这个 `pos`
    /// 发一个 Range 请求补数据。
    pub fn seek(&mut self, pos: u64) {
        self.reset();
        self.start = pos;
    }
}
