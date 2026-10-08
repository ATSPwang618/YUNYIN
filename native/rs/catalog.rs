//! Rust-owned online catalog snapshots.
//!
//! Network workers write the list files, but the UI must not read and parse
//! those files from the guest frame.  This module parses each document on a
//! native worker thread and exposes only small, bounded views over the bridge:
//! menu summaries, visible song windows, and id-only queue data.

use crate::media::provider::netease::{json::Json, lists};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, Once};
use std::thread;
use std::time::Duration;

const KNOWN_FILES: &[&str] = &[
    "discover.json",
    "daily.json",
    "account_playlists.json",
    "toplist_3778678.json",
    "toplist_19723756.json",
    "toplist_3779629.json",
    "toplist_2884035.json",
];

#[derive(Clone)]
struct Document {
    name: String,
    stamp: String,
    root: Json,
}

#[derive(Clone)]
struct PageCache {
    name: String,
    stamp: String,
    offset: usize,
    limit: usize,
    raw: String,
}

enum Command {
    Refresh(String),
    Page(String, usize, usize),
}

static START: Once = Once::new();
static COMMANDS: Mutex<Option<Sender<Command>>> = Mutex::new(None);
static WATCHED: Mutex<Vec<String>> = Mutex::new(Vec::new());
/* 存 Arc<Document>：解析好的歌单可能上千首，guest 线程每次取一页都深拷贝整份
 * 文档是纯浪费（真机日志里 catalogPage 一次 30~200ms 就是这么来的）。现在只
 * 复制一个 Arc 计数。 */
static DOCUMENTS: Mutex<Vec<Arc<Document>>> = Mutex::new(Vec::new());
static PAGE_CACHE: Mutex<Vec<PageCache>> = Mutex::new(Vec::new());
static PAGE_PENDING: Mutex<Vec<(String, usize, usize)>> = Mutex::new(Vec::new());
static VERSION: AtomicU64 = AtomicU64::new(0);

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && name.ends_with(".json")
}

fn remember_watch(name: &str) {
    if !valid_name(name) {
        return;
    }
    if let Ok(mut watched) = WATCHED.lock() {
        if !watched.iter().any(|n| n == name) {
            watched.push(String::from(name));
        }
    }
}

fn command_sender() -> Option<Sender<Command>> {
    COMMANDS.lock().ok().and_then(|slot| slot.as_ref().cloned())
}

fn parse_document(name: &str, body: &str) -> Option<Document> {
    let root = Json::parse(body).ok()?;
    Some(Document {
        name: String::from(name),
        stamp: lists::stat(name),
        root,
    })
}

fn replace_document(document: Document) {
    let name = document.name.clone();
    let Ok(mut docs) = DOCUMENTS.lock() else {
        return;
    };
    if let Some(old) = docs.iter_mut().find(|d| d.name == document.name) {
        if old.stamp == document.stamp {
            return;
        }
        *old = Arc::new(document);
    } else {
        docs.push(Arc::new(document));
    }
    drop(docs);
    if let Ok(mut pages) = PAGE_CACHE.lock() {
        pages.retain(|page| page.name != name);
    }
    VERSION.fetch_add(1, Ordering::AcqRel);
}

fn refresh_one(name: &str) {
    if !valid_name(name) {
        return;
    }
    let stamp = lists::stat(name);
    if stamp.is_empty() {
        return;
    }
    if DOCUMENTS
        .lock()
        .ok()
        .and_then(|docs| docs.iter().find(|d| d.name == name).map(|d| d.stamp == stamp))
        .unwrap_or(false)
    {
        return;
    }
    let body = lists::read(name);
    if let Some(document) = parse_document(name, &body) {
        replace_document(document);
    }
}

