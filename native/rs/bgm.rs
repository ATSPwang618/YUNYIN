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

fn vita_audio_end() {
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
        .stack_size(64 * 1024)
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
    if path.is_empty() {
        return;
    }
    vita_audio_end();
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
 */
pub fn play_url(url: &str, referer: &str, duration_ms: i64) {
    if url.is_empty() {
        return;
    }
    vita_audio_end();
    if crate::media::source::remote::open_remote(url, referer, duration_ms).is_err() {
        log::append(&format!("bgm: 在线打开失败 {url}"));
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
    set_path(url);
    if !vita_audio_init(rate) {
        PLAYING.store(false, Ordering::Release);
        crate::media::source::remote::close_remote();
        return;
    }
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
    PLAYING.store(false, Ordering::Release);
    PAUSED.store(false, Ordering::Release);
    vita_audio_end();
    POS_MS.store(0, Ordering::Release);
}

pub fn state_json() -> String {
    let path = get_path();
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
