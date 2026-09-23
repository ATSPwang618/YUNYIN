//! Device-local `io.offload` for PocketJS 0.12.0 streamed CJK (PJFA).
//!
//! PSP's worker talks USB/`ms0:`; Vita reads `app0:/fonts/*.pjfa` on a
//! background thread so glyph I/O never touches QuickJS or the UI core.
#![allow(static_mut_refs)]

use core::{
    cell::UnsafeCell,
    fmt::{self, Write},
    sync::atomic::{AtomicU32, Ordering::*},
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    thread,
    time::Duration,
};

use pocketjs_core::font_archive::{Archive, ReadAt};

const FREE: u32 = 0;
const QUEUED: u32 = 1;
const BUSY: u32 = 2;
const READY: u32 = 3;
const ROOT: &str = "app0:/";

#[derive(Clone, Copy)]
pub struct Request {
    pub id: u32,
    pub op: u8,
    pub path: [u8; 128],
    pub generation: u32,
    pub slot: u8,
    pub count: u8,
    pub cps: [u32; 4],
}

impl Request {
    pub const fn empty() -> Self {
        Self {
            id: 0,
            op: 0,
            path: [0; 128],
            generation: 0,
            slot: 0,
            count: 0,
            cps: [0; 4],
        }
    }
}

struct Data {
    epoch: u32,
    seq: u32,
    request: Request,
    len: usize,
    reply: [u8; 4096],
}

struct Slot {
    state: AtomicU32,
    data: UnsafeCell<Data>,
}

unsafe impl Sync for Slot {}

impl Slot {
    const fn new() -> Self {
        Self {
            state: AtomicU32::new(FREE),
            data: UnsafeCell::new(Data {
                epoch: 0,
                seq: 0,
                request: Request::empty(),
                len: 0,
                reply: [0; 4096],
            }),
        }
    }
}

static SLOTS: [Slot; 8] = [const { Slot::new() }; 8];
static EPOCH: AtomicU32 = AtomicU32::new(1);
static ONLINE: AtomicU32 = AtomicU32::new(0);
static STARTED: AtomicU32 = AtomicU32::new(0);
static mut SEQUENCE: u32 = 0;
static mut SENT: bool = false;
static mut TAKEN: bool = false;

struct IoFile(File);

impl ReadAt for IoFile {
    fn read_at(&mut self, offset: u32, out: &mut [u8]) -> bool {
        if self.0.seek(SeekFrom::Start(offset as u64)).is_err() {
            return false;
        }
        let mut done = 0;
        while done < out.len() {
            match self.0.read(&mut out[done..]) {
                Ok(0) => return false,
                Ok(n) => done += n,
                Err(_) => return false,
            }
        }
        true
    }
}

fn open(path: &[u8; 128]) -> Result<(IoFile, u32), &'static str> {
    let n = path.iter().position(|c| *c == 0).ok_or("Path too long")?;
    let rel = core::str::from_utf8(&path[..n]).map_err(|_| "Invalid local path")?;
    if rel.is_empty()
        || rel.starts_with('/')
        || rel.contains("..")
        || !rel
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"/._-".contains(&c))
    {
        return Err("Invalid local path");
    }
    let full = format!("{ROOT}{rel}");
    let mut f = File::open(&full).map_err(|_| "Local file unavailable")?;
    let len = f.seek(SeekFrom::End(0)).map_err(|_| "Local file size unavailable")?;
    if len > u32::MAX as u64 {
        return Err("Local file size unavailable");
    }
    Ok((IoFile(f), len as u32))
}

struct Buffer<'a> {
    bytes: &'a mut [u8],
    len: usize,
}

impl Write for Buffer<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.len + s.len() > self.bytes.len() {
            return Err(fmt::Error);
        }
        self.bytes[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
        self.len += s.len();
        Ok(())
    }
}

fn quote(out: &mut Buffer<'_>, s: &str) -> fmt::Result {
    out.write_char('"')?;
    for c in s.chars() {
        match c {
            '"' => out.write_str("\\\"")?,
            '\\' => out.write_str("\\\\")?,
            '\n' => out.write_str("\\n")?,
            '\r' => out.write_str("\\r")?,
            '\t' => out.write_str("\\t")?,
            c if c < ' ' => write!(out, "\\u{:04x}", c as u32)?,
            c => out.write_char(c)?,
        }
    }
    out.write_char('"')
}

