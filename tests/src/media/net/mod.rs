//! 传输层替身：`transport.rs` 是真文件，`http.rs` 是要联网才会用到的真机实现。

#[path = "../../../../native/rs/net/transport.rs"]
pub mod transport;

/// 与真身 `native/rs/net/http.rs` 里的 `VitaPost` 同名同形；测试不碰它。
pub mod http {
    use super::transport::{FormPost, FormRequest, FormResponse, PostError};
    use alloc::string::String;

    pub struct VitaPost;

    impl FormPost for VitaPost {
        fn post_form(&mut self, _req: &FormRequest) -> Result<FormResponse, PostError> {
            Err(PostError::Network(String::from(
                "宿主机上没有真网络传输（测试请注入假实现）",
            )))
        }
    }

    /// 与真身同名同形：宿主机上没有真实传输，永远没有下载进度。
    pub fn download_progress() -> Option<(u64, u64)> {
        None
    }
}
