'use strict';

// ZCode 用量监控组件 — 数据层入口（Electron 版）。
// 结构对齐 pywebview 版 Api.status()。模块：
//   sqlite（任务/用量/实时活动）→ volc（SigV4 + 套餐）→ deepseek / opencode
//   → scheduler 后台刷新（网络调用全在后台，status() 零网络只读缓存）。

const {
  readTasks,
  readTaskTokens,
  readUsage,
  readLiveActivity,
  emptyUsage,
} = require('./sqlite');
const volc = require('./volc');
const deepseek = require('./deepseek');
const opencode = require('./opencode');
const wuhen = require('./wuhen');
const { startAll } = require('./scheduler');

// 时间戳 → "HH:MM:SS"
function fmtTs(ms) {
  if (!ms) return '';
  const d = new Date(ms);
  return Number.isFinite(d.getTime()) ? d.toTimeString().slice(0, 8) : '';
}

function createApi() {
  return {
    status() {
      const tasks = readTasks();
      // 每个任务挂 token 用量（tasks.task_id == model_usage.session_id）
      const taskTokens = readTaskTokens(tasks.map((t) => t.taskId));
      const zero = { total: 0, input: 0, output: 0, requests: 0 };
      for (const t of tasks) {
        t.tokens = taskTokens[t.taskId] || zero;
        t.updatedAtLabel = fmtTs(t.updatedAt);
        t.createdAtLabel = fmtTs(t.createdAt);
      }
      // 当前任务：最新更新的运行中任务；无运行中则最新任务
      const current = tasks.find((t) => t.status === 'running') || tasks[0] || null;
      if (current) {
        current.updatedAtLabel = fmtTs(current.updatedAt);
        current.createdAtLabel = fmtTs(current.createdAt);
      }

      return {
        currentTask: current,
        recentTasks: tasks.slice(0, 8),
        usage: readUsage(),
        planUsage: volc.getPlanUsage(),
        deepseekUsage: deepseek.getDeepseekUsage(),
        opencodeUsage: opencode.getOpencodeUsage(),
        opencodeGo: opencode.getOpencodeGo(),
        deepseekBalance: deepseek.getDeepseekBalance(),
        wuhenUsage: wuhen.getWuhenUsage(),
        live: readLiveActivity(),
        now: new Date().toTimeString().slice(0, 8),
      };
    },
  };
}

module.exports = { createApi, startAll };
