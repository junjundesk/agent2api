//! 网关面跨域访问开关：`GET/PUT /api/cors`。
//!
//! ── 这个开关做什么 ──────────────────────────────────────────
//! 打开后网关面（`/v1/*`）按面板路由的老口径应答 CORS：预检请求直接 204，
//! 三个 `Access-Control-Allow-*` 头随每个响应下发（来源 `*`）。关着时（默认）
//! 网关面不碰 CORS —— 浏览器里第三方来源的页面调不到它：预检请求会落到 API Key
//! 中间件上被 401 拒掉（预检按规范不携带 `Authorization` 头），页面侧只能看到
//! 一句「无法连接 API」。
//!
//! ── 为什么要这个开关，而不是直接常开 ─────────────────────────
//! 网关面是**真正转发上游、消耗额度**的那一面：开着 `*` 又没配 API Key 时，
//! 任何网页都能借本机网关打上游。默认关 + 显式开启，至少让用户清楚自己打开了
//! 什么。面板路由（`/api/*`）不受本开关影响，保持既有的无条件 CORS
//! （面板与 `/api/*` 同源，浏览器本来不需要它，改它属于另一件事）。
//!
//! ── 写盘位置 ────────────────────────────────────────────────
//! `config` 的 `corsEnabled` 键（统一库 `kv` 表里的配置行）。CORS 中间件逐请求
//! 读快照，改完下一个请求立即生效，不重启进程。

use axum::body::Bytes;
use axum::extract::State;
use axum::response::Response;
use serde_json::{json, Value};

use crate::server::config;
use crate::server::errors;
use crate::server::http::{ok_json, parse_body};
use crate::server::logging;
use crate::server::ServerState;

/// GET /api/cors —— 当前开关状态
pub async fn get_cors(State(_state): State<ServerState>) -> Response {
    ok_json(cors_json())
}

/// PUT /api/cors —— body `{corsEnabled: true|false}`
///
/// 写配置，下一个请求立即生效（CORS 中间件逐请求读快照，不重启进程）。
/// 两种状态都在事件日志里留一条记录：开关本身是「谁能调本机网关」的安全边界，
/// 用户该在日志里看得到它被改过。
pub async fn put_cors(State(_state): State<ServerState>, body: Bytes) -> Response {
    let payload = match parse_body(&body) {
        Ok(value) => value,
        Err(error) => return errors::management_error(400, error.message),
    };
    let Some(object) = payload.as_object() else {
        return errors::management_error(400, "请求体必须是 JSON 对象");
    };
    let key = config::KEY_CORS_ENABLED;
    let Some(value) = object.get(key) else {
        return errors::management_error(400, format!("缺少 {key}"));
    };
    let Some(enabled) = value.as_bool() else {
        return errors::management_error(400, format!("{key} 必须是 true 或 false"));
    };
    if !config::set_cors_enabled(enabled) {
        logging::log("[Config]", "⚠️  网关跨域访问设置写入失败，本次运行内仍生效");
    }
    logging::log(
        "[Config]",
        if enabled {
            "网关跨域访问已开启：/v1/* 允许浏览器跨来源调用（Access-Control-Allow-Origin: *）"
        } else {
            "网关跨域访问已关闭：/v1/* 不再应答跨源请求，浏览器客户端会被预检拦下"
        },
    );
    ok_json(cors_json())
}

/// 开关状态（GET 与 PUT 共用，前端直接用响应刷新界面）
fn cors_json() -> Value {
    json!({
        config::KEY_CORS_ENABLED: config::current().cors_enabled(),
    })
}
