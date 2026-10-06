//! 账号歌单（Phase 4）：登录后拉"我的歌单"列表，以及某张歌单的歌曲。
//!
//! 接口（全部 weapi，加密参数 + 会话 Cookie，登录后自动带 MUSIC_U）：
//!
//! ```text
//! /api/nuser/account/get    → account.id / profile.userId（拿 uid）
//! /api/user/playlist        → playlist[] {id, name, trackCount, coverImgUrl}
//! /api/v6/playlist/detail   → playlist.{name, tracks[]}（歌曲节点与 song/detail 同构）
//! ```
//!
//! 拿到之后界面就能"按网易云歌单分门别类"显示：歌单页列出每张歌单，
//! 点进去直接播里面的歌（每首按 `netease:<id>` 走解析，不需要写 playlist.json）。

use super::account::Session;
use super::api::{self, Call, Flavour};
use super::detail::{parse_song_node, SongDetail};
use super::json::{self, Json};
use crate::media::net::transport::FormPost;
use crate::media::provider::ProviderError;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug)]
pub struct PlaylistInfo {
    pub id: String,
    pub name: String,
    pub count: u32,
}

/// 歌单里的一首歌：`id` 是网易云歌曲 ID（播放时解析成地址），detail 是公共字段。
pub struct CloudSong {
    pub id: String,
    pub detail: SongDetail,
    /// 下架 / 无版权（网易云里显示成灰色的那种）：能列出来，但播不了。
    pub off: bool,
    /// 当前账号**拿不到**播放资源（服务端 `pl == 0`）：多半是会员 / 购买限定。
    /// 这是服务端的裁决，不是客户端猜的。
    pub vip: bool,
    /// `pl` = 当前账号实际允许播放的码率（0 = 没有）；`plLevel` = 对应音质档。
    /// `None` = 响应里没带 privileges（**不能**当成"不能播"）。
    pub pl: Option<u64>,
    pub pl_level: Option<String>,
    /// 网易云的 `fee`：0 免费 / 1 VIP 歌曲 / 4 购买专辑 / 8 低音质免费。
    /// **只用来在列表里打「VIP」标签**，不参与播放裁决（裁决看 pl / st）。
    pub fee: u8,
}

/// 一条 `privileges[]` 记录：网易云**按当前账号**算出来的播放权限。
#[derive(Clone, Debug)]
struct Priv {
    st: i64,
    pl: u64,
    pl_level: String,
    fee: u8,
}

/*
 * 读 `privileges[]` —— "这首歌此刻能不能播、能播什么音质"。
 *
 *   `st`      < 0（常见 -200）= 下架 / 无版权；
 *   `pl`      = 当前身份允许播放的码率，**0 = 拿不到播放资源**（会员 / 购买限定）；
 *   `plLevel` = 对应的音质档（standard / higher / exhigh / lossless / …）。
 *
 * 只看 `fee` 判断"能不能播"是不对的：`fee=1` 只说明"这是会员内容"
 * （VIP 账号照样能播），`fee=0` 也可能 `pl=0`（已下架）。
 * 参考实现（ClouDS-Music）干脆把裁决交给服务端，这里读的就是服务端的裁决。
 */
fn privilege_map(root: &Json) -> alloc::collections::BTreeMap<String, Priv> {
    let mut out = alloc::collections::BTreeMap::new();
    /* 榜单 / 歌单挂在根上，每日推荐在 data 里 —— 两处都看一眼。 */
    let sources = [
        root.get("privileges"),
        root.get("data").and_then(|d| d.get("privileges")),
    ];
    for list in sources.into_iter().flatten() {
        let mut i = 0usize;
        while let Some(p) = list.at(i) {
            i += 1;
            let id = as_id(p.get("id"));
            if id.is_empty() {
                continue;
            }
            out.insert(
                id,
                Priv {
                    st: p.get("st").and_then(Json::as_i64).unwrap_or(0),
                    pl: p.get("pl").and_then(Json::as_u64).unwrap_or(0),
                    fee: p.get("fee").and_then(Json::as_u64).unwrap_or(0).min(255) as u8,
                    pl_level: p
                        .get("plLevel")
                        .and_then(Json::as_str)
                        .unwrap_or("")
                        .trim()
                        .to_string(),
                },
            );
        }
    }
    out
}

