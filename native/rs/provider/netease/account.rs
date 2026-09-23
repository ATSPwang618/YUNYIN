//! 网易云会话 / Cookie 处理（任务书 §48/§49）。
//!
//! 这个平台的反爬很严：没有有效的会话 Cookie，多数账号根本拿不到音频 URL。
//! 所以这个模块从现在就有（虽然 Phase 0 还用不到）。
//!
//! 存储规则：用户没明确开启就不落盘 —— Cookie 只活在本次会话的内存里，
//! "额外存一份文件"是另一个独立的、明确的决定（§48）。扫码登录（§49）属于后面的
//! 阶段，先在这里列出来，免得以后重新发明一遍接缝。
#![allow(dead_code)]

use alloc::string::String;

#[derive(Clone, Debug, Default)]
pub struct Session {
    /// `MUSIC_U=...; __csrf=...; ...` —— 匿名会话时为空。
    cookie: String,
    logged_in: bool,
    /// 账号是否有权使用更高音质档（§47）。
    vip: bool,
}

impl Session {
    pub fn anonymous() -> Self {
        Self::default()
    }

    pub fn from_cookie(cookie: &str) -> Self {
        Self {
            cookie: String::from(cookie),
            logged_in: !cookie.is_empty(),
            vip: false,
        }
    }

    pub fn cookie_header(&self) -> Option<String> {
        if self.cookie.is_empty() {
            None
        } else {
            Some(self.cookie.clone())
        }
    }

    pub fn is_logged_in(&self) -> bool {
        self.logged_in
    }

    pub fn is_vip(&self) -> bool {
        self.vip
    }

    /// Phase 4：载入用户手填的、或扫码登录得到的 Cookie。
    pub fn load(&mut self, cookie: &str) {
        *self = Self::from_cookie(cookie);
    }

    pub fn clear(&mut self) {
        *self = Self::anonymous();
    }
}