fn worker(rx: Receiver<Command>) {
    let mut scan_tick = 0u8;
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Command::Refresh(name)) => refresh_one(&name),
            Ok(Command::Page(name, offset, limit)) => {
                /* Scrolling can enqueue several windows before the worker gets
                 * CPU time.  Only the newest window for this file matters.
                 *
                 * The old code treated *any* other pending window as proof
                 * that this command was stale.  That was not enough: it then
                 * left the current command in PAGE_PENDING and continued.
                 * A fast 0 -> 16 -> 32 turn could therefore discard every
                 * command while leaving the last key permanently pending;
                 * every later read of that page would return loading forever.
                 *
                 * The channel is FIFO, so a command is stale only when a
                 * newer request for the same file appears after its own key in
                 * PAGE_PENDING.  Always remove the current key before either
                 * continuing or processing it. */
                let current_key = (name.clone(), offset, limit);
                let stale = PAGE_PENDING
                    .lock()
                    .map(|pending| {
                        let Some(index) = pending.iter().position(|item| item == &current_key)
                        else {
                            return false;
                        };
                        pending[index + 1..]
                            .iter()
                            .any(|(file, _, _)| file == &name)
                    })
                    .unwrap_or(false);
                if let Ok(mut pending) = PAGE_PENDING.lock() {
                    pending.retain(|item| item != &current_key);
                }
                if stale {
                    continue;
                }
                refresh_one(&name);
                if let Some(doc) = document(&name) {
                    let raw = page_json_from_document(&doc, offset, limit);
                    if let Ok(mut pages) = PAGE_CACHE.lock() {
                        if let Some(old) = pages.iter_mut().find(|page| {
                            page.name == doc.name && page.offset == offset && page.limit == limit
                        }) {
                            old.stamp = doc.stamp.clone();
                            old.raw = raw;
                        } else {
                            pages.push(PageCache {
                                name: doc.name.clone(),
                                stamp: doc.stamp.clone(),
                                offset,
                                limit,
                                raw,
                            });
                        }
                        while pages.len() > 32 {
                            pages.remove(0);
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }

        /* External tools may replace a list file without going through write_raw.
         * Poll only from this native worker, never from the JS frame. */
        scan_tick = scan_tick.wrapping_add(1);
        if scan_tick < 4 {
            continue;
        }
        scan_tick = 0;
        let watched = WATCHED.lock().map(|g| g.clone()).unwrap_or_default();
        for name in watched {
            refresh_one(&name);
        }
    }
}

/// Start the native parser worker once.  Calling this repeatedly is harmless.
pub fn start() {
    START.call_once(|| {
        let (tx, rx) = mpsc::channel();
        if let Ok(mut slot) = COMMANDS.lock() {
            *slot = Some(tx.clone());
        }
        let spawned = thread::Builder::new()
            .name("yunyin-catalog".into())
            .stack_size(128 * 1024)
            .spawn(move || worker(rx));
        if spawned.is_err() {
            if let Ok(mut slot) = COMMANDS.lock() {
                *slot = None;
            }
            return;
        }
        for name in KNOWN_FILES {
            remember_watch(name);
            let _ = tx.send(Command::Refresh(String::from(*name)));
        }
    });
}

/// Publish the exact body written by the native list worker.  Parsing happens
/// on that worker, not on the QuickJS/render thread.
pub fn publish(name: &str, body: &str) {
    start();
    remember_watch(name);
    if let Some(document) = parse_document(name, body) {
        replace_document(document);
        crate::media::platform::log::append(&format!(
            "catalog: publish_ok name={} bytes={} version={}",
            name,
            body.len(),
            version()
        ));
    } else {
        crate::media::platform::log::append(&format!(
            "catalog: publish_parse_failed name={} bytes={}",
            name,
            body.len()
        ));
    }
}

/// Ask the native worker to load a dynamically opened playlist file.
pub fn request(name: &str) {
    start();
    if !valid_name(name) {
        return;
    }
    remember_watch(name);
    if let Some(tx) = command_sender() {
        let _ = tx.send(Command::Refresh(String::from(name)));
    }
}

fn request_page(name: &str, offset: usize, limit: usize) {
    start();
    if !valid_name(name) {
        return;
    }
    remember_watch(name);
    let safe_limit = limit.min(16);
    let key = (String::from(name), offset, safe_limit);
    if let Ok(mut pending) = PAGE_PENDING.lock() {
        if pending.iter().any(|item| item == &key) {
            return;
        }
        pending.push(key.clone());
    }
    if let Some(tx) = command_sender() {
        if tx
            .send(Command::Page(String::from(name), offset, safe_limit))
            .is_err()
        {
            if let Ok(mut pending) = PAGE_PENDING.lock() {
                pending.retain(|item| item != &key);
            }
        }
    }
}

pub fn version() -> u64 {
    start();
    VERSION.load(Ordering::Acquire)
}

fn document(name: &str) -> Option<Arc<Document>> {
    DOCUMENTS
        .lock()
        .ok()
        .and_then(|docs| docs.iter().find(|d| d.name == name).cloned())
}

fn page_cache(name: &str, offset: usize, limit: usize) -> Option<String> {
    let doc_stamp = document(name)?.stamp.clone();
    PAGE_CACHE
        .lock()
        .ok()
        .and_then(|pages| {
            pages
                .iter()
                .find(|page| {
                    page.name == name
                        && page.stamp == doc_stamp
                        && page.offset == offset
                        && page.limit == limit
                })
                .map(|page| page.raw.clone())
        })
}

fn string_field(root: &Json, key: &str) -> String {
    root.get(key)
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_string()
}

fn number_field(root: &Json, key: &str) -> u64 {
    root.get(key).and_then(Json::as_u64).unwrap_or(0)
}

fn song_id(song: &Json) -> String {
    match song.get("id") {
        Some(Json::Str(id)) => id.clone(),
        Some(Json::Num(n)) if n.fract() == 0.0 && *n >= 0.0 => format!("{n:.0}"),
        _ => String::new(),
    }
}

fn song_json(song: &Json) -> Option<String> {
    let id = song_id(song);
    if id.is_empty() {
        return None;
    }
    let title = string_field(song, "title");
    let artists = string_field(song, "artists");
    let album = string_field(song, "album");
    let duration = number_field(song, "durationMs");
    let off = number_field(song, "off");
    let vip = number_field(song, "vip");
    let fee = number_field(song, "fee");
    let mut out = format!(
        "{{\"id\":\"{}\",\"title\":\"{}\",\"artists\":\"{}\",\"album\":\"{}\",\"durationMs\":{},\"off\":{},\"vip\":{},\"fee\":{}",
        crate::media::json_escape(&id),
        crate::media::json_escape(&title),
        crate::media::json_escape(&artists),
        crate::media::json_escape(&album),
        duration,
        off,
        vip,
        fee,
    );
    if let Some(pl) = song.get("pl").and_then(Json::as_u64) {
        out.push_str(&format!(",\"pl\":{}", pl));
    }
    if let Some(level) = song.get("plLevel").and_then(Json::as_str) {
        out.push_str(&format!(",\"plLevel\":\"{}\"", crate::media::json_escape(level)));
    }
    out.push('}');
    Some(out)
}

fn songs(root: &Json) -> Vec<&Json> {
    let Some(Json::Arr(items)) = root.get("songs") else {
        return Vec::new();
    };
    items.iter().collect()
}

fn fallback_name(name: &str) -> &'static str {
    if name == "daily.json" {
        return "每日推荐";
    }
    let id = name.strip_prefix("toplist_").and_then(|s| s.strip_suffix(".json"));
    id.and_then(|id| lists::TOPLISTS.iter().find(|(known, _)| *known == id).map(|(_, title)| *title))
        .unwrap_or("")
}

