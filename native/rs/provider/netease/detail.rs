//! 在线歌曲详情（Phase 4）：歌名 / 歌手 / 专辑 / 封面 / 时长。
//!
//! 接口 `/weapi/v3/song/detail`，参数 `c = [{"id":…}]`。
//! 电脑实测返回：`songs[0].{name, ar[].name, al.name, al.picUrl, dt}`。
//! 这些信息用来替代 `[在线] 歌曲ID` 这种占位显示，也从根上解决
//! "原生 path（CDN 网址）反查不到曲目"导致的显示错位。

use super::account::Session;
use super::api::{self, Call, Flavour};
use super::json::{self, Json};
use crate::media::net::transport::FormPost;
use crate::media::provider::ProviderError;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

#[derive(Clone, Debug, Default)]
pub struct SongDetail {
    /// 歌曲 ID —— **必须自带**：批量接口的返回顺序不保证和请求顺序一致
    /// （少一首、顺序变了都会串歌），缓存只认这个 id。
    pub id: String,
    pub title: String,
    /// 多位歌手用 " / " 连接。
    pub artists: String,
    pub album: String,
    pub cover_url: Option<String>,
    pub duration_ms: u32,
}

pub fn to_json(d: &SongDetail) -> String {
    format!(
        "{{\"title\":\"{}\",\"artists\":\"{}\",\"album\":\"{}\",\"coverUrl\":\"{}\",\"durationMs\":{}}}",
        json::escape(&d.title),
        json::escape(&d.artists),
        json::escape(&d.album),
        json::escape(d.cover_url.as_deref().unwrap_or("")),
        d.duration_ms,
    )
}

pub fn song_detail(
    song_id: &str,
    secret: &[u8; 16],
    post: &mut dyn FormPost,
    session: &Session,
) -> Result<SongDetail, ProviderError> {
    let mut list = songs_detail(&[String::from(song_id)], secret, post, session)?;
    if list.is_empty() {
        return Err(ProviderError::NotFound);
    }
    Ok(list.remove(0))
}

/// 批量详情：一次请求问多首（`c=[{"id":1},{"id":2}]`）。
///
/// 在线清单（`playlist.json`）里只写了 id 的条目，启动时用**一次**请求把所有
/// 占位名换成真名 —— 以前只有"正在播的那首"会去取，列表里其余全是
/// `[在线] 网络歌曲`，看着像重复条目。
pub fn songs_detail(
    ids: &[String],
    secret: &[u8; 16],
    post: &mut dyn FormPost,
    session: &Session,
) -> Result<Vec<SongDetail>, ProviderError> {
    let valid: Vec<&String> = ids
        .iter()
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
        .collect();
    if valid.is_empty() {
        return Ok(Vec::new());
    }
    let mut list = String::from("[");
    for (i, id) in valid.iter().enumerate() {
        if i > 0 {
            list.push(',');
        }
        list.push_str(&format!("{{\"id\":{id}}}"));
    }
    list.push(']');

    let call = Call {
        path: api::PATH_SONG_DETAIL,
        flavour: Flavour::WeApi,
        params: vec![(String::from("c"), list)],
    };
    let reply = api::call_full(&call, secret, post, session)?;
    let root = Json::parse(&reply.body)
        .map_err(|e| ProviderError::Network(format!("详情响应不是 JSON：{e}")))?;
    match root.get("code").and_then(Json::as_i64) {
        Some(200) => {}
        Some(c) => return Err(ProviderError::Network(format!("详情接口 code={c}"))),
        None => {
            return Err(ProviderError::Network(String::from(
                "详情响应缺少 code",
            )))
        }
    }
    let mut out = Vec::new();
    if let Some(songs) = root.get("songs") {
        let mut i = 0usize;
        while let Some(node) = songs.at(i) {
            out.push(parse_song_node(node));
            i += 1;
        }
    }
    if out.is_empty() {
        return Err(ProviderError::NotFound);
    }
    Ok(out)
}

/// 从"歌曲节点"里取公共字段 —— `/song/detail` 与 `/playlist/detail` 里的
/// 歌曲节点结构一致（name / ar[].name / al.name / al.picUrl / dt），共用一份解析。
pub fn parse_song_node(song: &Json) -> SongDetail {
    let id = match song.get("id") {
        Some(Json::Num(n)) => format!("{}", *n as i64),
        Some(Json::Str(s)) => String::from(s.trim()),
        _ => String::new(),
    };
    let title = song
        .get("name")
        .and_then(Json::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let album = song
        .get("al")
        .and_then(|a| a.get("name"))
        .and_then(Json::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let cover_url = song
        .get("al")
        .and_then(|a| a.get("picUrl"))
        .and_then(Json::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let mut artist_names: Vec<&str> = Vec::new();
    if let Some(ar) = song.get("ar") {
        let mut i = 0usize;
        while let Some(entry) = ar.at(i) {
            if let Some(name) = entry.get("name").and_then(Json::as_str) {
                artist_names.push(name);
            }
            i += 1;
        }
    }

    let duration_ms = song
        .get("dt")
        .and_then(Json::as_u64)
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32;

    SongDetail {
        id,
        title,
        artists: artist_names.join(" / "),
        album,
        cover_url,
        duration_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::provider::netease::test_support::{FakePost, FIXED_SECRET};

    const FIXTURE: &str = r#"{"songs":[{"name":"穿越苍穹的旅行","id":3346495279,"dt":241379,"al":{"name":"观海策之夜","picUrl":"https://p4.music.126.net/Ao3z8Adsv-yzXfN6LjKPyw==/109951172692547015.jpg"},"ar":[{"name":"主唱A"},{"name":"jixwang"},{"name":"小B"}]}],"code":200}"#;

    #[test]
    fn parses_song_detail_fields() {
        let mut post = FakePost::ok(FIXTURE);
        let d = song_detail("3346495279", FIXED_SECRET, &mut post, &Session::anonymous()).unwrap();
        assert_eq!(d.title, "穿越苍穹的旅行");
        assert_eq!(d.artists, "主唱A / jixwang / 小B");
        assert_eq!(d.album, "观海策之夜");
        assert_eq!(d.duration_ms, 241379);
        assert_eq!(
            d.cover_url.as_deref(),
            Some("https://p4.music.126.net/Ao3z8Adsv-yzXfN6LjKPyw==/109951172692547015.jpg")
        );
        // 请求确实走了 song/detail 的 weapi
        assert!(post.requests[0]
            .url
            .ends_with("/weapi/v3/song/detail?csrf_token="));
    }

    #[test]
    fn missing_song_is_not_found() {
        let mut post = FakePost::ok(r#"{"songs":[],"code":200}"#);
        let err = song_detail("1", FIXED_SECRET, &mut post, &Session::anonymous()).unwrap_err();
        assert_eq!(err, ProviderError::NotFound);
    }

    #[test]
    fn to_json_escapes_fields() {
        let d = SongDetail {
            id: String::from("1"),
            title: String::from("带\"引号\"歌"),
            artists: String::from("A / B"),
            album: String::from("某专辑"),
            cover_url: Some(String::from("https://x/y.jpg")),
            duration_ms: 1234,
        };
        assert!(to_json(&d).contains(r#""title":"带\"引号\"歌""#));
        assert!(to_json(&d).contains(r#""coverUrl":"https://x/y.jpg""#));
    }
}
