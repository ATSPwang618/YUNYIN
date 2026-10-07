"""宿主补丁（帧循环 / 正式包开关 / 诊断）：让 PocketJS 原生宿主装得下 YUNYIN
的媒体模块，并关掉 0.13 的两个开发用开关（devmenu 叠层、guest 看门狗）。"""

import re

from . import patches_graphics
from .config import APP_NAME, CATCH_HANG, FORCE_CJK_BAKED, NO_COVER, PKJ
from .patching import patch


# --- 3 patch host ---------------------------------------------------------
def patch_host():
    lib = PKJ / "hosts/vita/src/lib.rs"
    t = lib.read_text()
    if "pub mod media;" not in t:
        t = patch(t, "pub mod vid;", "\npub mod media;", "lib.rs module")
    if "media::register(ctx" not in t:
        t = patch(t, "ffi::register(ctx, global, &textures, &sprites);",
                  "\n        media::register(ctx, global);", "lib.rs register")
    lib.write_text(t)

    cargo = PKJ / "hosts/vita/Cargo.toml"
    c = cargo.read_text()
    if "[build-dependencies]" not in c:
        c += "\n[build-dependencies]\ncc = \"1\"\n"
    # 需要的 stub：
    #   ScePower      电源 tick
    #   SceAppMgr     BGM 口（sceAppMgrAcquireBgmPort）
    #   SceShellSvc   锁 PS 键（sceShellUtilLock/Unlock）
    #   SceAudiodec   M4A 里的 AAC 硬件解码（sceAudiodecInitLibrary/CreateDecoder/Decode）
    #   SceSysmem     上面那条要的 uncached memblock（sceKernelAllocMemBlock 等）
    # 逐个补进 features 列表，重复构建也安全。
    for needed in ("ScePower_stub", "SceAppMgr_stub", "SceShellSvc_stub",
                   "SceAudiodec_stub", "SceSysmem_stub",
                   # Phase 0 network probe (native/net/yhttp.c)
                   "SceHttp_stub", "SceSsl_stub", "SceNet_stub",
                   "SceNetCtl_stub"):
        if f'"{needed}"' not in c:
            c = c.replace(
                'vitasdk-sys = { version = "0.3.3", features = [',
                f'vitasdk-sys = {{ version = "0.3.3", features = ["{needed}", ',
                1,
            )
    cargo.write_text(c)

    build = PKJ / "hosts/vita/build.rs"
    b = build.read_text()
    if "use std::path::{Path, PathBuf};" not in b:
        b = "use std::path::{Path, PathBuf};\n" + b
    marker = '    println!("cargo:rerun-if-env-changed=POCKETJS_CAPTURE_DIR");'
    # Rewrite the native block unless it already matches exactly what this
    # project ships.  An older experiment left the PocketJS checkout's block
    # referencing ym4a.c/yhttp.c files that stage() had already deleted, so the
    # stale block simply broke the next build; comparing against every file we
    # ship keeps the two in step.
    needs_cc = (
        "host/yunyin_listdir.c" not in b
        or "audio/yplayer.c" not in b
        or "audio/yp_io_file.c" not in b
        or "audio/ym4a.c" not in b
        or "audio/yaac.c" not in b
        or "net/yhttp.c" not in b
        or 'join("yhttp.c")' in b  # the abandoned flat-path block
        or "yunyin_shellsvc_stub.S" in b
        or "empva_bridge" in b
        or "taihen_loader" in b
        or 'cargo:rustc-link-lib=mpg123' not in b
        or 'cargo:rustc-link-lib=curl_yunyin' not in b
    )
    if needs_cc:
        b = re.sub(
            r'\n    let native = Path::new\("native"\);'
            r'\n    if native\.join\("[a-z0-9_]+\.c"\)\.exists\(\) \{\n'
            r'.*?println!\("cargo:rerun-if-changed=native/[a-z0-9_]+\.c"\);\n'
            r'    \}',
            '', b, flags=re.S)
        b = re.sub(
            r'\n    \{ let native = Path::new\("native"\);'
            r'.*?\n    \}',
            '\n', b, flags=re.S)
        blocks = (
            '\n    { let native = Path::new("native");'
            # C sources are grouped: audio/ (player + M4A + AAC hardware),
            # host/ (image + directory listing + the shared log helper).
            # Both folders are include roots so a file can say "ym4a.h"
            # (same folder) or "vendor/dr_wav.h" (native/ root).
            '\n      let audio = native.join("audio");'
            '\n      let host = native.join("host");'
            '\n      cc::Build::new().file(audio.join("yplayer.c"))'
            '.include(native).include(&audio).include(&host)'
            '.define("YPLAYER", None).compile("yplayer");'
            # Phase 1：解码器的输入层（yp_io 的文件实现）
            '\n      cc::Build::new().file(audio.join("yp_io_file.c"))'
            '.include(native).include(&audio).include(&host)'
            '.compile("yp_io_file");'
            '\n      cc::Build::new().file(audio.join("ym4a.c"))'
            '.include(native).include(&audio).include(&host)'
            '.compile("ym4a");'
            '\n      cc::Build::new().file(audio.join("yaac.c"))'
            '.include(native).include(&audio).include(&host)'
            '.compile("yaac");'
            '\n      cc::Build::new().file(host.join("yunyin_image.c"))'
            '.include(native).include(&host)'
            '.define("STBI_NO_STDIO", None).compile("yunyin_image");'
            '\n      cc::Build::new().file(host.join("yunyin_listdir.c"))'
            '.include(native).include(&host)'
            '.compile("yunyin_listdir");'
            # Phase 0 transport probe: Vita-only (SceNet/SceSsl/SceHttp).
            '\n      let net = native.join("net");'
            '\n      cc::Build::new().file(net.join("yhttp.c"))'
            '.include(native).include(&net).include(&host)'
            '.compile("yhttp");'
            '\n      println!("cargo:rustc-link-lib=mpg123");'
            '\n      println!("cargo:rustc-link-lib=vorbisfile");'
            '\n      println!("cargo:rustc-link-lib=vorbis");'
            '\n      println!("cargo:rustc-link-lib=ogg");'
            '\n      println!("cargo:rustc-link-lib=opusfile");'
            '\n      println!("cargo:rustc-link-lib=opus");'
            # Use the YUNYIN-built libcurl/OpenSSL transport rather than the
            # firmware-dependent SceHttp/SceSsl handshake. The stock 2026.08
            # SDK curl archive was built against a different OpenSSL package;
            # libcurl_yunyin is built in the same image against the SDK's
            # current headers and libraries.
            '\n      println!("cargo:rustc-link-lib=curl_yunyin");'
            '\n      println!("cargo:rustc-link-lib=ssl");'
            '\n      println!("cargo:rustc-link-lib=crypto");'
            '\n      println!("cargo:rustc-link-lib=z");'
            '\n      println!("cargo:rustc-link-lib=zstd");'
            # OpenSSL's VitaSDK archive uses pthread rwlocks; keep pthread
            # after the archive that introduced those references.
            '\n      println!("cargo:rustc-link-lib=pthread");'
            '\n      println!("cargo:rustc-link-search=native/libs");'
            '\n      println!("cargo:rerun-if-changed=native/audio");'
            '\n      println!("cargo:rerun-if-changed=native/host");'
            '\n      println!("cargo:rerun-if-changed=native/net");'
            '\n      println!("cargo:rerun-if-changed=native/vendor");'
            '\n    }'
        )
        if marker not in b:
            raise SystemExit("[build-vpk] build.rs POCKETJS_CAPTURE_DIR marker not found")
        b = b.replace(marker, blocks + "\n" + marker)
    build.write_text(b)
    patches_graphics.patch_streamed_cjk()
    print("[build-vpk] host patched (v0.12.0 anchors, no SceShellSvc)")


