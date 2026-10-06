//! `globalThis.vitaMedia` 的 QuickJS 绑定。**方法名是冻结的契约**，不能改。

use crate::media::platform::{fs, log, store};
use crate::media::{bgm, tags};
use alloc::string::String;
use libquickjs_sys::*;

extern "C" {
    fn JS_ToCStringLen2(
        ctx: *mut JSContext,
        plen: *mut size_t,
        val1: JSValue,
        cesu8: i32,
    ) -> *const i8;
    fn JS_NewStringLen(ctx: *mut JSContext, str1: *const u8, len1: usize) -> JSValue;
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

/// 把 `body` 包起来跑，panic 不外泄。
///
/// Rust 的 panic 绝不能从 `extern "C"` 回调里往外展开：QuickJS 的栈帧没有 unwind
/// 表，展开器会一路走进后面的随机内存。这不是理论 —— 真机上读 OGG 标签时一个
/// `&str` 切片 panic，就把程序计数器带进了应用自己的 JS 打包字符串里
/// （dump 里显示为 "undefined instruction exception"）。
/// 现在原生侧能踩到的任何问题，都会退化成 `fallback`。
fn guarded<T>(what: &str, fallback: T, body: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(v) => v,
        Err(_) => {
            log::append(&format!("media: caught panic in {what}"));
            fallback
        }
    }
}

unsafe extern "C" fn js_list(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let path = arg_string(ctx, argc, argv, 0);
    js_str(ctx, &guarded("list", String::new(), || fs::list_dir(&path)))
}

unsafe extern "C" fn js_roots(
    ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    js_str(ctx, guarded("roots", "", fs::roots_json))
}

unsafe extern "C" fn js_play(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let path = arg_string(ctx, argc, argv, 0);
    bgm::play(&path);
    JS_UNDEFINED
}

unsafe extern "C" fn js_pause(
    _ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    bgm::pause();
    JS_UNDEFINED
}

unsafe extern "C" fn js_resume(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let path = arg_string(ctx, argc, argv, 0);
    bgm::resume(&path);
    JS_UNDEFINED
}

unsafe extern "C" fn js_stop(
    _ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    bgm::stop();
    JS_UNDEFINED
}

unsafe extern "C" fn js_state(
    ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    js_str(ctx, &guarded("state", String::new(), bgm::state_json))
}

unsafe extern "C" fn js_cover(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let path = arg_string(ctx, argc, argv, 0);
    JS_NewInt32(ctx, guarded("cover", -1, || tags::upload_cover(&path)))
}

unsafe extern "C" fn js_tags(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let path = arg_string(ctx, argc, argv, 0);
    js_str(ctx, &guarded("tags", String::new(), || tags::tags_json(&path)))
}

unsafe extern "C" fn js_log(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let s = arg_string(ctx, argc, argv, 0);
    log::append(&s);
    JS_NewInt32(ctx, 0)
}

/// 日志是否开着（卡里有 ux0:/data/yunyin/debug 才开）。JS 用它决定要不要
/// 花力气采集诊断数据 —— 正式版默认关，就不做任何额外工作。
unsafe extern "C" fn js_log_enabled(
    ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    JS_NewInt32(ctx, if log::enabled() { 1 } else { 0 })
}

/// 读 list/ 下的清单 JSON（`discover.json` / `toplist_<id>.json` / …）。
/// 当前在线流的缓冲进度：`"已缓冲字节,总字节"`（总长未知时第二项是 0）。
/// 界面拿它画进度条里那根浅色的缓存条。
unsafe extern "C" fn js_net_buffer(
    ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    let (done, total) = crate::media::source::http::buffer_snapshot();
    js_str(ctx, &format!("{done},{total}"))
}

/// 读 list/ 下的清单 JSON（`discover.json` / `toplist_<id>.json` / …）。
/// `netPreload` 保留成**兼容入口**：现在直接转给统一的下一首预取。
///
/// 以前它是"只解析 + 缓存地址"的第二套预热机制，和 `prefetch` 各干一半，
/// 结果同一首下一曲被解析两遍、还互相抢带宽。现在只留 `prefetch` 一条路。
unsafe extern "C" fn js_net_preload(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let id = arg_string(ctx, argc, argv, 0);
    let _level = arg_string(ctx, argc, argv, 1);
    guarded("netPreload", (), || {
        crate::media::source::prefetch::set_next(&id)
    });
    JS_UNDEFINED
}

/// 清单文件的"版本戳"：`"大小,修改时间ms"`（文件不存在返回空串）。
/// 界面每秒只问这个，变了才真去读整份 JSON —— SD 卡上省的是整文件读取。
unsafe extern "C" fn js_list_stat(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let name = arg_string(ctx, argc, argv, 0);
    js_str(
        ctx,
        &guarded("listStat", String::new(), || {
            crate::media::provider::netease::lists::stat(&name)
        }),
    )
}

