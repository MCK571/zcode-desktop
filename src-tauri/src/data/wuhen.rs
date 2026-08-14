// 无痕中转（api.wuhen-ai.com）— 余额 + 用量查询，对齐 Electron src/data/wuhen.js。
// key 从 ~/.zcode/v2/config.json 按 baseURL 关键字匹配；GET /v1/usage 一次拿
// 余额 + 今日/累计 + 按日 + 按模型。后台刷新 + 缓存。

use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::data::volc::provider_api_key;
use crate::data::Cache;

pub const CACHE_TTL: u64 = 15;
const USAGE_URL: &str = "https://api.wuhen-ai.com/v1/usage";

fn usage_cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::new()))
}

fn empty_payload(enabled: bool, error: &str) -> Value {
    json!({
        "enabled": enabled,
        "balance": "",
        "unit": "",
        "isValid": false,
        "planName": "",
        "today": { "requests": 0, "tokens": 0, "cost": 0 },
        "total": { "requests": 0, "tokens": 0, "cost": 0 },
        "daily": [],
        "models": [],
        "error": error,
        "updatedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    })
}

pub fn get_wuhen_usage() -> Value {
    let cached = usage_cache().lock().map(|c| c.get()).unwrap_or(Value::Null);
    if cached.is_null() {
        empty_payload(!provider_api_key("api.wuhen-ai.com").is_empty(), "")
    } else {
        cached
    }
}

async fn fetch_usage(api_key: &str) -> Option<Value> {
    let body = crate::data::net::get(USAGE_URL, &[("Authorization", &format!("Bearer {api_key}")), ("Accept", "application/json")]).await?;
    serde_json::from_str(&body).ok()
}

pub async fn refresh_once() {
    let api_key = provider_api_key("api.wuhen-ai.com");
    if api_key.is_empty() {
        usage_cache().lock().unwrap().set(empty_payload(false, ""));
        return;
    }

    // 网络调用期间不持锁（MutexGuard 跨 await 会让 future 非 Send）
    let obj = fetch_usage(&api_key).await;
    let mut guard = usage_cache().lock().unwrap();
    let Some(obj) = obj else {
        // 失败：复用旧缓存（若有效）；否则写 error 结构
        let old = guard.get();
        let has_valid = old.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false)
            && old.get("balance").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
        if !has_valid {
            guard.set(empty_payload(true, "查询失败"));
        }
        return;
    };

    let usage = obj.get("usage").cloned().unwrap_or(json!({}));
    let today = usage.get("today").cloned().unwrap_or(json!({}));
    let total = usage.get("total").cloned().unwrap_or(json!({}));

    let daily: Vec<Value> = obj
        .get("daily_usage")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .rev()
                .take(7)
                .map(|d| {
                    json!({
                        "date": d.get("date").cloned().unwrap_or(json!("")),
                        "requests": d.get("requests").and_then(|v| v.as_i64()).unwrap_or(0),
                        "tokens": d.get("total_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
                        "cost": d.get("cost").and_then(|v| v.as_i64()).unwrap_or(0),
                    })
                })
                .collect::<Vec<Value>>()
        })
        .unwrap_or_default()
        .into_iter()
        .rev()
        .collect();

    let models: Vec<Value> = obj
        .get("model_stats")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|m| {
                    json!({
                        "model": m.get("model").cloned().unwrap_or(json!("")),
                        "requests": m.get("requests").and_then(|v| v.as_i64()).unwrap_or(0),
                        "tokens": m.get("total_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
                        "cost": m.get("cost").and_then(|v| v.as_i64()).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    guard.set(json!({
        "enabled": true,
        "balance": obj.get("balance").or_else(|| obj.get("remaining")).cloned().unwrap_or(json!("")),
        "unit": obj.get("unit").and_then(|v| v.as_str()).unwrap_or("USD"),
        "isValid": obj.get("isValid").and_then(|v| v.as_bool()).unwrap_or(true),
        "planName": obj.get("planName").and_then(|v| v.as_str()).unwrap_or(""),
        "today": {
            "requests": today.get("requests").and_then(|v| v.as_i64()).unwrap_or(0),
            "tokens": today.get("total_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
            "cost": today.get("cost").and_then(|v| v.as_i64()).unwrap_or(0),
            "actualCost": today.get("actual_cost").and_then(|v| v.as_i64()).unwrap_or(0),
        },
        "total": {
            "requests": total.get("requests").and_then(|v| v.as_i64()).unwrap_or(0),
            "tokens": total.get("total_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
            "cost": total.get("cost").and_then(|v| v.as_i64()).unwrap_or(0),
            "actualCost": total.get("actual_cost").and_then(|v| v.as_i64()).unwrap_or(0),
        },
        "daily": daily,
        "models": models,
        "error": "",
        "updatedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    }));
}
