//! 极小的 JSON 解析器（Phase 3）。
//!
//! 为什么不用 serde：Vita 的交叉编译链上不想多任何依赖，而我们只需要
//! "把服务端的响应读成树、按 key 取值" 这一件事。文件很小，能单测。
#![allow(dead_code)]

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn parse(text: &str) -> Result<Json, &'static str> {
        let mut p = Parser {
            b: text.as_bytes(),
            i: 0,
        };
        let v = p.value()?;
        p.ws();
        if p.i != p.b.len() {
            return Err("json: 值后面还有多余内容");
        }
        Ok(v)
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn at(&self, index: usize) -> Option<&Json> {
        match self {
            Json::Arr(items) => items.get(index),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Num(n) if n.fract() == 0.0 && *n >= i64::MIN as f64 && *n < i64::MAX as f64 => {
                Some(*n as i64)
            }
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Num(n) if n.fract() == 0.0 && *n >= 0.0 && *n < u64::MAX as f64 => {
                Some(*n as u64)
            }
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    /// `true` / `false` 字面量。网易云的布尔字段（例如歌单的 `subscribed`）
    /// 偶尔也会用 0/1 表示，所以数字 0/1 也认。
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            Json::Num(n) if *n == 0.0 => Some(false),
            Json::Num(n) if *n == 1.0 => Some(true),
            _ => None,
        }
    }
}

