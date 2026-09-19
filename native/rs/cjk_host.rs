//! PocketJS 0.12.0 streamed-glyph HostOps + local io.offload, mounted onto
//! the existing `globalThis.ui` / `globalThis.offload` objects.

use alloc::ffi::CString;
use alloc::string::String;
use core::ffi::c_char;
use libquickjs_sys::*;

use crate::ffi::{add_fn, ui};
use crate::media::offload_local;

extern "C" {
    fn JS_ParseJSON(
        ctx: *mut JSContext,
        buf: *const c_char,
        len: usize,
        filename: *const c_char,
    ) -> JSValue;
    fn JS_ToCStringLen2(
        ctx: *mut JSContext,
        plen: *mut size_t,
        val1: JSValue,
        cesu8: i32,
    ) -> *const i8;
    fn JS_NewStringLen(ctx: *mut JSContext, str1: *const u8, len1: usize) -> JSValue;
    fn JS_GetArrayBuffer(ctx: *mut JSContext, plen: *mut size_t, obj: JSValue) -> *mut u8;
}

unsafe fn buffer_bytes(ctx: *mut JSContext, val: JSValue) -> Option<(*const u8, usize)> {
    let mut len: size_t = 0;
    let p = JS_GetArrayBuffer(ctx, &mut len, val);
    if !p.is_null() {
        return Some((p as *const u8, len));
    }
    JS_FreeValue(ctx, JS_GetException(ctx));
    let buf = JS_GetPropertyStr(ctx, val, c"buffer".as_ptr());
    let mut blen: size_t = 0;
    let bp = JS_GetArrayBuffer(ctx, &mut blen, buf);
    JS_FreeValue(ctx, buf);
    if bp.is_null() {
        JS_FreeValue(ctx, JS_GetException(ctx));
        return None;
    }
    let off_v = JS_GetPropertyStr(ctx, val, c"byteOffset".as_ptr());
    let mut off: i32 = 0;
    JS_ToInt32(ctx, &mut off, off_v);
    JS_FreeValue(ctx, off_v);
    let len_v = JS_GetPropertyStr(ctx, val, c"byteLength".as_ptr());
    let mut vlen: i32 = 0;
    JS_ToInt32(ctx, &mut vlen, len_v);
    JS_FreeValue(ctx, len_v);
    if off < 0 || vlen < 0 || (off as usize) + (vlen as usize) > blen {
        return None;
    }
    Some((bp.add(off as usize) as *const u8, vlen as usize))
}

unsafe fn local_number(ctx: *mut JSContext, obj: JSValue, key: &[u8]) -> u32 {
    let v = JS_GetPropertyStr(ctx, obj, key.as_ptr() as *const _);
    let mut n = 0;
    JS_ToInt32(ctx, &mut n, v);
    JS_FreeValue(ctx, v);
    n as u32
}

unsafe fn local_string(ctx: *mut JSContext, obj: JSValue, key: &[u8]) -> String {
    let v = JS_GetPropertyStr(ctx, obj, key.as_ptr() as *const _);
    let mut n: size_t = 0;
    let p = JS_ToCStringLen2(ctx, &mut n, v, 0);
    let s = if !p.is_null() && n <= 4096 {
        String::from_utf8_lossy(core::slice::from_raw_parts(p as *const u8, n)).into_owned()
    } else {
        String::new()
    };
    if !p.is_null() {
        JS_FreeCString(ctx, p);
    }
    JS_FreeValue(ctx, v);
    s
}

unsafe extern "C" fn js_font_stream_configure(
    ctx: *mut JSContext,
    _: JSValue,
    n: i32,
    a: *mut JSValue,
) -> JSValue {
    let ok = if n > 0 {
        buffer_bytes(ctx, *a)
            .map(|(p, l)| ui().font_stream_configure(core::slice::from_raw_parts(p, l)))
            .unwrap_or(false)
    } else {
        false
    };
    JS_NewBool(ctx, ok)
}

unsafe extern "C" fn js_font_stream_commit(
    ctx: *mut JSContext,
    _: JSValue,
    n: i32,
    a: *mut JSValue,
) -> JSValue {
    let count = if n > 0 {
        buffer_bytes(ctx, *a)
            .map(|(p, l)| ui().font_stream_commit(core::slice::from_raw_parts(p, l)))
            .unwrap_or(0)
    } else {
        0
    };
    JS_NewInt32(ctx, count as i32)
}

unsafe extern "C" fn js_font_stream_requests(
    ctx: *mut JSContext,
    _: JSValue,
    _: i32,
    _: *mut JSValue,
) -> JSValue {
    let s = ui().font_stream_requests();
    JS_NewStringLen(ctx, s.as_ptr(), s.len())
}

unsafe extern "C" fn js_font_stream_batch(
    ctx: *mut JSContext,
    _: JSValue,
    n: i32,
    a: *mut JSValue,
) -> JSValue {
    let result = if n > 0 {
        buffer_bytes(ctx, *a)
            .map(|(p, l)| ui().font_stream_batch(core::slice::from_raw_parts(p, l)))
            .unwrap_or(-3)
    } else {
        -3
    };
    JS_NewInt32(ctx, result)
}

