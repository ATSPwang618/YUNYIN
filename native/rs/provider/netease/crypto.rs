//! 网易云 web API 的请求加密（Phase 3，任务书 §46）。
//!
//! 两种形式，算法照公开参考实现（不是自己推的）：
//!
//! ```text
//! weapi   json ──AES-128-CBC(固定预设密钥, 固定 IV, PKCS#7)──▶ base64
//!              ──AES-128-CBC(每次随机的 16 字符密钥, 固定 IV)──▶ base64 = params
//!              随机密钥（字节序反转）──RSA(固定公钥, e=65537)──▶ 256 位十六进制 = encSecKey
//! eapi    json ──"nobody{path}use{json}md5forever" 的 MD5 摘要接在明文后
//!              ──AES-128-ECB(固定密钥 e82ckenh8dichen8, PKCS#7)──▶ 大写十六进制 = params
//! ```
//!
//! 关键事实（都是电脑上用真接口验证过的，不是猜的）：
//!   * `weapi` 只有**一层** RSA：加密的是那 16 个字符的随机密钥，不是 JSON；
//!   * 密钥是 16 个 ASCII 字符（`[A-Za-z0-9]`），不是二进制密钥；
//!   * 两次 AES 都是 **PKCS#7** 填充，块长 16 字节；
//!   * `expi` 由服务端在响应里给（实测 1200 秒），本地不用猜。
//!
//! 为什么自己写而不引 crate：Vita 的交叉编译链上多一个依赖就多一份踩坑机会，
//! 而这里只需要"加密"方向（AES 只用加密、RSA 是裸模幂），代码量可控，
//! 每一块都有公开标准向量锁着（FIPS-197 / NIST SP800-38A / RFC 1321 / RFC 4648）。
//! 模块里的一切都是纯计算，能在电脑上直接跑测试。
#![allow(dead_code)]

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// 可以直接当表单 body 发出去的载荷。
#[derive(Clone, Debug, Default)]
pub struct Payload {
    pub params: String,
    /// 只有 `weapi` 会带这个字段。
    pub enc_sec_key: Option<String>,
}

/// weapi 第一层的固定预设密钥（公开常量，不是秘密）。
pub const WEAPI_PRESET_KEY: &[u8; 16] = b"0CoJUm6Qyw8W8jud";
/// weapi 两层的固定 IV。
pub const WEAPI_IV: &[u8; 16] = b"0102030405060708";
/// eapi 的固定 AES 密钥（公开常量）。
pub const EAPI_KEY: &[u8; 16] = b"e82ckenh8dichen8";
/// eapi 明文里的固定分隔串。
pub const EAPI_SEP: &str = "-36cd479b6b5-";

/// 16 字符密钥用的字符表（62 进制，顺序无所谓，但要与随机映射一致）。
const BASE62: &[u8; 62] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/* --------------------------------------------------------------- 编码 -- */

pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

pub fn hex_lower(data: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 0x0F) as usize] as char);
    }
    s
}

pub fn hex_upper(data: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789ABCDEF";
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 0x0F) as usize] as char);
    }
    s
}

/* ----------------------------------------------------------------- MD5 -- */

pub fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20,
        5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
        6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    let mut msg = Vec::with_capacity(data.len() + 72);
    msg.extend_from_slice(data);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&(data.len() as u64).wrapping_mul(8).to_le_bytes());

    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            *w = u32::from_le_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            let sum = a
                .wrapping_add(f)
                .wrapping_add(K[i])
                .wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(S[i]));
            a = tmp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

/* --------------------------------------------------------------- AES -- */

const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab,
    0x76, 0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4,
    0x72, 0xc0, 0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71,
    0xd8, 0x31, 0x15, 0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2,
    0xeb, 0x27, 0xb2, 0x75, 0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6,
    0xb3, 0x29, 0xe3, 0x2f, 0x84, 0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb,
    0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf, 0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45,
    0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8, 0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5,
    0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2, 0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44,
    0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73, 0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a,
    0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb, 0xe0, 0x32, 0x3a, 0x0a, 0x49,
    0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79, 0xe7, 0xc8, 0x37, 0x6d,
    0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08, 0xba, 0x78, 0x25,
    0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a, 0x70, 0x3e,
    0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e, 0xe1,
    0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb,
    0x16,
];

