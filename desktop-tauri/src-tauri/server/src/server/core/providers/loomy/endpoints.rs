//! Loomy（讯飞）上游地址与**客户端内置签名对**（环境变量可覆盖）。
//!
//! ── 上游长什么样（逆向来源：`D:\Program Files\Loomy\resources\app.asar`）──
//! Loomy 是讯飞系 Electron 客户端，三套平面各一个域：
//!
//! ```text
//!   账号 CAccount     https://account.xfinfr.com        （HMAC-SHA1 签名头，见 sign.rs）
//!   集成网关（积分）  https://loomyad.xunfei.cn          （明文 session 放 `token` 头）
//!   模型网关          {集成网关}/api/v1                  （OpenAI 兼容，token + Bearer 双头）
//! ```
//!
//! 前两个值来自安装包内加密的 `.env.prod`（`VITE_XFYUN_BASE_URL` 与
//! `LOOMY_POINTS_BASE_URL`，用客户端自带的 `env-file-crypto.js` 解出）；
//! 模型网关没有独立配置 —— 客户端 `bundled-resources.js` 的
//! `_applyManagedProviderRuntimeConfig` 把它拼成「积分基址 + `/api/v1`」。
//!
//! ── 签名对为什么内置 ────────────────────────────────────────
//! CAccount 的每个请求都要用**客户端内置的** AccessKey 对签 HMAC-SHA1
//! （`2thryb…` / `zsak6…`，随安装包分发、不是用户账号凭证），没有它登录接口
//! 一个都调不通。这与 `accio::endpoints::DEFAULT_APP_KEY` 同一处置：内置为默认值，
//! 环境变量可覆盖（`LOOMY_XFYUN_*`），改动不需要重新编译。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

/// 账号（CAccount）默认基址（安装包 `.env.prod` 的 `VITE_XFYUN_BASE_URL`）
pub const DEFAULT_XFYUN_BASE_URL: &str = "https://account.xfinfr.com";

/// 集成网关默认基址（安装包 `.env.prod` 的 `LOOMY_POINTS_BASE_URL`）。
/// 积分、每日登录刷新（签到）与模型网关都挂在这个域下。
pub const DEFAULT_INTEGRATION_BASE_URL: &str = "https://loomyad.xunfei.cn";

/// 客户端内置 AccessKeyId（安装包 `.env.prod` 的 `VITE_XFYUN_ACCESS_KEY_ID`）
pub const DEFAULT_ACCESS_KEY_ID: &str = "2thryby66wxi53sk";

/// 客户端内置 AccessKeySecret（同上 `_SECRET`）
pub const DEFAULT_ACCESS_KEY_SECRET: &str = "zsak6eadrbawz683wf5r3m2snrwj868r";

/// 客户端 AppID（请求信封 `base.appid`；`GM3LOOMY` 是 Loomy 桌面端的固定值）
pub const DEFAULT_APP_ID: &str = "GM3LOOMY";

/// 读环境变量覆盖（空值视为未设置），去掉尾部斜杠
fn env_override(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
}

/// 账号（CAccount）基址：`LOOMY_XFYUN_BASE_URL` 可覆盖
pub fn xfyun_base_url() -> String {
    env_override("LOOMY_XFYUN_BASE_URL").unwrap_or_else(|| DEFAULT_XFYUN_BASE_URL.to_string())
}

/// 集成网关基址：`LOOMY_POINTS_BASE_URL`（沿用客户端自己的变量名）可覆盖
pub fn integration_base_url() -> String {
    env_override("LOOMY_POINTS_BASE_URL")
        .unwrap_or_else(|| DEFAULT_INTEGRATION_BASE_URL.to_string())
}

/// 模型网关基址：`{集成网关}/api/v1`（与客户端 `bundled-resources.js` 同款拼接）
pub fn model_base_url() -> String {
    format!("{}/api/v1", integration_base_url())
}

/// 签名用 AccessKeyId：`LOOMY_XFYUN_ACCESS_KEY_ID` 可覆盖
pub fn access_key_id() -> String {
    env_override("LOOMY_XFYUN_ACCESS_KEY_ID").unwrap_or_else(|| DEFAULT_ACCESS_KEY_ID.to_string())
}

/// 签名用 AccessKeySecret：`LOOMY_XFYUN_ACCESS_KEY_SECRET` 可覆盖
pub fn access_key_secret() -> String {
    env_override("LOOMY_XFYUN_ACCESS_KEY_SECRET")
        .unwrap_or_else(|| DEFAULT_ACCESS_KEY_SECRET.to_string())
}

/// 请求信封 `base.appid`：`LOOMY_XFYUN_APP_ID` 可覆盖
pub fn app_id() -> String {
    env_override("LOOMY_XFYUN_APP_ID").unwrap_or_else(|| DEFAULT_APP_ID.to_string())
}
