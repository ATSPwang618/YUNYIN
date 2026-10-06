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
/// 最近一次在线打开失败的原因。界面读它，把"没声音还卡着"变成一行明确的提示。
static PLAY_ERR: Mutex<String> = Mutex::new(String::new());

fn set_play_error(msg: &str) {
    if let Ok(mut g) = PLAY_ERR.lock() {
        *g = String::from(msg);
    }
}

fn clear_play_error() {
    if let Ok(mut g) = PLAY_ERR.lock() {
        g.clear();
    }
}

pub fn play_error() -> String {
    PLAY_ERR.lock().map(|g| g.clone()).unwrap_or_default()
}

/*
 * 给界面的**短**原因：播放器那一行只有 196 宽、12px 字，塞不下
 * `Network("yhttp_post 失败 0x80431068")` 这种东西（真机上就是把状态行撑爆）。
 * 界面上只给固定几个词，详细原因全部留在日志里。
 */
fn short_reason(e: &crate::media::source::SourceError) -> &'static str {
    use crate::media::source::SourceError as E;
    match e {
        E::Http { status } if *status == 403 || *status == 404 || *status == 410 => "需要会员",
        E::Http { .. } => "无版权",
        E::Network(_) => "网络故障",
        E::Cancelled => "已取消",
        E::Expired => "链接失效",
        E::Eof => "流已结束",
        E::NotSeekable => "流不可拖动",
        E::Io(_) => "读写失败",
        _ => "打开失败",
    }
}

