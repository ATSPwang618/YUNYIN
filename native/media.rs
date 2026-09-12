//! Local music I/O for Yunyin: list mounts, decode wav/mp3/ogg, extract
//! embedded album art (ID3 APIC / Vorbis METADATA_BLOCK_PICTURE), feed
//! sceAudioOut. MP3 tries SceAudiodec first, then minimp3.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Mutex;

use libquickjs_sys::*;
use pocketjs_core::spec::psm;

use crate::audio;

extern "C" {
    fn JS_ToCStringLen2(
        ctx: *mut JSContext,
        plen: *mut size_t,
        val1: JSValue,
        cesu8: i32,
    ) -> *const i8;
    fn JS_NewStringLen(ctx: *mut JSContext, str1: *const u8, len1: usize) -> JSValue;
    fn yp_open(path: *const i8) -> i32;
    fn yp_rate() -> i32;
    fn yp_decode(buf: *mut i16, max_frames: i32) -> i32;
    fn yp_seek(frame: i64) -> i32;
    fn yp_length() -> i64;
    fn yp_close();
    fn yunyin_image_decode(
        data: *const u8,
        len: i32,
        rgba: *mut *mut u8,
        w: *mut i32,
        h: *mut i32,
    ) -> i32;
    fn yunyin_image_free(p: *mut u8);
    fn yunyin_image_resize(
        src: *const u8,
        sw: i32,
        sh: i32,
        dst: *mut u8,
        dw: i32,
        dh: i32,
    ) -> i32;
    fn yunyin_list_dir(path: *const u8, out: *mut u8, cap: i32) -> i32;
}

const TARGET_RATE: u32 = 44100;
const MAX_ART: usize = 1024 * 1024;
const COVER_PX: u32 = 256;
/// Bounded metadata prefix: ID3v2 tag + embedded cover live near the start, so
/// we read at most a few hundred KB (vs the whole 10-30MB audio file). This is
/// the dominant cost on a large library, turning O(file) reads into O(KB).
const PREFIX_CAP: usize = (MAX_ART as usize) + 65536;
const DEC_SW: u32 = 0;
const DEC_HW: u32 = 1;
const DEC_PCM: u32 = 2;

static PLAYING: AtomicBool = AtomicBool::new(false);
static PAUSED: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
static WORKER: AtomicBool = AtomicBool::new(false);
static POS_MS: AtomicU32 = AtomicU32::new(0);
static DUR_MS: AtomicU32 = AtomicU32::new(0);
static RATE_HZ: AtomicU32 = AtomicU32::new(TARGET_RATE);
static DEC_KIND: AtomicU32 = AtomicU32::new(DEC_SW);
static PATH_LEN: AtomicUsize = AtomicUsize::new(0);
static PATH_BUF: Mutex<String> = Mutex::new(String::new());
static COVER_HANDLES: Mutex<Option<HashMap<String, i32>>> = Mutex::new(None);

/// 日志开关。默认关：正式版不写 ux0:data/yunyin.log，也不建这个文件。
/// 要抓日志就在卡里建一个空文件 `ux0:/data/yunyin/debug`，再打开云音，
/// 原生侧和 JS 侧（logMsg）的所有日志都会写进 ux0:data/yunyin.log。
static LOG_ON: AtomicBool = AtomicBool::new(false);
const LOG_FLAG: &str = "ux0:data/yunyin/debug";

