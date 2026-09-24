//! In-process BGM output.
//!
//! Behavioral baseline: ElevenMPVScrobbling
//!   `source/main.c`            sceAppMgrAcquireBgmPort / ReleaseBgmPort
//!   `source/audio/vitaaudiolib.c`  OpenPort(BGM) → callback thread → blocking Output
//!   `source/audio/audio.c`     Audio_Init / Audio_Decode / Audio_Pause / Audio_Term
//!
//! Decoder is YUNYIN `yplayer.c` (no xmp / FFmpeg). No libShellAudio, no ring,
//! no host `crate::audio`, no QUIT watchdog.

use crate::media::decoder;
use crate::media::platform::log;
use alloc::format;
use alloc::string::String;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread::JoinHandle;

const VITA_NUM_AUDIO_SAMPLES: i32 = 960;
const SCE_AUDIO_OUT_PORT_TYPE_BGM: i32 = 1;
const SCE_AUDIO_OUT_MODE_STEREO: i32 = 1;
const SCE_AUDIO_VOLUME_FLAG_L_CH: i32 = 1;
const SCE_AUDIO_VOLUME_FLAG_R_CH: i32 = 2;
const SCE_AUDIO_OUT_MAX_VOL: i32 = 0x8000;

extern "C" {
    fn sceAudioOutOpenPort(type_: i32, len: i32, freq: i32, mode: i32) -> i32;
    fn sceAudioOutOutput(port: i32, buf: *const c_void) -> i32;
    fn sceAudioOutReleasePort(port: i32) -> i32;
    fn sceAudioOutSetVolume(port: i32, ch: i32, vol: *mut i32) -> i32;
    fn sceAppMgrAcquireBgmPort() -> i32;
}

static PLAYING: AtomicBool = AtomicBool::new(false);
static PAUSED: AtomicBool = AtomicBool::new(false);
static AUDIO_TERMINATE: AtomicBool = AtomicBool::new(true);
static AUDIO_READY: AtomicBool = AtomicBool::new(false);
static BGM_HELD: AtomicBool = AtomicBool::new(false);
static PORT: AtomicI32 = AtomicI32::new(-1);
static PORT_RATE: AtomicI32 = AtomicI32::new(0);
static POS_MS: AtomicU32 = AtomicU32::new(0);
static DUR_MS: AtomicU32 = AtomicU32::new(0);
static RATE_HZ: AtomicU32 = AtomicU32::new(44100);
static PATH_LEN: AtomicUsize = AtomicUsize::new(0);
static PATH_BUF: Mutex<String> = Mutex::new(String::new());
static WORKER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
/*
 * 换歌锁：所有会动"当前播放会话"的操作（本地播放、在线打开、停止）都排队走这里。
 * 在线打开要花几秒，必须扔到后台线程做，界面才不会卡住；这把锁保证后台那一步
 * 和"用户又点了别的歌"不会同时改同一份状态。
 */
static SESSION: Mutex<()> = Mutex::new(());

fn lock_session() -> std::sync::MutexGuard<'static, ()> {
    SESSION.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn acquire_on_start() {
    if BGM_HELD.swap(true, Ordering::AcqRel) {
        return;
    }
    let ret = unsafe { sceAppMgrAcquireBgmPort() };
    log::append(&format!(
        "bgm: sceAppMgrAcquireBgmPort -> 0x{:08X}",
        ret as u32
    ));
}

