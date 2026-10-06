//! 缓冲策略的宿主测试：阈值必须是**秒**，而且要有迟滞（掉线 5 s、恢复 8 s）。

use super::policy::*;

#[test]
fn buffer_seconds_follow_bitrate() {
    /* 128 kbps → 512 KB ≈ 32.8 秒；320 kbps → 512 KB ≈ 13.1 秒。
     * 这就是"固定 512 KB"不合理的证据：同一个字节数，时长差 2.5 倍。 */
    assert_eq!(buffer_ms(512 * 1024, 128_000, 192_000), 32_768);
    assert_eq!(buffer_ms(512 * 1024, 320_000, 192_000), 13_107);
}

#[test]
fn unknown_bitrate_falls_back() {
    /* 未知码率时用兜底值（默认 192 kbps），不是 0、也不是 fixed KB。 */
    assert_eq!(buffer_ms(512 * 1024, 0, 192_000), 21_845);
    assert_eq!(buffer_ms(512 * 1024, 0, 0), 21_845); /* 连兜底都没给：用默认 */
}

#[test]
fn bitrate_from_size_and_duration() {
    /* 5 MB / 180 秒 ≈ 222 kbps（整首歌的平均码率，够用来算 buffer 秒数）。 */
    assert_eq!(bitrate_from_size_duration(5_000_000, 180_000), Some(222_222));
    assert_eq!(bitrate_from_size_duration(0, 180_000), None);
    assert_eq!(bitrate_from_size_duration(5_000_000, 0), None);
}

#[test]
fn net_state_thresholds() {
    /* 2 倍 = FAST；1.2 倍 = NORMAL；相等 = SLOW；低于 = STARVING。 */
    assert_eq!(classify(600_000, 256_000), NetState::Fast);
    assert_eq!(classify(400_000, 256_000), NetState::Normal);
    assert_eq!(classify(256_000, 256_000), NetState::Slow);
    assert_eq!(classify(100_000, 256_000), NetState::Starving);
}

#[test]
fn gate_needs_ten_seconds_to_start() {
    let mut g = GateState::new();
    assert_eq!(g.decide(3_000, false), GateDecision::Rebuffer);
    assert_eq!(g.decide(9_999, false), GateDecision::Rebuffer);
    assert_eq!(g.decide(10_000, false), GateDecision::Play);
}

#[test]
fn gate_rebuffers_below_min_and_resumes_at_eight() {
    let mut g = GateState::new();
    assert_eq!(g.decide(10_000, false), GateDecision::Play);
    /* 播放中 5 秒以上继续播（不因为掉到 6 秒就停）。 */
    assert_eq!(g.decide(6_000, false), GateDecision::Play);
    /* 低于 MIN(5 s) → 进入 rebuffer。 */
    assert_eq!(g.decide(4_000, false), GateDecision::Rebuffer);
    /* 迟滞：回到 5~8 秒之间仍然保持 rebuffer，避免"一有数据就抖"。 */
    assert_eq!(g.decide(6_000, false), GateDecision::Rebuffer);
    /* 到恢复阈值 8 秒才继续播。 */
    assert_eq!(g.decide(8_000, false), GateDecision::Play);
}

#[test]
fn gate_always_plays_at_stream_end() {
    /* 流末尾不会再有数据：必须放行，否则歌尾永远卡住（真机踩过）。 */
    let mut g = GateState::new();
    assert_eq!(g.decide(0, true), GateDecision::Play);
}

#[test]
fn retry_backoff_is_exponential_with_a_cap() {
    /* 200 → 500 → 1000 → 2000，之后一直 2000（无限退避会像卡死）。 */
    assert_eq!(backoff_ms(0), 200);
    assert_eq!(backoff_ms(1), 500);
    assert_eq!(backoff_ms(2), 1_000);
    assert_eq!(backoff_ms(3), 2_000);
    assert_eq!(backoff_ms(9), 2_000);
}

#[test]
fn prefetch_yields_to_the_playing_song() {
    /* 缓冲没到 2×TARGET(40 s) → 不预取（比"维持播放"更宽裕才动手）。 */
    assert!(!prefetch_should_run(25_000, NetState::Fast, false));
    assert!(!prefetch_should_run(39_999, NetState::Fast, false));
    /* 网络不是 FAST → 不预取，哪怕缓冲很足。 */
    assert!(!prefetch_should_run(60_000, NetState::Normal, false));
    assert!(!prefetch_should_run(60_000, NetState::Starving, false));
    /* 缓冲足 + 网络 FAST → 可以预取。 */
    assert!(prefetch_should_run(45_000, NetState::Fast, false));
    /* 当前曲已经全部到手 → 带宽空着，直接预取。 */
    assert!(prefetch_should_run(0, NetState::Starving, true));
}

#[test]
fn prefetch_keeps_quiet_right_after_user_input() {
    /* 用户刚按过键（<3 s）：哪怕缓冲和网络都完美，也不许抢带宽。 */
    assert!(!prefetch_should_run_q(120_000, NetState::Fast, false, 0));
    assert!(!prefetch_should_run_q(120_000, NetState::Fast, false, 2_999));
    /* 静默 3 秒之后才开跑。 */
    assert!(prefetch_should_run_q(120_000, NetState::Fast, false, 3_000));
    /* 例外：当前曲已全部到手，按键也不影响（反正不用网络）。 */
    assert!(prefetch_should_run_q(0, NetState::Slow, true, 0));
}

#[test]
fn unified_generation_invalidates_stale_workers() {
    /* 播放世代没变 → 任务还有效。 */
    assert!(!generation_stale(100, 100));
    /* 播放世代变了（切歌/停止）→ 一定作废，哪怕没人记得去 cancel。 */
    assert!(generation_stale(100, 101));
    /* 反向（理论上不会发生）也不能当成有效。 */
    assert!(generation_stale(101, 100));
}

#[test]
fn readahead_is_one_third_of_the_song_within_limits() {
    /* 整首 7.8 MB → 1/3 ≈ 2.6 MB（在区间内，原样给）。 */
    assert_eq!(readahead_budget(Some(7_800_000)), 2_600_000);
    /* 很短的一首（3 MB）→ 下限 2 MiB 兜住（抓满一首就停，没有浪费）。 */
    assert_eq!(readahead_budget(Some(3 * 1024 * 1024)), READAHEAD_MIN_BYTES);
    /* 一首 50 MB 的无损 → 1/3 有 16 MB，但不能整块塞 RAM：封顶 6 MiB。 */
    assert_eq!(readahead_budget(Some(50 * 1024 * 1024)), READAHEAD_MAX_BYTES);
    /* 长度未知 / 0 → 中间值，偏保守。 */
    assert_eq!(readahead_budget(None), READAHEAD_FALLBACK_BYTES);
    assert_eq!(readahead_budget(Some(0)), READAHEAD_FALLBACK_BYTES);
}
