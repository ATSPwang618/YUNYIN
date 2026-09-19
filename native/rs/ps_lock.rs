//! 播放期间锁 PS 键。
//!
//! 按 PS 会回到 LiveArea、把应用切到后台；想在后台继续出声要一整套宿主改造
//! （上游 PocketJS 的动态字形/后台那套只做在 PSP/WASM 上），所以换个思路：
//! **播放中把 PS 键锁住，想离开应用必须先暂停** —— 暂停/停止/放完立刻解锁，
//! 进程退出时锁随进程一起消失。
//!
//! 对应 ElevenMPVScrobbling `source/utils.c` 的 `Utils_LockPower()`：
//! `sceShellUtilLock(SCE_SHELL_UTIL_LOCK_TYPE_PS_BTN)`。

use alloc::format;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::media::log;

/// SCE_SHELL_UTIL_LOCK_TYPE_PS_BTN
const LOCK_PS_BTN: i32 = 0x1;

static LOCKED: AtomicBool = AtomicBool::new(false);

extern "C" {
    fn sceShellUtilLock(kind: i32) -> i32;
    fn sceShellUtilUnlock(kind: i32) -> i32;
}

/// 播放状态变化时调用：true = 锁 PS 键，false = 解锁。同一个状态重复调用是空操作。
pub fn set_locked(on: bool) {
    if LOCKED.swap(on, Ordering::AcqRel) == on {
        return;
    }
    let ret = unsafe {
        if on {
            sceShellUtilLock(LOCK_PS_BTN)
        } else {
            sceShellUtilUnlock(LOCK_PS_BTN)
        }
    };
    log::append(&format!("ps btn lock: {} -> 0x{:08X}", on as i32, ret as u32));
}
