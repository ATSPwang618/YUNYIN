//! 标签与内嵌封面（从旧的 media.rs 里拆出来的）。
//! 播放逻辑不在这里。

use crate::media::{COVER_PX, MAX_ART, PREFIX_CAP};
use pocketjs_core::spec::psm;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Mutex;

extern "C" {
    fn yunyin_image_decode(
        data: *const u8,
        len: i32,
        rgba: *mut *mut u8,
        w: *mut i32,
        h: *mut i32,
    ) -> i32;
    fn yunyin_image_free(p: *mut u8);
    fn yunyin_image_resize(
        src: *const u8,
        sw: i32,
        sh: i32,
        dst: *mut u8,
        dw: i32,
        dh: i32,
    ) -> i32;
}

static COVER_HANDLES: Mutex<Option<HashMap<String, i32>>> = Mutex::new(None);

fn synchsafe(b: &[u8]) -> usize {
    if b.len() < 4 {
        return 0;
    }
    (((b[0] as usize) & 0x7f) << 21)
        | (((b[1] as usize) & 0x7f) << 14)
        | (((b[2] as usize) & 0x7f) << 7)
        | ((b[3] as usize) & 0x7f)
}

fn be32(b: &[u8]) -> usize {
    if b.len() < 4 {
        return 0;
    }
    ((b[0] as usize) << 24) | ((b[1] as usize) << 16) | ((b[2] as usize) << 8) | (b[3] as usize)
}

fn le32(b: &[u8]) -> usize {
    if b.len() < 4 {
        return 0;
    }
    (b[0] as usize) | ((b[1] as usize) << 8) | ((b[2] as usize) << 16) | ((b[3] as usize) << 24)
}

fn be64(b: &[u8]) -> usize {
    if b.len() < 8 {
        return 0;
    }
    ((be32(&b[0..4]) as u64) << 32 | be32(&b[4..8]) as u64) as usize
}

fn cstr_skip(p: &[u8]) -> Option<usize> {
    p.iter().position(|&c| c == 0).map(|i| i + 1)
}

/// Vitawave metadata_get_album_art_texture: JPEG 0xFFD8 / PNG 89 50 4E 47.
fn image_payload(p: &[u8]) -> Option<Vec<u8>> {
    let mut i = 0usize;
    while i + 3 < p.len() {
        if p[i] == 0xff && p[i + 1] == 0xd8 {
            let rest = &p[i..];
            if !rest.is_empty() && rest.len() <= MAX_ART {
                return Some(rest.to_vec());
            }
        }
        if i + 7 < p.len() && p[i] == 0x89 && p[i + 1] == b'P' && p[i + 2] == b'N' && p[i + 3] == b'G' {
            let rest = &p[i..];
            if !rest.is_empty() && rest.len() <= MAX_ART {
                return Some(rest.to_vec());
            }
        }
        i += 1;
    }
    None
}

fn walk_id3_frames(bytes: &[u8], mut on_frame: impl FnMut(&[u8], &[u8])) {
    if bytes.len() < 10 || &bytes[0..3] != b"ID3" {
        return;
    }
    let ver = bytes[3];
    let tag_size = synchsafe(&bytes[6..10]);
    let mut pos = 10usize;
    let end = (10 + tag_size).min(bytes.len());
    if bytes[5] & 0x40 != 0 && pos + 4 <= end {
        let ext = if ver >= 4 {
            synchsafe(&bytes[pos..pos + 4])
        } else {
            be32(&bytes[pos..pos + 4])
        };
        pos = (pos + ext.max(4)).min(end);
    }
    while pos + 10 <= end {
        if bytes[pos] == 0 {
            break;
        }
        let id = &bytes[pos..pos + 4];
        let fsize = if ver >= 4 {
            synchsafe(&bytes[pos + 4..pos + 8])
        } else {
            be32(&bytes[pos + 4..pos + 8])
        };
        pos += 10;
        if fsize == 0 || pos + fsize > end {
            break;
        }
        on_frame(id, &bytes[pos..pos + fsize]);
        pos += fsize;
    }
}