/// 追加一行到 ux0:data/yunyin.log（与 JS 侧 logMsg 共用同一个文件）。
/// 没开日志就直接丢掉，不碰文件系统。
fn append_log(s: &str) {
    if !LOG_ON.load(Ordering::Relaxed) {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("ux0:data/yunyin.log")
    {
        let _ = writeln!(f, "{}", s);
    }
}

/* =========================================================
 * 后台播放：把系统解码器支持的格式交给 SceShell 播
 *
 * 解码和播放都在 shell 侧进行，所以应用按 PS 回到 LiveArea（被系统挂起）
 * 之后音乐照常响，也不占用本应用自己的线程和内存。系统解码器只认
 * MP3 / AAC(m4a/.aac) / ATRAC9(.at9) / WAV；其余格式（FLAC / OGG / OPUS）
 * 继续走本文件下面的软件解码路径。
 *
 * 接口来自对 Sony 系统静态库的逆向（GrapheneCt/libShellAudio，MIT 许可，
 * 用法参考 GrapheneCt/ElevenMPV-A）。shell 服务对象等入口 vitasdk 没有
 * 导出，由 native/yunyin_shellsvc_stub.S 手写 NID 导入提供。
 * ========================================================= */

/// shell 音频会话：服务对象指针、AudioControl 函数指针、跟踪 id。
/// 存成 usize 是因为裸指针不是 Send，放不进 static。
static SHELL_CTX: Mutex<Option<(usize, usize, i32, i32)>> = Mutex::new(None);
/// 当前是不是由 shell 在播。
static SHELL_ACTIVE: AtomicBool = AtomicBool::new(false);
/// shell 报回来的位置（毫秒）与状态（1 = 播放中，2 = 停/暂停）。
static SHELL_POS_MS: AtomicU32 = AtomicU32::new(0);
static SHELL_STATE: AtomicU32 = AtomicU32::new(0);
/// 这一首是否已经放完（shell 停住且位置归零）。
static SHELL_ENDED: AtomicBool = AtomicBool::new(false);
/// 用户是不是希望它在放（按播放后为真，暂停/停止后为假）。
static SHELL_WANT_PLAY: AtomicBool = AtomicBool::new(false);
/// 是否已经握着 BGM 端口（0x80 档 = 系统认可的 BGM provider）。
static HOLDING_BGM: AtomicBool = AtomicBool::new(false);

const SCE_MUSIC_EVENT_PLAY: i32 = 1;
const SCE_MUSIC_EVENT_STOP: i32 = 2;
/// 音乐播放器服务的事件号（逆向得来，见 libShellAudio 的 ShellAudio.c）。
const EV_MUSIC_INIT: i32 = 0x30000;
const EV_MUSIC_TRACK_INFO: i32 = 0x30003;
const EV_MUSIC_OPEN: i32 = 0x30004;
const EV_MUSIC_COMMAND: i32 = 0x30006;
const EV_MUSIC_STATUS: i32 = 0x30007;
const EV_MUSIC_TERMINATE: i32 = 0x30001;

/* appmgr 的应用事件（数值取自 ElevenMPV-A 的 include/utils.h）。 */
const SCE_APP_EVENT_ON_ACTIVATE: i32 = 0x10000001;
const SCE_APP_EVENT_ON_DEACTIVATE: i32 = 0x10000002;
const SCE_APP_EVENT_REQUEST_QUIT: i32 = 0x20000001;

extern "C" {
    /// 取 shell 音频服务对象（NID 导入见 yunyin_shellsvc_stub.S）。
    fn sceShellSvcGetSvcObj() -> *mut c_void;
    /// 取 appmgr 事件队列里的数量 / 下一条事件。
    fn sceAppMgrReceiveEventNum(event_num: *mut i32) -> i32;
    fn sceAppMgrReceiveEvent(event: *mut SceAppMgrEvent) -> i32;
    /// 按 TITLE_ID 查运行中的进程（用于确认插件注入目标是否存在）。
    fn sceAppMgrGetIdByName(pid: *mut vitasdk_sys::SceUID, name: *const i8) -> i32;
    /// 带优先级申请 BGM 端口（0x80 = "通过 shell 播放"档，参考项目用它）。
    fn sceAppMgrAcquireBgmPortWithPriority(priority: i32) -> i32;
    fn sceAppMgrReleaseBgmPort() -> i32;
    /// 压住系统自动待机（SceKernel 的用户接口）。
    fn sceKernelPowerTick(tick_type: i32) -> i32;
    /// 初始化 shell/shell 事件系统。参考项目在用它之前都会先调这个；
    /// 少了这一步，SceShellUtil / appmgr 事件一类接口会直接报错。
    fn sceShellUtilInitEvents(unk: i32) -> i32;
    /// 0 = disable / 1 = enable BackGround Music：让 shell 把这段播放当成
    /// 系统级 BGM 来管理（这样应用退出时系统会把播放收走）。
    fn sceShellUtilSetBGMMode(mode: i32) -> i32;
    /// 电源回调：参考项目用它在息屏/待机前把播放"续上"。
    /// 参考项目的调用是 4 个参数：(名字, attr=0, 回调, 用户数据)。
    /// 之前我按 3 个参数声明，导致创建失败、回调从未注册。
    fn sceKernelCreateCallback(
        name: *const i8,
        attr: i32,
        func: extern "C" fn(i32, i32, i32, *mut c_void) -> i32,
        arg: *mut c_void,
    ) -> i32;
    fn scePowerRegisterCallback(cbid: i32) -> i32;
}

/// 电源回调里只置这个标志（回调上下文里不能做文件 I/O 之类的重活，
/// 参考项目也是丢给工作线程处理）。
static POWER_EVENT: AtomicU32 = AtomicU32::new(0);

/// 系统要息屏/待机时，趁我们还没被挂起，给 shell 补发一次 PLAY ——
/// 这样应用被挂起后音乐仍在响（参考项目 ElevenMPV-A 的做法）。
extern "C" fn power_callback(_id: i32, _count: i32, power_info: i32, _common: *mut c_void) -> i32 {
    POWER_EVENT.store(power_info as u32, Ordering::Release);
    0
}

/// 处理电源事件的工作线程：息屏/待机前把播放续上。
fn spawn_power_watch() {
    let _ = std::thread::Builder::new()
        .name("yunyin-power".into())
        .stack_size(16 * 1024)
        .spawn(|| loop {
            let info = POWER_EVENT.swap(0, Ordering::AcqRel);
            if info != 0 {
                append_log(&format!("power: callback 0x{:08X}", info));
                /* 息屏/待机/唤醒都补发一次 PLAY：应用被挂起期间由 shell 继续放。
                 * 位定义在各固件上不一致，所以不挑位，反正只有在"用户要它在放"
                 * 的时候才会补发。 */
                if SHELL_WANT_PLAY.load(Ordering::Acquire) {
                    shell_audio_command(SCE_MUSIC_EVENT_PLAY);
                    append_log("power: re-send PLAY");
                }
            }
            unsafe { vitasdk_sys::sceKernelDelayThread(200_000) };
        });
}

fn register_power_callback() {
    let cbid = unsafe {
        sceKernelCreateCallback(
            b"yunyin_power\0".as_ptr() as *const i8,
            0,
            power_callback,
            core::ptr::null_mut(),
        )
    };
    if cbid >= 0 {
        unsafe { scePowerRegisterCallback(cbid) };
        append_log("power callback registered");
    } else {
        append_log(&format!("power callback failed -> 0x{:08X}", cbid as u32));
    }
}

/// SCE_KERNEL_POWER_TICK_DISABLE_AUTO_SUSPEND
const POWER_TICK_DISABLE_AUTO_SUSPEND: i32 = 1;
/// SCE_KERNEL_POWER_TICK_DISABLE_OLED_OFF —— 压住"自动关屏"。
/// 播放期间必须连这个一起压：自动关屏之后系统会进待机，音乐就断了。
/// 用户想关屏时按 Start 走应用主动关屏（音频继续）。
const POWER_TICK_DISABLE_OLED_OFF: i32 = 4;

/// appmgr 事件结构（大小 0x64）。
#[repr(C)]
struct SceAppMgrEvent {
    event: i32,
    app_id: i32,
    param: [u8; 56],
}

/// sceShellSvcAudioControl 的参数块。
#[repr(C)]
struct ShellAudioParams {
    params1: *mut c_void,
    params1_size: u32,
    params2: *mut c_void,
    params2_size: u32,
    params3: *mut c_void,
    params3_size: u32,
    params4: *mut c_void,
    params4_size: u32,
}

impl ShellAudioParams {
    fn new() -> Self {
        Self {
            params1: core::ptr::null_mut(),
            params1_size: 0,
            params2: core::ptr::null_mut(),
            params2_size: 0,
            params3: core::ptr::null_mut(),
            params3_size: 0,
            params4: core::ptr::null_mut(),
            params4_size: 0,
        }
    }
}

/// 服务用来认这段会话的两个 id（初始化时由服务回填）。
#[repr(C)]
struct ShellAudioTracking {
    unk00: i32,
    tracking1: i32,
    tracking2: i32,
}

#[repr(C)]
struct ShellAudioCommand {
    command: i32,
    param: i32,
}

#[repr(C)]
struct ShellAudioOpt {
    flag: i32,
    param1: i32,
    param2: i32,
    param3: i32,
}

type ShellAudioControlFn = unsafe extern "C" fn(
    *mut c_void,
    i32,
    *mut ShellAudioParams,
    i32,
    *mut i32,
    *mut ShellAudioParams,
    i32,
) -> i32;

/// 系统解码器认识这些格式（其余留给软件解码）。
fn shell_audio_supported(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    [".mp3", ".m4a", ".aac", ".at9", ".wav"]
        .iter()
        .any(|ext| p.ends_with(ext))
}

/// 已建立的会话（服务对象、AudioControl、跟踪 id）。
fn shell_ctx() -> Option<(*mut c_void, ShellAudioControlFn, i32, i32)> {
    let (obj, func, t1, t2) = SHELL_CTX.lock().ok().and_then(|g| *g)?;
    let control: ShellAudioControlFn = unsafe { core::mem::transmute(func) };
    Some((obj as *mut c_void, control, t1, t2))
}

/// 拿服务对象和 AudioControl 入口（第 6 个槽位）。
fn shell_audio_svc() -> Option<(*mut c_void, ShellAudioControlFn)> {
    let obj = unsafe { sceShellSvcGetSvcObj() };
    if (obj as usize) < 0x1000 {
        return None;
    }
    let table = unsafe { *(obj as *mut *const usize) };
    if (table as usize) < 0x1000 {
        return None;
    }
    let entry = unsafe { *table.add(5) };
    if (entry as usize) < 0x1000 {
        return None;
    }
    let control: ShellAudioControlFn = unsafe { core::mem::transmute(entry) };
    Some((obj, control))
}

/// 调一次 sceShellSvcAudioControl。
unsafe fn shell_audio_call(
    obj: *mut c_void,
    control: ShellAudioControlFn,
    event: i32,
    params: *mut ShellAudioParams,
    num_in: i32,
    out_params: *mut ShellAudioParams,
) -> i32 {
    let mut res: i32 = 0;
    control(
        obj,
        event,
        params,
        num_in,
        &mut res,
        out_params,
        if out_params.is_null() { 0 } else { 1 },
    )
}

/// 给 shell 发一条播放命令（1 = 播放，2 = 停/暂停）。没有会话就什么都不做。
fn shell_audio_command(cmd: i32) {
    let Some((obj, control, t1, t2)) = shell_ctx() else {
        return;
    };
    let mut command = ShellAudioCommand {
        command: cmd,
        param: 0,
    };
    let mut track = ShellAudioTracking {
        unk00: 0,
        tracking1: t1,
        tracking2: t2,
    };
    let mut params = ShellAudioParams::new();
    params.params1 = &mut command as *mut _ as *mut c_void;
    params.params1_size = 0x8;
    params.params2 = &mut track as *mut _ as *mut c_void;
    params.params2_size = 0xC;
    unsafe {
        shell_audio_call(
            obj,
            control,
            EV_MUSIC_COMMAND,
            &mut params,
            2,
            core::ptr::null_mut(),
        )
    };
}

/// 建立会话（只做一次）：初始化音乐播放器服务，并把播放绑到应用的生命周期上。
fn shell_audio_session() -> bool {
    if SHELL_CTX.lock().map(|g| g.is_some()).unwrap_or(false) {
        return true;
    }
    let Some((obj, control)) = shell_audio_svc() else {
        return false;
    };
    let mut track = ShellAudioTracking {
        unk00: 0,
        tracking1: 1,
        tracking2: 0x8,
    };
    let mut params = ShellAudioParams::new();
    params.params1 = &mut track as *mut _ as *mut c_void;
    params.params1_size = 0xC;
    let ret = unsafe {
        shell_audio_call(
            obj,
            control,
            EV_MUSIC_INIT,
            &mut params,
            1,
            core::ptr::null_mut(),
        )
    };
    if ret != 0 {
        append_log(&format!("shell audio: init -> 0x{:08X}", ret as u32));
        return false;
    }
    let (t1, t2) = (track.tracking1, track.tracking2);

    if let Ok(mut g) = SHELL_CTX.lock() {
        *g = Some((obj as usize, control as usize, t1, t2));
    }
    append_log(&format!(
        "shell audio: session ready (t1={} t2={})",
        t1, t2
    ));
    true
}

/// 结束会话（把 shell 的音乐播放服务还给系统）。退出应用、从 shell 解码
/// 切回软件解码时调用。
fn shell_audio_terminate() {
    let Some((obj, control, t1, t2)) = shell_ctx() else {
        return;
    };
    let mut track = ShellAudioTracking {
        unk00: 0,
        tracking1: t1,
        tracking2: t2,
    };
    let mut params = ShellAudioParams::new();
    params.params1 = &mut track as *mut _ as *mut c_void;
    params.params1_size = 0xC;
    let ret = unsafe {
        shell_audio_call(
            obj,
            control,
            EV_MUSIC_TERMINATE,
            &mut params,
            1,
            core::ptr::null_mut(),
        )
    };
    append_log(&format!("shell audio: terminate -> 0x{:08X}", ret as u32));
    if let Ok(mut g) = SHELL_CTX.lock() {
        *g = None;
    }
}

/// 播放期间压住系统自动待机（对应 ElevenMPV-A 的 PowerTickTask）：
/// 放着不管太久时系统会进待机，把声音一起掐掉；这个 tick 就是挡住它。
fn spawn_power_tick_watch() {
    let _ = std::thread::Builder::new()
        .name("yunyin-powertick".into())
        .stack_size(16 * 1024)
        .spawn(|| loop {
            /* 只要在放（壳播或自家解码）就压住自动待机 / 自动关屏。
             * 屏幕真被系统关掉之后按键就不再报给应用了，黑屏下的肩键
             * 换曲会跟着一起失效，所以息屏这件事只交给 Start 键做。 */
            let shell_playing =
                SHELL_ACTIVE.load(Ordering::Acquire) && SHELL_WANT_PLAY.load(Ordering::Acquire);
            let local_playing = PLAYING.load(Ordering::Acquire) && !PAUSED.load(Ordering::Acquire);
            if shell_playing || local_playing {
                unsafe { sceKernelPowerTick(POWER_TICK_DISABLE_AUTO_SUSPEND) };
                unsafe { sceKernelPowerTick(POWER_TICK_DISABLE_OLED_OFF) };
            }
            unsafe { vitasdk_sys::sceKernelDelayThread(10_000_000) };
        });
}

/// 应用事件监听线程：收到 `REQUEST_QUIT`（用户从 LiveArea 关掉应用）时把 shell
/// 那条播放停掉并结束会话 —— "彻底退出应用，音乐就停"靠的就是这里。
/// 做法与 ElevenMPV-A 的 AppWatchdogTask 一致。
fn spawn_app_event_watch() {
    let _ = std::thread::Builder::new()
        .name("yunyin-appev".into())
        .stack_size(32 * 1024)
        .spawn(|| {
            append_log("app event: watchdog started");
            let mut num_err_logged = false;
            let mut unknown_logged = 0u32;
            loop {
                let mut count: i32 = 0;
                let rc = unsafe { sceAppMgrReceiveEventNum(&mut count) };
                if rc < 0 {
                    /* 接口报错只记一次，免得刷屏 */
                    if !num_err_logged {
                        num_err_logged = true;
                        append_log(&format!("app event: ReceiveEventNum -> 0x{:08X}", rc as u32));
                    }
                    unsafe { vitasdk_sys::sceKernelDelayThread(100_000) };
                    continue;
                }
                for _ in 0..count.max(0) {
                    let mut event = SceAppMgrEvent {
                        event: 0,
                        app_id: 0,
                        param: [0u8; 56],
                    };
                    if unsafe { sceAppMgrReceiveEvent(&mut event) } < 0 {
                        break;
                    }
                    match event.event {
                        SCE_APP_EVENT_REQUEST_QUIT => {
                            append_log("app event: request quit -> 停止 shell 播放");
                            shell_audio_stop();
                            shell_audio_terminate();
                        }
                        SCE_APP_EVENT_ON_DEACTIVATE => append_log("app event: deactivate"),
                        SCE_APP_EVENT_ON_ACTIVATE => append_log("app event: activate"),
                        other => {
                            if unknown_logged < 10 {
                                unknown_logged += 1;
                                append_log(&format!("app event: 0x{:08X}", other as u32));
                            }
                        }
                    }
                }
                unsafe { vitasdk_sys::sceKernelDelayThread(100_000) };
            }
        });
}

/// 问 shell 这首歌的时长（毫秒）；失败返回 0，JS 会退回曲库里的时长。
fn shell_audio_duration() -> u32 {
    let Some((obj, control, t1, t2)) = shell_ctx() else {
        return 0;
    };
    let mut track = ShellAudioTracking {
        unk00: 0,
        tracking1: t1,
        tracking2: t2,
    };
    let mut params = ShellAudioParams::new();
    params.params1 = &mut track as *mut _ as *mut c_void;
    params.params1_size = 0xC;
    let mut buf = [0u8; 0x828];
    let mut out_params = ShellAudioParams::new();
    out_params.params1 = buf.as_mut_ptr() as *mut c_void;
    out_params.params1_size = buf.len() as u32;
    let ret = unsafe {
        shell_audio_call(
            obj,
            control,
            EV_MUSIC_TRACK_INFO,
            &mut params,
            1,
            &mut out_params,
        )
    };
    if ret != 0 {
        return 0;
    }
    u32::from_ne_bytes([buf[8], buf[9], buf[10], buf[11]])
}

/// 时长：优先问 shell；拿不到就用自家解码器读一下文件头（不播放，只是解析）。
/// 时长报 0 会让上层把它当成"1 毫秒"，于是每首歌一播就被判定为放完。
fn shell_audio_duration_or_probe(path: &str) -> u32 {
    let from_shell = shell_audio_duration();
    if from_shell > 0 {
        return from_shell;
    }
    let Ok(c_path) = std::ffi::CString::new(path) else {
        return 0;
    };
    if unsafe { yp_open(c_path.as_ptr()) } != 0 {
        return 0;
    }
    let rate = (unsafe { yp_rate() }).max(1) as u64;
    let frames = (unsafe { yp_length() }).max(0) as u64;
    unsafe { yp_close() };
    ((frames * 1000) / rate) as u32
}

/// 用 shell 播这首（仅支持的格式）。成功之后位置/状态都从 shell 读。
fn shell_audio_start(path: &str) -> bool {
    if !shell_audio_session() {
        return false;
    }
    let Some((obj, control, t1, t2)) = shell_ctx() else {
        return false;
    };
    /* 以 0x80 档申请 BGM 端口（参考项目 ElevenMPV-A 就是这么做的）：
     * 那是"应用通过 shell 播放"的档位，shell 会把我们当成正式的 BGM
     * 提供者 —— LiveArea 上那套系统音乐控件才会出现（shell 插件要往里
     * 写歌名/歌手的正是那几个控件）。
     * 之前被系统拒绝（0x8080201F）是因为还没有 SceShell 权限，现在重试。 */
    /* 只在还没握住端口时申请一次；换歌（同一会话内）不重复申请，
     * 否则黑屏时会话重建会丢掉"后台出声"的特权。 */
    if !HOLDING_BGM.swap(true, Ordering::AcqRel) {
        unsafe { sceAppMgrReleaseBgmPort() };
        let port_ret = unsafe { sceAppMgrAcquireBgmPortWithPriority(0x80) };
        append_log(&format!(
            "shell audio: AcquireBgmPortWithPriority(0x80) -> 0x{:08X}",
            port_ret as u32
        ));
    }
    let mut path_buf = path.as_bytes().to_vec();
    let mut track = ShellAudioTracking {
        unk00: 0,
        tracking1: t1,
        tracking2: t2,
    };
    let mut opt = ShellAudioOpt {
        flag: 0,
        param1: 0,
        param2: 0,
        param3: 0,
    };
    let mut params = ShellAudioParams::new();
    params.params1 = &mut track as *mut _ as *mut c_void;
    params.params1_size = 0xC;
    params.params2 = path_buf.as_mut_ptr() as *mut c_void;
    params.params2_size = path_buf.len() as u32;
    params.params3 = &mut opt as *mut _ as *mut c_void;
    params.params3_size = 0x10;
    let ret = unsafe {
        shell_audio_call(
            obj,
            control,
            EV_MUSIC_OPEN,
            &mut params,
            3,
            core::ptr::null_mut(),
        )
    };
    if ret != 0 {
        append_log(&format!("shell audio: open -> 0x{:08X}", ret as u32));
        return false;
    }
    SHELL_POS_MS.store(0, Ordering::Release);
    SHELL_STATE.store(SCE_MUSIC_EVENT_PLAY as u32, Ordering::Release);
    SHELL_ENDED.store(false, Ordering::Release);
    SHELL_ACTIVE.store(true, Ordering::Release);
    DUR_MS.store(shell_audio_duration_or_probe(path), Ordering::Release);
    SHELL_WANT_PLAY.store(true, Ordering::Release);
    shell_audio_command(SCE_MUSIC_EVENT_PLAY);
    /* 起播要等 shell 一拍，确认它真的在放再交给它；否则退回自家解码器，
     * 免得出现"看上去成功了但没声音"。 */
    if !shell_audio_wait_playing(20) {
        append_log("shell audio: shell 没有开始播放，退回软件解码");
        /* 没播起来就别占着 BGM 端口（否则系统音乐/别的应用会被挡住） */
        HOLDING_BGM.store(false, Ordering::Release);
        unsafe { sceAppMgrReleaseBgmPort() };
        SHELL_ACTIVE.store(false, Ordering::Release);
        return false;
    }
    append_log(&format!(
        "shell audio: playing {} (dur={}ms)",
        path,
        DUR_MS.load(Ordering::Acquire)
    ));
    /* 交给 shell 托管这段播放（enable BackGround Music）。 */
    let bgm_ret = unsafe { sceShellUtilSetBGMMode(1) };
    append_log(&format!("shell audio: SetBGMMode(1) -> 0x{:08X}", bgm_ret as u32));
    true
}

/// 等 shell 进入"播放中"状态，最多 tries × 50ms。
fn shell_audio_wait_playing(tries: u32) -> bool {
    for _ in 0..tries {
        unsafe { vitasdk_sys::sceKernelDelayThread(50_000) };
        shell_audio_poll();
        if SHELL_STATE.load(Ordering::Acquire) == SCE_MUSIC_EVENT_PLAY as u32 {
            return true;
        }
    }
    false
}

/// 停止 shell 那条播放（换歌、按停止、退回软件解码时调用）。
fn shell_audio_stop() {
    if !SHELL_ACTIVE.swap(false, Ordering::AcqRel) {
        return;
    }
    SHELL_WANT_PLAY.store(false, Ordering::Release);
    shell_audio_command(SCE_MUSIC_EVENT_STOP);
    /* 注意：这里不释放 BGM 端口、也不关 BGM 模式。
     * 换歌走的就是这条路（stop -> load -> play），如果把会话拆了重建，
     * 黑屏时重建出来的会话拿不回"后台出声"的特权，就会出现
     * "关屏换歌没声音、亮屏才恢复"。真正暂停时（js_pause）才让出端口。 */
}

/// 用户真正暂停时让出 BGM 端口与 BGM 模式（背景：让系统音乐/别的应用
/// 能重新使用 BGM；参考项目也是在"暂停 + 切到后台"时释放）。
fn shell_audio_yield_bgm() {
    HOLDING_BGM.store(false, Ordering::Release);
    unsafe { sceShellUtilSetBGMMode(0) };
    unsafe { sceAppMgrReleaseBgmPort() };
}

/// 读一次 shell 的播放状态，刷新位置 / 状态 / 是否放完。
fn shell_audio_poll() {
    let Some((obj, control, t1, t2)) = shell_ctx() else {
        return;
    };
    let mut track = ShellAudioTracking {
        unk00: 0,
        tracking1: t1,
        tracking2: t2,
    };
    let mut params = ShellAudioParams::new();
    params.params1 = &mut track as *mut _ as *mut c_void;
    params.params1_size = 0xC;
    let mut buf = [0u8; 0x70];
    let mut out_params = ShellAudioParams::new();
    out_params.params1 = buf.as_mut_ptr() as *mut c_void;
    out_params.params1_size = buf.len() as u32;
    let ret = unsafe {
        shell_audio_call(
            obj,
            control,
            EV_MUSIC_STATUS,
            &mut params,
            1,
            &mut out_params,
        )
    };
    if ret != 0 {
        return;
    }
    let state = i32::from_ne_bytes([buf[0x10], buf[0x11], buf[0x12], buf[0x13]]);
    let time = u32::from_ne_bytes([buf[0x2C], buf[0x2D], buf[0x2E], buf[0x2F]]);
    SHELL_STATE.store(state as u32, Ordering::Release);
    if state == SCE_MUSIC_EVENT_PLAY {
        SHELL_POS_MS.store(time, Ordering::Release);
        SHELL_ENDED.store(false, Ordering::Release);
    } else if time > 0 {
        /* 停住但位置还在：这不是放完，是被外部打断（息屏 / 电源键 / 系统
         * 切走音频焦点）。参考 ElevenMPV-A 的做法：用户还想听就补发一次
         * PLAY 让它接着放，否则界面会出现"按钮是播放状态但进度不动"。 */
        SHELL_POS_MS.store(time, Ordering::Release);
        if SHELL_WANT_PLAY.load(Ordering::Acquire) {
            shell_audio_command(SCE_MUSIC_EVENT_PLAY);
        }
    } else {
        /* 位置被清零：可能是放完了，也可能是被外力打断（息屏时系统会把
         * 位置一起清掉）。只有"刚才已经放到接近总时长"才算真的放完，
         * 其余情况一律续播 —— 否则息屏就会永久停在那里。 */
        let last = SHELL_POS_MS.load(Ordering::Acquire);
        let dur = DUR_MS.load(Ordering::Acquire);
        if dur > 0 && last + 1000 >= dur {
            SHELL_ENDED.store(true, Ordering::Release);
        } else if SHELL_WANT_PLAY.load(Ordering::Acquire) {
            shell_audio_command(SCE_MUSIC_EVENT_PLAY);
        }
    }
}

fn set_path(p: &str) {
    if let Ok(mut g) = PATH_BUF.lock() {
        g.clear();
        g.push_str(p);
        PATH_LEN.store(g.len(), Ordering::Release);
    }
}

fn get_path() -> String {
    PATH_BUF.lock().map(|g| g.clone()).unwrap_or_default()
}

fn json_escape(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            _ => o.push(c),
        }
    }
    o
}

