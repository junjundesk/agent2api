//! Cline 适配器：把凭证层接进 provider 注册表（无状态，OpenAI 兼容）。
//!
//! ── 上游协议（全部实测核对，2026-09）─────────────────────────
//!   - LLM：`POST {apiBase}/chat/completions`，base 是
//!     `https://api.cline.bot/api/v1`（完整路径 `/api/v1/chat/completions`）。
//!     上游用 AI SDK 的 `createOpenAICompatible`，把 baseURL 当**根**拼 ——
//!     所以路径里**只有一个 `v1`**。**发错路径的表现是 404 `{"error":"Not Found"}`**：
//!     实测 `/api/v1/v1/chat/completions`（照 base 含 v1 直觉拼的）就是 404。
//!   - 认证：`Authorization: Bearer <token>`，token **含 `workos:` 前缀**
//!     （见 `credentials.rs`）。
//!   - **产品面校验头 `X-CLIENT-TYPE: cline-sdk`（本集成的关键）**：
//!     不带这个头时，**免费池模型一律 403**
//!     （`only available via Cline product surfaces`）；订阅池模型则报
//!     `ENTITLEMENT_ERROR`（那是账号确实没订阅，与头无关）。
//!     实测：带上 `X-CLIENT-TYPE: cline-sdk` 后免费池返回 200。
//!     `User-Agent: Cline/...` **不是**必需（实测只带 X-CLIENT-TYPE 就通过），
//!     但一并带上更贴近官方客户端（上游的行为可能随版本收紧，带上没有代价）。
//!   - **响应形态的不对称（必须记住）**：
//!       - `stream: true` → **裸 `chat.completion.chunk` 帧**（与 OpenAI 逐字同形），
//!         可以直接透传，**不需要解包**；
//!       - 非流式 → **信封** `{"data":{...},"success":true}`，内层才是
//!         `chat.completion`。
//!     网关**总是以 `stream: true` 请求上游**（`upstream::mod` 的既有编排），
//!     因此转发链路上只走前者；信封形态只影响「我们自己打非流式调试」的场景。
//!     `balance.rs` 那几个管理接口同样要解信封（见那里）。
//!   - SSE 帧里的 `model` 是**上游内部名**（实测：请求
//!     `cline-free/deepseek-v4.1-flash`，回帧的 model 是 `deepseek/deepseek-v4.1-flash`
//!     —— 它背后走 OpenRouter 那类聚合，回的是真实承载模型的 slug）。
//!     因此 `sse_model_rewrite()` 必须为 **true**，否则客户端会看到
//!     `deepseek/deepseek-v4.1-flash` 这种它没请求过的名字。
//!
//! ── 错误分类（实测的三种错误体）──────────────────────────────
//!   - 401 `{"error":"Unauthorized: Please make sure you're using the latest
//!     version of Cline and re-authenticate your Cline account."}`
//!     → `TokenExpired`（刷新后同账号重试一次）；
//!   - 403 `{"error":{"code":"ENTITLEMENT_ERROR","message":"...not subscribed
//!     to required model plan"}}` 或 `{"error":{"code":"API_REQUEST_ERROR_CODE",
//!     "message":"... only available via Cline product surfaces"}}`
//!     → **`Fatal`（不是 QuotaLimited！）** —— 这两个都是「这个账号对**这个模型**
//!     没有权限」，换账号重试同样会失败（除非另一个账号有订阅），而把它当
//!     限额冷却会让账号被无意义地冷却一整个窗口。**403 一律不重试**是这里
//!     刻意的判断，理由见 `classify_error`；
//!   - 404 `{"error":"model not found"}` → `Fatal`（模型名不对，换账号无用）；
//!   - 429 → `QuotaLimited`（上游未给结构化恢复时间，但错误文案里带人话时长
//!     `"Try again in 17h 59m"` —— 解析成恢复时间戳填进 `reset_at`，见
//!     [`parse_inference_cap_reset_at`]；解析不出仍是 None，下游走 10 分钟兜底）。
//!
//! ── 500 `empty response content` 不是错误分类问题 ────────────
//! 实测：`max_tokens` 给小了（如 10）而模型要先输出一大段 reasoning 时，
//! 上游回 500 `{"error":"empty response content","success":false}`。
//! 这**不是**本适配器要处理的（我们不限制客户端的 max_tokens），记在这里
//! 是为了避免后来者把它误判成「上游不稳定」。
//!
//! ── 与另外几家的一致性 ──────────────────────────────────────
//!   - **不做 system 注入**：上游是 OpenAI 兼容网关，源实现从不改消息序列；
//!   - **不认「默认模型」**（`supports_default_model` = false）：客户端的
//!     `defaultModel` 是 workbuddy 语义的配置项，Cline 的默认是它自己目录里的
//!     第一个推荐模型 —— 注入一个 workbuddy 模型名再路由到 Cline 只会 404；
//!   - **没有环境变量旁路**（`CLINE_TOKEN` 这类约定不存在），因此
//!     `allows_anonymous_default_session` 保持默认 false；
//!   - **`supports_model_refresh` = true**：上游确实有目录接口
//!     （`GET {apiBase}/ai/cline/recommended-models`，实测**无需鉴权**即可拉）。
//!
//! ── 超时 ────────────────────────────────────────────────────
//! 与另外四家共用 `core::egress` 的出网点（connect 30s + read 600s，
//! **不设总超时**）—— LLM 是长回答场景，与 autoclaw 同一取舍。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件绝不 unwrap/expect/panic，取值走 Option 链与
//! `unwrap_or`；不持有任何锁（刷新回写在 await 之后才做，见 `persist_refresh`）。