fn extract_id3_apic(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut found = None;
    walk_id3_frames(bytes, |id, data| {
        if found.is_some() || id != b"APIC" || data.len() < 4 {
            return;
        }
        let enc = data[0];
        let utf16 = enc == 1 || enc == 2;
        let mut p = &data[1..];
        let Some(n) = cstr_skip(p) else {
            return;
        };
        p = &p[n..];
        if p.is_empty() {
            return;
        }
        p = &p[1..];
        if utf16 {
            let mut i = 0;
            while i + 1 < p.len() {
                if p[i] == 0 && p[i + 1] == 0 {
                    p = &p[i + 2..];
                    break;
                }
                i += 2;
            }
            if i + 1 >= p.len() && !(p.len() >= 2 && p[0] == 0 && p[1] == 0) {
                return;
            }
        } else {
            let Some(n) = cstr_skip(p) else {
                return;
            };
            p = &p[n..];
        }
        if p.is_empty() || p.len() > MAX_ART {
            return;
        }
        if let Some(img) = image_payload(p) {
            found = Some(img);
        } else if let Some(img) = image_payload(data) {
            found = Some(img);
        }
    });
    found
}

fn extract_id3_text(bytes: &[u8], want: &[u8; 4]) -> String {
    let mut out = String::new();
    walk_id3_frames(bytes, |id, data| {
        if !out.is_empty() || id != want || data.is_empty() {
            return;
        }
        let enc = data[0];
        let txt = &data[1..];
        if enc == 3 || enc == 0 {
            out = String::from_utf8_lossy(txt)
                .trim_matches('\0')
                .trim()
                .to_string();
        } else if txt.len() >= 2 {
            let mut i = 0;
            if txt[0] == 0xff && txt[1] == 0xfe {
                i = 2;
            } else if txt[0] == 0xfe && txt[1] == 0xff {
                i = 2;
            }
            let mut u = String::new();
            while i + 1 < txt.len() {
                let cp = u16::from_le_bytes([txt[i], txt[i + 1]]);
                i += 2;
                if cp == 0 {
                    break;
                }
                if let Some(c) = char::from_u32(cp as u32) {
                    u.push(c);
                }
            }
            out = u.trim().to_string();
        }
    });
    out
}

/// 按 ID3v2 编码字节（data[0]）解码从 `start` 开始的字符串，遇到 `\0` 结束。
fn decode_id3_from(data: &[u8], enc: u8, start: usize) -> String {
    let txt = if start < data.len() { &data[start..] } else { &[] };
    if txt.is_empty() {
        return String::new();
    }
    if enc == 3 || enc == 0 {
        return String::from_utf8_lossy(txt)
            .trim_matches('\0')
            .trim()
            .to_string();
    }
    let mut little = true;
    let mut i = 0usize;
    if txt.len() >= 2 {
        if txt[0] == 0xff && txt[1] == 0xfe {
            i = 2;
        } else if txt[0] == 0xfe && txt[1] == 0xff {
            little = false;
            i = 2;
        }
    }
    let mut u = String::new();
    while i + 1 < txt.len() {
        let cp = if little {
            u16::from_le_bytes([txt[i], txt[i + 1]])
        } else {
            u16::from_be_bytes([txt[i], txt[i + 1]])
        };
        i += 2;
        if cp == 0 {
            break;
        }
        if let Some(c) = char::from_u32(cp as u32) {
            u.push(c);
        }
    }
    u.trim().to_string()
}

/// 返回 ID3v2 字符串的结束下标（含终止符）：enc 1/2 为 UTF-16 双字节 `00 00`。
fn id3_cstr_end(data: &[u8], enc: u8, start: usize) -> usize {
    if enc == 1 || enc == 2 {
        let mut i = start;
        while i + 1 < data.len() {
            if data[i] == 0 && data[i + 1] == 0 {
                return i + 2;
            }
            i += 2;
        }
        data.len()
    } else {
        cstr_skip(&data[start..])
            .map(|n| start + n)
            .unwrap_or(data.len())
    }
}

