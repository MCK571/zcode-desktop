'use strict';

// DSH（DeepSeek Harness）用量统计 — 读 ~/.dsh/sessions 的 zstd 压缩会话日志。
// DSH 会话无 token usage 事件（zcode 有），token 按 ~4 字符/token 估算（标注"估算"）。
// 15s TTL 缓存（复用 deepseek/opencode 的 {ts,payload} 模式），status() 路径零网络。

const fs = require('fs');
const path = require('path');
const os = require('os');
const { zstdDecompressSync } = require('node:zlib');

const SESSIONS_ROOT = path.join(os.homedir(), '.dsh', 'sessions');
const CACHE_TTL = 15.0;
const ZSTD_MAGIC = Buffer.from([0x28, 0xb5, 0x2f, 0xfd]); // zstd frame magic

let cache = { ts: 0, payload: null };

// 逐帧解压 zstd 流（每帧独立 frame，循环找 magic 边界解压拼接）
function decompressZstd(buf) {
  let out = '', pos = 0;
  while (pos < buf.length) {
    const idx = buf.indexOf(ZSTD_MAGIC, pos);
    if (idx < 0) break;
    const next = buf.indexOf(ZSTD_MAGIC, idx + 4);
    const end = next < 0 ? buf.length : next;
    try {
      out += zstdDecompressSync(buf.subarray(idx, end)).toString('utf8');
    } catch (e) {
      break; // 尾帧不完整（正在写入）：丢弃
    }
    pos = end;
  }
  return out;
}

// 统计单个会话日志：turn/step/模型/工具调用/估算 token
function statLog(logPath) {
  let text;
  try {
    const buf = fs.readFileSync(logPath);
    text = logPath.endsWith('.zstd') ? decompressZstd(buf) : buf.toString('utf8');
  } catch (e) {
    return null;
  }
  const s = {
    turns: 0, steps: 0, toolCalls: 0,
    models: {}, estTokens: 0, chars: 0, lastTs: 0,
  };
  for (const line of text.split('\n')) {
    let j;
    try { j = JSON.parse(line); } catch (e) { continue; }
    if (!j || typeof j !== 'object') continue;
    const t = j.type;
    if (t === 'turn/start' || t === 'turn/end') s.turns++;
    else if (t === 'step/start' || t === 'step/end') s.steps++;
    else if (t === 'tool/call') s.toolCalls++;
    else if (t === 'request/context') {
      const m = (j.data && (j.data.model || j.data.provider)) || '';
      if (m) s.models[m] = (s.models[m] || 0) + 1;
    }
    // 文本类事件按字符数累计（估算 token 用）：texts 数组（chunk 流）或 text/content 字段
    if (t === 'assistant/chunk' || t === 'text-chunks' || t === 'reasoning-chunks' || t === 'tool/result') {
      const d = j.data || {};
      const texts = d.texts;
      if (Array.isArray(texts)) {
        for (const x of texts) if (typeof x === 'string') s.chars += x.length;
      } else {
        const txt = d.text || d.content;
        if (typeof txt === 'string') s.chars += txt.length;
        else if (typeof txt === 'object' && txt && Array.isArray(txt.content) && txt.content[0] && typeof txt.content[0].text === 'string') {
          s.chars += txt.content[0].text.length; // tool/result 的 message.content[0].content[0].text
        }
      }
    }
    const ts = Number(j.time || j.createdAt || 0);
    if (ts > s.lastTs) s.lastTs = ts;
  }
  s.estTokens = Math.round(s.chars / 4); // ponytail: 4字符/token 估算，DSH 无 usage 事件
  return s;
}

// 聚合所有 workspace 的会话统计
function scan() {
  const workspaces = [];
  let total = { sessions: 0, turns: 0, steps: 0, toolCalls: 0, estTokens: 0 };
  const modelsAll = {};
  let latestTs = 0;
  let dirs;
  try { dirs = fs.readdirSync(SESSIONS_ROOT); } catch (e) { return { workspaces: [], total, models: [], latestTs }; }

  for (const wsDir of dirs) {
    const wsPath = path.join(SESSIONS_ROOT, wsDir);
    let st;
    try { st = fs.statSync(wsPath); } catch (e) { continue; }
    if (!st.isDirectory()) continue;
    let sids;
    try { sids = fs.readdirSync(wsPath); } catch (e) { continue; }
    const w = { name: wsDir.replace(/^--|--$/g, ''), sessions: 0, turns: 0, steps: 0, toolCalls: 0, estTokens: 0, models: {}, latestTs: 0 };
    for (const sid of sids) {
      const logPath = path.join(wsPath, sid, 'session.jsonl.zstd');
      if (!fs.existsSync(logPath)) continue;
      const s = statLog(logPath);
      if (!s) continue;
      w.sessions++; w.turns += s.turns; w.steps += s.steps; w.toolCalls += s.toolCalls; w.estTokens += s.estTokens;
      for (const [m, c] of Object.entries(s.models)) { w.models[m] = (w.models[m] || 0) + c; modelsAll[m] = (modelsAll[m] || 0) + c; }
      if (s.lastTs > w.latestTs) w.latestTs = s.lastTs;
    }
    if (!w.sessions) continue;
    // 模型分布转数组（按调用次数降序）
    w.modelList = Object.entries(w.models).sort((a, b) => b[1] - a[1]).map(([model, calls]) => ({ model, calls }));
    workspaces.push(w);
    total.sessions += w.sessions; total.turns += w.turns; total.steps += w.steps;
    total.toolCalls += w.toolCalls; total.estTokens += w.estTokens;
    if (w.latestTs > latestTs) latestTs = w.latestTs;
  }
  workspaces.sort((a, b) => b.latestTs - a.latestTs);
  return {
    workspaces,
    total,
    models: Object.entries(modelsAll).sort((a, b) => b[1] - a[1]).map(([model, calls]) => ({ model, calls })),
    latestTs,
    updatedAt: new Date().toISOString(),
  };
}

function getDshUsage() {
  if (cache.payload && (Date.now() - cache.ts) / 1000 < CACHE_TTL) return cache.payload;
  cache = { ts: Date.now(), payload: scan() };
  return cache.payload;
}

module.exports = { getDshUsage, CACHE_TTL, _scan: scan };