/// 把"这首歌能播到什么音质"记进 provider 的小表：播放时按它选 level，
/// 不再自己猜（猜错就是 403，白等两轮）。
fn remember_level(id: &str, p: &Priv) {
    if !id.is_empty() && !p.pl_level.is_empty() {
        super::remember_level_hint(id, &p.pl_level);
    }
}

/// 一个 song 节点 + 它的权限 → `CloudSong`。
fn song_from_node(
    node: &Json,
    privs: &alloc::collections::BTreeMap<String, Priv>,
) -> Option<CloudSong> {
    let id = as_id(node.get("id"));
    if id.is_empty() {
        return None;
    }
    let p = privs.get(&id);
    if let Some(p) = p {
        remember_level(&id, p);
    }
    let st = p.map(|x| x.st).unwrap_or(0);
    let pl = p.map(|x| x.pl);
    /*
     * `fee` 三处取：节点自己 → privileges → 0。
     * 只做"VIP"标签（ClouDS-Music 的做法），**不参与能否播放的判断** ——
     * VIP 账号对 fee=1 的歌照样能播。
     */
    let fee = node
        .get("fee")
        .and_then(Json::as_u64)
        .or_else(|| p.map(|x| x.fee as u64))
        .unwrap_or(0)
        .min(255) as u8;
    Some(CloudSong {
        id,
        detail: parse_song_node(node),
        /* 只有真的拿到了 privileges 才敢判"不可播" */
        off: p.is_some() && st < 0,
        vip: p.is_some() && st >= 0 && pl == Some(0),
        pl,
        pl_level: p.map(|x| x.pl_level.clone()).filter(|s| !s.is_empty()),
        fee,
    })
}

/// 发一次 weapi 调用并校验 `code == 200`。
pub(crate) fn call_json(
    path: &'static str,
    params: Vec<(String, String)>,
    secret: &[u8; 16],
    post: &mut dyn FormPost,
    session: &Session,
) -> Result<Json, ProviderError> {
    let call = Call {
        path,
        flavour: Flavour::WeApi,
        params,
    };
    let reply = api::call_full(&call, secret, post, session)?;
    let root = Json::parse(&reply.body)
        .map_err(|e| ProviderError::Network(format!("响应不是 JSON：{e}")))?;
    if let Some(code) = root.get("code").and_then(Json::as_i64) {
        if code != 200 {
            /* 301 = 未登录；其它按网络错误报出去，日志里能看到原码 */
            if code == 301 {
                /* 服务器说未登录 = 这份 Cookie 不作数了：把内存会话丢掉，
                 * 界面才会退回"未登录"，用户才知道该重新扫码。 */
                super::session_expired();
                return Err(ProviderError::Auth(String::from("账号未登录")));
            }
            return Err(ProviderError::Network(format!("接口 code={code}")));
        }
    }
    Ok(root)
}

pub(crate) fn as_id(v: Option<&Json>) -> String {
    match v {
        Some(Json::Num(n)) => format!("{}", *n as i64),
        Some(Json::Str(s)) => String::from(s.trim()),
        _ => String::new(),
    }
}

/// 当前登录账号的 uid —— 会员歌单类接口都要先有它。
pub fn user_id(
    secret: &[u8; 16],
    post: &mut dyn FormPost,
    session: &Session,
) -> Result<String, ProviderError> {
    let root = call_json("/api/nuser/account/get", Vec::new(), secret, post, session)?;
    let uid = as_id(root.get("account").and_then(|a| a.get("id")));
    let uid = if uid.is_empty() {
        as_id(root.get("profile").and_then(|p| p.get("userId")))
    } else {
        uid
    };
    if uid.is_empty() || !uid.bytes().all(|b| b.is_ascii_digit()) {
        /* `account=null` 也是"未登录"的同一种表达（Cookie 过期/被踢下线）。 */
        if uid.is_empty() {
            super::session_expired();
        }
        return Err(ProviderError::Auth(String::from("没拿到账号 uid")));
    }
    Ok(uid)
}