fn page_json_from_document(doc: &Document, offset: usize, limit: usize) -> String {
    let all = songs(&doc.root);
    let safe_limit = limit.min(16);
    let mut out = format!(
        "{{\"state\":\"ready\",\"name\":\"{}\",\"total\":{},\"songs\":[",
        crate::media::json_escape(&{
            let title = string_field(&doc.root, "name");
            if title.is_empty() {
                String::from(fallback_name(&doc.name))
            } else {
                title
            }
        }),
        all.len(),
    );
    let mut first = true;
    for song in all.iter().skip(offset).take(safe_limit) {
        if let Some(raw) = song_json(song) {
            if !first {
                out.push(',');
            }
            first = false;
            out.push_str(&raw);
        }
    }
    out.push_str("]}");
    out
}

/// Return at most `limit` songs.  The full document never crosses the bridge.
///
/// If the document is already parsed, this returns a bounded window directly;
/// otherwise it queues the file for the native worker and returns `loading`.
pub fn page_json(file_name: &str, offset: usize, limit: usize) -> String {
    let safe_limit = limit.min(16);
    if let Some(raw) = page_cache(file_name, offset, safe_limit) {
        return raw;
    }
    /* Once the document is already parsed, producing at most 16 bounded song
     * records is cheap and deterministic.  Do it here instead of waiting for
     * a queued worker command: page turns must not remain loading behind old
     * windows.  Full file IO and JSON parsing still stay on the worker. */
    if let Some(doc) = document(file_name) {
        return page_json_from_document(&doc, offset, safe_limit);
    }
    request_page(file_name, offset, safe_limit);
    String::from("{\"state\":\"loading\",\"name\":\"\",\"total\":0,\"songs\":[]}")
}

