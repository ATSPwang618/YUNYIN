//! NetEase session / cookie handling (task book §48/§49).
//!
//! Anti-scraping measures on this platform mean most accounts get *no* audio URL
//! without a valid session cookie, so this module exists from day one even
//! though Phase 0 does not use it yet.
//!
//! Storage rule: nothing is persisted unless the user explicitly opts in — the
//! cookie lives in memory for the session, and an optional file mirror is a
//! separate, explicit decision (§48).  Login flows (QR code, §49) are later
//! phases and are listed here so the seam does not have to be invented twice.
#![allow(dead_code)]

use alloc::string::String;

#[derive(Clone, Debug, Default)]
pub struct Session {
    /// `MUSIC_U=...; __csrf=...; ...` — empty for an anonymous session.
    cookie: String,
    logged_in: bool,
    /// Whether the account may use the higher quality tiers (§47).
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

    /// Phase 4: load a cookie the user typed in or produced with a QR login.
    pub fn load(&mut self, cookie: &str) {
        *self = Self::from_cookie(cookie);
    }

    pub fn clear(&mut self) {
        *self = Self::anonymous();
    }
}
