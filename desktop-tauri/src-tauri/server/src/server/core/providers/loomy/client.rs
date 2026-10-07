//! Loomy 两种出站请求的薄封装：签名 POST（CAccount）与 token 请求（集成网关）。
//!
//! ── 为什么收在一处 ──────────────────────────────────────────
//! 三处消费者（login / checkin / balance / models）共用同一套超时、错误翻译与
//! 「业务码 → 人话」口径；各写一份会让「哪个码算登录失效」这类判据分叉。
//!
//! ── 业务码的口径（实测 + 客户端源码对照）────────────────────
//! CAccount 与集成网关都用 HTTP 200 + `{code, desc, data}` 表达业务结果：
//!   - `000000`：成功；
//!   - `020002` / `100002`：登录态无效（客户端 `AUTH_ERROR_CODES`，两个码分别来自
//!     CAccount 与业务网关）；
//!   - 其余：业务失败，`desc` 是上游文案。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::Value;

use crate::server::core::auth_http::send_raw;
use crate::server::errors::GatewayError;

use super::endpoints;
use super::sign;

/// 登录 / 积分接口的超时（与客户端 30 秒口径对齐，略收紧到 20 秒）
const REQUEST_TIMEOUT_MS: u64 = 20_000;

/// 登录态失效业务码（客户端 `AUTH_ERROR_CODES`）
pub const AUTH_ERROR_CODES: [&str; 2] = ["020002", "100002"];

/// 成功码
pub const SUCCESS_CODE: &str = "000000";

/// 读取响应体里的业务码（字符串；缺失时为空串）
pub fn business_code(payload: &Value) -> String {
    payload
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 业务码是不是「登录态无效」
pub fn is_auth_error_code(code: &str) -> bool {
    AUTH_ERROR_CODES.contains(&code)
}

/// 上游业务文案（`desc` → `message` 兜底）
pub fn upstream_message(payload: &Value) -> String {
    payload
        .get("desc")
        .and_then(Value::as_str)
        .or_else(|| payload.get("message").and_then(Value::as_str))
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

/// 向 CAccount 发一次**带签名**的 POST（登录链路，没有账号上下文，不走代理）。
///
/// 返回上游响应体（`{code, desc, data}`）。HTTP 层失败翻译成网关错误；
/// 业务码**不在这里判**（调用方各有各的翻译，见 `login.rs`）。
pub async fn signed_post(
    path: &str,
    body: &Value,
    what: &str,
) -> Result<Value, GatewayError> {
    let url = format!("{}{path}", endpoints::xfyun_base_url());
    let headers = sign::signed_headers("POST", path, Some(body))?;
    let response = send_raw("POST", &url, Some(body), &headers, None, Some(REQUEST_TIMEOUT_MS))
        .await
        .map_err(|error| {
            if error.is_timeout() {
                GatewayError::with_status(504, format!("{what}超时，请稍后重试"))
            } else {
                GatewayError::with_status(502, format!("{what}失败：{error}"))
            }
        })?;
    if !response.ok {
        return Err(GatewayError::with_status(
            response.status as i32,
            format!("{what}返回 HTTP {}（上游地址 {}）", response.status, endpoints::xfyun_base_url()),
        ));
    }
    Ok(response.payload.unwrap_or(Value::Null))
}

/// 向集成网关发一次带 `token`（= 登录 session）的请求。
///
/// `path` 形如 `/api/v1/points/first-login`；`query` 以 query string 形态直接
/// 拼在 path 后（调用方自己拼好，见 `balance::query_usage`）。
///
/// 成功（HTTP 2xx）返回响应体；HTTP 失败 / 超时翻译成网关错误。
/// **登录态失效（业务码）由调用方判定** —— 不同接口的成功码判定不同
/// （模型列表接口根本不返回 `code`）。
pub async fn token_request(
    method: &str,
    path: &str,
    session: &str,
    body: Option<&Value>,
    what: &str,
) -> Result<Value, GatewayError> {
    if session.trim().is_empty() {
        return Err(GatewayError::with_status(401, "Loomy 账号缺少 session"));
    }
    let url = format!("{}{path}", endpoints::integration_base_url());
    let headers = vec![
        ("token".to_string(), session.to_string()),
        ("Authorization".to_string(), format!("Bearer {session}")),
    ];
    let response = send_raw(method, &url, body, &headers, None, Some(REQUEST_TIMEOUT_MS))
        .await
        .map_err(|error| {
            if error.is_timeout() {
                GatewayError::with_status(504, format!("{what}超时，请稍后重试"))
            } else {
                GatewayError::with_status(502, format!("{what}失败：{error}"))
            }
        })?;
    if !response.ok {
        if response.status == 401 || response.status == 403 {
            return Err(GatewayError::with_status(
                401,
                format!("{what}：Loomy 登录态已失效，请重新登录（HTTP {}）", response.status),
            ));
        }
        return Err(GatewayError::with_status(
            response.status as i32,
            format!("{what}返回 HTTP {}", response.status),
        ));
    }
    Ok(response.payload.unwrap_or(Value::Null))
}
