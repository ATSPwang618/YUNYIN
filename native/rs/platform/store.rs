//! Tiny key-value files under `ux0:/data/yunyin/store`.

fn store_path(key: &str) -> String {
    format!("ux0:/data/yunyin/store/{}", key)
}

fn sanitize(key: &str) -> String {
    key.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

pub fn get(key: &str) -> String {
    let data = std::fs::read(store_path(&sanitize(key))).unwrap_or_default();
    String::from_utf8_lossy(&data).into_owned()
}

pub fn set(key: &str, val: &str) {
    let _ = std::fs::create_dir_all("ux0:/data/yunyin/store");
    let _ = std::fs::write(store_path(&sanitize(key)), val.as_bytes());
}