fn synchsafe(b: &[u8]) -> usize {
    if b.len() < 4 {
        return 0;
    }
    (((b[0] as usize) & 0x7f) << 21)
        | (((b[1] as usize) & 0x7f) << 14)
        | (((b[2] as usize) & 0x7f) << 7)
        | ((b[3] as usize) & 0x7f)
}

fn be32(b: &[u8]) -> usize {
    if b.len() < 4 {
        return 0;
    }
    ((b[0] as usize) << 24) | ((b[1] as usize) << 16) | ((b[2] as usize) << 8) | (b[3] as usize)
}

fn le32(b: &[u8]) -> usize {
    if b.len() < 4 {
        return 0;
    }
    (b[0] as usize) | ((b[1] as usize) << 8) | ((b[2] as usize) << 16) | ((b[3] as usize) << 24)
}

fn cstr_skip(p: &[u8]) -> Option<usize> {
    p.iter().position(|&c| c == 0).map(|i| i + 1)
}

/// Vitawave metadata_get_album_art_texture: JPEG 0xFFD8 / PNG 89 50 4E 47.
fn image_payload(p: &[u8]) -> Option<Vec<u8>> {
    let mut i = 0usize;
    while i + 3 < p.len() {
        if p[i] == 0xff && p[i + 1] == 0xd8 {
            let rest = &p[i..];
            if !rest.is_empty() && rest.len() <= MAX_ART {
                return Some(rest.to_vec());
            }
        }
        if i + 7 < p.len() && p[i] == 0x89 && p[i + 1] == b'P' && p[i + 2] == b'N' && p[i + 3] == b'G' {
            let rest = &p[i..];
            if !rest.is_empty() && rest.len() <= MAX_ART {
                return Some(rest.to_vec());
            }
        }
        i += 1;
    }
    None
}