use axum::http::HeaderMap;
use serde_json::Value;

use crate::server::core::account_store::AccountStore;
use crate::server::core::providers::content_block;
use crate::server::core::providers::adapter::{
    ChatRequestPlan, ModelRefreshOutcome, ProviderAdapter, UpstreamErrorClass,
};
use crate::server::core::providers::ProviderKind;
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::credentials;
use super::headers;
use super::models::{self, Pool};
use super::refresh;

/// 产品面标识头（**本集成的关键**，见模块头）。
///
/// 上游按这个头判定「调用方是不是 Cline 自家产品」，缺了它免费池模型一律 403。
/// 取 `cline-sdk` 而不是 `cline-cli`：实测 `cline-cli` 会让上游走另一条
/// 兼容路径（回 500 `empty response content`），`cline-sdk` 是干净通过的那个。
///
/// 转发侧的头取值**引用本常量**（`headers::DEFAULT_HEADERS` 里的
/// `X-CLIENT-TYPE` 一项），不另抄字面量：改这里就同时改了发出去的伪装头。
/// 登录 / 目录 / 余额三条非转发链（`login` / `models` / `balance`）直接用本
/// 常量拼头。
pub const CLIENT_TYPE: &str = "cline-sdk";

// 其余伪装头（User-Agent / X-CLIENT-VERSION / X-PLATFORM 等）的默认值集中在
// `headers` 模块（可在设置页逐键覆盖，见 `headers::DEFAULT_HEADERS`），
// 适配器只负责「固定头 + 覆盖合并」这一套顺序（见 `build_chat_headers`）。

/// Cline 适配器：**按池参数化**（同一套实现，两个实例）。
///
/// ── 为什么是两个实例而不是两个 impl ──────────────────────────
/// `cline-free` 与 `cline-pass` 是同一上游、同一协议、同一凭证格式，差别只有
/// 「收哪个池的模型」。把整份实现复制一份只为了换一个过滤条件，正是要在
/// 这里避免的事：凭证续期、错误分类、SSE 回写、余额查询任何一处改动都要记得
/// 改两遍，漏一次就是两个 provider 行为不一致。
///
/// 因此实现共用，实例由池决定（[`CLINE_FREE_ADAPTER`] / [`CLINE_PASS_ADAPTER`]，
/// `adapter::adapter_for` 按 kind 给出对应那一个）。
pub struct ClineAdapter {
    /// 这个实例服务哪个池（= 哪家 provider，见 `Pool::kind`）
    pool: Pool,
}

/// 免费池实例（provider id `cline-free`）
pub static CLINE_FREE_ADAPTER: ClineAdapter = ClineAdapter { pool: Pool::Free };

/// 订阅池实例（provider id `cline-pass`）
pub static CLINE_PASS_ADAPTER: ClineAdapter = ClineAdapter { pool: Pool::Pass };

impl ProviderAdapter for ClineAdapter {
    fn kind(&self) -> ProviderKind {
        self.pool.kind()
    }

    /// Cline 的模型清单（**本池那一批**，见 `models::list`）。
    ///
    /// 池是身份而不是账号属性，所以这里已经是收窄后的结果，不需要
    /// `advertise_models` 再收一层（那个方法的默认实现就是原样透传）。
    fn list_models(&self) -> Vec<Value> {
        models::list(self.pool)
    }

