'use strict';

// 火山方舟 OpenAPI 用量查询 — 移植自 data.py。
// SigV4 签名 POST + CodingPlan/AgentPlan 套餐解析 + 60s 缓存后台刷新。
// status() 路径零网络（只读缓存），网络调用全在 scheduler 的后台刷新里。

const crypto = require('crypto');
const fs = require('fs');
const os = require('os');
const path = require('path');
const https = require('https');
const { DatabaseSync } = require('node:sqlite');

const HOST = 'open.volcengineapi.com';
const SERVICE = 'ark';
const REGION = 'cn-beijing';
const VERSION = '2024-01-01';
const CACHE_TTL = 15.0;
const MS_PER_DAY = 86_400_000;

const ZCODE_DIR = path.join(os.homedir(), '.zcode');
const MODEL_USAGE_DB = path.join(ZCODE_DIR, 'cli', 'db', 'db.sqlite');
const CONFIG_PATH = path.join(ZCODE_DIR, 'v2', 'config.json');

// ---- 凭证：系统环境变量 > 同目录 .volc.env > 家目录 .volc.env ----

function loadEnvFile(p) {
  try {
    const lines = fs.readFileSync(p, 'utf8').split('\n');
    for (const line of lines) {
      const s = line.trim();
      if (!s || s.startsWith('#')) continue;
      const eq = s.indexOf('=');
      if (eq < 0) continue;
      const k = s.slice(0, eq).trim();
      let v = s.slice(eq + 1).trim();
      if ((v.startsWith('"') && v.endsWith('"')) || (v.startsWith("'") && v.endsWith("'"))) {
        v = v.slice(1, -1);
      }
      if (k && !(k in process.env)) process.env[k] = v;
    }
    return true;
  } catch {
    return false;
  }
}

// 凭证查找目录：exe 同目录（portable 用 PORTABLE_EXECUTABLE_DIR——便携版
// 自解压运行时 cwd 不可靠，实测双击 exe 读不到同目录 .volc.env）> 家目录。
// 开发模式（npx electron）用 cwd（项目根放了 .volc.env）。
const EXE_DIR = process.env.PORTABLE_EXECUTABLE_DIR
  || (process.defaultApp ? process.cwd() : path.dirname(process.execPath || ''));
for (const p of [path.join(EXE_DIR, '.volc.env'), path.join(os.homedir(), '.volc.env')]) {
  if (fs.existsSync(p)) { loadEnvFile(p); break; }
}

const AK_ID = process.env.VOLC_AK_ID || '';
const AK_SECRET = process.env.VOLC_AK_SECRET || '';
const PLAN_TYPE = (process.env.VOLC_PLAN_TYPE || 'coding').trim().toLowerCase() === 'agent' ? 'agent' : 'coding';
const PLAN_TIER = (process.env.VOLC_PLAN_TIER || '').trim();

// 套餐开通时间（可选，weekly/monthly 窗口倒推用）
const PLAN_START_RAW = (process.env.VOLC_PLAN_START || '').trim();
let PLAN_START = null;
if (PLAN_START_RAW) {
  const t = new Date(PLAN_START_RAW.replace(' ', 'T'));
  if (Number.isFinite(t.getTime())) PLAN_START = t;
}

// ---- SigV4 签名 ----

function sha256Hex(data) {
  return crypto.createHash('sha256').update(data).digest('hex');
}
function hmacSha256(key, msg) {
  return crypto.createHmac('sha256', key).update(msg).digest();
}
function volcSigningKey(dateStamp) {
  return hmacSha256(
    hmacSha256(hmacSha256(hmacSha256(Buffer.from(AK_SECRET, 'utf8'), dateStamp), REGION), SERVICE),
    'request'
  );
}

