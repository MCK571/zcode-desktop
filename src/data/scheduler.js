'use strict';

// 后台刷新调度器 — 移植自 data.py 的三个守护线程（火山 / DeepSeek /
// opencode Go）。统一用 setInterval 循环，首次立即执行；网络调用全部
// 在后台，status() 路径零网络（只读缓存）。

const volc = require('./volc');
const deepseek = require('./deepseek');
const opencode = require('./opencode');

function startRefresher(name, fn, ttlMs) {
  const loop = async () => {
    try {
      await fn();
    } catch {
      // 后台刷新绝不能因异常退出，否则缓存永远不再刷新
    }
    setTimeout(loop, ttlMs);
  };
  loop();
  console.log(`[scheduler] ${name} refresher started (ttl ${ttlMs}ms)`);
}

function startAll() {
  startRefresher('volc', volc.refreshOnce, volc.CACHE_TTL * 1000);
  startRefresher('deepseek', deepseek.refreshOnce, deepseek.CACHE_TTL * 1000);
  startRefresher('opencode-go', opencode.refreshOnce, opencode.CACHE_TTL * 1000);
}

module.exports = { startAll };