    /// 构造 `POST {apiBase}/chat/completions`。
    ///
    /// body **白名单重建**（移植自 cline-proxy 的 `buildUpstreamBody`，见
    /// [`build_upstream_body`]）：固定键注入默认值 + 白名单透传，不再是全量透传。
    ///
    /// `account` 是**会话形态**（`store.get_session_by_id` /
    /// `auth.get_current_session` 的返回值）：token 从 `auth.accessToken` 取，
    /// 与另外五家同一约定。桌面端账号（实时登录态）的 token 由账号存储的
    /// `session_from_record` 在构造会话时实时读入，因此这里不需要知道账号来源。
    fn build_chat_request(
        &self,
        account: &Value,
        body: &Value,
        _client_headers: &HeaderMap,
    ) -> Result<ChatRequestPlan, GatewayError> {
        let token = account
            .get("auth")
            .and_then(|auth| auth.get("accessToken"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if token.is_empty() {
            return Err(GatewayError::with_status(
                401,
                "Cline 账号缺少 accessToken，无法转发（请重新登录或导入桌面端登录态）",
            ));
        }
        // session_id 每请求生成（`sess_<毫秒>`，与官方 CLI 同形态）：进 body 的
        // `session_id` 与 `X-Task-ID` 头。401 刷新重试与 429 换号都会重走这里、
        // 自然拿到新值；`send_with_retry` 的退避重发复用同一 transport（头不重建），
        // 那是全项目一致的行为（workbuddy 的 X-Request-ID 同样如此）。
        let session_id = format!("sess_{}", crate::server::logging::now_ms());
        Ok(ChatRequestPlan::chat(
            format!("{}/chat/completions", credentials::API_BASE_URL),
            build_chat_headers(token, &session_id),
            build_upstream_body(body, &session_id),
        ))
    }

    /// 上游错误分类（判定依据全部来自实测，见模块头）。
    ///
    /// ── 403 为什么不重试（本适配器最需要说清的一个判断）─────────
    /// Cline 的 403 有两种业务码，**都表示「这个账号对这个模型没有权限」**：
    ///   - `ENTITLEMENT_ERROR`：账号没订阅该模型所属的套餐（如 pass 池模型）；
    ///   - `API_REQUEST_ERROR_CODE`：模型只对官方产品面开放（免费池）；
    /// 二者都是**账号 × 模型**维度的确定性拒绝，与「用太猛被限流」是两件事：
    ///   - 当作 `QuotaLimited` 会把这个账号冷却一整个窗口 —— 但换一个账号重试
    ///     同样会 403（除非那个账号恰好有订阅），冷却只是把「确定失败」变成
    ///     「一段时间内静默跳过」，用户更难看出真正原因；
    ///   - 当作 `Fatal` 则原样把 403 文案透给客户端，用户立刻看到
    ///     「not subscribed to required model plan」，知道该去订阅或换个模型。
    /// 因此这里归 `Fatal`。**注意这与 429 不同**：429 是真限流，归 `QuotaLimited`
    /// （换账号有意义）。
    ///
    /// ── `upstream_code` 取什么 ──────────────────────────────────
    /// 上游错误体是 `{"error":{"code":"ENTITLEMENT_ERROR","message":"..."}}`
    /// 的**嵌套**形态（code 是字符串，不是数字），而本项目的
    /// `UpstreamErrorClass` 的 `upstream_code` 是 `Option<i64>`（为 workbuddy 的
    /// 数字业务码设计的）。字符串码放不进去，硬编成数字只会是编造的假码 ——
    /// 因此一律 `None`，把可读的字符串码留在 `message` 里（`ErrorCode` 那层
    /// 文案本来就会带出上游原文）。
    fn classify_error(&self, status: u16, error_body: &Value) -> UpstreamErrorClass {
        let raw = upstream_message(error_body);
        let message = format!("上游返回 {status}: {raw}");
        if status == 401 {
            return UpstreamErrorClass::TokenExpired { message };
        }
        if status == 429 {
            return UpstreamErrorClass::QuotaLimited {
                // 上游不给结构化的恢复时间（实测错误体里没有任何时间字段），但
                // 错误文案里带人话时长（"Try again in 17h 59m"）—— 解析成恢复
                // 时间戳交给编排层（Some 且未过期时直接采用，跳过 10 分钟兜底）；
                // 解析不出维持 None（下游文案解析 → 10 分钟兜底的既有链路不变）。
                reset_at: parse_inference_cap_reset_at(error_body),
                message,
                upstream_code: None,
                status,
            };
        }
        // 内容策略拦截（审核文案）→ ContentBlocked：不罚账号，交给编排层换中性
        // 提示词重试一次 + 触发降级（见 `core::degrade`）
        content_block::classify_or_fatal(status, error_body, message, None)
    }

    /// 从发送体读出随请求上行的思考等级（请求日志「上游等级」列的采集口）。
    ///
    /// ── 为什么必须覆写默认实现 ──────────────────────────────────
    /// 采集（`payload::send_body` 的 `note_upstream_reasoning`）发生在
    /// `build_chat_request` **之前**，而本家「客户端没给档位 → 默认 high」
    /// 这一步是重建请求体时才写进字节的：不覆写，这一列会对这类请求恒为空，
    /// 而线上确实发了 high（该列的语义是「实际发出去的档位」）。
    ///
    /// ── 取值链必须与 build_upstream_body 同源 ────────────────────
    /// 默认实现读的是全项目展示用的**并集链**（`model_rules::read_client_level`，
    /// 五键），比本家真正认的两个键宽 —— 只写 `effort`（CatPaw 的键）的请求
    /// 会被显示成「发了 effort 那一档」，而本家实际发的是 high。这里改用
    /// [`explicit_reasoning_effort`]（与重建体同一个函数），显示即字节。
    ///
    /// 另：**不**套用默认实现「关闭思考不算随行档位」的过滤 —— 本家把客户端
    /// 写的 `off` / `none` 原样上行（移植语义，见 `build_upstream_body`），
    /// 它在字节里，如实显示才是这一列的本意。
    fn outbound_reasoning(&self, body: &Value) -> Option<String> {
        Some(
            explicit_reasoning_effort(body)
                .unwrap_or(DEFAULT_REASONING_EFFORT)
                .to_string(),
        )
    }

    /// 取可用 access token：**凭证快照 + 临期主动刷新**（10 分钟窗口，
    /// 见 `credentials::PROACTIVE_REFRESH_MARGIN_MS`）。
    ///
    /// 刷新结果回写 accounts.json（桌面端来源按设计不回写，见 `refresh::persist_refresh`）。
    fn ensure_access_token<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let credentials = refresh::ensure_fresh(store, account_id, false).await?;
            Ok(credentials.bearer_token())
        })
    }

    /// 401（token 被上游拒绝）后的**强制**刷新：不看临期窗口，直接续期。
    ///
    /// 必须覆盖默认实现：`ensure_access_token` 只在临期时刷新，而 401 完全可能
    /// 发生在一个时间上还很新的 token 上（服务端侧失效、会话被顶下线）。
    /// 此时只调 ensure 会拿回同一个被拒的 token，「刷新后重试一次」就退化成
    /// 「用同一个坏 token 再打一次」。
    ///
    /// 没 refreshToken 时 `ensure_fresh(force=true)` 会返回 400 —— 那是对的：
    /// 编排层据此把「无法续期」如实告诉客户端，而不是静默重试。
    fn refresh_access_token<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let credentials = refresh::ensure_fresh(store, account_id, true).await?;
            Ok(credentials.bearer_token())
        })
    }

    /// 拉取远程模型目录（`GET {apiBase}/ai/cline/recommended-models`）。
    ///
    /// ── 无鉴权也能拉（实测）─────────────────────────────────────
    /// 因此这里**不依赖账号状态**：一个账号都没有时也能刷新目录（与另外几家
    /// 「没登录态就跳过」的取向不同 —— 那几家的目录接口要凭证）。这不违反
    /// 「刷新失败不返回错误」的约定，只是少了一个失败原因。
    ///
    /// `force` 参数的语义与另外几家一致（`true` = 用户手动点了刷新），
    /// 但本家**没有 TTL 早退**：上游接口轻量且无鉴权，缓存的价值主要是
    /// 「避免每次 /v1/models 都打一次上游」，那由更上层的调用频率控制，
    /// 不需要在这里再压一层时间窗（多一个旋钮就多一处不一致）。
    fn refresh_models<'a>(
        &'a self,
        _store: &'a AccountStore,
        // Cline 的目录接口无鉴权、清单是全局的（见模块头），没有「用哪个账号
        // 去拉」这一维 —— 弹窗里那一列对它显示为空，参数照收不用
        _account_id: &'a str,
        _force: bool,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = ModelRefreshOutcome> + Send + 'a>,
    > {
        Box::pin(async move {
            match models::refresh().await {
                Ok(count) => {
                    // 清单变了就补种默认映射（新增的带前缀模型自动获得友好名）。
                    // **只种本池那一批**：远程接口一次拉回两个池，但种子按
                    // (provider, id) 记账，越界种到另一个池的 id 会让那家的
                    // seeded 里出现不属于它的键。
                    if let Some(summary) = seed_defaults(self.pool) {
                        logging::log("[Models]", &summary);
                    }
                    ModelRefreshOutcome::refreshed(count)
                }
                Err(reason) => {
                    logging::verbose(
                        "[Models]",
                        &format!("Cline 模型目录刷新失败：{reason}"),
                    );
                    ModelRefreshOutcome::failed(format!("Cline 目录刷新失败：{reason}"))
                }
            }
        })
    }

    /// Cline **有**远程模型目录（`GET {apiBase}/ai/cline/recommended-models`，实测）。
    fn supports_model_refresh(&self) -> bool {
        true
    }

    /// Cline 的目录清单是全局的、接口无鉴权：刷新不走「账号」维度
    /// （与 `refresh_models` 忽略 account_id 配对，见那边的注释）
    fn refresh_uses_account(&self) -> bool {
        false
    }

    /// Cline **没有**「默认模型」概念：不注入 workbuddy 语义的默认模型名
    /// （理由见模块头）。
    fn supports_default_model(&self) -> bool {
        false
    }

    /// SSE 帧的 model 名回写：**要写**。
    ///
    /// 实测：请求 `cline-free/deepseek-v4.1-flash`，回帧的 model 是
    /// `deepseek/deepseek-v4.1-flash`（上游背后走聚合通道，回的是真实承载
    /// 模型的 slug）。客户端按自己请求的名字识别响应，不回写会让它看到
    /// 一个它从没请求过的模型名。
    ///
    /// ── 关于思考帧的字段名（**本家不回写、也不改写，这是对的**）────
    /// Cline 的思考增量用的是 OpenRouter 那套 **`delta.reasoning`**
    /// （外加一个 `reasoning_details` 数组），而本项目 SSE 合并器认的是
    /// **`reasoning_content`**（workbuddy / 小浣熊的形态）。
    ///
    /// 两者对不上**不影响透传**：合并器的判据是「有 `reasoning_content`
    /// 且没有 content/tool_calls」，Cline 的帧不命中，于是走「其余事件 →
    /// 原样透传」那一支 —— 客户端收到的是 Cline 自己的 `reasoning` 字段，
    /// 逐字节未改。这正好是我们要的：**不替客户端翻译上游的字段名**，
    /// 那属于客户端 SDK 的事，网关擅自改名反而会让它认不出。
    /// 代价只是少了「把碎思考帧攒成一块」这一个优化（Cline 的思考帧本来就
    /// 比 workbuddy 粗），不影响正确性。
    fn sse_model_rewrite(&self) -> bool {
        true
    }

    /// Cline 支持主动刷新（`POST {apiBase}/auth/refresh`，实测）。
    fn supports_refresh(&self) -> bool {
        true
    }

    /// 临期判定：取凭证快照，用凭证自己的 `is_expiring()`（10 分钟窗口）与
    /// `can_refresh()` 判一次。
    ///
    /// **`can_refresh()` 是必需的**：用户手填 access token 而不给 refreshToken
    /// 时，凭证天然不可刷新 —— 少这道判定会被维护任务每轮都算成「待刷新」，
    /// 然后稳定失败并往日志里灌错误（与 AutoClaw 的 `openclaw.json` 来源同一
    /// 取舍）。
    ///
    /// ── 判不出过期时间时的兜底（本次修复）───────────────────────
    /// 凭证里既没有可解析的 `expiresAt`、access token 也不是带 `exp` 的 JWT
    /// （例如用户粘贴了 opaque token）时，`is_expiring()` 恒为 false ——
    /// 于是**主动续期链路完全静默失效**，只能等 401。官方 CLI 对这种情况用
    /// 25 分钟周期兜底刷（见 `credentials::UNKNOWN_EXP_REFRESH_INTERVAL_MS`），
    /// 这里在维护任务这条路上对齐同一语义。
    ///
    /// 只在**本方法**兜底、不在 `is_expiring()` 里改：后者被转发链路的
    /// `ensure_fresh` 每请求调用一次，在那里返回 true 会让每个请求都打一次
    /// 续期接口。维护任务每 10 分钟才问一次，节流后实际每 25 分钟才刷一次。
    fn credentials_expiring(&self, store: &AccountStore, account_id: &str) -> bool {
        if account_id.is_empty() {
            return false;
        }
        match refresh::snapshot(store, account_id) {
            Ok(credentials) => {
                if !credentials.can_refresh() {
                    return false;
                }
                if credentials.is_expiring() {
                    return true;
                }
                // 有续期手段、但判不出过期时间 → 按官方口径定期兜底刷一次
                credentials.expires_at.is_none()
                    && credentials::unknown_exp_refresh_due(
                        account_id,
                        crate::server::logging::now_ms(),
                    )
            }
            Err(_) => false,
        }
    }

    /// Cline 有 credit 余额与订阅概念（`cline/balance.rs`）。
    fn supports_usage(&self) -> bool {
        true
    }

    /// 查余额 + 用量（`GET /users/{id}/balance` + `/users/me/plan`）。
    ///
    /// ── 为什么不在这里触发刷新 ──────────────────────────────────
    /// 与另外几家同一分工：适配器只提供「怎么查」，401 之后的刷新重试由
    /// `api::accounts::query_usage_inner` 统一处置（它调 `refresh_access_token`）。
    /// 本家 `supports_refresh = true`，因此那条链路对它是有效的。
    fn query_usage<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Value, GatewayError>> + Send + 'a>,
    > {
        Box::pin(async move { super::balance::query_usage(store, account_id).await })
    }
}

