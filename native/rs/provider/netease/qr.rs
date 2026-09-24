//! 字节模式二维码（版本 1–6，纠错 L）。
//!
//! 只服务于扫码登录那一条 URL。不引入新的 crate：PocketJS 的依赖表里没有二维码库。
//! 矩阵画法对照 Nayuki 的放置顺序；主机上用解码器扫过生成的图，扫不出来就不算完成。
#![allow(dead_code)]

use alloc::vec::Vec;

const QUIET: usize = 4;
const OUT: usize = 256;

pub fn rgba(data: &[u8]) -> Option<Vec<u8>> {
    let modules = matrix(data)?;
    let n = modules.len();
    let side = side_of(n);
    if side * side != n {
        return None;
    }
    let span = side + QUIET * 2;
    let scale = OUT / span;
    if scale == 0 {
        return None;
    }
    let drawn = span * scale;
    let origin = (OUT - drawn) / 2;
    let mut px = vec![255u8; OUT * OUT * 4];
    for y in 0..OUT {
        for x in 0..OUT {
            px[(y * OUT + x) * 4 + 3] = 255;
        }
    }
    for my in 0..span {
        for mx in 0..span {
            let dark = if my >= QUIET && mx >= QUIET && my < QUIET + side && mx < QUIET + side {
                modules[(my - QUIET) * side + (mx - QUIET)]
            } else {
                false
            };
            if !dark {
                continue;
            }
            let y0 = origin + my * scale;
            let x0 = origin + mx * scale;
            for y in y0..y0 + scale {
                for x in x0..x0 + scale {
                    let i = (y * OUT + x) * 4;
                    px[i] = 0;
                    px[i + 1] = 0;
                    px[i + 2] = 0;
                }
            }
        }
    }
    Some(px)
}

fn side_of(n: usize) -> usize {
    let mut s = 0;
    while s * s < n {
        s += 1;
    }
    s
}

fn matrix(data: &[u8]) -> Option<Vec<bool>> {
    let ver = pick_version(data.len())?;
    let codewords = encode(data, ver)?;
    let size = ver.size;
    let mut modules = vec![false; size * size];
    let mut func = vec![false; size * size];
    draw_function(&mut modules, &mut func, ver);
    draw_codewords(&mut modules, &func, &codewords, size);
    let mut best: Option<(i32, Vec<bool>)> = None;
    for mask in 0..8 {
        let mut m = modules.clone();
        apply_mask(&mut m, &func, mask, size);
        draw_format(&mut m, &mut func, size, mask);
        let score = penalty(&m, size);
        if best.as_ref().map(|(s, _)| score < *s).unwrap_or(true) {
            best = Some((score, m));
        }
    }
    best.map(|(_, m)| m)
}

#[derive(Clone, Copy)]
struct Ver {
    size: usize,
    data_cw: usize,
    ecc: usize,
    blocks: usize,
    align: usize,
}

fn version_of(v: usize) -> Option<Ver> {
    /* ECC-L。v1–v6 每组里的块一样长，所以只要记块数。 */
    let (data_cw, ecc, blocks, align) = match v {
        1 => (19, 7, 1, 0),
        2 => (34, 10, 1, 18),
        3 => (55, 15, 1, 22),
        4 => (80, 20, 1, 26),
        5 => (108, 26, 1, 30),
        6 => (136, 18, 2, 34),
        _ => return None,
    };
    Some(Ver {
        size: 17 + 4 * v,
        data_cw,
        ecc,
        blocks,
        align,
    })
}

fn pick_version(len: usize) -> Option<Ver> {
    for v in 1..=6 {
        let ver = version_of(v)?;
        let bits = 4 + 8 + len * 8;
        if bits <= ver.data_cw * 8 {
            return Some(ver);
        }
    }
    None
}

