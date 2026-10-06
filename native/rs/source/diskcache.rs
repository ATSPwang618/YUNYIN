//! 固定大小的磁盘缓存（在线播放的"第二层"）：一个 `cache.dat` + 一个索引文件。
//!
//! ## 为什么这么设计（PS Vita / SD2Vita 的存储特性）
//!
//! 真机上最贵的存储操作是 `create / write / close / remove`，不是"多占几 MB"。
//! 所以这里**不按"播过几首"去删缓存文件**，而是：
//!
//! ```text
//! 网络 ──→ RAM 窗口（source/http.rs 的双窗口预取） ──→ 解码器 ──→ 音频
//!   └────→ cache.dat（固定 32 MB，环形覆盖；正常播放期间一次 remove 都没有）
//! ```
//!
//! * **按容量算，不按首数算**：一首 3 MB 和一首 80 MB 不能都算"1 首"。
//! * **环形覆盖**：写到末尾回到开头接着写，被盖住的旧条目顺手从索引里划掉
//!   （延迟回收 —— 清理就是"没在索引里"，不需要删文件）。
//! * **索引单独一个小文件**：一首歌下完 / 最多每 10 秒落一次盘，别让播放路径
//!   去写文件。
//! * **写缓存在取数线程里做**：音频线程永远只读 RAM 窗口，存储再慢也卡不到解码。
//!
//! ## 什么时候命中
//!
//! 只有**完整下完**（写进去的字节 = 这首歌的总长度）的条目才算命中，
// 半截的条目留在索引外 —— 免得拿一段缺头少尾的数据去喂解码器。
//! 点开同一首歌第二次就是纯磁盘读，零网络请求。

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// 缓存文件上限。够放十来首 320 kbps 的歌，又不会把卡塞满。
pub const CACHE_CAP_DEFAULT: u64 = 32 * 1024 * 1024;
/// 索引落盘的最小间隔（毫秒）。
const INDEX_MIN_GAP_MS: u64 = 10_000;
/// 磁盘缓存的固定路径（真机）。宿主机测试用 `DiskCache::new()` 指别处。
pub const DATA_PATH: &str = "ux0:/data/yunyin/cache.dat";
pub const INDEX_PATH: &str = "ux0:/data/yunyin/cache.idx";
/// 单首歌超过这个大小就不进缓存（一次写入撑满整个环，收益低还把别人挤光）。
const SINGLE_MAX: u64 = 16 * 1024 * 1024;

fn now_ms() -> u64 {
    crate::media::platform::time::now_ms()
}

/// 索引里的一条：某个 key 的数据住在 cache.dat 的 `[off, off+len)`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub off: u64,
    pub len: u64,
    pub total: u64,
    pub used_ms: u64,
    pub complete: bool,
}

struct Inner {
    entries: Vec<Entry>,
    write_pos: u64,
    loaded: bool,
    last_index_ms: u64,
    hits: u64,
    misses: u64,
}

/// 一个 cache.dat + 一个索引。进程里只应该有一个（`global()`）。
pub struct DiskCache {
    data_path: String,
    index_path: String,
    cap: u64,
    inner: Mutex<Inner>,
}

