// 火山方舟 OpenAPI 用量查询 — 对齐 Electron src/data/volc.js。
// SigV4 签名 POST + CodingPlan/AgentPlan 套餐解析 + 15s 缓存后台刷新。
// status() 路径零网络（只读缓存），网络调用全在 scheduler 后台。

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use hmac::{Hmac, Mac};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::data::sqlite::{home_dir, MODEL_USAGE_DB};
use crate::data::Cache;

const HOST: &str = "open.volcengineapi.com";
const SERVICE: &str = "ark";
const REGION: &str = "cn-beijing";
const VERSION: &str = "2024-01-01";
pub const CACHE_TTL: u64 = 15;
const MS_PER_DAY: i64 = 86_400_000;

// ---- 凭证：环境变量 > 同目录 .volc.env > 家目录 .volc.env ----

fn load_env_file(p: &std::path::Path) {
    if let Ok(content) = std::fs::read_to_string(p) {
        for line in content.lines() {
            let s = line.trim();
            if s.is_empty() || s.starts_with('#') {
                continue;
            }
            let Some(eq) = s.find('=') else { continue };
            let k = s[..eq].trim();
            let mut v = s[eq + 1..].trim().to_string();
            if (v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')) {
                v = v[1..v.len() - 1].to_string();
            }
            if !k.is_empty() && std::env::var_os(k).is_none() {
                std::env::set_var(k, &v);
            }
        }
    }
}

struct Creds {
    ak_id: String,
    ak_secret: String,
    plan_type: String,
    plan_tier: String,
    plan_start_ms: Option<i64>,
}

fn creds() -> &'static Creds {
    static CREDS: OnceLock<Creds> = OnceLock::new();
    CREDS.get_or_init(|| {
        // 凭证查找目录：exe 同目录（打包后）> 当前目录（dev 时是项目根，放着 .volc.env）> 家目录
        let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let cwd = std::env::current_dir().ok();
        let home = home_dir();
        for dir in [exe_dir, cwd].into_iter().flatten().chain(std::iter::once(home.clone())) {
            let p = dir.join(".volc.env");
            if p.exists() {
                load_env_file(&p);
                break;
            }
        }
        let plan_start = std::env::var("VOLC_PLAN_START")
            .ok()
            .map(|s| s.trim().replace(' ', "T"))
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|t| t.timestamp_millis());
        Creds {
            ak_id: std::env::var("VOLC_AK_ID").unwrap_or_default(),
            ak_secret: std::env::var("VOLC_AK_SECRET").unwrap_or_default(),
            plan_type: if std::env::var("VOLC_PLAN_TYPE")
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("agent")
            {
                "agent".into()
            } else {
                "coding".into()
            },
            plan_tier: std::env::var("VOLC_PLAN_TIER").unwrap_or_default(),
            plan_start_ms: plan_start,
        }
    })
}

// ---- SigV4 签名 ----

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut mac = <Hmac<Sha256>>::new_from_slice(key).expect("hmac key");
    mac.update(msg);
    mac.finalize().into_bytes().to_vec()
}

