'use strict';

// DeepSeek（自带 key 的 provider）— 移植自 data.py。
// token 用量：本地 model_usage 按 provider_id 聚合；余额：官方 /user/balance
// 后台刷新 + 缓存，status() 路径零网络。

const https = require('https');
const { readProviderUsage } = require('./sqlite');
const { providerIdsByBaseurl, providerApiKey } = require('./volc');

const BALANCE_URL = 'https://api.deepseek.com/user/balance';
const CACHE_TTL = 15.0;

let cache = { ts: 0, payload: null };

function getDeepseekUsage() {
  return readProviderUsage(providerIdsByBaseurl('api.deepseek.com'));
}

function getDeepseekBalance() {
  if (cache.payload) return cache.payload;
  return {
    enabled: Boolean(providerApiKey('api.deepseek.com')),
    balance: '', currency: '', isAvailable: false, error: '',
    updatedAt: new Date().toISOString(),
  };
}

function fetchBalance(apiKey) {
  return new Promise((resolve) => {
    const req = https.request(BALANCE_URL, {
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
  const apiKey = providerApiKey('api.deepseek.com');
  if (!apiKey) {
    cache = {
      ts: Date.now(),
      payload: { enabled: false, balance: '', currency: '', isAvailable: false, error: '', updatedAt: new Date().toISOString() },
    };
    return;
  }

  const obj = await fetchBalance(apiKey);
  if (!obj) {
    // 失败：复用旧缓存（若有效）；否则写 error 结构
    if (cache.payload && cache.payload.enabled && cache.payload.balance !== '') return;
    cache = {
      ts: Date.now(),
      payload: { enabled: true, balance: '', currency: '', isAvailable: false, error: '查询失败', updatedAt: new Date().toISOString() },
    };
    return;
  }

  const infos = obj.balance_infos || [];
  const info = infos.find((b) => b.currency === 'CNY') || infos[0] || {};
  cache = {
    ts: Date.now(),
    payload: {
      enabled: true,
      balance: info.total_balance ?? '',
      currency: info.currency || 'CNY',
      isAvailable: Boolean(obj.is_available),
      error: '',
      updatedAt: new Date().toISOString(),
    },
  };
}

module.exports = { refreshOnce, getDeepseekUsage, getDeepseekBalance, CACHE_TTL };
