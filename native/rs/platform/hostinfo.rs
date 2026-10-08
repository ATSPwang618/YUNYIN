//! 状态栏要的主机信息：电量、时间、联网状态。
//!
//! 一次调用全部返回（`{"battery":95,"charging":0,"online":1,"time":"18:47"}`）：
//! 界面每 15 秒问一次就够，别放进帧循环 —— 那样一秒会多出几十次系统调用。

extern "C" {
    fn scePowerGetBatteryLifePercent() -> i32;
    fn scePowerIsBatteryCharging() -> i32;
    fn sceNetCtlInetGetState(state: *mut i32) -> i32;
}

/// SCE_NETCTL_STATE_CONNECTED
const NETCTL_CONNECTED: i32 = 3;

pub fn json() -> alloc::string::String {
    use alloc::format;
    let pct = unsafe { scePowerGetBatteryLifePercent() };
    let charging = unsafe { scePowerIsBatteryCharging() };
    let mut state: i32 = 0;
    let rc = unsafe { sceNetCtlInetGetState(&mut state) };
    let online = rc >= 0 && state == NETCTL_CONNECTED;
    let time = match crate::media::platform::time::wall_clock() {
        Some((_, _, _, h, mi, _)) => format!("{h:02}:{mi:02}"),
        None => alloc::string::String::from("--:--"),
    };
    format!(
        "{{\"battery\":{},\"charging\":{},\"online\":{},\"time\":\"{}\"}}",
        pct,
        if charging > 0 { 1 } else { 0 },
        if online { 1 } else { 0 },
        time
    )
}