//! Rust-owned QR encoder for the NetEase login code.
//!
//! The login URL is prepared on a native worker and only the finished RGBA
//! pixels cross the QuickJS bridge. The bridge itself only transfers a
//! 128x128 texture into the core UI, so no QR matrix construction or pixel
//! painting runs in the guest frame.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

const TEX: usize = 128;
const QUIET: usize = 4;
const VERSION: usize = 4;
const MODULES: usize = VERSION * 4 + 17;
const DATA_CODEWORDS: usize = 80;
const ECC_CODEWORDS: usize = 20;
const MAX_PAYLOAD: usize = DATA_CODEWORDS - 2;

static GENERATION: AtomicU64 = AtomicU64::new(0);
static READY: Mutex<Option<Vec<u8>>> = Mutex::new(None);

/// Start native QR preparation on a worker, never on QuickJS's frame thread.
pub fn prepare(text: &str) {
    let generation = GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    if let Ok(mut ready) = READY.lock() {
        *ready = None;
    }
    let payload = String::from(text);
    let spawned = std::thread::Builder::new()
        .name("yunyin-qr".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            let result = encode_rgba(payload.as_bytes());
            if GENERATION.load(Ordering::Acquire) != generation {
                return;
            }
            match result {
                Ok(rgba) => {
                    if let Ok(mut ready) = READY.lock() {
                        if GENERATION.load(Ordering::Acquire) == generation {
                            *ready = Some(rgba);
                        }
                    }
                }
                Err(message) => crate::media::platform::log::append(&format!(
                    "qr: Rust 编码失败 {message}"
                )),
            }
        });
    if spawned.is_err() {
        crate::media::platform::log::append("qr: Rust 编码线程创建失败");
    }
}

/// Invalidate an in-flight QR worker and discard a result not yet uploaded.
pub fn reset() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    if let Ok(mut ready) = READY.lock() {
        *ready = None;
    }
}

/// Take the newest completed 128x128 RGBA image exactly once.
pub fn take_ready() -> Option<Vec<u8>> {
    READY.lock().ok().and_then(|mut ready| ready.take())
}

fn encode_rgba(payload: &[u8]) -> Result<Vec<u8>, &'static str> {
    if payload.is_empty() || payload.len() > MAX_PAYLOAD {
        return Err("登录 URL 超出版本 4-L 容量");
    }
    let matrix = encode_matrix(payload)?;
    let modules = MODULES + QUIET * 2;
    let scale = TEX / modules;
    if scale == 0 {
        return Err("二维码尺寸不足");
    }
    let drawn = modules * scale;
    let offset = (TEX - drawn) / 2;
    let mut rgba = vec![255u8; TEX * TEX * 4];
    for row in 0..MODULES {
        for col in 0..MODULES {
            if !matrix[row][col] {
                continue;
            }
            let x0 = offset + (col + QUIET) * scale;
            let y0 = offset + (row + QUIET) * scale;
            for y in y0..y0 + scale {
                for x in x0..x0 + scale {
                    let at = (y * TEX + x) * 4;
                    rgba[at] = 0;
                    rgba[at + 1] = 0;
                    rgba[at + 2] = 0;
                    rgba[at + 3] = 255;
                }
            }
        }
    }
    Ok(rgba)
}

fn encode_matrix(payload: &[u8]) -> Result<Vec<Vec<bool>>, &'static str> {
    let codewords = create_codewords(payload)?;
    let mut modules = vec![vec![None::<bool>; MODULES]; MODULES];

    finder(&mut modules, 0, 0);
    finder(&mut modules, MODULES - 7, 0);
    finder(&mut modules, 0, MODULES - 7);
    alignment(&mut modules);
    timing(&mut modules);
    format_info(&mut modules, false, 0);
    map_data(&mut modules, &codewords, 0);

    Ok(modules
        .into_iter()
        .map(|row| row.into_iter().map(|cell| cell.unwrap_or(false)).collect())
        .collect())
}

