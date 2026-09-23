//! Request encryption for the NetEase web API (task book §46).
//!
//! Phase 3 status: interface only.  The reference implementation is
//! `music-lib`'s `netease` package, and the plan is to port it verbatim rather
//! than re-derive it — a subtly wrong padding or key encoding produces working
//! requests with wrong answers, which is the worst kind of bug to chase on a
//! handheld.
//!
//! What each flavour needs, for whoever ports it:
//!
//! ```text
//! weapi   params -> JSON -> AES-128-CBC(secret key, fixed IV, PKCS#7)
//!                  -> hex; the same AES key is RSA-encrypted with the fixed
//!                  public modulus and also sent as `encSecKey`
//! eapi    params -> JSON -> AES-128-ECB(fixed key) -> hex, sent as `params`
//! ```
//!
//! Both need a 16-byte random prefix on the plaintext; the reference shows the
//! exact layout.  No key material is invented here.
#![allow(dead_code)]

use alloc::string::String;
use alloc::vec::Vec;

/// A payload ready to be sent as a form body.
#[derive(Clone, Debug, Default)]
pub struct Payload {
    pub params: String,
    /// Only `weapi` sends this.
    pub enc_sec_key: Option<String>,
}

pub fn encrypt_weapi(_params: &[(String, String)]) -> Result<Payload, &'static str> {
    Err("weapi encryption not ported yet (§46: port from music-lib)")
}

pub fn encrypt_eapi(_params: &[(String, String)]) -> Result<Payload, &'static str> {
    Err("eapi encryption not ported yet (§46: port from music-lib)")
}

/// Base64 of a raw byte slice, used by the picture/lyric endpoints.
pub fn base64(_data: &[u8]) -> String {
    String::new()
}

pub fn hex(_data: &[u8]) -> String {
    let mut out = String::new();
    for b in _data {
        out.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((b & 0x0F) as u32, 16).unwrap_or('0'));
    }
    out
}

/// Placeholder for the AES primitive so the module tree stays honest about what
/// it still needs; the port will bring its own implementation.
pub fn aes_128_cbc_encrypt(_key: &[u8], _iv: &[u8], _data: &[u8]) -> Vec<u8> {
    Vec::new()
}