/// 上游错误体 → 可读文案（本项目的归一化只保证 `message` 键，但 Cline 的
/// 两种形态是 `{"error":"..."}` 与 `{"error":{"code":..,"message":..}}`）。
///
/// `error_body` 是**已归一化**的错误对象（见 trait 契约：至少含 `code` 与
/// `message`）。归一化层取的是 `message` / `msg` / `error.message`，
/// 因此嵌套形态的 message 已经在 `message` 里了；纯字符串形态
/// （`{"error":"Unauthorized: ..."}`）取不到 `error.message`，归一化层会把
/// 整个 JSON 的截断原文放进去 —— 这里做最后一层适配：优先 `message`，
/// 空则回落到 `error` 的字符串形态。
fn upstream_message(error_body: &Value) -> String {
    if let Some(text) = error_body
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return text.to_string();
    }
    if let Some(text) = error_body
        .get("error")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return text.to_string();
    }
    "上游错误".to_string()
}

/// 对本池清单补种默认映射（刷新落地后调用）。
///
/// 用当前清单（含静态兜底）而不是只传远程结果：静态兜底里也有带前缀的模型，
/// 一次种完更省事，且种子本身幂等（种过的跳过）。
///
/// ── 为什么不需要「按用户所在的池排序」了（拆分后删掉的一段）────
/// 早先两池共用一个 provider，同名模型（`deepseek-v4.1-flash` 两池都有）的
/// 短名归属由**账号里有哪个池**决定 —— 只加了免费池账号的用户必须拿到
/// 免费池那条，否则短名会稳定 403（`ENTITLEMENT_ERROR`）。那套排序连同
/// 「池是账号属性」这个前提一起退场了。
///
/// 现在两家各记各的 seeded，先刷到的池拿到「去前缀」那条；两家的短名都由
/// `model_rules::EXTRA_ALIASES` 点名补挂，因此**无论哪家先刷，短名都能路由到
/// 两个池**（各有一条映射进候选链，发送名跟着实际承载的 provider 走，见
/// `catalog::wire_target_for_provider` 的 ②）。短名最终落在谁家只影响
/// 「候选链里哪个在前」，而那由账号优先级与候选链的既有规则决定，不再是
/// 一个藏在种子里、随刷新顺序漂移的隐规则。
pub(crate) fn seed_defaults(pool: Pool) -> Option<String> {
    models::seed_cline_defaults(pool, &models::ids_of(pool))
}