fn resample_stereo(pcm: &[i16], rate: u32, ch: u32) -> Vec<i16> {
    if pcm.is_empty() {
        return Vec::new();
    }
    let frames = if ch == 1 { pcm.len() } else { pcm.len() / 2 };
    if rate == TARGET_RATE && ch == 2 {
        return pcm.to_vec();
    }
    let out_frames = ((frames as u64) * (TARGET_RATE as u64) / (rate.max(1) as u64)) as usize;
    let mut out = vec![0i16; out_frames.saturating_mul(2)];
    for i in 0..out_frames {
        let src = (i as u64) * (rate as u64) / (TARGET_RATE as u64);
        let s = (src as usize).min(frames.saturating_sub(1));
        let (l, r) = if ch == 1 {
            (pcm[s], pcm[s])
        } else {
            (pcm[s * 2], pcm[s * 2 + 1])
        };
        out[i * 2] = l;
        out[i * 2 + 1] = r;
    }
    out
}

fn pump_stream(path: &str) {
    let c_path = match std::ffi::CString::new(path) {
        Ok(c) => c,
        Err(_) => {
            PLAYING.store(false, Ordering::Release);
            return;
        }
    };
    if unsafe { yp_open(c_path.as_ptr()) } != 0 {
        PLAYING.store(false, Ordering::Release);
        return;
    }

    let rate = (unsafe { yp_rate() }).max(0) as u32;
    let total_frames = (unsafe { yp_length() }).max(0) as u64;
    let duration_ms = ((total_frames as u64) * 1000 / (rate.max(1) as u64)) as u32;
    unsafe {
        let _ = audio::start(TARGET_RATE);
        audio::flush();
    }
    /* 环形缓冲总容量：缓冲空着时 free_frames() 就是容量（44.1kHz 下约
     * 743ms）。解码线程总是抢在播放前面把音频灌满这个缓冲，所以“解码到哪
     * 一帧”比“听众听到哪一帧”要早一大截 —— 两者之差就是还没播出去的部分。 */
    let ring_cap = audio::free_frames() as u64;

    RATE_HZ.store(TARGET_RATE, Ordering::Release);
    DUR_MS.store(duration_ms, Ordering::Release);
    POS_MS.store(0, Ordering::Release);
    PLAYING.store(true, Ordering::Release);
    PAUSED.store(false, Ordering::Release);

    /* yplayer outputs stereo (2ch) frames at the file's native rate. */
    let mut buf = vec![0i16; 1024 * 2];
    let mut at: u64 = 0;
    let mut eof = false;
    let mut held = false; /* 暂停已经落地：声音已停、解码器已退回 */
    let queued = || ring_cap.saturating_sub(audio::free_frames() as u64);
    let to_ms = |f: u64| -> u32 { ((f * 1000) / (rate.max(1) as u64)) as u32 };
    loop {
        if STOP.load(Ordering::Acquire) {
            break;
        }
        if PAUSED.load(Ordering::Acquire) {
            if !held {
                /* 暂停要同时做到两件事：
                 *   1) 马上安静 —— 只置标志的话，缓冲里最多 743ms 的音频还会
                 *      继续放完，听起来就是“按了暂停还要等一会”；
                 *   2) 能原地续播 —— 丢掉的那段要在恢复时补回来，所以把解码器
                 *      退回听众真正听到的位置，而不是接在被丢掉的那段后面。 */
                let heard = at.saturating_sub(queued());
                audio::flush();
                if heard < at && unsafe { yp_seek(heard as i64) } == 0 {
                    at = heard;
                }
                POS_MS.store(to_ms(at), Ordering::Release);
                held = true;
            }
            unsafe { vitasdk_sys::sceKernelDelayThread(8_000) };
            continue;
        }
        held = false;
        let free = audio::free_frames();
        if free < 1024 {
            unsafe { vitasdk_sys::sceKernelDelayThread(4_000) };
            continue;
        }
        let want = free.min(1024);
        let got = unsafe { yp_decode(buf.as_mut_ptr(), want as i32) };
        if got <= 0 {
            eof = true;
            break;
        }
        let frames = got as usize;
        let stereo = resample_stereo(&buf[..frames * 2], rate, 2);
        unsafe { audio::push(&stereo, 2) };
        at += frames as u64;
        /* 上报“正在响”的位置（进度条 / 歌词才对得上声音）。 */
        POS_MS.store(to_ms(at.saturating_sub(queued())), Ordering::Release);
    }

    // On a natural end-of-stream, let the queued tail finish playing out, then
    // pin pos to the full duration so the JS frame loop reliably hits
    // pos >= dur and advances / loops (without cutting the last fraction of a
    // second off the song).  A manual STOP does not pin, so the stopped
    // position stays put.
    if eof {
        let mut waited = 0u32;
        while queued() > 256 && waited < 400 {
            if STOP.load(Ordering::Acquire) {
                break;
            }
            if PAUSED.load(Ordering::Acquire) {
                /* 尾巴还没放完就暂停：停在听众听到的位置，恢复后再放完
                 * （暂停的时间不计入等待上限）。 */
                unsafe { vitasdk_sys::sceKernelDelayThread(8_000) };
                continue;
            }
            unsafe { vitasdk_sys::sceKernelDelayThread(5_000) };
            waited += 1;
        }
        if !STOP.load(Ordering::Acquire) && !PAUSED.load(Ordering::Acquire) {
            POS_MS.store(duration_ms, Ordering::Release);
        }
    }
    PLAYING.store(false, Ordering::Release);
    unsafe { yp_close() };
}

