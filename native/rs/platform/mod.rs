//! 平台服务：播放器需要、但和音频无关的那些零碎能力。
//!
//! 集中在一处，好处有两个：媒体引擎模块（`bgm`/`decoder`/`source`/`provider`）
//! 不用直接摸系统调用；重构计划里"不可动清单"上的 `power`、`ps_lock`
//! 在目录里一眼就能看见。
#![allow(dead_code)]

pub mod fs;
pub mod log;
pub mod power;
pub mod ps_lock;
pub mod store;
