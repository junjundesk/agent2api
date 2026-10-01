# ZCode 活动套餐 · 验证码令牌纯后台铸造器（sidecar）

网关（agent2api）自带的令牌铸造器跑在**面板页面**里（`desktop-tauri/ui/zcode-captcha-pool.js`）：
页面一关，令牌池 120 秒内耗尽，活动套餐通道随之回 `3007 captcha verify failed`。
本 sidecar 把同一条铸造链搬进服务器上的常驻进程——**面板开不开都持续铸造**。

## 原理

与面板/桌面端完全同一条路：happy-dom 提供的假 DOM 里跑阿里云验证码 SDK
（`o.alicdn.com` 的 `AliyunCaptcha.js`），`initAliyunCaptcha({mode:'popup'})` 拿到
实例后调 `startTracelessVerification()` 无痕铸出验证串（不弹滑块、无需用户操作），
再 `POST /api/zcode/captcha` 推回网关令牌池。三个 HTTP 全部走管理 API、用网关 Key
鉴权（`x-api-key` 头），所以不需要面板会话：

1. `GET /api/zcode/captcha` — 池子概况 + `needsTokens` 判据（有活动套餐账号且库存<3）
2. `POST /api/accounts/{id}/zcode-claim/captcha-config` — 上游风控配置（5 分钟缓存）
3. `POST /api/zcode/captcha {tokens:[{param,region}]}` — 令牌入池

节奏照抄页面守卫：4 秒轮询、每轮最多补 2 个、`needsTokens=false` 时一个不铸
（阿里云对铸造频率有风控）、连续失败指数退让 8s→60s、每 5 分钟一条心跳日志。

## ⚠️ happy-dom 补丁（改动这里的代码前必读）

阿里云 SDK 会**动态加载设备指纹 chunk**（`g.alicdn.com/captcha-frontend/FeiLin/…`），
它使用一批 happy-dom 没实现的浏览器全局。实测（2026-10-01）缺一不可：

- `Option` / `Audio` / `Image` 构造器没挂到裸全局 → 指纹 chunk 抛
  `ReferenceError: Option is not defined` 崩掉 → `window.AliyunCaptcha` 类没建立
  → 主 SDK 读 `.prototype` 抛 TypeError → `getInstance` 永不触发（症状是"静默挂起"）；
- `alert` / `confirm` / `prompt` / `moveBy` / `scrollBy` 等窗口方法缺失；
- `document.fonts` 缺失；`<img>` 的 `onload` 在 happy-dom 里永不触发（默认不加载图片）。

`mintOne()` 开头那段 `window.eval(...)` 就是这批补丁的**全部**，不是优化而是前提。
另外 UA 必须伪装成 Chrome（happy-dom 默认 UA 带 `HappyDOM` 字样，上游指纹会分支）；
审计脚本的返回值也要用 `window.eval` 在 Window 世界内取（跨世界对象原型链不可见，
主世界调用实例方法会静默无效）。

## 部署（已在 headless 服务器上以 systemd 常驻）

宿主机自带 Node 22，直接装 systemd 服务（Docker Hub 在该服务器拉基础镜像不稳，
故不走容器；`Dockerfile` / `docker-compose.mint.yml` 保留给网络正常的机器）：

```bash
# 服务器上：
mkdir -p /opt/agent2api-mint && cd /opt/agent2api-mint
# 放 mint.js / package.json 后：
npm install --omit=dev --no-audit --no-fund --registry=https://registry.npmmirror.com
# 安装 agent2api-mint.service（本目录同名的文件）：
cp agent2api-mint.service /etc/systemd/system/
systemctl daemon-reload && systemctl enable --now agent2api-mint
```

环境变量（systemd 单元里已内置）：

| 变量 | 说明 | 默认 |
| --- | --- | --- |
| `AGENT2API_BASE_URL` | 网关地址 | `http://agent2api:3065` |
| `AGENT2API_GATEWAY_KEY` | 网关 Key（面板「网关 Key」页的 `sk-a2a-…`） | 必填 |

## 验证

```bash
systemctl status agent2api-mint                  # active (running)
journalctl -u agent2api-mint -f                  # 「令牌入池 N 个」/五分钟心跳
curl -s http://127.0.0.1:3065/api/zcode/captcha -H 'x-api-key: sk-a2a-…'
# ready 稳定在 3、needsTokens 在 false/true 间随令牌老化切换 = 后台铸造在自持
# 端到端：POST /v1/chat/completions 用活动套餐允许的模型（glm-5.3 / glm-5.3-flash）
#  → 正常回复且 rejected 计数保持 0（若回 3007 才说明令牌有问题）
```

## 单测

```bash
node mint.js --once <sceneId> <prefix> <region>   # 只铸一个，打印结果不推池
# 参数从网关取：POST /api/accounts/{id}/zcode-claim/captcha-config
```
