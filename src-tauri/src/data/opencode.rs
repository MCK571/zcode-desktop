// opencode — 对齐 Electron src/data/opencode.js。
// token 用量：本地 model_usage 按 baseURL 含 opencode.ai 的 provider 聚合；
// Go 套餐余量：抓取 dashboard 页面解析（SolidJS SSR 水合数据 / data-slot）。

use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::data::sqlite::{read_provider_usage, TtlMemo};
use crate::data::volc::provider_ids_by_baseurl;
use crate::data::Cache;

pub const CACHE_TTL: u64 = 15;

fn go_cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::new()))
}

pub fn get_opencode_usage() -> Value {
    static MEMO: OnceLock<Mutex<TtlMemo>> = OnceLock::new();
    MEMO
        .get_or_init(|| {
            Mutex::new(TtlMemo::new(15_000, || read_provider_usage(&provider_ids_by_baseurl("opencode.ai"))))
        })
        .lock()
        .map(|mut m| m.get())
        .unwrap_or_else(|_| {
            json!({ "enabled": false, "today": json!({}), "week": json!({}), "month": json!({}) })
        })
}

pub fn get_opencode_go() -> Value {
    go_cache().lock().map(|c| c.get()).unwrap_or(Value::Null)
}

// 把 (已用百分比, 剩余秒) 归一成 window dict
fn make_window(used_pct: f64, reset_sec: f64) -> Value {
    let used_pct = used_pct.clamp(0.0, 100.0);
    let reset_sec = reset_sec.max(0.0);
    json!({
        "usedPct": (used_pct * 10.0).round() / 10.0,
        "remainingPct": ((100.0 - used_pct) * 10.0).round() / 10.0,
        "resetMs": ((chrono::Local::now().timestamp_millis() as f64 / 1000.0 + reset_sec) * 1000.0).round() as i64,
    })
}

// "6 days 2 hours 30 minutes" → 秒
fn parse_human_time(text: &str) -> Option<f64> {
    let normalized = text
        .to_lowercase()
        .replace('\u{2014}', " ")
        .trim()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if ["reset-now", "reset now", "now", "resets now"].contains(&normalized.as_str()) {
        return Some(0.0);
    }
    let mut total = 0.0;
    let mut found = false;
    for (unit, mult) in [("days?", 86400.0), ("hours?", 3600.0), ("minutes?", 60.0), ("seconds?", 1.0)] {
        let re = regex::Regex::new(&format!(r"([\d.]+)\s*{unit}")).unwrap();
        if let Some(cap) = re.captures(&normalized) {
            if let Ok(v) = cap[1].parse::<f64>() {
                total += v * mult;
                found = true;
            }
        }
    }
    if found {
        Some(total)
    } else {
        None
    }
}

