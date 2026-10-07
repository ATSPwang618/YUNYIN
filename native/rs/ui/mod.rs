//! 界面支撑：Vita2D 原生文字管线，以及"这一帧要不要跳过"的判断。
//! 它们让 PocketJS 的渲染循环在 Vita 上看起来正常；与音频或网络引擎无关，
//! 所以放在这里而不是紧挨着 `bgm.rs`。
#![allow(dead_code)]

pub mod frame_skip;
pub mod native_text;
