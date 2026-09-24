//! 网易云扫码登录（Phase 4）。
//!
//! 界面线程只读状态、只负责把已经画好的二维码上传成纹理。
//! HTTP 在单独的线程里：unikey → 轮询 801 等待 / 802 已扫 / 803 成功 / 800 过期。
//! Cookie 只有这一条成功路径会写进 `ux0:/data/yunyin/store/netease_cookie`。
//! 日志里不写 Cookie，也不写完整的 codekey。
#![allow(dead_code)]

use super::api;
use super::qr;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

const STORE_KEY: &str = "netease_cookie";
const POLL_ROUNDS: u32 = 50;

struct QrState {
    phase: &'static str,
    hint: String,
    tex: i32,
    pending: Option<Vec<u8>>,
    cookie: String,
    loaded: bool,
}

impl QrState {
    fn fresh() -> Self {
        Self {
            phase: "idle",
            hint: String::from("未登录"),
            tex: -1,
            pending: None,
            cookie: String::new(),
            loaded: false,
        }
    }
}

static GEN: AtomicU32 = AtomicU32::new(0);
static STATE: Mutex<QrState> = Mutex::new(QrState {
    phase: "idle",
    hint: String::new(),
    tex: -1,
    pending: None,
    cookie: String::new(),
    loaded: false,
});

fn lock() -> std::sync::MutexGuard<'static, QrState> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn alive(gen: u32) -> bool {
    GEN.load(Ordering::Acquire) == gen
}

fn ensure_loaded() {
    let already = lock().loaded;
    if already {
        return;
    }
    let saved = crate::media::platform::store::get(STORE_KEY);
    let mut g = lock();
    if g.loaded {
        return;
    }
    g.loaded = true;
    if saved.contains("MUSIC_U") && saved.len() < 4000 && !saved.contains('\0') {
        g.cookie = saved;
        g.phase = "ok";
        g.hint = String::from("已登录");
        drop(g);
        crate::media::platform::log::append("netease: 已载入登录");
    } else {
        g.phase = "idle";
        g.hint = String::from("未登录");
    }
}

pub fn load_saved() {
    ensure_loaded();
}

pub fn api_cookie() -> String {
    ensure_loaded();
    let cookie = lock().cookie.clone();
    if cookie.is_empty() {
        String::from("os=pc")
    } else if cookie.contains("os=pc") {
        cookie
    } else {
        alloc::format!("os=pc; {cookie}")
    }
}

pub fn start() {
    ensure_loaded();
    let gen = GEN.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
    {
        let mut g = lock();
        g.phase = "wait";
        g.hint = String::from("正在获取二维码");
        g.pending = None;
        g.tex = -1;
    }
    let spawned = std::thread::Builder::new()
        .name("yunyin-qr".into())
        .stack_size(96 * 1024)
        .spawn(move || qr_worker(gen));
    if spawned.is_err() {
        let mut g = lock();
        if alive(gen) {
            g.phase = "fail";
            g.hint = String::from("登录线程失败");
        }
    }
}

pub fn logout() {
    GEN.fetch_add(1, Ordering::AcqRel);
    crate::media::platform::store::set(STORE_KEY, "");
    let mut g = lock();
    g.cookie.clear();
    g.phase = "idle";
    g.hint = String::from("未登录");
    g.pending = None;
    g.tex = -1;
    crate::media::platform::log::append("netease: 已退出登录");
}

pub fn state_json() -> String {
    ensure_loaded();
    let pending = {
        let mut g = lock();
        g.pending.take()
    };
    if let Some(rgba) = pending {
        let handle = upload_qr(&rgba);
        lock().tex = handle;
        if handle < 0 {
            crate::media::platform::log::append("netease: 二维码纹理上传失败");
        }
    }
    let g = lock();
    let hint = crate::media::json_escape(&clip_chars(&g.hint, 24));
    alloc::format!(
        "{{\"phase\":\"{}\",\"hint\":\"{hint}\",\"tex\":{}}}",
        g.phase, g.tex
    )
}

