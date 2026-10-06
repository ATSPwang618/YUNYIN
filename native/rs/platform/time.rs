//! 平台时钟与一点点熵（Phase 3）。
//!
//! 为什么不用 `std::time`：Vita 上的 std 时间实现没在真机上验证过；而
//! `sceKernelGetSystemTimeWide`（微秒）在 C 侧已经用了很久，是确定可用的。
//! URL 过期判断用单调时钟反而更对 —— 用户改系统时间不该让缓存提前失效。
#![allow(dead_code)]

extern "C" {
    fn sceKernelGetSystemTimeWide() -> i64;
}

/*
 * 卡里的**系统时间**（本地时区）。
 *
 * 为什么要专门读它：证书校验里"有效期（NOT_BEFORE / NOT_AFTER）"是开着的
 * （见 native/net/yhttp.c 的 yhttp: sceHttpsEnableOption(0x3D)），主机时钟一旦
 * 不对，**所有** HTTPS 请求都会在握手阶段被拒（0x80431075）——表现就是
 * "二维码死活刷新不了 / 歌单一个都同步不了，但网络明明是好的"。
 * 所以每次启动把日期打进日志：真有这个毛病，一眼就能看出来。
 */
#[repr(C)]
struct SceDateTime {
    year: u16,
    month: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    microsecond: u32,
}

extern "C" {
    fn sceRtcGetCurrentClockLocalTime(t: *mut SceDateTime) -> i32;
}

/// 系统时间（本地时区）：`(年, 月, 日, 时, 分, 秒)`；读不到返回 None。
pub fn wall_clock() -> Option<(u16, u16, u16, u16, u16, u16)> {
    let mut t = SceDateTime {
        year: 0,
        month: 0,
        day: 0,
        hour: 0,
        minute: 0,
        second: 0,
        microsecond: 0,
    };
    let rc = unsafe { sceRtcGetCurrentClockLocalTime(&mut t) };
    if rc < 0 || t.year == 0 {
        return None;
    }
    Some((t.year, t.month, t.day, t.hour, t.minute, t.second))
}

/// 系统时间的日志文本；年份明显不对时自带警告（那会让所有 HTTPS 失败）。
pub fn wall_clock_text() -> alloc::string::String {
    use alloc::format;
    match wall_clock() {
        Some((y, mo, d, h, mi, s)) => {
            let line = format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}");
            if y < 2020 || y > 2100 {
                format!("{line} ← 时钟不对！证书有效期检查会拒掉所有 HTTPS（0x80431075）")
            } else {
                line
            }
        }
        None => alloc::string::String::from("读不到（sceRtcGetCurrentClockLocalTime 失败）"),
    }
}

/// 单调微秒（开机起算）。
pub fn now_us() -> u64 {
    let t = unsafe { sceKernelGetSystemTimeWide() };
    if t > 0 {
        t as u64
    } else {
        0
    }
}

pub fn now_ms() -> u64 {
    now_us() / 1000
}

/// 给"每次请求的 16 字符密钥"凑熵：时间 + 计数器 + 一个地址。
///
/// Phase 3 的请求里没有秘密（只是要一个播放地址），这个强度够用；
/// Phase 4 要带登录 Cookie 时再换成更强的源。
pub fn entropy64() -> u64 {
    use core::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = now_us();
    let a = (&COUNTER as *const AtomicU64) as u64;
    let x = t ^ a.rotate_left(17) ^ c.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z ^= z >> 30;
    z = z.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z ^= z >> 27;
    z = z.wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
