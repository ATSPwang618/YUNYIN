//! 磁盘缓存的电脑侧测试：写进去 → 命中 → 原样读回；写满 → 环形覆盖；
//! 跨环的条目也要能整段读出来（真机上这类错会表现成"歌播到一半断"）。

use super::diskcache::DiskCache;
use std::path::PathBuf;

fn temp_cache(name: &str, cap: u64) -> (DiskCache, PathBuf) {
    let dir = std::env::temp_dir().join(format!("yunyin-cache-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let data = dir.join("cache.dat");
    let index = dir.join("cache.idx");
    let cache = DiskCache::new(
        data.to_str().unwrap(),
        index.to_str().unwrap(),
        cap,
    );
    (cache, dir)
}

fn write_all(cache: &DiskCache, key: &str, bytes: &[u8]) {
    let mut w = cache
        .begin_write(key, bytes.len() as u64)
        .unwrap_or_else(|| panic!("{key} 应该能进缓存"));
    assert!(w.accepts(0));
    w.append(bytes);
    w.finish();
}

fn read_back(cache: &DiskCache, key: &str, expect_len: usize) -> Vec<u8> {
    let (off, len, total) = cache.lookup(key).unwrap_or_else(|| panic!("{key} 应命中"));
    let mut r = cache.open_reader(off, len, total).expect("能打开读通道");
    let mut buf = vec![0u8; expect_len];
    let mut filled = 0usize;
    while filled < expect_len {
        let n = r.read_at(filled as u64, &mut buf[filled..]).expect("读不失败");
        if n == 0 {
            break;
        }
        filled += n;
    }
    assert_eq!(filled, expect_len, "{key} 应能整段读出");
    buf
}

#[test]
fn 写完就能命中并且原样读回() {
    let (cache, _dir) = temp_cache("hit", 4096);
    let payload: Vec<u8> = (0..1500u32).map(|i| (i % 251) as u8).collect();
    write_all(&cache, "n123", &payload);
    assert_eq!(read_back(&cache, "n123", payload.len()), payload);
    let (hits, _) = cache.stats();
    assert_eq!(hits, 1);
}

#[test]
fn 没写完的不进索引也不给命中() {
    let (cache, _dir) = temp_cache("partial", 4096);
    {
        let mut w = cache.begin_write("n9", 4000).expect("能开写入");
        w.append(&[7u8; 100]);
        /* 不调 finish：模拟切歌打断 */
    }
    assert!(cache.lookup("n9").is_none(), "半截数据不该命中");
}

#[test]
fn 写满一圈就环形覆盖旧的() {
    let (cache, _dir) = temp_cache("ring", 1024);
    write_all(&cache, "k1", &[1u8; 700]);
    write_all(&cache, "k2", &[2u8; 700]);
    assert!(cache.lookup("k1").is_none(), "被覆盖的旧条目要划掉");
    assert_eq!(read_back(&cache, "k2", 700), vec![2u8; 700]);
}

#[test]
fn 跨环末尾的条目也能整段读出() {
    /* 700 字节写在 [700,1400) → 越过 1024 的环尾，绕回 [0,376)。
     * 读的时候必须按环推进，否则尾部那 324 字节会读到文件末尾之外。 */
    let (cache, _dir) = temp_cache("wrap", 1024);
    write_all(&cache, "k1", &[1u8; 700]);
    write_all(&cache, "k2", &[2u8; 700]);
    let got = read_back(&cache, "k2", 700);
    assert!(got.iter().all(|b| *b == 2), "跨环的字节必须都对");
}

#[test]
fn 乱序探针的字节不写进缓存() {
    let (cache, _dir) = temp_cache("probe", 4096);
    let mut w = cache.begin_write("k3", 1000).expect("能开写入");
    assert!(w.accepts(0), "第一段要从 0 开始");
    w.append(&[3u8; 200]);
    assert!(w.accepts(200), "紧接着的才算顺序");
    assert!(!w.accepts(900), "解码器探文件尾巴那种不算");
    assert!(!w.is_full());
    w.append(&[3u8; 800]);
    assert!(w.is_full(), "写满总长度就算下完");
    w.finish();
    assert_eq!(read_back(&cache, "k3", 1000), vec![3u8; 1000]);
}
