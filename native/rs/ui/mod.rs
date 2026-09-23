//! UI support: text/font plumbing and the frame-skip decision.  These modules
//! make the PocketJS render loop look right on the Vita; none of them belong to
//! the audio or network engine, which is why they live here rather than next to
//! `bgm.rs`.
#![allow(dead_code)]

pub mod cjk_host;
pub mod font_gpu;
pub mod frame_skip;
pub mod offload_local;
