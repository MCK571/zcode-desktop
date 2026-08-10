'use strict';

// SQLite / 日志读取层 — 移植自 data.py（读 ~/.zcode 本地数据）。
// 权威 token 来源：model_usage 表（跨 session 持久、不剪枝）。

const { DatabaseSync } = require('node:sqlite');
const fs = require('fs');
const os = require('os');
const path = require('path');

const ZCODE_DIR = path.join(os.homedir(), '.zcode');
const DB_PATH = path.join(ZCODE_DIR, 'v2', 'tasks-index.sqlite');
const MODEL_USAGE_DB = path.join(ZCODE_DIR, 'cli', 'db', 'db.sqlite');
const LOG_DIR = path.join(ZCODE_DIR, 'cli', 'log');

const LIVE_LOG_TAIL = 400;
// 会话状态判断（activeSessionIds 等）只关心最近活跃会话，读文件尾部足够。
// 全文件 readFileSync 在日志涨到几十 MB 后，每 1.5s 轮询会同步阻塞主进程
// 数百 ms（拖动窗口时消息泵停转 → 周期性卡顿），尾部读是根因修复。
const STATUS_LOG_TAIL = 2000;
const ACTIVE_TURN_FRESH_MS = 30 * 60 * 1000; // 30 分钟
const MS_PER_DAY = 86_400_000;

function openRo(dbPath) {
  if (!fs.existsSync(dbPath)) return null;
  try {
    return new DatabaseSync(dbPath, { readOnly: true });
  } catch {
    return null;
  }
}

// ---- 日志读取 ----

function newestLogFile() {
  try {
    const files = fs.readdirSync(LOG_DIR)
      .filter((f) => f.startsWith('zcode-') && f.endsWith('.jsonl'))
      .sort();
    if (!files.length) return null;
    return path.join(LOG_DIR, files[files.length - 1]);
  } catch {
    return null;
  }
}

function readLogLines(tail = 0) {
  const fp = newestLogFile();
  if (!fp) return [];
  try {
    if (tail <= 0) return fs.readFileSync(fp, 'utf8').split('\n');
    // 只读文件尾部 chunk：seek 到 (size - chunk) 再读，避免整文件解析。
    // chunk 起点可能切断一行（仅当文件大于 chunk 时），丢弃不完整首行。
    const MAX_TAIL_BYTES = 512 * 1024;
    const fd = fs.openSync(fp, 'r');
    let lines;
    try {
      const size = fs.fstatSync(fd).size;
      const len = Math.min(MAX_TAIL_BYTES, size);
      const buf = Buffer.alloc(len);
      fs.readSync(fd, buf, 0, len, size - len);
      lines = buf.toString('utf8').split('\n');
      if (size > len) lines.shift();
      if (lines[lines.length - 1] === '') lines.pop();
    } finally {
      fs.closeSync(fd);
    }
    return lines.slice(-tail);
  } catch {
    return [];
  }
}

// ---- 任务列表 ----

function readTasks() {
  const db = openRo(DB_PATH);
  if (!db) return [];
  let rows = [];
  try {
    const stmt = db.prepare(
      `SELECT task_id, title, task_status, provider, model, mode,
              created_at, updated_at, meta_json
       FROM tasks WHERE deleted = 0 ORDER BY updated_at DESC LIMIT 40`
    );
    rows = stmt.all().map((r) => {
      let meta = {};
      if (r.meta_json) {
        try { meta = JSON.parse(r.meta_json); } catch { meta = {}; }
      }
      return {
        taskId: r.task_id,
        title: r.title || '(未命名任务)',
        status: r.task_status,
        provider: r.provider,
        model: r.model,
        mode: r.mode,
        createdAt: r.created_at,
        updatedAt: r.updated_at,
        workspacePath: meta.workspacePath || '',
        thoughtLevel: meta.thoughtLevel || '',
      };
    });
  } catch {
    rows = [];
  } finally {
    db.close();
  }

  // ZCode 的 task_status/mode/thoughtLevel 更新有滞后，用 log 实时信号覆盖：
  // 有未闭合 turn 的会话 = 运行中；session.mode.updated 是最新模式等。
  const active = activeSessionIds();
  const modes = sessionModes();
  const levels = sessionThoughtLevels();
  for (const t of rows) {
    if (active.has(t.taskId)) t.status = 'running';
    if (modes[t.taskId]) t.mode = modes[t.taskId];
    if (levels[t.taskId]) t.thoughtLevel = levels[t.taskId];
  }
  return rows;
}

