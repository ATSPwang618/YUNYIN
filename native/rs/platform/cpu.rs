//! CPU 亲和性：把"系统保留核"（第 4 个核 / CPU3）纳入本线程的调度范围。
//!
//! VitaSDK 的线程 API 写得很清楚：核分成
//!   `SCE_KERNEL_CPU_MASK_USER_0/1/2`（应用能用）+ `SCE_KERNEL_CPU_MASK_SYSTEM`
//!（系统保留核，就是 CPU3）。游戏模式进程的默认掩码**不含** SYSTEM 位，所以应用
//! 天然只用 3 个核 —— 真机 CPU 监视器上"3 个核 80%、第 4 个看戏"就是这么来的。
//!
//! 要让第 4 个核真正可用，必须先在**内核层**解锁（社区做法是 CapUnlocker /
//! CoreUnlocker 这类 `.skprx`：放进 `ur0:tai/`，在 `config.txt` 的 `*KERNEL` 段
//! 加载，开机生效）。用户态程序没有权限加载内核模块，**应用自己做不到这件事**。
//! 插件装好之后，还需要应用侧主动把这一位加进掩码，线程才会被调度过去 ——
//! 这里做的就是这一半。没有插件时内核会返回错误或截断掩码，日志里能看出区别。

use alloc::format;

extern "C" {
    fn sceKernelGetThreadId() -> i32;
    fn sceKernelGetThreadCpuAffinityMask(thid: i32) -> i32;
    fn sceKernelChangeThreadCpuAffinityMask(thid: i32, mask: i32) -> i32;
}

/*
 * 掩码位来自 `psp2/kernel/cpu.h`（注意不是 0x1/0x2/0x4/0x8 —— 那几位是"空核位"）：
 *
 *   SCE_KERNEL_CPU_MASK_USER_0   0x00010000
 *   SCE_KERNEL_CPU_MASK_USER_1   0x00020000
 *   SCE_KERNEL_CPU_MASK_USER_2   0x00040000
 *   SCE_KERNEL_CPU_MASK_SYSTEM   0x00080000   ← 第 4 个核，CapUnlocker 解锁的就是它
 *   SCE_KERNEL_CPU_MASK_USER_ALL  0x00070000  ← 应用默认拿到的三个核
 *
 * 上一版这里误写成 0x8（一个不存在的核位），内核会直接拒绝或忽略 —— 白改。
 */
const CPU_MASK_SYSTEM: i32 = 0x0008_0000;
/// 应用默认拿到的三个用户核（`SCE_KERNEL_CPU_MASK_USER_ALL`）。
const CPU_MASK_USER_ALL: i32 = 0x0007_0000;

/// 把本线程的亲和性扩到包含系统保留核。
///
/// 只记一行日志：`ret=0` 说明内核接纳了（装了插件），负数说明被拒（没插件）。
pub fn widen(tag: &str) {
    unsafe {
        let thid = sceKernelGetThreadId();
        let before = sceKernelGetThreadCpuAffinityMask(thid);
        if before < 0 {
            crate::media::platform::log::append(&format!("cpu: {tag} 取掩码失败 {before}"));
            return;
        }
        if before & CPU_MASK_SYSTEM != 0 {
            crate::media::platform::log::append(&format!("cpu: {tag} 掩码已含系统核 0x{before:X}"));
            return;
        }
        /*
         * 基准掩码：读不到（<= 0，pthread 线程会这样）时按 USER_ALL 算。
         *
         * 真机日志里音频线程读到的是 0，当时直接 |0x80000 把线程设成了**只有系统核** ——
         * 等于把音频从 3 个用户核上挪走，随时可能被系统核上的其它活挤掉。这里兜底。
         */
        let base = if before > 0 { before } else { CPU_MASK_USER_ALL };
        let want = base | CPU_MASK_SYSTEM;
        let ret = sceKernelChangeThreadCpuAffinityMask(thid, want);
        let after = sceKernelGetThreadCpuAffinityMask(thid);
        crate::media::platform::log::append(&format!(
            "cpu: {tag} 亲和性 0x{before:X} -> 0x{want:X} ret={ret} 实得 0x{after:X}{}",
            if after & CPU_MASK_SYSTEM != 0 {
                "（系统核已启用）"
            } else {
                "（系统核被内核剔除 = 没装解锁插件）"
            }
        ));
    }
}