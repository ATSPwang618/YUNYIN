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
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/*
 * 排障用的逐步轨迹（只在卡里有 `ux0:/data/yunyin/debug` 时写日志）。
 *
 * 为什么值得在正式代码里留这一小段：在线播放的失败几乎全在"取数线程 ↔ 解码线程"
 * 的时序里，只看两端的现象永远猜不出来 —— 上一次真机排障就是靠这类轨迹才把
 * "末尾"这个词的语义搞对的。上限 400 行，正常播放不会刷屏。
 */
static TRACE_LINES: AtomicU32 = AtomicU32::new(0);
static COUNTERS: [AtomicU32; 6] = [
    AtomicU32::new(0), /* 0 read 调用 */
    AtomicU32::new(0), /* 1 seek 调用 */
    AtomicU32::new(0), /* 2 取数次数 */
    AtomicU32::new(0), /* 3 取数结果被作废次数 */
    AtomicU32::new(0), /* 4 等超时次数 */
    AtomicU32::new(0), /* 5 WouldBlock 次数 */
];

pub(crate) fn trace(msg: &str) {
    if !crate::media::platform::log::enabled() {
        return;
    }
    let n = TRACE_LINES.fetch_add(1, Ordering::Relaxed);
    if n < 400 {
        crate::media::platform::log::append(&format!("netdbg#{n} {msg}"));
    }
}

fn bump(which: usize) {
    COUNTERS[which].fetch_add(1, Ordering::Relaxed);
}

/// 收尾时把计数汇总一行，方便一眼看出"到底谁在反复跑"。
pub(crate) fn trace_summary() {
    if !crate::media::platform::log::enabled() {
        return;
    }
    crate::media::platform::log::append(&format!(
        "netdbg 统计: read={} seek={} fetch={} discard={} wait_timeout={} wouldblock={}（轨迹 {} 行）",
        COUNTERS[0].load(Ordering::Relaxed),
        COUNTERS[1].load(Ordering::Relaxed),
        COUNTERS[2].load(Ordering::Relaxed),
        COUNTERS[3].load(Ordering::Relaxed),
        COUNTERS[4].load(Ordering::Relaxed),
        COUNTERS[5].load(Ordering::Relaxed),
        TRACE_LINES.load(Ordering::Relaxed),
    ));
}

/// 一次 Range 抓多少（任务书 §13 的 refill 量级；也是"每窗口一次请求"的粒度）。
pub const WINDOW_BYTES: usize = 256 * 1024;
/// 消费端等数据的上限：等不到就返回"稍后再来"，由上层 Gate 决定是否静音。
pub const READ_WAIT_MS: u64 = 4000;

/*
 * 一次 read() 内部最多等几轮（每轮 READ_WAIT_MS）。
 *
 * 为什么不能等一轮就放弃：解码器**读文件头**时不能被"暂时没数据"打断 —— 它会把
 * 负的读返回值当成硬错误，然后连"回到开头重读"都不做了，整首歌就废了（真机上
 * "在线歌打不开"的最后一段就是这么来的）。所以打开阶段要足够耐心，把网络首包
 * 的几秒等完。真正等不到的时候仍然返回 WouldBlock，由 Gate 输出静音。
 */
pub const READ_WAIT_ROUNDS: usize = 3;

/// "从哪儿按偏移取字节"的最小接口。
pub trait ByteTransport: Send {
    /// 从绝对偏移 `off` 读最多 `dst.len()` 字节；`Ok(0)` = 真正结束。
    fn read_at(&mut self, off: u64, dst: &mut [u8]) -> Result<usize, SourceError>;
    /// 总长度（未知则 None）。
    fn size(&self) -> Option<u64>;
}