// 每个任务挂 token 用量（tasks.task_id == model_usage.session_id）
function readTaskTokens(taskIds) {
  if (!taskIds.length) return {};
  const db = openRo(MODEL_USAGE_DB);
  if (!db) return {};
  const out = {};
  try {
    const ph = taskIds.map(() => '?').join(',');
    const stmt = db.prepare(
      `SELECT session_id,
              COALESCE(SUM(computed_total_tokens),0) AS total,
              COALESCE(SUM(input_tokens),0)        AS input,
              COALESCE(SUM(output_tokens),0)       AS output,
              COUNT(*)                              AS requests
       FROM model_usage
       WHERE status = 'completed' AND session_id IN (${ph})
       GROUP BY session_id`
    );
    for (const r of stmt.all(...taskIds)) {
      out[r.session_id] = {
        total: r.total,
        input: r.input,
        output: r.output,
        requests: r.requests,
      };
    }
  } catch {
    // 忽略，返回部分结果
  } finally {
    db.close();
  }
  return out;
}

// ---- 用量聚合（权威 model_usage 表）----

function emptyUsage() {
  return {
    inputTokens: 0,
    outputTokens: 0,
    totalTokens: 0,
    cacheReadTokens: 0,
    reasoningTokens: 0,
    requests: 0,
  };
}

function readUsage() {
  const db = openRo(MODEL_USAGE_DB);
  if (!db) return emptyUsage();

  const now = new Date();
  const todayStart = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const todayStartMs = todayStart.getTime();
  const weekAgoMs = todayStartMs - 7 * MS_PER_DAY;

  let todayRow = null;
  let weekRow = null;
  let allRow = null;
  let models = [];
  let lastTs = '';
  try {
    const sumStmt = (where, ...params) =>
      db.prepare(
        `SELECT COALESCE(SUM(input_tokens),0) AS ti,
                COALESCE(SUM(output_tokens),0) AS toks,
                COALESCE(SUM(computed_total_tokens),0) AS tt,
                COALESCE(SUM(cache_read_input_tokens),0) AS cr,
                COALESCE(SUM(reasoning_tokens),0) AS rt,
                COUNT(*) AS reqs
         FROM model_usage WHERE ${where}`
      ).get(...params);

    const ok = "status = 'completed'";
    todayRow = sumStmt(`${ok} AND completed_at >= ?`, todayStartMs);
    weekRow = sumStmt(`${ok} AND completed_at >= ?`, weekAgoMs);
    allRow = sumStmt(ok);

    models = db.prepare(
      `SELECT LOWER(model_id) AS mid,
              COUNT(*) AS reqs,
              COALESCE(SUM(input_tokens),0) AS ti,
              COALESCE(SUM(output_tokens),0) AS toks,
              COALESCE(SUM(computed_total_tokens),0) AS tt
       FROM model_usage
       WHERE ${ok} AND completed_at >= ?
       GROUP BY mid ORDER BY tt DESC`
    ).all(todayStartMs).map((r) => ({
      model: r.mid,
      requests: r.reqs,
      inputTokens: r.ti,
      outputTokens: r.toks,
      totalTokens: r.tt,
    }));

    const mx = db.prepare(
      'SELECT MAX(completed_at) AS mx FROM model_usage WHERE completed_at IS NOT NULL'
    ).get().mx;
    if (mx) {
      try { lastTs = new Date(mx).toISOString(); } catch { lastTs = ''; }
    }
  } catch {
    db.close();
    return emptyUsage();
  }
  db.close();

  const pack = (row) => ({
    inputTokens: row.ti,
    outputTokens: row.toks,
    totalTokens: row.tt,
    cacheReadTokens: row.cr,
    reasoningTokens: row.rt,
    requests: row.reqs,
  });

  return {
    label: '模型用量',
    today: pack(todayRow),
    week: pack(weekRow),
    total: pack(allRow),
    grandTotal: {
      inputTokens: allRow.ti,
      outputTokens: allRow.toks,
      totalTokens: allRow.tt,
    },
    models,
    lastActivity: lastTs,
    updatedAt: now.toISOString(),
  };
}