/// 读取 TXXX（自定义文本帧）里描述为 wanted 的取值，如 lyrics-eng / lyrics-XXX。
fn extract_txxx(bytes: &[u8], wanted: &[&str]) -> String {
    let mut out = String::new();
    walk_id3_frames(bytes, |id, data| {
        if !out.is_empty() || id != b"TXXX" || data.len() < 2 {
            return;
        }
        let enc = data[0];
        let desc_start = 1;
        let desc_end = id3_cstr_end(data, enc, desc_start);
        if desc_end > data.len() {
            return;
        }
        let desc = decode_id3_from(data, enc, desc_start).to_lowercase();
        if !wanted.iter().any(|w| desc == w.to_lowercase()) {
            return;
        }
        out = decode_id3_from(data, enc, desc_end);
    });
    out
}

/// 读取标准 USLT（非同步歌词）帧：encoding + 3 字节语言 + 描述符 + 正文。
fn extract_uslt(bytes: &[u8]) -> String {
    let mut out = String::new();
    walk_id3_frames(bytes, |id, data| {
        if !out.is_empty() || id != b"USLT" || data.len() < 5 {
            return;
        }
        let enc = data[0];
        let desc_start = 4;
        let desc_end = id3_cstr_end(data, enc, desc_start);
        if desc_end > data.len() {
            return;
        }
        out = decode_id3_from(data, enc, desc_end);
    });
    out
}

/// 尽力从 MP3/OGG 里取歌词文本。优先级：USLT → TXXX(lyrics-eng/XXX/LYRICS) → OGG LYRICS。
fn extract_embedded_lyrics(bytes: &[u8]) -> String {
    let uslt = extract_uslt(bytes);
    if !uslt.trim().is_empty() {
        return uslt;
    }
    let txxx = extract_txxx(
        bytes,
        &[
            "lyrics-eng",   // 主流中文/英文 MP3（如网易云/百度）自定义帧
            "lyrics-xxx",   // 同上，大小写不敏感
            "lyrics",
            "lyrics3",
            "unsync",
            "unsyncedlyrics",
            "eng",
            "xxx",
        ],
    );
    if !txxx.trim().is_empty() {
        return txxx;
    }
    if bytes.len() >= 4 && &bytes[0..4] == b"OggS" {
        return extract_ogg_text(bytes, "LYRICS");
    }
    String::new()
}

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut vals = [255u8; 256];
    for (i, &c) in B64.iter().enumerate() {
        vals[c as usize] = i as u8;
    }
    vals[b'=' as usize] = 0;
    let clean: Vec<u8> = s
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    if clean.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(clean.len() / 4 * 3);
    let mut i = 0;
    while i + 3 < clean.len() {
        let a = vals[clean[i] as usize];
        let b = vals[clean[i + 1] as usize];
        let c = vals[clean[i + 2] as usize];
        let d = vals[clean[i + 3] as usize];
        if a == 255 || b == 255 || c == 255 || d == 255 {
            return None;
        }
        out.push((a << 2) | (b >> 4));
        if clean[i + 2] != b'=' {
            out.push((b << 4) | (c >> 2));
        }
        if clean[i + 3] != b'=' {
            out.push((c << 6) | d);
        }
        i += 4;
    }
    Some(out)
}

fn flac_picture_data(block: &[u8]) -> Option<Vec<u8>> {
    if block.len() < 32 {
        return None;
    }
    let mut i = 4usize;
    let mime_len = be32(&block[i..]);
    i += 4 + mime_len;
    if i + 4 > block.len() {
        return None;
    }
    let desc_len = be32(&block[i..]);
    i += 4 + desc_len + 16;
    if i + 4 > block.len() {
        return None;
    }
    let data_len = be32(&block[i..]);
    i += 4;
    if data_len == 0 || data_len > MAX_ART || i + data_len > block.len() {
        return None;
    }
    Some(block[i..i + data_len].to_vec())
}