impl DiskCache {
    pub fn new(data_path: &str, index_path: &str, cap: u64) -> Self {
        Self {
            data_path: String::from(data_path),
            index_path: String::from(index_path),
            cap: if cap == 0 { CACHE_CAP_DEFAULT } else { cap },
            inner: Mutex::new(Inner {
                entries: Vec::new(),
                write_pos: 0,
                loaded: false,
                last_index_ms: 0,
                hits: 0,
                misses: 0,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn cap(&self) -> u64 {
        self.cap
    }

    /// 命中/未命中计数（日志和界面用）。
    pub fn stats(&self) -> (u64, u64) {
        let g = self.lock();
        (g.hits, g.misses)
    }

    /// 索引只读一次；顺带把"上次写到哪儿了"接着往下走（跨启动不重头写）。
    fn ensure_loaded(&self, g: &mut Inner) {
        if g.loaded {
            return;
        }
        g.loaded = true;
        g.entries = self.read_index();
        /* write_pos 优先用索引头里存的那份（真值）；老索引没有头就按
         * "最后一个条目的末尾"估一个（只是兜底，不会再拿它当唯一依据）。 */
        let from_header = self.read_write_pos();
        g.write_pos = if from_header.is_some() {
            from_header.unwrap_or(0) % self.cap
        } else {
            g
            .entries
            .iter()
            .map(|e| e.off + e.len)
            .max()
            .unwrap_or(0)
            % self.cap
        };
    }

    fn read_index(&self) -> Vec<Entry> {
        let Ok(text) = std::fs::read_to_string(&self.index_path) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for line in text.lines() {
            if line.starts_with("#w ") {
                continue; /* 头行（write_pos）由 read_write_pos 单独读 */
            }
            let mut f = line.split('\t');
            let (Some(key), Some(off), Some(len), Some(total), Some(used), Some(flag)) = (
                f.next(),
                f.next(),
                f.next(),
                f.next(),
                f.next(),
                f.next(),
            ) else {
                continue;
            };
            let (Some(off), Some(len), Some(total), Some(used)) = (
                off.parse::<u64>().ok(),
                len.parse::<u64>().ok(),
                total.parse::<u64>().ok(),
                used.parse::<u64>().ok(),
            ) else {
                continue;
            };
            if key.is_empty() || len == 0 {
                continue;
            }
            out.push(Entry {
                key: String::from(key),
                off,
                len,
                total,
                used_ms: used,
                complete: flag == "1",
            });
        }
        out
    }

    /// 读索引头里的 `write_pos`（环形缓存的真实写指针）。
    fn read_write_pos(&self) -> Option<u64> {
        let text = std::fs::read_to_string(&self.index_path).ok()?;
        let first = text.lines().next()?;
        first
            .strip_prefix("#w ")
            .and_then(|v| v.trim().parse::<u64>().ok())
    }

    /// 索引落盘（同目录下就地覆盖，不做 rename —— 少一次文件操作）。
    fn save_index(&self, g: &mut Inner, force: bool) {
        let now = now_ms();
        if !force && now.saturating_sub(g.last_index_ms) < INDEX_MIN_GAP_MS {
            return;
        }
        g.last_index_ms = now;
        /* 头一行存真实写指针：索引里没有天然的时间顺序，
         * 靠 max(off+len) 推算在跨环条目下不可靠。 */
        let mut text = format!("#w {}\n", g.write_pos);
        for e in &g.entries {
            text.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\n",
                e.key,
                e.off,
                e.len,
                e.total,
                e.used_ms,
                if e.complete { 1 } else { 0 }
            ));
        }
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.index_path)
        {
            let _ = f.write_all(text.as_bytes());
        }
    }

    /// 新写进去的 `[off, off+len)` 盖住了谁，就把谁从索引里划掉（延迟回收）。
    ///
    /// **必须按"环形区间"判断**：条目自己可能跨环（`off + len > cap` 时它实际占
    /// `[off, cap)` + `[0, off+len-cap)` 两段）。拿 `[off, off+len)` 直接做普通
    /// 区间比较会漏判 —— 比如旧条目在 28MB..32MB + 0..4MB、新写在 1..3MB 时，
    /// `28 >= 3` 成立，旧条目会被误判"没被覆盖"留下来，命中后读到被覆盖的字节。
    fn evict_overlap(g: &mut Inner, off: u64, len: u64, cap: u64) {
        if len == 0 || cap == 0 {
            return;
        }
        let end = off + len;
        g.entries.retain(|e| {
            let e_end = e.off + e.len;
            /* 第一段 [e.off, min(e_end, cap)) 和新写的 [off, end) 相交？ */
            let a_end = if e_end < cap { e_end } else { cap };
            let a_hit = e.off < end && a_end > off;
            /* 跨环时还有第二段 [0, e_end-cap) */
            let b_hit = e_end > cap && (e_end - cap) > off;
            !(a_hit || b_hit)
        });
    }

    /// 完整命中：返回 `(在 cache.dat 里的偏移, data.rs 长度, 总长度)`。
    pub fn lookup(&self, key: &str) -> Option<(u64, u64, u64)> {
        let mut g = self.lock();
        self.ensure_loaded(&mut g);
        let now = now_ms();
        let mut found: Option<(u64, u64, u64)> = None;
        let mut hit_index: Option<usize> = None;
        for (i, e) in g.entries.iter().enumerate() {
            if e.key == key && e.complete && e.total == e.len && e.len > 0 {
                found = Some((e.off, e.len, e.total));
                hit_index = Some(i);
                break;
            }
        }
        match (found, hit_index) {
            (Some(hit), Some(i)) => {
                g.entries[i].used_ms = now;
                g.hits += 1;
                self.save_index(&mut g, false);
                Some(hit)
            }
            _ => {
                g.misses += 1;
                None
            }
        }
    }

    /// 打开一条"从磁盘读"的通道。
    pub fn open_reader(&self, off: u64, len: u64, total: u64) -> Option<Reader> {
        let file = File::open(&self.data_path).ok()?;
        Some(Reader {
            file,
            base: off,
            len,
            total,
            cap: self.cap,
        })
    }

    /// 开始往缓存里写：返回一个顺序追加器。已经完整命中 / 太大 / 长度未知的都不写。
    pub fn begin_write(&self, key: &str, total: u64) -> Option<Writer<'_>> {
        if key.is_empty() || total == 0 || total > self.cap.min(SINGLE_MAX) {
            return None;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&self.data_path)
            .ok()?;
        let mut g = self.lock();
        self.ensure_loaded(&mut g);
        if g.entries
            .iter()
            .any(|e| e.key == key && e.complete && e.total == total)
        {
            return None; /* 已经在缓存里，别再写一遍 */
        }
        /* 同一个 key 的旧条目（半截的）先划掉，免得索引里堆垃圾 */
        g.entries.retain(|e| e.key != key);
        /*
         * 保护"正在播的那一首"：如果这次写入会盖到它，就**放弃写缓存**
         * （宁可这一首不进缓存，也不能把正在读的数据改掉）。
         * 环形缓存写满一圈才会轮到它，正常情况下碰不到。
         */
        let start = g.write_pos % self.cap;
        if write_hits_entry(&g, start, total, self.cap, &protected_key()) {
            crate::media::platform::log::append(
                "cache: 这次写入会盖到正在播的那首，跳过缓存（保住播放）",
            );
            return None;
        }
        let read_pos = start; /* Drop 里要用，先存一份 */
        drop(g);
        Some(Writer {
            cache: self,
            file,
            key: String::from(key),
            start,
            pos: read_pos,
            written: 0,
            total,
            done: false,
        })
    }

}