fn short_provider_reason(e: &crate::media::provider::ProviderError) -> &'static str {
    use crate::media::provider::ProviderError as P;
    match e {
        P::Auth(_) => "需要登录",
        P::VipRequired => "需要会员",
        P::NotFound => "无版权",
        P::Network(_) => "网络故障",
        P::Unsupported => "格式不支持",
        P::Cancelled => "已取消",
        P::Expired => "链接失效",
    }
}
static AUDIO_TERMINATE: AtomicBool = AtomicBool::new(true);
static AUDIO_READY: AtomicBool = AtomicBool::new(false);
static BGM_HELD: AtomicBool = AtomicBool::new(false);
static PORT: AtomicI32 = AtomicI32::new(-1);
static PORT_RATE: AtomicI32 = AtomicI32::new(0);
static POS_MS: AtomicU32 = AtomicU32::new(0);
static DUR_MS: AtomicU32 = AtomicU32::new(0);
static RATE_HZ: AtomicU32 = AtomicU32::new(44100);
static PATH_LEN: AtomicUsize = AtomicUsize::new(0);
/* Gate 的静音状态：只为在日志里留一行"什么时候开始静音 / 什么时候恢复"。 */
static GATE_SILENT: AtomicBool = AtomicBool::new(false);
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
     *
     * 但静音这条路**必须顺手叫醒取数线程**（remote::prime）：否则就成了
     * "没人拉数据 → 永远没数据 → 一直静音"的死锁 —— 真机上"在线歌播到十几秒
     * 卡死"就是这个（第一窗播完、缓存低于阈值，取数线程再没被叫过）。
     */
    if !crate::media::source::remote::gate_ok() {
        crate::media::source::remote::prime();
        if !GATE_SILENT.swap(true, Ordering::AcqRel) {
            /* 只记状态跳变那一次，不刷屏（音频线程上写日志现在是安全的：日志有锁）。 */
            log::append(&format!(
                "remote: 缓冲不足，先静音（剩 {}.{}s / 启动需 10s，网络 {:?}）",
                crate::media::source::remote::buffer_ms() / 1000,
                (crate::media::source::remote::buffer_ms() % 1000) / 100,
                crate::media::source::remote::net_state()
            ));
        }
        buf.fill(0);
        return;
    }
    if GATE_SILENT.swap(false, Ordering::AcqRel) {
        log::append(&format!(
            "remote: 缓冲恢复（{}.{}s，网络 {:?}）",
            crate::media::source::remote::buffer_ms() / 1000,
            (crate::media::source::remote::buffer_ms() % 1000) / 100,
            crate::media::source::remote::net_state()
        ));
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
    /* 先叫醒可能正卡在网络等待里的音频线程：不叫的话 join 要等它超时
     * （最多 3×4 秒），界面线程会被一起拖住（真机 0.13："点歌卡死"的来源之一）。 */
    crate::media::source::remote::cancel();
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
    /* 一按播放就把上一次的失败提示清掉：界面不会拿着旧错误不放。 */
    clear_play_error();
    /*
     * 在线曲目在界面里就是一个普通条目：它的 audioPath 直接是 URL。
     * 这里按前缀分流，界面完全不用知道"本地 / 在线"的区别。
     */
    /* Phase 3：`netease:<id>` 是"待解析"的在线条目 —— 先解析出 CDN 地址再播。 */
    if let Some(song_id) = path.strip_prefix("netease:") {
        play_song_id(song_id);
        return;
    }
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

/*
 * Phase 3：在线曲库条目（`netease:<id>`）。
 *
 * 解析地址要发一次加密 POST，绝不能占界面线程；这里和在线打开一样用后台线程，
 * 并复用同一个 OPEN_TOKEN —— 用户在解析途中换了歌，这次解析的结果直接作废。
 */
fn play_song_id(song_id: &str) {
    if song_id.is_empty() {
        return;
    }
    let token = crate::media::source::remote::new_token();
    PLAYING.store(false, Ordering::Release);
    PAUSED.store(false, Ordering::Release);
    let id = String::from(song_id);
    let spawned = std::thread::Builder::new()
        .name("yunyin-netease-resolve".into())
        .stack_size(96 * 1024)
        .spawn(move || {
            log::append(&format!("bgm: 解析在线歌曲 id={id} generation={token}"));
            /*
             * 解析要发一次加密 POST。真机实测（手机热点 / 校园网）这条链路经常
             * "这一次超时、下一次就通"（日志里 0x80431068 = SCE_HTTP_ERROR_TIMEOUT，
             * 更早还出现过一片 DNS 失败后恢复）。只试一次的话，用户看到的就是
             * 歌停在 00:00 不动，直到他自己再按一次播放。所以这里最多试 3 次、
             * 指数退避；每次重试前先核对序号，用户中途换歌就立刻收手。
             */
            let mut last_reason = "打开失败";
            for attempt in 1..=3u32 {
                if !crate::media::source::remote::token_current(token) {
                    return; /* 解析途中用户换了歌 */
                }
                let started = std::time::Instant::now();
                /*
                 * **标准码率优先**：先保证能出声，音质往后放。
                 *
                 * 为什么必须这样：网易云给"这个账号没权限的码率"返回的是带
                 * `authSecret` 的加密地址，CDN 一律回 403（真机日志里 41 次失败
                 * 全是 `jdyyaac … authSecret=…`）。先要高码率就是先撞 403，
                 * 用户要等两轮网络超时才听到声音 —— 顺序反过来才是对的。
                 *
                 *   1) 标准码率（普通地址，绝大多数歌直接能放）
                 *   2) 标准码率再来一条新地址（地址过期/被拒的兜底）
                 *   3) 还不行才试高码率：有些歌只有高码率那条链能通
                 */
                /*
                 * 服务端早就说过"这首歌能播到什么档"（privileges.plLevel）：
                 * 第 1 次就按它请求，别自己猜。没提示就退回标准码率
                 * （"先能出声"那条规矩），第 3 次才无脑试高码率。
                 */
                let quality = if attempt == 1 {
                    match crate::media::provider::netease::level_hint(&id) {
                        Some(level) => {
                            crate::media::provider::netease::quality_from_level(&level)
                        }
                        None => crate::media::provider::Quality::Low,
                    }
                } else if attempt == 2 {
                    crate::media::provider::Quality::Low
                } else {
                    crate::media::provider::Quality::Auto
                };
                let resolved = if attempt == 1 {
                    crate::media::provider::netease::resolve_song(&id, quality)
                } else {
                    /* 第 2 次起：先丢掉上一条地址再解析 —— 上一次多半就是被 CDN
                     * 拒了（403），拿同一条地址再试一次没有任何意义。 */
                    crate::media::provider::netease::invalidate_song_url(&id);
                    crate::media::provider::netease::resolve_song(
                        &id,
                        crate::media::provider::Quality::Auto,
                    )
                };
                match resolved {
                    Ok(info) => {
                        log::append(&format!(
                            "bgm: 在线歌曲已解析（第 {attempt}/3 次，{}ms，{}{}）{}",
                            started.elapsed().as_millis(),
                            if attempt <= 2 { "标准码率" } else { "自动" },
                            if info.url.contains("authSecret") {
                                "，带 authSecret"
                            } else {
                                ""
                            },
                            info.describe()
                        ));
                        if !crate::media::source::remote::token_current(token) {
                            return;
                        }
                        /* 解析 → 打开在同一个线程里连着做完：打开失败要能立刻
                         * 换一条地址重试，而不是把用户丢在"没声音"的状态里。 */
                        match open_online(
                            &info.url,
                            crate::media::provider::netease::REFERER,
                            info.duration_ms as i64,
                            token,
                        ) {
                            Ok(()) => return,
                            Err(crate::media::source::SourceError::Cancelled) => return,
                            Err(e) => {
                                log::append(&format!(
                                    "bgm: 在线打开失败（第 {attempt}/3 次）{:?} {}",
                                    e, info.url
                                ));
                                last_reason = short_reason(&e);
                                /*
                                 * 地址带 authSecret = 网易云给的是**加密地址**；
                                 * CDN 对没权限的账号一律回 403。这种情况再重试
                                 * 也只是白等两轮（真机日志里 6 秒才放弃），
                                 * 直接判"需要会员/暂无版权"，让界面标出来。
                                 */
                                let http = match e {
                                    crate::media::source::SourceError::Http { status } => {
                                        Some(status)
                                    }
                                    _ => None,
                                };
                                if info.url.contains("authSecret")
                                    && matches!(http, Some(403) | Some(404) | Some(410))
                                {
                                    log::append(&format!(
                                        "bgm: 加密地址被 CDN 拒（需要会员 / 暂无版权）id={id}"
                                    ));
                                    set_play_error("需要会员");
                                    return;
                                }
                                /* 链接被 CDN 拒 → 丢掉缓存地址，下一轮重新解析 */
                                crate::media::provider::netease::invalidate_song_url(&id);
                            }
                        }
                    }
                    Err(e) => {
                        log::append(&format!(
                            "bgm: 在线歌曲解析失败（第 {attempt}/3 次，{}ms）{:?} id={id}",
                            started.elapsed().as_millis(),
                            e
                        ));
                        last_reason = short_provider_reason(&e);
                        /* 业务拒绝不会因为重发同一个请求而改变：-110（登录/会员
                         * 权限）、404（无版权）和明确的认证错误直接结束，别让用户
                         * 再等两轮并误以为是网络超时。 */
                        if matches!(
                            e,
                            crate::media::provider::ProviderError::Auth(_)
                                | crate::media::provider::ProviderError::VipRequired
                                | crate::media::provider::ProviderError::NotFound
                                | crate::media::provider::ProviderError::Unsupported
                        ) {
                            log::append(&format!(
                                "bgm: 在线歌曲最终失败（无需重试）{last_reason} id={id}"
                            ));
                            set_play_error(last_reason);
                            return;
                        }
                    }
                }
                if attempt < 3 {
                    /* 退避一下再试：给链路（DNS/连接池/CDN）一点恢复时间。 */
                    std::thread::sleep(std::time::Duration::from_millis(
                        400 * attempt as u64,
                    ));
                }
            }
            /* 三次都不行：把原因交给界面（否则用户只看到"没声音、还卡着"）。 */
            log::append(&format!(
                "bgm: 在线歌曲最终失败（重按 ○ 可再试）{last_reason} id={id}"
            ));
            set_play_error(last_reason);
        });
    if spawned.is_err() {
        log::append("bgm: 在线歌曲解析线程创建失败");
    }
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
    let token = crate::media::source::remote::new_token();
    play_url_with_token(url, referer, duration_ms, token);
}

/// 用调用方已经拿到的序号开流（Phase 3 的 `netease:` 解析线程用）。
///
/// 为什么要有这个变体：解析线程先校验"我还是当前播放请求"，如果此刻再另起一个
/// 序号，就会出现"用户已经点了别的歌、旧解析却把新序号抢走"的窗口。沿用同一个
/// 序号，`open_online` 里的每一步检查都会自然作废过期请求。
fn play_url_with_token(url: &str, referer: &str, duration_ms: i64, token: u32) {
    if url.is_empty() {
        return;
    }
    /* 立刻标成"还没在放"：界面上的进度条不会拿着上一首的数字发呆。 */
    PLAYING.store(false, Ordering::Release);
    PAUSED.store(false, Ordering::Release);
    clear_play_error();

    let url_owned = String::from(url);
    let referer_owned = String::from(referer);
    let spawned = std::thread::Builder::new()
        .name("yunyin-net-open".into())
        .stack_size(96 * 1024) /* 打开阶段会走 yp_open_io → 解码器 → 取数层，同样要留余量 */
        .spawn(move || {
            if let Err(e) = open_online(&url_owned, &referer_owned, duration_ms, token) {
                log::append(&format!("bgm: 在线打开失败 {:?} {}", e, url_owned));
                set_play_error(short_reason(&e));
            }
        });
    if spawned.is_err() {
        log::append("bgm: 在线打开线程创建失败");
    }
}

/// 后台线程里的那一步：先收掉上一首，再打开在线源，最后起 BGM 口。
///
/// 返回 `Err` = "这次没开成"：调用方（`netease:<id>` 的解析线程）据此换一条地址
/// 重试。真机上"切歌之后没声音、界面还卡着"就是拿到了一条被 CDN 拒掉（403）的地址。
fn open_online(
    url: &str,
    referer: &str,
    duration_ms: i64,
    token: u32,
) -> Result<(), crate::media::source::SourceError> {
    use crate::media::source::SourceError;
    let _guard = lock_session();
    if !crate::media::source::remote::token_current(token) {
        return Err(SourceError::Cancelled); /* 还没轮到我，就已经被新的播放请求取代了 */
    }
    session_end();
    if !crate::media::source::remote::token_current(token) {
        return Err(SourceError::Cancelled);
    }
    /* 开流要带登录会话 Cookie：登录用户拿到的地址带 authSecret，
     * CDN 会校这个（不带就是 403）。 */
    let cookie = crate::media::provider::netease::current_cookie();
    crate::media::source::remote::open_remote(url, referer, &cookie, duration_ms, token)?;
    /*
     * 打开可能花好几秒；这期间用户按了停止/换了歌（token 变了）就**别起播**：
     * 否则旧的那首会在用户已经走开之后突然出声。stop() 现在不再等这把锁，
     * 所以这里必须自己再确认一次。
     */
    if !crate::media::source::remote::token_current(token) {
        log::append("remote: 打开完成后已被停止/换歌取代，丢弃这次源");
        crate::media::source::remote::close_remote();
        return Err(SourceError::Cancelled);
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
        return Err(SourceError::Io(String::from("音频口打不开")));
    }
    log::append("dbg: 音频口已起");
    log::append(&format!("bgm: play_url {url} rate={rate} dur={dur}ms"));
    Ok(())
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
    /*
     * 顺序很要紧：**先作废正在进行的打开，再限时拿会话锁**。
     *
     * 为什么：在线打开（DNS+TLS+第一个窗口）最长能占住会话锁好几秒 ——
     * 真机日志里出现过 `HOST: guest 帧耗时 8857ms`，就是界面线程在 stop() 里
     * 等这把锁（0.13 的 dev 宿主直接判"guest JavaScript time budget exceeded"
     * 把应用打死了）。token 是原子量，先改它；拿不到锁就先不动解码器，
     * 让那次打开自己收手 —— 界面绝不为网络等待。
     */
    let _ = crate::media::source::remote::new_token();
    PLAYING.store(false, Ordering::Release);
    PAUSED.store(false, Ordering::Release);
    clear_play_error();
    POS_MS.store(0, Ordering::Release);
    /* 最多等 600ms；超时就跳过 session_end（打开线程自己会收尾）。 */
    let mut waited_ms = 0u32;
    loop {
        match SESSION.try_lock() {
            Ok(guard) => {
                session_end();
                drop(guard);
                return;
            }
            Err(std::sync::TryLockError::Poisoned(p)) => {
                let _guard = p.into_inner();
                session_end();
                return;
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                if waited_ms >= 600 {
                    log::append("bgm: stop 跳过 session_end（会话锁被网络打开占着）");
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
                waited_ms += 20;
            }
        }
    }
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
        "{{\"playing\":{},\"paused\":{},\"path\":\"{}\",\"pos\":{},\"dur\":{},\"rate\":{},\"err\":\"{}\",\"dec\":\"bgm\"}}",
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
        crate::media::json_escape(&play_error()),
    )
}
