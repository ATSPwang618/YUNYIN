//! `AudioSource` — the one seam between "where the bytes come from" and the
//! decoders (task book §14/§15).
//!
//! Rules that keep the architecture honest:
//!   - nothing above this trait may know about HTTP;
//!   - nothing below it may know about a music provider;
//!   - `read() == 0` means **real end of stream** and nothing else.  A source
//!     that is temporarily out of bytes must not report EOF — see
//!     `SourceError::WouldBlock` and the decoder gate described in §7/§8.
//!
//! Phase 0/1 status: the trait, the local implementation and the byte cache are
//! real and host-testable.  The HTTP source is the documented stub that Phase 2
//! fills in; nothing in the shipping player references these modules yet, which
//! is why unused items are allowed here.
#![allow(dead_code)]

pub mod cache;
pub mod http;
pub mod local;

use alloc::string::String;

/// Why a source operation failed.
///
/// `WouldBlock` is deliberately separate from `Eof`: the audio thread turns it
/// into silence *without* consuming the decoder (§8), while `Eof` is permanent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceError {
    /// No data right now, but the stream is still alive.
    WouldBlock,
    /// End of stream reached.
    Eof,
    /// The source cannot seek (network streams without ranges, live streams).
    NotSeekable,
    /// Transport failure (socket, DNS, TLS, read error).
    Network(String),
    /// Server answered, but not with a usable response.
    Http { status: u16 },
    /// The resolved URL expired and must be fetched again (§52).
    Expired,
    /// The operation was cancelled by a newer request (§56).
    Cancelled,
    /// Backing store failure (file gone, card removed).
    Io(String),
    /// Feature not implemented yet (Phase 2+ placeholders).
    Unsupported,
}

/// What kind of thing the bytes come from.  Used for logging/UI only — decoders
/// must not branch on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    LocalFile,
    HttpRange,
}

/// The seam itself: a seekable byte stream, decoders pull from it.
pub trait AudioSource {
    /// Fill `buf`, returning how many bytes were written.
    ///
    /// `Ok(0)` means end of stream.  `Err(WouldBlock)` means "call me again
    /// later with the same position".
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, SourceError>;

    /// Absolute seek.  Sources that cannot seek return `NotSeekable`.
    fn seek(&mut self, pos: u64) -> Result<(), SourceError>;

    /// Current absolute read position.
    fn tell(&self) -> u64;

    /// Total size when known (HTTP without Content-Length: `None`).
    fn size(&self) -> Option<u64>;

    /// Bytes readable *without blocking* — this is what the decoder gate and the
    /// cache watermarks look at (§13).
    fn available(&self) -> usize;

    /// True once the stream is finished; never a stand-in for "empty for now".
    fn is_eof(&self) -> bool;

    /// Sticky error, if the source already failed.
    fn error(&self) -> Option<SourceError>;

    fn kind(&self) -> SourceKind;
}
