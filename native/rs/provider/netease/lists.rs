//! `ux0:/data/yunyin/list/`：把在线清单**按类别落成 JSON 文件**。
//!
//! 设计：**文件就是界面读的数据源**。每次打开应用后台刷一遍（覆盖文件），
//! 界面直接读文件 —— 一进应用就有内容、没网也能看上次的、每类一个文件互不干扰。
//!
//! ```text
//! discover.json           {"at":ts,"playlists":[{id,name,count}]}     发现页=热门推荐
//! daily.json              {"at":ts,"songs":[{id,title,artists,album,durationMs,off}]}  每日推荐（需登录）
//! account_playlists.json  {"at":ts,"list":[{id,name,count}]}          我的歌单（需登录）
//! toplist_<榜单id>.json    {"at":ts,"name":"热歌榜","songs":[…]}        榜单页：一个榜一个文件
//! playlist_<歌单id>.json   {"at":ts,"name":"…","songs":[…]}           打开过的歌单/榜单落盘
//!
//! 歌曲节点的 `off` = 1 表示这首在网易云是**下架/无版权**（官方 App 里标灰的那种）：
//! 界面上照样列出来、但按不动、也不进播放队列。
//! ```
//!
//! 榜单与歌单文件里只有**元数据**（id/歌名/歌手/专辑/时长），没有播放地址 ——
//! 地址会过期，播放时仍走 `netease:<id>` 现场解析。

use super::mine;
use crate::media::platform::log;
use crate::media::platform::time::now_ms;
use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;

pub const LIST_DIR: &str = "ux0:/data/yunyin/list";

/// 网易云的"排行榜"本身也是歌单，id 是固定的 —— 榜单页就按这个顺序列。
pub const TOPLISTS: &[(&str, &str)] = &[
    ("3778678", "热歌榜"),
    ("19723756", "飙升榜"),
    ("3779629", "新歌榜"),
    ("2884035", "原创榜"),
];

fn path(name: &str) -> String {
    format!("{LIST_DIR}/{name}")
}

pub fn ensure_dir() {
    let _ = std::fs::create_dir_all(LIST_DIR);
}

/// 读一个清单文件。只允许简单文件名（挡掉 `../` 这类路径穿越）。
pub fn read(name: &str) -> String {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return String::new();
    }
    std::fs::read_to_string(path(name)).unwrap_or_default()
}

/// 清单文件的"版本戳"：`"大小,修改时间ms"`。
///
/// 界面每秒轮询时只问这个（stat 一次，几微秒），**变了才读整份文件** ——
/// Vita 上（尤其 SD2Vita）整文件读比 stat 贵得多，榜单文件动辄 20–50 KB。
/// 文件不存在返回空串；拿不到修改时间时返回 `"0,0"`，界面会退回"每次都读"。
pub fn stat(name: &str) -> String {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return String::new();
    }
    let Ok(meta) = std::fs::metadata(path(name)) else {
        return String::new();
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    format!("{},{}", meta.len(), mtime)
}

pub fn write_raw(name: &str, body: &str) {
    ensure_dir();
    /*
     * tmp + rename 原子替换：掉电 / 崩溃 / LiveArea 强杀时不会留下半截 JSON
     * （下次启动 JSON parse 失败，界面就一直"同步中"）。
     * 只多一次 rename，比"重新下载一整张榜单"便宜得多。
     */
    let tmp = format!("{}.tmp", path(name));
    if std::fs::write(&tmp, body).is_ok() {
        if std::fs::rename(&tmp, path(name)).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        /*
         * 记下"这个名字被写过"：界面靠它做到**事件驱动**，不再每秒去 stat 文件。
         *
         * 为什么必须这样：真机实测一次 `listStat` 要 **35~86ms**（SD 卡 metadata IO），
         * 而帧循环每秒要问 6 个文件 —— 那就是每秒 200ms 以上的卡顿来源。
         * 文件本来就是我们自己写的，写的时候顺手报一声，界面只读"刚变过的"那几份。
         */
        if let Ok(mut g) = TOUCHED.lock() {
            if !g.iter().any(|n| n == name) {
                g.push(String::from(name));
            }
        }
        /* Parse the new snapshot on the native catalog worker.  The guest
         * only receives bounded menu/page views through bridge.rs. */
        crate::media::catalog::publish(name, body);
        log::append(&format!(
            "list: snapshot_written name={} bytes={} stat={}",
            name,
            body.len(),
            stat(name)
        ));
    }
}

