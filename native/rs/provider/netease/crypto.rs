//! 网易云 web API 的请求加密（任务书 §46）。
//!
//! 当前状态：只有接口。参考实现是 `music-lib` 的 `netease` 包，计划是**照抄**而不是
//! 自己推 —— 填充或密钥编码差一点点，请求照样发得出去、但拿回来的是错数据，
//! 这种 bug 在掌机上最难查。
//!
//! 给移植的人：两种形式各自要做什么
//!
//! ```text
//! weapi   params → JSON → AES-128-CBC(密钥, 固定 IV, PKCS#7)
//!                  → hex；同一个 AES 密钥再用固定 RSA 公钥加密，作为 encSecKey 一起发
//! eapi    params → JSON → AES-128-ECB(固定密钥) → hex，作为 params 发
//! ```
//!
//! 两者都需要在明文前加 16 字节随机前缀；具体排布看参考实现。
//! 这里不发明任何密钥材料。
#![allow(dead_code)]

use alloc::string::String;
use alloc::vec::Vec;

/// 可以直接当表单 body 发出去的载荷。
#[derive(Clone, Debug, Default)]
pub struct Payload {
    pub params: String,
    /// 只有 `weapi` 会带这个字段。
    pub enc_sec_key: Option<String>,
}

pub fn encrypt_weapi(_params: &[(String, String)]) -> Result<Payload, &'static str> {
    Err("weapi encryption not ported yet (§46: port from music-lib)")
}

pub fn encrypt_eapi(_params: &[(String, String)]) -> Result<Payload, &'static str> {
    Err("eapi encryption not ported yet (§46: port from music-lib)")
}

/// 对原始字节做 Base64，封面/歌词相关端点会用到。
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

/// AES 原语的占位：让模块树诚实反映"还缺什么"；移植时会连带它自己的实现一起进来。
pub fn aes_128_cbc_encrypt(_key: &[u8], _iv: &[u8], _data: &[u8]) -> Vec<u8> {
    Vec::new()
}