const RCON: [u8; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];

fn expand_key(key: &[u8; 16]) -> [u8; 176] {
    let mut w = [0u8; 176];
    w[..16].copy_from_slice(key);
    for i in 4..44 {
        let mut t = [
            w[(i - 1) * 4],
            w[(i - 1) * 4 + 1],
            w[(i - 1) * 4 + 2],
            w[(i - 1) * 4 + 3],
        ];
        if i % 4 == 0 {
            t = [t[1], t[2], t[3], t[0]]; /* RotWord */
            for b in t.iter_mut() {
                *b = SBOX[*b as usize]; /* SubWord */
            }
            t[0] ^= RCON[i / 4 - 1];
        }
        for j in 0..4 {
            w[i * 4 + j] = w[(i - 4) * 4 + j] ^ t[j];
        }
    }
    w
}

fn add_round_key(s: &mut [u8; 16], rk: &[u8; 176], round: usize) {
    for i in 0..16 {
        s[i] ^= rk[round * 16 + i];
    }
}

fn sub_bytes(s: &mut [u8; 16]) {
    for b in s.iter_mut() {
        *b = SBOX[*b as usize];
    }
}

/// 状态按列优先存放（`s[c*4+r]`），行 r 循环左移 r 格。
fn shift_rows(s: &mut [u8; 16]) {
    let t = *s;
    for r in 1..4 {
        for c in 0..4 {
            s[c * 4 + r] = t[((c + r) % 4) * 4 + r];
        }
    }
}

fn xtime(x: u8) -> u8 {
    (x << 1) ^ if x & 0x80 != 0 { 0x1b } else { 0 }
}

fn mix_columns(s: &mut [u8; 16]) {
    for c in 0..4 {
        let a = [s[c * 4], s[c * 4 + 1], s[c * 4 + 2], s[c * 4 + 3]];
        let t = a[0] ^ a[1] ^ a[2] ^ a[3];
        s[c * 4] = a[0] ^ t ^ xtime(a[0] ^ a[1]);
        s[c * 4 + 1] = a[1] ^ t ^ xtime(a[1] ^ a[2]);
        s[c * 4 + 2] = a[2] ^ t ^ xtime(a[2] ^ a[3]);
        s[c * 4 + 3] = a[3] ^ t ^ xtime(a[3] ^ a[0]);
    }
}

pub fn aes128_encrypt_block(key: &[u8; 16], block: &[u8; 16]) -> [u8; 16] {
    let rk = expand_key(key);
    let mut s = *block;
    add_round_key(&mut s, &rk, 0);
    for round in 1..10 {
        sub_bytes(&mut s);
        shift_rows(&mut s);
        mix_columns(&mut s);
        add_round_key(&mut s, &rk, round);
    }
    sub_bytes(&mut s);
    shift_rows(&mut s);
    add_round_key(&mut s, &rk, 10);
    s
}

fn pkcs7_pad(data: &[u8]) -> Vec<u8> {
    let pad = 16 - (data.len() % 16);
    let mut out = Vec::with_capacity(data.len() + pad);
    out.extend_from_slice(data);
    out.resize(out.len() + pad, pad as u8);
    out
}

pub fn aes128_ecb_encrypt_pkcs7(key: &[u8], data: &[u8]) -> Result<Vec<u8>, &'static str> {
    let key: &[u8; 16] = key.try_into().map_err(|_| "AES 密钥必须是 16 字节")?;
    let padded = pkcs7_pad(data);
    let mut out = Vec::with_capacity(padded.len());
    for chunk in padded.chunks_exact(16) {
        let block: [u8; 16] = chunk.try_into().map_err(|_| "内部错误：块不是 16 字节")?;
        out.extend_from_slice(&aes128_encrypt_block(key, &block));
    }
    Ok(out)
}

