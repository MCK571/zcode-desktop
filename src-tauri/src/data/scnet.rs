// SCNet Token Plan（scent）— 控制台 session cookie 抓取套餐额度，
// 对齐 opencode.rs 的 dashboard cookie 模式。凭证：.volc.env 的 SCNET_AUTH_COOKIE
// （浏览器 DevTools 复制，含 httpOnly jsessionid，过期需重新复制）。
// API：GET /acx/charge/account/currentuser/tokenplan/list，返回
// name/status/usedAmount/totalAmount/unit/totalDays/minValidTime/maxExpireTime。

use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::data::Cache;

pub const CACHE_TTL: u64 = 15;
const QUOTA_URL: &str = "https://www.scnet.cn/acx/charge/account/currentuser/tokenplan/list";

fn quota_cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::new()))
}

fn empty_payload(enabled: bool, error: &str) -> Value {
    json!({
        "enabled": enabled,
        "planName": "",
        "status": "",
        "unit": "CREDITS",
        "usedAmount": 0,
        "totalAmount": 0,
        "totalDays": 0,
        "startTime": "",
        "expireTime": "",
        "usedPct": 0.0,
        "error": error,
        "updatedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    })
}

pub fn get_scnet_usage() -> Value {
    let cached = quota_cache().lock().map(|c| c.get()).unwrap_or(Value::Null);
    if cached.is_null() {
        empty_payload(
            !std::env::var("SCNET_AUTH_COOKIE").map(|s| !s.is_empty()).unwrap_or(false),
            "",
        )
    } else {
        cached
    }
}

async fn fetch_quota(cookie: &str) -> Option<Value> {
    let body = crate::data::net::get(
        QUOTA_URL,
        &[
            ("Cookie", cookie),
            ("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36"),
            ("Referer", "https://www.scnet.cn/ui/console/index.html"),
            ("Accept", "application/json, text/plain, */*"),
        ],
    )
    .await?;
    let obj: Value = serde_json::from_str(&body).ok()?;
    let item = obj
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|a| a.first())?
        .clone();
    let used = item.get("usedAmount").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let total = item.get("totalAmount").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let used_pct = if total > 0.0 { (used / total * 100.0 * 10.0).round() / 10.0 } else { 0.0 };
    Some(json!({
        "enabled": true,
        "planName": item.get("name").and_then(|v| v.as_str()).unwrap_or(""),
        "status": item.get("status").and_then(|v| v.as_str()).unwrap_or(""),
        "unit": item.get("unit").and_then(|v| v.as_str()).unwrap_or("CREDITS"),
        "usedAmount": used,
        "totalAmount": total,
        "totalDays": item.get("totalDays").and_then(|v| v.as_i64()).unwrap_or(0),
        "startTime": item.get("minValidTime").and_then(|v| v.as_str()).unwrap_or(""),
        "expireTime": item.get("maxExpireTime").and_then(|v| v.as_str()).unwrap_or(""),
        "usedPct": used_pct,
        "error": "",
        "updatedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    }))
}

pub async fn refresh_once() {
    crate::data::volc::ensure_env_loaded();
    let cookie = std::env::var("SCNET_AUTH_COOKIE").unwrap_or_default().trim().to_string();
    if cookie.is_empty() {
        quota_cache().lock().unwrap().set(empty_payload(false, ""));
        return;
    }

    // 网络调用期间不持锁（MutexGuard 跨 await 会让 future 非 Send）
    let parsed = fetch_quota(&cookie).await;
    let mut guard = quota_cache().lock().unwrap();
    let Some(parsed) = parsed else {
        // 失败：复用旧缓存（若有效）；否则写 error 结构
        let old = guard.get();
        let has_valid = old.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false)
            && old.get("totalAmount").and_then(|v| v.as_f64()).map(|v| v > 0.0).unwrap_or(false);
        if !has_valid {
            guard.set(empty_payload(true, "查询失败（cookie 可能过期）"));
        }
        return;
    };
    guard.set(parsed);
}

#[cfg(test)]
mod tests {
    #[test]
    fn dump_scnet() {
        // 真实验证：从项目根 .volc.env 读 SCNET_AUTH_COOKIE（cargo test cwd 是 src-tauri）
        if let Ok(content) = std::fs::read_to_string("../.volc.env") {
            for line in content.lines() {
                let s = line.trim();
                if let Some(rest) = s.strip_prefix("SCNET_AUTH_COOKIE=") {
                    std::env::set_var("SCNET_AUTH_COOKIE", rest);
                    break;
                }
            }
        }
        let cookie = std::env::var("SCNET_AUTH_COOKIE").unwrap_or_default();
        println!("cookie_len={}", cookie.len());
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(super::refresh_once());
        let v = super::get_scnet_usage();
        println!("{}", serde_json::to_string(&v).unwrap());
    }
}
