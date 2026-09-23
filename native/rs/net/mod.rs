//! Network layer (Phase 0/2, task book §20/§21).
//!
//! This module owns *transport*: HTTP over the Vita's native stack, with the
//! pieces the platform actually needs — TLS, Range, redirects, cookies,
//! cancellation.  It knows nothing about music providers, and providers never
//! touch a socket.
//!
//! Phase 0 status: the types and the `HttpClient` seam are here; the Vita
//! implementation lands with `yhttp.c` (§21) once the on-device smoke test
//! (§25/§26) confirms HTTPS + Range + 206 + cookies + cancel.
#![allow(dead_code)]

pub mod http;
pub mod probe;

/// Where the network layer is in its bring-up.  Reported to the UI so the
/// About page can say "network: not implemented" instead of pretending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetState {
    /// Nothing implemented yet (Phase 0).
    Absent,
    /// Transport works, providers do not (Phase 2).
    Transport,
    /// Providers work (Phase 3+).
    Ready,
}

pub fn state() -> NetState {
    /* Phase 0: the transport probe exists, but no provider does yet. */
    NetState::Absent
}