/*
 * "自上次问过之后，哪些清单文件被我们写过"。
 *
 * 只增不删，最多几十个名字（一张歌单一个），取走即清空。界面每 0.5 秒问一次，
 * 拿到空串就完全不做任何文件操作 —— 这才是"事件驱动"，而不是"每帧问一遍变没变"。
 */
static TOUCHED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// 取走（并清空）"被写过的清单文件名"，逗号分隔；没有变化时返回空串。
pub fn take_touched() -> String {
    let Ok(mut g) = TOUCHED.lock() else {
        return String::new();
    };
    if g.is_empty() {
        return String::new();
    }
    let joined = g.join(",");
    g.clear();
    joined
}

/// 歌单/榜单落盘：界面打开某个 id 时（原生拉到数据后）调用。
pub fn write_playlist(id: &str, name: &str, songs_json: &str) {
    if id.is_empty() {
        return;
    }
    write_raw(
        &format!("playlist_{id}.json"),
        &format!(
            "{{\"at\":{},\"name\":\"{}\",\"songs\":{}}}",
            now_ms(),
            super::json::escape(name),
            songs_json
        ),
    );
}

/// 一次完整同步：热门推荐 → 每日推荐 → 我的歌单 → 四个榜单。
/// 单项失败只记一行日志并**保留旧文件**（下次启动再补），绝不写空文件覆盖。
pub fn sync_once() {
    ensure_dir();
    /* 7 件事：热门推荐 / 每日推荐 / 我的歌单 / 四个榜单。界面据此显示 "3/7"。 */
    super::prog_begin(1, 3 + TOPLISTS.len());
    let session = super::current_session();
    log::append(&format!(
        "list: sync_begin session_logged_in={} provider_logged_in={}",
        session.is_logged_in(),
        super::is_logged_in()
    ));
    let mut post = crate::media::net::http::VitaPost;
    let mk_secret =
        || super::crypto::secret_key_from_entropy(crate::media::platform::time::entropy64());

    /* 发现页：热门推荐（推荐歌单） */
    {
        let secret = mk_secret();
        match mine::recommend_playlists(&secret, &mut post, &session) {
            Ok(list) => {
                write_raw(
                    "discover.json",
                    &format!(
                        "{{\"at\":{},\"playlists\":{}}}",
                        now_ms(),
                        mine::playlists_json(&list)
                    ),
                );
                log::append(&format!("list: discover.json 已更新（{} 张推荐歌单）", list.len()));
            }
            Err(e) => log::append(&format!("list: 热门推荐失败 {e:?}")),
        }
        super::prog_step();
    }

    /* 每日推荐（需登录）：未登录时不发请求，等登录成功后的强制同步。 */
    {
        if session.is_logged_in() && super::is_logged_in() {
            let secret = mk_secret();
            match mine::daily_songs(&secret, &mut post, &session) {
                Ok(songs) => {
                    write_raw(
                        "daily.json",
                        &format!(
                            "{{\"at\":{},\"songs\":{}}}",
                            now_ms(),
                            mine::cloud_songs_json(&songs)
                        ),
                    );
                    log::append(&format!("list: daily.json 已更新（{} 首）", songs.len()));
                }
                Err(e) => log::append(&format!("list: 每日推荐失败 {e:?}")),
            }
        } else {
            log::append("list: daily.json 跳过（未登录，登录后懒刷新）");
        }
        super::prog_step();
    }

    /* 我的歌单（需登录）：未登录时不问 uid，也不产生 Auth 失败。 */
    {
        let session_logged_in = session.is_logged_in();
        let provider_logged_in = super::is_logged_in();
        log::append(&format!(
            "list: account_begin session_logged_in={} provider_logged_in={}",
            session_logged_in, provider_logged_in
        ));
        if session_logged_in && provider_logged_in {
            let secret = mk_secret();
            match mine::my_playlists(&secret, &mut post, &session) {
                Ok(list) => {
                    log::append(&format!("list: account_api_ok count={}", list.len()));
                    write_raw(
                        "account_playlists.json",
                        &format!(
                            "{{\"at\":{},\"list\":{}}}",
                            now_ms(),
                            mine::playlists_json(&list)
                        ),
                    );
                    log::append(&format!("list: account_playlists.json 已更新（{} 张）", list.len()));
                }
                Err(e) => log::append(&format!(
                    "list: account_api_error error={e:?} provider_logged_in_after={}",
                    super::is_logged_in()
                )),
            }
        } else {
            log::append("list: account_playlists.json 跳过（未登录，登录后懒刷新）");
        }
        super::prog_step();
    }

    /* 四个榜单：每个榜一个文件 */
    for (id, fallback_name) in TOPLISTS.iter() {
        let secret = mk_secret();
        match mine::playlist_tracks(id, &secret, &mut post, &session) {
            Ok((name, songs)) => {
                let title = if name.is_empty() {
                    String::from(*fallback_name)
                } else {
                    name
                };
                write_raw(
                    &format!("toplist_{id}.json"),
                    &format!(
                        "{{\"at\":{},\"name\":\"{}\",\"songs\":{}}}",
                        now_ms(),
                        super::json::escape(&title),
                        mine::cloud_songs_json(&songs)
                    ),
                );
                log::append(&format!("list: toplist_{id}.json 已更新（{title}，{} 首）", songs.len()));
            }
            Err(e) => log::append(&format!("list: 榜单 {fallback_name} 失败 {e:?}")),
        }
        /* 一次同步连着打五个请求，间隔一下别把接口打爆 */
        thread::sleep(std::time::Duration::from_millis(300));
        super::prog_step();
    }
    super::prog_end();
}