/*
 * 取数线程与消费端共享的窗口。
 *
 * 一条重要规矩：**窗口里的数据不会因为"有人问了别的位置"而失效**。
 *
 * 以前每次登记新请求 / 取数线程开工，都会先把 `buf` 清空。于是：
 *   解码器问一句"文件多长"（跳到末尾）→ 窗口被清掉 → 它再回尾部读
 *   → 又得重新发一次 HTTP，把同一段 128 字节下载回来。
 * 真机上就是靠这个把同一段尾部反复下载了十几次，一直拖到解码器放弃。
 * 现在只有**真的抓到新数据**才会整体替换窗口。
 */
struct Window {
    buf: Vec<u8>,
    start: u64,
    /*
     * 「流到哪儿就没有数据了」——这是一个**位置**，不是一个布尔。
     *
     * 以前这里是个 bool `eof`，语义是"我们见过一次末尾"。问题是解码器探测完
     * 文件尾巴一定会 seek 回前面，那时这个陈旧的 true 会立刻告诉它"没数据"，
     * 解码器拿到 -1 之后连"回到开头重读"都不做了 —— 真机上"在线歌打不开、
     * 卡在文件尾部刷屏"就是这么来的。
     *   Some(off) = 从 off 开始就已经没有数据了；
     *   None      = 目前不知道（刚换了位置 / 这次抓到了数据）。
     */
    end_at: Option<u64>,
    err: Option<SourceError>,
    /// 消费端希望窗口从哪开始（seek 会改它）。
    want_from: u64,
    /// 请求代数：seek 后 +1，取数线程据此丢弃过期结果。
    generation: u64,
    /// 消费端已登记一个取数需求，等取数线程去做。
    pending: bool,
    /*
     * 取数线程**正在**抓这一段。
     *
     * 为什么必须有它：以前"有没有人在抓"只能靠 pending 猜，而取数线程一开工就会把
     * pending 置回 false。于是消费端在等待里被唤醒时，看到 pending=false 就以为
     * "没人管这事"，于是**又登记一次**（代数 +1）—— 正在飞的那次抓取立刻被判过期，
     * 抓到的字节被丢掉、窗口被清空，然后重来。真机日志里每一次"取数作废"后面
     * 紧跟两次"登记取数"就是这么来的，最后拖到解码器放弃（详见 docs §8）。
     */
    inflight: bool,
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
                end_at: None,
                err: None,
                want_from: 0,
                generation: 0,
                pending: true, /* 打开就先抓第一窗 */
                inflight: false,
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
                            /*
                             * 只登记"这一抓由我负责"，**不碰已缓存的数据**：
                             * 抓不到（越界/失败）时旧窗口依然是对的，解码器
                             * 还能照常命中缓存，不必为一个长度探测重下 128 KB。
                             */
                            w.pending = false;
                            w.inflight = true; /* 告诉消费端"有人正在抓，别再重复登记" */
                        }
                        bump(2);
                        let got = st.read_at(from, &mut tmp);
                        {
                            let (lock, cv) = &*shared;
                            let Ok(mut w) = lock.lock() else { break };
                            if w.cancelled {
                                break;
                            }
                            w.inflight = false;
                            if w.generation != gen {
                                /*
                                 * 期间发生了真正的换位置（seek）：这一抓作废。
                                 *
                                 * 注意**不能**把 pending 清掉：能改代数的只有"登记"，
                                 * 而登记一定会把 pending 置 true。这里清掉等于把
                                 * 别人刚登记的那个请求吃掉，消费端会一直等到超时。
                                 */
                                bump(3);
                                trace(&format!(
                                    "取数作废 from={from} got={got:?}（抓取期间登记了新请求）"
                                ));
                                cv.notify_all();
                                continue;
                            }
                            trace(&format!("取数 from={from} got={got:?} 已发布"));
                            match got {
                                Ok(0) => w.end_at = Some(from),
                                Ok(n) => {
                                    /* 抓到数据：整体替换窗口（只有这里会动 buf）。 */
                                    w.buf.clear();
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
            /* 「到底」是一个位置：只有你要的位置已经过了那个点，才算是结束。 */
            let ended = w.end_at.map_or(false, |end| pos >= end);
            if covered || ended || w.err.is_some() || w.cancelled {
                trace(&format!(
                    "等 pos={pos} -> covered={covered} ended={ended} err={:?} 窗口=[{},{})",
                    w.err,
                    w.start,
                    w.start + w.buf.len() as u64
                ));
                return covered;
            }
            /* 窗口没盖住要的位置：登记需求叫醒取数线程（读尽当前窗口时走这里）。 */
            /*
             * 登记的三个条件，缺一不可：
             *   !pending  —— 还没有人为这个位置登记过；
             *   !inflight —— 取数线程**不在**抓东西（这是关键：它一开工就会把
             *                pending 置 false，只看 pending 会误判成"没人管"，
             *                于是重复登记、把正在飞的抓取作废掉）；
             *   还没超时   —— 超时之后再登记，等于留下一个没人等的请求，还会让
             *                下一轮重复登记。所以超时判断必须放在登记**之前**。
             */
            let now = std::time::Instant::now();
            if now >= deadline {
                bump(4);
                trace(&format!(
                    "等 pos={pos} 超时（窗口=[{},{}) pending={} inflight={}）",
                    w.start,
                    w.start + w.buf.len() as u64,
                    w.pending,
                    w.inflight
                ));
                return false;
            }
            if !w.pending && !w.inflight {
                w.want_from = pos;
                w.generation = w.generation.wrapping_add(1);
                w.pending = true;
                trace(&format!("登记取数 pos={pos} generation={}", w.generation));
                cv.notify_all();
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
        bump(0);
        trace(&format!("read 入口 pos={} want={}", self.pos, want));
        /* 尽量填满：解码器（尤其是 m4a 的精确读）不接受"短读"，所以这里
         * 循环取数据，只有真正到流末尾才返回短读。 */
        while done < want {
            if let Some(e) = self.error() {
                trace(&format!("read 出错返回 {:?}", e));
                return Err(e);
            }
            if self.is_eof() {
                trace(&format!("read 到末尾：pos={} -> 返回 {} 字节", self.pos, done));
                break; /* 真正结束 */
            }
            let mut ready = false;
            for _ in 0..READ_WAIT_ROUNDS {
                if self.wait_for(self.pos, READ_WAIT_MS) {
                    ready = true;
                    break;
                }
                if self.error().is_some() || self.is_eof() {
                    break;
                }
            }
            if !ready {
                if let Some(e) = self.error() {
                    return Err(e);
                }
                if self.is_eof() {
                    break;
                }
                if done > 0 {
                    break; /* 等超时：把已有的先给出去，上层 Gate 会决定是否静音 */
                }
                bump(5);
                trace("read 等不到数据 -> WouldBlock");
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
        trace(&format!("read 出口 pos={} 返回 {} 字节", self.pos, done));
        Ok(done)
    }

    fn seek(&mut self, pos: u64) -> Result<(), SourceError> {
        self.pos = pos;
        bump(1);
        /* 窗口里已经有目标位置就白拿；否则让取数线程挪过去。 */
        let covered = {
            let (lock, _cv) = &*self.shared;
            let Ok(w) = lock.lock() else {
                return Err(SourceError::Io(String::from("window lock")));
            };
            let c = pos >= w.start && pos < w.start + w.buf.len() as u64;
            trace(&format!(
                "seek pos={pos} 命中窗口={c} 窗口=[{},{})",
                w.start,
                w.start + w.buf.len() as u64
            ));
            c
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
         * 长度已知时，"结束"只由**当前位置**决定；长度未知时才看"取数线程
         * 在哪儿发现的末尾"（这也是一个位置，不是布尔）。
         */
        if let Some(sz) = self.size {
            return self.pos >= sz;
        }
        let (lock, _cv) = &*self.shared;
        let Ok(w) = lock.lock() else { return false };
        w.end_at.map_or(false, |end| self.pos >= end)
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