pub fn session_active() -> bool {
    AUDIO_READY.load(Ordering::Acquire)
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

/// Audio_Decode: pause/stop fills silence; otherwise pull from the decoder.
fn audio_decode(buf: &mut [i16], frames: i32) {
    if !PLAYING.load(Ordering::Acquire) || PAUSED.load(Ordering::Acquire) {
        buf.fill(0);
        return;
    }
    /*
     * Phase 2 的 Gate（任务书 §8/§65）：在线源缓存不够时只输出静音，
     * **不调解码器**。这样"网络暂时没数据"永远不会被解码器当成流结束，
     * 断网时表现是"卡住缓冲"，而不是"这首歌放完了"。
     */
    if !crate::media::source::remote::gate_ok() {
        buf.fill(0);
        return;
    }
    let got = decoder::decode(buf, frames);
    if got <= 0 {
        PLAYING.store(false, Ordering::Release);
        POS_MS.store(DUR_MS.load(Ordering::Acquire), Ordering::Release);
        buf.fill(0);
        return;
    }
    let got = got as usize;
    let need = frames as usize;
    if got < need {
        for i in (got * 2)..(need * 2) {
            if i < buf.len() {
                buf[i] = 0;
            }
        }
    }
    POS_MS.store(decoder::position_ms(), Ordering::Release);
}

fn audio_out_blocking(port: i32, buf: &[i16]) {
    if !AUDIO_READY.load(Ordering::Acquire) || port < 0 {
        return;
    }
    let mut vol = [SCE_AUDIO_OUT_MAX_VOL, SCE_AUDIO_OUT_MAX_VOL];
    unsafe {
        sceAudioOutSetVolume(
            port,
            SCE_AUDIO_VOLUME_FLAG_L_CH | SCE_AUDIO_VOLUME_FLAG_R_CH,
            vol.as_mut_ptr(),
        );
        sceAudioOutOutput(port, buf.as_ptr() as *const c_void);
    }
}

/// vitaAudioChannelThread: callback then blocking BGM output, double buffer.
fn audio_channel_thread() {
    let mut buf_a = vec![0i16; (VITA_NUM_AUDIO_SAMPLES as usize) * 2];
    let mut buf_b = vec![0i16; (VITA_NUM_AUDIO_SAMPLES as usize) * 2];
    let mut use_a = true;
    {
        /* 面包屑：音频线程真的跑起来了就记一行（只记第一次）。 */
        static FIRST_TICK: AtomicBool = AtomicBool::new(true);
        if FIRST_TICK.swap(false, Ordering::AcqRel) {
            log::append("dbg: 音频线程第一拍");
        }
    }
    while AUDIO_TERMINATE.load(Ordering::Acquire) == false {
        let buf = if use_a {
            buf_a.as_mut_slice()
        } else {
            buf_b.as_mut_slice()
        };
        audio_decode(buf, VITA_NUM_AUDIO_SAMPLES);
        let port = PORT.load(Ordering::Acquire);
        audio_out_blocking(port, buf);
        use_a = !use_a;
    }
}

/// 结束当前会话：停音频线程 → 放掉 BGM 口 → 关在线源 → 关解码器。
///
/// **调用方必须持有 `SESSION` 锁**：音频线程被 join 掉之前，解码器句柄不能被换掉。
fn session_end() {
    let had_session = AUDIO_READY.load(Ordering::Acquire) || PORT.load(Ordering::Acquire) >= 0;
    AUDIO_READY.store(false, Ordering::Release);
    AUDIO_TERMINATE.store(true, Ordering::Release);
    if had_session {
        unsafe { vitasdk_sys::sceKernelDelayThread(100_000) };
    }
    if let Ok(mut g) = WORKER.lock() {
        if let Some(h) = g.take() {
            let _ = h.join();
        }
    }
    let port = PORT.swap(-1, Ordering::AcqRel);
    if port >= 0 {
        unsafe { sceAudioOutReleasePort(port) };
        log::append("bgm: sceAudioOutReleasePort");
    }
    PORT_RATE.store(0, Ordering::Release);
    /* 在线源要先关播放器、再放源（见 source::remote 的说明）。 */
    crate::media::source::remote::close_remote();
    decoder::close();
}

fn vita_audio_init(freq: i32) -> bool {
    AUDIO_TERMINATE.store(false, Ordering::Release);
    AUDIO_READY.store(false, Ordering::Release);
    let port = unsafe {
        sceAudioOutOpenPort(
            SCE_AUDIO_OUT_PORT_TYPE_BGM,
            VITA_NUM_AUDIO_SAMPLES,
            freq,
            SCE_AUDIO_OUT_MODE_STEREO,
        )
    };
    if port < 0 {
        log::append(&format!(
            "bgm: sceAudioOutOpenPort({freq}) -> 0x{:08X}",
            port as u32
        ));
        AUDIO_TERMINATE.store(true, Ordering::Release);
        return false;
    }
    PORT.store(port, Ordering::Release);
    PORT_RATE.store(freq, Ordering::Release);
    AUDIO_READY.store(true, Ordering::Release);
    let handle = std::thread::Builder::new()
        .name("yunyin-bgm".into())
        /*
         * 音频线程要走"mpg123 → 我们的 io 回调 → Rust 取数层"这条链，
         * 比一般的线程深；真机上出过一次栈顶落在 Rust 字符串格式化的崩溃，
         * 顺手把栈加厚一倍，别让解码路径贴着栈顶跑。
         */
        .stack_size(128 * 1024)
        .spawn(audio_channel_thread);
    match handle {
        Ok(h) => {
            if let Ok(mut g) = WORKER.lock() {
                *g = Some(h);
            }
            true
        }
        Err(_) => {
            log::append("bgm: audio thread spawn failed");
            AUDIO_READY.store(false, Ordering::Release);
            AUDIO_TERMINATE.store(true, Ordering::Release);
            unsafe { sceAudioOutReleasePort(port) };
            PORT.store(-1, Ordering::Release);
            false
        }
    }
}

/// Audio_Init: close previous session, open decoder, open BGM port at native rate.
pub fn play(path: &str) {
    /*
     * 在线曲目在界面里就是一个普通条目：它的 audioPath 直接是 URL。
     * 这里按前缀分流，界面完全不用知道"本地 / 在线"的区别。
     */
    if path.starts_with("http://") || path.starts_with("https://") {
        let referer = crate::media::source::remote::referer_for(path);
        play_url(path, &referer, -1);
        return;
    }
    let _guard = lock_session();
    /* 有正在后台打开的在线流的话，这次请求就是新的，让它作废。 */
    let _ = crate::media::source::remote::new_token();
    if path.is_empty() {
        return;
    }
    session_end();
    if !decoder::open(path) {
        log::append(&format!("bgm: yp_open failed {path}"));
        PLAYING.store(false, Ordering::Release);
        return;
    }
    let rate = decoder::rate();
    let dur = decoder::duration_ms();
    RATE_HZ.store(rate as u32, Ordering::Release);
    DUR_MS.store(dur, Ordering::Release);
    POS_MS.store(0, Ordering::Release);
    PLAYING.store(true, Ordering::Release);
    PAUSED.store(false, Ordering::Release);
    set_path(path);
    if !vita_audio_init(rate) {
        PLAYING.store(false, Ordering::Release);
        decoder::close();
        return;
    }
    log::append(&format!("bgm: play {path} rate={rate} dur={dur}ms"));
}

pub fn pause() {
    PAUSED.store(true, Ordering::Release);
}

/*
 * Phase 2：播放在线 URL。
 * 与 play(path) 的差别只有"谁提供字节"：路径换成 URL + Referer，
 * 其余（BGM 口、960 帧、状态上报）完全一样。
 *   duration_ms: 调用方给的时长提示（§37），-1 表示没有
 *
 * 关键一点：网络打开（DNS + TLS + 抓第一个窗口）可能要好几秒，**绝不能占着
 * 界面线程**。所以这里立刻返回，真正的打开放到后台线程；期间用户换歌、按停止
 * 都会让这次打开作废（序号变了就收手）。
 */
pub fn play_url(url: &str, referer: &str, duration_ms: i64) {
    if url.is_empty() {
        return;
    }
    let token = crate::media::source::remote::new_token();
    /* 立刻标成"还没在放"：界面上的进度条不会拿着上一首的数字发呆。 */
    PLAYING.store(false, Ordering::Release);
    PAUSED.store(false, Ordering::Release);

    let url_owned = String::from(url);
    let referer_owned = String::from(referer);
    let spawned = std::thread::Builder::new()
        .name("yunyin-net-open".into())
        .stack_size(96 * 1024) /* 打开阶段会走 yp_open_io → 解码器 → 取数层，同样要留余量 */
        .spawn(move || open_online(url_owned, referer_owned, duration_ms, token));
    if spawned.is_err() {
        log::append("bgm: 在线打开线程创建失败");
    }
}

/// 后台线程里的那一步：先收掉上一首，再打开在线源，最后起 BGM 口。
fn open_online(url: String, referer: String, duration_ms: i64, token: u32) {
    let _guard = lock_session();
    if !crate::media::source::remote::token_current(token) {
        return; /* 还没轮到我，就已经被新的播放请求取代了 */
    }
    session_end();
    if !crate::media::source::remote::token_current(token) {
        return;
    }
    if let Err(e) =
        crate::media::source::remote::open_remote(&url, &referer, duration_ms, token)
    {
        log::append(&format!("bgm: 在线打开失败 {:?} {url}", e));
        return;
    }
    /* 排障面包屑：这条链每一步单独记一行，崩了就知道停在哪一步（用完可删）。 */
    log::append("dbg: 在线源已接上，取格式");
    let rate = decoder::rate();
    log::append("dbg: 取时长（mpg123_length 会扫流）");
    let dur = decoder::duration_ms();
    log::append("dbg: 时长已取到");
    RATE_HZ.store(rate as u32, Ordering::Release);
    DUR_MS.store(dur, Ordering::Release);
    POS_MS.store(0, Ordering::Release);
    PLAYING.store(true, Ordering::Release);
    /* PAUSED 不动：用户在打开过程中按了暂停，就保持暂停。 */
    set_path(&url);
    log::append("dbg: 起音频口");
    if !vita_audio_init(rate) {
        PLAYING.store(false, Ordering::Release);
        crate::media::source::remote::close_remote();
        return;
    }
    log::append("dbg: 音频口已起");
    log::append(&format!("bgm: play_url {url} rate={rate} dur={dur}ms"));
}

pub fn resume(path: &str) {
    if AUDIO_READY.load(Ordering::Acquire) && PAUSED.load(Ordering::Acquire) {
        PAUSED.store(false, Ordering::Release);
        PLAYING.store(true, Ordering::Release);
        return;
    }
    if !path.is_empty() {
        play(path);
    }
}

pub fn stop() {
    let _guard = lock_session();
    /* 正在后台打开的在线流也要作废，否则它会在这之后偷偷接上来。 */
    let _ = crate::media::source::remote::new_token();
    PLAYING.store(false, Ordering::Release);
    PAUSED.store(false, Ordering::Release);
    session_end();
    POS_MS.store(0, Ordering::Release);
}

pub fn state_json() -> String {
    let path = get_path();
    /*
     * 面包屑：真机上崩在"格式化一个 60 字节字符串"的地方，而这条 URL 正好 60 字节，
     * 所以先确认状态 JSON 这条路径（它由界面每 6 帧问一次）。只记前几次，不刷屏。
     */
    if path.starts_with("http") {
        static STATE_DBG: AtomicU32 = AtomicU32::new(0);
        let n = STATE_DBG.fetch_add(1, Ordering::Relaxed);
        if n < 3 {
            log::append(&format!("dbg: state_json 进入（在线，path {} 字节）", path.len()));
        }
    }
    format!(
        "{{\"playing\":{},\"paused\":{},\"path\":\"{}\",\"pos\":{},\"dur\":{},\"rate\":{},\"dec\":\"bgm\"}}",
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
        crate::media::json_escape(&path),
        POS_MS.load(Ordering::Acquire),
        DUR_MS.load(Ordering::Acquire),
        RATE_HZ.load(Ordering::Acquire),
    )
}
