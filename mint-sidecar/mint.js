#!/usr/bin/env node
/* ZCode 活动套餐 · 验证码令牌**纯后台铸造器**（sidecar）
 *
 * ── 解决什么问题 ─────────────────────────────────────────────
 * 网关自带的铸造器跑在**面板页面**里（ui/zcode-captcha-pool.js）：页面一关，
 * 令牌池就见底（令牌寿命 120 秒），活动套餐通道随之 3007。本进程把同一条
 * 铸造链搬进服务端常驻进程：轮询 → 取风控配置 → happy-dom 里跑阿里云 SDK
 * 无痕铸造 → 推回网关令牌池。**不需要任何浏览器页面。**
 *
 * ── 与网关的全部交互（三条 HTTP，全部走管理 API）────────────
 *   1. GET  /api/zcode/captcha                                池子概况 + needsTokens 判据
 *   2. POST /api/accounts/{captchaAccountId}/zcode-claim/captcha-config   风控配置（带 5 分钟缓存）
 *   3. POST /api/zcode/captcha  {tokens:[{param,region}]}     铸好的令牌入池
 * 鉴权用网关 Key（x-api-key 头）：面板开启登录时 Key 与会话同权（见 http.rs
 * 的 require_api_key），面板未开登录时 /api/* 本来就认 Key。
 *
 * ── 铸造原理 ────────────────────────────────────────────────
 * 与桌面端/面板完全同一条路（ui/aliyun-captcha.js 的 mintTraceless）：
 * SDK 从 o.alicdn.com 加载，`window.AliyunCaptchaConfig` 必须在加载**之前**
 * 设好；initAliyunCaptcha（mode:'popup'，success/fail 回调收结果）拿到实例后
 * 调 `startTracelessVerification()`，SDK 自己跑完风控把 base64 验证串交给
 * success。**一次一铸**：实例第二次 start 必失败，每次铸造都新开一个
 * happy-dom Window，用完即毁。
 *
 * ── 节奏（照抄 ui/zcode-captcha-pool.js，不铸多、不空窗）────
 * 每 4 秒问一次 needsTokens：库存低于目标（3）才补，每轮最多 2 个，铸间 300ms；
 * 上游刚回 3007（rejected 计数变大）→ 立刻补；连续失败指数退让 8s→60s。
 * needsTokens=false 时一个都不铸（阿里云对铸造频率有风控，白铸烧配额）。
 *
 * 环境变量：
 *   AGENT2API_BASE_URL     网关地址（默认 http://agent2api:3065，同 compose 网络）
 *   AGENT2API_GATEWAY_KEY  网关 Key（必填，面板「网关 Key」页创建的 sk-a2a-…）
 */

import { Window } from 'happy-dom';

const BASE_URL = (process.env.AGENT2API_BASE_URL || 'http://agent2api:3065').replace(/\/+$/, '');
const API_KEY = process.env.AGENT2API_GATEWAY_KEY || '';
if (!API_KEY && !process.argv.includes('--once')) {
  console.error('[MINT] 缺少 AGENT2API_GATEWAY_KEY（网关 Key）——退出');
  process.exit(1);
}

const POLL_MS = 4000;
const MAX_PER_ROUND = 2;
const BACKOFF_MIN_MS = 8000;
const BACKOFF_MAX_MS = 60000;
const CONFIG_TTL_MS = 5 * 60000;

const SDK_URL = 'https://o.alicdn.com/captcha-frontend/aliyunCaptcha/AliyunCaptcha.js';
const SCRIPT_LOAD_TIMEOUT_MS = 40000;
const INIT_TIMEOUT_MS = 40000;
const MINT_TIMEOUT_MS = 20000;

let timer = 0;
let running = false;
let stopped = false;
let failures = 0;
let lastRejected = 0;
let lastHeartbeatAt = 0;
let configCache = { value: null, expiresAt: 0 };
/** 心跳间隔：面板看不到本进程，没有心跳就没法从日志判断它是否还活着 */
const HEARTBEAT_MS = 5 * 60000;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** 单测入口：`node mint.js --once` 只铸一个（打印结果不推池） */
if (process.argv.includes('--once')) {
  const config = {
    sceneId: process.argv[process.argv.indexOf('--once') + 1] || '',
    prefix: process.argv[process.argv.indexOf('--once') + 2] || '',
    region: process.argv[process.argv.indexOf('--once') + 3] || '',
  };
  if (!config.sceneId) {
    console.error('用法：node mint.js --once <sceneId> <prefix> <region>');
    process.exit(1);
  }
  try {
    const param = await mintOne(config);
    console.log(`MINT_OK len=${param.length}`);
    console.log(JSON.parse(Buffer.from(param, 'base64').toString('utf8')).securityToken ? 'securityToken OK' : 'no securityToken');
  } catch (error) {
    console.error(`MINT_FAIL ${error.message}`);
    process.exit(3);
  }
  process.exit(0);
}

