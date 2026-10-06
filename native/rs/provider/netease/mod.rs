//! 网易云音乐 Provider（Phase 3，任务书 §44–§52）。
//!
//! 当前状态：匿名 weapi 链已打通 —— `resolve_song()` 会用固定公钥加密请求、
//! 解析回播放地址，并在内存里缓存到 URL 过期。
//!
//! 职责（只有这些）：
//!   1. 拿歌曲 ID + 音质，向 API 要一个可播放的 URL；
//!   2. 带上会话 Cookie、Referer 和桌面浏览器 User-Agent（§48）；
//!   3. 汇报 `AudioInfo`：URL、大小、码率、格式提示、过期时间；
//!   4. 在**内存里**缓存 URL，过期后重新解析（§50/§51/§52）。
//!
//! 它绝对不能：自己开 socket、解码任何东西、或在没有明确开关的情况下把 Cookie 写进磁盘。
#![allow(dead_code)]

pub mod account;
pub mod api;
pub mod crypto;
pub mod detail;
pub mod json;
pub mod lists;
pub mod login;
pub mod mine;
pub mod resolve;

#[cfg(test)]
pub mod test_support;

use super::{AudioInfo, MusicProvider, ProviderError, Quality};
use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use alloc::vec::Vec;
use std::sync::Mutex;

pub struct NetEaseProvider {
    session: account::Session,
}

impl Default for NetEaseProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl NetEaseProvider {
    pub fn new() -> Self {
        Self {
            session: account::Session::anonymous(),
        }
    }

    pub fn session(&self) -> &account::Session {
        &self.session
    }
}

impl MusicProvider for NetEaseProvider {
    fn name(&self) -> &'static str {
        "netease"
    }

    fn resolve(&self, song_id: &str, quality: Quality) -> Result<AudioInfo, ProviderError> {
        resolve_with_session(&self.session, song_id, quality)
    }
}

/*
 * URL 缓存常驻（进程生命周期），按 (song_id, quality) 索引；不落盘（§51）。
 *
 * 锁的规矩：**绝不拿着缓存锁等网络**。解析线程照常先拿锁（命中就直接返回），
 * 但如果另一个解析正在跑（用户连续切歌），这次就退化成"用一次性本地缓存"，
 * 不让新歌排在旧请求后面 —— 最多丢掉一次缓存，不会多等十秒。
 */
static CACHE: Mutex<Option<resolve::UrlCache>> = Mutex::new(None);

fn resolve_with_session(
    session: &account::Session,
    song_id: &str,
    quality: Quality,
) -> Result<AudioInfo, ProviderError> {
    let secret = crypto::secret_key_from_entropy(crate::media::platform::time::entropy64());
    let now_ms = crate::media::platform::time::now_ms();
    let mut post = crate::media::net::http::VitaPost;
    if let Ok(mut guard) = CACHE.try_lock() {
        let cache = guard.get_or_insert_with(resolve::UrlCache::default);
        return resolve::resolve(
            song_id, quality, &secret, &mut post, session, cache, now_ms,
        );
    }
    let mut scratch = resolve::UrlCache::default();
    resolve::resolve(
        song_id, quality, &secret, &mut post, session, &mut scratch, now_ms,
    )
}