pub fn aes128_cbc_encrypt_pkcs7(
    key: &[u8],
    iv: &[u8],
    data: &[u8],
) -> Result<Vec<u8>, &'static str> {
    let key: &[u8; 16] = key.try_into().map_err(|_| "AES 密钥必须是 16 字节")?;
    let iv: &[u8; 16] = iv.try_into().map_err(|_| "CBC 的 IV 必须是 16 字节")?;
    let padded = pkcs7_pad(data);
    let mut out = Vec::with_capacity(padded.len());
    let mut prev = *iv;
    for chunk in padded.chunks_exact(16) {
        let mut block = [0u8; 16];
        for i in 0..16 {
            block[i] = chunk[i] ^ prev[i];
        }
        let enc = aes128_encrypt_block(key, &block);
        out.extend_from_slice(&enc);
        prev = enc;
    }
    Ok(out)
}

/* --------------------------------------------------------------- RSA -- */
/*
 * 这里只需要"裸 RSA 加密"：把 16 字节的 ASCII 密钥（字节序反转）当成大整数，
 * 做一次 m^65537 mod n，输出 128 字节大端。
 *
 * 没有第三方大数库，所以用 1024 位定点数组 + "倍加"模乘：
 *   mul_mod 从高位到低位扫描 b 的每一 bit：r = 2r mod n；该位为 1 再加 a。
 * 一次模幂约 17 次模乘，每次 1024 轮 32 位加法 —— 在 Vita 上也是毫秒级，
 * 而且只发生在"解析一首歌"的后台线程里。
 */

/// 1024 位无符号整数，小端 32 位肢。
type Big = [u32; 32];

/// 网易云 weapi 固定公钥的模数（公开常量）。
const RSA_N_HEX: &str = "\
00e0b509f6259df8642dbc35662901477df22677ec152b5ff68ace615bb7b725152b3ab17a876aea8a5aa76d2e\
417629ec4ee341f56135fccf695280104e0312ecbda92557c93870114af6c9d05c4f7f0c3685b7a46bee255932\
575cce10b424d813cfe4875d3e82047b97ddef52741d546b8e289dc6935b3ece0462db0a22b8e7";
const RSA_E: u32 = 0x010001;

fn big_from_be_bytes(bytes: &[u8]) -> Big {
    let mut out = [0u32; 32];
    let start = bytes.len().saturating_sub(128);
    for (i, b) in bytes[start..].iter().rev().enumerate() {
        out[i / 4] |= (*b as u32) << (8 * (i % 4));
    }
    out
}

fn big_from_hex_128(hex: &str) -> Big {
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0))
        .collect();
    big_from_be_bytes(&bytes)
}

fn big_ge(a: &Big, b: &Big) -> bool {
    for i in (0..32).rev() {
        if a[i] != b[i] {
            return a[i] > b[i];
        }
    }
    true
}

/// `(extra<<1024 + r) - b`，结果写回 `r`；`extra` 只允许 0/1（保证不借穿）。
fn big_sub_in_place(r: &mut Big, b: &Big, extra: u32) {
    let mut borrow = 0u64;
    for i in 0..32 {
        let lhs = r[i] as u64;
        let sub = b[i] as u64 + borrow;
        if lhs >= sub {
            r[i] = (lhs - sub) as u32;
            borrow = 0;
        } else {
            r[i] = (lhs + (1u64 << 32) - sub) as u32;
            borrow = 1;
        }
    }
    /* 最高肢：extra 借 1 是唯一来源，减完必然是 0 */
    let _ = extra as u64 - borrow;
}

fn big_double_mod(a: &Big, n: &Big) -> Big {
    let mut r = [0u32; 32];
    let mut carry = 0u64;
    for i in 0..32 {
        let v = (a[i] as u64) * 2 + carry;
        r[i] = v as u32;
        carry = v >> 32;
    }
    if carry != 0 || big_ge(&r, n) {
        big_sub_in_place(&mut r, n, carry as u32);
    }
    r
}