/// 把一个字符串转义成 JSON 字符串字面量的内容（不含两侧引号）。
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&alloc::format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let c = self.peek();
        if c.is_some() {
            self.i += 1;
        }
        c
    }

    fn expect(&mut self, c: u8) -> Result<(), &'static str> {
        if self.next() == Some(c) {
            Ok(())
        } else {
            Err("json: 结构字符不对")
        }
    }

    fn lit(&mut self, lit: &[u8]) -> Result<(), &'static str> {
        if self.b.len() >= self.i + lit.len() && &self.b[self.i..self.i + lit.len()] == lit {
            self.i += lit.len();
            Ok(())
        } else {
            Err("json: 字面量不对")
        }
    }

    fn value(&mut self) -> Result<Json, &'static str> {
        self.ws();
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => {
                self.lit(b"true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.lit(b"false")?;
                Ok(Json::Bool(false))
            }
            Some(b'n') => {
                self.lit(b"null")?;
                Ok(Json::Null)
            }
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            _ => Err("json: 这里应该是一个值"),
        }
    }

    fn object(&mut self) -> Result<Json, &'static str> {
        self.expect(b'{')?;
        let mut fields = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Json::Obj(fields));
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.ws();
            self.expect(b':')?;
            let value = self.value()?;
            fields.push((key, value));
            self.ws();
            match self.next() {
                Some(b',') => {}
                Some(b'}') => break,
                _ => return Err("json: 对象缺少逗号或右括号"),
            }
        }
        Ok(Json::Obj(fields))
    }

    fn array(&mut self) -> Result<Json, &'static str> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            items.push(self.value()?);
            self.ws();
            match self.next() {
                Some(b',') => {}
                Some(b']') => break,
                _ => return Err("json: 数组缺少逗号或右括号"),
            }
        }
        Ok(Json::Arr(items))
    }

    fn string(&mut self) -> Result<String, &'static str> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let c = self.next().ok_or("json: 字符串没有收尾")?;
            match c {
                b'"' => return Ok(out),
                b'\\' => {
                    let e = self.next().ok_or("json: 转义没有收尾")?;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            if (0xD800..=0xDBFF).contains(&hi) {
                                self.expect(b'\\')?;
                                self.expect(b'u')?;
                                let lo = self.hex4()?;
                                if !(0xDC00..=0xDFFF).contains(&lo) {
                                    return Err("json: 代理对不完整");
                                }
                                let cp = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                                out.push(char::from_u32(cp).ok_or("json: 非法码点")?);
                            } else if (0xDC00..=0xDFFF).contains(&hi) {
                                return Err("json: 孤立的低代理");
                            } else {
                                out.push(char::from_u32(hi).ok_or("json: 非法码点")?);
                            }
                        }
                        _ => return Err("json: 未知转义"),
                    }
                }
                c if c < 0x20 => return Err("json: 字符串里有控制字符"),
                c if c < 0x80 => out.push(c as char),
                _ => {
                    /* 非 ASCII：退回一个字节，按 UTF-8 整字符消费。 */
                    self.i -= 1;
                    let rest =
                        core::str::from_utf8(&self.b[self.i..]).map_err(|_| "json: 非法 UTF-8")?;
                    let ch = rest.chars().next().ok_or("json: 字符串被截断")?;
                    out.push(ch);
                    self.i += ch.len_utf8();
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, &'static str> {
        let mut v = 0u32;
        for _ in 0..4 {
            let c = self.next().ok_or("json: 转义码点不完整")?;
            let d = (c as char).to_digit(16).ok_or("json: 转义码点不是十六进制")?;
            v = (v << 4) | d;
        }
        Ok(v)
    }

    fn number(&mut self) -> Result<Json, &'static str> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.i += 1;
                if matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                    return Err("json: 数字不能有前导零");
                }
            }
            Some(c) if c.is_ascii_digit() => {
                while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                    self.i += 1;
                }
            }
            _ => return Err("json: 数字格式不对"),
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            if !matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                return Err("json: 小数点后没有数字");
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                return Err("json: 指数没有数字");
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        let text = core::str::from_utf8(&self.b[start..self.i]).map_err(|_| "json: 非法数字")?;
        let v: f64 = text.parse().map_err(|_| "json: 数字解析失败")?;
        Ok(Json::Num(v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_response_shape() {
        let v = Json::parse(
            r#"{"data":[{"url":"http://x/y.m4a","size":7771899,"freeTrialInfo":null}],"code":200}"#,
        )
        .unwrap();
        assert_eq!(v.get("code").and_then(Json::as_u64), Some(200));
        let item = v.get("data").and_then(|d| d.at(0)).unwrap();
        assert_eq!(
            item.get("url").and_then(Json::as_str),
            Some("http://x/y.m4a")
        );
        assert_eq!(item.get("size").and_then(Json::as_u64), Some(7771899));
        assert!(item.get("freeTrialInfo").unwrap().is_null());
    }

    #[test]
    fn decodes_escapes_and_unicode_surrogate_pairs() {
        let v = Json::parse(r#"{"s":"a\"b\\c\n\t\u4e2d\ud83d\ude00"}"#).unwrap();
        assert_eq!(
            v.get("s").and_then(Json::as_str),
            Some("a\"b\\c\n\t中😀")
        );
    }

    #[test]
    fn numbers_keep_integer_precision_in_our_range() {
        let v = Json::parse(
            r#"{"size":7771899,"br":256009,"peak":1.0646,"exp":1e3,"neg":-1}"#,
        )
        .unwrap();
        assert_eq!(v.get("size").and_then(Json::as_u64), Some(7771899));
        assert_eq!(v.get("peak").and_then(Json::as_f64), Some(1.0646));
        assert_eq!(v.get("exp").and_then(Json::as_u64), Some(1000));
        assert_eq!(v.get("neg").and_then(Json::as_i64), Some(-1));
        assert_eq!(v.get("neg").and_then(Json::as_u64), None);
    }

    #[test]
    fn rejects_malformed_documents() {
        assert!(Json::parse("").is_err());
        assert!(Json::parse("{").is_err());
        assert!(Json::parse(r#"{"a":}"#).is_err());
        assert!(Json::parse(r#"{"a":1} trailing"#).is_err());
        assert!(Json::parse(r#"{"a":01}"#).is_err());
    }

    #[test]
    fn missing_keys_and_out_of_range_indexes_return_none() {
        let v = Json::parse(r#"{"data":[]}"#).unwrap();
        assert!(v.get("nope").is_none());
        assert!(v.get("data").unwrap().at(0).is_none());
        assert!(v.at(0).is_none());
        assert!(v.as_str().is_none());
    }
}