def patch_host_defer_dynamic_texture_gpu():
    """Defer JS texture GPU mirrors until the render scene is open.

    `ui.uploadTexture()` is called from QuickJS.  The Vita host used to copy
    the same texture into a vita2d/GXM texture immediately, which can wait for
    the previous GPU scene and make the guest frame exceed a second on real
    hardware.  The graphics backend already lazily resolves missing handles
    from the DrawList, so keeping the core upload here and letting render()
    register the mirror preserves the ABI while keeping blocking GPU work out
    of the JS/input frame.
    """
    ffi = PKJ / "hosts/vita/src/ffi.rs"
    if not ffi.exists():
        print("[build-vpk] dynamic texture GPU deferral skipped (PocketJS ffi.rs missing)")
        return
    t = ffi.read_text()
    if "YUNYIN_DEFER_DYNAMIC_TEXTURE_GPU" in t:
        print("[build-vpk] dynamic texture GPU upload already deferred")
        return
    old = (
        "    if handle >= 0 {\n"
        "        // GE samples RAM: write the core's aligned copy (pixels + CLUT) back\n"
        "        // once at upload.\n"
        "        crate::graphics::register_texture(ui(), handle);\n"
        "    }\n"
    )
    new = (
        "    /* YUNYIN_DEFER_DYNAMIC_TEXTURE_GPU: graphics::resolve_texture() registers\n"
        "     * this handle during render, after begin_frame() has made GXM idle.\n"
        "     * Doing it here runs inside QuickJS and can stall input/guest frames. */\n"
    )
    if old in t:
        ffi.write_text(t.replace(old, new, 1))
        print("[build-vpk] dynamic texture GPU upload deferred to render")
        return

    # PocketJS 0.13.0 and the later 0.13 snapshots kept the same operation
    # but changed the explanatory comments.  Match the call as a fallback so
    # a harmless upstream comment change cannot silently re-enable a blocking
    # GPU mirror upload in QuickJS.
    call = "crate::graphics::register_texture(ui(), handle);"
    if call not in t:
        raise SystemExit(
            "[build-vpk] ffi.rs dynamic texture upload anchor not found; "
            "refusing to build without the guest-frame GPU deferral"
        )
    replacement = (
        "        /* YUNYIN_DEFER_DYNAMIC_TEXTURE_GPU: resolve the core handle "
        "during render, after begin_frame(). */"
    )
    ffi.write_text(t.replace(call, replacement, 1))
    print("[build-vpk] dynamic texture GPU upload deferred to render")


