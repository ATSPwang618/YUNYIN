//! `crate::media::*` 的宿主机替身树：真文件用 `#[path]` 挂载，缺口用工装顶住。

pub mod net;

/* 磁盘缓存（环形覆盖 + 索引）是纯逻辑，能在电脑上测 —— 真机上"缓存把音频
 * 读坏"是最难查的一类问题，先在这里把环的边界钉死。 */
pub mod source {
    #[path = "../../../../native/rs/source/diskcache.rs"]
    pub mod diskcache;

    #[path = "../../../../native/rs/source/policy.rs"]
    pub mod policy;

    #[cfg(test)]
    mod tests;

    #[cfg(test)]
    mod policy_tests;
}

pub mod platform {
    pub mod time {
        pub fn now_ms() -> u64 {
            0
        }

        pub fn entropy64() -> u64 {
            0
        }
    }

    pub mod log {
        pub fn append(_s: &str) {}
    }

    pub mod store {
        use alloc::string::String;

        pub fn get(_key: &str) -> String {
            String::new()
        }

        pub fn set(_key: &str, _value: &str) {}
    }
}

#[path = "../../../native/rs/provider/mod.rs"]
pub mod provider;