/// 顺序追加器：只写"紧接着上一次"的字节（解码器乱序探针那段不写）。
pub struct Writer<'a> {
    cache: &'a DiskCache,
    file: File,
    key: String,
    start: u64,
    pos: u64,
    written: u64,
    total: u64,
    done: bool,
}

impl Writer<'_> {
    /// 这条数据是不是"紧接着上次写的"（解码器探测尾部时会乱序读，那种不写）。
    pub fn accepts(&self, off: u64) -> bool {
        off == self.written
    }

    /// 追加一段（调用方保证顺序）。
    pub fn append(&mut self, bytes: &[u8]) {
        if self.done || bytes.is_empty() {
            return;
        }
        let cap = self.cache.cap;
        let mut chunk = bytes;
        while !chunk.is_empty() {
            if self.pos >= cap {
                self.pos = 0; /* 环形：到末尾回开头接着写 */
            }
            let room = (cap - self.pos) as usize;
            let n = core::cmp::min(room, chunk.len());
            if self.file.seek(SeekFrom::Start(self.pos)).is_err() {
                self.done = true;
                return;
            }
            if self.file.write_all(&chunk[..n]).is_err() {
                self.done = true;
                return;
            }
            {
                let mut g = self.cache.lock();
                DiskCache::evict_overlap(&mut g, self.pos, n as u64, cap);
                g.write_pos = (self.pos + n as u64) % cap;
            }
            self.pos = (self.pos + n as u64) % cap;
            self.written += n as u64;
            chunk = &chunk[n..];
        }
    }

    /// 整首都写完了吗（写满了就该登记成"可命中"）。
    pub fn is_full(&self) -> bool {
        self.written >= self.total
    }

    /// 收尾：写满整首才登记成"可命中"。
    pub fn finish(mut self) {
        self.done = true;
        let complete = self.written >= self.total;
        let mut g = self.cache.lock();
        if self.written > 0 {
            g.entries.push(Entry {
                key: self.key.clone(),
                off: self.start,
                len: self.written,
                total: self.total,
                used_ms: now_ms(),
                complete,
            });
            self.cache.save_index(&mut g, true);
            crate::media::platform::log::append(&format!(
                "cache: {} 写入 {} KB（{}）",
                self.key,
                self.written / 1024,
                if complete { "可命中" } else { "半截，不入索引" }
            ));
        }
    }
}

impl Drop for Writer<'_> {
    fn drop(&mut self) {
        if !self.done {
            /* 没显式 finish（切歌打断等）：半截数据留在盘上，但不登记 —— 
             * 反正下次写到这里会被环形覆盖掉，不需要删。 */
            self.done = true;
            let mut g = self.cache.lock();
            g.write_pos = self.pos;
            let _ = self.file.flush();
        }
    }
}

