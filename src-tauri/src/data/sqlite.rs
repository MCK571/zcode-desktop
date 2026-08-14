// SQLite / 日志读取层 — 对齐 Electron src/data/sqlite.js。
// 权威 token 来源：model_usage 表（跨 session 持久、不剪枝）。
// 全部只读打开；任何失败返回空结构，绝不 panic。

use std::path::PathBuf;

use rusqlite::{types::ToSql, Connection, OpenFlags};
use serde_json::{json, Value};

pub const DB_PATH: &str = ".zcode/v2/tasks-index.sqlite";
pub const MODEL_USAGE_DB: &str = ".zcode/cli/db/db.sqlite";
pub const LOG_DIR: &str = ".zcode/cli/log";

pub const LIVE_LOG_TAIL: usize = 400;
// 会话状态判断只关心最近活跃会话，读文件尾部足够（全文件读在日志几十 MB
// 后会阻塞轮询，尾部 chunk 读是 Electron 版实测根因修复）
pub const STATUS_LOG_TAIL: usize = 2000;
const ACTIVE_TURN_FRESH_MS: i64 = 30 * 60 * 1000; // 30 分钟
const MS_PER_DAY: i64 = 86_400_000;

pub fn home_dir() -> PathBuf {
    std::env::var("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("C:/Users/default"))
}

fn open_ro(db_path: &str) -> Option<Connection> {
    let p = home_dir().join(db_path);
    if !p.exists() {
        return None;
    }
    Connection::open_with_flags(&p, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
}

// 时间戳 → "HH:MM:SS"（本地时区，对齐 JS toTimeString().slice(0,8)）
pub fn fmt_ts(ms: i64) -> String {
    if ms == 0 {
        return String::new();
    }
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
        .unwrap_or_default()
}

// ISO 时间串 → epoch ms（对齐 JS new Date(ts.replace('Z','+00:00')).getTime()）
fn parse_iso_ms(ts: &str) -> Option<i64> {
    let norm = ts.replace('Z', "+00:00");
    chrono::DateTime::parse_from_rfc3339(&norm).ok().map(|t| t.timestamp_millis())
}

// 本地时区当天 00:00 的 epoch ms（对齐 JS new Date(y,m,d).getTime()）
fn today_start_ms() -> i64 {
    chrono::Local::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .map(|dt| dt.and_utc().timestamp_millis())
        .unwrap_or_else(|| chrono::Local::now().timestamp_millis())
}

// ---- 日志读取（尾部 chunk） ----

fn newest_log_file() -> Option<PathBuf> {
    let dir = home_dir().join(LOG_DIR);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| {
                    let n = n.to_string_lossy();
                    n.starts_with("zcode-") && n.ends_with(".jsonl")
                })
                .unwrap_or(false)
        })
        .collect();
    if files.is_empty() {
        return None;
    }
    files.sort();
    files.pop()
}

fn read_log_lines(tail: usize) -> Vec<String> {
    let fp = match newest_log_file() {
        Some(f) => f,
        None => return Vec::new(),
    };
    use std::io::{Read, Seek, SeekFrom};
    let mut f = match std::fs::File::open(&fp) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    if size == 0 {
        return Vec::new();
    }
    // 只读尾部 chunk：seek 到 (size - chunk) 再读，避免整文件解析。
    // chunk 起点可能切断一行（仅当文件大于 chunk 时），丢弃不完整首行。
    let max_tail_bytes: u64 = 512 * 1024;
    let len = max_tail_bytes.min(size);
    if f.seek(SeekFrom::Start(size - len)).is_err() {
        return Vec::new();
    }
    let mut buf = String::new();
    if f.take(len).read_to_string(&mut buf).is_err() {
        return Vec::new();
    }
    let mut lines: Vec<String> = buf.split('\n').map(|s| s.to_string()).collect();
    if size > len {
        lines.remove(0);
    }
    if lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    let start = lines.len().saturating_sub(tail);
    lines[start..].to_vec()
}

fn parse_lines(lines: &[String]) -> Vec<Value> {
    lines
        .iter()
        .filter_map(|l| {
            if l.is_empty() {
                return None;
            }
            serde_json::from_str::<Value>(l).ok()
        })
        .collect()
}

