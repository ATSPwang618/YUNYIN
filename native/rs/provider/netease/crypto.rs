//! 网易云 weapi 请求加密。
//!
//! 照 NeteaseCloudMusicApi 的 `weapi`（不是本文件旧注释里的「先 hex 再把同一把
//! AES 密钥做 RSA」）。差一个字节，服务器就回「参数错误」，掌机上查不出来，
//! 所以步骤按已经对过真接口的那份来：
//!
//! ```text
//! json = 参数对象（键顺序保持调用方给的顺序）
//! params = Base64( AES-128-CBC( Base64( AES-128-CBC(json, 固定密钥) ), 随机密钥 ) )
//!          固定密钥 0CoJUm6Qyw8W8jud ，IV 0102030405060708 ，PKCS#7
//! encSecKey = RSA( 随机密钥反转后的字节，无填充，指数 010001 )
//!             结果是 256 位十六进制
//! ```
//!
//! eapi 还没用到，保持未移植。这里不引入第三方大数库：Vita 那份 PocketJS
//! 工程没有现成的 bigint crate，AES 和 1024 位乘方都写在这个文件里。
#![allow(dead_code)]

use alloc::string::String;
use alloc::vec::Vec;

const PRESET_KEY: &[u8] = b"0CoJUm6Qyw8W8jud";
const IV: &[u8] = b"0102030405060708";
const BASE62: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
/// ASN.1 里 INTEGER 带的符号字节 `00` 去掉之后，就是这 128 字节模数。
const RSA_MODULUS_HEX: &str = "\
e0b509f6259df8642dbc35662901477df22677ec152b5ff68ace615bb7b725\
152b3ab17a876aea8a5aa76d2e417629ec4ee341f56135fccf695280104e03\
12ecbda92557c93870114af6c9d05c4f7f0c3685b7a46bee255932575cce10\
b424d813cfe4875d3e82047b97ddef52741d546b8e289dc6935b3ece0462db\
0a22b8e7";
const RSA_EXP: u32 = 0x10001;

/// 可以直接当表单 body 发出去的载荷。
#[derive(Clone, Debug, Default)]
pub struct Payload {
    pub params: String,
    /// 只有 `weapi` 会带这个字段。
    pub enc_sec_key: Option<String>,
}

/// 把键值对按给定顺序收成 JSON 对象，再做 weapi。随机 16 位密钥。
pub fn encrypt_weapi(params: &[(String, String)]) -> Result<Payload, &'static str> {
    let json = json_object(params);
    encrypt_weapi_json(&json, &random_secret())
}

/// 指定密钥的 weapi。主机对照测试用固定密钥，真机走 [`encrypt_weapi`]。
pub fn encrypt_weapi_json(json: &str, secret: &str) -> Result<Payload, &'static str> {
    if secret.len() != 16 || !secret.is_ascii() {
        return Err("weapi secret must be 16 ascii chars");
    }
    let first = aes_128_cbc_encrypt(PRESET_KEY, IV, json.as_bytes());
    let first_b64 = base64(&first);
    let second = aes_128_cbc_encrypt(secret.as_bytes(), IV, first_b64.as_bytes());
    Ok(Payload {
        params: base64(&second),
        enc_sec_key: Some(rsa_encrypt_secret(secret)),
    })
}

pub fn encrypt_eapi(_params: &[(String, String)]) -> Result<Payload, &'static str> {
    Err("eapi encryption not ported yet")
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        let n = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8) | data[i + 2] as u32;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(T[((n >> 6) & 63) as usize] as char);
        out.push(T[(n & 63) as usize] as char);
        i += 3;
    }
    let rest = data.len() - i;
    if rest == 1 {
        let n = (data[i] as u32) << 16;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rest == 2 {
        let n = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(T[((n >> 6) & 63) as usize] as char);
        out.push('=');
    }
    out
}

pub fn hex(data: &[u8]) -> String {
    let mut out = String::new();
    for b in data {
        out.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((b & 0x0F) as u32, 16).unwrap_or('0'));
    }
    out
}

/// `application/x-www-form-urlencoded`。Base64 里的 `+` `/` `=` 必须转义。
pub fn form_escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                const HEX: &[u8] = b"0123456789ABCDEF";
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0F) as usize] as char);
            }
        }
    }
    out
}

pub fn aes_128_cbc_encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    if key.len() != 16 || iv.len() != 16 {
        return Vec::new();
    }
    let mut src = Vec::with_capacity(data.len() + 16);
    src.extend_from_slice(data);
    let pad = 16 - (src.len() % 16);
    src.extend(core::iter::repeat(pad as u8).take(pad));
    let rk = expand_key(key);
    let mut prev = [0u8; 16];
    prev.copy_from_slice(iv);
    let mut out = Vec::with_capacity(src.len());
    for chunk in src.chunks(16) {
        let mut block = [0u8; 16];
        for i in 0..16 {
            block[i] = chunk[i] ^ prev[i];
        }
        encrypt_block(&mut block, &rk);
        out.extend_from_slice(&block);
        prev = block;
    }
    out
}