def patch_host_offload_frames():
    """流式字库的本地 io.offload 通道**每帧必须复位一次** —— PSP 宿主有这一步，
    Vita 宿主漏了（本补丁把它补上）。

    为什么这条是"流式字库全是空白"的根因：`offload_local` 用两个"每帧一次"的
    闸门控制与 JS 的往来 ——

      * `SENT`  —— 一帧只允许提交一条请求（`submit()` 看到它就返回 false）；
      * `TAKEN` —— 一帧只允许取回一条应答（`take()` 看到它就返回 None）。

    两盏灯都靠 `offload_local::frame()` 熄灭。PSP 宿主在帧循环里调它
    （hosts/psp/src/main.rs），Vita 宿主从头到尾没调过：

      * `font.open`（第一条请求）能成功 —— 所以字库显示 ready；
      * 之后每一条 `font.glyphs` 都提交不出去 → 到点在 JS 侧超时 →
        `Error: Provider unavailable`；就算宿主真去取，答复也永远取不回来。

    真机/Vita3K 日志里的样子：
        cjk: mode=stream status=ready host={... "resident":0,"inked":0 ...}
             js={state:ready,req:63,loaded:0,err:Error: Provider unavailable}
    屏幕上的样子：歌词/列表一片空白（字形还在"等待"态，画的是透明占位）。

    所以这里在 guest 帧入口（`call_frame`，所有帧路径的唯一入口）调用一次，
    和 PSP 宿主同一位置、同一语义。
    """
    lib = PKJ / "hosts/vita/src/lib.rs"
    if not lib.exists():
        return
    t = lib.read_text()
    if "YUNYIN_OFFLOAD_FRAME" in t:
        print("[build-vpk] offload_local per-frame reset already patched")
        return
    anchor = "    unsafe fn call_frame(&mut self, values: &mut [JSValue]) -> Result<(), String> {\n"
    hook = (
        "        /* YUNYIN_OFFLOAD_FRAME: 流式字形 / 本地 offload 通道的每帧闸门\n"
        "         * （PSP 宿主有这一步，Vita 宿主漏了 → 第二条请求永远发不出去）。 */\n"
        "        crate::media::offload_local::frame();\n"
    )
    if anchor in t:
        lib.write_text(t.replace(anchor, anchor + hook, 1))
        print("[build-vpk] patch: offload_local per-frame reset (streamed CJK fix)")
    else:
        print("[build-vpk] WARN: lib.rs call_frame anchor not found; offload frame hook skipped")