// ---- 任务列表 ----

pub fn read_tasks() -> Vec<Value> {
    let db = match open_ro(DB_PATH) {
        Some(db) => db,
        None => return Vec::new(),
    };
    let mut rows: Vec<(String, Option<String>, String, Option<String>, Option<String>, Option<String>, i64, i64, Option<String>)> = Vec::new();
    if let Ok(mut stmt) = db.prepare(
        "SELECT task_id, title, task_status, provider, model, mode,
                created_at, updated_at, meta_json
         FROM tasks WHERE deleted = 0 ORDER BY updated_at DESC LIMIT 40",
    ) {
        if let Ok(iter) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, Option<String>>(8)?,
            ))
        }) {
            rows = iter.filter_map(|r| r.ok()).collect();
        }
    }
    let _ = db.close();

    // ZCode 的 task_status/mode/thoughtLevel 更新有滞后，用 log 实时信号覆盖
    let active = active_session_ids();
    let modes = session_modes();
    let levels = session_thought_levels();

    rows.into_iter()
        .map(|(id, title, status, provider, model, mode, created_at, updated_at, meta_json)| {
            let meta = serde_json::from_str::<Value>(&meta_json.unwrap_or_default()).unwrap_or(json!({}));
            let status = if active.contains(&id) {
                "running".to_string()
            } else {
                status
            };
            let thought = levels
                .get(&id)
                .cloned()
                .or_else(|| meta.get("thoughtLevel").and_then(|v| v.as_str()).map(String::from))
                .unwrap_or_default();
            json!({
                "taskId": id,
                "title": title.unwrap_or_else(|| "(未命名任务)".into()),
                "status": status,
                "provider": provider.unwrap_or_default(),
                "model": model.unwrap_or_default(),
                "mode": modes.get(&id).cloned().unwrap_or(mode.unwrap_or_default()),
                "createdAt": created_at,
                "updatedAt": updated_at,
                "workspacePath": meta.get("workspacePath").and_then(|v| v.as_str()).unwrap_or(""),
                "thoughtLevel": thought,
            })
        })
        .collect()
}

// 每个任务挂 token 用量（tasks.task_id == model_usage.session_id）
pub fn read_task_tokens(task_ids: &[String]) -> Value {
    let mut out = json!({});
    if task_ids.is_empty() {
        return out;
    }
    let db = match open_ro(MODEL_USAGE_DB) {
        Some(db) => db,
        None => return out,
    };
    let ph = vec!["?"; task_ids.len()].join(",");
    let sql = format!(
        "SELECT session_id,
                COALESCE(SUM(computed_total_tokens),0) AS total,
                COALESCE(SUM(input_tokens),0)        AS input,
                COALESCE(SUM(output_tokens),0)       AS output,
                COUNT(*)                              AS requests
         FROM model_usage
         WHERE status = 'completed' AND session_id IN ({ph})
         GROUP BY session_id"
    );
    if let Ok(mut stmt) = db.prepare(&sql) {
        let params: Vec<&dyn ToSql> = task_ids.iter().map(|s| s as &dyn ToSql).collect();
        if let Ok(iter) = stmt.query_map(params.as_slice(), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        }) {
            for row in iter.flatten() {
                out[row.0.clone()] = json!({
                    "total": row.1, "input": row.2, "output": row.3, "requests": row.4,
                });
            }
        }
    }
    let _ = db.close();
    out
}

// ---- 用量聚合（权威 model_usage 表） ----

pub fn empty_usage() -> Value {
    json!({
        "inputTokens": 0, "outputTokens": 0, "totalTokens": 0,
        "cacheReadTokens": 0, "reasoningTokens": 0, "requests": 0,
    })
}

#[derive(Clone)]
struct SumRow {
    ti: i64,
    toks: i64,
    tt: i64,
    cr: i64,
    rt: i64,
    reqs: i64,
}

