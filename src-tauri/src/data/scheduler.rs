// 后台刷新调度器 — 对齐 Electron src/data/scheduler.js。
// 网络调用全部在后台循环（首次立即执行），status() 路径零网络（只读缓存）。
// 注意：setup 阶段不在 tokio runtime 上下文，spawn 必须走 tauri::async_runtime。

use std::time::Duration;

use tauri::AppHandle;

use crate::data::{deepseek, opencode, scnet, volc, wuhen};

pub fn start_all(_app: AppHandle) {
    let ttl = Duration::from_secs(volc::CACHE_TTL);
    tauri::async_runtime::spawn(async move {
        loop {
            volc::refresh_once().await;
            tokio::time::sleep(ttl).await;
        }
    });
    let ttl = Duration::from_secs(deepseek::CACHE_TTL);
    tauri::async_runtime::spawn(async move {
        loop {
            deepseek::refresh_once().await;
            tokio::time::sleep(ttl).await;
        }
    });
    let ttl = Duration::from_secs(opencode::CACHE_TTL);
    tauri::async_runtime::spawn(async move {
        loop {
            opencode::refresh_once().await;
            tokio::time::sleep(ttl).await;
        }
    });
    let ttl = Duration::from_secs(wuhen::CACHE_TTL);
    tauri::async_runtime::spawn(async move {
        loop {
            wuhen::refresh_once().await;
            tokio::time::sleep(ttl).await;
        }
    });
    let ttl = Duration::from_secs(scnet::CACHE_TTL);
    tauri::async_runtime::spawn(async move {
        loop {
            scnet::refresh_once().await;
            tokio::time::sleep(ttl).await;
        }
    });
    println!("[scheduler] refreshers started (ttl 15s)");
}