function log(message) {
  console.log(`[MINT ${new Date().toISOString()}] ${message}`);
}

/** 调网关管理 API（x-api-key 鉴权；返回信封里的 data） */
async function gateway(path, options = {}) {
  const response = await fetch(`${BASE_URL}${path}`, {
    ...options,
    headers: {
      'content-type': 'application/json',
      'x-api-key': API_KEY,
      ...(options.headers || {}),
    },
  });
  const text = await response.text();
  if (!response.ok) {
    throw new Error(`HTTP ${response.status} ${text.slice(0, 200)}`);
  }
  try {
    const body = JSON.parse(text);
    return body && body.data !== undefined ? body.data : body;
  } catch {
    return text;
  }
}

async function getStats() {
  try {
    return await gateway('/api/zcode/captcha');
  } catch (error) {
    log(`读取令牌池概况失败：${error.message}`);
    return null;
  }
}

/** 风控配置（{sceneId,prefix,region}；5 分钟缓存）。enabled:false 时返回 null（此刻不铸） */
async function getCaptchaConfig(accountId) {
  if (configCache.value && configCache.expiresAt > Date.now()) return configCache.value;
  if (!accountId) return null;
  try {
    const config = await gateway(`/api/accounts/${encodeURIComponent(accountId)}/zcode-claim/captcha-config`, {
      method: 'POST',
    });
    if (!config?.enabled || !config.sceneId) {
      log('上游风控配置未启用（enabled:false）——不铸造');
      return null;
    }
    configCache = {
      value: { sceneId: config.sceneId, prefix: config.prefix, region: config.region },
      expiresAt: Date.now() + CONFIG_TTL_MS,
    };
    return configCache.value;
  } catch (error) {
    log(`取风控配置失败：${error.message}`);
    return null;
  }
}

async function pushTokens(tokens) {
  try {
    const result = await gateway('/api/zcode/captcha', {
      method: 'POST',
      body: JSON.stringify({ tokens }),
    });
    log(`令牌入池 ${tokens.length} 个（库存 ${result?.ready ?? '?'}，目标 ${result?.target ?? '?'}）`);
    return true;
  } catch (error) {
    log(`令牌入池失败：${error.message}`);
    return false;
  }
}

/** 上游认的验证串形态：约 280 字符的 base64 JSON，内含长 securityToken（照抄 validateVerifyParam） */
function validateVerifyParam(value) {
  if (typeof value !== 'string' || value.trim().length < 200) {
    throw new Error('验证串不完整');
  }
  const text = value.trim();
  try {
    const json = JSON.parse(Buffer.from(text, 'base64').toString('utf8'));
    const token = json && (json.securityToken || json.SecurityToken);
    if (!token || String(token).length < 50) throw new Error('no securityToken');
  } catch {
    throw new Error('验证串不是上游认的形态');
  }
  return text;
}

/**
 * 在一个全新的 happy-dom Window 里铸一个令牌。
 *
 * 整个铸造流程（加载 SDK → initAliyunCaptcha → startTracelessVerification →
 * success 回调拿串）**整体在 Window 世界里执行**（主世界把流程代码 eval 进去，
 * 结果落在 `window.__MINT_RESULT`）：SDK 实例是 Window 世界的对象，主世界直接
 * 持有/调用它拿不到方法（原型链跨世界不可见，实测 hasStart 显示 function 但
 * 调用静默无效），而纯字符串结果跨世界传递没有问题。
 *
 * 镜像 ui/aliyun-captcha.js 的 mintTraceless：AliyunCaptchaConfig 必须在 SDK
 * 加载**之前**设好；实例**一次一铸**，每次铸造都新开 Window，用完即毁。
 */