fn big_add_mod(a: &Big, b: &Big, n: &Big) -> Big {
    let mut r = [0u32; 32];
    let mut carry = 0u64;
    for i in 0..32 {
        let v = a[i] as u64 + b[i] as u64 + carry;
        r[i] = v as u32;
        carry = v >> 32;
    }
    if carry != 0 || big_ge(&r, n) {
        big_sub_in_place(&mut r, n, carry as u32);
    }
    r
}

fn big_mul_mod(a: &Big, b: &Big, n: &Big) -> Big {
    let mut r = [0u32; 32];
    for i in (0..32).rev() {
        for bit in (0..32).rev() {
            r = big_double_mod(&r, n);
            if (b[i] >> bit) & 1 == 1 {
                r = big_add_mod(&r, a, n);
            }
        }
    }
    r
}

fn big_modexp(base: &Big, exp: u32, n: &Big) -> Big {
    let mut result = [0u32; 32];
    result[0] = 1;
    let mut b = *base;
    let mut e = exp;
    while e != 0 {
        if e & 1 == 1 {
            result = big_mul_mod(&result, &b, n);
        }
        b = big_mul_mod(&b, &b, n);
        e >>= 1;
    }
    result
}

fn big_to_be_bytes(a: &Big) -> [u8; 128] {
    let mut out = [0u8; 128];
    for i in 0..128 {
        out[i] = (a[(127 - i) / 4] >> (8 * ((127 - i) % 4))) as u8;
    }
    out
}

/// 把 16 字节随机密钥（字节序反转后当大端整数）用固定公钥加密，
/// 输出 256 位小写十六进制 —— 即表单里的 `encSecKey`。
pub fn rsa_encrypt_sec_key(secret: &[u8]) -> String {
    let mut reversed = secret.to_vec();
    reversed.reverse();
    let m = big_from_be_bytes(&reversed);
    let n = big_from_hex_128(RSA_N_HEX);
    let c = big_modexp(&m, RSA_E, &n);
    hex_lower(&big_to_be_bytes(&c))
}

/* ------------------------------------------------------------ 载荷组装 -- */

/// 每次请求的 16 字符密钥。熵由调用方给（真机上是系统时间 + 计数器），
/// 这里只做"可复现的均匀映射"——顺带让测试能锁死结果。
pub fn secret_key_from_entropy(seed: u64) -> [u8; 16] {
    let mut x = seed ^ 0x9E37_79B9_7F4A_7C15;
    let mut out = [0u8; 16];
    for slot in out.iter_mut() {
        x ^= x >> 30;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 27;
        x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^= x >> 31;
        *slot = BASE62[(x % 62) as usize];
    }
    out
}

pub fn weapi_payload(json: &str, secret: &[u8]) -> Result<Payload, &'static str> {
    if secret.len() != 16 {
        return Err("weapi 的随机密钥必须是 16 字节");
    }
    let inner = base64_encode(&aes128_cbc_encrypt_pkcs7(
        WEAPI_PRESET_KEY,
        WEAPI_IV,
        json.as_bytes(),
    )?);
    let outer = base64_encode(&aes128_cbc_encrypt_pkcs7(
        secret,
        WEAPI_IV,
        inner.as_bytes(),
    )?);
    Ok(Payload {
        params: outer,
        enc_sec_key: Some(rsa_encrypt_sec_key(secret)),
    })
}