/// 我**创建**的歌单列表（收藏的不要）。
///
/// 为什么要滤掉收藏的：账号攒久了，收藏歌单能到几十上百张，列表本身要解析、
/// 界面要列、点开每张还得拉一次歌单详情 —— 用户真正天天用的是自己那几张。
/// 判据用接口返回的 `subscribed`（收藏 = true）与 `userId`（自己的 = 账号 uid），
/// 两个都在就用，取不到就按 `subscribed` 判断，绝不误删自己的歌单。
pub fn my_playlists(
    secret: &[u8; 16],
    post: &mut dyn FormPost,
    session: &Session,
) -> Result<Vec<PlaylistInfo>, ProviderError> {
    let uid = user_id(secret, post, session)?;
    let root = call_json(
        "/api/user/playlist",
        vec![
            (String::from("uid"), uid.clone()),
            (String::from("offset"), String::from("0")),
            (String::from("limit"), String::from("1000")),
            (String::from("includeVideo"), String::from("true")),
        ],
        secret,
        post,
        session,
    )?;

    let mut out = Vec::new();
    if let Some(list) = root.get("playlist") {
        let mut i = 0usize;
        while let Some(p) = list.at(i) {
            i += 1;
            let id = as_id(p.get("id"));
            let name = p
                .get("name")
                .and_then(Json::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let count = p
                .get("trackCount")
                .and_then(Json::as_u64)
                .unwrap_or(0)
                .min(u32::MAX as u64) as u32;
            if id.is_empty() || name.is_empty() {
                continue;
            }
            /* 收藏的跳过：`subscribed=true` 或 creator.userId != 自己的 uid */
            let subscribed = p
                .get("subscribed")
                .and_then(Json::as_bool)
                .unwrap_or(false);
            let owner = as_id(p.get("userId"));
            let owner = if owner.is_empty() {
                as_id(p.get("creator").and_then(|c| c.get("userId")))
            } else {
                owner
            };
            if subscribed || (!owner.is_empty() && owner != uid) {
                continue;
            }
            out.push(PlaylistInfo { id, name, count });
        }
    }
    if out.is_empty() {
        crate::media::platform::log::append("list: 我的歌单是空表（账号里没有自建歌单？）");
        return Err(ProviderError::NotFound);
    }
    Ok(out)
}

/// 某张歌单的歌曲（返回歌单名 + 歌曲列表）。
pub fn playlist_tracks(
    playlist_id: &str,
    secret: &[u8; 16],
    post: &mut dyn FormPost,
    session: &Session,
) -> Result<(String, Vec<CloudSong>), ProviderError> {
    if playlist_id.is_empty() || !playlist_id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ProviderError::NotFound);
    }
    let root = call_json(
        "/api/v6/playlist/detail",
        vec![
            (String::from("id"), String::from(playlist_id)),
            (String::from("n"), String::from("1000")),
            (String::from("s"), String::from("8")),
        ],
        secret,
        post,
        session,
    )?;
    let pl = root.get("playlist").ok_or(ProviderError::NotFound)?;
    let name = pl
        .get("name")
        .and_then(Json::as_str)
        .unwrap_or("")
        .trim()
        .to_string();

    /* 下架/无版权的歌在 tracks 里可能是 null（彻底查不到信息），也可能
     * 是正常节点但 privileges.st < 0（能显示、不能播）。前者只能跳过，
     * 后者按"灰色"标记出来。 */
    let privs = privilege_map(&root);
    let mut out = Vec::new();
    let mut removed = 0usize;
    if let Some(tracks) = pl.get("tracks") {
        let mut i = 0usize;
        while let Some(node) = tracks.at(i) {
            i += 1;
            /* 无版权/下架的曲目在网易云里是 null 占位，跳过 */
            if node.is_null() {
                removed += 1;
                continue;
            }
            let id = as_id(node.get("id"));
            if id.is_empty() {
                continue;
            }
            if let Some(song) = song_from_node(node, &privs) {
                out.push(song);
            }
        }
    }
    if removed > 0 {
        crate::media::platform::log::append(&format!(
            "playlist: {name} 有 {removed} 首已下架（网易云只给 null 占位，无法列出歌名）"
        ));
    }
    if out.is_empty() {
        return Err(ProviderError::NotFound);
    }
    Ok((name, out))
}