fn spawn_play(path: String) {
    STOP.store(true, Ordering::Release);
    for _ in 0..50 {
        if !WORKER.load(Ordering::Acquire) {
            break;
        }
        unsafe { vitasdk_sys::sceKernelDelayThread(4_000) };
    }
    STOP.store(false, Ordering::Release);
    set_path(&path);
    WORKER.store(true, Ordering::Release);
    let _ = std::thread::Builder::new()
        .name("yunyin-dec".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            pump_stream(&path);
            WORKER.store(false, Ordering::Release);
        });
}

/// List a directory using SceIo directly (via the C shim).  Real hardware's
/// SceIo is stricter than Vita3K about the `device:path` form, so the shim
/// tries a few accepted spellings and logs which one worked / the error.  This
/// avoids Rust's `std::fs::read_dir`, which is unreliable on the real device
/// for directory streams (file open via std::fs works, dir listing does not).
fn list_dir(path: &str) -> String {
    let c_path = match std::ffi::CString::new(path) {
        Ok(c) => c,
        Err(_) => return "[]".to_string(),
    };
    let mut buf = vec![0u8; 256 * 1024];
    let n = unsafe {
        yunyin_list_dir(
            c_path.as_ptr() as *const u8,
            buf.as_mut_ptr(),
            buf.len() as i32,
        )
    };
    if n <= 0 {
        return "[]".to_string();
    }
    let n = (n as usize).min(buf.len());
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

fn walk_id3_frames(bytes: &[u8], mut on_frame: impl FnMut(&[u8], &[u8])) {
    if bytes.len() < 10 || &bytes[0..3] != b"ID3" {
        return;
    }
    let ver = bytes[3];
    let tag_size = synchsafe(&bytes[6..10]);
    let mut pos = 10usize;
    let end = (10 + tag_size).min(bytes.len());
    if bytes[5] & 0x40 != 0 && pos + 4 <= end {
        let ext = if ver >= 4 {
            synchsafe(&bytes[pos..pos + 4])
        } else {
            be32(&bytes[pos..pos + 4])
        };
        pos = (pos + ext.max(4)).min(end);
    }
    while pos + 10 <= end {
        if bytes[pos] == 0 {
            break;
        }
        let id = &bytes[pos..pos + 4];
        let fsize = if ver >= 4 {
            synchsafe(&bytes[pos + 4..pos + 8])
        } else {
            be32(&bytes[pos + 4..pos + 8])
        };
        pos += 10;
        if fsize == 0 || pos + fsize > end {
            break;
        }
        on_frame(id, &bytes[pos..pos + fsize]);
        pos += fsize;
    }
}

fn extract_id3_apic(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut found = None;
    walk_id3_frames(bytes, |id, data| {
        if found.is_some() || id != b"APIC" || data.len() < 4 {
            return;
        }
        let enc = data[0];
        let utf16 = enc == 1 || enc == 2;
        let mut p = &data[1..];
        let Some(n) = cstr_skip(p) else {
            return;
        };
        p = &p[n..];
        if p.is_empty() {
            return;
        }
        p = &p[1..];
        if utf16 {
            let mut i = 0;
            while i + 1 < p.len() {
                if p[i] == 0 && p[i + 1] == 0 {
                    p = &p[i + 2..];
                    break;
                }
                i += 2;
            }
            if i + 1 >= p.len() && !(p.len() >= 2 && p[0] == 0 && p[1] == 0) {
                return;
            }
        } else {
            let Some(n) = cstr_skip(p) else {
                return;
            };
            p = &p[n..];
        }
        if p.is_empty() || p.len() > MAX_ART {
            return;
        }
        if let Some(img) = image_payload(p) {
            found = Some(img);
        } else if let Some(img) = image_payload(data) {
            found = Some(img);
        }
    });
    found
}

fn extract_id3_text(bytes: &[u8], want: &[u8; 4]) -> String {
    let mut out = String::new();
    walk_id3_frames(bytes, |id, data| {
        if !out.is_empty() || id != want || data.is_empty() {
            return;
        }
        let enc = data[0];
        let txt = &data[1..];
        if enc == 3 || enc == 0 {
            out = String::from_utf8_lossy(txt)
                .trim_matches('\0')
                .trim()
                .to_string();
        } else if txt.len() >= 2 {
            let mut i = 0;
            if txt[0] == 0xff && txt[1] == 0xfe {
                i = 2;
            } else if txt[0] == 0xfe && txt[1] == 0xff {
                i = 2;
            }
            let mut u = String::new();
            while i + 1 < txt.len() {
                let cp = u16::from_le_bytes([txt[i], txt[i + 1]]);
                i += 2;
                if cp == 0 {
                    break;
                }
                if let Some(c) = char::from_u32(cp as u32) {
                    u.push(c);
                }
            }
            out = u.trim().to_string();
        }
    });
    out
}

/// 按 ID3v2 编码字节（data[0]）解码从 `start` 开始的字符串，遇到 `\0` 结束。
fn decode_id3_from(data: &[u8], enc: u8, start: usize) -> String {
    let txt = if start < data.len() { &data[start..] } else { &[] };
    if txt.is_empty() {
        return String::new();
    }
    if enc == 3 || enc == 0 {
        return String::from_utf8_lossy(txt)
            .trim_matches('\0')
            .trim()
            .to_string();
    }
    let mut little = true;
    let mut i = 0usize;
    if txt.len() >= 2 {
        if txt[0] == 0xff && txt[1] == 0xfe {
            i = 2;
        } else if txt[0] == 0xfe && txt[1] == 0xff {
            little = false;
            i = 2;
        }
    }
    let mut u = String::new();
    while i + 1 < txt.len() {
        let cp = if little {
            u16::from_le_bytes([txt[i], txt[i + 1]])
        } else {
            u16::from_be_bytes([txt[i], txt[i + 1]])
        };
        i += 2;
        if cp == 0 {
            break;
        }
        if let Some(c) = char::from_u32(cp as u32) {
            u.push(c);
        }
    }
    u.trim().to_string()
}

/// 返回 ID3v2 字符串的结束下标（含终止符）：enc 1/2 为 UTF-16 双字节 `00 00`。
fn id3_cstr_end(data: &[u8], enc: u8, start: usize) -> usize {
    if enc == 1 || enc == 2 {
        let mut i = start;
        while i + 1 < data.len() {
            if data[i] == 0 && data[i + 1] == 0 {
                return i + 2;
            }
            i += 2;
        }
        data.len()
    } else {
        cstr_skip(&data[start..])
            .map(|n| start + n)
            .unwrap_or(data.len())
    }
}

/// 读取 TXXX（自定义文本帧）里描述为 wanted 的取值，如 lyrics-eng / lyrics-XXX。
fn extract_txxx(bytes: &[u8], wanted: &[&str]) -> String {
    let mut out = String::new();
    walk_id3_frames(bytes, |id, data| {
        if !out.is_empty() || id != b"TXXX" || data.len() < 2 {
            return;
        }
        let enc = data[0];
        let desc_start = 1;
        let desc_end = id3_cstr_end(data, enc, desc_start);
        if desc_end > data.len() {
            return;
        }
        let desc = decode_id3_from(data, enc, desc_start).to_lowercase();
        if !wanted.iter().any(|w| desc == w.to_lowercase()) {
            return;
        }
        out = decode_id3_from(data, enc, desc_end);
    });
    out
}

/// 读取标准 USLT（非同步歌词）帧：encoding + 3 字节语言 + 描述符 + 正文。
fn extract_uslt(bytes: &[u8]) -> String {
    let mut out = String::new();
    walk_id3_frames(bytes, |id, data| {
        if !out.is_empty() || id != b"USLT" || data.len() < 5 {
            return;
        }
        let enc = data[0];
        let desc_start = 4;
        let desc_end = id3_cstr_end(data, enc, desc_start);
        if desc_end > data.len() {
            return;
        }
        out = decode_id3_from(data, enc, desc_end);
    });
    out
}

/// 尽力从 MP3/OGG 里取歌词文本。优先级：USLT → TXXX(lyrics-eng/XXX/LYRICS) → OGG LYRICS。
fn extract_embedded_lyrics(bytes: &[u8]) -> String {
    let uslt = extract_uslt(bytes);
    if !uslt.trim().is_empty() {
        return uslt;
    }
    let txxx = extract_txxx(
        bytes,
        &[
            "lyrics-eng",   // 主流中文/英文 MP3（如网易云/百度）自定义帧
            "lyrics-xxx",   // 同上，大小写不敏感
            "lyrics",
            "lyrics3",
            "unsync",
            "unsyncedlyrics",
            "eng",
            "xxx",
        ],
    );
    if !txxx.trim().is_empty() {
        return txxx;
    }
    if bytes.len() >= 4 && &bytes[0..4] == b"OggS" {
        return extract_ogg_text(bytes, "LYRICS");
    }
    String::new()
}

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut vals = [255u8; 256];
    for (i, &c) in B64.iter().enumerate() {
        vals[c as usize] = i as u8;
    }
    vals[b'=' as usize] = 0;
    let clean: Vec<u8> = s
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    if clean.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(clean.len() / 4 * 3);
    let mut i = 0;
    while i + 3 < clean.len() {
        let a = vals[clean[i] as usize];
        let b = vals[clean[i + 1] as usize];
        let c = vals[clean[i + 2] as usize];
        let d = vals[clean[i + 3] as usize];
        if a == 255 || b == 255 || c == 255 || d == 255 {
            return None;
        }
        out.push((a << 2) | (b >> 4));
        if clean[i + 2] != b'=' {
            out.push((b << 4) | (c >> 2));
        }
        if clean[i + 3] != b'=' {
            out.push((c << 6) | d);
        }
        i += 4;
    }
    Some(out)
}

