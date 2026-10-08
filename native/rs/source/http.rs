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

use super::{AudioSource, SourceError, SourceKind};
use alloc::collections::VecDeque;
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
/*
 * 逐条轨迹的开关。
 *
 * **解码器一旦接上就必须关掉**：轨迹要给每条读/等/取数拼一行字符串，而播放阶段
 * 这些读发生在音频线程的 mpg123 回调里 —— 真机上就是这么崩的（dump 的栈顶是
 * `<core::fmt::Formatter>::pad`，紧跟着 `mp3_io_read`）。
 * 排障只需要**打开阶段**的轨迹，之后留计数就够了：计数只是原子加，不拼字符串。
 */
static TRACE_ON: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// 逐条轨迹的开关文件：卡里放 `ux0:/data/yunyin/netdbg` 才开。
///
/// 默认关：在线播放阶段任何一处字符串格式化都可能踩到真机上那个坑
/// （见 docs/排障.md），排障要用时再显式打开。
const TRACE_FLAG: &str = "ux0:data/yunyin/netdbg";

pub(crate) fn trace_enable_if_requested() {
    if std::fs::metadata(TRACE_FLAG).is_ok() {
        TRACE_ON.store(true, Ordering::Relaxed);
    }
}
static COUNTERS: [AtomicU32; 6] = [
    AtomicU32::new(0), /* 0 read 调用 */
    AtomicU32::new(0), /* 1 seek 调用 */
    AtomicU32::new(0), /* 2 取数次数 */
    AtomicU32::new(0), /* 3 取数结果被作废次数 */
    AtomicU32::new(0), /* 4 等超时次数 */
    AtomicU32::new(0), /* 5 WouldBlock 次数 */
];

pub(crate) fn trace(msg: &str) {
    if !TRACE_ON.load(Ordering::Relaxed) || !crate::media::platform::log::enabled() {
        return;
    }
    let n = TRACE_LINES.fetch_add(1, Ordering::Relaxed);
    if n < 400 {
        crate::media::platform::log::append(&format!("netdbg#{n} {msg}"));
    }
}

/*
 * 带闭包的轨迹：**只有真的要写日志时才拼字符串**。
 * 直接写 `trace(&format!(...))` 会在每次都拼一遍（哪怕日志关着），
 * 这在解码回调里既费时又危险（真机崩过一次，见上面的说明）。
 */
pub(crate) fn trace_f<F: FnOnce() -> alloc::string::String>(f: F) {
    if !TRACE_ON.load(Ordering::Relaxed) || !crate::media::platform::log::enabled() {
        return;
    }
    trace(&f());
}

/// 关掉逐条轨迹（解码器接上以后调用）。计数仍然保留。
pub(crate) fn trace_off() {
    TRACE_ON.store(false, Ordering::Relaxed);
}

fn bump(which: usize) {
    COUNTERS[which].fetch_add(1, Ordering::Relaxed);
}

/* ---------------------------------------------------- 缓冲进度（界面用） -- */
/*
 * "这首在线歌已经缓冲到哪儿了"：界面拿它画进度条里那根浅色的缓存条。
 *
 * 为什么放在模块级静态：同一时刻只有一路在线源（REMOTE 槽的规矩），
 * 界面只关心"当前正在播的这条流"。窗口一抓到手就更新，读只需两次原子读，
 * 音频线程完全不受影响。
 */
static BUF_DONE: AtomicU64 = AtomicU64::new(0);
static BUF_TOTAL: AtomicU64 = AtomicU64::new(0);

fn buf_reset(total: Option<u64>) {
    BUF_DONE.store(0, Ordering::Release);
    BUF_TOTAL.store(total.unwrap_or(0), Ordering::Release);
}

fn buf_mark(done: u64, total: Option<u64>) {
    BUF_DONE.store(done, Ordering::Release);
    if let Some(t) = total {
        BUF_TOTAL.store(t, Ordering::Release);
    }
}

/// `(已缓冲字节, 总字节)`；总长未知时第二项是 0。
pub fn buffer_snapshot() -> (u64, u64) {
    (
        BUF_DONE.load(Ordering::Acquire),
        BUF_TOTAL.load(Ordering::Acquire),
    )
}