fn json_object(pairs: &[(String, String)]) -> String {
    let mut s = String::from("{");
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push('"');
        push_json_str(&mut s, k);
        s.push_str("\":\"");
        push_json_str(&mut s, v);
        s.push('"');
    }
    s.push('}');
    s
}

fn push_json_str(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&alloc::format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}

pub(crate) fn random_secret() -> String {
    use core::sync::atomic::{AtomicU64, Ordering};
    static STATE: AtomicU64 = AtomicU64::new(0xA5A5_5A5A_1234_5678);
    let tick = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    let mut x = STATE.load(Ordering::Relaxed) ^ tick.rotate_left(17);
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    if x == 0 {
        x = 0x9E37_79B9_7F4A_7C15;
    }
    STATE.store(x, Ordering::Relaxed);
    let mut out = String::new();
    let mut z = x;
    for _ in 0..16 {
        z = z.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(0x6A09_E667);
        out.push(BASE62[(z as usize) % 62] as char);
    }
    out
}

fn rsa_encrypt_secret(secret: &str) -> String {
    let mut msg = [0u8; 128];
    let raw = secret.as_bytes();
    /* 反转后靠右放，左边补 0。这就是 RSA_NO_PADDING 的 128 字节块。 */
    for (i, &b) in raw.iter().rev().enumerate() {
        msg[128 - raw.len() + i] = b;
    }
    let n = Uint::from_be_hex(RSA_MODULUS_HEX);
    let m = Uint::from_be(&msg);
    let c = m.modpow(RSA_EXP, &n);
    let bytes = c.to_be_fixed(128);
    hex(&bytes)
}

/* --------------------------------------------------------------- AES-128 -- */

const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

const RCON: [u8; 11] = [
    0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36,
];

fn xtime(a: u8) -> u8 {
    let hi = a & 0x80;
    let mut b = a << 1;
    if hi != 0 {
        b ^= 0x1b;
    }
    b
}

fn expand_key(key: &[u8]) -> [u8; 176] {
    let mut w = [0u8; 176];
    w[..16].copy_from_slice(&key[..16]);
    let mut i = 16;
    let mut rcon_i = 1;
    while i < 176 {
        let mut temp = [w[i - 4], w[i - 3], w[i - 2], w[i - 1]];
        if i % 16 == 0 {
            let t0 = temp[0];
            temp[0] = SBOX[temp[1] as usize] ^ RCON[rcon_i];
            temp[1] = SBOX[temp[2] as usize];
            temp[2] = SBOX[temp[3] as usize];
            temp[3] = SBOX[t0 as usize];
            rcon_i += 1;
        }
        for k in 0..4 {
            w[i] = w[i - 16] ^ temp[k];
            i += 1;
        }
    }
    w
}

fn encrypt_block(block: &mut [u8; 16], rk: &[u8; 176]) {
    add_round_key(block, &rk[0..16]);
    for round in 1..10 {
        sub_bytes(block);
        shift_rows(block);
        mix_columns(block);
        add_round_key(block, &rk[round * 16..round * 16 + 16]);
    }
    sub_bytes(block);
    shift_rows(block);
    add_round_key(block, &rk[160..176]);
}

fn add_round_key(block: &mut [u8; 16], key: &[u8]) {
    for i in 0..16 {
        block[i] ^= key[i];
    }
}

fn sub_bytes(block: &mut [u8; 16]) {
    for b in block.iter_mut() {
        *b = SBOX[*b as usize];
    }
}

fn shift_rows(block: &mut [u8; 16]) {
    let b = *block;
    block[1] = b[5];
    block[5] = b[9];
    block[9] = b[13];
    block[13] = b[1];
    block[2] = b[10];
    block[6] = b[14];
    block[10] = b[2];
    block[14] = b[6];
    block[3] = b[15];
    block[7] = b[3];
    block[11] = b[7];
    block[15] = b[11];
}

fn mix_columns(block: &mut [u8; 16]) {
    for c in 0..4 {
        let i = c * 4;
        let a0 = block[i];
        let a1 = block[i + 1];
        let a2 = block[i + 2];
        let a3 = block[i + 3];
        block[i] = xtime(a0) ^ (xtime(a1) ^ a1) ^ a2 ^ a3;
        block[i + 1] = a0 ^ xtime(a1) ^ (xtime(a2) ^ a2) ^ a3;
        block[i + 2] = a0 ^ a1 ^ xtime(a2) ^ (xtime(a3) ^ a3);
        block[i + 3] = (xtime(a0) ^ a0) ^ a1 ^ a2 ^ xtime(a3);
    }
}

/* ---------------------------------------------------- 1024-bit 无符号整数 -- */

#[derive(Clone)]
struct Uint {
    /// 小端，每个 limb 32 位。
    d: Vec<u32>,
}

impl Uint {
    fn from_u32(v: u32) -> Self {
        Self { d: alloc::vec![v] }
    }

    fn normalize(&mut self) {
        while self.d.len() > 1 && *self.d.last().unwrap_or(&0) == 0 {
            self.d.pop();
        }
        if self.d.is_empty() {
            self.d.push(0);
        }
    }

