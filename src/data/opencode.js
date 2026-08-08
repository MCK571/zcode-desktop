'use strict';

// opencode — 移植自 data.py。
// token 用量：本地 model_usage 按 baseURL 含 opencode.ai 的 provider 聚合；
// Go 套餐余量：抓取 dashboard 页面解析（SolidJS SSR 水合数据 / data-slot）。

const https = require('https');
const { readProviderUsage } = require('./sqlite');
const { providerIdsByBaseurl } = require('./volc');

const CACHE_TTL = 60.0;
const WORKSPACE_ID = (process.env.OPENCODE_GO_WORKSPACE_ID || '').trim();
const AUTH_COOKIE = (process.env.OPENCODE_GO_AUTH_COOKIE || '').trim();
const WINDOW_FIELDS = ['rollingUsage', 'weeklyUsage', 'monthlyUsage'];
const WINDOW_LABELS = { rolling: '5小时', weekly: '每周', monthly: '每月' };

let cache = { ts: 0, payload: null };

function getOpencodeUsage() {
  return readProviderUsage(providerIdsByBaseurl('opencode.ai'));
}

function getOpencodeGo() {
  return cache.payload;
}

// 把 (已用百分比, 剩余秒) 归一成 window dict
function makeWindow(usedPct, resetSec) {
  usedPct = Math.max(0, Math.min(100, usedPct));
  resetSec = Math.max(0, resetSec);
  return {
    usedPct: Math.round(usedPct * 10) / 10,
    remainingPct: Math.round((100 - usedPct) * 10) / 10,
    resetMs: Math.round((Date.now() / 1000 + resetSec) * 1000),
  };
}

// "6 days 2 hours 30 minutes" → 秒
function parseHumanTime(text) {
  const normalized = text.toLowerCase().replace(/\u2014/g, ' ').trim().replace(/\s+/g, ' ');
  if (['reset-now', 'reset now', 'now', 'resets now'].includes(normalized)) return 0.0;
  let total = 0.0;
  let found = false;
  const units = [
    ['days?', 86400], ['hours?', 3600], ['minutes?', 60], ['seconds?', 1],
  ];
  for (const [unit, mult] of units) {
    const m = normalized.match(new RegExp(`([\\d.]+)\\s*${unit}`));
    if (m) { total += parseFloat(m[1]) * mult; found = true; }
  }
  return found ? total : null;
}

function parseWindow(html, field) {
  // SolidJS SSR：usagePercent 与 resetInSec 两种顺序
  for (const pctFirst of [true, false]) {
    const body = new RegExp(
      `${field}:\\$R\\[\\d+\\]=\\{[^}]*` +
      (pctFirst
        ? 'usagePercent:([\\d.]+)[^}]*resetInSec:([\\d.]+)'
        : 'resetInSec:([\\d.]+)[^}]*usagePercent:([\\d.]+)') +
      '[^}]*\\}'
    );
    const m = html.match(body);
    if (m) {
      const pct = pctFirst ? parseFloat(m[1]) : parseFloat(m[2]);
      const resetSec = pctFirst ? parseFloat(m[2]) : parseFloat(m[1]);
      if (!Number.isNaN(pct) && !Number.isNaN(resetSec)) return makeWindow(pct, resetSec);
    }
  }

  // data-slot 格式：按 usage-item 分割，label 里含窗口名
  const want = field.replace('Usage', '');
  for (const item of html.split('data-slot="usage-item"').slice(1)) {
    const lm = item.match(/data-slot="usage-label">([^<]+)</);
    if (!lm) continue;
    const label = lm[1].trim().toLowerCase();
    const key = ['rolling', 'weekly', 'monthly'].find((k) => label.includes(k));
    if (key !== want) continue;
    const um = item.match(/data-slot="usage-value">[^0-9]*([\d.]+)/);
    if (!um) continue;
    const rm = item.match(/data-slot="(reset-time|reset-now)">([\s\S]*?)<\/span>/);
    if (!rm) continue;
    let resetSec;
    if (rm[1] === 'reset-now') {
      resetSec = 0.0;
    } else {
      resetSec = parseHumanTime(rm[2]);
      if (resetSec === null) continue;
    }
    return makeWindow(parseFloat(um[1]), resetSec);
  }
  return null;
}

// 抓取 dashboard 页面（认证用 auth cookie）
function fetchDashboard() {
  return new Promise((resolve, reject) => {
    const url = `https://opencode.ai/workspace/${WORKSPACE_ID}/go`;
    const req = https.request(url, {
      method: 'GET',
      timeout: 5000,
      headers: {
        'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) Gecko/20100101 Firefox/148.0',
        Accept: 'text/html',
        Cookie: `auth=${AUTH_COOKIE}`,
      },
    }, (res) => {
      let html = '';
      res.on('data', (c) => { html += c; });
      res.on('end', () => {
        if (res.statusCode !== 200) return reject(new Error(`HTTP ${res.statusCode}`));
        resolve(html);
      });
    });
    req.on('timeout', () => { req.destroy(); reject(new Error('timeout')); });
    req.on('error', reject);
    req.end();
  });
}

async function ocgoFetch() {
  const html = await fetchDashboard();
  const out = {};
  for (const field of WINDOW_FIELDS) {
    const win = parseWindow(html, field);
    if (win) out[field.replace('Usage', '')] = win;
  }
  if (!Object.keys(out).length) throw new Error('no usage window found');
  return out;
}

async function refreshOnce() {
  if (!WORKSPACE_ID || !AUTH_COOKIE) {
    cache = {
      ts: Date.now(),
      payload: { enabled: false, workspaceId: '', buckets: [], error: '', updatedAt: new Date().toISOString() },
    };
    return;
  }

  let parsed;
  try {
    parsed = await ocgoFetch();
  } catch {
    // 失败：复用旧缓存（若有效）；否则写 error 结构
    if (cache.payload && cache.payload.enabled && cache.payload.buckets && cache.payload.buckets.length) return;
    cache = {
      ts: Date.now(),
      payload: { enabled: true, workspaceId: WORKSPACE_ID, buckets: [], error: '抓取失败（cookie 可能过期）', updatedAt: new Date().toISOString() },
    };
    return;
  }

  cache = {
    ts: Date.now(),
    payload: {
      enabled: true,
      workspaceId: WORKSPACE_ID,
      buckets: Object.entries(parsed).map(([key, win]) => ({
        key,
        label: WINDOW_LABELS[key] || key,
        usedPct: win.usedPct,
        remainingPct: win.remainingPct,
        resetMs: win.resetMs,
      })),
      error: '',
      updatedAt: new Date().toISOString(),
    },
  };
}

module.exports = { refreshOnce, getOpencodeUsage, getOpencodeGo, CACHE_TTL };