fn sum_stmt(db: &Connection, where_clause: &str, params: &[&dyn ToSql]) -> Option<SumRow> {
    let sql = format!(
        "SELECT COALESCE(SUM(input_tokens),0) AS ti,
                COALESCE(SUM(output_tokens),0) AS toks,
                COALESCE(SUM(computed_total_tokens),0) AS tt,
                COALESCE(SUM(cache_read_input_tokens),0) AS cr,
                COALESCE(SUM(reasoning_tokens),0) AS rt,
                COUNT(*) AS reqs
         FROM model_usage WHERE {where_clause}"
    );
    let mut stmt = db.prepare(&sql).ok()?;
    stmt.query_row(params, |r| {
        Ok(SumRow {
            ti: r.get(0)?,
            toks: r.get(1)?,
            tt: r.get(2)?,
            cr: r.get(3)?,
            rt: r.get(4)?,
            reqs: r.get(5)?,
        })
    })
    .ok()
}

fn pack_usage(row: &SumRow) -> Value {
    json!({
        "inputTokens": row.ti,
        "outputTokens": row.toks,
        "totalTokens": row.tt,
        "cacheReadTokens": row.cr,
        "reasoningTokens": row.rt,
        "requests": row.reqs,
    })
}

pub fn read_usage() -> Value {
    let db = match open_ro(MODEL_USAGE_DB) {
        Some(db) => db,
        None => return empty_usage(),
    };
    let today_ms = today_start_ms();
    let week_ago = today_ms - 7 * MS_PER_DAY;
    let ok = "status = 'completed'";

    let today_row = sum_stmt(&db, &format!("{ok} AND completed_at >= ?"), &[&today_ms]);
    let week_row = sum_stmt(&db, &format!("{ok} AND completed_at >= ?"), &[&week_ago]);
    let all_row = sum_stmt(&db, ok, &[]);

    let mut models: Vec<Value> = Vec::new();
    if let Ok(mut stmt) = db.prepare(
        "SELECT LOWER(model_id) AS mid,
                COUNT(*) AS reqs,
                COALESCE(SUM(input_tokens),0) AS ti,
                COALESCE(SUM(output_tokens),0) AS toks,
                COALESCE(SUM(computed_total_tokens),0) AS tt
         FROM model_usage
         WHERE status = 'completed' AND completed_at >= ?
         GROUP BY mid ORDER BY tt DESC",
    ) {
        if let Ok(iter) = stmt.query_map([today_ms], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?))
        }) {
            models = iter
                .filter_map(|r| r.ok())
                .map(|(mid, reqs, ti, toks, tt)| {
                    json!({ "model": mid, "requests": reqs, "inputTokens": ti, "outputTokens": toks, "totalTokens": tt })
                })
                .collect();
        }
    }

    let mut last_ts = String::new();
    if let Ok(mut stmt) = db.prepare("SELECT MAX(completed_at) AS mx FROM model_usage WHERE completed_at IS NOT NULL") {
        if let Ok(Some(mx)) = stmt.query_row([], |r| r.get::<_, Option<i64>>(0)) {
            if let Some(t) = chrono::DateTime::from_timestamp_millis(mx) {
                last_ts = t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            }
        }
    }

    let _ = db.close();

    let (today_row, week_row, all_row) = match (today_row, week_row, all_row) {
        (Some(t), Some(w), Some(a)) => (t, w, a),
        _ => return empty_usage(),
    };

    json!({
        "label": "模型用量",
        "today": pack_usage(&today_row),
        "week": pack_usage(&week_row),
        "total": pack_usage(&all_row),
        "grandTotal": {
            "inputTokens": all_row.ti,
            "outputTokens": all_row.toks,
            "totalTokens": all_row.tt,
        },
        "models": models,
        "lastActivity": last_ts,
        "updatedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    })
}

// ---- 指定 provider 集合的用量（deepseek/opencode，今日/7日/30日三窗口） ----

pub fn read_provider_usage(pids: &[String]) -> Value {
    let zero = || json!({ "totalTokens": 0, "inputTokens": 0, "outputTokens": 0, "requests": 0 });
    if pids.is_empty() {
        return json!({ "enabled": false, "today": zero(), "week": zero(), "month": zero() });
    }
    let db = match open_ro(MODEL_USAGE_DB) {
        Some(db) => db,
        None => return json!({ "enabled": true, "today": zero(), "week": zero(), "month": zero() }),
    };
    let today_ms = today_start_ms();
    let week_ago = today_ms - 7 * MS_PER_DAY;
    let month_ago = today_ms - 30 * MS_PER_DAY;
    let ph = vec!["?"; pids.len()].join(",");
    let pids_ref: Vec<&dyn ToSql> = pids.iter().map(|s| s as &dyn ToSql).collect();

    let sum = |db: &Connection, where_clause: &str, params: &[&dyn ToSql]| -> Value {
        let sql = format!(
            "SELECT COALESCE(SUM(input_tokens),0) AS ti,
                    COALESCE(SUM(output_tokens),0) AS toks,
                    COALESCE(SUM(computed_total_tokens),0) AS tt,
                    COUNT(*) AS reqs
             FROM model_usage WHERE {where_clause}"
        );
        db.prepare(&sql)
            .and_then(|mut stmt| {
                stmt.query_row(params, |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?))
                })
            })
            .map(|(ti, toks, tt, reqs)| {
                json!({ "totalTokens": tt, "inputTokens": ti, "outputTokens": toks, "requests": reqs })
            })
            .unwrap_or_else(|_| zero())
    };

    let ok = "status = 'completed'";
    let mut params: Vec<&dyn ToSql> = Vec::new();
    params.push(&today_ms);
    params.extend(pids_ref.iter().copied());
    let today = sum(&db, &format!("{ok} AND completed_at >= ? AND provider_id IN ({ph})"), &params);
    params[0] = &week_ago;
    let week = sum(&db, &format!("{ok} AND completed_at >= ? AND provider_id IN ({ph})"), &params);
    params[0] = &month_ago;
    let month = sum(&db, &format!("{ok} AND completed_at >= ? AND provider_id IN ({ph})"), &params);
    let _ = db.close();

    json!({ "enabled": true, "today": today, "week": week, "month": month })
}