fn flac_picture_data(block: &[u8]) -> Option<Vec<u8>> {
    if block.len() < 32 {
        return None;
    }
    let mut i = 4usize;
    let mime_len = be32(&block[i..]);
    i += 4 + mime_len;
    if i + 4 > block.len() {
        return None;
    }
    let desc_len = be32(&block[i..]);
    i += 4 + desc_len + 16;
    if i + 4 > block.len() {
        return None;
    }
    let data_len = be32(&block[i..]);
    i += 4;
    if data_len == 0 || data_len > MAX_ART || i + data_len > block.len() {
        return None;
    }
    Some(block[i..i + data_len].to_vec())
}

fn extract_ogg_picture(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut i = 0usize;
    let mut packet: Vec<u8> = Vec::new();
    while i + 27 <= bytes.len() {
        if &bytes[i..i + 4] != b"OggS" {
            i += 1;
            continue;
        }
        let nsegs = bytes[i + 26] as usize;
        let hdr = i + 27;
        if hdr + nsegs > bytes.len() {
            break;
        }
        let mut payload = 0usize;
        let mut last = 0u8;
        for k in 0..nsegs {
            last = bytes[hdr + k];
            payload += last as usize;
        }
        let start = hdr + nsegs;
        let end = start + payload;
        if end > bytes.len() {
            break;
        }
        packet.extend_from_slice(&bytes[start..end]);
        i = end;
        if last == 255 {
            continue;
        }
        if packet.len() >= 7 && packet[0] == 3 && &packet[1..7] == b"vorbis" {
            let body = &packet[7..];
            if body.len() < 8 {
                break;
            }
            let vendor = le32(body);
            let mut p = 4 + vendor;
            if p + 4 > body.len() {
                break;
            }
            let count = le32(&body[p..]);
            p += 4;
            for _ in 0..count {
                if p + 4 > body.len() {
                    break;
                }
                let n = le32(&body[p..]);
                p += 4;
                if p + n > body.len() {
                    break;
                }
                let comment = core::str::from_utf8(&body[p..p + n]).unwrap_or("");
                p += n;
                if let Some((_, rest)) = comment.split_once('=') {
                    if comment.len() >= 24
                        && comment[..24].eq_ignore_ascii_case("METADATA_BLOCK_PICTURE=")
                    {
                        if let Some(block) = b64_decode(rest) {
                            if let Some(pic) = flac_picture_data(&block) {
                                return image_payload(&pic).or(Some(pic));
                            }
                        }
                    }
                }
            }
            break;
        }
        packet.clear();
    }
    None
}

fn extract_flac_picture(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 8 || &bytes[0..4] != b"fLaC" {
        return None;
    }
    let mut i = 4usize;
    loop {
        if i + 4 > bytes.len() {
            break;
        }
        let last = bytes[i] & 0x80 != 0;
        let typ = bytes[i] & 0x7f;
        let size = ((bytes[i + 1] as usize) << 16) | ((bytes[i + 2] as usize) << 8) | (bytes[i + 3] as usize);
        i += 4;
        if i + size > bytes.len() {
            break;
        }
        if typ == 6 {
            if let Some(pic) = flac_picture_data(&bytes[i..i + size]) {
                return image_payload(&pic).or(Some(pic));
            }
        }
        i += size;
        if last {
            break;
        }
    }
    None
}