// ─── 上游请求头（固定四个 + 伪装头覆盖合并）──────────────────

/// 构造上游请求头：固定四个（协议必需 + 动态任务标识）之后，伪装头
/// （默认值 + 设置页逐键覆盖，见 `headers` 模块）**覆盖式**合并 ——
/// 与 cline-proxy 的 `clineHeaders` 同一顺序：后写赢，用户可以覆盖任何
/// 一个默认头，也可以新增默认清单之外的自定义头。**固定四个头在配置侧
/// 就被挡住**（覆盖表里写它们由 `api::cline_headers` 返回 400，见那里的
/// `FIXED_HEADERS`），这里的 upsert 只是同一条不变量的执行侧。
fn build_chat_headers(token: &str, session_id: &str) -> Vec<(String, String)> {
    let mut headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "text/event-stream".to_string()),
        (
            "Authorization".to_string(),
            format!("Bearer {}", credentials::ensure_token_prefix(token)),
        ),
        // 任务标识：与 body 的 `session_id` 同值（官方 CLI 的形态）
        ("X-Task-ID".to_string(), session_id.to_string()),
    ];
    for (key, value) in headers::effective_headers() {
        upsert_header(&mut headers, key, value);
    }
    headers
}

/// 覆盖式写头：同名（HTTP 头名大小写不敏感）**替换**而非追加 ——
/// reqwest 对 `.header()` 的重复调用是追加出多值头，伪装头的覆盖语义
/// 必须是替换（与 cline-proxy `http.Header.Set` 同语义）。
fn upsert_header(headers: &mut Vec<(String, String)>, key: String, value: String) {
    if let Some(slot) = headers
        .iter_mut()
        .find(|(name, _)| name.eq_ignore_ascii_case(&key))
    {
        slot.1 = value;
    } else {
        headers.push((key, value));
    }
}

