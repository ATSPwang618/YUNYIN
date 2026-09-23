//! Platform services: the small pieces of the Vita that the player needs but
//! that have nothing to do with audio.
//!
//! Kept in one place so the media-engine modules (`bgm`, `decoder`, `source`,
//! `provider`) never reach for a syscall directly, and so the "do not touch"
//! list of the refactor plan (`power`, `ps_lock`) is obvious in the tree.
#![allow(dead_code)]

pub mod fs;
pub mod log;
pub mod power;
pub mod ps_lock;
pub mod store;