fn run(
    request: &Request,
    archive: &mut Option<Archive<IoFile>>,
    generation: &mut u32,
    payload: &mut Buffer<'_>,
) -> Result<(), &'static str> {
    match request.op {
        1 => {
            let (f, len) = open(&request.path)?;
            let face = Archive::open(f, len).map_err(|e| e.message())?;
            *generation = generation.wrapping_add(1).max(1);
            write!(payload, "{{\"generation\":{},\"identity\":\"", generation)
                .map_err(|_| "Reply overflow")?;
            for b in face.identity {
                write!(payload, "{:02x}", b).map_err(|_| "Reply overflow")?;
            }
            payload
                .write_str("\",\"strikes\":[")
                .map_err(|_| "Reply overflow")?;
            for (i, s) in face.strikes[..face.count].iter().enumerate() {
                write!(
                    payload,
                    "{}[{},{},{},{},{},{},{},{}]",
                    if i > 0 { "," } else { "" },
                    s.slot,
                    s.width,
                    s.height,
                    s.baseline,
                    s.line_height,
                    s.advance,
                    s.density,
                    s.count
                )
                .map_err(|_| "Reply overflow")?;
            }
            payload.write_str("]}").map_err(|_| "Reply overflow")?;
            *archive = Some(face);
        }
        2 => {
            if request.generation != *generation {
                return Err("Stale font generation");
            }
            let face = archive.as_mut().ok_or("Font archive not open")?;
            let mut bytes = [0; 1250];
            let n = face
                .batch(
                    *generation,
                    request.slot,
                    &request.cps[..request.count as usize],
                    &mut bytes,
                )
                .map_err(|e| e.message())?;
            for b in &bytes[..n] {
                write!(payload, "{:02x}", b).map_err(|_| "Reply overflow")?;
            }
        }
        3 => {
            let s = archive.as_ref().ok_or("Font archive not open")?.stats;
            write!(
                payload,
                "{{\"reads\":{},\"bytes\":{},\"indexHits\":{},\"glyphs\":{},\"missing\":{},\"failures\":{}}}",
                s.reads, s.bytes, s.index_hits, s.glyphs, s.missing, s.failures
            )
            .map_err(|_| "Reply overflow")?;
        }
        4 => {
            let (mut f, n) = open(&request.path)?;
            if n > 1536 {
                return Err("Text file exceeds 1536 bytes");
            }
            let mut b = [0; 1536];
            if !f.read_at(0, &mut b[..n as usize]) {
                return Err("Text read failed");
            }
            payload
                .write_str(core::str::from_utf8(&b[..n as usize]).map_err(|_| "Text is not UTF-8")?)
                .map_err(|_| "Reply overflow")?;
        }
        5 => {
            *archive = None;
            payload.write_str("closed").map_err(|_| "Reply overflow")?;
        }
        _ => return Err("Unsupported local capability"),
    }
    Ok(())
}

fn worker() {
    let mut archive = None;
    let mut generation = 0u32;
    let mut epoch = 0u32;
    loop {
        let current = EPOCH.load(Acquire);
        if current != epoch {
            archive = None;
            epoch = current;
            for s in &SLOTS {
                s.state.store(FREE, Release);
            }
            ONLINE.store(epoch, Release);
        }
        let next = SLOTS
            .iter()
            .filter(|s| s.state.load(Acquire) == QUEUED)
            .min_by_key(|s| unsafe { (*s.data.get()).seq });
        if let Some(slot) = next {
            slot.state.store(BUSY, Release);
            let d = unsafe { &mut *slot.data.get() };
            let mut raw = [0u8; 2500];
            let mut payload = Buffer {
                bytes: &mut raw,
                len: 0,
            };
            let result = run(&d.request, &mut archive, &mut generation, &mut payload);
            let mut reply = Buffer {
                bytes: &mut d.reply,
                len: 0,
            };
            let _ = write!(reply, "{{\"id\":{},", d.request.id);
            let ok = match result {
                Ok(()) => reply.write_str("\"payload\":").and_then(|_| {
                    quote(
                        &mut reply,
                        core::str::from_utf8(&payload.bytes[..payload.len]).unwrap_or(""),
                    )
                }),
                Err(e) => reply.write_str("\"error\":").and_then(|_| quote(&mut reply, e)),
            };
            if ok.is_ok() && reply.write_str("}").is_ok() {
                d.len = reply.len;
            } else {
                d.len = 0;
            }
            slot.state.store(READY, Release);
        } else {
            thread::sleep(Duration::from_millis(1));
        }
    }
}

pub fn start() {
    if STARTED.swap(1, AcqRel) != 0 {
        return;
    }
    thread::spawn(worker);
}

pub fn session() -> i32 {
    start();
    let e = EPOCH.load(Acquire);
    if ONLINE.load(Acquire) == e {
        e as i32
    } else {
        0
    }
}

pub unsafe fn frame() {
    SENT = false;
    TAKEN = false;
}

pub unsafe fn reset() {
    EPOCH.fetch_add(1, AcqRel);
}

pub unsafe fn submit(request: Request) -> bool {
    if SENT || session() <= 0 {
        return false;
    }
    for slot in &SLOTS {
        if slot.state.load(Acquire) == FREE {
            let d = &mut *slot.data.get();
            d.epoch = EPOCH.load(Acquire);
            SEQUENCE = SEQUENCE.wrapping_add(1);
            d.seq = SEQUENCE;
            d.request = request;
            slot.state.store(QUEUED, Release);
            SENT = true;
            return true;
        }
    }
    false
}

pub unsafe fn take() -> Option<String> {
    if TAKEN {
        return None;
    }
    for slot in &SLOTS {
        if slot.state.load(Acquire) == READY {
            let d = &*slot.data.get();
            let result = if d.epoch == EPOCH.load(Acquire) {
                core::str::from_utf8(&d.reply[..d.len])
                    .ok()
                    .map(String::from)
            } else {
                None
            };
            slot.state.store(FREE, Release);
            if result.is_some() {
                TAKEN = true;
                return result;
            }
        }
    }
    None
}