// ---- 指定 provider 集合的用量（deepseek/opencode，今日/7日/30日三窗口）----

function readProviderUsage(pids) {
  const zero = { totalTokens: 0, inputTokens: 0, outputTokens: 0, requests: 0 };
  if (!pids.length) {
    return { enabled: false, today: { ...zero }, week: { ...zero }, month: { ...zero } };
  }
  const db = openRo(MODEL_USAGE_DB);
  if (!db) {
    return { enabled: true, today: { ...zero }, week: { ...zero }, month: { ...zero } };
  }
  const now = new Date();
  const todayStartMs = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const weekAgoMs = todayStartMs - 7 * MS_PER_DAY;
  const monthAgoMs = todayStartMs - 30 * MS_PER_DAY;
  try {
    const ph = pids.map(() => '?').join(',');
    const ok = "status = 'completed'";
    const sum = (where, ...params) =>
      db.prepare(
        `SELECT COALESCE(SUM(input_tokens),0) AS ti,
                COALESCE(SUM(output_tokens),0) AS toks,
                COALESCE(SUM(computed_total_tokens),0) AS tt,
                COUNT(*) AS reqs
         FROM model_usage WHERE ${where}`
      ).get(...params);
    const pack = (r) => ({
      totalTokens: r.tt,
      inputTokens: r.ti,
      outputTokens: r.toks,
      requests: r.reqs,
    });
    const today = pack(sum(`${ok} AND completed_at >= ? AND provider_id IN (${ph})`, todayStartMs, ...pids));
    const week = pack(sum(`${ok} AND completed_at >= ? AND provider_id IN (${ph})`, weekAgoMs, ...pids));
    const month = pack(sum(`${ok} AND completed_at >= ? AND provider_id IN (${ph})`, monthAgoMs, ...pids));
    return { enabled: true, today, week, month };
  } catch {
    return { enabled: true, today: { ...zero }, week: { ...zero }, month: { ...zero } };
  } finally {
    db.close();
  }
}

// ---- 实时活动（tail 日志）----

function readLiveActivity() {
  const lines = readLogLines(LIVE_LOG_TAIL);
  const toolCalls = {};
  const events = [];
  let turnActive = false;

  for (const line of lines) {
    let obj = null;
    try { obj = JSON.parse(line); } catch { continue; }
    const ev = obj.event || '';
    const ts = obj.timestamp || '';
    const ctx = obj.context || {};
    const sess = obj.sessionId || '';
    const toolName = ctx.toolName || '';

    if (ev === 'tool.call.started') {
      const tcid = obj.toolCallId || '';
      toolCalls[tcid] = { tool: toolName, startedAt: ts, completedAt: null, durationMs: null, sessionId: sess };
      events.push({ type: 'tool_start', tool: toolName, ts, sessionId: sess });
    } else if (ev === 'tool.call.completed') {
      const tcid = obj.toolCallId || '';
      if (toolCalls[tcid]) {
        toolCalls[tcid].completedAt = ts;
        toolCalls[tcid].durationMs = obj.durationMs;
      }
      events.push({ type: 'tool_end', tool: toolName, ts, durationMs: obj.durationMs, sessionId: sess });
    } else if (ev === 'model.request.completed') {
      events.push({ type: 'model', ts, sessionId: sess, model: ctx.modelId || '', durationMs: obj.durationMs });
    } else if (ev === 'turn.started') {
      turnActive = true;
      events.push({ type: 'turn_start', ts, sessionId: sess });
    } else if (ev === 'turn.completed') {
      turnActive = false;
      events.push({ type: 'turn_end', ts, sessionId: sess });
    }
  }

  let runningTool = null;
  for (const tc of Object.values(toolCalls)) {
    if (tc.completedAt === null) { runningTool = tc; break; }
  }

  return {
    currentTool: runningTool,
    activity: events.slice(-8),
    turnActive,
    logFile: newestLogFile() ? path.basename(newestLogFile()) : '',
  };
}