def patch_host_frame_loop():
    """帧循环补丁：刷字形钩子 + "画面没变就跳过 render/present"。

    这一步和图形管线无关，所以 **BARE_GRAPHICS=1 也要打**：
    以前它被塞在 patch_font_gpu() 里，正式包一开 bare 就跟着丢 ——
    结果每帧无条件重建顶点 + 提交 GXM + 换缓冲（暂停时纯发热耗电）。
    判定见 native/rs/ui/frame_skip.rs：DrawList 内容 + raster_revision 的哈希，
    和上一帧一样就跳过绘制；第一帧一定画。

    注意这里所有改动都在**同一份文本**上做（以前两段各自读旧文本写回，
    后面那段会把前一段的结果覆盖掉）。
    """
    m = PKJ / "hosts/vita/src/main.rs"
    s = m.read_text()
    changed = False

    if "refresh_font_atlases()" not in s:
        # 0.12：runtime.tick()；0.13：帧循环变成 if let Some(guest) { guest.tick(); ... }。
        # 两种都认，谁在就用谁。
        for old_tick, new_tick in (
            (
                "        runtime.tick();\n",
                "        runtime.tick();\n"
                "        pocketjs_vita::media::refresh_font_atlases();\n",
            ),
            (
                "            guest.tick();\n",
                "            guest.tick();\n"
                "            pocketjs_vita::media::refresh_font_atlases();\n",
            ),
        ):
            if old_tick in s:
                s = s.replace(old_tick, new_tick, 1)
                changed = True
                print("[build-vpk] patch: main.rs refresh_font_atlases()")
                break
        else:
            raise SystemExit("[build-vpk] main.rs tick anchor not found")
    else:
        print("[build-vpk] main.rs refresh_font_atlases already patched")

    # 三种可能的现场，按"最新→最旧"依次匹配：
    #   A. 已带帧诊断计时（patch_host_frame_diag / present_diag 先跑过）—— 现在的正式流水线
    #   B. 0.13 原始帧循环
    #   C. 0.12 原始帧循环
    oldA = (
        "            let yunyin_r0 = std::time::Instant::now(); /* YUNYIN_HOST_FRAME_DIAG */\n"
        "            guest.render();\n"
        "            let yunyin_rms = yunyin_r0.elapsed().as_millis();\n"
        "            if yunyin_rms > 800 {\n"
        "                pocketjs_vita::media::log::append(&std::format!(\n"
        '                    "HOST: render 耗时 {yunyin_rms}ms"\n'
        "                ));\n"
        "            }\n"
        "        } else {\n"
        "            graphics::begin_frame(0xff1c_1410);\n"
        "        }\n"
        "        dev.overlay();\n"
        "        let yunyin_p0 = std::time::Instant::now(); /* YUNYIN_HOST_FRAME_DIAG */\n"
        "        graphics::present();\n"
        "        {\n"
        "            let yunyin_pms = yunyin_p0.elapsed().as_millis();\n"
        "            if yunyin_pms > 800 {\n"
        "                pocketjs_vita::media::log::append(&std::format!(\n"
        '                    "HOST: present 耗时 {yunyin_pms}ms"\n'
        "                ));\n"
        "            }\n"
        "        }\n"
    )
    newA = (
        "            let yunyin_r0 = std::time::Instant::now(); /* YUNYIN_HOST_FRAME_DIAG */\n"
        "            if pocketjs_vita::media::frame_changed() {\n"
        "                guest.render();\n"
        "                let yunyin_rms = yunyin_r0.elapsed().as_millis();\n"
        "                if yunyin_rms > 800 {\n"
        "                    pocketjs_vita::media::log::append(&std::format!(\n"
        '                        "HOST: render 耗时 {yunyin_rms}ms"\n'
        "                    ));\n"
        "                }\n"
        "                dev.overlay();\n"
        "                let yunyin_p0 = std::time::Instant::now(); /* YUNYIN_HOST_FRAME_DIAG */\n"
        "                graphics::present();\n"
        "                let yunyin_pms = yunyin_p0.elapsed().as_millis();\n"
        "                if yunyin_pms > 800 {\n"
        "                    pocketjs_vita::media::log::append(&std::format!(\n"
        '                        "HOST: present 耗时 {yunyin_pms}ms"\n'
        "                    ));\n"
        "                }\n"
        "            }\n"
        "        } else {\n"
        "            graphics::begin_frame(0xff1c_1410);\n"
        "            dev.overlay();\n"
        "            graphics::present();\n"
        "        }\n"
    )
    # B. 0.13 原始帧循环：if let Some(guest) { tick; refresh; render } else { begin_frame }
    #    dev.overlay(); present();
    oldB = (
        "            guest.tick();\n"
        "            pocketjs_vita::media::refresh_font_atlases();\n"
        "            guest.render();\n"
        "        } else {\n"
        "            graphics::begin_frame(0xff1c_1410);\n"
        "        }\n"
        "        dev.overlay();\n"
        "        graphics::present();\n"
    )
    newB = (
        "            guest.tick();\n"
        "            pocketjs_vita::media::refresh_font_atlases();\n"
        "            if pocketjs_vita::media::frame_changed() {\n"
        "                guest.render();\n"
        "                dev.overlay();\n"
        "                graphics::present();\n"
        "            }\n"
        "        } else {\n"
        "            graphics::begin_frame(0xff1c_1410);\n"
        "            dev.overlay();\n"
        "            graphics::present();\n"
        "        }\n"
    )
    # C. 0.12 原始帧循环：refresh 钩子 + runtime.render() + present()
    oldC = (
        "        pocketjs_vita::media::refresh_font_atlases();\n"
        "        runtime.render();\n"
        "        graphics::present();\n"
    )
    newC = (
        "        pocketjs_vita::media::refresh_font_atlases();\n"
        "        if pocketjs_vita::media::frame_changed() {\n"
        "            runtime.render();\n"
        "            graphics::present();\n"
        "        }\n"
    )

    if "media::frame_changed()" in s:
        print("[build-vpk] main.rs frame_changed already patched")
    elif oldA in s:
        s = s.replace(oldA, newA, 1)
        changed = True
        print("[build-vpk] patch: main.rs skip unchanged frames (0.13 + 帧诊断)")
    elif oldB in s:
        s = s.replace(oldB, newB, 1)
        changed = True
        print("[build-vpk] patch: main.rs skip unchanged frames (0.13)")
    elif oldC in s:
        s = s.replace(oldC, newC, 1)
        changed = True
        print("[build-vpk] patch: main.rs skip unchanged frames (0.12)")
    else:
        print("[build-vpk] WARN: main.rs render/present anchor not found; frames not skipped")

    if changed:
        m.write_text(s)