/// 一条"只从磁盘读"的通道。
pub struct Reader {
    file: File,
    base: u64,
    len: u64,
    total: u64,
    cap: u64,
}

impl Reader {
    pub fn size(&self) -> u64 {
        self.total
    }

    /// 读条目内偏移 `off` 开始的一段。
    ///
    /// 注意**条目可能在环上跨了末尾**（写到 cache.dat 尾部就绕回开头继续），
    /// 所以这里按环形推进：越过文件末尾就回到 0，不能拿 `base + off` 直接 seek。
    pub fn read_at(&mut self, off: u64, dst: &mut [u8]) -> Result<usize, String> {
        if off >= self.len {
            return Ok(0);
        }
        let want = core::cmp::min(dst.len() as u64, self.len - off) as usize;
        if want == 0 {
            return Ok(0);
        }
        let cap = self.cap.max(1);
        let mut src = (self.base + off) % cap;
        let mut done = 0usize;
        while done < want {
            let room = core::cmp::min(cap - src, (want - done) as u64) as usize;
            self.file
                .seek(SeekFrom::Start(src))
                .map_err(|e| format!("缓存 seek 失败 {e}"))?;
            let mut got = 0usize;
            while got < room {
                match self.file.read(&mut dst[done + got..done + room]) {
                    Ok(0) => break,
                    Ok(n) => got += n,
                    Err(e) => return Err(format!("缓存读失败 {e}")),
                }
            }
            done += got;
            if got < room {
                break; /* 文件比索引短（被外部截断）：把已有的交出去 */
            }
            src = 0;
        }
        Ok(done)
    }
}

/// 缓存 key：能从地址里抠出歌曲 id 就用它（换 URL 还是同一首歌），否则哈希整串。
pub fn key_for_url(url: &str) -> String {
    if let Some(id) = extract_song_id(url) {
        return format!("n{id}");
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in url.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("u{h:016x}")
}

fn extract_song_id(url: &str) -> Option<&str> {
    if let Some(rest) = url.strip_prefix("netease:") {
        if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
            return Some(rest);
        }
    }
    for key in ["id=", "/song/"] {
        if let Some(pos) = url.find(key) {
            let rest = &url[pos + key.len()..];
            let digits: &str = rest
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap_or("");
            if !digits.is_empty() {
                /* 注意返回的是 url 里的一段，生命周期跟着 url */
                let start = rest.as_ptr() as usize - url.as_ptr() as usize;
                return Some(&url[start..start + digits.len()]);
            }
        }
    }
    None
}

static GLOBAL: OnceLock<DiskCache> = OnceLock::new();

/// 正在播放的那首歌的 key（`source/remote.rs` 打开流时登记）。
///
/// 环形缓存写满一圈才会轮到它，但真要有那么一天，宁可**这一首不进缓存**，
/// 也不能把正在读的字节改掉 —— 那就是"播到一半声音变了"。
static PROTECTED: Mutex<String> = Mutex::new(String::new());

pub fn set_protected(key: &str) {
    if let Ok(mut g) = PROTECTED.lock() {
        g.clear();
        g.push_str(key);
    }
}

fn protected_key() -> String {
    PROTECTED.lock().map(|g| g.clone()).unwrap_or_default()
}

/// 两次写入是否落在环上的同一段（环形区间相交）。
fn seg_hit(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> bool {
    a_start < b_end && b_start < a_end
}

/// 从 `start` 起写 `len` 字节（可能绕环）会不会盖到 `key` 那条记录。
fn write_hits_entry(g: &Inner, start: u64, len: u64, cap: u64, key: &str) -> bool {
    if key.is_empty() || len == 0 || cap == 0 {
        return false;
    }
    let Some(e) = g.entries.iter().find(|e| e.key == key) else {
        return false;
    };
    let e_end = e.off + e.len;
    let new_end = start + len;
    /* 新写入的两段：[start, min(new_end,cap)) 与跨环后的 [0, new_end-cap) */
    let new_segs = [(start, new_end.min(cap)), (0, new_end.saturating_sub(cap))];
    for (ns, ne) in new_segs {
        if ne <= ns {
            continue;
        }
        if seg_hit(ns, ne, e.off, e_end.min(cap)) {
            return true;
        }
        if e_end > cap && seg_hit(ns, ne, 0, e_end - cap) {
            return true;
        }
    }
    false
}

/// 真机上的那一个（路径固定，容量 32 MB）。
pub fn global() -> &'static DiskCache {
    GLOBAL.get_or_init(|| DiskCache::new(DATA_PATH, INDEX_PATH, CACHE_CAP_DEFAULT))
}