fn extract_ogg_picture(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut i = 0usize;
    let mut packet: Vec<u8> = Vec::new();
    while i + 27 <= bytes.len() {
        if &bytes[i..i + 4] != b"OggS" {
            i += 1;
            continue;
        }
        let nsegs = bytes[i + 26] as usize;
        let hdr = i + 27;
        if hdr + nsegs > bytes.len() {
            break;
        }
        let mut payload = 0usize;
        let mut last = 0u8;
        for k in 0..nsegs {
            last = bytes[hdr + k];
            payload += last as usize;
        }
        let start = hdr + nsegs;
        let end = start + payload;
        if end > bytes.len() {
            break;
        }
        packet.extend_from_slice(&bytes[start..end]);
        i = end;
        if last == 255 {
            continue;
        }
        if packet.len() >= 7 && packet[0] == 3 && &packet[1..7] == b"vorbis" {
            let body = &packet[7..];
            if body.len() < 8 {
                break;
            }
            let vendor = le32(body);
            let mut p = 4 + vendor;
            if p + 4 > body.len() {
                break;
            }
            let count = le32(&body[p..]);
            p += 4;
            for _ in 0..count {
                if p + 4 > body.len() {
                    break;
                }
                let n = le32(&body[p..]);
                p += 4;
                if p + n > body.len() {
                    break;
                }
                let comment = core::str::from_utf8(&body[p..p + n]).unwrap_or("");
                p += n;
                if let Some((_, rest)) = comment.split_once('=') {
                    /* 这里用字节比较，而不是 `comment[..24]`：后者在第 24 个字节
                     * 正好落在多字节字符中间时会 panic，而这对中文/日文标签
                     * （"TITLE=宇多田…"）是常态。那里的 panic 会一路展开进
                     * JS 引擎的 C 栈帧，直接把整个应用带崩。 */
                    if comment.as_bytes().len() >= 24
                        && comment.as_bytes()[..24]
                            .eq_ignore_ascii_case(b"METADATA_BLOCK_PICTURE=")
                    {
                        if let Some(block) = b64_decode(rest) {
                            if let Some(pic) = flac_picture_data(&block) {
                                return image_payload(&pic).or(Some(pic));
                            }
                        }
                    }
                }
            }
            break;
        }
        packet.clear();
    }
    None
}

fn extract_flac_picture(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 8 || &bytes[0..4] != b"fLaC" {
        return None;
    }
    let mut i = 4usize;
    loop {
        if i + 4 > bytes.len() {
            break;
        }
        let last = bytes[i] & 0x80 != 0;
        let typ = bytes[i] & 0x7f;
        let size = ((bytes[i + 1] as usize) << 16) | ((bytes[i + 2] as usize) << 8) | (bytes[i + 3] as usize);
        i += 4;
        if i + size > bytes.len() {
            break;
        }
        if typ == 6 {
            if let Some(pic) = flac_picture_data(&bytes[i..i + size]) {
                return image_payload(&pic).or(Some(pic));
            }
        }
        i += size;
        if last {
            break;
        }
    }
    None
}

fn extract_ogg_text(bytes: &[u8], key: &str) -> String {
    let mut i = 0usize;
    let mut packet: Vec<u8> = Vec::new();
    let mut out = String::new();
    while i + 27 <= bytes.len() {
        if &bytes[i..i + 4] != b"OggS" {
            i += 1;
            continue;
        }
        let nsegs = bytes[i + 26] as usize;
        let hdr = i + 27;
        if hdr + nsegs > bytes.len() {
            break;
        }
        let mut payload = 0usize;
        let mut last = 0u8;
        for k in 0..nsegs {
            last = bytes[hdr + k];
            payload += last as usize;
        }
        let start = hdr + nsegs;
        let end = start + payload;
        if end > bytes.len() {
            break;
        }
        packet.extend_from_slice(&bytes[start..end]);
        i = end;
        if last == 255 {
            continue;
        }
        if packet.len() >= 7 && packet[0] == 3 && &packet[1..7] == b"vorbis" {
            let body = &packet[7..];
            if body.len() < 8 {
                break;
            }
            let vendor = le32(body);
            let mut p = 4 + vendor;
            if p + 4 > body.len() {
                break;
            }
            let count = le32(&body[p..]);
            p += 4;
            for _ in 0..count {
                if p + 4 > body.len() {
                    break;
                }
                let n = le32(&body[p..]);
                p += 4;
                if p + n > body.len() {
                    break;
                }
                let comment = core::str::from_utf8(&body[p..p + n]).unwrap_or("");
                p += n;
                if let Some((k, rest)) = comment.split_once('=') {
                    if out.is_empty() && k.eq_ignore_ascii_case(key) {
                        out = rest.trim().to_string();
                    }
                }
            }
            break;
        }
        packet.clear();
    }
    out
}