// POST https://HOST/?Action=..&Version=..，返回 Result 或 None（任何异常吞掉）
async fn volc_call(action: &str, body: &Value) -> Option<Value> {
    let c = creds();
    if c.ak_id.is_empty() || c.ak_secret.is_empty() {
        return None;
    }
    let amz_date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = &amz_date[..8];

    let body_bytes = serde_json::to_vec(body).ok()?;
    let payload_hash = sha256_hex(&body_bytes);

    let canonical_query = format!("Action={action}&Version={VERSION}");
    let signed_headers = "host;x-content-sha256;x-date";
    let canonical_headers = format!("host:{HOST}\nx-content-sha256:{payload_hash}\nx-date:{amz_date}\n");
    let canonical_request =
        format!("POST\n/\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}");
    let credential_scope = format!("{date_stamp}/{REGION}/{SERVICE}/request");
    let string_to_sign = format!(
        "HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let k_date = hmac_sha256(c.ak_secret.as_bytes(), date_stamp.as_bytes());
    let k_region = hmac_sha256(&k_date, REGION.as_bytes());
    let k_service = hmac_sha256(&k_region, SERVICE.as_bytes());
    let signing_key = hmac_sha256(&k_service, b"request");
    let signature = hmac_sha256(&signing_key, string_to_sign.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let authorization = format!(
        "HMAC-SHA256 Credential={}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}",
        c.ak_id
    );

    let url = format!("https://{HOST}/?{canonical_query}");
    let resp = crate::data::net::client()
        .post(&url)
        .body(body_bytes)
        .header("Content-Type", "application/json; charset=utf-8")
        .header("X-Date", &amz_date)
        .header("X-Content-Sha256", &payload_hash)
        .header("Authorization", &authorization)
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<Value>().await.ok()?.get("Result").cloned()
}

// ---- config.json 辅助（plan provider / deepseek / opencode 识别） ----

fn load_config() -> Value {
    std::fs::read_to_string(home_dir().join(".zcode/v2/config.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(json!({}))
}

pub fn provider_ids_by_baseurl(keyword: &str) -> Vec<String> {
    let cfg = load_config();
    let mut out = Vec::new();
    if let Some(providers) = cfg.get("provider").and_then(|p| p.as_object()) {
        for (pid, info) in providers {
            let url = info
                .get("options")
                .and_then(|o| o.get("baseURL"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if url.contains(keyword) {
                out.push(pid.clone());
            }
        }
    }
    out
}

pub fn provider_api_key(keyword: &str) -> String {
    let cfg = load_config();
    if let Some(providers) = cfg.get("provider").and_then(|p| p.as_object()) {
        for info in providers.values() {
            let url = info
                .get("options")
                .and_then(|o| o.get("baseURL"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if url.contains(keyword) {
                if let Some(k) = info
                    .get("options")
                    .and_then(|o| o.get("apiKey"))
                    .and_then(|v| v.as_str())
                {
                    if !k.is_empty() {
                        return k.to_string();
                    }
                }
            }
        }
    }
    String::new()
}

fn provider_plan_ids() -> Vec<String> {
    let cfg = load_config();
    let mut out = Vec::new();
    if let Some(providers) = cfg.get("provider").and_then(|p| p.as_object()) {
        for (pid, info) in providers {
            let url = info
                .get("options")
                .and_then(|o| o.get("baseURL"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let name = info.get("name").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
            if url.contains("/plan") || name.contains("plan") {
                out.push(pid.clone());
            }
        }
    }
    out
}

// 按套餐窗口起点聚合本地 model_usage 分模型 token 明细（只统计 plan provider）
fn plan_window_models(start_ms: i64) -> Vec<Value> {
    let pids = provider_plan_ids();
    if pids.is_empty() || start_ms == 0 {
        return Vec::new();
    }
    let p = home_dir().join(MODEL_USAGE_DB);
    if !p.exists() {
        return Vec::new();
    }
    let db = match Connection::open_with_flags(&p, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(db) => db,
        Err(_) => return Vec::new(),
    };
    let ph = vec!["?"; pids.len()].join(",");
    let sql = format!(
        "SELECT LOWER(model_id) AS mid,
                COUNT(*) AS reqs,
                COALESCE(SUM(computed_total_tokens), 0) AS tt
         FROM model_usage
         WHERE status = 'completed' AND completed_at >= ? AND completed_at <= ?
           AND provider_id IN ({ph})
         GROUP BY mid ORDER BY tt DESC"
    );
    let now_ms = chrono::Local::now().timestamp_millis();
    let mut rows: Vec<(String, i64, i64)> = Vec::new();
    if let Ok(mut stmt) = db.prepare(&sql) {
        let params: Vec<&dyn rusqlite::types::ToSql> = {
            let mut v: Vec<&dyn rusqlite::types::ToSql> = Vec::new();
            v.push(&start_ms);
            v.push(&now_ms);
            for pid in &pids {
                v.push(pid);
            }
            v
        };
        if let Ok(iter) = stmt.query_map(params.as_slice(), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
        }) {
            rows = iter.filter_map(|r| r.ok()).collect();
        }
    }
    let _ = db.close();
    let total: i64 = rows.iter().map(|r| r.2).sum();
    rows.into_iter()
        .map(|(mid, reqs, tt)| {
            json!({
                "model": mid,
                "tokens": tt,
                "requests": reqs,
                "pct": if total > 0 { (tt as f64 / total as f64 * 1000.0).round() / 10.0 } else { 0.0 },
            })
        })
        .collect()
}

// ---- 套餐解析 ----

fn parse_coding_plan(cp: &Value) -> (String, Vec<Value>) {
    let label_map: [(&str, &str); 3] = [("session", "会话"), ("weekly", "每周"), ("monthly", "每月")];
    let mut buckets = Vec::new();
    if let Some(quota_usage) = cp.get("QuotaUsage").and_then(|q| q.as_array()) {
        for item in quota_usage {
            let level = item.get("Level").and_then(|v| v.as_str()).unwrap_or("");
            let Some((_, label)) = label_map.iter().find(|(k, _)| *k == level) else { continue };
            let used_pct = (item.get("Percent").and_then(|v| v.as_f64()).unwrap_or(0.0) * 10.0).round() / 10.0;
            let remaining_pct = (0.0f64.max(100.0 - used_pct) * 10.0).round() / 10.0;
            let reset_ts = item.get("ResetTimestamp").and_then(|v| v.as_i64()).unwrap_or(0);
            let reset_ms = if reset_ts != 0 { reset_ts * 1000 } else { 0 };

            // 倒推本地 token 聚合窗口起点（毫秒）
            let mut window_start_ms: i64 = 0;
            let mut needs_config = false;
            if reset_ms != 0 {
                let reset_dt = chrono::DateTime::from_timestamp_millis(reset_ms);
                match level {
                    "session" => {
                        window_start_ms = reset_ms - 5 * 3600 * 1000;
                    }
                    "weekly" => {
                        if let Some(ps) = creds().plan_start_ms {
                            let mut start = reset_ms - 7 * MS_PER_DAY;
                            if start < ps {
                                start = ps;
                            }
                            window_start_ms = start;
                        } else {
                            needs_config = true;
                        }
                    }
                    "monthly" => {
                        if let (Some(dt), Some(ps)) = (reset_dt, creds().plan_start_ms) {
                            use chrono::Datelike;
                            // 月份减 1（年进位），归零到当天 00:00，max(开通时刻)
                            let mut m = dt.month() as i32 - 1;
                            let mut y = dt.year();
                            if m < 1 {
                                m = 12;
                                y -= 1;
                            }
                            let start_dt = chrono::NaiveDate::from_ymd_opt(y, m as u32, dt.day())
                                .and_then(|d| d.and_hms_opt(0, 0, 0))
                                .map(|d| d.and_utc().timestamp_millis());
                            if let Some(mut start) = start_dt {
                                if start < ps {
                                    start = ps;
                                }
                                window_start_ms = start;
                            }
                        } else {
                            needs_config = true;
                        }
                    }
                    _ => {}
                }
            }

            buckets.push(json!({
                "key": level,
                "label": label,
                "quota": 100,
                "used": used_pct,
                "remaining": remaining_pct,
                "remainingPct": remaining_pct,
                "usedPct": used_pct,
                "resetMs": reset_ms,
                "windowStart": window_start_ms,
                "needsConfig": needs_config,
                "models": [],
            }));
        }
    }
    let raw_status = cp.get("Status").and_then(|v| v.as_str()).unwrap_or("").to_string();
    (raw_status, buckets)
}

fn parse_agent_plan(afp: &Value) -> Vec<Value> {
    let label_map: [(&str, &str); 3] = [("AFPFiveHour", "5小时"), ("AFPWeekly", "每周"), ("AFPMonthly", "每月")];
    let mut buckets = Vec::new();
    for (key, label) in label_map {
        let Some(b) = afp.get(key) else { continue };
        let quota = b.get("Quota").and_then(|v| v.as_i64()).unwrap_or(0);
        let used = b.get("Used").and_then(|v| v.as_i64()).unwrap_or(0);
        let used_pct = if quota > 0 { ((used as f64 / quota as f64) * 1000.0).round() / 10.0 } else { 0.0 };
        buckets.push(json!({
            "key": key,
            "label": label,
            "quota": quota,
            "used": used,
            "remaining": 0.max(quota - used),
            "remainingPct": if quota > 0 { ((quota - used) as f64 / quota as f64 * 1000.0).round() / 10.0 } else { 0.0 },
            "usedPct": used_pct,
            "resetMs": b.get("ResetTime").and_then(|v| v.as_i64()).unwrap_or(0),
        }));
    }
    buckets
}

// ---- 缓存 + 后台刷新 ----

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::new()))
}

pub fn get_plan_usage() -> Value {
    cache().lock().map(|c| c.get()).unwrap_or(Value::Null)
}

fn empty_payload(enabled: bool, error: &str) -> Value {
    let c = creds();
    json!({
        "enabled": enabled,
        "planType": c.plan_type,
        "tier": c.plan_tier,
        "rawStatus": "",
        "buckets": [],
        "error": error,
        "updatedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    })
}

pub async fn refresh_once() {
    let c = creds();
    if c.ak_id.is_empty() || c.ak_secret.is_empty() {
        cache().lock().unwrap().set(empty_payload(false, ""));
        return;
    }

    let action = if c.plan_type == "coding" { "GetCodingPlanUsage" } else { "GetAFPUsage" };
    // 网络调用期间不持锁（MutexGuard 跨 await 会让 future 非 Send）
    let resp = volc_call(action, &json!({})).await;
    let mut guard = cache().lock().unwrap();
    let Some(resp) = resp else {
        // 失败：复用旧缓存（若有效）；否则写 error 结构
        let old = guard.get();
        let has_valid = old.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false)
            && old.get("buckets").and_then(|v| v.as_array()).map(|a| !a.is_empty()).unwrap_or(false);
        if !has_valid {
            guard.set(empty_payload(true, "调用失败"));
        }
        return;
    };

    let (raw_status, buckets) = if c.plan_type == "coding" {
        let (raw, mut buckets) = parse_coding_plan(&resp);
        for b in buckets.iter_mut() {
            let needs = b.get("needsConfig").and_then(|v| v.as_bool()).unwrap_or(false);
            let ws = b.get("windowStart").and_then(|v| v.as_i64()).unwrap_or(0);
            if !needs && ws != 0 {
                b["models"] = json!(plan_window_models(ws));
            }
        }
        (raw, buckets)
    } else {
        (String::new(), parse_agent_plan(&resp))
    };

    let error = if c.plan_type == "coding" && !raw_status.is_empty() && raw_status != "Running" {
        format!("套餐未生效（{raw_status}）")
    } else {
        String::new()
    };
    guard.set(json!({
        "enabled": true,
        "planType": c.plan_type,
        "tier": c.plan_tier,
        "rawStatus": raw_status,
        "buckets": buckets,
        "error": error,
        "updatedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    }));
}
