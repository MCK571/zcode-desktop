// 数据层入口（Tauri 版）— 对齐 Electron src/data/index.js。
// 模块：sqlite（任务/用量/活动）→ volc（SigV4 + 套餐）→ deepseek / wuhen /
// opencode → dsh（zstd 会话）→ scheduler 后台刷新。
// status() 路径零网络（网络调用全在后台），聚合结果 15s TTL 缓存。

pub mod deepseek;
pub mod dsh;
pub mod net;
pub mod opencode;
pub mod scnet;
pub mod scheduler;
pub mod sqlite;
pub mod volc;
pub mod wuhen;

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

// 通用后台缓存（对齐 JS 的 {ts,payload} 模式）
pub struct Cache {
    ts: Instant,
    payload: Option<Value>,
}

impl Cache {
    pub fn new() -> Self {
        Cache {
            ts: Instant::now() - Duration::from_secs(3600),
            payload: None,
        }
    }

    pub fn get(&self) -> Value {
        self.payload.clone().unwrap_or(Value::Null)
    }

    pub fn fresh(&self, ttl_ms: u64) -> bool {
        self.payload.is_some() && (self.ts.elapsed().as_millis() as u64) < ttl_ms
    }

    pub fn set(&mut self, v: Value) {
        self.ts = Instant::now();
        self.payload = Some(v);
    }
}

// ---- 聚合结果缓存（SUM 全表扫描 ~20ms/个，15s 缓存对齐 JS ttlMemo） ----

fn usage_cached() -> Value {
    static CACHE: OnceLock<Mutex<sqlite::TtlMemo>> = OnceLock::new();
    CACHE
        .get_or_init(|| Mutex::new(sqlite::TtlMemo::new(15_000, sqlite::read_usage)))
        .lock()
        .map(|mut m| m.get())
        .unwrap_or_else(|_| sqlite::empty_usage())
}

pub fn status() -> Value {
    let tasks = sqlite::read_tasks();
    let task_ids: Vec<String> = tasks
        .iter()
        .filter_map(|t| t.get("taskId").and_then(|v| v.as_str()).map(String::from))
        .collect();
    let task_tokens = sqlite::read_task_tokens(&task_ids);
    let zero = json!({ "total": 0, "input": 0, "output": 0, "requests": 0 });

    let tasks: Vec<Value> = tasks
        .into_iter()
        .map(|mut t| {
            let id = t.get("taskId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            t["tokens"] = task_tokens.get(&id).cloned().unwrap_or_else(|| zero.clone());
            let updated = t.get("updatedAt").and_then(|v| v.as_i64()).unwrap_or(0);
            let created = t.get("createdAt").and_then(|v| v.as_i64()).unwrap_or(0);
            t["updatedAtLabel"] = json!(sqlite::fmt_ts(updated));
            t["createdAtLabel"] = json!(sqlite::fmt_ts(created));
            t
        })
        .collect();

    let current = tasks
        .iter()
        .find(|t| t.get("status").and_then(|v| v.as_str()) == Some("running"))
        .cloned()
        .or_else(|| tasks.first().cloned());

    json!({
        "currentTask": current,
        "recentTasks": tasks.iter().take(8).cloned().collect::<Vec<_>>(),
        "usage": usage_cached(),
        "planUsage": volc::get_plan_usage(),
        "deepseekUsage": deepseek::get_deepseek_usage(),
        "opencodeUsage": opencode::get_opencode_usage(),
        "opencodeGo": opencode::get_opencode_go(),
        "deepseekBalance": deepseek::get_deepseek_balance(),
        "wuhenUsage": wuhen::get_wuhen_usage(),
        "scnetUsage": scnet::get_scnet_usage(),
        "dshUsage": dsh::get_dsh_usage(),
        "live": sqlite::read_live_activity(),
        "now": chrono::Local::now().format("%H:%M:%S").to_string(),
    })
}

// 冒烟对拍：cargo test -- --nocapture dump_status 打印 status() JSON，
// 与 Electron 版 node 脚本输出对比（见 docs/tauri-migration.md）
#[cfg(test)]
mod tests {
    #[test]
    fn dump_status() {
        let v = super::status();
        println!("{}", serde_json::to_string(&v).unwrap());
    }
}
