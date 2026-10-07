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
use std::sync::{Mutex, Once};
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

enum Command {
    Refresh(String),
}

static START: Once = Once::new();
static COMMANDS: Mutex<Option<Sender<Command>>> = Mutex::new(None);
static WATCHED: Mutex<Vec<String>> = Mutex::new(Vec::new());
static DOCUMENTS: Mutex<Vec<Document>> = Mutex::new(Vec::new());
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
    let Ok(mut docs) = DOCUMENTS.lock() else {
        return;
    };
    if let Some(old) = docs.iter_mut().find(|d| d.name == document.name) {
        if old.stamp == document.stamp {
            return;
        }
        *old = document;
    } else {
        docs.push(document);
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

pub fn version() -> u64 {
    start();
    VERSION.load(Ordering::Acquire)
}

fn document(name: &str) -> Option<Document> {
    DOCUMENTS
        .lock()
        .ok()
        .and_then(|docs| docs.iter().find(|d| d.name == name).cloned())
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

/// Return at most `limit` songs.  The full document never crosses the bridge.
pub fn page_json(file_name: &str, offset: usize, limit: usize) -> String {
    request(file_name);
    let Some(doc) = document(file_name) else {
        return String::from("{\"state\":\"loading\",\"name\":\"\",\"total\":0,\"songs\":[]}");
    };
    let all = songs(&doc.root);
    let safe_limit = limit.min(8);
    let mut out = format!(
        "{{\"state\":\"ready\",\"name\":\"{}\",\"total\":{},\"songs\":[",
        crate::media::json_escape(&{
            let title = string_field(&doc.root, "name");
            if title.is_empty() {
                String::from(fallback_name(file_name))
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