// ─── 上游请求体（白名单重建）────────────────────────────────

/// `max_tokens` 缺失时的默认值（cline-proxy 同值）。
const DEFAULT_MAX_TOKENS: i64 = 128_000;

/// `reasoning_effort` 缺失时的默认档位（cline-proxy 同值）。
const DEFAULT_REASONING_EFFORT: &str = "high";

/// 客户端**显式**给的思考档位（`reasoning_effort` → 驼峰 `reasoningEffort`，
/// 非空才算）：[`build_upstream_body`] 的默认值注入与
/// [`ClineAdapter::outbound_reasoning`] 的显示共用的**同一条取值链** ——
/// 两处各抄一份迟早分叉，而分叉的表现是「日志里说的与发出去的不是一个值」。
fn explicit_reasoning_effort(body: &Value) -> Option<&str> {
    let object = body.as_object()?;
    let text = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
    };
    text("reasoning_effort").or_else(|| text("reasoningEffort"))
}

/// 白名单透传键（照抄 cline-proxy 的 `passThroughKeys`）：客户端传了才带上。
///
/// 注意这份清单就是**全部**能上行的可选键：入站协议层（`core::protocol`）产出的
/// 顶层键里，Responses 的 `service_tier` 不在其中，会被这一层静默剔除（照抄
/// 参照实现的口径 —— 那只对 OpenAI 自己的网关有意义）。
const PASS_THROUGH_KEYS: &[&str] = &[
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "functions",
    "function_call",
    "temperature",
    "top_p",
    "top_k",
    "stop",
    "presence_penalty",
    "frequency_penalty",
    "response_format",
    "user",
    "n",
    "logit_bias",
    "seed",
    "logprobs",
    "top_logprobs",
    "stream_options",
    "metadata",
];

/// 上游请求体的**白名单重建**（移植自 cline-proxy 的 `buildUpstreamBody`）。
///
/// body 传进来时已经是「待发送的定稿」—— 模型名改写、提示词、脱敏都做完了
/// （见 `core::upstream::payload` 的分层顺序），这里按 cline-proxy 的口径把它
/// 收敛成上游认识的形状：
///   - **固定键**：`model`（**原样保留** —— 含池前缀 `cline-free/...`，那是
///     上游的通道选择器，绝不能剥，见 `models.rs`）、`max_tokens`（客户端
///     `max_tokens` → `max_completion_tokens` → 默认 128000）、`session_id`
///     （每请求生成的 `sess_<毫秒>`，与 `X-Task-ID` 头同值）、
///     `reasoning_effort`（客户端 `reasoning_effort` / 驼峰 `reasoningEffort`
///     显式值优先，否则默认 `high`）；
///   - **保留**：`messages` 与 `stream`（上游恒为流式，编排层已强制写入）；
///   - **白名单透传**：[`PASS_THROUGH_KEYS`] 里客户端传了的键原样带上，
///     白名单之外的键不上游（上游是 OpenAI 兼容网关，未知键没有意义）。
///
/// 客户端显式给了小的 `max_tokens` 时不强制抬高 —— 上游在 max_tokens 太小
/// 而模型要先输出一大段 reasoning 时会回 500 `empty response content`，
/// 那是客户端自己的取舍（见模块头「500」一节）。但 `≤ 0` 的值不算「显式小
/// 值」、按缺失处理（走 128000 默认）：参照实现会把 0 / 负数原样发出去，
/// 那对上游只会是参数错误，没有「用户本意」可谈。
fn build_upstream_body(body: &Value, session_id: &str) -> Value {
    let object = body.as_object();
    let get = |key: &str| object.and_then(|map| map.get(key));

    let mut out = serde_json::Map::new();
    if let Some(model) = get("model") {
        out.insert("model".to_string(), model.clone());
    }
    // 数值统一按 f64 读（JSON 数字在手写体里可能是 8192.0 这类浮点形态）
    let max_tokens = get("max_tokens")
        .and_then(Value::as_f64)
        .or_else(|| get("max_completion_tokens").and_then(Value::as_f64))
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|value| value as i64)
        .unwrap_or(DEFAULT_MAX_TOKENS);
    out.insert("max_tokens".to_string(), Value::from(max_tokens));
    out.insert("session_id".to_string(), Value::from(session_id));
    out.insert(
        "reasoning_effort".to_string(),
        Value::from(explicit_reasoning_effort(body).unwrap_or(DEFAULT_REASONING_EFFORT)),
    );
    if let Some(messages) = get("messages") {
        out.insert("messages".to_string(), messages.clone());
    }
    if let Some(stream) = get("stream") {
        out.insert("stream".to_string(), stream.clone());
    }
    for key in PASS_THROUGH_KEYS {
        if let Some(value) = get(key) {
            out.insert((*key).to_string(), value.clone());
        }
    }
    Value::Object(out)
}

