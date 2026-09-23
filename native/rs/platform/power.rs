//! Power tick only.
//!
//! Maps ElevenMPVScrobbling `source/utils.c` `power_tick_thread`:
//! `sceKernelPowerTick(SCE_KERNEL_POWER_TICK_DISABLE_AUTO_SUSPEND)` every 10s
//! while a playback session is active.
//!
//! Intentionally NOT copied from ElevenMPV:
//! - `sceShellUtilLock(SCE_SHELL_UTIL_LOCK_TYPE_PS_BTN)` (would block PS→LiveArea)
//! - OLED-off tick
//! - power callback that re-sends PLAY

extern "C" {
    fn sceKernelPowerTick(tick_type: i32) -> i32;
}

/// SCE_KERNEL_POWER_TICK_DISABLE_AUTO_SUSPEND
const POWER_TICK_DISABLE_AUTO_SUSPEND: i32 = 1;

pub fn start() {
    let _ = std::thread::Builder::new()
        .name("yunyin-powertick".into())
        .stack_size(16 * 1024)
        .spawn(|| loop {
            if crate::media::bgm::session_active() {
                unsafe { sceKernelPowerTick(POWER_TICK_DISABLE_AUTO_SUSPEND) };
            }
            unsafe { vitasdk_sys::sceKernelDelayThread(10_000_000) };
        });
}
