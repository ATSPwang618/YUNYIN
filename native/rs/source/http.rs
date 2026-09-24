//! `HttpRangeSource` —— 网络侧的 `AudioSource`（Phase 2，任务书 §17-§19/§78）。
//!
//! 形状照参考实现 cspot 的 `CDNAudioFile`：
//!
//! ```text
//! 取数线程（网络线程）           消费端（解码器所在的音频线程）
//! ──────────────────            ──────────────────────────────
//! 按窗口抓字节（256 KiB）   ──→   窗口里有就直接拷走
//! 窗口不够就继续抓               没有就等（条件变量），最多等 READ_WAIT_MS
//! seek 时丢掉窗口重抓            真正的结束只有"已 EOF 且窗口空"才报
//! ```
//!
//! 两条硬约束都落在这里：
//   - 解码器**看不到 socket**：它只看到窗口里的字节，或者"稍后再来"；
//   - 绝不做"解码器读一次就发一次请求"：一个窗口只发一次 Range。
//!
//! `ByteTransport` 把"怎么取字节"抽出来，Vita 上用 `net::http::Stream`，
// 电脑上用一个内存假实现 —— 窗口/seek/EOF 这套逻辑因此可以离线验证。
#![allow(dead_code)]

use super::cache::CacheConfig;
use super::{AudioSource, SourceError, SourceKind};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// 一次 Range 抓多少（任务书 §13 的 refill 量级；也是"每窗口一次请求"的粒度）。
pub const WINDOW_BYTES: usize = 256 * 1024;
/// 消费端等数据的上限：等不到就返回"稍后再来"，由上层 Gate 决定是否静音。
pub const READ_WAIT_MS: u64 = 4000;

/// "从哪儿按偏移取字节"的最小接口。
pub trait ByteTransport: Send {
    /// 从绝对偏移 `off` 读最多 `dst.len()` 字节；`Ok(0)` = 真正结束。
    fn read_at(&mut self, off: u64, dst: &mut [u8]) -> Result<usize, SourceError>;
    /// 总长度（未知则 None）。
    fn size(&self) -> Option<u64>;
}

/// 取数线程与消费端共享的窗口。
struct Window {
    buf: Vec<u8>,
    start: u64,
    eof: bool,
    err: Option<SourceError>,
    /// 消费端希望窗口从哪开始（seek 会改它）。
    want_from: u64,
    /// 请求代数：seek 后 +1，取数线程据此丢弃过期结果。
    generation: u64,
    /// 消费端已登记一个取数需求，等取数线程去做。
    pending: bool,
    cancelled: bool,
}

pub struct HttpRangeSource<T: ByteTransport + 'static> {
    shared: Arc<(Mutex<Window>, Condvar)>,
    pos: u64,
    size: Option<u64>,
    gen_hint: Arc<AtomicU64>,
    url: String,
    worker: Option<JoinHandle<()>>,
    /// 传输由取数线程独占，这里只是让类型参数有落脚点。
    _owns: PhantomData<T>,
}