// ─── 429 的人话时长解析（移植自 cline-proxy）─────────────────

/// 从 Cline 429 错误体解析 `"Try again in 17h 59m"` 形式的等待时长，
/// 返回**恢复时间戳**（毫秒；`now + 时长`）。解析不出返回 None。
///
/// 移植自 cline-proxy 的 `parseInferenceCapDuration`：在错误体序列化后的
/// JSON 全文里找 `Try again in`（等价于扫原始 body —— 归一化层不会改写
/// 文案本体），取到下一个 `"` / 换行 / `}` 为止的片段按人话时长解析。
/// 命中时编排层（`rotate::mark_account_limited`）直接采用这个时间戳、
/// 跳过 10 分钟兜底 —— Cline 免费额度撞限的真实冷却可达小时级，只按
/// 兜底冷却会反复打无效请求。
fn parse_inference_cap_reset_at(error_body: &Value) -> Option<i64> {
    let text = serde_json::to_string(error_body).ok()?;
    let duration_ms = parse_try_again_ms(&text)?;
    Some(crate::server::logging::now_ms() + duration_ms)
}

/// `Try again in` 之后的片段（到下一个 `"` / 换行 / `}` 为止）→ 毫秒。
fn parse_try_again_ms(text: &str) -> Option<i64> {
    const MARKER: &str = "Try again in";
    let index = text.find(MARKER)?;
    let rest = &text[index + MARKER.len()..];
    let end = rest.find(['"', '\n', '\r', '}']).unwrap_or(rest.len());
    parse_human_duration_ms(rest[..end].trim())
}

/// 人话时长 → 毫秒。支持 `d` / `h` / `m` / `ms` / `s` 任意组合
/// （`"17h 59m"`、`"1d 2h 30m"`、`"30s"`）。
///
/// 字节扫描移植自 cline-proxy 的 `parseHumanDuration`（不用 regex，避开
/// 中文字符边界的 panic 风险，口径与 `errors::parse_quota_reset_at` 一致）：
/// 未知字符重置累积（半截 token 不算数），总时长 ≤ 0 返回 None。
fn parse_human_duration_ms(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let mut total_ms: i64 = 0;
    let mut num: i64 = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'0'..=b'9' => {
                num = num.saturating_mul(10).saturating_add((bytes[index] - b'0') as i64);
            }
            b'd' => {
                total_ms = total_ms.saturating_add(num.saturating_mul(86_400_000));
                num = 0;
            }
            b'h' => {
                total_ms = total_ms.saturating_add(num.saturating_mul(3_600_000));
                num = 0;
            }
            // "ms" 必须在裸 `m` 之前判（两个字符是一个单位）
            b'm' if bytes.get(index + 1) == Some(&b's') => {
                total_ms = total_ms.saturating_add(num);
                num = 0;
                index += 1;
            }
            b'm' => {
                total_ms = total_ms.saturating_add(num.saturating_mul(60_000));
                num = 0;
            }
            b's' => {
                total_ms = total_ms.saturating_add(num.saturating_mul(1_000));
                num = 0;
            }
            // 空格只是分隔符；其余未知字符重置累积（半截 token 不算数）
            b' ' => {}
            _ => num = 0,
        }
        index += 1;
    }
    (total_ms > 0).then_some(total_ms)
}

#[cfg(test)]
mod tests {
    //! 请求体白名单重建 / 伪装头合并 / 429 人话时长解析的纯函数测试
    //! （移植 cline-proxy 行为时的对账基准，均不起网络、不依赖全局状态）。
    use serde_json::json;

    use super::*;

    // ── build_upstream_body：默认值注入 ──

    #[test]
    fn a_bare_body_gets_session_id_max_tokens_and_effort_defaults() {
        let out = build_upstream_body(&json!({"model": "cline-free/deepseek-v4.1-flash", "messages": []}), "sess_1");
        assert_eq!(out["model"], "cline-free/deepseek-v4.1-flash");
        assert_eq!(out["session_id"], "sess_1");
        assert_eq!(out["max_tokens"], 128_000);
        assert_eq!(out["reasoning_effort"], "high");
        assert!(out.get("stream").is_none());
        assert!(out.get("messages").is_some());
    }

    #[test]
    fn client_explicit_values_win_over_the_defaults() {
        let body = json!({
            "model": "cline-free/x",
            "max_tokens": 4096.0,
            "max_completion_tokens": 8192.0,
            "reasoningEffort": "low",
            "stream": true,
        });
        let out = build_upstream_body(&body, "sess_2");
        // max_tokens 优先于 max_completion_tokens；驼峰的 effort 也认
        assert_eq!(out["max_tokens"], 4096);
        assert_eq!(out["reasoning_effort"], "low");
        assert_eq!(out["stream"], true);
        // 驼峰键本身不上游
        assert!(out.get("reasoningEffort").is_none());
        assert!(out.get("max_completion_tokens").is_none());
    }

