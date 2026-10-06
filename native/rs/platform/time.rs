//! 平台时钟与一点点熵（Phase 3）。
//!
//! 为什么不用 `std::time`：Vita 上的 std 时间实现没在真机上验证过；而
//! `sceKernelGetSystemTimeWide`（微秒）在 C 侧已经用了很久，是确定可用的。
//! URL 过期判断用单调时钟反而更对 —— 用户改系统时间不该让缓存提前失效。
#![allow(dead_code)]

extern "C" {
    fn sceKernelGetSystemTimeWide() -> i64;
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