    fn from_be(bytes: &[u8]) -> Self {
        let mut d = alloc::vec![0u32; (bytes.len() + 3) / 4];
        for (i, &b) in bytes.iter().enumerate() {
            let from_end = bytes.len() - 1 - i;
            d[from_end / 4] |= (b as u32) << ((from_end % 4) * 8);
        }
        let mut u = Self { d };
        u.normalize();
        u
    }

    fn from_be_hex(s: &str) -> Self {
        let mut bytes = Vec::with_capacity(s.len() / 2);
        let sb = s.as_bytes();
        let mut i = 0;
        while i + 1 < sb.len() {
            let hi = hex_val(sb[i]);
            let lo = hex_val(sb[i + 1]);
            bytes.push((hi << 4) | lo);
            i += 2;
        }
        Self::from_be(&bytes)
    }

    fn to_be_fixed(&self, n: usize) -> Vec<u8> {
        let mut out = alloc::vec![0u8; n];
        for (i, limb) in self.d.iter().enumerate() {
            for b in 0..4 {
                let from_end = i * 4 + b;
                if from_end < n {
                    out[n - 1 - from_end] = ((*limb >> (b * 8)) & 0xff) as u8;
                }
            }
        }
        out
    }

    fn bitlen(&self) -> usize {
        let last = *self.d.last().unwrap_or(&0);
        if self.d.len() == 1 && last == 0 {
            return 0;
        }
        (self.d.len() - 1) * 32 + (32 - last.leading_zeros() as usize)
    }

    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        let a = self.d.len();
        let b = other.d.len();
        if a != b {
            return a.cmp(&b);
        }
        for i in (0..a).rev() {
            if self.d[i] != other.d[i] {
                return self.d[i].cmp(&other.d[i]);
            }
        }
        core::cmp::Ordering::Equal
    }

    fn shl(&self, bits: usize) -> Self {
        if bits == 0 {
            return self.clone();
        }
        let words = bits / 32;
        let rem = bits % 32;
        let mut d = alloc::vec![0u32; self.d.len() + words + 1];
        if rem == 0 {
            d[words..words + self.d.len()].copy_from_slice(&self.d);
        } else {
            let mut carry = 0u32;
            for (i, &w) in self.d.iter().enumerate() {
                d[words + i] = (w << rem) | carry;
                carry = w >> (32 - rem);
            }
            d[words + self.d.len()] = carry;
        }
        let mut u = Self { d };
        u.normalize();
        u
    }

    fn add(&self, other: &Self) -> Self {
        let n = self.d.len().max(other.d.len());
        let mut d = alloc::vec![0u32; n + 1];
        let mut carry = 0u64;
        for i in 0..n {
            let a = *self.d.get(i).unwrap_or(&0) as u64;
            let b = *other.d.get(i).unwrap_or(&0) as u64;
            let s = a + b + carry;
            d[i] = s as u32;
            carry = s >> 32;
        }
        d[n] = carry as u32;
        let mut u = Self { d };
        u.normalize();
        u
    }

    fn sub(&self, other: &Self) -> Self {
        let mut d = alloc::vec![0u32; self.d.len()];
        let mut borrow = 0i64;
        for i in 0..self.d.len() {
            let a = self.d[i] as i64;
            let b = *other.d.get(i).unwrap_or(&0) as i64;
            let mut v = a - b - borrow;
            if v < 0 {
                v += 1i64 << 32;
                borrow = 1;
            } else {
                borrow = 0;
            }
            d[i] = v as u32;
        }
        let mut u = Self { d };
        u.normalize();
        u
    }

    fn mul(&self, other: &Self) -> Self {
        let mut d = alloc::vec![0u32; self.d.len() + other.d.len() + 1];
        for i in 0..self.d.len() {
            let mut carry = 0u64;
            for j in 0..other.d.len() {
                let t = d[i + j] as u64 + (self.d[i] as u64) * (other.d[j] as u64) + carry;
                d[i + j] = t as u32;
                carry = t >> 32;
            }
            d[i + other.d.len()] = carry as u32;
        }
        let mut u = Self { d };
        u.normalize();
        u
    }

    fn rem(&self, m: &Self) -> Self {
        if m.bitlen() == 0 {
            return Self::from_u32(0);
        }
        if self.cmp(m) == core::cmp::Ordering::Less {
            return self.clone();
        }
        let mut a = self.clone();
        let shift = a.bitlen() - m.bitlen();
        for i in (0..=shift).rev() {
            let s = m.shl(i);
            if a.cmp(&s) != core::cmp::Ordering::Less {
                a = a.sub(&s);
            }
        }
        a
    }

    fn modpow(&self, mut exp: u32, m: &Self) -> Self {
        let mut result = Self::from_u32(1);
        let mut base = self.rem(m);
        while exp > 0 {
            if exp & 1 == 1 {
                result = result.mul(&base).rem(m);
            }
            exp >>= 1;
            if exp > 0 {
                base = base.mul(&base).rem(m);
            }
        }
        result
    }
}

fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}