fn encode(data: &[u8], ver: Ver) -> Option<Vec<u8>> {
    let mut bits: Vec<bool> = Vec::new();
    push_bits(&mut bits, 0b0100, 4);
    push_bits(&mut bits, data.len() as u32, 8);
    for &b in data {
        push_bits(&mut bits, b as u32, 8);
    }
    let cap = ver.data_cw * 8;
    if bits.len() > cap {
        return None;
    }
    let term = (cap - bits.len()).min(4);
    push_bits(&mut bits, 0, term as u8);
    while bits.len() % 8 != 0 {
        bits.push(false);
    }
    let mut bytes = Vec::new();
    for chunk in bits.chunks(8) {
        let mut v = 0u8;
        for (i, bit) in chunk.iter().enumerate() {
            if *bit {
                v |= 1 << (7 - i);
            }
        }
        bytes.push(v);
    }
    let mut toggle = false;
    while bytes.len() < ver.data_cw {
        toggle = !toggle;
        bytes.push(if toggle { 0xEC } else { 0x11 });
    }
    let per = ver.data_cw / ver.blocks;
    let mut blocks = Vec::new();
    let mut eccs = Vec::new();
    for i in 0..ver.blocks {
        let part = bytes[i * per..(i + 1) * per].to_vec();
        eccs.push(rs(&part, ver.ecc));
        blocks.push(part);
    }
    let mut out = Vec::with_capacity(ver.data_cw + ver.ecc * ver.blocks);
    for i in 0..per {
        for b in &blocks {
            out.push(b[i]);
        }
    }
    for i in 0..ver.ecc {
        for e in &eccs {
            out.push(e[i]);
        }
    }
    Some(out)
}

fn draw_function(modules: &mut [bool], func: &mut [bool], ver: Ver) {
    let size = ver.size;
    draw_finder(modules, func, 0, 0, size);
    draw_finder(modules, func, size - 7, 0, size);
    draw_finder(modules, func, 0, size - 7, size);
    for i in 8..size - 8 {
        set(modules, func, 6, i, i % 2 == 0, size);
        set(modules, func, i, 6, i % 2 == 0, size);
    }
    if ver.align > 0 {
        draw_align(modules, func, ver.align, ver.align, size);
    }
    set(modules, func, 8, size - 8, true, size);
    set(modules, func, size - 8, 8, true, size);
    reserve_format(func, size);
}

fn draw_finder(modules: &mut [bool], func: &mut [bool], x: usize, y: usize, size: usize) {
    for dy in -1i32..=7 {
        for dx in -1i32..=7 {
            let r = y as i32 + dy;
            let c = x as i32 + dx;
            if r < 0 || c < 0 || r >= size as i32 || c >= size as i32 {
                continue;
            }
            let dark = (0..=6).contains(&dx)
                && (0..=6).contains(&dy)
                && (dx == 0
                    || dx == 6
                    || dy == 0
                    || dy == 6
                    || ((2..=4).contains(&dx) && (2..=4).contains(&dy)));
            set(modules, func, c as usize, r as usize, dark, size);
        }
    }
}

fn draw_align(modules: &mut [bool], func: &mut [bool], cx: usize, cy: usize, size: usize) {
    for dy in -2i32..=2 {
        for dx in -2i32..=2 {
            let r = cy as i32 + dy;
            let c = cx as i32 + dx;
            if r < 0 || c < 0 || r >= size as i32 || c >= size as i32 {
                return;
            }
            if func[r as usize * size + c as usize] {
                return;
            }
        }
    }
    for dy in -2i32..=2 {
        for dx in -2i32..=2 {
            let dark = dx.abs().max(dy.abs()) != 1;
            set(
                modules,
                func,
                (cx as i32 + dx) as usize,
                (cy as i32 + dy) as usize,
                dark,
                size,
            );
        }
    }
}

fn reserve_format(func: &mut [bool], size: usize) {
    for i in 0..9 {
        if i != 6 {
            mark(func, 8, i, size);
            mark(func, i, 8, size);
        }
    }
    for i in 0..8 {
        mark(func, size - 1 - i, 8, size);
        mark(func, 8, size - 1 - i, size);
    }
}

fn draw_codewords(modules: &mut [bool], func: &[bool], data: &[u8], size: usize) {
    let mut bits = Vec::with_capacity(data.len() * 8);
    for &b in data {
        push_bits(&mut bits, b as u32, 8);
    }
    let mut i = 0;
    let mut col = size as i32 - 1;
    let mut upward = true;
    while col > 0 {
        if col == 6 {
            col -= 1;
        }
        for row_i in 0..size {
            let row = if upward { size - 1 - row_i } else { row_i };
            for k in 0..2 {
                let c = (col - k) as usize;
                let idx = row * size + c;
                if func[idx] {
                    continue;
                }
                modules[idx] = if i < bits.len() { bits[i] } else { false };
                i += 1;
            }
        }
        upward = !upward;
        col -= 2;
    }
}

fn apply_mask(modules: &mut [bool], func: &[bool], mask: u8, size: usize) {
    for r in 0..size {
        for c in 0..size {
            let idx = r * size + c;
            if func[idx] {
                continue;
            }
            if mask_bit(mask, r, c) {
                modules[idx] = !modules[idx];
            }
        }
    }
}