    #[test]
    fn keys_outside_the_whitelist_do_not_reach_upstream() {
        let body = json!({
            "model": "cline-free/x",
            "tools": [{"type": "function"}],
            "temperature": 0.5,
            "service_tier": "default",
            "some_client_junk": {"a": 1},
        });
        let out = build_upstream_body(&body, "sess_3");
        assert!(out.get("tools").is_some());
        assert_eq!(out["temperature"], 0.5);
        assert!(out.get("service_tier").is_none());
        assert!(out.get("some_client_junk").is_none());
    }

    // ── build_chat_headers：固定头 + 覆盖合并 ──

    #[test]
    fn fixed_headers_come_first_and_disguise_headers_follow() {
        let headers = build_chat_headers("tok", "sess_9");
        let names: Vec<&str> = headers.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(&names[..4], &["Content-Type", "Accept", "Authorization", "X-Task-ID"][..]);
        assert!(names.contains(&"X-CLIENT-TYPE"));
        assert!(names.contains(&"X-PLATFORM"));
        let task_id = headers.iter().find(|(key, _)| key == "X-Task-ID");
        assert_eq!(task_id.map(|(_, value)| value.as_str()), Some("sess_9"));
    }

    #[test]
    fn a_same_name_header_is_replaced_not_appended() {
        // 覆盖合并的语义：同名（大小写不敏感）替换。追加的话 reqwest 会发出
        // 多值头（`headers_mut().append()`），上游看到的是两个值
        let mut headers = vec![("X-PLATFORM".to_string(), "terminal".to_string())];
        upsert_header(&mut headers, "x-platform".to_string(), "extension".to_string());
        assert_eq!(headers, vec![("X-PLATFORM".to_string(), "extension".to_string())]);
        // 默认清单之外的新头是追加
        upsert_header(&mut headers, "X-Custom-Trace".to_string(), "abc".to_string());
        assert_eq!(headers.len(), 2);
    }

    #[test]
    fn the_reported_upstream_level_matches_what_is_actually_sent() {
        // 客户端没给档位：线上发的是注入的默认 high，日志这一列也要报 high
        let bare = json!({ "model": "cline-free/x" });
        assert_eq!(
            CLINE_FREE_ADAPTER.outbound_reasoning(&bare).as_deref(),
            Some("high")
        );
        assert_eq!(
            build_upstream_body(&bare, "sess_4")["reasoning_effort"],
            "high"
        );
        // 客户端显式给了：两边都跟着客户端走（含本家原样上行的 off）
        let explicit = json!({ "model": "cline-free/x", "reasoningEffort": "low" });
        assert_eq!(
            CLINE_FREE_ADAPTER.outbound_reasoning(&explicit).as_deref(),
            Some("low")
        );
        assert_eq!(build_upstream_body(&explicit, "sess_5")["reasoning_effort"], "low");
    }

    // ── 429 人话时长 ──

    #[test]
    fn try_again_durations_in_all_unit_combinations_are_parsed() {
        assert_eq!(parse_human_duration_ms("17h 59m"), Some(17 * 3_600_000 + 59 * 60_000));
        assert_eq!(parse_human_duration_ms("1d 2h 30m"), Some(86_400_000 + 2 * 3_600_000 + 30 * 60_000));
        assert_eq!(parse_human_duration_ms("30s"), Some(30_000));
        assert_eq!(parse_human_duration_ms("17h"), Some(17 * 3_600_000));
        assert_eq!(parse_human_duration_ms("500ms"), Some(500));
        assert_eq!(parse_human_duration_ms("no duration here"), None);
        assert_eq!(parse_human_duration_ms(""), None);
    }

    #[test]
    fn the_wait_text_is_found_inside_a_json_error_body() {
        let body = json!({"error": {"code": "INFERENCE_CAP_ERROR", "message": "Rate limited. Try again in 17h 59m."}});
        let text = serde_json::to_string(&body).unwrap_or_default();
        // 引号/句点截断不吞字：`59m.` 的句点落在单位字母之后，m 已结算
        assert_eq!(parse_try_again_ms(&text), Some(17 * 3_600_000 + 59 * 60_000));
    }

    #[test]
    fn a_429_with_a_parseable_wait_yields_a_future_reset_at() {
        let body = json!({"error": "Too many requests. Try again in 1h 30m"});
        let before = crate::server::logging::now_ms();
        let classified = CLINE_FREE_ADAPTER.classify_error(429, &body);
        let UpstreamErrorClass::QuotaLimited { reset_at, .. } = classified else {
            panic!("429 应归类为 QuotaLimited");
        };
        let reset_at = reset_at.expect("带时长文案的 429 应给出恢复时间");
        assert!(reset_at >= before + 90 * 60_000);
    }

    #[test]
    fn a_429_without_wait_text_still_falls_back_to_none() {
        let body = json!({"error": "Too many requests"});
        let classified = CLINE_FREE_ADAPTER.classify_error(429, &body);
        let UpstreamErrorClass::QuotaLimited { reset_at, .. } = classified else {
            panic!("429 应归类为 QuotaLimited");
        };
        assert_eq!(reset_at, None);
    }
}