unsafe extern "C" fn js_font_stream_stats(
    ctx: *mut JSContext,
    _: JSValue,
    _: i32,
    _: *mut JSValue,
) -> JSValue {
    let s = ui().font_stream_stats();
    JS_NewStringLen(ctx, s.as_ptr(), s.len())
}

unsafe extern "C" fn js_local_session(
    ctx: *mut JSContext,
    _: JSValue,
    _: i32,
    _: *mut JSValue,
) -> JSValue {
    JS_NewInt32(ctx, offload_local::session())
}

unsafe extern "C" fn js_local_take(
    ctx: *mut JSContext,
    _: JSValue,
    _: i32,
    _: *mut JSValue,
) -> JSValue {
    match offload_local::take() {
        Some(s) => JS_NewStringLen(ctx, s.as_ptr(), s.len()),
        None => JS_UNDEFINED,
    }
}

unsafe extern "C" fn js_local_submit(
    ctx: *mut JSContext,
    _: JSValue,
    n: i32,
    a: *mut JSValue,
) -> JSValue {
    if n < 1 {
        return JS_NewBool(ctx, false);
    }
    let mut len: size_t = 0;
    let p = JS_ToCStringLen2(ctx, &mut len, *a, 0);
    if p.is_null() {
        return JS_NewBool(ctx, false);
    }
    if len > 4096 {
        JS_FreeCString(ctx, p);
        return JS_NewBool(ctx, false);
    }
    let obj = JS_ParseJSON(ctx, p, len, b"local request\0".as_ptr() as *const _);
    JS_FreeCString(ctx, p);
    if JS_IsException(obj) {
        JS_FreeValue(ctx, JS_GetException(ctx));
        return JS_NewBool(ctx, false);
    }
    let mut r = offload_local::Request::empty();
    r.id = local_number(ctx, obj, b"id\0");
    let method = local_string(ctx, obj, b"method\0");
    let payload = local_string(ctx, obj, b"payload\0");
    let version = local_number(ctx, obj, b"v\0");
    JS_FreeValue(ctx, obj);
    if version != 1 || r.id == 0 {
        return JS_NewBool(ctx, false);
    }
    r.op = match method.as_str() {
        "font.open" => 1,
        "font.glyphs" => 2,
        "font.stats" => 3,
        "fs.read-text" => 4,
        "font.close" => 5,
        _ => 0,
    };
    if r.op == 1 || r.op == 4 {
        if payload.len() >= 128 {
            return JS_NewBool(ctx, false);
        }
        r.path[..payload.len()].copy_from_slice(payload.as_bytes());
    }
    if r.op == 2 {
        let Ok(json) = CString::new(payload.as_bytes()) else {
            return JS_NewBool(ctx, false);
        };
        let args = JS_ParseJSON(
            ctx,
            json.as_ptr(),
            payload.len(),
            b"font batch\0".as_ptr() as *const _,
        );
        if JS_IsException(args) {
            JS_FreeValue(ctx, JS_GetException(ctx));
            return JS_NewBool(ctx, false);
        }
        r.generation = local_number(ctx, args, b"generation\0");
        let slot = local_number(ctx, args, b"slot\0");
        let cps = JS_GetPropertyStr(ctx, args, b"scalars\0".as_ptr() as *const _);
        let count = local_number(ctx, cps, b"length\0");
        if count > 0 && count <= 4 && slot < 24 {
            r.count = count as u8;
            r.slot = slot as u8;
            for i in 0..count {
                let key = [b'0' + i as u8, 0];
                r.cps[i as usize] = local_number(ctx, cps, &key);
            }
        }
        JS_FreeValue(ctx, cps);
        JS_FreeValue(ctx, args);
        if r.count == 0 {
            return JS_NewBool(ctx, false);
        }
    }
    JS_NewBool(ctx, offload_local::submit(r))
}

/// Attach fontStream* onto `globalThis.ui` and `offload.local` onto global.
///
/// # Safety
/// Same QuickJS realm as `ffi::register`, render thread, once per guest.
pub unsafe fn install(ctx: *mut JSContext, global: JSValue) {
    offload_local::start();

    let ui_obj = JS_GetPropertyStr(ctx, global, b"ui\0".as_ptr() as *const _);
    if !JS_IsUndefined(ui_obj) {
        add_fn(ctx, ui_obj, b"fontStreamConfigure\0", js_font_stream_configure, 1);
        add_fn(ctx, ui_obj, b"fontStreamCommit\0", js_font_stream_commit, 1);
        add_fn(ctx, ui_obj, b"fontStreamRequests\0", js_font_stream_requests, 0);
        add_fn(ctx, ui_obj, b"fontStreamStats\0", js_font_stream_stats, 0);
        add_fn(ctx, ui_obj, b"fontStreamBatch\0", js_font_stream_batch, 1);
    }
    JS_FreeValue(ctx, ui_obj);

    let io = JS_NewObject(ctx);
    let local = JS_NewObject(ctx);
    add_fn(ctx, local, b"session\0", js_local_session, 0);
    add_fn(ctx, local, b"submit\0", js_local_submit, 1);
    add_fn(ctx, local, b"take\0", js_local_take, 0);
    JS_SetPropertyStr(ctx, io, b"local\0".as_ptr() as *const _, local);
    JS_SetPropertyStr(ctx, global, b"offload\0".as_ptr() as *const _, io);
}