// POST https://HOST/?Action=..&Version=..，返回 Result 或 null（任何异常吞掉）
function volcCall(action, body) {
  return new Promise((resolve) => {
    if (!AK_ID || !AK_SECRET) return resolve(null);
    try {
      const now = new Date();
      const amzDate = now.toISOString().replace(/[:-]|\.\d{3}/g, '');
      const dateStamp = amzDate.slice(0, 8);

      const bodyBytes = Buffer.from(JSON.stringify(body), 'utf8');
      const payloadHash = sha256Hex(bodyBytes);

      const canonicalQuery = `Action=${action}&Version=${VERSION}`;
      const signedHeaders = 'host;x-content-sha256;x-date';
      const canonicalHeaders =
        `host:${HOST}\n` +
        `x-content-sha256:${payloadHash}\n` +
        `x-date:${amzDate}\n`;
      const canonicalRequest = [
        'POST', '/', canonicalQuery, canonicalHeaders, signedHeaders, payloadHash,
      ].join('\n');
      const credentialScope = `${dateStamp}/${REGION}/${SERVICE}/request`;
      const stringToSign = [
        'HMAC-SHA256', amzDate, credentialScope, sha256Hex(canonicalRequest),
      ].join('\n');
      const signature = crypto.createHmac('sha256', volcSigningKey(dateStamp))
        .update(stringToSign).digest('hex');
      const authorization =
        `HMAC-SHA256 Credential=${AK_ID}/${credentialScope}, ` +
        `SignedHeaders=${signedHeaders}, Signature=${signature}`;

      const req = https.request({
        hostname: HOST,
        path: `/?${canonicalQuery}`,
        method: 'POST',
        timeout: 3000,
        headers: {
          Host: HOST,
          'Content-Type': 'application/json; charset=utf-8',
          'X-Date': amzDate,
          'X-Content-Sha256': payloadHash,
          Authorization: authorization,
        },
      }, (res) => {
        let data = '';
        res.on('data', (c) => { data += c; });
        res.on('end', () => {
          try {
            const obj = JSON.parse(data);
            if (res.statusCode !== 200) return resolve(null);
            resolve(obj.Result || {});
          } catch {
            resolve(null);
          }
        });
      });
      req.on('timeout', () => { req.destroy(); resolve(null); });
      req.on('error', () => resolve(null));
      req.write(bodyBytes);
      req.end();
    } catch {
      resolve(null);
    }
  });
}

// ---- config.json 辅助（plan provider / deepseek / opencode 识别）----

function loadConfig() {
  try {
    return JSON.parse(fs.readFileSync(CONFIG_PATH, 'utf8'));
  } catch {
    return {};
  }
}

function providerIdsByBaseurl(keyword) {
  const cfg = loadConfig();
  const out = new Set();
  for (const [pid, info] of Object.entries(cfg.provider || {})) {
    const url = (info.options || {}).baseURL || '';
    if (url.includes(keyword)) out.add(pid);
  }
  return [...out];
}

function providerApiKey(keyword) {
  const cfg = loadConfig();
  for (const info of Object.values(cfg.provider || {})) {
    const url = (info.options || {}).baseURL || '';
    if (url.includes(keyword)) {
      const key = (info.options || {}).apiKey || '';
      if (key) return key;
    }
  }
  return '';
}

function providerPlanIds() {
  const cfg = loadConfig();
  const out = new Set();
  for (const [pid, info] of Object.entries(cfg.provider || {})) {
    const url = (info.options || {}).baseURL || '';
    const name = (info.name || '').toLowerCase();
    if (url.includes('/plan') || name.includes('plan')) out.add(pid);
  }
  return [...out];
}

// 按套餐窗口起点聚合本地 model_usage 分模型 token 明细（只统计 plan provider）
function planWindowModels(startMs) {
  if (!fs.existsSync(MODEL_USAGE_DB) || !startMs) return [];
  const pids = providerPlanIds();
  if (!pids.length) return [];
  try {
    const db = new DatabaseSync(MODEL_USAGE_DB, { readOnly: true });
    const ph = pids.map(() => '?').join(',');
    const rows = db.prepare(
      `SELECT LOWER(model_id) AS mid,
              COUNT(*) AS reqs,
              COALESCE(SUM(computed_total_tokens), 0) AS tt
       FROM model_usage
       WHERE status = 'completed' AND completed_at >= ? AND completed_at <= ?
         AND provider_id IN (${ph})
       GROUP BY mid ORDER BY tt DESC`
    ).all(startMs, Date.now(), ...pids);
    db.close();
    const total = rows.reduce((a, r) => a + r.tt, 0) || 0;
    return rows.map((r) => ({
      model: r.mid,
      tokens: r.tt,
      requests: r.reqs,
      pct: total ? Math.round((r.tt / total) * 1000) / 10 : 0.0,
    }));
  } catch {
    return [];
  }
}

// ---- 套餐解析 ----

