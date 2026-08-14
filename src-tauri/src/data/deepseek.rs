// DeepSeek（自带 key 的 provider）— 对齐 Electron src/data/deepseek.js。
// token 用量：本地 model_usage 按 provider_id 聚合；余额：官方 /user/balance
// 后台刷新 + 缓存，status() 路径零网络。

use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::data::sqlite::{read_provider_usage, TtlMemo};
use crate::data::volc::{provider_api_key, provider_ids_by_baseurl};
use crate::data::Cache;

pub const CACHE_TTL: u64 = 15;
const BALANCE_URL: &str = "https://api.deepseek.com/user/balance";

fn balance_cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::new()))
}

pub fn get_deepseek_usage() -> Value {
    static MEMO: OnceLock<Mutex<TtlMemo>> = OnceLock::new();
    MEMO
        .get_or_init(|| {
            Mutex::new(TtlMemo::new(15_000, || read_provider_usage(&provider_ids_by_baseurl("api.deepseek.com"))))
        })
        .lock()
        .map(|mut m| m.get())
        .unwrap_or_else(|_| {
            json!({ "enabled": false, "today": json!({}), "week": json!({}), "month": json!({}) })
        })
}

fn empty_balance(enabled: bool, error: &str) -> Value {
    json!({
        "enabled": enabled,
        "balance": "",
        "currency": "",
        "isAvailable": false,
        "error": error,
        "updatedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    })
}

pub fn get_deepseek_balance() -> Value {
    let cached = balance_cache().lock().map(|c| c.get()).unwrap_or(Value::Null);
    if cached.is_null() {
        empty_balance(!provider_api_key("api.deepseek.com").is_empty(), "")
    } else {
        cached
    }
}

async fn fetch_balance(api_key: &str) -> Option<Value> {
    let body = crate::data::net::get(BALANCE_URL, &[("Authorization", &format!("Bearer {api_key}")), ("Accept", "application/json")]).await?;
    serde_json::from_str(&body).ok()
}

pub async fn refresh_once() {
    let api_key = provider_api_key("api.deepseek.com");
    if api_key.is_empty() {
        balance_cache().lock().unwrap().set(empty_balance(false, ""));
        return;
    }

    // 网络调用期间不持锁（MutexGuard 跨 await 会让 future 非 Send）
    let obj = fetch_balance(&api_key).await;
    let mut guard = balance_cache().lock().unwrap();
    let Some(obj) = obj else {
        // 失败：复用旧缓存（若有效）；否则写 error 结构
        let old = guard.get();
        let has_valid = old.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false)
            && old.get("balance").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
        if !has_valid {
            guard.set(empty_balance(true, "查询失败"));
        }
        return;
    };

    let infos = obj.get("balance_infos").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let info = infos
        .iter()
        .find(|b| b.get("currency").and_then(|v| v.as_str()) == Some("CNY"))
        .or_else(|| infos.first())
        .cloned()
        .unwrap_or(json!({}));
    guard.set(json!({
        "enabled": true,
        "balance": info.get("total_balance").cloned().unwrap_or(json!("")),
        "currency": info.get("currency").and_then(|v| v.as_str()).unwrap_or("CNY"),
        "isAvailable": obj.get("is_available").and_then(|v| v.as_bool()).unwrap_or(false),
        "error": "",
        "updatedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    }));
}