/// 预取（预热）一首歌的播放地址：**解析并塞进 URL 缓存**，不播放、不开流。
///
/// 用途：当前歌还在放的时候，把"下一首"的地址先解析好 —— 用户按下一首时
/// 直接命中缓存，省掉一次加密 POST + CDN 首包（真机 1.8–2.7 秒）。
/// 只做一次、只做一个目标（`MAX_PREFETCH_TARGETS = 1`），失败静默。
pub fn preload_song_url(song_id: &str, level: Option<&str>) {
    if song_id.is_empty() || !song_id.bytes().all(|b| b.is_ascii_digit()) {
        return;
    }
    /*
     * **同时只允许一个预取**（ClouDS 的 MAX_ACTIVE = 1）：
     * 快速连按切歌时，以前每按一次就 spawn 一个线程，四五个线程同时打网络 + 抢
     * URL 缓存锁 —— 弱机上等于自己给自己造拥堵。
     */
    static PREFETCH_BUSY: core::sync::atomic::AtomicBool =
        core::sync::atomic::AtomicBool::new(false);
    if PREFETCH_BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    let gen = SESSION_EPOCH.load(Ordering::Acquire);
    let id = String::from(song_id);
    let quality = match level {
        Some(l) if !l.is_empty() => quality_from_level(l),
        _ => Quality::Low,
    };
    let spawned = std::thread::Builder::new()
        .name("yunyin-prefetch".into())
        .stack_size(96 * 1024)
        .spawn(move || {
            let session = current_session();
            let started = crate::media::platform::time::now_ms();
            match resolve_with_session(&session, &id, quality) {
                Ok(info) => {
                    /*
                     * 预取期间换过账号（登录/退出）就丢掉这次结果：
                     * URL 是服务端按账号算的，留着它下次会拿旧账号的地址去播。
                     * 光清缓存不够 —— 旧线程晚一点结束就又把旧的写回去了。
                     */
                    if gen != SESSION_EPOCH.load(Ordering::Acquire) {
                        invalidate_song_url(&id);
                        crate::media::platform::log::append(&format!(
                            "prefetch: 会话已变，丢弃预热结果 id={id}"
                        ));
                    } else {
                        crate::media::platform::log::append(&format!(
                            "prefetch: 已预热 id={id}（{}ms）",
                            crate::media::platform::time::now_ms().saturating_sub(started)
                        ));
                    }
                    let _ = info;
                }
                Err(e) => crate::media::platform::log::append(&format!(
                    "prefetch: 预热失败 id={id} {e:?}（忽略）"
                )),
            }
            PREFETCH_BUSY.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        PREFETCH_BUSY.store(false, Ordering::Release);
        crate::media::platform::log::append("prefetch: 线程建不出来（忽略）");
    }
}

/// 真机播放 `netease:<id>` 在线曲目时走这里（匿名会话）。
pub fn resolve_song(song_id: &str, quality: Quality) -> Result<AudioInfo, ProviderError> {
    let session = current_session();
    resolve_with_session(&session, song_id, quality)
}

/// 丢掉某首歌的地址缓存。
///
/// 什么时候用：CDN 回了 403/404/410（链接失效或被拒）—— 下一轮重新解析会拿一条
/// 新地址。真机上"切歌之后没声音还卡着"就是拿到了一条已经被拒的地址。
pub fn invalidate_song_url(song_id: &str) {
    if let Ok(mut guard) = CACHE.try_lock() {
        if let Some(cache) = guard.as_mut() {
            cache.invalidate(song_id);
        }
    }
}

/// 清掉所有"跟账号有关"的缓存：URL 表 + 音质提示表。
///
/// 为什么要清：播放 URL 和 `plLevel` 都是**服务端按当前账号**算出来的。
/// 换个账号（登录 / 退出 / 重新扫码）之后还留着上一个账号的结果，
/// 轻则多一次 403 重试，重则按错误的音质档去请求。
pub fn clear_account_caches() {
    /* 记一代：预取线程醒来发现代次变了就丢掉自己的结果（见 preload_song_url）。 */
    SESSION_EPOCH.fetch_add(1, Ordering::AcqRel);
    if let Ok(mut guard) = CACHE.try_lock() {
        if let Some(cache) = guard.as_mut() {
            cache.clear();
        }
    }
    if let Ok(mut g) = LEVEL_HINTS.lock() {
        g.clear();
    }
}

/// 账号代次：登录 / 退出时 +1。所有"按账号算出来的东西"（URL、音质档、预取结果）
/// 都挂在它下面，晚到的旧结果一律作废。
static SESSION_EPOCH: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/* ---------------------------------------------------------------- 登录 -- */
/*
 * 扫码登录的状态机（Phase 4）。
 *
 * 规矩跟在线播放一样：HTTP 一律在后台线程里跑，界面只读状态 —— 绝不让按键
 * 处理等网络。JS 侧在登录页里定期调一次 `login_tick()` 推状态机即可。
 */
#[derive(Clone, Debug)]
pub enum LoginState {
    Idle,
    Starting,
    Waiting {
        key: String,
        /// 创建二维码那次响应发的会话 Cookie —— 轮询必须带上同一个会话。
        cookie: Option<String>,
        scanned: bool,
    },
    Confirmed,
    Expired,
    Failed(String),
}

static LOGIN: Mutex<LoginState> = Mutex::new(LoginState::Idle);
static LOGIN_POLLING: AtomicBool = AtomicBool::new(false);
/// 刚被刷新掉的旧码（手机可能扫的是它）：刷新后继续追一段时间，确认了也算。
static PREV: Mutex<Option<(String, String)>> = Mutex::new(None);
static SESSION: Mutex<Option<account::Session>> = Mutex::new(None);
static SESSION_LOADED: AtomicBool = AtomicBool::new(false);
static REMEMBER: AtomicBool = AtomicBool::new(false);

/// 当前会话：登录过就是登录会话，否则匿名；首次调用时尝试读回保存的会话。
pub fn current_session() -> account::Session {
    if let Ok(mut slot) = SESSION.lock() {
        if let Some(s) = slot.as_ref() {
            return s.clone();
        }
        if !SESSION_LOADED.swap(true, Ordering::AcqRel) {
            let saved = account::Session::load_saved();
            if saved.is_logged_in() {
                /*
                 * 一定要记一行来源：卡里的 `store/session` 是"上次登录留下的"，
                 * 卸载应用不会删掉它。不写日志的话，用户重装后会看到界面自己
                 * 从"未登录"翻成"已登录"，很容易以为应用在装假。
                 */
                crate::media::platform::log::append(
                    "login: 使用卡里保存的会话（store/session，上次登录留下的；已失效会自动退回未登录）",
                );
                *slot = Some(saved.clone());
                return saved;
            }
            /* 卡里手填的 Cookie（模拟器上扫码/TLS 走不通时的唯一登录路子） */
            let manual = account::Session::load_manual_cookie();
            if manual.is_logged_in() {
                crate::media::platform::log::append(
                    "login: 使用卡里 cookie.txt 的会话（手动 Cookie 登录）",
                );
                *slot = Some(manual.clone());
                return manual;
            }
        }
    }
    account::Session::anonymous()
}

fn set_login(state: LoginState) {
    if let Ok(mut g) = LOGIN.lock() {
        *g = state;
    }
}

/// 账号接口明确回答"未登录"时，把内存里的会话丢掉。
///
/// 为什么要丢：卡里手填的 `cookie.txt` 过期后，服务器对每个账号接口都回
/// `account=null` / `code=301`，但本地还以为"已登录"——界面一直显示已登录、
/// 歌单却全都同步失败（日志里是 `Auth("没拿到账号 uid")`），用户根本看不出
/// 该重新扫码。丢掉之后登录状态回到 false，扫码入口立刻可用。
///
/// 只动内存，不删卡里的 `cookie.txt`：那份文件是用户自己放的，
/// 万一他想换一份新的直接覆盖即可。
pub fn session_expired() {
    let logged = SESSION
        .lock()
        .ok()
        .and_then(|s| s.as_ref().map(|x| x.is_logged_in()))
        .unwrap_or(false);
    if !logged {
        return;
    }
    crate::media::platform::log::append(
        "login: 会话已失效（服务器回未登录），已退回未登录状态并清掉卡里 store/session；重新扫码即可",
    );
    if let Ok(mut slot) = SESSION.lock() {
        *slot = Some(account::Session::anonymous());
    }
    /*
     * 卡里那份也要清掉：不清的话下次启动又会把这份死会话当成"已登录"，
     * 界面先显示已登录、过一会儿再翻成未登录 —— 看起来就像应用在装假。
     * 手填的 `cookie.txt` 是用户自己的文件，一律不动。
     */
    account::Session::clear_saved();
}

/// 码尾号：日志和屏幕用它核对"扫的是不是同一张码"。
fn key_tail(key: &str) -> String {
    let n = key.len();
    String::from(&key[n.saturating_sub(4)..])
}

/// 只在"还是同一个 unikey"时改写状态，避免刷新二维码后被旧轮询结果污染。
fn set_login_if_key(key: &str, next: LoginState) {
    if let Ok(mut g) = LOGIN.lock() {
        if let LoginState::Waiting { key: k, .. } = &*g {
            if k == key {
                *g = next;
            }
        }
    }
}

/* ---------------------------------------------------------------- 同步进度 -- */
/*
 * 界面要显示"正在同步清单 3/7"这种进度：以前只有一句"同步中…"，
 * 用户分不清是慢还是卡死（真机反馈过）。
 *
 * 只记"当前正在跑的那件事"：清单同步和单个歌单取曲目不会同时需要给用户看，
 * 后来的任务直接覆盖前一个，不做并发叠加。
 */
static PROG_KIND: AtomicU8 = AtomicU8::new(0); /* 0=空闲 1=清单 2=歌单曲目 */
static PROG_DONE: AtomicUsize = AtomicUsize::new(0);
static PROG_TOTAL: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn prog_begin(kind: u8, total: usize) {
    PROG_TOTAL.store(total, Ordering::Release);
    PROG_DONE.store(0, Ordering::Release);
    PROG_KIND.store(kind, Ordering::Release);
}

pub(crate) fn prog_step() {
    PROG_DONE.fetch_add(1, Ordering::AcqRel);
}

pub(crate) fn prog_end() {
    PROG_KIND.store(0, Ordering::Release);
    PROG_DONE.store(0, Ordering::Release);
    PROG_TOTAL.store(0, Ordering::Release);
}

/// 给界面的进度 JSON：`{"kind":"lists","done":3,"total":7}`；空闲时 kind 为空串。
pub fn sync_progress_json() -> String {
    /*
     * 有请求正在收正文时，**优先报下载百分比**：那才是真正在等的那一段
     * （歌单详情几 MB，收完才谈得上解析）。分母是服务器的 Content-Length，
     * 收不到就退回下面的"步数"。
     */
    if let Some((got, total)) = crate::media::net::http::download_progress() {
        let scope = match PROG_KIND.load(Ordering::Acquire) {
            1 => "lists",
            2 => "tracks",
            _ => "",
        };
        return format!(
            "{{\"kind\":\"download\",\"scope\":\"{}\",\"done\":{},\"total\":{}}}",
            scope, got, total
        );
    }
    let kind = match PROG_KIND.load(Ordering::Acquire) {
        1 => "lists",
        2 => "tracks",
        _ => "",
    };
    let done = PROG_DONE.load(Ordering::Acquire);
    let total = PROG_TOTAL.load(Ordering::Acquire);
    format!(
        "{{\"kind\":\"{}\",\"done\":{},\"total\":{}}}",
        kind,
        if kind.is_empty() { 0 } else { done },
        if kind.is_empty() { 0 } else { total }
    )
}

pub fn login_start() {
    {
        let Ok(mut g) = LOGIN.lock() else { return };
        if matches!(*g, LoginState::Starting) {
            return;
        }
        if let LoginState::Waiting { key, cookie, .. } = &*g {
            if let Ok(mut prev) = PREV.lock() {
                *prev = Some((key.clone(), cookie.clone().unwrap_or_default()));
            }
        }
        *g = LoginState::Starting;
    }
    crate::media::platform::log::append("login: 开始获取二维码");
    let spawned = std::thread::Builder::new()
        .name("yunyin-net-login".into())
        .stack_size(96 * 1024)
        .spawn(|| {
            let session = current_session();
            let secret =
                crypto::secret_key_from_entropy(crate::media::platform::time::entropy64());
            let mut post = crate::media::net::http::VitaPost;
            match login::start(&secret, &mut post, &session) {
                Ok(start) => {
                    let cookie_len = start.cookie.as_deref().map(str::len).unwrap_or(0);
                    let tail = key_tail(&start.unikey);
                    crate::media::platform::log::append(&format!(
                        "login: 二维码会话 cookie={cookie_len} 字节（轮询按电脑端验证过的做法：只带 os=pc）"
                    ));
                    crate::media::platform::log::append(&format!(
                        "login: 二维码就绪 key=…{tail}"
                    ));
                    set_login(LoginState::Waiting {
                        key: start.unikey,
                        /* 电脑对照实验证明：轮询不带 NMTID、只带 os=pc; appver=2.9.7 就能
                         * 正常走到 802/803；带上服务器刚发的 NMTID 反而可能让服务器认为
                         * "确认的是另一个客户端"。所以这里固定用电脑端验证过的那条。 */
                        cookie: Some(String::from("os=pc; appver=2.9.7")),
                        scanned: false,
                    })
                }
                Err(e) => {
                    crate::media::platform::log::append(&format!("login: 取二维码失败 {e:?}"));
                    set_login(LoginState::Failed(format!("{e:?}")))
                }
            }
        });
    if spawned.is_err() {
        set_login(LoginState::Failed(String::from("登录线程创建失败")));
    }
}

/// 推一次状态机：等待扫码/待确认时，后台补一次轮询（同一时刻只飞一个）。
pub fn login_tick() {
    let (key, cookie) = match LOGIN.lock().map(|g| g.clone()) {
        Ok(LoginState::Waiting { key, cookie, .. }) => (key, cookie),
        _ => return,
    };
    if LOGIN_POLLING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("yunyin-net-login-poll".into())
        .stack_size(96 * 1024)
        .spawn({
            let key = key.clone();
            let cookie = cookie.clone();
            move || {
                /* 轮询必须带上创建二维码时的那个会话 Cookie（扫码 = 授权这个会话）。 */
                let login_session = match cookie.as_deref() {
                    Some(c) if !c.is_empty() => account::Session::from_cookie(c),
                    _ => account::Session::anonymous(),
                };
                let secret =
                    crypto::secret_key_from_entropy(crate::media::platform::time::entropy64());
                let mut post = crate::media::net::http::VitaPost;
                let result = login::poll(&key, &secret, &mut post, &login_session);
                match &result {
                    Ok((status, _)) => {
                        crate::media::platform::log::append(&format!(
                            "login: 轮询 key=…{} {status:?}",
                            key_tail(&key)
                        ))
                    }
                    Err(e) => {
                        crate::media::platform::log::append(&format!("login: 轮询失败 {e:?}"))
                    }
                }
                match result {
                    Ok((login::QrStatus::WaitingScan, _)) => {
                        set_login_if_key(
                            &key,
                            LoginState::Waiting {
                                key: key.clone(),
                                cookie: cookie.clone(),
                                scanned: false,
                            },
                        );
                    }
                    Ok((login::QrStatus::WaitingConfirm, _)) => {
                        set_login_if_key(
                            &key,
                            LoginState::Waiting {
                                key: key.clone(),
                                cookie: cookie.clone(),
                                scanned: true,
                            },
                        );
                    }
                    Ok((login::QrStatus::Expired, _)) => {
                        set_login_if_key(&key, LoginState::Expired)
                    }
                    Ok((login::QrStatus::Confirmed, cookie)) => match cookie {
                        Some(c) if !c.is_empty() => {
                            crate::media::platform::log::append(
                                "login: 已确认（803），会话已建立",
                            );
                            let mut sess = current_session();
                            sess.adopt_cookie(&c);
                            /* 记一行长度：Cookie 被截断过（1 KiB 上限）时就是
                             * "登录成功但接口仍判未登录"，光看状态看不出来。 */
                            crate::media::platform::log::append(&format!(
                                "login: 会话 cookie={} 字节",
                                c.len()
                            ));
                            if let Ok(mut slot) = SESSION.lock() {
                                *slot = Some(sess.clone());
                            }
                            /* 换账号了：上一个账号的 URL / 音质提示不能再用 */
                            clear_account_caches();
                            if REMEMBER.load(Ordering::Acquire) {
                                sess.save();
                            }
                            set_login_if_key(&key, LoginState::Confirmed);
                        }
                        _ => set_login_if_key(
                            &key,
                            LoginState::Failed(String::from("登录成功但没拿到 Cookie")),
                        ),
                    },
                    Err(e) => {
                        /* 单次超时/抖动不判死：码还在有效期内，下一轮继续问。 */
                        crate::media::platform::log::append(&format!(
                            "login: 轮询失败（忽略，继续）{e:?}"
                        ));
                    }
                }
                /* 顺带追一眼刚被刷新掉的旧码：手机很可能扫的是它。 */
                if let Some((pkey, pcookie)) = PREV.lock().ok().and_then(|g| g.clone()) {
                    let psession = if pcookie.is_empty() {
                        account::Session::anonymous()
                    } else {
                        account::Session::from_cookie(&pcookie)
                    };
                    match login::poll(&pkey, &secret, &mut post, &psession) {
                        Ok((login::QrStatus::Confirmed, cookie)) => {
                            if let Some(c) = cookie.filter(|c| !c.is_empty()) {
                                crate::media::platform::log::append(
                                    "login: 旧码被确认，会话已建立",
                                );
                                let mut sess = current_session();
                                sess.adopt_cookie(&c);
                                if let Ok(mut slot) = SESSION.lock() {
                                    *slot = Some(sess.clone());
                                }
                                if REMEMBER.load(Ordering::Acquire) {
                                    sess.save();
                                }
                                if let Ok(mut p) = PREV.lock() {
                                    *p = None;
                                }
                                set_login(LoginState::Confirmed);
                            }
                        }
                        Ok((login::QrStatus::Expired, _)) => {
                            if let Ok(mut p) = PREV.lock() {
                                *p = None;
                            }
                        }
                        _ => {}
                    }
                }
                LOGIN_POLLING.store(false, Ordering::Release);
            }
        });
    if spawned.is_err() {
        LOGIN_POLLING.store(false, Ordering::Release);
    }
}

/// "记住登录"：打开才会把会话写到卡里（任务书 §48）。
pub fn login_remember(on: bool) {
    REMEMBER.store(on, Ordering::Release);
    if on {
        if let Ok(slot) = SESSION.lock() {
            if let Some(s) = slot.as_ref() {
                s.save();
            }
        }
    } else {
        account::Session::clear_saved();
    }
}

pub fn logout() {
    if let Ok(mut slot) = SESSION.lock() {
        *slot = None;
    }
    account::Session::clear_saved();
    set_login(LoginState::Idle);
    /* 退出登录：URL 表 / 音质提示表都是按上一个账号算的，全部作废。 */
    clear_account_caches();
}

pub fn is_logged_in() -> bool {
    current_session().is_logged_in()
}

/// 给 JS 的状态快照。
pub fn login_state_json() -> String {
    let snapshot = LOGIN.lock().map(|g| g.clone()).unwrap_or(LoginState::Idle);
    let (state, message, url) = match snapshot {
        LoginState::Idle => ("idle", String::new(), String::new()),
        LoginState::Starting => ("starting", String::new(), String::new()),
        LoginState::Waiting { key, scanned, .. } => (
            if scanned { "scanned" } else { "waiting" },
            String::new(),
            login::qr_content(&key),
        ),
        LoginState::Confirmed => ("confirmed", String::new(), String::new()),
        LoginState::Expired => ("expired", String::new(), String::new()),
        LoginState::Failed(m) => ("failed", m, String::new()),
    };
    format!(
        "{{\"state\":\"{}\",\"loggedIn\":{},\"message\":\"{}\",\"url\":\"{}\"}}",
        state,
        if is_logged_in() { "true" } else { "false" },
        json::escape(&message),
        json::escape(&url)
    )
}

/* ------------------------------------------------------------ 歌曲详情 -- */

static DETAIL: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
static DETAIL_FETCHING: AtomicBool = AtomicBool::new(false);

/// 给 JS 的在线歌曲详情 JSON；首次调用会在后台取一次并缓存。
/// 还没拿到时返回 `{}`，界面用"占位名 + 稍后刷新"的方式处理。
pub fn song_info_json(song_id: &str) -> String {
    if let Ok(cache) = DETAIL.lock() {
        if let Some((_, json)) = cache.iter().find(|(id, _)| id == song_id) {
            return json.clone();
        }
    }
    let valid_id = !song_id.is_empty() && song_id.bytes().all(|b| b.is_ascii_digit());
    if valid_id && !DETAIL_FETCHING.swap(true, Ordering::AcqRel) {
        let id = String::from(song_id);
        let spawned = std::thread::Builder::new()
            .name("yunyin-net-detail".into())
            .stack_size(96 * 1024)
            .spawn(move || {
                let session = current_session();
                let secret =
                    crypto::secret_key_from_entropy(crate::media::platform::time::entropy64());
                let mut post = crate::media::net::http::VitaPost;
                match detail::song_detail(&id, &secret, &mut post, &session) {
                    Ok(d) => {
                        crate::media::platform::log::append(&format!(
                            "detail: {} - {} - {}（{}ms）",
                            d.title, d.artists, d.album, d.duration_ms
                        ));
                        if let Ok(mut cache) = DETAIL.lock() {
                            cache.retain(|(k, _)| k != &id);
                            cache.push((id, detail::to_json(&d)));
                        }
                    }
                    Err(e) => {
                        crate::media::platform::log::append(&format!(
                            "detail: 取详情失败 {e:?}"
                        ));
                    }
                }
                DETAIL_FETCHING.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            DETAIL_FETCHING.store(false, Ordering::Release);
        }
    }
    String::from("{}")
}

/* -------------------------------------------- 批量详情（一次问多首） -- */

static BATCH_FETCHING: AtomicBool = AtomicBool::new(false);

/// 一次问多首的详情（`ids_csv` 逗号分隔）。只回**缓存里已经有**的那些：
/// `[{"id":"…","detail":{…}}]`；缺的会在后台一次补完，界面下次再问。
///
/// 为什么要有它：在线清单里只写 id 的条目，启动时列表会全是一模一样的
/// `[在线] 网络歌曲`；逐首打请求又太慢，所以一次批量问回来。
pub fn songs_info_json(ids_csv: &str) -> String {
    /*
     * **原生侧也限流**：最多收 8 个 ID。
     *
     * JS 那边已经 `slice(0, 8)` 了，但弱机优化讲究"上层限流 + 原生再兜一层" ——
     * 只要有人（以后的搜索 / 批量导入 / 别的界面）递过来 1000 个 ID，
     * 这里就会生成 1000 个的请求 JSON + 一份大响应。这里是最后一道闸。
     */
    const MAX_DETAIL_BATCH: usize = 8;
    let mut want: Vec<String> = Vec::new();
    for raw in ids_csv.split(',') {
        let id = raw.trim();
        if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) {
            want.push(String::from(id));
            if want.len() >= MAX_DETAIL_BATCH {
                break;
            }
        }
    }
    if want.is_empty() {
        return String::from("[]");
    }

    let mut out = String::from("[");
    let mut missing: Vec<String> = Vec::new();
    let mut first = true;
    if let Ok(cache) = DETAIL.lock() {
        for id in &want {
            match cache.iter().find(|(k, _)| k == id) {
                Some((_, json)) => {
                    if !first {
                        out.push(',');
                    }
                    first = false;
                    out.push_str(&format!(
                        "{{\"id\":\"{}\",\"detail\":{}}}",
                        json::escape(id),
                        json
                    ));
                }
                None => missing.push(id.clone()),
            }
        }
    }
    out.push(']');

    if !missing.is_empty() && !BATCH_FETCHING.swap(true, Ordering::AcqRel) {
        let spawned = std::thread::Builder::new()
            .name("yunyin-net-songs".into())
            .stack_size(96 * 1024)
            .spawn(move || {
                let session = current_session();
                let secret =
                    crypto::secret_key_from_entropy(crate::media::platform::time::entropy64());
                let mut post = crate::media::net::http::VitaPost;
                match detail::songs_detail(&missing, &secret, &mut post, &session) {
                    Ok(list) => {
                        crate::media::platform::log::append(&format!(
                            "detail: 批量取到 {} 首",
                            list.len()
                        ));
                        if let Ok(mut cache) = DETAIL.lock() {
                            /* **按返回节点自带的 id 入库**，不靠"返回顺序 == 请求顺序"：
                             * 服务器少给一首或换了顺序，用 zip 就会把元数据串到别的歌上。 */
                            for d in list.iter() {
                                if d.id.is_empty() {
                                    continue;
                                }
                                cache.retain(|(k, _)| k != &d.id);
                                cache.push((d.id.clone(), detail::to_json(d)));
                            }
                            /* 上限 256 条（FIFO）：以前是无限增长，扫过的歌全留在内存里。 */
                            const DETAIL_MAX: usize = 256;
                            while cache.len() > DETAIL_MAX {
                                cache.remove(0);
                            }
                        }
                    }
                    Err(e) => {
                        crate::media::platform::log::append(&format!(
                            "detail: 批量取详情失败 {e:?}"
                        ));
                    }
                }
                BATCH_FETCHING.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            /* 线程建不出来就放开标志，下次调用再试 */
            BATCH_FETCHING.store(false, Ordering::Release);
        }
    }
    out
}

/// 某张歌单的歌曲：`{"state":"…","name":"…","songs":[…]}`。
pub fn playlist_tracks_json(playlist_id: &str) -> String {
    static STATE: AtomicU8 = AtomicU8::new(0);
    /* (id, name, songs_json, 拿到的时间 ms) */
    static CACHE: Mutex<Vec<(String, String, String, u64)>> = Mutex::new(Vec::new());
    static ERR: Mutex<(String, String)> = Mutex::new((String::new(), String::new()));
    /*
     * 正在拉的歌单 id 列表（不是全局一个 bool）。
     *
     * 为什么：以前 A 在拉的时候，B 打开会看到 FETCHING=true 就直接不拉自己 ——
     * 两个歌单共用一个状态。现在按 id 记，**并发仍然全局 1**
     * （`MAX_PLAYLIST_REQUEST = 1`，同一时刻只允许一个在网）。
     */
    static INFLIGHT: Mutex<Vec<String>> = Mutex::new(Vec::new());
    fn claim(id: &str) -> bool {
        let Ok(mut g) = INFLIGHT.lock() else {
            return false;
        };
        if !g.is_empty() {
            return false; /* 已有别的歌单在拉：全局并发 1 */
        }
        g.push(String::from(id));
        true
    }
    fn release(id: &str) {
        if let Ok(mut g) = INFLIGHT.lock() {
            g.retain(|k| k != id);
        }
    }
    const STATES: [&str; 5] = ["idle", "loading", "ready", "auth", "failed"];
    const TTL_MS: u64 = 60_000; /* 手机上改了歌单，最多一分钟这边跟着变 */

    let valid = !playlist_id.is_empty() && playlist_id.bytes().all(|b| b.is_ascii_digit());
    if !valid {
        return String::from("{\"state\":\"failed\",\"name\":\"\",\"songs\":[]}");
    }

    let now = crate::media::platform::time::now_ms();
    /* 缓存命中（含"是否还新鲜"）：新鲜就直接交，过期就先把旧内容交出去、
     * 同时触发一次后台刷新 —— 界面上不会因为刷新而闪回"同步中"。 */
    let cached: Option<(String, String, bool)> = CACHE.lock().ok().and_then(|cache| {
        cache
            .iter()
            .find(|(id, _, _, _)| id == playlist_id)
            .map(|(_, name, songs, at)| {
                (name.clone(), songs.clone(), now.saturating_sub(*at) < TTL_MS)
            })
    });
    let need_fetch = match &cached {
        Some((_, _, fresh)) => !*fresh,
        None => STATE.load(Ordering::Acquire) != 1,
    };

    if need_fetch && claim(playlist_id) {
        STATE.store(1, Ordering::Release);
        let id = String::from(playlist_id);
        let spawned = std::thread::Builder::new()
            .name("yunyin-net-pl-tracks".into())
            .stack_size(96 * 1024)
            .spawn(move || {
                let session = current_session();
                let secret =
                    crypto::secret_key_from_entropy(crate::media::platform::time::entropy64());
                let mut post = crate::media::net::http::VitaPost;
                /* 按需取一张歌单的曲目：界面用这段时间显示"正在同步歌单…"，
                 * 进度就是这个任务（1 步），拿到就结束。 */
                prog_begin(2, 1);
                match mine::playlist_tracks(&id, &secret, &mut post, &session) {
                    Ok((name, songs)) => {
                        crate::media::platform::log::append(&format!(
                            "playlist: {} 首（{name}）",
                            songs.len()
                        ));
                        if let Ok(mut cache) = CACHE.lock() {
                            cache.retain(|(k, _, _, _)| k != &id);
                            cache.push((
                                id.clone(),
                                name.clone(),
                                mine::cloud_songs_json(&songs),
                                crate::media::platform::time::now_ms(),
                            ));
                            /* 上限：最多 4 张、合计 1.5 MiB（FIFO）。
                             * 以前这里是无界 Vec —— 每打开一张歌单就永久留一份 songs_json，
                             * 大歌单几百 KB，几轮下来就是几 MB 常驻内存。 */
                            const CACHE_MAX_ENTRIES: usize = 4;
                            const CACHE_MAX_BYTES: usize = 1536 * 1024;
                            while !cache.is_empty()
                                && (cache.len() > CACHE_MAX_ENTRIES
                                    || cache
                                        .iter()
                                        .map(|(_, _, s, _)| s.len())
                                        .sum::<usize>()
                                        > CACHE_MAX_BYTES)
                            {
                                cache.remove(0); /* 丢最旧的 */
                            }
                        }
                        /* 顺手落盘到 list/：下次打开直接读文件（离线也能看）。 */
                        lists::write_playlist(&id, &name, &mine::cloud_songs_json(&songs));
                        STATE.store(2, Ordering::Release);
                    }
                    Err(e) => {
                        crate::media::platform::log::append(&format!(
                            "playlist: 取歌单失败 {e:?}"
                        ));
                        if let Ok(mut err) = ERR.lock() {
                            *err = (id.clone(), format!("{e:?}"));
                        }
                        STATE.store(
                            if matches!(e, ProviderError::Auth(_)) { 3 } else { 4 },
                            Ordering::Release,
                        );
                    }
                }
                prog_step();
                prog_end();
                release(&id);
            });
        if spawned.is_err() {
            STATE.store(4, Ordering::Release);
            release(playlist_id);
        }
    }

    if let Some((name, songs, _)) = &cached {
        return format!(
            "{{\"state\":\"ready\",\"name\":\"{}\",\"songs\":{}}}",
            json::escape(name),
            songs
        );
    }
    if STATE.load(Ordering::Acquire) == 4 {
        if let Ok(err) = ERR.lock() {
            if err.0 == playlist_id {
                return format!(
                    "{{\"state\":\"failed\",\"name\":\"\",\"message\":\"{}\",\"songs\":[]}}",
                    json::escape(&err.1)
                );
            }
        }
    }
    format!(
        "{{\"state\":\"{}\",\"name\":\"\",\"songs\":[]}}",
        STATES.get(STATE.load(Ordering::Acquire) as usize).unwrap_or(&"idle")
    )
}

/// CDN 期望的 Referer。放在 Provider 旁边，因为这是"平台的事实"，
/// 不是"传输层的事实"。
pub const REFERER: &str = "https://music.163.com/";
pub const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
     Chrome/120.0.0.0 Safari/537.36";

/// API 里音质档位的名字（§47）。Provider 负责把我们内部的 `Quality` 映射过去，
/// 播放器其他地方永远看不到这些字符串。
pub fn quality_id(q: Quality) -> &'static str {
    match q {
        Quality::Auto | Quality::High => "exhigh",
        Quality::Low => "standard",
        Quality::Medium => "higher",
        Quality::Lossless => "lossless",
    }
}

pub fn session_cookie(session: &account::Session) -> Option<String> {
    session.cookie_header()
}

/// 当前会话的 Cookie（给**开流**用：登录用户的播放地址带 authSecret，
/// CDN 要校 Cookie，不带就 403）。
pub fn current_cookie() -> String {
    current_session().cookie_header().unwrap_or_default()
}

/* ------------------------------------------------------- 音质档提示 -- */
/*
 * 歌曲 → 音质档（来自服务端的 `privileges[].plLevel`）。
 *
 * 为什么要记：播放时如果自己猜 level（先 standard 再 exhigh…），猜错就是
 * 一条带 `authSecret` 的加密地址 + CDN 403 + 两轮重试。服务端其实早就告诉过
 * 我们"这个账号这首歌能播到什么档"，照着请求就行。
 *
 * 只留最近 256 条（一张榜单的量级），满了丢最旧的 —— 这是提示，不是数据源。
 */
static LEVEL_HINTS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

pub fn remember_level_hint(song_id: &str, level: &str) {
    if song_id.is_empty() || level.is_empty() {
        return;
    }
    let Ok(mut g) = LEVEL_HINTS.lock() else {
        return;
    };
    if let Some(entry) = g.iter_mut().find(|(id, _)| id == song_id) {
        entry.1 = String::from(level);
        return;
    }
    while g.len() >= 256 {
        g.remove(0);
    }
    g.push((String::from(song_id), String::from(level)));
}

pub fn level_hint(song_id: &str) -> Option<String> {
    LEVEL_HINTS
        .lock()
        .ok()
        .and_then(|g| g.iter().find(|(id, _)| id == song_id).map(|(_, l)| l.clone()))
}

/// `plLevel`（服务端给的音质档）→ 我们请求用的 `Quality`。
pub fn quality_from_level(level: &str) -> Quality {
    match level {
        "standard" => Quality::Low,
        "higher" => Quality::Medium,
        "exhigh" => Quality::High,
        "lossless" => Quality::Lossless,
        /* hires / jymaster / sky / vivid 我们没有专门的 level 字符串，
         * 退到 exhigh —— 权限不够时服务端自己会降级。 */
        _ => Quality::High,
    }
}
