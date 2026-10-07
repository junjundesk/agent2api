//! Loomy 的「每日签到」= **每日首次登录刷新赠送积分**（逆向确认）。
//!
//! ── 上游是什么（安装包全包检索 + `points-service.js` 对照）────
//! Loomy **没有独立的签到 / 领取接口**（客户端全包检索「签到 / checkin /
//! 每日任务」零命中）。它每天把「每日赠送积分」刷回 5000 的机制是：
//!
//! ```text
//! POST {集成网关}/api/v1/points/first-login      header: token: <session>
//! body: {}（可带 inviteCode / deviceId / channel，签到场景都不需要）
//!   → {code:"000000", desc, data:{…}}
//! ```
//!
//! 客户端**每次登录成功后**都会调它（短信登录与微信绑定两条路径都调），
//! 服务端以「当天是否第一次」决定是否把赠送额度刷回 5000 —— 也就是说
//! 「签到」的触发点是登录，而这里把同一个调用接进自动签到框架，让长期不登录
//! 的账号也能每天把赠送积分续上。
//!
//! ── 判成功的口径（与别家对齐）───────────────────────────────
//! `{success, msg}`：HTTP 通了且业务码 `000000` 才算 success。重复调用是
//! **按天幂等**的（服务端当天已刷过就原样返回成功），所以「今天已刷过」不算失败 ——
//! 与 AutoClaw「今天已签到 → success:false」不同，这里没有可区分的返回字段，
//! 如实报成功并说明「每日赠送积分已刷新」。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::account_store::AccountStore;
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::client;
use super::credentials;

/// 每日登录刷新路径（相对集成网关）
const FIRST_LOGIN_PATH: &str = "/api/v1/points/first-login";

/// Loomy 账号的每日积分刷新（自动签到框架的 claim）。
///
/// 返回 `{success, msg}` —— 与 WorkBuddy / 小浣熊 / AutoClaw / Qoder 的 claim
/// 同一形状（`billing::checkin` 的汇总只认这两个字段）。
pub async fn claim_daily_login(
    store: &AccountStore,
    account_id: &str,
) -> Result<Value, GatewayError> {
    let record = store.loomy_account_record(account_id);
    if !account_id.is_empty() && record.is_none() {
        return Err(GatewayError::with_status(
            404,
            format!("Loomy 账号 {account_id} 不存在（请重新添加）"),
        ));
    }
    let credentials = credentials::from_record(record.as_ref())?;

    let payload = client::token_request(
        "POST",
        FIRST_LOGIN_PATH,
        &credentials.session,
        Some(&json!({})),
        "每日积分刷新",
    )
    .await?;

    let code = client::business_code(&payload);
    if client::is_auth_error_code(&code) {
        return Err(GatewayError::with_status(
            401,
            "Loomy 登录态已失效，无法刷新每日积分（请重新登录）",
        ));
    }
    if code != client::SUCCESS_CODE {
        let message = client::upstream_message(&payload);
        let message = if message.trim().is_empty() {
            format!("每日积分刷新失败（上游 code={code}）")
        } else {
            format!("每日积分刷新失败：{message}")
        };
        logging::log("[Checkin]", &format!("⚠️ {message}"));
        return Ok(json!({ "success": false, "msg": message }));
    }

    // 响应里可能带今日的赠送额度（字段名未在客户端硬编码，取到就播报）
    let daily_hint = payload
        .pointer("/data/dailyBalance")
        .and_then(Value::as_i64)
        .or_else(|| payload.pointer("/data/data/dailyBalance").and_then(Value::as_i64));
    let msg = match daily_hint {
        Some(balance) => format!("每日赠送积分已刷新（当前 {balance}）"),
        None => "每日赠送积分已刷新".to_string(),
    };
    Ok(json!({ "success": true, "msg": msg }))
}
