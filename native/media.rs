//! Local music I/O for Yunyin: list mounts, decode wav/mp3/ogg, extract
//! embedded album art (ID3 APIC / Vorbis METADATA_BLOCK_PICTURE), feed
//! sceAudioOut. MP3 tries SceAudiodec first, then minimp3.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::sync::Mutex;

use libquickjs_sys::*;
use pocketjs_core::spec::psm;

use crate::audio;

extern "C" {
    fn JS_ToCStringLen2(
        ctx: *mut JSContext,
        plen: *mut size_t,
        val1: JSValue,
        cesu8: i32,
    ) -> *const i8;
    fn JS_NewStringLen(ctx: *mut JSContext, str1: *const u8, len1: usize) -> JSValue;
    fn yp_open(path: *const i8) -> i32;
    fn yp_rate() -> i32;
    fn yp_decode(buf: *mut i16, max_frames: i32) -> i32;
    fn yp_length() -> i64;
    fn yp_close();
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
    fn yunyin_list_dir(path: *const u8, out: *mut u8, cap: i32) -> i32;
}

const TARGET_RATE: u32 = 44100;
const MAX_ART: usize = 1024 * 1024;
const COVER_PX: u32 = 256;
const DEC_SW: u32 = 0;
const DEC_HW: u32 = 1;
const DEC_PCM: u32 = 2;

static PLAYING: AtomicBool = AtomicBool::new(false);
static PAUSED: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
static WORKER: AtomicBool = AtomicBool::new(false);
static POS_MS: AtomicU32 = AtomicU32::new(0);
static DUR_MS: AtomicU32 = AtomicU32::new(0);
static RATE_HZ: AtomicU32 = AtomicU32::new(TARGET_RATE);
static DEC_KIND: AtomicU32 = AtomicU32::new(DEC_SW);
static PATH_LEN: AtomicUsize = AtomicUsize::new(0);
static PATH_BUF: Mutex<String> = Mutex::new(String::new());
static COVER_HANDLES: Mutex<Option<HashMap<String, i32>>> = Mutex::new(None);

fn set_path(p: &str) {
    if let Ok(mut g) = PATH_BUF.lock() {
        g.clear();
        g.push_str(p);
        PATH_LEN.store(g.len(), Ordering::Release);
    }
}

fn get_path() -> String {
    PATH_BUF.lock().map(|g| g.clone()).unwrap_or_default()
}

fn json_escape(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            _ => o.push(c),
        }
    }
    o
}

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

fn resample_stereo(pcm: &[i16], rate: u32, ch: u32) -> Vec<i16> {
    if pcm.is_empty() {
        return Vec::new();
    }
    let frames = if ch == 1 { pcm.len() } else { pcm.len() / 2 };
    if rate == TARGET_RATE && ch == 2 {
        return pcm.to_vec();
    }
    let out_frames = ((frames as u64) * (TARGET_RATE as u64) / (rate.max(1) as u64)) as usize;
    let mut out = vec![0i16; out_frames.saturating_mul(2)];
    for i in 0..out_frames {
        let src = (i as u64) * (rate as u64) / (TARGET_RATE as u64);
        let s = (src as usize).min(frames.saturating_sub(1));
        let (l, r) = if ch == 1 {
            (pcm[s], pcm[s])
        } else {
            (pcm[s * 2], pcm[s * 2 + 1])
        };
        out[i * 2] = l;
        out[i * 2 + 1] = r;
    }
    out
}

fn pump_stream(path: &str) {
    let c_path = match std::ffi::CString::new(path) {
        Ok(c) => c,
        Err(_) => {
            PLAYING.store(false, Ordering::Release);
            return;
        }
    };
    if unsafe { yp_open(c_path.as_ptr()) } != 0 {
        PLAYING.store(false, Ordering::Release);
        return;
    }

    let rate = (unsafe { yp_rate() }).max(0) as u32;
    let total_frames = (unsafe { yp_length() }).max(0) as u64;
    let duration_ms = ((total_frames as u64) * 1000 / (rate.max(1) as u64)) as u32;
    unsafe {
        let _ = audio::start(TARGET_RATE);
        audio::flush();
    }
    RATE_HZ.store(TARGET_RATE, Ordering::Release);
    DUR_MS.store(duration_ms, Ordering::Release);
    POS_MS.store(0, Ordering::Release);
    PLAYING.store(true, Ordering::Release);
    PAUSED.store(false, Ordering::Release);

    /* yplayer outputs stereo (2ch) frames at the file's native rate. */
    let mut buf = vec![0i16; 1024 * 2];
    let mut at: u64 = 0;
    loop {
        if STOP.load(Ordering::Acquire) {
            break;
        }
        if PAUSED.load(Ordering::Acquire) {
            unsafe { vitasdk_sys::sceKernelDelayThread(8_000) };
            continue;
        }
        let free = unsafe { audio::free_frames() };
        if free < 1024 {
            unsafe { vitasdk_sys::sceKernelDelayThread(4_000) };
            continue;
        }
        let want = (free as usize).min(1024);
        let got = unsafe { yp_decode(buf.as_mut_ptr(), want as i32) };
        if got <= 0 {
            break;
        }
        let frames = got as usize;
        let stereo = resample_stereo(&buf[..frames * 2], rate, 2);
        unsafe { audio::push(&stereo, 2) };
        at += frames as u64;
        POS_MS.store(((at * 1000) / (rate.max(1) as u64)) as u32, Ordering::Release);
    }

    PLAYING.store(false, Ordering::Release);
    unsafe { yp_close() };
}