fn qr_worker(gen: u32) {
    crate::media::platform::log::append("netease: 开始扫码登录");
    let (body, set_cookie) = match api::post_weapi(
        "/api/login/qrcode/unikey",
        r#"{"type":1,"csrf_token":""}"#,
        "os=pc",
    ) {
        Ok(v) => v,
        Err(_) => {
            fail(gen, "网络失败");
            return;
        }
    };
    if !alive(gen) {
        return;
    }
    let code = super::resolve::json_i32(&body, "code").unwrap_or(0);
    if code != 200 {
        crate::media::platform::log::append(&alloc::format!("netease: 二维码接口 code={code}"));
        let msg = super::resolve::json_string(&body, "message").unwrap_or_else(|| String::from("拿不到二维码"));
        fail(gen, &clip_chars(&msg, 24));
        return;
    }
    let Some(key) = super::resolve::json_string(&body, "unikey") else {
        fail(gen, "没有二维码");
        return;
    };
    if !key_ok(&key) {
        fail(gen, "二维码无效");
        return;
    }
    let url = alloc::format!("https://music.163.com/login?codekey={key}");
    let Some(rgba) = qr::rgba(url.as_bytes()) else {
        fail(gen, "二维码画不出来");
        return;
    };
    if !alive(gen) {
        return;
    }
    {
        let mut g = lock();
        if alive(gen) {
            g.pending = Some(rgba);
            g.phase = "wait";
            g.hint = String::from("请用网易云扫码");
        }
    }
    crate::media::platform::log::append("netease: 二维码已生成");
    let mut jar = join_cookie("os=pc", &set_cookie);
    let mut misses = 0u32;
    for _ in 0..POLL_ROUNDS {
        if !alive(gen) {
            return;
        }
        std::thread::sleep(Duration::from_secs(2));
        if !alive(gen) {
            return;
        }
        let json = alloc::format!(r#"{{"key":"{key}","type":1,"csrf_token":""}}"#);
        let polled = api::post_weapi("/api/login/qrcode/client/login", &json, &jar);
        let (body, more) = match polled {
            Ok(v) => v,
            Err(_) => {
                misses += 1;
                if misses >= 4 {
                    fail(gen, "轮询失败");
                    return;
                }
                continue;
            }
        };
        misses = 0;
        if !more.is_empty() {
            jar = join_cookie(&jar, &more);
        }
        let code = super::resolve::json_i32(&body, "code").unwrap_or(0);
        match code {
            801 => {}
            802 => set_hint(gen, "scanned", "已扫码，请在手机确认"),
            803 => {
                let cookie = pick_cookie(&body, &more, &jar);
                if !cookie.contains("MUSIC_U") {
                    fail(gen, "登录没有账号信息");
                    return;
                }
                if !alive(gen) {
                    return;
                }
                crate::media::platform::store::set(STORE_KEY, &cookie);
                {
                    let mut g = lock();
                    if alive(gen) {
                        g.cookie = cookie;
                        g.phase = "ok";
                        g.hint = String::from("登录成功");
                    }
                }
                crate::media::platform::log::append("netease: 登录成功");
                return;
            }
            800 => {
                set_hint(gen, "expired", "二维码已过期");
                crate::media::platform::log::append("netease: 二维码过期");
                return;
            }
            _ => {
                crate::media::platform::log::append(&alloc::format!("netease: 扫码 code={code}"));
                let msg = super::resolve::json_string(&body, "message")
                    .unwrap_or_else(|| String::from("登录失败"));
                fail(gen, &clip_chars(&msg, 24));
                return;
            }
        }
    }
    set_hint(gen, "expired", "二维码已过期");
}

fn fail(gen: u32, hint: &str) {
    set_hint(gen, "fail", hint);
    crate::media::platform::log::append(&alloc::format!("netease: 登录失败 {hint}"));
}

fn set_hint(gen: u32, phase: &'static str, hint: &str) {
    if !alive(gen) {
        return;
    }
    let mut g = lock();
    if alive(gen) {
        g.phase = phase;
        g.hint = String::from(hint);
    }
}

fn key_ok(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn pick_cookie(body: &str, header: &str, jar: &str) -> String {
    if let Some(c) = super::resolve::json_string(body, "cookie") {
        if c.contains("MUSIC_U") {
            return clip_cookie(&c);
        }
    }
    if header.contains("MUSIC_U") {
        return clip_cookie(header);
    }
    if jar.contains("MUSIC_U") {
        return clip_cookie(jar);
    }
    String::new()
}

fn join_cookie(base: &str, extra: &str) -> String {
    let mut out = String::from(base.trim());
    for part in extra.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let name = part.split('=').next().unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let exists = out.split(';').any(|p| {
            let p = p.trim();
            p == name || p.starts_with(&alloc::format!("{name}="))
        });
        if exists {
            continue;
        }
        if !out.is_empty() {
            out.push_str("; ");
        }
        out.push_str(part);
    }
    clip_cookie(&out)
}

fn clip_cookie(s: &str) -> String {
    let s = s.trim();
    if s.len() <= 3800 {
        return String::from(s);
    }
    String::from(&s[..3800])
}

fn clip_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn upload_qr(rgba: &[u8]) -> i32 {
    if rgba.len() != 256 * 256 * 4 {
        return -1;
    }
    unsafe {
        let h = crate::ffi::ui().upload_texture(rgba, 256, 256, pocketjs_core::spec::psm::PSM_8888);
        if h >= 0 {
            crate::graphics::register_texture(crate::ffi::ui(), h);
        }
        h
    }
}