def patch_vita_release_guards():
    """0.13 宿主的两个"开发用"开关，正式包必须关掉（0.12 没这些文件，自动跳过）：

    * `guest_interrupt`：QuickJS 的时间预算看门狗 —— JS 一超时就打断 guest，
      抛 `InternalError: Interrupted` 并弹出 devmenu；
    * `devmenu::draw`：用 vita2d 画叠层，和我们的 GXM 渲染混用会在
      `vita2d_draw_rectangle` 里直接崩（真机 0.13 实测：专辑页/二维码页必崩，
      `DFAR: 0xff`，调用栈 `SceGxm <- vita2d_draw_rectangle <- devmenu::draw`）。
    """
    lib = PKJ / "hosts/vita/src/lib.rs"
    if lib.exists():
        t = lib.read_text()
        old = "(std::time::Instant::now() > *opaque.cast::<std::time::Instant>()) as i32"
        if CATCH_HANG:
            if "YUNYIN_CATCH_HANG" in t:
                print("[build-vpk] catch-hang 已就绪")
            else:
                t = t.replace(
                    "std::time::Duration::from_millis(250)",
                    "std::time::Duration::from_millis(2000) /* YUNYIN_CATCH_HANG */",
                    1,
                )
                t = t.replace(
                    "{ let _ = opaque; 0 } /* YUNYIN: 正式包不打断 guest */",
                    old,
                    1,
                )
                lib.write_text(t)
                print("[build-vpk] catch-hang: 中断保留、帧预算 2s")
        elif old in t:
            t = t.replace(old, "{ let _ = opaque; 0 } /* YUNYIN: 正式包不打断 guest */", 1)
            lib.write_text(t)
            print("[build-vpk] patch: 0.13 guest interrupt disabled")
        elif "YUNYIN: 正式包不打断 guest" in t:
            print("[build-vpk] 0.13 guest interrupt already disabled")

        # 0.13 also checks the same deadline while draining QuickJS jobs.
        # Disabling only JS_SetInterruptHandler is insufficient: a QR frame
        # can finish JS_Call, then drain_jobs() still returns the watchdog
        # error and the host shuts the guest down (black screen).
        drain_guard = (
            '            if cfg!(feature = "usb-debug") && '
            'std::time::Instant::now() > *self.deadline {\n'
            '                return Err("guest JavaScript time budget exceeded".into());\n'
            '            }\n'
        )
        if CATCH_HANG and "YUNYIN: 正式包关闭 drain_jobs 看门狗" in t:
            # The staged PocketJS checkout is reused between builds. Restore
            # the upstream drain guard when making the explicit diagnostic
            # package; otherwise CATCH_HANG would only restore the interrupt
            # callback while the second watchdog stayed disabled.
            t = t.replace(
                '            /* YUNYIN: 正式包关闭 drain_jobs 看门狗；长帧不能杀 guest。 */\n',
                drain_guard,
                1,
            )
            lib.write_text(t)
            print("[build-vpk] catch-hang: drain_jobs watchdog restored")
        elif not CATCH_HANG and drain_guard in t:
            t = t.replace(
                drain_guard,
                '            /* YUNYIN: 正式包关闭 drain_jobs 看门狗；长帧不能杀 guest。 */\n',
                1,
            )
            lib.write_text(t)
            print("[build-vpk] patch: 0.13 drain_jobs watchdog disabled")
        elif not CATCH_HANG and "YUNYIN: 正式包关闭 drain_jobs 看门狗" in t:
            print("[build-vpk] 0.13 drain_jobs watchdog already disabled")

    menu = PKJ / "hosts/vita/src/devmenu.rs"
    if menu.exists():
        t = menu.read_text()
        anchor = "pub unsafe fn draw(lines: &[String]) {\n"
        guard = (
            "    /* YUNYIN: 正式包不画 dev 叠层（vita2d 与 GXM 混用会崩） */\n"
            "    let _ = lines;\n"
            "    if true {\n"
            "        return;\n"
            "    }\n"
        )
        if anchor in t and "YUNYIN: 正式包不画" not in t:
            t = t.replace(anchor, anchor + guard, 1)
            menu.write_text(t)
            print("[build-vpk] patch: 0.13 devmenu overlay disabled")
        elif "YUNYIN: 正式包不画" in t:
            print("[build-vpk] 0.13 devmenu overlay already disabled")