fn extract_ogg_text(bytes: &[u8], key: &str) -> String {
    let mut i = 0usize;
    let mut packet: Vec<u8> = Vec::new();
    let mut out = String::new();
    while i + 27 <= bytes.len() {
        if &bytes[i..i + 4] != b"OggS" {
            i += 1;
            continue;
        }
        let nsegs = bytes[i + 26] as usize;
        let hdr = i + 27;
        if hdr + nsegs > bytes.len() {
            break;
        }
        let mut payload = 0usize;
        let mut last = 0u8;
        for k in 0..nsegs {
            last = bytes[hdr + k];
            payload += last as usize;
        }
        let start = hdr + nsegs;
        let end = start + payload;
        if end > bytes.len() {
            break;
        }
        packet.extend_from_slice(&bytes[start..end]);
        i = end;
        if last == 255 {
            continue;
        }
        if packet.len() >= 7 && packet[0] == 3 && &packet[1..7] == b"vorbis" {
            let body = &packet[7..];
            if body.len() < 8 {
                break;
            }
            let vendor = le32(body);
            let mut p = 4 + vendor;
            if p + 4 > body.len() {
                break;
            }
            let count = le32(&body[p..]);
            p += 4;
            for _ in 0..count {
                if p + 4 > body.len() {
                    break;
                }
                let n = le32(&body[p..]);
                p += 4;
                if p + n > body.len() {
                    break;
                }
                let comment = core::str::from_utf8(&body[p..p + n]).unwrap_or("");
                p += n;
                if let Some((k, rest)) = comment.split_once('=') {
                    if out.is_empty() && k.eq_ignore_ascii_case(key) {
                        out = rest.trim().to_string();
                    }
                }
            }
            break;
        }
        packet.clear();
    }
    out
}

fn extract_cover_bytes(bytes: &[u8]) -> Option<Vec<u8>> {
    extract_id3_apic(bytes)
        .or_else(|| extract_ogg_picture(bytes))
        .or_else(|| extract_flac_picture(bytes))
        .and_then(|raw| image_payload(&raw).or(Some(raw)))
}

fn decode_cover_rgba(art: &[u8]) -> Option<Vec<u8>> {
    let mut ptr: *mut u8 = core::ptr::null_mut();
    let mut w: i32 = 0;
    let mut h: i32 = 0;
    let rc = unsafe { yunyin_image_decode(art.as_ptr(), art.len() as i32, &mut ptr, &mut w, &mut h) };
    if rc != 0 || ptr.is_null() || w <= 0 || h <= 0 {
        if !ptr.is_null() {
            unsafe { yunyin_image_free(ptr) };
        }
        return None;
    }
    let src = unsafe { core::slice::from_raw_parts(ptr, (w as usize) * (h as usize) * 4) };
    let mut out = vec![0u8; (COVER_PX as usize) * (COVER_PX as usize) * 4];
    let ok = unsafe {
        yunyin_image_resize(src.as_ptr(), w, h, out.as_mut_ptr(), COVER_PX as i32, COVER_PX as i32)
    };
    unsafe { yunyin_image_free(ptr) };
    if ok != 0 {
        return None;
    }
    Some(out)
}

/// Read only a bounded prefix of the file (ID3v2 header-aware). Tags + embedded
/// artwork live near the start, so this avoids fs::read of the whole audio file.
fn read_prefix(path: &str) -> Vec<u8> {
    if let Ok(mut f) = std::fs::File::open(path) {
        let mut head = [0u8; 10];
        let head_len = f.read(&mut head).unwrap_or(0);
        let want = if head_len >= 10 && &head[0..3] == b"ID3" {
            let tag = ((head[6] as usize) << 21)
                | ((head[7] as usize) << 14)
                | ((head[8] as usize) << 7)
                | (head[9] as usize);
            10usize + tag
        } else if head_len >= 8 {
            // FLAC metadata blocks / OGG comment live near the start too.
            PREFIX_CAP / 2
        } else {
            head_len as usize
        };
        let n = want.min(PREFIX_CAP);
        let mut buf = vec![0u8; n];
        let _ = f.seek(SeekFrom::Start(0));
        let _ = f.read(&mut buf);
        buf
    } else {
        Vec::new()
    }
}

/// FNV-1a — stable, dependency-free hash for the cache filename.
fn fnv64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn cover_cache_path(path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let h = fnv64(path.as_bytes());
    Some(format!("ux0:/data/yunyin/covers/{:016x}.rgba", h))
}

fn ensure_cache_dir() {
    let _ = std::fs::create_dir_all("ux0:/data/yunyin/covers");
}

/// Load cached, decoded 256x256 RGBA if the source file is unchanged (its size
/// matches what we stored). Returns None on any failure — callers fall back to
/// a real decode, so the cache is never a correctness risk.
fn cover_cache_rgba(path: &str, size: u64) -> Option<Vec<u8>> {
    if size == 0 {
        return None;
    }
    let cp = cover_cache_path(path)?;
    let data = std::fs::read(&cp).ok()?;
    let expect = 8 + (COVER_PX as usize) * (COVER_PX as usize) * 4;
    if data.len() != expect {
        return None;
    }
    let stored = u64::from_ne_bytes(data[0..8].try_into().ok()?);
    if stored != size {
        return None;
    }
    Some(data[8..].to_vec())
}

/// Persist the decoded RGBA keyed by file size. Best-effort; errors are ignored
/// so a read-only / full card simply keeps using the in-memory decode path.
fn write_cover_cache(path: &str, size: u64, rgba: &[u8]) {
    if size == 0 || rgba.len() != (COVER_PX as usize) * (COVER_PX as usize) * 4 {
        return;
    }
    ensure_cache_dir();
    if let Some(cp) = cover_cache_path(path) {
        let mut buf = Vec::with_capacity(8 + rgba.len());
        buf.extend_from_slice(&size.to_ne_bytes());
        buf.extend_from_slice(rgba);
        let _ = std::fs::write(&cp, &buf);
    }
}

fn upload_cover(path: &str) -> i32 {
    if path.is_empty() {
        return -1;
    }
    if let Ok(mut g) = COVER_HANDLES.lock() {
        let map = g.get_or_insert_with(HashMap::new);
        if let Some(&h) = map.get(path) {
            return h;
        }
    }
    // fingerprint: source file size (cheap stat, no whole-file read).  The
    // persistent cache stores the decoded 256x256 RGBA so a later launch reuses
    // it without re-extracting / re-decoding the embedded JPEG.
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let cached = if size > 0 { cover_cache_rgba(path, size) } else { None };
    let rgba = match cached {
        Some(v) => v,
        None => {
            let prefix = read_prefix(path);
            if prefix.is_empty() {
                return -1;
            }
            let art = match extract_cover_bytes(&prefix) {
                Some(a) => a,
                None => return -1,
            };
            let v = match decode_cover_rgba(&art) {
                Some(p) => p,
                None => return -1,
            };
            if size > 0 {
                write_cover_cache(path, size, &v);
            }
            v
        }
    };
    let handle = unsafe {
        let h = crate::ffi::ui().upload_texture(&rgba, COVER_PX, COVER_PX, psm::PSM_8888);
        if h >= 0 {
            crate::graphics::register_texture(crate::ffi::ui(), h);
        }
        h
    };
    if handle >= 0 {
        if let Ok(mut g) = COVER_HANDLES.lock() {
            g.get_or_insert_with(HashMap::new).insert(path.to_string(), handle);
        }
    }
    handle
}