async function mintOne(config) {
  const window = new Window({
    url: 'https://www.z.ai/',
    settings: {
      // 伪装成 Chrome：上游设备指纹对 UA 分支，happy-dom 默认 UA 带 "HappyDOM" 显然不行
      navigator: {
        userAgent:
          'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36',
      },
    },
  });
  try {
    window.document.body.innerHTML =
      '<div id="mint-el" style="position:fixed;left:-9999px;top:0;width:360px;height:40px"></div>' +
      '<button id="mint-btn" type="button" aria-hidden="true"></button>';

    // ── happy-dom 缺失的浏览器全局补丁（2026-10-01 实测，缺一不可）─────
    // 阿里云 SDK 会动态加载设备指纹 chunk（g.alicdn.com/captcha-frontend/FeiLin/…），
    // 它 `new Option()` 建元素做指纹 —— happy-dom 没把 HTMLOptionElement 挂到裸
    // 全局 `Option`，chunk 抛 ReferenceError 崩掉 → `window.AliyunCaptcha` 类没建立
    // → 主 SDK 读 `window.AliyunCaptcha.prototype` 抛 TypeError → getInstance 永不
    // 触发。此外 SDK 还会调 alert/moveBy/scrollBy 之类窗口方法、等 document.fonts
    // 与图片 onload —— 全部打桩。**这些补丁不是「优化」，是铸造能跑通的前提。**
    window.eval(`
      if (typeof window.Option === 'undefined' && typeof HTMLOptionElement !== 'undefined') window.Option = HTMLOptionElement;
      if (typeof window.Audio === 'undefined' && typeof HTMLAudioElement !== 'undefined') window.Audio = HTMLAudioElement;
      if (typeof window.Image === 'undefined' && typeof HTMLImageElement !== 'undefined') window.Image = HTMLImageElement;
      if (typeof window.alert !== 'function') window.alert = function () {};
      if (typeof window.confirm !== 'function') window.confirm = function () { return true; };
      if (typeof window.prompt !== 'function') window.prompt = function () { return null; };
      ['moveBy','moveTo','resizeBy','resizeTo','scrollBy','scrollTo','print'].forEach(function (k) {
        if (typeof window[k] !== 'function') window[k] = function () {};
      });
      if (typeof window.open !== 'function') window.open = function () { return null; };
      if (!document.fonts) {
        document.fonts = { status: 'loaded', ready: Promise.resolve(), check: function(){return true;}, load: function(){ return Promise.resolve([]); } };
      }
      (function () {
        var proto = window.HTMLImageElement && window.HTMLImageElement.prototype;
        if (!proto) return;
        var desc = Object.getOwnPropertyDescriptor(proto, 'src');
        if (desc && desc.set) {
          Object.defineProperty(proto, 'src', {
            get: function () { try { return desc.get.call(this); } catch (e) { return this.__src || ''; } },
            set: function (v) {
              var self = this;
              try { desc.set.call(this, v); } catch (e) { this.__src = v; }
              setTimeout(function () { try { if (typeof self.onload === 'function') self.onload({ type: 'load' }); } catch (e) {} }, 30);
            },
            configurable: true,
          });
        }
      })();
    `);

    // 必须在 SDK 执行之前设好（SDK 读它决定打哪个阿里云站点，设晚了会一直转圈）
    window.AliyunCaptchaConfig = { region: config.region, prefix: config.prefix };

    const script = window.document.createElement('script');
    script.src = SDK_URL;
    window.document.head.appendChild(script);
    for (
      let waited = 0;
      typeof window.initAliyunCaptcha !== 'function' && waited < SCRIPT_LOAD_TIMEOUT_MS;
      waited += 250
    ) {
      await sleep(250);
    }
    if (typeof window.initAliyunCaptcha !== 'function') {
      throw new Error('验证码组件加载失败（initAliyunCaptcha 未定义）');
    }

    // 流程整体进 Window 世界；结果（串或错误消息）落在 __MINT_RESULT
    const sceneId = JSON.stringify(config.sceneId);
    const region = JSON.stringify(config.region);
    const prefix = JSON.stringify(config.prefix);
    window.eval(`
      window.__MINT_RESULT = { state: 'waiting' };
      (function () {
        var timeoutId = 0;
        var settle = function (state, param, error) {
          if (window.__MINT_RESULT.state !== 'waiting') return;
          window.__MINT_RESULT.state = state;
          window.__MINT_RESULT.param = param || null;
          window.__MINT_RESULT.error = error || null;
          if (timeoutId) window.clearTimeout(timeoutId);
          timeoutId = window.setTimeout(function () {
            window.__MINT_TIMER_DONE = true;
          }, 50);
        };
        var validate = function (value) {
          if (typeof value !== 'string' || value.trim().length < 200) {
            throw new Error('验证串不完整');
          }
          var text = value.trim();
          var json = JSON.parse(window.atob(text));
          var token = json && (json.securityToken || json.SecurityToken);
          if (!token || String(token).length < 50) throw new Error('no securityToken');
          return text;
        };
        var deliver = function (result) {
          var value = result;
          if (result && typeof result === 'object') {
            value = result.verifyParam || result.captchaVerifyParam || result.data || result.param;
          }
          try {
            settle('success', validate(value), null);
          } catch (error) {
            settle('fail', null, error.message);
          }
        };
        timeoutId = window.setTimeout(function () {
          settle('fail', null, '静默铸造超时');
        }, ${MINT_TIMEOUT_MS});
        initAliyunCaptcha({
          SceneId: ${sceneId},
          mode: 'popup',
          region: ${region},
          prefix: ${prefix},
          element: '#mint-el',
          button: '#mint-btn',
          captchaLogoImg: '',
          showErrorTip: false,
          language: 'cn',
          getInstance: function (instance) {
            var start = typeof instance.startTracelessVerification === 'function'
              ? instance.startTracelessVerification
              : instance.show;
            if (typeof start !== 'function') {
              settle('fail', null, '验证码组件不支持静默铸造');
              return;
            }
            try {
              start.call(instance);
            } catch (error) {
              settle('fail', null, (error && error.message) || '验证码组件启动失败');
            }
          },
          success: function (result) { deliver(result); },
          fail: function (error) {
            settle('fail', null, (error && error.message) || '验证码校验失败');
          },
          onError: function (error) {
            settle('fail', null, (error && error.message) || '验证码组件出错');
          },
        });
      })();
    `);

    // 主世界只轮询结果状态（跨世界只传字符串）
    for (let waited = 0; waited < MINT_TIMEOUT_MS + 10000; waited += 250) {
      await sleep(250);
      const state = window.eval("window.__MINT_RESULT.state");
      if (state === 'waiting') continue;
      if (state === 'success') {
        const param = window.eval("window.__MINT_RESULT.param");
        if (typeof param !== 'string' || param.length === 0) {
          throw new Error('铸造回调给出空结果');
        }
        return param;
      }
      const error = window.eval("window.__MINT_RESULT.error");
      throw new Error(String(error) || '铸造失败');
    }
    throw new Error('静默铸造超时（结果回调未到达）');
  } finally {
    // 用完即毁：实例一次一铸，Window 里的定时器/socket 一并清掉，防进程泄漏
    try {
      await window.happyDOM.close();
    } catch {
      /* 忽略：已不可用 */
    }
  }
}

