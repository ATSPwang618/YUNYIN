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
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;

use crate::media::platform::log;

/// SCE_SHELL_UTIL_LOCK_TYPE_PS_BTN
const LOCK_PS_BTN: i32 = 0x1;

static LOCKED: AtomicBool = AtomicBool::new(false);
/// 最近一次系统调用的返回值（0 = 成功），供 JS 显示/记录。
static LAST_RET: AtomicI32 = AtomicI32::new(0);

extern "C" {
    /// SceShellUtil 的事件系统：**调 Lock/Unlock 之前必须先初始化**，
    /// 否则这些接口会直接报错（0.5 时代踩过）。
    fn sceShellUtilInitEvents(unk: i32) -> i32;
    fn sceShellUtilLock(kind: i32) -> i32;
    fn sceShellUtilUnlock(kind: i32) -> i32;
}

/// 启动时调一次（在 register 里）。
pub fn init() {
    let ret = unsafe { sceShellUtilInitEvents(0) };
    log::append(&format!("shell util events init -> 0x{:08X}", ret as u32));
}

/* 系统调用串行化：两个状态变化挨得很近时（播放→暂停）保证不会乱序。 */
static SHELL_CALL: Mutex<()> = Mutex::new(());

/// 播放状态变化时调用：true = 锁 PS 键，false = 解锁。同一个状态重复调用是空操作。
/// 返回最近一次系统调用的结果（0 = 成功）。
///
/// **系统调用放后台线程**：真机日志里 `sceShellUtilLock/Unlock` 偶尔让所在帧卡
/// 0.5–1.2s（`HOST: guest 帧耗时 1203ms` 紧跟着 `ps lock: request=true`）。
/// 界面只关心"锁没锁上"这个状态，不需要等它返回。
pub fn set_locked(on: bool) -> i32 {
    if LOCKED.swap(on, Ordering::AcqRel) == on {
        return LAST_RET.load(Ordering::Acquire);
    }
    let spawned = std::thread::Builder::new()
        .name("yunyin-shellutil".into())
        .stack_size(16 * 1024)
        .spawn(move || {
            let _guard = SHELL_CALL.lock();
            /* 等锁的期间可能又变了一次（比如刚按播放就暂停）：那时这次调用已经过期，
             * 交给后一个线程去做，最终状态以最新请求为准。 */
            if LOCKED.load(Ordering::Acquire) != on {
                return;
            }
            let ret = unsafe {
                if on {
                    sceShellUtilLock(LOCK_PS_BTN)
                } else {
                    sceShellUtilUnlock(LOCK_PS_BTN)
                }
            };
            LAST_RET.store(ret, Ordering::Release);
            log::append(&format!("ps btn lock: {} -> 0x{:08X}", on as i32, ret as u32));
        });
    if spawned.is_err() {
        log::append("ps btn lock: 后台线程创建失败（PS 锁状态可能不准）");
    }
    LAST_RET.load(Ordering::Acquire)
}
