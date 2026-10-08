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

/* json_escape 是纯函数，直接挂真文件；再导出的名字与 native/rs/mod.rs 一致，
 * 因为 catalog / provider / tags 都按 crate::media::json_escape 这个短路径引用它。
 * （注意：内联模块里的 #[path] 会多带一层模块名目录，所以这里放在文件作用域。） */
#[path = "../../../native/rs/platform/json.rs"]
mod platform_json;
pub use platform_json::json_escape;

#[path = "../../../native/rs/provider/mod.rs"]
pub mod provider;

/* 清单解析线程与二维码编码都是纯逻辑（只用到 provider / platform::log），
 * 所以挂真文件；provider 里对它们的引用也必须在这里能解析。 */
#[path = "../../../native/rs/catalog.rs"]
pub mod catalog;

#[path = "../../../native/rs/qr.rs"]
pub mod qr;