/** 一轮：读概况 → 需要就补 → 排下一次（节奏与 ui/zcode-captcha-pool.js 一致） */
async function round() {
  if (running || stopped) return;
  running = true;
  try {
    const stats = await getStats();
    if (!stats) {
      schedule(BACKOFF_MIN_MS);
      return;
    }
    // 上游刚拒过令牌（3007）：库存即使「够」也不可信了，也补一轮
    const rejectedNow = Number(stats.rejected) || 0;
    const challenged = rejectedNow > lastRejected;
    lastRejected = rejectedNow;
    const want = stats.needsTokens === true || challenged;
    if (!want) {
      const now = Date.now();
      if (now - lastHeartbeatAt >= HEARTBEAT_MS) {
        lastHeartbeatAt = now;
        log(`心跳：库存 ${stats.ready ?? '?'}/${stats.target ?? '?'}，需要铸造=false，累计入池 ${stats.minted ?? '?'}`);
      }
      schedule(POLL_MS);
      return;
    }
    const config = await getCaptchaConfig(stats.captchaAccountId);
    if (!config) {
      schedule(BACKOFF_MIN_MS);
      return;
    }
    const target = Math.max(1, Number(stats.target) || 1);
    const ready = Number(stats.ready) || 0;
    // 上游刚 3007 过时至少补 1 个（库存读数已不可信）
    const need = challenged
      ? Math.min(MAX_PER_ROUND, Math.max(1, target - ready))
      : Math.min(MAX_PER_ROUND, Math.max(0, target - ready));
    if (need <= 0) {
      schedule(POLL_MS);
      return;
    }
    const tokens = [];
    for (let index = 0; index < need; index += 1) {
      let param = null;
      try {
        param = await mintOne(config);
      } catch (error) {
        log(`铸造失败：${error.message}`);
      }
      if (!param) break;
      tokens.push({ param, region: config.region || '' });
      // 连续铸造之间留间隔（SDK 同一进程里连跑会互相干扰）
      if (index < need - 1) await sleep(300);
    }
    if (tokens.length === 0) {
      failures += 1;
      schedule(Math.min(BACKOFF_MIN_MS * failures, BACKOFF_MAX_MS));
      return;
    }
    await pushTokens(tokens);
    failures = 0;
    schedule(POLL_MS);
  } catch (error) {
    failures += 1;
    log(`本轮异常：${error.message}`);
    schedule(Math.min(BACKOFF_MIN_MS * failures, BACKOFF_MAX_MS));
  } finally {
    running = false;
  }
}

function schedule(delay) {
  clearTimeout(timer);
  if (stopped) return;
  timer = setTimeout(() => void round(), delay);
}

process.on('SIGTERM', () => {
  stopped = true;
  clearTimeout(timer);
  process.exit(0);
});
process.on('SIGINT', () => {
  stopped = true;
  clearTimeout(timer);
  process.exit(0);
});

log(`启动：网关 ${BASE_URL}，库存目标由网关下发；needsTokens=false 时不铸造`);
void round();
