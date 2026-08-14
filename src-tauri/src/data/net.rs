// 共用 HTTP 客户端（对齐 deepseek.js/wuhen.js/opencode.js 的 https 调用）。

use std::time::Duration;

pub fn client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("reqwest client")
        })
        .clone()
}

// GET 文本；非 200 / 网络错误返回 None
pub async fn get(url: &str, headers: &[(&str, &str)]) -> Option<String> {
    let mut req = client().get(url);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.text().await.ok()
}
