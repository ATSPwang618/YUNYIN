//! 手写 JSON 时唯一的字符串转义实现。
//!
//! 放在 platform/ 而不是 crate 根，是为了让宿主机测试壳（tests/src/media）
//! 能用 #[path] 直接挂载真文件 —— crate 根上的函数没法单独挂进来。

use alloc::{format, string::String};

/// 把 `s` 转义成可放进双引号里的 JSON 字符串（不含两侧引号）。
pub fn json_escape(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            _ => o.push(c),
        }
    }
    o
}