fn create_codewords(payload: &[u8]) -> Result<Vec<u8>, &'static str> {
    let mut bits = Vec::with_capacity(DATA_CODEWORDS * 8);
    put_bits(&mut bits, 0b0100, 4);
    put_bits(&mut bits, payload.len() as u32, 8);
    for &byte in payload {
        put_bits(&mut bits, byte as u32, 8);
    }
    if bits.len() > DATA_CODEWORDS * 8 {
        return Err("数据位超出 QR 容量");
    }
    if bits.len() + 4 <= DATA_CODEWORDS * 8 {
        put_bits(&mut bits, 0, 4);
    }
    while bits.len() % 8 != 0 {
        bits.push(false);
    }

    let mut data = bits_to_bytes(&bits);
    let mut pad = 0;
    while data.len() < DATA_CODEWORDS {
        data.push(if pad % 2 == 0 { 0xec } else { 0x11 });
        pad += 1;
    }

    let ecc = reed_solomon(&data, ECC_CODEWORDS);
    data.extend_from_slice(&ecc);
    Ok(data)
}

fn put_bits(bits: &mut Vec<bool>, value: u32, count: usize) {
    for shift in (0..count).rev() {
        bits.push(((value >> shift) & 1) != 0);
    }
}

fn bits_to_bytes(bits: &[bool]) -> Vec<u8> {
    bits.chunks(8)
        .map(|chunk| {
            let mut value = 0u8;
            for &bit in chunk {
                value = (value << 1) | u8::from(bit);
            }
            value << (8 - chunk.len())
        })
        .collect()
}

fn gf_tables() -> ([u8; 256], [u8; 256]) {
    let mut exp = [0u8; 256];
    let mut log = [0u8; 256];
    for i in 0..8 {
        exp[i] = 1u8 << i;
    }
    for i in 8..256 {
        exp[i] = exp[i - 4] ^ exp[i - 5] ^ exp[i - 6] ^ exp[i - 8];
    }
    for i in 0..255 {
        log[exp[i] as usize] = i as u8;
    }
    (exp, log)
}

fn gf_mul(a: u8, b: u8, exp: &[u8; 256], log: &[u8; 256]) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    exp[(log[a as usize] as usize + log[b as usize] as usize) % 255]
}

fn reed_solomon(data: &[u8], ecc_len: usize) -> Vec<u8> {
    let (exp, log) = gf_tables();
    let mut generator = vec![1u8];
    for i in 0..ecc_len {
        let root = exp[i];
        let mut next = vec![0u8; generator.len() + 1];
        for (j, &coefficient) in generator.iter().enumerate() {
            next[j] ^= coefficient;
            next[j + 1] ^= gf_mul(coefficient, root, &exp, &log);
        }
        generator = next;
    }

    let mut remainder = vec![0u8; ecc_len];
    for &byte in data {
        let factor = byte ^ remainder[0];
        for j in 0..ecc_len - 1 {
            remainder[j] = remainder[j + 1] ^ gf_mul(generator[j + 1], factor, &exp, &log);
        }
        remainder[ecc_len - 1] = gf_mul(generator[ecc_len], factor, &exp, &log);
    }
    remainder
}

fn finder(modules: &mut [Vec<Option<bool>>], row: usize, col: usize) {
    for r in -1i32..=7 {
        let y = row as i32 + r;
        if y < 0 || y >= MODULES as i32 {
            continue;
        }
        for c in -1i32..=7 {
            let x = col as i32 + c;
            if x < 0 || x >= MODULES as i32 {
                continue;
            }
            modules[y as usize][x as usize] = Some(
                ((0..=6).contains(&r) && (c == 0 || c == 6))
                    || ((0..=6).contains(&c) && (r == 0 || r == 6))
                    || ((2..=4).contains(&r) && (2..=4).contains(&c)),
            );
        }
    }
}