/// 文件不存在就返回空串，界面显示"同步中"。
unsafe extern "C" fn js_list_read(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let name = arg_string(ctx, argc, argv, 0);
    js_str(
        ctx,
        &guarded("listRead", String::new(), || {
            crate::media::provider::netease::lists::read(&name)
        }),
    )
}

/// 触发一次后台清单同步。`listSync(1)` 跳过 10 分钟 TTL（登录成功后 /
/// 用户手动刷新时用）；启动时原生自己也会跑一次。
unsafe extern "C" fn js_list_sync(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let flag = arg_string(ctx, argc, argv, 0);
    let force = matches!(flag.as_str(), "1" | "true" | "force");
    guarded("listSync", (), || {
        crate::media::provider::netease::lists::sync_background(force)
    });
    JS_UNDEFINED
}

/// 批量歌曲详情：参数是逗号分隔的歌曲 ID，回缓存里已有的那些。
unsafe extern "C" fn js_net_songs_info(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let ids = arg_string(ctx, argc, argv, 0);
    js_str(
        ctx,
        &guarded("netSongsInfo", String::from("[]"), || {
            crate::media::provider::netease::songs_info_json(&ids)
        }),
    )
}

/// 某张歌单的歌曲：`{"state":"…","name":"…","songs":[…]}`。
unsafe extern "C" fn js_net_playlist_tracks(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let id = arg_string(ctx, argc, argv, 0);
    js_str(
        ctx,
        &guarded(
            "netPlaylistTracks",
            String::from("{\"state\":\"failed\",\"name\":\"\",\"songs\":[]}"),
            || crate::media::provider::netease::playlist_tracks_json(&id),
        ),
    )
}

/// 播放期间锁 PS 键：`setPsLock(true)` 锁、`false` 解锁（见 ps_lock.rs）。
unsafe extern "C" fn js_net_probe(
    ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    /* Phase 0: 立刻跑一次网络探针（不被 netprobe.url 开关限制），返回状态 JSON。
     * 详细证据写在 ux0:data/yunyin-netprobe.log。 */
    crate::media::net::probe::run_now();
    js_str(ctx, &guarded("net_probe", String::new(), crate::media::net::probe::state_json))
}

/// 卡里 `ux0:/data/yunyin/playlist.json` 描述的在线曲目清单。
///
/// 界面拿它把在线歌**当成曲库里的独立条目**显示（自己的专辑「在线歌曲」），
/// 这样在线歌不会顶掉任何本地歌曲的位置；点它、按 ○ 才会真的走网络播放。
/// 没有这个文件就返回 `[]`，正式版不受影响。
unsafe extern "C" fn js_netplay(
    ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    js_str(
        ctx,
        &guarded("netplay", String::from("[]"), crate::media::source::remote::netplay_json),
    )
}

/* ---------------- Phase 4：扫码登录（全部非阻塞，界面只读状态） ------------- */

unsafe extern "C" fn js_net_login_start(
    _ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    guarded("netLoginStart", (), crate::media::provider::netease::login_start);
    JS_UNDEFINED
}

unsafe extern "C" fn js_net_login_tick(
    _ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    guarded("netLoginTick", (), crate::media::provider::netease::login_tick);
    JS_UNDEFINED
}

/// `vitaMedia.listTouched()` —— 自上次调用后被写过的清单文件名（逗号分隔）。
///
/// 界面用它做**事件驱动**刷新：拿到空串就一个文件操作都不做；
/// 有名字才去读那几份 JSON。这样每秒 6 次 `listStat`（真机 35~86ms/次）彻底消失。
unsafe extern "C" fn js_list_touched(
    ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    js_str(
        ctx,
        &guarded(
            "listTouched",
            String::new(),
            crate::media::provider::netease::lists::take_touched,
        ),
    )
}

/// `vitaMedia.netUserActive()` —— 每次按键都调一次；预取据此在 3 秒内让路。
unsafe extern "C" fn js_net_user_active(
    _ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    guarded("netUserActive", (), crate::media::source::prefetch::note_user_input);
    JS_UNDEFINED
}

/// `vitaMedia.netPrefetchNext(id)` —— 声明下一首（切歌时调，空串只取消）。
unsafe extern "C" fn js_net_prefetch_next(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let id = arg_string(ctx, argc, argv, 0);
    guarded("netPrefetchNext", (), || {
        crate::media::source::prefetch::set_next(&id)
    });
    JS_UNDEFINED
}

/// `vitaMedia.netPrefetchCancel()` —— 停止预取（停止播放 / 换队列时调）。
unsafe extern "C" fn js_net_prefetch_cancel(
    _ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    guarded("netPrefetchCancel", (), crate::media::source::prefetch::cancel);
    JS_UNDEFINED
}

/// `vitaMedia.netSyncProgress()` → `{"kind":"lists","done":3,"total":7}`
unsafe extern "C" fn js_net_sync_progress(
    ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    js_str(
        ctx,
        &guarded(
            "netSyncProgress",
            String::from("{}"),
            crate::media::provider::netease::sync_progress_json,
        ),
    )
}