impl<T: ByteTransport + 'static> HttpRangeSource<T> {
    /// `transport` 交给取数线程独占（只有它做 I/O）。
    pub fn new(url: &str, transport: T, _cfg: CacheConfig) -> Self {
        let size = transport.size();
        let shared = Arc::new((
            Mutex::new(Window {
                buf: Vec::new(),
                start: 0,
                eof: false,
                err: None,
                want_from: 0,
                generation: 0,
                pending: true, /* 打开就先抓第一窗 */
                cancelled: false,
            }),
            Condvar::new(),
        ));
        let gen_hint = Arc::new(AtomicU64::new(0));

        let worker = {
            let shared = Arc::clone(&shared);
            let gen_hint = Arc::clone(&gen_hint);
            let mut st = transport;
            std::thread::Builder::new()
                .name("yunyin-net-http".into())
                .stack_size(64 * 1024)
                .spawn(move || {
                    let mut tmp = vec![0u8; WINDOW_BYTES];
                    loop {
                        let mut gen;
                        let from;
                        {
                            let (lock, cv) = &*shared;
                            let Ok(mut w) = lock.lock() else { break };
                            // 有需求才干活；没需求就睡着等消费端登记。
                            while !w.cancelled && !w.pending {
                                match cv.wait_timeout(w, Duration::from_millis(200)) {
                                    Ok((g, _)) => w = g,
                                    Err(e) => w = e.into_inner().0,
                                }
                            }
                            if w.cancelled {
                                break;
                            }
                            from = w.want_from;
                            gen = w.generation;
                            w.pending = false; /* 这一抓由我负责 */
                            w.buf.clear();
                            w.start = from;
                            w.eof = false; /* 这一窗的结果等下重新判定 */
                        }
                        let got = st.read_at(from, &mut tmp);
                        {
                            let (lock, cv) = &*shared;
                            let Ok(mut w) = lock.lock() else { break };
                            if w.cancelled {
                                break;
                            }
                            if w.generation != gen {
                                // 期间发生了 seek：这一抓作废。
                                w.pending = false;
                                cv.notify_all();
                                continue;
                            }
                            match got {
                                Ok(0) => w.eof = true,
                                Ok(n) => {
                                    w.buf.extend_from_slice(&tmp[..n]);
                                    w.start = from;
                                }
                                Err(e) => w.err = Some(e),
                            }
                            w.pending = false;
                            gen_hint.store(w.generation, Ordering::Release);
                            cv.notify_all();
                        }
                    }
                })
                .ok()
        };

        Self {
            shared,
            pos: 0,
            size,
            gen_hint,
            url: String::from(url),
            worker,
            _owns: PhantomData,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// 让取数线程把窗口挪到 `from`（内部用）。
    fn request_from(&self, from: u64) {
        let (lock, cv) = &*self.shared;
        if let Ok(mut w) = lock.lock() {
            w.want_from = from;
            w.generation = w.generation.wrapping_add(1);
            w.buf.clear();
            w.start = from;
            /*
             * "到过流末尾"是**上一个位置**的结论，不能跟着窗口一起搬过来：
             * 解码器探测完文件尾巴一定会 seek 回开头，那时这里必须重新允许取数，
             * 否则 wait_for 会因为陈旧的 eof 直接返回"没数据"，开门就是假 EOF。
             */
            w.eof = false;
            w.pending = true;
            cv.notify_all();
        }
        self.gen_hint.store(0, Ordering::Release);
    }

    /// 等窗口覆盖 `pos`（或 EOF / 出错）。
    fn wait_for(&self, pos: u64, timeout_ms: u64) -> bool {
        let (lock, cv) = &*self.shared;
        let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
        let Ok(mut w) = lock.lock() else { return false };
        loop {
            let covered = pos >= w.start && pos < w.start + w.buf.len() as u64;
            if covered || w.eof || w.err.is_some() || w.cancelled {
                return covered;
            }
            /* 窗口没盖住要的位置：登记需求叫醒取数线程（读尽当前窗口时走这里）。 */
            if !w.pending {
                w.want_from = pos;
                w.generation = w.generation.wrapping_add(1);
                w.buf.clear();
                w.start = pos;
                w.eof = false; /* 同上：换了位置，之前的"到底"结论作废 */
                w.pending = true;
                cv.notify_all();
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return false;
            }
            match cv.wait_timeout(w, deadline - now) {
                Ok((g, _)) => w = g,
                Err(e) => w = e.into_inner().0,
            }
        }
    }
}

impl<T: ByteTransport + 'static> AudioSource for HttpRangeSource<T> {
    fn read(&mut self, dst: &mut [u8]) -> Result<usize, SourceError> {
        let want = dst.len();
        let mut done = 0usize;
        if dst.is_empty() {
            return Ok(0);
        }
        /* 尽量填满：解码器（尤其是 m4a 的精确读）不接受"短读"，所以这里
         * 循环取数据，只有真正到流末尾才返回短读。 */
        while done < want {
            if let Some(e) = self.error() {
                return Err(e);
            }
            if self.is_eof() {
                break; /* 真正结束 */
            }
            if !self.wait_for(self.pos, READ_WAIT_MS) {
                if let Some(e) = self.error() {
                    return Err(e);
                }
                if self.is_eof() {
                    break;
                }
                if done > 0 {
                    break; /* 等超时：把已有的先给出去，上层 Gate 会决定是否静音 */
                }
                return Err(SourceError::WouldBlock);
            }
            {
                let (lock, _cv) = &*self.shared;
                let Ok(w) = lock.lock() else {
                    return Err(SourceError::Io(String::from("window lock")));
                };
                let off = (self.pos - w.start) as usize;
                let n = (want - done).min(w.buf.len() - off);
                dst[done..done + n].copy_from_slice(&w.buf[off..off + n]);
                drop(w);
                self.pos += n as u64;
                done += n;
            }
        }
        Ok(done)
    }

    fn seek(&mut self, pos: u64) -> Result<(), SourceError> {
        self.pos = pos;
        /* 窗口里已经有目标位置就白拿；否则让取数线程挪过去。 */
        let covered = {
            let (lock, _cv) = &*self.shared;
            let Ok(w) = lock.lock() else {
                return Err(SourceError::Io(String::from("window lock")));
            };
            pos >= w.start && pos < w.start + w.buf.len() as u64
        };
        if !covered {
            self.request_from(pos);
        }
        Ok(())
    }

    fn tell(&self) -> u64 {
        self.pos
    }

    fn size(&self) -> Option<u64> {
        self.size
    }

    fn available(&self) -> usize {
        let (lock, _cv) = &*self.shared;
        let Ok(w) = lock.lock() else { return 0 };
        if self.pos < w.start {
            return 0;
        }
        let off = (self.pos - w.start) as usize;
        w.buf.len().saturating_sub(off)
    }

    fn is_eof(&self) -> bool {
        /*
         * 长度已知时，"结束"只由**当前位置**决定。
         * 之前用"抓取线程报过 eof"来判断，结果解码器一探测（mpg123 打开流
         * 时会 seek 到很远处问长度，我就把那次越界当成整条流结束）之后，
         * 所有读都变成 0 = EOF，解码器直接放弃打开。
         */
        if let Some(sz) = self.size {
            return self.pos >= sz;
        }
        let (lock, _cv) = &*self.shared;
        let Ok(w) = lock.lock() else { return false };
        w.eof && self.pos >= w.start + w.buf.len() as u64
    }

    fn error(&self) -> Option<SourceError> {
        let (lock, _cv) = &*self.shared;
        let Ok(w) = lock.lock() else { return None };
        w.err.clone()
    }

    fn kind(&self) -> SourceKind {
        SourceKind::HttpRange
    }
}

impl<T: ByteTransport + 'static> Drop for HttpRangeSource<T> {
    fn drop(&mut self) {
        {
            let (lock, cv) = &*self.shared;
            if let Ok(mut w) = lock.lock() {
                w.cancelled = true;
                cv.notify_all();
            }
        }
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}