fn spawn_play(path: String) {
    STOP.store(true, Ordering::Release);
    for _ in 0..50 {
        if !WORKER.load(Ordering::Acquire) {
            break;
        }
        unsafe { vitasdk_sys::sceKernelDelayThread(4_000) };
    }
    STOP.store(false, Ordering::Release);
    set_path(&path);
    WORKER.store(true, Ordering::Release);
    let _ = std::thread::Builder::new()
        .name("yunyin-dec".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            pump_stream(&path);
            WORKER.store(false, Ordering::Release);
        });
}

/// List a directory using SceIo directly (via the C shim).  Real hardware's
/// SceIo is stricter than Vita3K about the `device:path` form, so the shim
/// tries a few accepted spellings and logs which one worked / the error.  This
/// avoids Rust's `std::fs::read_dir`, which is unreliable on the real device
/// for directory streams (file open via std::fs works, dir listing does not).
fn list_dir(path: &str) -> String {
    let c_path = match std::ffi::CString::new(path) {
        Ok(c) => c,
        Err(_) => return "[]".to_string(),
    };
    let mut buf = vec![0u8; 256 * 1024];
    let n = unsafe {
        yunyin_list_dir(
            c_path.as_ptr() as *const u8,
            buf.as_mut_ptr(),
            buf.len() as i32,
        )
    };
    if n <= 0 {
        return "[]".to_string();
    }
    let n = (n as usize).min(buf.len());
    String::from_utf8_lossy(&buf[..n]).into_owned()
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
                    if comment.len() >= 24
                        && comment[..24].eq_ignore_ascii_case("METADATA_BLOCK_PICTURE=")
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

fn upload_cover(path: &str) -> i32 {
    if path.is_empty() {
        return -1;
    }
    if let Ok(mut g) = COVER_HANDLES.lock() {
        let map = g.get_or_insert_with(HashMap::new);
        if let Some(&h) = map.get(path) {
            return h;
        }
    }
    let bytes = match fs::read(path) {
        Ok(b) => b,
        _ => return -1,
    };
    let art = match extract_cover_bytes(&bytes) {
        Some(a) => a,
        None => return -1,
    };
    let rgba = match decode_cover_rgba(&art) {
        Some(p) => p,
        None => return -1,
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

fn tags_json(path: &str) -> String {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        _ => {
            return "{\"title\":\"\",\"artist\":\"\",\"album\":\"\",\"cover\":false}".into();
        }
    };
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
        json_escape(&title),
        json_escape(&artist),
        json_escape(&album),
        if cover { "true" } else { "false" },
        json_escape(&lyrics)
    )
}

fn arg_string(ctx: *mut JSContext, argc: i32, argv: *mut JSValue, i: isize) -> String {
    if (i as i32) >= argc {
        return String::new();
    }
    let mut len: size_t = 0;
    let s = unsafe { JS_ToCStringLen2(ctx, &mut len, *argv.offset(i), 0) };
    if s.is_null() {
        return String::new();
    }
    let bytes = unsafe { core::slice::from_raw_parts(s as *const u8, len) };
    let text = String::from_utf8_lossy(bytes).into_owned();
    unsafe { JS_FreeCString(ctx, s) };
    text
}

unsafe fn js_str(ctx: *mut JSContext, s: &str) -> JSValue {
    JS_NewStringLen(ctx, s.as_ptr(), s.len())
}

unsafe extern "C" fn js_list(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let path = arg_string(ctx, argc, argv, 0);
    js_str(ctx, &list_dir(&path))
}

unsafe extern "C" fn js_roots(
    ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    js_str(
        ctx,
        "[{\"id\":\"app0\",\"path\":\"app0:music\"},{\"id\":\"ux0\",\"path\":\"ux0:\"},{\"id\":\"uma0\",\"path\":\"uma0:\"},{\"id\":\"ur0\",\"path\":\"ur0:\"}]",
    )
}

unsafe extern "C" fn js_play(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let path = arg_string(ctx, argc, argv, 0);
    if !path.is_empty() {
        spawn_play(path);
    }
    JS_UNDEFINED
}

unsafe extern "C" fn js_pause(
    _ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    PAUSED.store(true, Ordering::Release);
    JS_UNDEFINED
}

unsafe extern "C" fn js_resume(
    _ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    PAUSED.store(false, Ordering::Release);
    PLAYING.store(true, Ordering::Release);
    JS_UNDEFINED
}

unsafe extern "C" fn js_stop(
    _ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    STOP.store(true, Ordering::Release);
    PAUSED.store(false, Ordering::Release);
    PLAYING.store(false, Ordering::Release);
    POS_MS.store(0, Ordering::Release);
    unsafe { audio::flush() };
    JS_UNDEFINED
}

fn dec_label() -> &'static str {
    match DEC_KIND.load(Ordering::Acquire) {
        DEC_HW => "hw",
        DEC_PCM => "pcm",
        _ => "sw",
    }
}