fn extract_cover_bytes(bytes: &[u8]) -> Option<Vec<u8>> {
    extract_id3_apic(bytes)
        .or_else(|| extract_ogg_picture(bytes))
        .or_else(|| extract_flac_picture(bytes))
        .and_then(|raw| image_payload(&raw).or(Some(raw)))
}

/* ------------------------------------------------------------------ M4A ---- */
/*
 * M4A 是容器：元数据在 moov/udta/meta/ilst 里，音频是 mdat 里的 AAC。
 * 它和别的格式有两个不同点：
 *
 *   - moov 可能在文件**开头**（网易云那种"fast start"），也可能在文件最末尾，
 *     所以标签不能从固定的前缀里读 —— 必须带 seek 走一遍盒子链。
 *   - 文字和封面是**带类型**的 `data` 原子（1 = UTF-8），不是键值对。
 */

/// 带大封面的 moov 可能有几 MB；这里给读取设个上限。
const MP4_MOOV_CAP: usize = 8 * 1024 * 1024;

fn is_mp4(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[4..8] == b"ftyp"
}

/// 走一遍顶层盒子链，返回整个 `moov` 盒子（含头部）；没有、或者大得不合理时返回空。
///
/// **只对 `moov` 限长**：`mdat` 本来就可能比任何上限都大，
/// 当初就是因为把上限也套在 mdat 上，才导致"moov 在文件尾"的文件读不到标签。
fn mp4_moov(path: &str) -> Vec<u8> {
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(len) = f.metadata().map(|m| m.len()) else {
        return Vec::new();
    };
    let mut off: u64 = 0;
    while off + 8 <= len {
        let mut hdr = [0u8; 16];
        if f.seek(SeekFrom::Start(off)).is_err() {
            return Vec::new();
        }
        let Ok(n) = f.read(&mut hdr) else {
            return Vec::new();
        };
        if n < 8 {
            return Vec::new();
        }
        let size32 = be32(&hdr[0..4]) as u64;
        let (hdr_len, total) = if size32 == 1 {
            if n < 16 {
                return Vec::new();
            }
            (16u64, be64(&hdr[8..16]) as u64)
        } else if size32 == 0 {
            (8u64, len - off)
        } else {
            (8u64, size32)
        };
        if total < hdr_len || off + total > len {
            return Vec::new();
        }
        if &hdr[4..8] == b"moov" {
            if total > MP4_MOOV_CAP as u64 {
                return Vec::new();
            }
            let mut buf = vec![0u8; total as usize];
            if f.seek(SeekFrom::Start(off)).is_err() {
                return Vec::new();
            }
            let got = f.read(&mut buf).unwrap_or(0);
            buf.truncate(got);
            return buf;
        }
        off += total;
    }
    Vec::new()
}

/// Child box inside `buf[start..end]` -> (payload offset, payload length).
fn mp4_box(buf: &[u8], start: usize, end: usize, typ: &[u8; 4]) -> Option<(usize, usize)> {
    let end = end.min(buf.len());
    let mut off = start;
    while off + 8 <= end {
        let size = be32(&buf[off..off + 4]) as usize;
        let kind = &buf[off + 4..off + 8];
        let (hdr, total) = if size == 1 {
            if off + 16 > end {
                return None;
            }
            (16usize, be64(&buf[off + 8..off + 16]))
        } else if size == 0 {
            (8usize, end - off)
        } else {
            (8usize, size)
        };
        if total < hdr || off + total > end {
            return None;
        }
        if kind == typ {
            return Some((off + hdr, total - hdr));
        }
        off += total;
    }
    None
}

/// moov 盒子里的 `ilst` 载荷（同时支持 moov/meta 与 moov/udta/meta 两种放法）。
fn mp4_ilst(moov: &[u8]) -> Option<(usize, usize)> {
    /* mp4_moov 返回的是"整个盒子含头部"：跳过那 8 字节，再走它的载荷
     * （mvhd / trak / udta）。 */
    if moov.len() < 8 || &moov[4..8] != b"moov" {
        return None;
    }
    let body = 8usize;
    let moov_end = moov.len();
    if let Some((mb, ml)) = mp4_box(moov, body, moov_end, b"udta") {
        if let Some((meta, mlen)) = mp4_box(moov, mb, mb + ml, b"meta") {
            // `meta` 是 full box：它的子盒子前面还有 version+flags 4 字节。
            if let Some(ilst) = mp4_box(moov, meta + 4, meta + mlen, b"ilst") {
                return Some(ilst);
            }
        }
    }
    if let Some((meta, mlen)) = mp4_box(moov, body, moov_end, b"meta") {
        if let Some(ilst) = mp4_box(moov, meta + 4, meta + mlen, b"ilst") {
            return Some(ilst);
        }
    }
    None
}