static STARTED_MS: AtomicU64 = AtomicU64::new(0);
static RUNNING: AtomicBool = AtomicBool::new(false);
/// 同步跑着的时候又来人要求"强制刷"（典型：用户刚扫码登录成功）——
/// 记下来，这一轮跑完立刻再跑一轮，别把登录这次刷新吞掉。
static PENDING: AtomicBool = AtomicBool::new(false);

/// 后台同步（JS 每次启动调一次）。
///
/// * `force = false`：进程内 10 分钟内重复调用直接忽略（启动那次用）。
/// * `force = true`：跳过 TTL —— 登录成功后 / 用户手动点刷新时用
///   （登录前"每日推荐 / 我的歌单"必定是 Auth 失败，不强制刷就永远是空的）。
///
/// 单项失败不影响其它项，界面照样能读上一次的文件。
pub fn sync_background(force: bool) {
    let now = now_ms();
    let running = RUNNING.load(Ordering::Acquire);
    let age_ms = now.saturating_sub(STARTED_MS.load(Ordering::Acquire));
    log::append(&format!(
        "list: sync_request force={} running={} age_ms={}",
        force, running, age_ms
    ));
    if running {
        if force {
            PENDING.store(true, Ordering::Release);
            log::append("list: sync_request queued_pending=true");
        }
        return;
    }
    if !force && age_ms < 10 * 60 * 1000 {
        log::append("list: sync_request skipped_ttl=true");
        return;
    }
    STARTED_MS.store(now, Ordering::Release);
    RUNNING.store(true, Ordering::Release);
    let spawned = thread::Builder::new()
        .name("yunyin-net-list".into())
        .stack_size(128 * 1024)
        .spawn(|| {
            sync_once();
            if PENDING.swap(false, Ordering::AcqRel) {
                sync_once();
            }
            RUNNING.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        RUNNING.store(false, Ordering::Release);
        log::append("list: sync_request spawn_failed=true");
    } else {
        log::append("list: sync_request spawned=true");
    }
}