fn mask_bit(mask: u8, r: usize, c: usize) -> bool {
    let r = r as i32;
    let c = c as i32;
    match mask {
        0 => (r + c) % 2 == 0,
        1 => r % 2 == 0,
        2 => c % 3 == 0,
        3 => (r + c) % 3 == 0,
        4 => (r / 2 + c / 3) % 2 == 0,
        5 => (r * c) % 2 + (r * c) % 3 == 0,
        6 => ((r * c) % 2 + (r * c) % 3) % 2 == 0,
        _ => ((r + c) % 2 + (r * c) % 3) % 2 == 0,
    }
}

fn draw_format(modules: &mut [bool], func: &mut [bool], size: usize, mask: u8) {
    let bits = format_bits(mask);
    let bit = |i: u8| ((bits >> i) & 1) == 1;
    for i in 0..=5 {
        set(modules, func, 8, i, bit(i as u8), size);
    }
    set(modules, func, 8, 7, bit(6), size);
    set(modules, func, 8, 8, bit(7), size);
    set(modules, func, 7, 8, bit(8), size);
    for i in 9..15 {
        set(modules, func, 14 - i, 8, bit(i as u8), size);
    }
    for i in 0..8 {
        set(modules, func, size - 1 - i, 8, bit(i as u8), size);
    }
    for i in 8..15 {
        set(modules, func, 8, size - 15 + i, bit(i as u8), size);
    }
    set(modules, func, 8, size - 8, true, size);
}

fn format_bits(mask: u8) -> u16 {
    let data = (1u16 << 3) | (mask as u16);
    let mut rem = data;
    for _ in 0..10 {
        rem = (rem << 1) ^ (((rem >> 9) & 1) * 0x537);
    }
    ((data << 10) | (rem & 0x3FF)) ^ 0x5412
}

fn penalty(modules: &[bool], size: usize) -> i32 {
    let mut score = 0i32;
    let at = |r: usize, c: usize| modules[r * size + c];
    for r in 0..size {
        let mut run = 1;
        for c in 1..size {
            if at(r, c) == at(r, c - 1) {
                run += 1;
                if run == 5 {
                    score += 3;
                } else if run > 5 {
                    score += 1;
                }
            } else {
                run = 1;
            }
        }
    }
    for c in 0..size {
        let mut run = 1;
        for r in 1..size {
            if at(r, c) == at(r - 1, c) {
                run += 1;
                if run == 5 {
                    score += 3;
                } else if run > 5 {
                    score += 1;
                }
            } else {
                run = 1;
            }
        }
    }
    score
}

fn set(modules: &mut [bool], func: &mut [bool], x: usize, y: usize, dark: bool, size: usize) {
    if x >= size || y >= size {
        return;
    }
    let idx = y * size + x;
    modules[idx] = dark;
    func[idx] = true;
}

fn mark(func: &mut [bool], x: usize, y: usize, size: usize) {
    if x < size && y < size {
        func[y * size + x] = true;
    }
}

fn push_bits(out: &mut Vec<bool>, val: u32, n: u8) {
    for i in (0..n).rev() {
        out.push(((val >> i) & 1) == 1);
    }
}

fn rs(data: &[u8], nsym: usize) -> Vec<u8> {
    let gen = rs_generator(nsym);
    let mut res = vec![0u8; data.len() + nsym];
    res[..data.len()].copy_from_slice(data);
    for i in 0..data.len() {
        let coef = res[i];
        if coef == 0 {
            continue;
        }
        for j in 0..gen.len() {
            res[i + j] ^= gf_mul(gen[j], coef);
        }
    }
    res[data.len()..].to_vec()
}

fn rs_generator(nsym: usize) -> Vec<u8> {
    let mut g = vec![1u8];
    for i in 0..nsym {
        g = poly_mul(&g, &[1, gf_pow(2, i as i32)]);
    }
    g
}

fn poly_mul(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; a.len() + b.len() - 1];
    for (i, &av) in a.iter().enumerate() {
        for (j, &bv) in b.iter().enumerate() {
            out[i + j] ^= gf_mul(av, bv);
        }
    }
    out
}

fn gf_pow(x: u8, exp: i32) -> u8 {
    let mut v = 1u8;
    for _ in 0..exp {
        v = gf_mul(v, x);
    }
    v
}

fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    for _ in 0..8 {
        if b & 1 != 0 {
            p ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1D;
        }
        b >>= 1;
    }
    p
}