/// Return only ids for queue navigation.  Metadata remains native-owned and
/// is fetched by page windows, so a large playlist never becomes a JS object.
pub fn ids_json(name: &str) -> String {
    request(name);
    let Some(doc) = document(name) else {
        return String::from("[]");
    };
    let mut out = String::from("[");
    let mut first = true;
    for song in songs(&doc.root) {
        let id = song_id(song);
        if id.is_empty() {
            continue;
        }
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&format!("\"{}\"", crate::media::json_escape(&id)));
    }
    out.push(']');
    out
}

/// Menu data is deliberately small; it contains counts and names, never song
/// arrays.  The UI can redraw menus without reading any files itself.
pub fn menu_json(kind: &str) -> String {
    let file = match kind {
        "discover" => "discover.json",
        "charts" => "daily.json",
        "account" => "account_playlists.json",
        _ => return String::from("{\"state\":\"failed\"}"),
    };
    request(file);
    let Some(main) = document(file) else {
        return String::from("{\"state\":\"loading\"}");
    };
    if kind == "discover" || kind == "account" {
        /* The on-disk schemas differ: discover uses `playlists`, while
         * account_playlists.json uses `list`.  Normalize only the bridge
         * response; do not use the response key to read the file. */
        let input_key = if kind == "discover" { "playlists" } else { "list" };
        let Some(Json::Arr(items)) = main.root.get(input_key) else {
            return String::from(r#"{"state":"ready","playlists":[]}"#);
        };
        let mut out = String::from(r#"{"state":"ready","playlists":["#);
        let mut first = true;
        for item in items {
            let id = string_field(item, "id");
            let name = string_field(item, "name");
            if id.is_empty() || name.is_empty() {
                continue;
            }
            if !first {
                out.push(',');
            }
            first = false;
            out.push_str(&format!(
                r#"{{"id":"{}","name":"{}","count":{}}}"#,
                crate::media::json_escape(&id),
                crate::media::json_escape(&name),
                number_field(item, "count"),
            ));
        }
        out.push_str("]}");
        return out;
    }

    let daily_count = songs(&main.root).len();
    let mut out = format!(r#"{{"state":"ready","dailyCount":{},"charts":["#, daily_count);
    let mut first = true;
    for &(id, fallback) in lists::TOPLISTS {
        let name = format!("toplist_{id}.json");
        request(&name);
        let Some(doc) = document(&name) else { continue };
        if !first {
            out.push(',');
        }
        first = false;
        let title = string_field(&doc.root, "name");
        out.push_str(&format!(
            r#"{{"id":"{}","name":"{}","count":{}}}"#,
            id,
            crate::media::json_escape(if title.is_empty() { fallback } else { title.as_str() }),
            songs(&doc.root).len(),
        ));
    }
    out.push_str("]}");
    out
}

/// A compact version string is enough for JS to know that a menu/page should
/// be sampled again.  No filesystem access happens on the guest thread.
pub fn version_string() -> String {
    format!("{}", version())
}

/// Called by list writers after an atomic file replacement.
pub fn notify_written(name: &str) {
    request(name);
}
