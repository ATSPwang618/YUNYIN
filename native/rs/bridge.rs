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

unsafe extern "C" fn js_qr_start(
    _ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    guarded("qr_start", (), crate::media::provider::netease::login::start);
    JS_UNDEFINED
}

unsafe extern "C" fn js_qr_state(
    ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    js_str(
        ctx,
        &guarded(
            "qr_state",
            String::from("{\"phase\":\"fail\",\"hint\":\"\",\"tex\":-1}"),
            crate::media::provider::netease::login::state_json,
        ),
    )
}

unsafe extern "C" fn js_qr_logout(
    ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    guarded("qr_logout", (), crate::media::provider::netease::login::logout);
    JS_NewInt32(ctx, 0)
}

/// 卡里 `ux0:/data/yunyin/netplay.url` 描述的在线曲目清单。
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
    add_fn(ctx, obj, b"qrStart\0", js_qr_start, 0);
    add_fn(ctx, obj, b"qrState\0", js_qr_state, 0);
    add_fn(ctx, obj, b"qrLogout\0", js_qr_logout, 0);
    add_fn(ctx, obj, b"store_get\0", js_store_get, 1);
    add_fn(ctx, obj, b"store_set\0", js_store_set, 2);
    JS_SetPropertyStr(ctx, global, c"vitaMedia".as_ptr(), obj);
}