function parseCodingPlan(cp) {
  const buckets = [];
  const labelMap = { session: '会话', weekly: '每周', monthly: '每月' };
  for (const item of cp.QuotaUsage || []) {
    const level = item.Level || '';
    if (!(level in labelMap)) continue;
    const usedPct = Math.round(Number(item.Percent || 0) * 10) / 10;
    const remainingPct = Math.round(Math.max(0, 100 - usedPct) * 10) / 10;
    const resetTs = item.ResetTimestamp || 0;
    const resetMs = resetTs ? resetTs * 1000 : 0;

    // 倒推本地 token 聚合窗口起点（毫秒）
    let windowStartMs = 0;
    let needsConfig = false;
    if (resetMs) {
      const resetDt = new Date(resetTs * 1000);
      if (level === 'session') {
        windowStartMs = resetDt.getTime() - 5 * 3600 * 1000;
      } else if (level === 'weekly') {
        if (!PLAN_START) {
          needsConfig = true;
        } else {
          let start = new Date(resetDt.getTime() - 7 * MS_PER_DAY);
          if (start < PLAN_START) start = PLAN_START;
          windowStartMs = start.getTime();
        }
      } else if (level === 'monthly') {
        if (!PLAN_START) {
          needsConfig = true;
        } else {
          // 月份减 1（年进位），归零到当天 00:00，max(开通时刻)
          let m = resetDt.getMonth() - 1;
          let y = resetDt.getFullYear();
          if (m < 0) { m = 11; y -= 1; }
          let start = new Date(y, m, resetDt.getDate(), 0, 0, 0);
          if (start < PLAN_START) start = PLAN_START;
          windowStartMs = start.getTime();
        }
      }
    }

    buckets.push({
      key: level,
      label: labelMap[level],
      quota: 100,
      used: usedPct,
      remaining: remainingPct,
      remainingPct,
      usedPct,
      resetMs,
      windowStart: windowStartMs,
      needsConfig,
      models: [],
    });
  }
  return { rawStatus: cp.Status || '', buckets };
}

function parseAgentPlan(afp) {
  const buckets = [];
  const labelMap = { AFPFiveHour: '5小时', AFPWeekly: '每周', AFPMonthly: '每月' };
  for (const [key, label] of Object.entries(labelMap)) {
    const b = afp[key];
    if (!b) continue;
    const quota = b.Quota || 0;
    const used = b.Used || 0;
    buckets.push({
      key,
      label,
      quota,
      used,
      remaining: Math.max(0, quota - used),
      remainingPct: quota ? Math.round(((quota - used) / quota) * 1000) / 10 : 0.0,
      usedPct: quota ? Math.round((used / quota) * 1000) / 10 : 0.0,
      resetMs: b.ResetTime || 0,
    });
  }
  return { rawStatus: '', buckets };
}

// ---- 缓存 + 后台刷新 ----

let cache = { ts: 0, payload: null };

function getPlanUsage() {
  return cache.payload;
}

async function refreshOnce() {
  if (!AK_ID || !AK_SECRET) {
    cache = {
      ts: Date.now(),
      payload: {
        enabled: false, planType: PLAN_TYPE, tier: PLAN_TIER,
        rawStatus: '', buckets: [], error: '',
        updatedAt: new Date().toISOString(),
      },
    };
    return;
  }

  const action = PLAN_TYPE === 'coding' ? 'GetCodingPlanUsage' : 'GetAFPUsage';
  const resp = await volcCall(action, {});
  if (!resp) {
    // 失败：复用旧缓存（若有有效）；否则写 error 空结构
    if (cache.payload && cache.payload.enabled && cache.payload.buckets && cache.payload.buckets.length) {
      return;
    }
    cache = {
      ts: Date.now(),
      payload: {
        enabled: true, planType: PLAN_TYPE, tier: PLAN_TIER,
        rawStatus: '', buckets: [], error: '调用失败',
        updatedAt: new Date().toISOString(),
      },
    };
    return;
  }

  const parsed = PLAN_TYPE === 'coding' ? parseCodingPlan(resp) : parseAgentPlan(resp);
  if (PLAN_TYPE === 'coding') {
    for (const b of parsed.buckets) {
      if (b.needsConfig || !b.windowStart) continue;
      b.models = planWindowModels(b.windowStart);
    }
  }
  const rawStatus = parsed.rawStatus;
  cache = {
    ts: Date.now(),
    payload: {
      enabled: true,
      planType: PLAN_TYPE,
      tier: PLAN_TIER,
      rawStatus,
      buckets: parsed.buckets,
      error: (PLAN_TYPE === 'coding' && rawStatus && rawStatus !== 'Running')
        ? `套餐未生效（${rawStatus}）` : '',
      updatedAt: new Date().toISOString(),
    },
  };
}

module.exports = {
  refreshOnce,
  getPlanUsage,
  providerIdsByBaseurl,
  providerApiKey,
  providerPlanIds,
  CACHE_TTL,
};