/// 推荐歌单（发现页的"热门推荐"）：`/api/personalized/playlist`。
pub fn recommend_playlists(
    secret: &[u8; 16],
    post: &mut dyn FormPost,
    session: &Session,
) -> Result<Vec<PlaylistInfo>, ProviderError> {
    let root = call_json(
        "/api/personalized/playlist",
        vec![
            /* 发现页只放 8 张：30 张要接连串行拉、界面还得排 30 行，
             * 真机上又慢又占地方（用户明确要求砍到 8）。 */
            (String::from("limit"), String::from("8")),
            (String::from("total"), String::from("true")),
            (String::from("n"), String::from("1000")),
        ],
        secret,
        post,
        session,
    )?;
    let mut out = Vec::new();
    if let Some(list) = root.get("result") {
        let mut i = 0usize;
        while let Some(p) = list.at(i) {
            i += 1;
            let id = as_id(p.get("id"));
            let name = p
                .get("name")
                .and_then(Json::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let count = p
                .get("trackCount")
                .and_then(Json::as_u64)
                .unwrap_or(0)
                .min(u32::MAX as u64) as u32;
            if id.is_empty() || name.is_empty() {
                continue;
            }
            out.push(PlaylistInfo { id, name, count });
        }
    }
    if out.is_empty() {
        /* 这里空的是**推荐歌单**（发现页），和每日推荐无关。 */
        crate::media::platform::log::append("list: 热门推荐是空表（接口没给 result？）");
        return Err(ProviderError::NotFound);
    }
    Ok(out)
}

/// 每日推荐（需登录）：`/api/discovery/recommend/songs` → `data.dailySongs[]`。
pub fn daily_songs(
    secret: &[u8; 16],
    post: &mut dyn FormPost,
    session: &Session,
) -> Result<Vec<CloudSong>, ProviderError> {
    /*
     * 每日推荐：**加密 weapi**，关键是路径要带 `v3`。
     *
     * 实测（同一份 Cookie，2026-10-06，两条都是加密 POST）：
     *   /weapi/v3/discovery/recommend/songs → 200，`data.dailySongs` 23~31 首
     *   /weapi/discovery/recommend/songs    → 200，`dailySongs` 0（空壳）
     * 以前打的就是后者 —— "每日推荐永远是空的"的真正原因，与登录/会员无关。
     *
     * 数量每天、甚至每次刷新都会浮动（服务端按需给），不是我们算错。
     */
    let root = call_json(
        "/api/v3/discovery/recommend/songs",
        vec![(String::from("br"), String::from("320000"))],
        secret,
        post,
        session,
    )?;
    let songs = collect_daily(&root);
    if !songs.is_empty() {
        return Ok(songs);
    }
    /* v3 空（老账号 / 接口形状变了）：退 v1，**同样走加密 weapi**。 */
    let root = call_json(
        "/api/v1/discovery/recommend/songs",
        vec![(String::from("br"), String::from("320000"))],
        secret,
        post,
        session,
    )?;
    let songs = collect_daily(&root);
    if songs.is_empty() {
        return Err(ProviderError::NotFound);
    }
    Ok(songs)
}

/// 从每日推荐响应里收歌：两个版本都放在 `data.dailySongs`，
/// 但**节点字段名不同**（v3 用 `ar/al/dt`，v1 用 `artists/album/duration`），
/// 所以先按通用解析，再把缺的字段按老名字补一遍。
fn collect_daily(root: &Json) -> Vec<CloudSong> {
    let Some(list) = root.get("data").and_then(|d| d.get("dailySongs")) else {
        return Vec::new();
    };
    let privs = privilege_map(root);
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(node) = list.at(i) {
        i += 1;
        if node.is_null() {
            continue;
        }
        let id = as_id(node.get("id"));
        if id.is_empty() {
            continue;
        }
        let mut detail = parse_song_node(node);
        /*
         * **只在通用解析没拿到时**才按老字段名补。
         * 不能无条件覆盖：v3 的节点里同样有 `name`，但没有 `artists` ——
         * 无条件写会把 v3 已经解析好的歌手名清成空串。
         */
        if detail.title.is_empty() {
            detail.title = node
                .get("name")
                .and_then(Json::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
        }
        if detail.artists.is_empty() {
            let mut artists: Vec<&str> = Vec::new();
            if let Some(ar) = node.get("artists") {
                let mut k = 0usize;
                while let Some(entry) = ar.at(k) {
                    k += 1;
                    if let Some(name) = entry.get("name").and_then(Json::as_str) {
                        artists.push(name);
                    }
                }
            }
            detail.artists = artists.join(" / ");
        }
        if detail.album.is_empty() {
            detail.album = node
                .get("album")
                .and_then(|a| a.get("name"))
                .and_then(Json::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
        }
        if detail.cover_url.is_none() {
            detail.cover_url = node
                .get("album")
                .and_then(|a| a.get("picUrl"))
                .and_then(Json::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
        }
        if detail.duration_ms == 0 {
            detail.duration_ms = node
                .get("duration")
                .and_then(Json::as_u64)
                .unwrap_or(0)
                .min(u32::MAX as u64) as u32;
        }
        let p = privs.get(&id);
        let pl = p.map(|x| x.pl);
        let st = p.map(|x| x.st).unwrap_or(0);
        if let Some(p) = p {
            remember_level(&id, p);
        }
        out.push(CloudSong {
            id,
            detail,
            off: p.is_some() && st < 0,
            vip: p.is_some() && st >= 0 && pl == Some(0),
            pl,
            pl_level: p.map(|x| x.pl_level.clone()).filter(|s| !s.is_empty()),
            fee: p.map(|x| x.fee).unwrap_or(0),
        });
    }
    out
}

pub fn playlists_json(list: &[PlaylistInfo]) -> String {
    let mut s = String::from("[");
    for (i, p) in list.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "{{\"id\":\"{}\",\"name\":\"{}\",\"count\":{}}}",
            json::escape(&p.id),
            json::escape(&p.name),
            p.count
        ));
    }
    s.push(']');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::provider::netease::test_support::{FakePost, FIXED_SECRET};
    use core::sync::atomic::Ordering;

    /// 服务器明确回 `account=null` 时：
    ///   * 调用方必须拿到 `Auth`（不能当成"同步成功"）；
    ///   * 内存里的会话必须被丢掉 —— 否则界面一直显示"已登录"，
    ///     歌单却全在失败，用户会以为应用在装假。
    #[test]
    fn account_null_means_logged_out_and_drops_the_session() {
        super::super::SESSION_LOADED.store(true, Ordering::Release);
        if let Ok(mut slot) = super::super::SESSION.lock() {
            *slot = Some(Session::from_cookie("MUSIC_U=expired"));
        }
        assert!(super::super::is_logged_in(), "前置条件：先装作已登录");

        let mut post = FakePost::ok(r#"{"code":200,"account":null,"profile":null}"#);
        let err = user_id(FIXED_SECRET, &mut post, &Session::from_cookie("MUSIC_U=expired"))
            .unwrap_err();
        assert!(
            matches!(err, ProviderError::Auth(_)),
            "account=null 必须报 Auth，实际是 {err:?}"
        );
        assert_eq!(post.calls(), 1, "只该问一次账号接口");
        assert!(
            !super::super::is_logged_in(),
            "服务器说未登录之后，内存会话必须回到未登录"
        );
    }
}

pub fn cloud_songs_json(list: &[CloudSong]) -> String {
    let mut s = String::from("[");
    for (i, song) in list.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "{{\"id\":\"{}\",\"title\":\"{}\",\"artists\":\"{}\",\"album\":\"{}\",\"durationMs\":{},\"off\":{},\"vip\":{},\"fee\":{}{}}}",
            json::escape(&song.id),
            json::escape(&song.detail.title),
            json::escape(&song.detail.artists),
            json::escape(&song.detail.album),
            song.detail.duration_ms,
            if song.off { 1 } else { 0 },
            if song.vip { 1 } else { 0 },
            song.fee,
            /* pl / plLevel 只有服务端真给了才写：界面据此判断"能不能播 + 什么音质" */
            match (&song.pl, &song.pl_level) {
                (Some(pl), Some(level)) => format!(
                    ",\"pl\":{},\"plLevel\":\"{}\"",
                    pl,
                    json::escape(level)
                ),
                _ => String::new(),
            }
        ));
    }
    s.push(']');
    s
}