def patch_host_frame_diag():
    """帧级诊断：把"guest 帧异常"和"慢帧"写进应用日志（ux0:data/yunyin.log）。

    为什么必须有：0.13 宿主在 guest 帧出错时（JS 抛异常 / 看门狗打断）会直接
    `runtime.shutdown()`；而 devmenu 叠层在正式包里被我们禁用了 —— 真机上只剩
    "画面定格、声音还在"，日志里连一个字都没有，完全没法定位。
    这里在 call_frame 前后加计时与异常记录，一次就能分清三种情况：

      * `HOST: guest 帧异常 -> ...`   JS 抛了异常（含 InternalError: interrupted）
      * `HOST: guest 帧耗时 Nms`      这一帧被原生调用拖慢（界面会卡一下）
      * 两者都没有                     说明卡在宿主渲染/呈现（draw/present）里
    """
    lib = PKJ / "hosts/vita/src/lib.rs"
    if not lib.exists():
        return
    t = lib.read_text()
    # `media::platform` 只在 media 子树里可见（mod platform 是私有的）；
    # 从 crate 根要用 media 的再导出路径 `crate::media::log`。
    fixed = t.replace("crate::media::platform::log::append", "crate::media::log::append")
    if fixed != t:
        t = fixed
        lib.write_text(t)
        print("[build-vpk] patch: host frame diagnostics 路径修正 (media::log)")
    if "YUNYIN_HOST_FRAME_DIAG" in t:
        print("[build-vpk] host frame diagnostics already patched")
        # 阈值补丁：老版本写进源码的是 800ms，那个粒度只能抓死机，抓不到「卡一下」。
        if "yunyin_ms > 800 {" in t:
            t = t.replace("yunyin_ms > 800 {", "yunyin_ms > 120 {")
            lib.write_text(t)
            print("[build-vpk] patch: host 帧耗时阈值 800ms -> 120ms")
        return
    old = (
        "        let result = JS_Call(\n"
        "            self.ctx,\n"
        "            self.frame_fn,\n"
        "            self.global,\n"
        "            values.len() as i32,\n"
        "            values.as_mut_ptr(),\n"
        "        );\n"
        "        if JS_IsException(result) {\n"
        "            return Err(exception_string(self.ctx));\n"
        "        }\n"
    )
    new = (
        "        let yunyin_frame_t0 = std::time::Instant::now(); /* YUNYIN_HOST_FRAME_DIAG */\n"
        "        let result = JS_Call(\n"
        "            self.ctx,\n"
        "            self.frame_fn,\n"
        "            self.global,\n"
        "            values.len() as i32,\n"
        "            values.as_mut_ptr(),\n"
        "        );\n"
        "        {\n"
        "            let yunyin_ms = yunyin_frame_t0.elapsed().as_millis();\n"
        "            /* 阈值 120ms：以前是 800ms，那个粒度只能抓死机，抓不到「卡一下」。 */\n"
        "            if yunyin_ms > 120 {\n"
        "                crate::media::platform::log::append(&std::format!(\n"
        '                    "HOST: guest 帧耗时 {yunyin_ms}ms（含原生调用）"\n'
        "                ));\n"
        "            }\n"
        "        }\n"
        "        if JS_IsException(result) {\n"
        "            let yunyin_err = exception_string(self.ctx);\n"
        "            crate::media::platform::log::append(&std::format!(\n"
        '                "HOST: guest 帧异常 -> {yunyin_err}"\n'
        "            ));\n"
        "            return Err(yunyin_err);\n"
        "        }\n"
    )
    if old in t:
        lib.write_text(t.replace(old, new, 1))
        print("[build-vpk] patch: host frame diagnostics (slow frame + guest error)")
    else:
        print("[build-vpk] WARN: lib.rs call_frame anchor not found; frame diagnostics skipped")