// ---- log 状态信号（任务实时状态覆盖）----

function activeSessionIds() {
  const lines = readLogLines(STATUS_LOG_TAIL);
  const lastTurnOpen = {};
  const lastTurnTs = {};
  const nowMs = Date.now();

  for (const line of lines) {
    if (!line.includes('"turn.')) continue;
    let obj = null;
    try { obj = JSON.parse(line); } catch { continue; }
    const ev = obj.event || '';
    if (ev !== 'turn.started' && ev !== 'turn.completed') continue;
    const sess = obj.sessionId || '';
    if (!sess.startsWith('sess_') || sess.includes('subagent')) continue;
    lastTurnOpen[sess] = ev === 'turn.started';
    lastTurnTs[sess] = obj.timestamp || '';
  }

  const active = new Set();
  for (const [sess, isOpen] of Object.entries(lastTurnOpen)) {
    if (!isOpen) continue;
    const ts = lastTurnTs[sess] || '';
    const tsMs = new Date(ts.replace('Z', '+00:00')).getTime();
    if (!Number.isFinite(tsMs)) { active.add(sess); continue; } // 无法解析 → 视为运行中
    if (nowMs - tsMs <= ACTIVE_TURN_FRESH_MS) active.add(sess);
  }
  return active;
}

function sessionModes() {
  const lines = readLogLines(STATUS_LOG_TAIL);
  const lastMode = {};
  for (const line of lines) {
    if (!line.includes('session.mode.updated')) continue;
    let obj = null;
    try { obj = JSON.parse(line); } catch { continue; }
    if (obj.event !== 'session.mode.updated') continue;
    const sess = obj.sessionId || '';
    if (!sess.startsWith('sess_') || sess.includes('subagent')) continue;
    const mode = (obj.context || {}).mode || '';
    if (mode) lastMode[sess] = mode;
  }
  return lastMode;
}

function sessionThoughtLevels() {
  const lines = readLogLines(STATUS_LOG_TAIL);
  const lastLevel = {};
  for (const line of lines) {
    if (!line.includes('session.reasoning_effort.updated')) continue;
    let obj = null;
    try { obj = JSON.parse(line); } catch { continue; }
    if (obj.event !== 'session.reasoning_effort.updated') continue;
    const sess = obj.sessionId || '';
    if (!sess.startsWith('sess_') || sess.includes('subagent')) continue;
    const level = (obj.context || {}).thoughtLevel || '';
    if (level) lastLevel[sess] = level;
  }
  return lastLevel;
}

// ---- 聚合结果缓存 ----
// SUM 全表扫描每个 ~20ms，status() 一轮 6+ 个聚合 ≈ 180ms 同步阻塞主进程；
// 每 1.5s 轮询全跑一遍，拖动窗口时消息泵周期性停转 → 卡顿。completed 行
// 只在任务完成时新增，15s 缓存对显示无感知（对齐 deepseek/opencode 的
// cache TTL 模式）。
const AGG_TTL = 15_000;

function ttlMemo(fn, ttl) {
  let ts = 0;
  let val = null;
  return () => {
    if (val === null || Date.now() - ts >= ttl) {
      val = fn();
      ts = Date.now();
    }
    return val;
  };
}

const readUsageCached = ttlMemo(readUsage, AGG_TTL);
const providerMemos = new Map();
function readProviderUsageCached(pids) {
  const key = pids.join(',');
  let memo = providerMemos.get(key);
  if (!memo) {
    memo = ttlMemo(() => readProviderUsage(pids), AGG_TTL);
    providerMemos.set(key, memo);
  }
  return memo();
}

module.exports = {
  readTasks,
  readTaskTokens,
  readUsage: readUsageCached,
  readProviderUsage: readProviderUsageCached,
  readLiveActivity,
  emptyUsage,
};