/// 按 fourcc 找 ilst 里的原子（`©nam`、`covr`、`----`……）。
fn mp4_atom(moov: &[u8], key: &[u8; 4]) -> Option<(usize, usize)> {
    let (ilst, len) = mp4_ilst(moov)?;
    mp4_box(moov, ilst, ilst + len, key)
}

/// First `data` atom inside an ilst atom -> (well-known type, payload).
fn mp4_atom_data(moov: &[u8], atom: (usize, usize)) -> Option<(usize, &[u8])> {
    let (body, len) = atom;
    let end = (body + len).min(moov.len());
    let mut off = body;
    while off + 8 <= end {
        let size = be32(&moov[off..off + 4]) as usize;
        if size < 8 || off + size > end {
            return None;
        }
        if &moov[off + 4..off + 8] == b"data" {
            if size < 16 {
                return None;
            }
            let dtype = be32(&moov[off + 8..off + 12]);
            return Some((dtype, &moov[off + 16..off + size]));
        }
        off += size;
    }
    None
}

/// A UTF-8 / UTF-16 text tag.
fn mp4_text(moov: &[u8], key: &[u8; 4]) -> String {
    let Some(atom) = mp4_atom(moov, key) else {
        return String::new();
    };
    let Some((dtype, payload)) = mp4_atom_data(moov, atom) else {
        return String::new();
    };
    if payload.is_empty() {
        return String::new();
    }
    match dtype {
        2 => {
            // UTF-16BE
            let units: Vec<u16> = payload
                .chunks_exact(2)
                .map(|c| ((c[0] as u16) << 8) | c[1] as u16)
                .collect();
            String::from_utf16_lossy(&units).trim().to_string()
        }
        _ => String::from_utf8_lossy(payload).trim().to_string(),
    }
}

/// 自由格式标签（`----:com.apple.iTunes:LYRICS`），按它的 `name` 匹配。
fn mp4_freeform(moov: &[u8], name: &[u8]) -> String {
    let (body, len) = mp4_ilst(moov).unwrap_or((0, 0));
    let end = (body + len).min(moov.len());
    let mut off = body;
    while off + 8 <= end {
        let size = be32(&moov[off..off + 4]) as usize;
        if size < 8 || off + size > end {
            break;
        }
        if &moov[off + 4..off + 8] == b"----" {
            let atom_end = off + size;
            let mut inner = off + 8;
            let mut hit = false;
            while inner + 8 <= atom_end {
                let isize = be32(&moov[inner..inner + 4]) as usize;
                if isize < 8 || inner + isize > atom_end {
                    break;
                }
                let kind = &moov[inner + 4..inner + 8];
                if kind == b"name" {
                    let skip = if isize >= 12 { 12 } else { 8 };
                    let val = &moov[inner + skip..inner + isize];
                    if val.eq_ignore_ascii_case(name) {
                        hit = true;
                    }
                } else if kind == b"data" && hit && isize >= 16 {
                    let payload = &moov[inner + 16..inner + isize];
                    return String::from_utf8_lossy(payload).trim().to_string();
                }
                inner += isize;
            }
        }
        off += size;
    }
    String::new()
}

/// 内嵌封面（`covr`）：JPEG/PNG 字节，交给 decode_cover_rgba 的形态与别的格式一致。
fn mp4_cover(moov: &[u8]) -> Option<Vec<u8>> {
    let atom = mp4_atom(moov, b"covr")?;
    let (_, payload) = mp4_atom_data(moov, atom)?;
    if payload.is_empty() {
        return None;
    }
    image_payload(payload).or_else(|| Some(payload.to_vec()))
}