def patch_host_present_diag():
    """渲染/呈现计时：卡死既不发生在 JS 也不发生在异常里时，就落在这两段。

    真机 0.13 排障需要一个"到底卡在哪"的确定答案：JS 帧有计时（call_frame），
    宿主自己的 render()/present() 这一段之前是盲区 —— 加上之后，
    `HOST: render 耗时 Nms` / `HOST: present 耗时 Nms` 会直接说明是不是 GXM 卡住。
    """
    main = PKJ / "hosts/vita/src/main.rs"
    if not main.exists():
        return
    t = main.read_text()
    if "YUNYIN_HOST_PHASE_DIAG" in t:
        print("[build-vpk] host present diagnostics already patched")
        return
    # The staged PocketJS checkout is reused between builds.  After the first
    # diagnostic build frame_skip.py may already have moved render/present into
    # `if frame_changed()`.  Match that shape too; otherwise this diagnostic
    # silently disappeared on the next build and the log lost the host-side
    # phase that is needed to separate list/font work from GXM work.
    old_changed = (
        "        if let Some(guest) = runtime.as_mut() {\n"
        "            guest.tick();\n"
        "            pocketjs_vita::media::refresh_font_atlases();\n"
        "            if pocketjs_vita::media::frame_changed() {\n"
        "                guest.render();\n"
        "                dev.overlay();\n"
        "                graphics::present();\n"
        "            }\n"
        "        } else {\n"
        "            graphics::begin_frame(0xff1c_1410);\n"
        "            dev.overlay();\n"
        "            graphics::present();\n"
        "        }\n"
    )
    new_changed = (
        "        if let Some(guest) = runtime.as_mut() {\n"
        "            let yunyin_t0 = std::time::Instant::now(); /* YUNYIN_HOST_PHASE_DIAG */\n"
        "            guest.tick();\n"
        "            let yunyin_tms = yunyin_t0.elapsed().as_millis();\n"
        "            let yunyin_f0 = std::time::Instant::now();\n"
        "            pocketjs_vita::media::refresh_font_atlases();\n"
        "            let yunyin_fms = yunyin_f0.elapsed().as_millis();\n"
        "            let yunyin_c0 = std::time::Instant::now();\n"
        "            let yunyin_changed = pocketjs_vita::media::frame_changed();\n"
        "            let yunyin_cms = yunyin_c0.elapsed().as_millis();\n"
        "            if yunyin_tms >= 8 {\n"
        "                pocketjs_vita::media::log::append(&std::format!(\n"
        '                    "perf: host_phase=guest_tick ms={yunyin_tms}"\n'
        "                ));\n"
        "            }\n"
        "            if yunyin_fms >= 8 {\n"
        "                pocketjs_vita::media::log::append(&std::format!(\n"
        '                    "perf: host_phase=font_refresh ms={yunyin_fms}"\n'
        "                ));\n"
        "            }\n"
        "            if yunyin_cms >= 8 {\n"
        "                pocketjs_vita::media::log::append(&std::format!(\n"
        '                    "perf: host_phase=frame_changed_draw ms={yunyin_cms} changed={}"\n'
        "                    , yunyin_changed as u8\n"
        "                ));\n"
        "            }\n"
        "            if yunyin_changed {\n"
        "                let yunyin_r0 = std::time::Instant::now();\n"
        "                guest.render();\n"
        "                let yunyin_rms = yunyin_r0.elapsed().as_millis();\n"
        "                dev.overlay();\n"
        "                let yunyin_p0 = std::time::Instant::now();\n"
        "                graphics::present();\n"
        "                let yunyin_pms = yunyin_p0.elapsed().as_millis();\n"
        "                if yunyin_rms >= 8 {\n"
        "                    pocketjs_vita::media::log::append(&std::format!(\n"
        '                        "perf: host_phase=render ms={yunyin_rms}"\n'
        "                    ));\n"
        "                }\n"
        "                if yunyin_pms >= 8 {\n"
        "                    pocketjs_vita::media::log::append(&std::format!(\n"
        '                        "perf: host_phase=present ms={yunyin_pms}"\n'
        "                    ));\n"
        "                }\n"
        "            }\n"
        "        } else {\n"
        "            graphics::begin_frame(0xff1c_1410);\n"
        "            dev.overlay();\n"
        "            graphics::present();\n"
        "        }\n"
    )
    if old_changed in t:
        main.write_text(t.replace(old_changed, new_changed, 1))
        print("[build-vpk] patch: host phase diagnostics (tick/font/draw/render/present)")
        return
    old = (
        "        if let Some(guest) = runtime.as_mut() {\n"
        "            guest.tick();\n"
        "            pocketjs_vita::media::refresh_font_atlases();\n"
        "            guest.render();\n"
        "        } else {\n"
        "            graphics::begin_frame(0xff1c_1410);\n"
        "        }\n"
        "        dev.overlay();\n"
        "        graphics::present();\n"
    )
    new = (
        "        if let Some(guest) = runtime.as_mut() {\n"
        "            guest.tick();\n"
        "            pocketjs_vita::media::refresh_font_atlases();\n"
        "            let yunyin_r0 = std::time::Instant::now(); /* YUNYIN_HOST_FRAME_DIAG */\n"
        "            guest.render();\n"
        "            let yunyin_rms = yunyin_r0.elapsed().as_millis();\n"
        "            if yunyin_rms > 800 {\n"
        "                pocketjs_vita::media::log::append(&std::format!(\n"
        '                    "HOST: render 耗时 {yunyin_rms}ms"\n'
        "                ));\n"
        "            }\n"
        "        } else {\n"
        "            graphics::begin_frame(0xff1c_1410);\n"
        "        }\n"
        "        dev.overlay();\n"
        "        let yunyin_p0 = std::time::Instant::now(); /* YUNYIN_HOST_FRAME_DIAG */\n"
        "        graphics::present();\n"
        "        {\n"
        "            let yunyin_pms = yunyin_p0.elapsed().as_millis();\n"
        "            if yunyin_pms > 800 {\n"
        "                pocketjs_vita::media::log::append(&std::format!(\n"
        '                    "HOST: present 耗时 {yunyin_pms}ms"\n'
        "                ));\n"
        "            }\n"
        "        }\n"
    )
    if old in t:
        main.write_text(t.replace(old, new, 1))
        print("[build-vpk] patch: host render/present diagnostics")
    else:
        print("[build-vpk] WARN: main.rs render/present anchor not found; present diagnostics skipped")