// ---- 实时活动（tail 日志） ----

pub fn read_live_activity() -> Value {
    let lines = read_log_lines(LIVE_LOG_TAIL);
    let mut tool_calls: std::collections::HashMap<String, (String, String, Option<String>, Option<i64>, String)> =
        std::collections::HashMap::new();
    let mut events: Vec<Value> = Vec::new();
    let mut turn_active = false;

    for obj in parse_lines(&lines) {
        let ev = obj.get("event").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let ts = obj.get("timestamp").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let ctx = obj.get("context").cloned().unwrap_or(json!({}));
        let sess = obj.get("sessionId").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let tool_name = ctx.get("toolName").and_then(|v| v.as_str()).unwrap_or("").to_string();

        match ev.as_str() {
            "tool.call.started" => {
                let tcid = obj.get("toolCallId").and_then(|v| v.as_str()).unwrap_or("").to_string();
                tool_calls.insert(tcid, (tool_name.clone(), ts.clone(), None, None, sess.clone()));
                events.push(json!({ "type": "tool_start", "tool": tool_name, "ts": ts, "sessionId": sess }));
            }
            "tool.call.completed" => {
                let tcid = obj.get("toolCallId").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let dur = obj.get("durationMs").and_then(|v| v.as_i64());
                if let Some(entry) = tool_calls.get_mut(&tcid) {
                    entry.2 = Some(ts.clone());
                    entry.3 = dur;
                }
                events.push(json!({
                    "type": "tool_end", "tool": tool_name, "ts": ts,
                    "durationMs": dur.unwrap_or(0), "sessionId": sess
                }));
            }
            "model.request.completed" => {
                let model = ctx.get("modelId").and_then(|v| v.as_str()).unwrap_or("");
                let dur = obj.get("durationMs").and_then(|v| v.as_i64()).unwrap_or(0);
                events.push(json!({ "type": "model", "ts": ts, "sessionId": sess, "model": model, "durationMs": dur }));
            }
            "turn.started" => {
                turn_active = true;
                events.push(json!({ "type": "turn_start", "ts": ts, "sessionId": sess }));
            }
            "turn.completed" => {
                turn_active = false;
                events.push(json!({ "type": "turn_end", "ts": ts, "sessionId": sess }));
            }
            _ => {}
        }
    }

    let running_tool = tool_calls.values().find(|e| e.2.is_none()).map(|e| {
        json!({
            "tool": e.0, "startedAt": e.1, "completedAt": Value::Null,
            "durationMs": Value::Null, "sessionId": e.4
        })
    });

    let start = events.len().saturating_sub(8);
    let log_file = newest_log_file()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_default();
    json!({
        "currentTool": running_tool,
        "activity": events[start..].to_vec(),
        "turnActive": turn_active,
        "logFile": log_file,
    })
}