fn mp4_lyrics(moov: &[u8]) -> String {
    let l = mp4_text(moov, b"\xa9lyr");
    if !l.trim().is_empty() {
        return l;
    }
    mp4_freeform(moov, b"LYRICS")
}

fn decode_cover_rgba(art: &[u8]) -> Option<Vec<u8>> {
    let mut ptr: *mut u8 = core::ptr::null_mut();
    let mut w: i32 = 0;
    let mut h: i32 = 0;
    let rc = unsafe { yunyin_image_decode(art.as_ptr(), art.len() as i32, &mut ptr, &mut w, &mut h) };
    if rc != 0 || ptr.is_null() || w <= 0 || h <= 0 {
        if !ptr.is_null() {
            unsafe { yunyin_image_free(ptr) };
        }
        return None;
    }
    let src = unsafe { core::slice::from_raw_parts(ptr, (w as usize) * (h as usize) * 4) };
    let mut out = vec![0u8; (COVER_PX as usize) * (COVER_PX as usize) * 4];
    let ok = unsafe {
        yunyin_image_resize(src.as_ptr(), w, h, out.as_mut_ptr(), COVER_PX as i32, COVER_PX as i32)
    };
    unsafe { yunyin_image_free(ptr) };
    if ok != 0 {
        return None;
    }
    Some(out)
}

/// 只读文件的一段有上限的前缀（会看 ID3v2 头来定长度）。
/// 标签与内嵌封面都靠前，所以这样就不用把整个音频文件读进来。
fn read_prefix(path: &str) -> Vec<u8> {
    if let Ok(mut f) = std::fs::File::open(path) {
        let mut head = [0u8; 10];
        let head_len = f.read(&mut head).unwrap_or(0);
        let want = if head_len >= 10 && &head[0..3] == b"ID3" {
            let tag = ((head[6] as usize) << 21)
                | ((head[7] as usize) << 14)
                | ((head[8] as usize) << 7)
                | (head[9] as usize);
            10usize + tag
        } else if head_len >= 8 {
            // FLAC 的元数据块 / OGG 的 comment 也靠前。
            PREFIX_CAP / 2
        } else {
            head_len as usize
        };
        let n = want.min(PREFIX_CAP);
        let mut buf = vec![0u8; n];
        let _ = f.seek(SeekFrom::Start(0));
        let _ = f.read(&mut buf);
        buf
    } else {
        Vec::new()
    }
}

/// FNV-1a —— 稳定、不依赖外部库的哈希，用来当缓存文件名。
fn fnv64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// 任意受支持格式的内嵌封面。M4A 的标签在 moov 里，而 moov 可能在文件末尾，
/// 所以这种格式走盒子链，而不是别的格式用的"固定前缀"。
fn cover_bytes(path: &str) -> Option<Vec<u8>> {
    let prefix = read_prefix(path);
    if is_mp4(&prefix) {
        let moov = mp4_moov(path);
        if !moov.is_empty() {
            return mp4_cover(&moov);
        }
    }
    extract_cover_bytes(&prefix)
}

fn cover_cache_path(path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let h = fnv64(path.as_bytes());
    Some(format!("ux0:/data/yunyin/covers/{:016x}.rgba", h))
}

fn ensure_cache_dir() {
    let _ = std::fs::create_dir_all("ux0:/data/yunyin/covers");
}

/// 源文件没变（大小与我们记录的一致）时，直接读缓存的 256×256 RGBA。
/// 任何失败都返回 None —— 调用方会退回真正解码，所以缓存永远不会变成正确性风险。
fn cover_cache_rgba(path: &str, size: u64) -> Option<Vec<u8>> {
    if size == 0 {
        return None;
    }
    let cp = cover_cache_path(path)?;
    let data = std::fs::read(&cp).ok()?;
    let expect = 8 + (COVER_PX as usize) * (COVER_PX as usize) * 4;
    if data.len() != expect {
        return None;
    }
    let stored = u64::from_ne_bytes(data[0..8].try_into().ok()?);
    if stored != size {
        return None;
    }
    Some(data[8..].to_vec())
}

