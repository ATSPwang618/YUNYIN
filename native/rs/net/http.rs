//! HTTP vocabulary shared by the transport and the sources.
//!
//! Deliberately small: a request description, the response facts the player
//! needs, and a trait the Vita transport implements in `yhttp.c`.  Everything
//! here is transport-level; no provider knowledge (§15).
#![allow(dead_code)]

use alloc::string::String;
use alloc::vec::Vec;

/// One HTTP exchange, described rather than performed.
#[derive(Clone, Debug)]
pub struct Request {
    pub url: String,
    /// `bytes=0-8191`, `bytes=-12288`, ... (`None` for a plain GET).
    pub range: Option<String>,
    /// CDNs check these; the provider supplies them (§45).
    pub referer: Option<String>,
    pub user_agent: Option<String>,
    pub cookie: Option<String>,
    /// TLS verification mode.  `Verify` is the default everywhere (§23).
    pub tls: TlsMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlsMode {
    /// Certificate chain + hostname checked.
    Verify,
    /// Accepted only for endpoints the provider documents as pinning their own
    /// trust (used by NetEase's CDN in the reference implementation).
    Insecure,
}

/// What a completed exchange produced.
#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    /// `Content-Range`/`Content-Length` are how the source learns the real size
    /// of a stream (§19: never trust the URL suffix for that).
    pub content_length: Option<u64>,
    pub content_range: Option<(u64, u64, u64)>,
    pub bytes: Vec<u8>,
}

impl Response {
    pub fn is_partial(&self) -> bool {
        self.status == 206
    }
    pub fn is_ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Transport seam.  The Vita implementation is `yhttp.c`; tests can substitute
/// a fake that serves bytes from memory, which is how the cache logic gets
/// exercised without a network.
pub trait HttpClient {
    fn get(&mut self, req: &Request) -> Result<Response, super::super::source::SourceError>;
    /// Abort an in-flight transfer (§24/§56).
    fn cancel(&mut self) -> Result<(), super::super::source::SourceError>;
}

/// Redirect handling policy (§22): follow, but only the same scheme and host
/// the provider gave us, and never downgrade TLS.
pub fn redirect_allowed(from: &str, to: &str) -> bool {
    let scheme = |u: &str| u.split("://").next().unwrap_or("").as_bytes().to_vec();
    scheme(from) == scheme(to)
}