fn tags_json(path: &str) -> String {
    let bytes = read_prefix(path);
    if bytes.is_empty() {
        return "{\"title\":\"\",\"artist\":\"\",\"album\":\"\",\"cover\":false}".into();
    }
    let mut title = extract_id3_text(&bytes, b"TIT2");
    let mut artist = extract_id3_text(&bytes, b"TPE1");
    let mut album = extract_id3_text(&bytes, b"TALB");
    if title.is_empty() {
        title = extract_ogg_text(&bytes, "TITLE");
    }
    if artist.is_empty() {
        artist = extract_ogg_text(&bytes, "ARTIST");
    }
    if album.is_empty() {
        album = extract_ogg_text(&bytes, "ALBUM");
    }
    let cover = extract_cover_bytes(&bytes).is_some();
    let lyrics = extract_embedded_lyrics(&bytes);
    format!(
        "{{\"title\":\"{}\",\"artist\":\"{}\",\"album\":\"{}\",\"cover\":{},\"lyrics\":\"{}\"}}",
        json_escape(&title),
        json_escape(&artist),
        json_escape(&album),
        if cover { "true" } else { "false" },
        json_escape(&lyrics)
    )
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

unsafe extern "C" fn js_list(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let path = arg_string(ctx, argc, argv, 0);
    js_str(ctx, &list_dir(&path))
}

unsafe extern "C" fn js_roots(
    ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    js_str(
        ctx,
        "[{\"id\":\"app0\",\"path\":\"app0:music\"},{\"id\":\"ux0\",\"path\":\"ux0:\"},{\"id\":\"uma0\",\"path\":\"uma0:\"},{\"id\":\"ur0\",\"path\":\"ur0:\"}]",
    )
}

unsafe extern "C" fn js_play(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let path = arg_string(ctx, argc, argv, 0);
    if path.is_empty() {
        return JS_UNDEFINED;
    }
    /* 系统解码器认识的格式交给 shell 播：这样回到桌面音乐还在响。 */
    if shell_audio_supported(&path) && shell_audio_start(&path) {
        set_path(&path);
        return JS_UNDEFINED;
    }
    /* 其余格式（FLAC / OGG / OPUS…）走自家的软件解码器。 */
    shell_audio_stop();
    spawn_play(path);
    JS_UNDEFINED
}

unsafe extern "C" fn js_pause(
    _ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    if SHELL_ACTIVE.load(Ordering::Acquire) {
        SHELL_WANT_PLAY.store(false, Ordering::Release);
        shell_audio_command(SCE_MUSIC_EVENT_STOP);
        shell_audio_yield_bgm();
        return JS_UNDEFINED;
    }
    PAUSED.store(true, Ordering::Release);
    JS_UNDEFINED
}

unsafe extern "C" fn js_resume(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    if SHELL_ACTIVE.load(Ordering::Acquire) {
        SHELL_WANT_PLAY.store(true, Ordering::Release);
        shell_audio_command(SCE_MUSIC_EVENT_PLAY);
        return JS_UNDEFINED;
    }
    /* 解码线程还活着且确实处于暂停：只清标志位，让它从原地继续。
     * 关键是绝不能在这里重新 open 文件 —— 那就是“暂停后从头开始播”。 */
    if WORKER.load(Ordering::Acquire) && PAUSED.load(Ordering::Acquire) {
        PAUSED.store(false, Ordering::Release);
        PLAYING.store(true, Ordering::Release);
        return JS_UNDEFINED;
    }
    /* 没有可以续播的位置（播完了 / 已经被 stop）：给了路径就重新开一首。 */
    let path = arg_string(ctx, argc, argv, 0);
    if !path.is_empty() {
        spawn_play(path);
    }
    JS_UNDEFINED
}

unsafe extern "C" fn js_stop(
    _ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    shell_audio_stop();
    STOP.store(true, Ordering::Release);
    PAUSED.store(false, Ordering::Release);
    PLAYING.store(false, Ordering::Release);
    POS_MS.store(0, Ordering::Release);
    unsafe { audio::flush() };
    JS_UNDEFINED
}

fn dec_label() -> &'static str {
    match DEC_KIND.load(Ordering::Acquire) {
        DEC_HW => "hw",
        DEC_PCM => "pcm",
        _ => "sw",
    }
}

unsafe extern "C" fn js_state(
    ctx: *mut JSContext,
    _this: JSValue,
    _argc: i32,
    _argv: *mut JSValue,
) -> JSValue {
    let path = get_path();
    if SHELL_ACTIVE.load(Ordering::Acquire) {
        shell_audio_poll();
        let state = SHELL_STATE.load(Ordering::Acquire);
        let ended = SHELL_ENDED.load(Ordering::Acquire);
        let dur = DUR_MS.load(Ordering::Acquire);
        /* 放完时把位置钉在时长上，JS 那边才会走到"下一首"。 */
        let pos = if ended {
            dur
        } else {
            SHELL_POS_MS.load(Ordering::Acquire)
        };
        let json = format!(
            "{{\"playing\":{},\"paused\":{},\"path\":\"{}\",\"pos\":{},\"dur\":{},\"rate\":{},\"dec\":\"{}\"}}",
            if state == SCE_MUSIC_EVENT_PLAY as u32 && !ended {
                "true"
            } else {
                "false"
            },
            if state != SCE_MUSIC_EVENT_PLAY as u32 && !ended {
                "true"
            } else {
                "false"
            },
            json_escape(&path),
            pos,
            dur,
            TARGET_RATE,
            "shell"
        );
        return js_str(ctx, &json);
    }
    let json = format!(
        "{{\"playing\":{},\"paused\":{},\"path\":\"{}\",\"pos\":{},\"dur\":{},\"rate\":{},\"dec\":\"{}\"}}",
        if PLAYING.load(Ordering::Acquire) && !PAUSED.load(Ordering::Acquire) {
            "true"
        } else {
            "false"
        },
        if PAUSED.load(Ordering::Acquire) {
            "true"
        } else {
            "false"
        },
        json_escape(&path),
        POS_MS.load(Ordering::Acquire),
        DUR_MS.load(Ordering::Acquire),
        RATE_HZ.load(Ordering::Acquire),
        dec_label()
    );
    js_str(ctx, &json)
}

unsafe extern "C" fn js_cover(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let path = arg_string(ctx, argc, argv, 0);
    JS_NewInt32(ctx, upload_cover(&path))
}

unsafe extern "C" fn js_tags(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let path = arg_string(ctx, argc, argv, 0);
    js_str(ctx, &tags_json(&path))
}

unsafe extern "C" fn js_log(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let s = arg_string(ctx, argc, argv, 0);
    append_log(&s);
    JS_NewInt32(ctx, 0)
}

fn store_path(key: &str) -> String {
    format!("ux0:/data/yunyin/store/{}", key)
}

/// Read a small key-value blob (one file per key) under ux0:/data/yunyin/store.
/// Used to persist favorites / settings across sessions.  Fails -> "".
unsafe extern "C" fn js_store_get(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let key = arg_string(ctx, argc, argv, 0);
    let safe: String = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let data = std::fs::read(store_path(&safe)).unwrap_or_default();
    js_str(ctx, &String::from_utf8_lossy(&data))
}

unsafe extern "C" fn js_store_set(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let key = arg_string(ctx, argc, argv, 0);
    let val = arg_string(ctx, argc, argv, 1);
    let safe: String = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let _ = std::fs::create_dir_all("ux0:/data/yunyin/store");
    let _ = std::fs::write(store_path(&safe), val.as_bytes());
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

/// Install `globalThis.vitaMedia`.
///
/// # Safety
/// Same realm, render thread, once per guest.
pub unsafe fn register(ctx: *mut JSContext, global: JSValue) {
    /* 日志默认关：卡里没有 ux0:/data/yunyin/debug 这个文件就什么都不写。
     * 抓日志时建这个空文件、重开应用即可（正式版不留日志文件）。 */
    if std::fs::File::open(LOG_FLAG).is_ok() {
        LOG_ON.store(true, Ordering::Relaxed);
    }
    append_log("yunyin: start");
    /* 插件方案的前置检查：系统音乐播放器进程（NPXS19999）在不在。
     * 插件要靠它做宿主 —— 它不在的话，注入就无从谈起。 */
    for name in [b"NPXS19999\0".as_slice(), b"NPXS10008\0".as_slice()] {
        let mut pid: vitasdk_sys::SceUID = -1;
        let ret = unsafe { sceAppMgrGetIdByName(&mut pid, name.as_ptr() as *const i8) };
        let label = std::str::from_utf8(&name[..name.len() - 1]).unwrap_or("?");
        append_log(&format!(
            "probe: GetIdByName({}) -> ret=0x{:08X} pid=0x{:08X}",
            label, ret as u32, pid as u32
        ));
    }
    /* 先初始化 shell 事件系统：appmgr 的应用事件（激活/退出）要靠它。 */
    let init_ret = unsafe { sceShellUtilInitEvents(0) };
    append_log(&format!(
        "shell events init -> 0x{:08X}",
        init_ret as u32
    ));
    /* 监听 appmgr 应用事件：用户从 LiveArea 关掉应用时把 shell 播放收掉。 */
    spawn_app_event_watch();
    /* 播放期间挡住系统自动待机。 */
    spawn_power_tick_watch();
    /* 息屏/待机前把播放续上。 */
    register_power_callback();
    spawn_power_watch();
    let obj = JS_NewObject(ctx);
    add_fn(ctx, obj, b"list\0", js_list, 1);
    add_fn(ctx, obj, b"roots\0", js_roots, 0);
    add_fn(ctx, obj, b"play\0", js_play, 1);
    add_fn(ctx, obj, b"pause\0", js_pause, 0);
    add_fn(ctx, obj, b"resume\0", js_resume, 0);
    add_fn(ctx, obj, b"stop\0", js_stop, 0);
    add_fn(ctx, obj, b"state\0", js_state, 0);
    add_fn(ctx, obj, b"cover\0", js_cover, 1);
    add_fn(ctx, obj, b"tags\0", js_tags, 1);
    add_fn(ctx, obj, b"logMsg\0", js_log, 1);
    add_fn(ctx, obj, b"store_get\0", js_store_get, 1);
    add_fn(ctx, obj, b"store_set\0", js_store_set, 2);
    JS_SetPropertyStr(ctx, global, c"vitaMedia".as_ptr(), obj);
}

#[allow(dead_code)]
fn _keep_c_void(p: *mut c_void) {
    let _ = p;
}
