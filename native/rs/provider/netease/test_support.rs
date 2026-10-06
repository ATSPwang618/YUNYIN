//! Phase 3 的测试夹具：假传输 + 真响应样本（只在 `cargo test` 里编译）。
//!
//! 样本都是电脑上真接口抓下来的（URL 换成了假域名，其余字段一个不少）——
//! 假响应必须和真响应同形，否则解析代码在真机上碰到的字段这里碰不到。

use crate::media::net::transport::{FormPost, FormRequest, FormResponse, PostError};
use alloc::string::String;
use alloc::vec::Vec;

/// 与 `crypto` 固定向量对应的密钥，让 api 层的请求体也能逐字节断言。
pub const FIXED_SECRET: &[u8; 16] = b"0123456789abcdef";

pub struct FakePost {
    pub requests: Vec<FormRequest>,
    status: u16,
    reply: Vec<u8>,
    fail: Option<PostError>,
    pub cookie: Option<String>,
}

impl FakePost {
    pub fn ok(body: &str) -> Self {
        Self {
            requests: Vec::new(),
            status: 200,
            reply: body.as_bytes().to_vec(),
            fail: None,
            cookie: None,
        }
    }

    pub fn http_error(status: u16, body: &str) -> Self {
        Self {
            requests: Vec::new(),
            status,
            reply: body.as_bytes().to_vec(),
            fail: None,
            cookie: None,
        }
    }

    pub fn failing(fail: PostError) -> Self {
        Self {
            requests: Vec::new(),
            status: 0,
            reply: Vec::new(),
            fail: Some(fail),
            cookie: None,
        }
    }

    pub fn with_cookie(mut self, cookie: &str) -> Self {
        self.cookie = Some(String::from(cookie));
        self
    }

    pub fn calls(&self) -> usize {
        self.requests.len()
    }
}

impl FormPost for FakePost {
    fn post_form(&mut self, req: &FormRequest) -> Result<FormResponse, PostError> {
        self.requests.push(req.clone());
        if let Some(e) = &self.fail {
            return Err(e.clone());
        }
        Ok(FormResponse {
            status: self.status,
            bytes: self.reply.clone(),
            set_cookie: self.cookie.clone(),
        })
    }
}

/// 真接口响应（3346495279，256k AAC/M4A；URL 已换成假地址）。
pub const URL_V1_FIXTURE: &str = r#"{"data":[{"id":3346495279,"url":"http://example-cdn.invalid/aa/bb/cc.m4a?token=xyz","br":256009,"size":7771899,"md5":"0123456789abcdef0123456789abcdef","code":200,"expi":1200,"type":"m4a","gain":0.0,"peak":1.0646,"closedGain":0.0,"closedPeak":0.0,"fee":8,"uf":null,"payed":0,"flag":4,"canExtend":false,"freeTrialInfo":null,"level":"exhigh","encodeType":"aac","channelLayout":null,"freeTrialPrivilege":{"resConsumable":false,"userConsumable":false,"listenType":null,"cannotListenReason":null,"playReason":null,"freeLimitTagType":null},"freeTimeTrialPrivilege":{"resConsumable":false,"userConsumable":false,"type":0,"remainTime":0},"urlSource":0,"rightSource":0,"podcastCtrp":null,"effectTypes":null,"time":241379,"message":null,"levelConfuse":null,"musicId":"16099334456","accompany":null,"sr":48000,"auEff":null,"immerseType":null,"beatType":0}],"code":200}"#;

/// 真接口响应（186016，匿名拿不到版权：url=null、code=404）。
pub const NOT_FOUND_FIXTURE: &str = r#"{"data":[{"id":186016,"url":null,"br":0,"size":0,"md5":null,"code":404,"expi":1200,"type":null,"gain":0.0,"peak":null,"closedGain":0.0,"closedPeak":0.0,"fee":0,"uf":null,"payed":0,"flag":256,"canExtend":false,"freeTrialInfo":null,"level":null,"encodeType":null,"channelLayout":null,"freeTrialPrivilege":{"resConsumable":false,"userConsumable":false,"listenType":null,"cannotListenReason":1,"playReason":null,"freeLimitTagType":null},"freeTimeTrialPrivilege":{"resConsumable":false,"userConsumable":false,"type":0,"remainTime":0},"urlSource":0,"rightSource":0,"podcastCtrp":null,"effectTypes":null,"time":0,"message":null,"levelConfuse":null,"musicId":null,"accompany":null,"sr":0,"auEff":null,"immerseType":null,"beatType":0}],"code":200}"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_are_valid_json_shaped_text() {
        assert!(URL_V1_FIXTURE.contains("\"code\":200"));
        assert!(NOT_FOUND_FIXTURE.contains("\"url\":null"));
        assert_eq!(FIXED_SECRET.len(), 16);
    }

    #[test]
    fn fake_post_records_requests_and_replays_its_reply() {
        let mut p = FakePost::ok("{\"ok\":1}");
        let req = FormRequest {
            url: String::from("https://example.invalid/"),
            body: String::from("a=b"),
            referer: None,
            cookie: None,
            tls: crate::media::net::transport::TlsMode::Verify,
            max_body: 128 * 1024,
        };
        let resp = p.post_form(&req).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(p.calls(), 1);
        assert_eq!(p.requests[0].body, "a=b");
    }
}