/// 把解码好的 RGBA 按文件大小做键存起来。尽力而为；错误一律忽略，
/// 这样只读卡或写满的卡就继续走内存里那条解码路径。
fn write_cover_cache(path: &str, size: u64, rgba: &[u8]) {
    if size == 0 || rgba.len() != (COVER_PX as usize) * (COVER_PX as usize) * 4 {
        return;
    }
    ensure_cache_dir();
    if let Some(cp) = cover_cache_path(path) {
        let mut buf = Vec::with_capacity(8 + rgba.len());
        buf.extend_from_slice(&size.to_ne_bytes());
        buf.extend_from_slice(rgba);
        let _ = std::fs::write(&cp, &buf);
    }
}

pub(crate) fn upload_cover(path: &str) -> i32 {
    if path.is_empty() {
        return -1;
    }
    if let Ok(mut g) = COVER_HANDLES.lock() {
        let map = g.get_or_insert_with(HashMap::new);
        if let Some(&h) = map.get(path) {
            return h;
        }
    }
    // 指纹就用源文件大小（一次便宜 stat，不读整个文件）。持久缓存里存的是解好的
    // 256×256 RGBA，下次启动可以直接复用，不必再抽一次内嵌 JPEG、再解一次。
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let cached = if size > 0 { cover_cache_rgba(path, size) } else { None };
    let rgba = match cached {
        Some(v) => v,
        None => {
            let art = match cover_bytes(path) {
                Some(a) => a,
                None => return -1,
            };
            let v = match decode_cover_rgba(&art) {
                Some(p) => p,
                None => return -1,
            };
            if size > 0 {
                write_cover_cache(path, size, &v);
            }
            v
        }
    };
    let handle = unsafe {
        let h = crate::ffi::ui().upload_texture(&rgba, COVER_PX, COVER_PX, psm::PSM_8888);
        if h >= 0 {
            crate::graphics::register_texture(crate::ffi::ui(), h);
        }
        h
    };
    if handle >= 0 {
        if let Ok(mut g) = COVER_HANDLES.lock() {
            g.get_or_insert_with(HashMap::new).insert(path.to_string(), handle);
        }
    }
    handle
}

pub(crate) fn tags_json(path: &str) -> String {
    let bytes = read_prefix(path);
    if bytes.is_empty() {
        return "{\"title\":\"\",\"artist\":\"\",\"album\":\"\",\"cover\":false}".into();
    }
    /* M4A：按 fourcc 索引的 ilst 原子，moov 在哪读哪。 */
    if is_mp4(&bytes) {
        let moov = mp4_moov(path);
        let (title, artist, album, cover, lyrics) = if moov.is_empty() {
            (
                String::new(),
                String::new(),
                String::new(),
                false,
                String::new(),
            )
        } else {
            (
                mp4_text(&moov, b"\xa9nam"),
                mp4_text(&moov, b"\xa9ART"),
                mp4_text(&moov, b"\xa9alb"),
                mp4_cover(&moov).is_some(),
                mp4_lyrics(&moov),
            )
        };
        return format!(
            "{{\"title\":\"{}\",\"artist\":\"{}\",\"album\":\"{}\",\"cover\":{},\"lyrics\":\"{}\"}}",
            super::json_escape(&title),
            super::json_escape(&artist),
            super::json_escape(&album),
            if cover { "true" } else { "false" },
            super::json_escape(&lyrics)
        );
    }
    let mut title = extract_id3_text(&bytes, b"TIT2");
    let mut artist = extract_id3_text(&bytes, b"TPE1");
    let mut album = extract_id3_text(&bytes, b"TALB");
    if title.is_empty() {
        title = extract_ogg_text(&bytes, "TITLE");
    }
    if artist.is_empty() {
        artist = extract_ogg_text(&bytes, "ARTIST");
    }
    if album.is_empty() {
        album = extract_ogg_text(&bytes, "ALBUM");
    }
    let cover = extract_cover_bytes(&bytes).is_some();
    let lyrics = extract_embedded_lyrics(&bytes);
    format!(
        "{{\"title\":\"{}\",\"artist\":\"{}\",\"album\":\"{}\",\"cover\":{},\"lyrics\":\"{}\"}}",
        super::json_escape(&title),
        super::json_escape(&artist),
        super::json_escape(&album),
        if cover { "true" } else { "false" },
        super::json_escape(&lyrics)
    )
}