def patch_force_cjk_baked():
    """诊断构建：强制曲目/歌词走烘焙字库，绕开流式 CJK 的图集就地刷新路径。

    0.13 黑屏排查用：专辑页一口气画很多 StreamText、切歌又换一批字形，
    都会触发"运行时更新字体图集"。强制 baked 后这条路径完全不跑 ——
    如果黑屏消失，就说明是我们那套图集补丁和 0.13 的渲染模型不相容。
    """
    if not FORCE_CJK_BAKED:
        return
    cjk = PKJ / "apps" / APP_NAME / "core" / "cjk.tsx"
    if not cjk.exists():
        return
    t = cjk.read_text()
    marker = "YUNYIN_FORCE_CJK_BAKED"
    old = 'media()?.store_get?.("cjkMode") === "stream" ? "stream" : "baked"'
    new = f'false /* {marker} 诊断构建强制 baked */ ? "stream" : "baked"'
    if marker in t:
        print("[build-vpk] cjk 已强制 baked（诊断构建）")
    elif old in t:
        cjk.write_text(t.replace(old, new, 1))
        print("[build-vpk] 诊断：强制 cjk=baked")


def patch_no_cover():
    """诊断构建：完全跳过内嵌封面贴图的上传（只画占位色块）。

    0.13 灰屏排查用：专辑页/切歌都会上传封面贴图并交给 GXM 绘制。
    跳过之后如果画面恢复，就锁定是运行时贴图上传/绘制这条路。
    """
    if not NO_COVER:
        return
    app = PKJ / "apps" / APP_NAME / "app.tsx"
    if not app.exists():
        return
    t = app.read_text()
    marker = "YUNYIN_NO_COVER"
    if marker in t:
        print("[build-vpk] 封面上传已禁用（诊断构建）")
        return
    pairs = (
        ("handle = api.cover(cur.audioPath);", f"handle = -1; /* {marker} 诊断禁用封面 */"),
        ("h = api.cover(first.audioPath);", f"h = -1; /* {marker} 诊断禁用封面 */"),
    )
    done = 0
    for old, new in pairs:
        if old in t:
            t = t.replace(old, new, 1)
            done += 1
    app.write_text(t)
    print(f"[build-vpk] 诊断：禁用封面上传（{done} 处）")
