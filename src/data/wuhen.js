'use strict';

// 无痕中转（api.wuhen-ai.com）— 余额 + 用量查询，仿 deepseek.js 范式。
// key 从 ~/.zcode/v2/config.json 按 baseURL 关键字匹配（无痕gpt/无痕grok）；
// GET /v1/usage 一次拿余额 + 今日/累计 + 按日 + 按模型。后台刷新 + 缓存。

const https = require('https');
const { providerApiKey } = require('./volc');

const USAGE_URL = 'https://api.wuhen-ai.com/v1/usage';
const CACHE_TTL = 15.0;

let cache = { ts: 0, payload: null };

function getWuhenUsage() {
  if (cache.payload) return cache.payload;
  return {
    enabled: Boolean(providerApiKey('api.wuhen-ai.com')),
    balance: '', unit: '', isValid: false, planName: '',
    today: { requests: 0, tokens: 0, cost: 0 },
    total: { requests: 0, tokens: 0, cost: 0 },
    daily: [], models: [],
    error: '', updatedAt: new Date().toISOString(),
  };
}

function fetchUsage(apiKey) {
  return new Promise((resolve) => {
    const req = https.request(USAGE_URL, {
      method: 'GET',
      timeout: 3000,
      headers: { Authorization: `Bearer ${apiKey}`, Accept: 'application/json' },
    }, (res) => {
      let data = '';
      res.on('data', (c) => { data += c; });
      res.on('end', () => {
        try {
          const obj = JSON.parse(data);
          if (res.statusCode !== 200) return resolve(null);
          resolve(obj);
        } catch {
          resolve(null);
        }
      });
    });
    req.on('timeout', () => { req.destroy(); resolve(null); });
    req.on('error', () => resolve(null));
    req.end();
  });
}

async function refreshOnce() {
  const apiKey = providerApiKey('api.wuhen-ai.com');
  if (!apiKey) {
    cache = {
      ts: Date.now(),
      payload: {
        enabled: false, balance: '', unit: '', isValid: false, planName: '',
        today: { requests: 0, tokens: 0, cost: 0 },
        total: { requests: 0, tokens: 0, cost: 0 },
        daily: [], models: [],
        error: '', updatedAt: new Date().toISOString(),
      },
    };
    return;
  }

  const obj = await fetchUsage(apiKey);
  if (!obj) {
    // 失败：复用旧缓存（若有效）；否则写 error 结构
    if (cache.payload && cache.payload.enabled && cache.payload.balance !== '') return;
    cache = {
      ts: Date.now(),
      payload: {
        enabled: true, balance: '', unit: '', isValid: false, planName: '',
        today: { requests: 0, tokens: 0, cost: 0 },
        total: { requests: 0, tokens: 0, cost: 0 },
        daily: [], models: [],
        error: '查询失败', updatedAt: new Date().toISOString(),
      },
    };
    return;
  }

  const today = obj.usage && obj.usage.today || {};
  const total = obj.usage && obj.usage.total || {};
  cache = {
    ts: Date.now(),
    payload: {
      enabled: true,
      balance: obj.balance ?? obj.remaining ?? '',
      unit: obj.unit || 'USD',
      isValid: obj.isValid ?? true,
      planName: obj.planName || '',
      today: {
        requests: today.requests || 0,
        tokens: today.total_tokens || 0,
        cost: today.cost || 0,
        actualCost: today.actual_cost || 0,
      },
      total: {
        requests: total.requests || 0,
        tokens: total.total_tokens || 0,
        cost: total.cost || 0,
        actualCost: total.actual_cost || 0,
      },
      // 近 7 天按日（旧日期在前，取尾部翻转成近到远）
      daily: (obj.daily_usage || []).slice(-7).map((d) => ({
        date: d.date,
        requests: d.requests || 0,
        tokens: d.total_tokens || 0,
        cost: d.cost || 0,
      })),
      models: (obj.model_stats || []).map((m) => ({
        model: m.model || '',
        requests: m.requests || 0,
        tokens: m.total_tokens || 0,
        cost: m.cost || 0,
      })),
      error: '',
      updatedAt: new Date().toISOString(),
    },
  };
}

module.exports = { refreshOnce, getWuhenUsage, CACHE_TTL };