fn alignment(modules: &mut [Vec<Option<bool>>]) {
    for &row in &[6usize, 26] {
        for &col in &[6usize, 26] {
            if modules[row][col].is_some() {
                continue;
            }
            for r in -2i32..=2 {
                for c in -2i32..=2 {
                    modules[(row as i32 + r) as usize][(col as i32 + c) as usize] = Some(
                        r == -2 || r == 2 || c == -2 || c == 2 || (r == 0 && c == 0),
                    );
                }
            }
        }
    }
}

fn timing(modules: &mut [Vec<Option<bool>>]) {
    for r in 8..MODULES - 8 {
        if modules[r][6].is_none() {
            modules[r][6] = Some(r % 2 == 0);
        }
    }
    for c in 8..MODULES - 8 {
        if modules[6][c].is_none() {
            modules[6][c] = Some(c % 2 == 0);
        }
    }
}

fn bch_type_info(data: u32) -> u32 {
    const G15: u32 = (1 << 10) | (1 << 8) | (1 << 5) | (1 << 4) | (1 << 2) | (1 << 1) | 1;
    const MASK: u32 = (1 << 14) | (1 << 12) | (1 << 10) | (1 << 4) | (1 << 1);
    let mut value = data << 10;
    while bit_length(value) >= bit_length(G15) {
        value ^= G15 << (bit_length(value) - bit_length(G15));
    }
    ((data << 10) | value) ^ MASK
}

fn bit_length(mut value: u32) -> u32 {
    let mut length = 0;
    while value != 0 {
        length += 1;
        value >>= 1;
    }
    length
}

fn format_info(modules: &mut [Vec<Option<bool>>], test: bool, mask: u32) {
    let bits = bch_type_info(8 | mask);
    for i in 0..15 {
        let value = !test && ((bits >> i) & 1) != 0;
        let row = if i < 6 {
            i
        } else if i < 8 {
            i + 1
        } else {
            MODULES - 15 + i
        };
        modules[row][8] = Some(value);

        let col = if i < 8 {
            MODULES - i - 1
        } else if i < 9 {
            15 - i
        } else {
            15 - i - 1
        };
        modules[8][col] = Some(value);
    }
    modules[MODULES - 8][8] = Some(!test);
}

fn map_data(modules: &mut [Vec<Option<bool>>], data: &[u8], mask: u32) {
    let mut inc: i32 = -1;
    let mut row: i32 = MODULES as i32 - 1;
    let mut bit_index: i32 = 7;
    let mut byte_index = 0usize;
    let mask_on = |r: i32, c: i32| -> bool {
        match mask {
            0 => (r + c) % 2 == 0,
            1 => r % 2 == 0,
            2 => c % 3 == 0,
            3 => (r + c) % 3 == 0,
            4 => (r / 2 + c / 3) % 2 == 0,
            5 => (r * c) % 2 + (r * c) % 3 == 0,
            6 => ((r * c) % 2 + (r * c) % 3) % 2 == 0,
            _ => ((r * c) % 3 + (r + c) % 2) % 2 == 0,
        }
    };

    let mut col = MODULES as i32 - 1;
    while col > 0 {
        if col == 6 {
            col -= 1;
        }
        loop {
            for c in 0..2 {
                let x = col - c;
                if modules[row as usize][x as usize].is_none() {
                    let mut dark = if byte_index < data.len() {
                        ((data[byte_index] >> bit_index) & 1) != 0
                    } else {
                        false
                    };
                    if mask_on(row, x) {
                        dark = !dark;
                    }
                    modules[row as usize][x as usize] = Some(dark);
                    bit_index -= 1;
                    if bit_index < 0 {
                        byte_index += 1;
                        bit_index = 7;
                    }
                }
            }
            row += inc;
            if row < 0 || row >= MODULES as i32 {
                row -= inc;
                inc = -inc;
                break;
            }
        }
        col -= 2;
    }
}