/// 磁盘缓存命中：整首已经"缓冲"完（界面那根浅色条直接拉满）。
pub fn buf_mark_all(total: u64) {
    buf_mark(total, Some(total));
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
///
/// **1 MiB**，和 C 侧 `YHTTP_WINDOW`（native/net/yhttp.h）严格同尺寸 —— 那里写死
/// 的规矩：不一致时一个 Rust 窗口会被拆成两次 Range 请求，慢网下白白多等一次首包。
///
/// 慢网下的提前量**不再靠加粗单个窗口**，而是靠后面的**滚动预读队列**
/// （`Window::budget`，按整首的 1/3 给、夹在 2–6 MiB，见 policy::readahead_budget）：
/// 取数线程抓满一块就立刻接着抓下一块，不再像以前那样"当前窗 + 一窗预取"抓完就闲着。
/// 真机 2026-10-06 的日志里"播几秒又要缓冲"（恢复 10 s → 剩 4.9 s → 又静音）就是
/// 那个空档造成的：解码器把当前窗读掉一半，取数线程还在睡。
///
/// RAM 代价：队头 1 MiB + 队列 ≤ budget + 取数线程中转 1 MiB（C 侧再一块同尺寸）。
pub const WINDOW_BYTES: usize = 1024 * 1024;
/// 消费端等数据的上限：等不到就返回"稍后再来"，由上层 Gate 决定是否静音。
pub const READ_WAIT_MS: u64 = 4000;

/*
 * 连续失败多少次才判定"这条流坏了"。
 *
 * 以前是**失败一次就永久记住**（err 字段粘性），于是网络抖一下之后：窗口永远补不上、
 * Gate 永远静音、取数线程也再不去试（它看到已有错误就直接返回）—— 真机上表现就是
 * "播着播着卡死、再也不动"。对网络流来说正确的做法是**自动重试**，只有连续多次
 * 都失败才当成真坏了。
 */
pub const MAX_CONSECUTIVE_FAILS: u32 = 4;

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
     * 累计"从网络收到多少字节"。
     *
     * 为什么单独记：界面/策略需要的是**下载速度**，而不是"窗口里剩多少"。
     * 以前用"剩余字节"的增量去估速，而解码器同时在消费它 —— 稳态下增量≈0，
     * 于是永远被判成"网络 Starving"（真机日志里就一直是这个假状态）。
     */
    fetched: u64,
    /*
     * **滚动预读队列**（读得比解码快很多窗）。
     *
     * 以前这里只有一个 `next` 槽：抓满当前窗 + 预取窗之后取数线程就**没事干了**，
     * 一直睡到解码器把当前窗读光、`next` 升格才去抓再下一窗。真机上这正好是
     * "播几秒又要缓冲"：提前量在解码器跨窗那一下掉到 0，然后干等一次 HTTP。
     *
     * 现在改成队列 + 预算（`budget`，见 policy::readahead_budget）：只要队列总字节
     * 没到预算就继续往后抓，取数线程在整个播放期间是满的，提前量不会断链。
     * 队头（`buf`）是解码器正在读的那一块，队列里每一块都和前一块首尾相接。
     */
    queue: VecDeque<Chunk>,
    /// `queue` 里的总字节数（算预算用，不用每次遍历）。
    queued: usize,
    /// 预读预算：队列里最多留多少字节（队头那块不算）。
    budget: usize,
    /*
     * 升格时换下来的空块（最多留两块）。
     *
     * SD2Vita 上最不想看到的就是"播着播着一直在 malloc/free 1 MiB"：老设计只有
     * 当前窗 + 预取窗两块，容量是复用的。队列化之后每块都会换新，所以这里把换下来
     * 的那块留住，下一块抓回来时直接复用同一片内存（稳态下一个字节都不再新分配）。
     */
    spare: Vec<Vec<u8>>,
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
    /// 连续失败次数：到 MAX_CONSECUTIVE_FAILS 才把 err 置上（见常量说明）。
    fail_count: u32,
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

/// 预读队列里的一整块（永远和前一块首尾相接）。
struct Chunk {
    start: u64,
    data: Vec<u8>,
}

impl Window {
    /// 当前位置被队头（解码器正在读的那块）盖住了吗。
    fn covers(&self, pos: u64) -> bool {
        pos >= self.start && pos < self.start + self.buf.len() as u64
    }

    /// 已经抓到的最后一字节的偏移（= 下一个该抓的位置）。
    fn fetch_cursor(&self) -> u64 {
        match self.queue.back() {
            Some(c) => c.start + c.data.len() as u64,
            None => self.start + self.buf.len() as u64,
        }
    }

    /// 队头没盖住 `pos` 时，把队列里接得上的块依次升格成队头。
    /// 返回"升格之后盖住了吗"。升格之后**顺手把预读补回预算**，别让提前量断链。
    ///
    /// 顺带一个好处：往前 seek 到"已经预读到的范围"里，现在是零网络开销
    /// （以前只认一窗，seek 到队列里的地方照样要重开一次 Range）。
    fn promote_if_needed(&mut self, pos: u64, cv: &Condvar) -> bool {
        let mut promoted = false;
        while !self.covers(pos) {
            let head_end = self.start + self.buf.len() as u64;
            let Some(front) = self.queue.front() else { break };
            /* 队列必须首尾相接；接不上（位置回跳 / 中间有洞）就别瞎升格。 */
            if front.start != head_end || pos < front.start {
                break;
            }
            let chunk = match self.queue.pop_front() {
                Some(c) => c,
                None => break,
            };
            self.queued = self.queued.saturating_sub(chunk.data.len());
            let old = core::mem::replace(&mut self.buf, chunk.data);
            if old.capacity() > 0 && self.spare.len() < 2 {
                self.spare.push(old); /* 留着重用，别在 SD2Vita 上反复 malloc */
            }
            self.start = chunk.start;
            promoted = true;
        }
        if promoted {
            /* 升格用掉了一块 —— 立刻把预读补回预算，否则下一次跨窗又要等一次 HTTP。 */
            Self::maybe_prefetch(self, cv);
        }
        self.covers(pos)
    }

    /// 把预读队列补到预算（只在"没人干活、还没到底"时登记）。
    fn maybe_prefetch(w: &mut Self, cv: &Condvar) {
        if w.cancelled || w.pending || w.inflight || w.err.is_some() {
            return;
        }
        if w.buf.is_empty() || w.queued >= w.budget {
            return;
        }
        let at = w.fetch_cursor();
        if let Some(end) = w.end_at {
            if at >= end {
                return; /* 已知到底 */
            }
        }
        w.want_from = at;
        w.generation = w.generation.wrapping_add(1);
        w.pending = true;
        cv.notify_all();
    }
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
    ///
    /// `budget` = 滚动预读的总预算（字节），由调用方按 policy::readahead_budget
    /// 算好传进来（整首的 1/3，夹在 2–6 MiB）。这里再兜一道底：至少一块。
    pub fn new(url: &str, transport: T, budget: u64) -> Self {
        trace_enable_if_requested(); /* 卡里放了 netdbg 文件才开逐条轨迹 */
        let size = transport.size();
        buf_reset(size); /* 新的一路流：缓存条从 0 开始 */
        let shared = Arc::new((
            Mutex::new(Window {
                buf: Vec::new(),
                start: 0,
                fetched: 0,
                queue: VecDeque::new(),
                queued: 0,
                budget: (budget as usize).max(WINDOW_BYTES),
                spare: Vec::new(),
                end_at: None,
                err: None,
                fail_count: 0,
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
            let total_hint = size;
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
                        let failed = got.is_err(); /* match 会把 got 里的错误值移出去，先记下来 */
                        let mut backoff_ms = 0u64; /* 这一轮失败该退避多久（由失败次数决定） */
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
                                trace_f(|| format!(
                                    "取数作废 from={from} got={got:?}（抓取期间登记了新请求）"
                                ));
                                cv.notify_all();
                                continue;
                            }
                            trace_f(|| format!("取数 from={from} got={got:?} 已发布"));
                            match got {
                                Ok(0) => {
                                    w.end_at = Some(from);
                                    w.fail_count = 0;
                                }
                                Ok(n) => {
                                    w.fail_count = 0; /* 抓到了：之前的失败不算数 */
                                    /* 累计下载量（含预读队列）：测速用真值，不再看"剩余字节"。 */
                                    w.fetched = w.fetched.saturating_add(n as u64);
                                    /* 界面那根浅色"缓存条"看的就是这个：抓到手多少字节。 */
                                    buf_mark(from + n as u64, total_hint);
                                    let contiguous = !w.buf.is_empty() && from == w.fetch_cursor();
                                    if contiguous {
                                        /* 接在队尾后面：进预读队列，等解码器读到那儿。 */
                                        let data = match w.spare.pop() {
                                            Some(mut v) => {
                                                v.clear();
                                                v.extend_from_slice(&tmp[..n]);
                                                v
                                            }
                                            None => tmp[..n].to_vec(),
                                        };
                                        w.queue.push_back(Chunk { start: from, data });
                                        w.queued += n;
                                    } else {
                                        /*
                                         * 第一块 / 消费者跳到了别处（seek）：这次结果就是新的
                                         * 队头，后面整条队列作废（位置已经对不上了）。
                                         */
                                        w.buf.clear();
                                        w.buf.extend_from_slice(&tmp[..n]);
                                        w.start = from;
                                        w.queue.clear();
                                        w.queued = 0;
                                    }
                                }
                                Err(e) => {
                                    /*
                                     * 网络抖一下不该把整首歌判死：连续失败够多次才算真坏。
                                     * 中间的失败只记数 + 退避重试，Gate 那边会一直是静音，
                                     * 一旦抓回来就自动继续放（真机上的"卡死再也不动"就是这么来的）。
                                     */
                                    w.fail_count = w.fail_count.saturating_add(1);
                                    /* 指数退避（policy 里有测试）：抖得越久，等得越久，封顶 2 秒。 */
                                    backoff_ms = super::policy::backoff_ms(w.fail_count);
                                    if w.fail_count >= MAX_CONSECUTIVE_FAILS {
                                        trace_f(|| {
                                            format!(
                                                "取数连续失败 {} 次，判定这条流坏了：{:?}",
                                                w.fail_count, e
                                            )
                                        });
                                        w.err = Some(e);
                                    } else {
                                        trace_f(|| {
                                            format!(
                                                "取数失败（第 {} 次），退避后重试：{:?}",
                                                w.fail_count, e
                                            )
                                        });
                                    }
                                }
                            }
                            w.pending = false;
                            Window::maybe_prefetch(&mut w, cv);
                            gen_hint.store(w.generation, Ordering::Release);
                            cv.notify_all();
                        }
                        if failed {
                            /* 失败后退避一下再去试，别把网络打爆（阶梯见 policy::backoff_ms）。 */
                            std::thread::sleep(Duration::from_millis(backoff_ms.max(200)));
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
            /* 先用预取槽兜一下：上一窗读尽时，下一窗往往已经在内存里了。 */
            let covered = w.promote_if_needed(pos, cv);
            /* 「到底」是一个位置：只有你要的位置已经过了那个点，才算是结束。 */
            let ended = w.end_at.map_or(false, |end| pos >= end);
            if covered || ended || w.err.is_some() || w.cancelled {
                trace_f(|| format!(
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
                trace_f(|| format!(
                    "等 pos={pos} 超时（窗口=[{},{}) pending={} inflight={}）",
                    w.start,
                    w.start + w.buf.len() as u64,
                    w.pending,
                    w.inflight
                ));
                return false;
            }
            Self::register_locked(&mut w, cv, pos);
            match cv.wait_timeout(w, deadline - now) {
                Ok((g, _)) => w = g,
                Err(e) => w = e.into_inner().0,
            }
        }
    }

    /// 登记一次取数需求（拿锁后再调）。
    fn register_locked(w: &mut Window, cv: &Condvar, pos: u64) {
        if w.pending || w.inflight || w.cancelled {
            return;
        }
        w.want_from = pos;
        w.generation = w.generation.wrapping_add(1);
        w.pending = true;
        trace_f(|| format!("登记取数 pos={pos} generation={}", w.generation));
        cv.notify_all();
    }

    /// 累计从网络收到的字节数（测速用；测速不能再拿"窗口剩余"去估）。
    pub fn fetched_total(&self) -> u64 {
        let (lock, _) = &*self.shared;
        match lock.lock() {
            Ok(w) => w.fetched,
            Err(_) => 0,
        }
    }

    /*
     * **主动补数据**：Gate 决定"先静音"时也要叫一次。
     *
     * 为什么必须这样（真机现象：在线歌播到第 12~16 秒卡死）：
     *   第一个窗口 256 KiB 播完后，缓存剩余低于 Gate 的阈值 → Gate 只输出静音、
     *   **不调解码器** → 解码器没机会要下一段 → 取数线程收不到任何登记 → 缓存
     *   永远补不上 → 一直静音。也就是"没人拉数据，就永远没数据"的死锁。
     *   所以静音这条路必须顺手把取数线程叫醒，让它去把窗口挪到当前位置。
     */
    pub fn prime(&self) {
        let (lock, cv) = &*self.shared;
        if let Ok(mut w) = lock.lock() {
            let pos = self.pos;
            /*
             * 注意这里**不能**写成"当前位置没被盖住才去抓"。
             *
             * Gate 是在"剩余不足阈值"时静音的（真机日志里是剩 63 KB），那时位置
             * 往往**还在窗口里** —— 按旧写法这里什么都不做，于是没人取数、Gate 永远
             * 不开，一首歌播到 28 秒就永久静音。
             *
             * 正确的事是**保证提前量**：位置上没盖住就按位置抓；盖住了就把预读队列
             * 补满（maybe_prefetch 会从队尾继续往后抓，直到预算用完）。
             */
            if w.covers(pos) {
                Window::maybe_prefetch(&mut w, cv);
            } else {
                Self::register_locked(&mut w, cv, pos);
            }
        }
    }

    /// 缓存是不是已经接到了"已知的流末尾"。
    ///
    /// Gate 需要它：文件最后一段（比如只剩 100 KB）本身就不足阈值，如果还按
    /// "缓存够不够"来判，解码器永远拿不到收尾的那几帧 —— 歌就卡在结尾了。
    pub fn at_cached_end(&self) -> bool {
        let (lock, _cv) = &*self.shared;
        let Ok(w) = lock.lock() else { return false };
        let Some(end) = w.end_at else { return false };
        w.fetch_cursor() >= end
    }

    /// 预读队列已经填到预算了吗？
    ///
    /// Gate 用它区分"取数线程没活干"和"网速慢"：队列抓满时下载速度采样会掉到 0，
    /// 那是**假** Starving，不能因此把启动无限期推迟。
    pub fn prefetch_full(&self) -> bool {
        let (lock, _cv) = &*self.shared;
        match lock.lock() {
            Ok(w) => w.budget > 0 && w.queued >= w.budget,
            Err(_) => true,
        }
    }

    /// 让正在进行的 `read()` 立刻返回（取消等待），但不动窗口里的数据。
    ///
    /// 切歌 / 停止时必须在 join 音频线程**之前**叫这一下：否则音频线程可能正卡在
    /// `wait_for()` 里等一个坏掉的网络窗口（每轮最多 READ_WAIT_MS，三轮），
    /// `session_end()` 的 join 就会把界面线程一起拖住 —— 真机 0.13 上表现为
    /// "点歌后画面定格、歌曲还在唱"（看门狗 2s 一到直接把 guest 打断）。
    pub fn cancel(&self) {
        let (lock, cv) = &*self.shared;
        if let Ok(mut w) = lock.lock() {
            w.cancelled = true;
            cv.notify_all();
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
        trace_f(|| format!("read 入口 pos={} want={}", self.pos, want));
        /* 尽量填满：解码器（尤其是 m4a 的精确读）不接受"短读"，所以这里
         * 循环取数据，只有真正到流末尾才返回短读。 */
        while done < want {
            if let Some(e) = self.error() {
                trace_f(|| format!("read 出错返回 {:?}", e));
                return Err(e);
            }
            if self.is_eof() {
                trace_f(|| format!("read 到末尾：pos={} -> 返回 {} 字节", self.pos, done));
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
        trace_f(|| format!("read 出口 pos={} 返回 {} 字节", self.pos, done));
        Ok(done)
    }

    fn seek(&mut self, pos: u64) -> Result<(), SourceError> {
        self.pos = pos;
        bump(1);
        /* 窗口里已经有目标位置就白拿；否则让取数线程挪过去。 */
        let covered = {
            let (lock, cv) = &*self.shared;
            let Ok(w) = lock.lock() else {
                return Err(SourceError::Io(String::from("window lock")));
            };
            let mut w = w;
            let c = w.promote_if_needed(pos, cv);
            trace_f(|| format!(
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
        let (lock, cv) = &*self.shared;
        let Ok(mut w) = lock.lock() else { return 0 };
        /* 顺手升格队列：Gate 看的就是这个数，不能因为"还没升格"而误判缓存不够。 */
        if !w.promote_if_needed(self.pos, cv) {
            return 0;
        }
        let off = (self.pos - w.start) as usize;
        let in_window = w.buf.len().saturating_sub(off);
        /* 预读队列里的字节也算"已经拿到的数据"：块块相接，解码器马上就能用。 */
        in_window + w.queued
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