unsafe extern "C" fn js_net_login_state(
    ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    js_str(
        ctx,
        &guarded("netLoginState", String::from("{}"), crate::media::provider::netease::login_state_json),
    )
}

unsafe extern "C" fn js_net_login_remember(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let flag = arg_string(ctx, argc, argv, 0);
    let on = flag.trim() == "1" || flag.trim().eq_ignore_ascii_case("true");
    guarded("netLoginRemember", (), || {
        crate::media::provider::netease::login_remember(on)
    });
    JS_UNDEFINED
}

unsafe extern "C" fn js_net_logout(
    _ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    guarded("netLogout", (), crate::media::provider::netease::logout);
    JS_UNDEFINED
}

unsafe extern "C" fn js_net_song_info(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let id = arg_string(ctx, argc, argv, 0);
    js_str(
        ctx,
        &guarded("netSongInfo", String::from("{}"), || {
            crate::media::provider::netease::song_info_json(&id)
        }),
    )
}

/// 播放期间锁 PS 键：`setPsLock(true)` 锁、`false` 解锁（见 ps_lock.rs）。
unsafe extern "C" fn js_set_ps_lock(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let mut on: i32 = 0;
    if argc > 0 {
        unsafe { JS_ToInt32(ctx, &mut on, *argv) };
    }
    let ret = crate::media::platform::ps_lock::set_locked(on != 0);
    JS_NewInt32(ctx, ret)
}

unsafe extern "C" fn js_store_get(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let key = arg_string(ctx, argc, argv, 0);
    js_str(ctx, &guarded("store_get", String::new(), || store::get(&key)))
}

unsafe extern "C" fn js_store_set(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let key = arg_string(ctx, argc, argv, 0);
    let val = arg_string(ctx, argc, argv, 1);
    store::set(&key, &val);
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

pub unsafe fn install(ctx: *mut JSContext, global: JSValue) {
    let obj = JS_NewObject(ctx);
    add_fn(ctx, obj, b"list\0", js_list, 1);
    add_fn(ctx, obj, b"roots\0", js_roots, 0);
    add_fn(ctx, obj, b"play\0", js_play, 1);
    add_fn(ctx, obj, b"pause\0", js_pause, 0);
    add_fn(ctx, obj, b"resume\0", js_resume, 1);
    add_fn(ctx, obj, b"stop\0", js_stop, 0);
    add_fn(ctx, obj, b"state\0", js_state, 0);
    add_fn(ctx, obj, b"cover\0", js_cover, 1);
    add_fn(ctx, obj, b"tags\0", js_tags, 1);
    add_fn(ctx, obj, b"logMsg\0", js_log, 1);
    add_fn(ctx, obj, b"logEnabled\0", js_log_enabled, 0);
    add_fn(ctx, obj, b"setPsLock\0", js_set_ps_lock, 1);
    add_fn(ctx, obj, b"netProbe\0", js_net_probe, 0);
    add_fn(ctx, obj, b"netplay\0", js_netplay, 0);
    add_fn(ctx, obj, b"netLoginStart\0", js_net_login_start, 0);
    add_fn(ctx, obj, b"netLoginTick\0", js_net_login_tick, 0);
    add_fn(ctx, obj, b"netLoginState\0", js_net_login_state, 0);
    add_fn(ctx, obj, b"netLoginRemember\0", js_net_login_remember, 1);
    add_fn(ctx, obj, b"netSyncProgress\0", js_net_sync_progress, 0);
    add_fn(ctx, obj, b"netPrefetchNext\0", js_net_prefetch_next, 1);
    add_fn(ctx, obj, b"netPrefetchCancel\0", js_net_prefetch_cancel, 0);
    add_fn(ctx, obj, b"netUserActive\0", js_net_user_active, 0);
    add_fn(ctx, obj, b"netLogout\0", js_net_logout, 0);
    add_fn(ctx, obj, b"netSongInfo\0", js_net_song_info, 1);
    add_fn(ctx, obj, b"netSongsInfo\0", js_net_songs_info, 1);
    add_fn(ctx, obj, b"netPlaylistTracks\0", js_net_playlist_tracks, 1);
    add_fn(ctx, obj, b"listRead\0", js_list_read, 1);
    add_fn(ctx, obj, b"listStat\0", js_list_stat, 1);
    add_fn(ctx, obj, b"listTouched\0", js_list_touched, 0);
    add_fn(ctx, obj, b"netPreload\0", js_net_preload, 2);
    add_fn(ctx, obj, b"listSync\0", js_list_sync, 0);
    add_fn(ctx, obj, b"netBuffer\0", js_net_buffer, 0);
    add_fn(ctx, obj, b"store_get\0", js_store_get, 1);
    add_fn(ctx, obj, b"store_set\0", js_store_set, 2);
    JS_SetPropertyStr(ctx, global, c"vitaMedia".as_ptr(), obj);
}