// ---- log 状态信号（任务实时状态覆盖） ----

fn active_session_ids() -> std::collections::HashSet<String> {
    let lines = read_log_lines(STATUS_LOG_TAIL);
    let mut last_turn_open: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    let mut last_turn_ts: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let now_ms = chrono::Local::now().timestamp_millis();

    for obj in parse_lines(&lines) {
        let ev = obj.get("event").and_then(|v| v.as_str()).unwrap_or("");
        if ev != "turn.started" && ev != "turn.completed" {
            continue;
        }
        let sess = obj.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");
        if !sess.starts_with("sess_") || sess.contains("subagent") {
            continue;
        }
        last_turn_open.insert(sess.to_string(), ev == "turn.started");
        last_turn_ts.insert(
            sess.to_string(),
            obj.get("timestamp").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        );
    }

    let mut active = std::collections::HashSet::new();
    for (sess, is_open) in last_turn_open {
        if !is_open {
            continue;
        }
        let ts = last_turn_ts.get(&sess).cloned().unwrap_or_default();
        match parse_iso_ms(&ts) {
            Some(ts_ms) => {
                if now_ms - ts_ms <= ACTIVE_TURN_FRESH_MS {
                    active.insert(sess);
                }
            }
            None => {
                active.insert(sess); // 无法解析 → 视为运行中
            }
        }
    }
    active
}

fn session_modes() -> std::collections::HashMap<String, String> {
    let lines = read_log_lines(STATUS_LOG_TAIL);
    let mut last_mode = std::collections::HashMap::new();
    for obj in parse_lines(&lines) {
        if obj.get("event").and_then(|v| v.as_str()) != Some("session.mode.updated") {
            continue;
        }
        let sess = obj.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");
        if !sess.starts_with("sess_") || sess.contains("subagent") {
            continue;
        }
        let mode = obj
            .get("context")
            .and_then(|c| c.get("mode"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !mode.is_empty() {
            last_mode.insert(sess.to_string(), mode.to_string());
        }
    }
    last_mode
}

fn session_thought_levels() -> std::collections::HashMap<String, String> {
    let lines = read_log_lines(STATUS_LOG_TAIL);
    let mut last_level = std::collections::HashMap::new();
    for obj in parse_lines(&lines) {
        if obj.get("event").and_then(|v| v.as_str()) != Some("session.reasoning_effort.updated") {
            continue;
        }
        let sess = obj.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");
        if !sess.starts_with("sess_") || sess.contains("subagent") {
            continue;
        }
        let level = obj
            .get("context")
            .and_then(|c| c.get("thoughtLevel"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !level.is_empty() {
            last_level.insert(sess.to_string(), level.to_string());
        }
    }
    last_level
}

// ---- 聚合结果缓存 ----
// SUM 全表扫描每个 ~20ms，status() 一轮 6+ 个聚合 ≈ 180ms 同步阻塞；
// completed 行只在任务完成时新增，15s 缓存对显示无感知（对齐 JS ttlMemo）。

pub struct TtlMemo {
    ts: std::time::Instant,
    val: Option<Value>,
    ttl: std::time::Duration,
    f: Box<dyn Fn() -> Value + Send + Sync>,
}

impl TtlMemo {
    pub fn new(ttl_ms: u64, f: impl Fn() -> Value + Send + Sync + 'static) -> Self {
        TtlMemo {
            ts: std::time::Instant::now() - std::time::Duration::from_millis(ttl_ms + 1),
            val: None,
            ttl: std::time::Duration::from_millis(ttl_ms),
            f: Box::new(f),
        }
    }

    pub fn get(&mut self) -> Value {
        if self.val.is_none() || self.ts.elapsed() >= self.ttl {
            self.val = Some((self.f)());
            self.ts = std::time::Instant::now();
        }
        self.val.clone().unwrap_or_default()
    }
}
