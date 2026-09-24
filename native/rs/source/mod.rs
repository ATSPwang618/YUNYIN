//! `AudioSource` —— "字节从哪来"与"解码器"之间的**唯一接缝**（任务书 §14/§15）。
//!
//! 三条规矩，整个架构靠它们保持干净：
//!   - 这个 trait 之上的东西不许认识 HTTP；
//!   - 之下的东西不许认识"音乐平台"；
//!   - `read() == 0` **只代表真正结束**，别的什么都不代表。暂时没数据的源
//!     不允许报 EOF —— 用 `SourceError::WouldBlock`，配合 §7/§8 的 Gate 处理。
//!
//! 当前状态：trait、本地源、字节缓存都是真实现，且能在电脑上跑测试；HTTP 源是
//! Phase 2 要填的桩。正式播放路径还没引用这些模块，所以这里允许出现"未被使用"的项。
#![allow(dead_code)]

pub mod cache;
pub mod http;
pub mod local;
pub mod remote;

use alloc::string::String;

/// 源操作失败的原因。
///
/// `WouldBlock` 故意和 `Eof` 分开：音频线程遇到它只会**输出静音**、绝不消耗解码器
/// （§8）；而 `Eof` 是永久结束。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceError {
    /// 现在没数据，但流还活着（等缓存补齐即可继续）。
    WouldBlock,
    /// 真正的流结束。
    Eof,
    /// 这个源不能 seek（不支持 Range 的流、直播流）。
    NotSeekable,
    /// 传输层失败（socket / DNS / TLS / 读错误）。
    Network(String),
    /// 服务器回了，但不是能用的响应。
    Http { status: u16 },
    /// 解析出来的 URL 过期了，必须重新获取（§52）。
    Expired,
    /// 被更新的请求取消了（切歌就是这条，§56）。
    Cancelled,
    /// 底层存储出问题（文件没了、卡被拔了）。
    Io(String),
    /// 功能还没实现（Phase 2 及以后的占位）。
    Unsupported,
}

/// 字节来自哪一类源。只给日志和界面用 —— **解码器不许按它分支**。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    LocalFile,
    HttpRange,
}

/// 接缝本体：一条可 seek 的字节流，解码器从它拉数据。
pub trait AudioSource {
    /// 填 `buf`，返回实际写入的字节数。
    ///
    /// `Ok(0)` = 结束；`Err(WouldBlock)` = "位置不变，过会儿再来问我"。
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, SourceError>;

    /// 绝对 seek；不能 seek 的源返回 `NotSeekable`。
    fn seek(&mut self, pos: u64) -> Result<(), SourceError>;

    /// 当前绝对读位置。
    fn tell(&self) -> u64;

    /// 已知的总长度（HTTP 没给 Content-Length 时是 `None`）。
    fn size(&self) -> Option<u64>;

    /// **不阻塞**就能读出的字节数 —— Gate 与缓存水位都看这个值（§13）。
    fn available(&self) -> usize;

    /// 流真正结束了才是 true；**不能**拿它表示"暂时空了"。
    fn is_eof(&self) -> bool;

    /// 已经失败时返回粘性错误，否则 None。
    fn error(&self) -> Option<SourceError>;

    /// 源的类型（本地文件 / HTTP Range）。
    fn kind(&self) -> SourceKind;
}