fn parse_window(html: &str, field: &str) -> Option<Value> {
    // SolidJS SSR：usagePercent 与 resetInSec 两种顺序
    for pct_first in [true, false] {
        let body = format!(
            r"{field}:\$R\[\d+\]=\{{[^}}]*{}",
            if pct_first {
                r"usagePercent:([\d.]+)[^}]*resetInSec:([\d.]+)"
            } else {
                r"resetInSec:([\d.]+)[^}]*usagePercent:([\d.]+)"
            }
        );
        let re = regex::Regex::new(&body).ok()?;
        if let Some(cap) = re.captures(html) {
            let (pct, reset) = if pct_first {
                (cap[1].parse::<f64>().ok()?, cap[2].parse::<f64>().ok()?)
            } else {
                (cap[2].parse::<f64>().ok()?, cap[1].parse::<f64>().ok()?)
            };
            return Some(make_window(pct, reset));
        }
    }

    // data-slot 格式：按 usage-item 分割，label 里含窗口名
    let want = field.replace("Usage", "");
    for item in html.split("data-slot=\"usage-item\"").skip(1) {
        let lm = regex::Regex::new(r#"data-slot="usage-label">([^<]+)<"#).ok()?;
        let Some(lcap) = lm.captures(item) else { continue };
        let label = lcap[1].trim().to_lowercase();
        let key = ["rolling", "weekly", "monthly"].iter().find(|k| label.contains(**k))?;
        if *key != want.as_str() {
            continue;
        }
        let um = regex::Regex::new(r#"data-slot="usage-value">[^0-9]*([\d.]+)"#).ok()?;
        let Some(ucap) = um.captures(item) else { continue };
        let rm = regex::Regex::new(r#"data-slot="(reset-time|reset-now)">([\s\S]*?)</span>"#).ok()?;
        let Some(rcap) = rm.captures(item) else { continue };
        let reset_sec = if &rcap[1] == "reset-now" {
            0.0
        } else {
            parse_human_time(&rcap[2])?
        };
        return Some(make_window(ucap[1].parse::<f64>().ok()?, reset_sec));
    }
    None
}

// 抓取 dashboard 页面（认证用 auth cookie）
async fn fetch_dashboard(workspace_id: &str, auth_cookie: &str) -> Option<String> {
    crate::data::net::get(
        &format!("https://opencode.ai/workspace/{workspace_id}/go"),
        &[
            ("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Gecko/20100101 Firefox/148.0"),
            ("Accept", "text/html"),
            ("Cookie", &format!("auth={auth_cookie}")),
        ],
    )
    .await
}

async fn ocgo_fetch(workspace_id: &str, auth_cookie: &str) -> Option<Value> {
    let html = fetch_dashboard(workspace_id, auth_cookie).await?;
    let mut out = serde_json::Map::new();
    for field in ["rollingUsage", "weeklyUsage", "monthlyUsage"] {
        if let Some(win) = parse_window(&html, field) {
            out.insert(field.replace("Usage", ""), win);
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(Value::Object(out))
}

fn empty_payload(enabled: bool, workspace_id: &str, error: &str) -> Value {
    json!({
        "enabled": enabled,
        "workspaceId": workspace_id,
        "buckets": [],
        "error": error,
        "updatedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    })
}

pub async fn refresh_once() {
    let workspace_id = std::env::var("OPENCODE_GO_WORKSPACE_ID").unwrap_or_default().trim().to_string();
    let auth_cookie = std::env::var("OPENCODE_GO_AUTH_COOKIE").unwrap_or_default().trim().to_string();
    if workspace_id.is_empty() || auth_cookie.is_empty() {
        go_cache().lock().unwrap().set(empty_payload(false, "", ""));
        return;
    }

    // 网络调用期间不持锁（MutexGuard 跨 await 会让 future 非 Send）
    let parsed = ocgo_fetch(&workspace_id, &auth_cookie).await;
    let mut guard = go_cache().lock().unwrap();
    let Some(parsed) = parsed else {
        // 失败：复用旧缓存（若有效）；否则写 error 结构
        let old = guard.get();
        let has_valid = old.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false)
            && old.get("buckets").and_then(|v| v.as_array()).map(|a| !a.is_empty()).unwrap_or(false);
        if !has_valid {
            guard.set(empty_payload(true, &workspace_id, "抓取失败（cookie 可能过期）"));
        }
        return;
    };

    let label_map: [(&str, &str); 3] = [("rolling", "5小时"), ("weekly", "每周"), ("monthly", "每月")];
    let buckets: Vec<Value> = parsed
        .as_object()
        .map(|obj| {
            obj.iter()
                .map(|(key, win)| {
                    json!({
                        "key": key,
                        "label": label_map.iter().find(|(k, _)| *k == key).map(|(_, l)| *l).unwrap_or(key),
                        "usedPct": win.get("usedPct").cloned().unwrap_or(json!(0)),
                        "remainingPct": win.get("remainingPct").cloned().unwrap_or(json!(0)),
                        "resetMs": win.get("resetMs").cloned().unwrap_or(json!(0)),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    guard.set(json!({
        "enabled": true,
        "workspaceId": workspace_id,
        "buckets": buckets,
        "error": "",
        "updatedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    }));
}