unsafe extern "C" fn js_state(
    ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    let path = get_path();
    let json = format!(
        "{{\"playing\":{},\"paused\":{},\"path\":\"{}\",\"pos\":{},\"dur\":{},\"rate\":{},\"dec\":\"{}\"}}",
        if PLAYING.load(Ordering::Acquire) && !PAUSED.load(Ordering::Acquire) {
            "true"
        } else {
            "false"
        },
        if PAUSED.load(Ordering::Acquire) {
            "true"
        } else {
            "false"
        },
        json_escape(&path),
        POS_MS.load(Ordering::Acquire),
        DUR_MS.load(Ordering::Acquire),
        RATE_HZ.load(Ordering::Acquire),
        dec_label()
    );
    js_str(ctx, &json)
}

unsafe extern "C" fn js_cover(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let path = arg_string(ctx, argc, argv, 0);
    JS_NewInt32(ctx, upload_cover(&path))
}

unsafe extern "C" fn js_tags(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let path = arg_string(ctx, argc, argv, 0);
    js_str(ctx, &tags_json(&path))
}

unsafe extern "C" fn js_log(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let s = arg_string(ctx, argc, argv, 0);
    let line = format!("{}\n", s);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("ux0:data/yunyin.log")
    {
        let _ = f.write_all(line.as_bytes());
    }
    JS_NewInt32(ctx, 0)
}

unsafe fn add_fn(
    ctx: *mut JSContext,
    obj: JSValue,
    name: &[u8],
    f: unsafe extern "C" fn(*mut JSContext, JSValue, i32, *mut JSValue) -> JSValue,
    nargs: i32,
) {
    let v = JS_NewCFunction2(
        ctx,
        Some(f),
        name.as_ptr() as *const _,
        nargs,
        JS_CFUNC_generic,
        0,
    );
    JS_SetPropertyStr(ctx, obj, name.as_ptr() as *const _, v);
}

/// Install `globalThis.vitaMedia`.
///
/// # Safety
/// Same realm, render thread, once per guest.
pub unsafe fn register(ctx: *mut JSContext, global: JSValue) {
    let obj = JS_NewObject(ctx);
    add_fn(ctx, obj, b"list\0", js_list, 1);
    add_fn(ctx, obj, b"roots\0", js_roots, 0);
    add_fn(ctx, obj, b"play\0", js_play, 1);
    add_fn(ctx, obj, b"pause\0", js_pause, 0);
    add_fn(ctx, obj, b"resume\0", js_resume, 0);
    add_fn(ctx, obj, b"stop\0", js_stop, 0);
    add_fn(ctx, obj, b"state\0", js_state, 0);
    add_fn(ctx, obj, b"cover\0", js_cover, 1);
    add_fn(ctx, obj, b"tags\0", js_tags, 1);
    add_fn(ctx, obj, b"logMsg\0", js_log, 1);
    JS_SetPropertyStr(ctx, global, c"vitaMedia".as_ptr(), obj);
}

#[allow(dead_code)]
fn _keep_c_void(p: *mut c_void) {
    let _ = p;
}