pub fn eapi_payload(path: &str, json: &str) -> Result<Payload, &'static str> {
    let message = format!("nobody{path}use{json}md5forever");
    let digest = hex_lower(&md5(message.as_bytes()));
    let plain = format!("{path}{EAPI_SEP}{json}{EAPI_SEP}{digest}");
    let enc = aes128_ecb_encrypt_pkcs7(EAPI_KEY, plain.as_bytes())?;
    Ok(Payload {
        params: hex_upper(&enc),
        enc_sec_key: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用十六进制解码（与实现无关，期望值仍然是字面量）。
    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn aes128_block_matches_fips197_vector() {
        let key: [u8; 16] = unhex("000102030405060708090a0b0c0d0e0f")
            .try_into()
            .unwrap();
        let plain: [u8; 16] = unhex("00112233445566778899aabbccddeeff")
            .try_into()
            .unwrap();
        assert_eq!(
            hex_lower(&aes128_encrypt_block(&key, &plain)),
            "69c4e0d86a7b0430d8cdb78070b4c55a"
        );
    }

    #[test]
    fn aes128_cbc_pkcs7_matches_nist_vector_with_full_padding_block() {
        let key = unhex("2b7e151628aed2a6abf7158809cf4f3c");
        let iv = unhex("000102030405060708090a0b0c0d0e0f");
        let plain =
            unhex("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51");
        let ct = aes128_cbc_encrypt_pkcs7(&key, &iv, &plain).unwrap();
        assert_eq!(ct.len(), 48); /* 32 字节明文 + 整块填充 = 3 块 */
        assert_eq!(
            hex_lower(&ct),
            "7649abac8119b246cee98e9b12e9197d\
             5086cb9b507219ee95db113a917678b2\
             55e21d7100b988ffec32feeafaf23538"
        );
    }

    #[test]
    fn md5_matches_rfc1321_vectors() {
        assert_eq!(hex_lower(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(
            hex_lower(&md5(b"abc")),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert_eq!(
            hex_lower(&md5(b"message digest")),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    const URL_V1_JSON: &str =
        r#"{"ids":"[3346495279]","level":"exhigh","encodeType":"aac","_q":"exhigh"}"#;

    #[test]
    fn weapi_payload_matches_reference_vector() {
        let p = weapi_payload(URL_V1_JSON, b"0123456789abcdef").unwrap();
        assert_eq!(
            p.params,
            "paW/6pUb3aLd8mrvP2LfgxH9F/1lJfgOkA2RSVzlp3FUGYOG3dA8GrrxAj+Ave25xc4+KprvGq5aihUrhC9viB\
             QcBArOc8hde0n1J8hafrfEh1UxOqXz0mjCQOxdA8dyAWZpM7m9t/75/+UuO72Scw=="
        );
        assert_eq!(
            p.enc_sec_key.as_deref(),
            Some(
                "35701388baf89fed412e11269b9c76625d095ecaf17f03fa018abe19ea2d38b949debf242ee39a71ca1f6cda71b1b86a\
                 45aa909ee27f7e78e267d34e732f0de948206c3340a788d0003372183e2f753c1f78b66ac23d134ac1fc9b993156520\
                 ea826b8aa89a962d4491b4b8d7e08738e1da9b07aa39bf4a7ef0b1c210728cd52"
            )
        );
    }

    #[test]
    fn eapi_payload_matches_reference_vector() {
        let p = eapi_payload("/api/song/enhance/player/url/v1", URL_V1_JSON).unwrap();
        assert_eq!(p.enc_sec_key, None);
        assert_eq!(
            p.params,
            "FA90B329E9614F79E79598F37DC2EDB487F00D1BC4C9B24CD57E6C318B9073569338432CD7D98D1A3626E997A2C531217E\
             849FBA6CDC6A3A33C9BCEF0C2B734B8D4EBDF181CDBCD4213E43962F96C98CDE7F7DC50670ED7C820E146A12F67B096CD4\
             EE31CAF807265FFD8615B03F5FC64D578E39731D086E8257A5E27A6FD3C0C9F182794930D6723B5D63ED17E0242E6F9A80\
             0AB47622E299D77DCE0B5EA2A41F655A1D85BF9E4C3DF91A18B551BF25"
        );
    }

    #[test]
    fn secret_key_is_16_base62_chars_and_depends_on_entropy() {
        let a = secret_key_from_entropy(0);
        let b = secret_key_from_entropy(1);
        assert_eq!(a.len(), 16);
        assert!(a.iter().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(a, b);
        assert_eq!(a, secret_key_from_entropy(0)); /* 同一个种子必须可复现 */
    }
}
