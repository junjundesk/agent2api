//! Cline 伪装头：`GET/PUT /api/cline/headers`。
//!
//! ── 这个端点管什么 ──────────────────────────────────────────
//! Cline 上游请求的伪装头（`X-CLIENT-TYPE: cline-sdk`、平台标识、版本号等）
//! 的**逐键覆盖**。默认值硬编码在 `core::providers::cline::headers`（与官方
//! 客户端形态对齐的那一套，上游收紧时跟着版本走），配置里只存用户改过的键
//! （`config::KEY_CLINE_UPSTREAM_HEADERS`），适配器每次构造上游请求都读快照
//! —— 改完下一个请求立即生效，不重启进程。
//!
//! ── 接口形状 ────────────────────────────────────────────────
//! GET 返回 `{overrides, defaults, effective}`：第一份是用户改过的键（原样
//! 回显，界面据此标出哪些行被改过），第二份是代码里的默认值（界面用它区分
//! 「改回了默认」与「本来就没改」），第三份是合并后的生效值。
//! PUT 收 `{overrides: {...}}` **整体替换**覆盖表：非空值按键覆盖/新增，
//! 空串 = 这个头不发；把某个键从覆盖表里删掉 = 回落默认值。固定头
//! （见 [`FIXED_HEADERS`]）与非法头名/头值一律 400 拒绝，不落库。

use std::collections::BTreeMap;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderName, HeaderValue};
use axum::response::Response;
use serde_json::{json, Map, Value};

use crate::server::config;
use crate::server::core::providers::cline::headers;
use crate::server::errors;
use crate::server::http::{ok_json, parse_body};
use crate::server::logging;
use crate::server::ServerState;

/// 覆盖表规模上限：伪装头就十来个键，64 是给自定义头留的余量
const MAX_ENTRIES: usize = 64;
/// 单个头名的长度上限（与 `http::HeaderName` 的名字长度上界同值）
const MAX_KEY_LEN: usize = 128;
/// 单个头值的长度上限
const MAX_VALUE_LEN: usize = 4096;

/// **固定头**：协议必需（`Content-Type` / `Accept`）或每请求动态生成
/// （`Authorization` 带账号 token、`X-Task-ID` 与 body 的 `session_id` 同值），
/// 覆盖表里出现它们一律 400。
///
/// 与 `core::providers::cline::headers::DEFAULT_HEADERS` 的**缺席项**一一对应
/// ——那里是执行侧（这些头由适配器固定构造），这里是配置侧（用户不该能改）。
/// 必须拦在配置侧：覆盖表里写一个 `Authorization` 会经适配器的覆盖合并顶掉
/// 真正的凭证，写 `X-Task-ID` 会让它与 body 的 `session_id` 不再同值。
const FIXED_HEADERS: [&str; 4] = ["Authorization", "Content-Type", "Accept", "X-Task-ID"];

/// GET /api/cline/headers —— 当前覆盖表 + 默认值 + 合并后的生效值
pub async fn get_cline_headers(State(_state): State<ServerState>) -> Response {
    ok_json(headers_json())
}

/// PUT /api/cline/headers —— body `{overrides: {"User-Agent": "...", ...}}`
///
/// 整体替换覆盖表（不是增量 merge：界面按「编辑后的整张表」提交，删除某行
/// 就是把它从表里去掉）。校验见 [`validated_overrides`]：形状、规模、固定头、
/// 头名与头值的字符合法性；非法项直接 400，不做静默修正。
pub async fn put_cline_headers(State(_state): State<ServerState>, body: Bytes) -> Response {
    let payload = match parse_body(&body) {
        Ok(value) => value,
        Err(error) => return errors::management_error(400, error.message),
    };
    let Some(object) = payload.as_object() else {
        return errors::management_error(400, "请求体必须是 JSON 对象");
    };
    let Some(raw_overrides) = object.get("overrides") else {
        return errors::management_error(400, "缺少 overrides");
    };
    let Some(entries) = raw_overrides.as_object() else {
        return errors::management_error(400, "overrides 必须是对象（头名 → 头值）");
    };
    let overrides = match validated_overrides(entries) {
        Ok(overrides) => overrides,
        Err(message) => return errors::management_error(400, message),
    };
    if !config::set_cline_upstream_headers(overrides) {
        logging::log("[Config]", "⚠️  Cline 伪装头写入失败，本次运行内仍生效");
    }
    logging::log("[Config]", "Cline 伪装头覆盖表已更新（下一个 Cline 请求生效）");
    ok_json(headers_json())
}

/// 覆盖表的校验与归一（PUT 的本体，单测直接喂 JSON 对象）。
///
/// 非法项一律 `Err`（400 文案），不做静默修正。**字符合法性必须在这里拦**：
/// 头名不是 HTTP token（含空格、冒号、非 ASCII）或头值带控制字符时，
/// `core::upstream::request::send_chat_request` 的 `builder.header(...)` 会
/// 拿不到合法的 `HeaderName`/`HeaderValue`，reqwest 把这次请求标成构造失败
/// —— 于是**每一条** Cline 请求都发不出去，而客户端看到的是「上游请求失败
/// （…）」，与真实原因（配置里有个畸形头名）毫无关系，且配置已落库、
/// 重启也在。校验用 `HeaderName`/`HeaderValue` 的 `try_from`：与 reqwest
/// 构造请求时用的是**同一套规则**，不需要另维护一张字符白名单。
fn validated_overrides(entries: &Map<String, Value>) -> Result<BTreeMap<String, String>, String> {
    if entries.len() > MAX_ENTRIES {
        return Err(format!("overrides 最多 {} 项", MAX_ENTRIES));
    }
    let mut overrides = BTreeMap::new();
    for (key, value) in entries {
        let key = key.trim();
        if key.is_empty() || key.len() > MAX_KEY_LEN {
            return Err(format!("头名非法或过长: {key:?}"));
        }
        if HeaderName::try_from(key).is_err() {
            return Err(format!(
                "头名含非法字符（只能是字母、数字与 !#$%&'*+-.^_`|~）: {key:?}"
            ));
        }
        if FIXED_HEADERS.iter().any(|name| name.eq_ignore_ascii_case(key)) {
            return Err(format!("{key} 是网关固定头，不能覆盖"));
        }
        let Some(text) = value.as_str() else {
            return Err(format!("头 {key} 的值必须是字符串"));
        };
        if text.len() > MAX_VALUE_LEN {
            return Err(format!("头 {key} 的值过长"));
        }
        if HeaderValue::try_from(text).is_err() {
            return Err(format!("头 {key} 的值含非法字符（不能有换行等控制字符）"));
        }
        overrides.insert(key.to_string(), text.to_string());
    }
    Ok(overrides)
}

/// 响应形状（GET 与 PUT 共用，前端直接用响应刷新界面）
///
/// `defaults` 一并给出：界面要把「用户把值改回了默认」与「本来就没改」区分开
/// （前者是一份覆盖、后者不该出现在覆盖表里），没有默认值就做不到这个判断。
fn headers_json() -> Value {
    let overrides: BTreeMap<String, String> = config::cline_upstream_overrides().into_iter().collect();
    let defaults: Map<String, Value> = headers::DEFAULT_HEADERS
        .iter()
        .map(|(key, value)| ((*key).to_string(), Value::String((*value).to_string())))
        .collect();
    let effective: Map<String, Value> = headers::effective_headers()
        .into_iter()
        .map(|(key, value)| (key, Value::String(value)))
        .collect();
    json!({
        "overrides": overrides,
        "defaults": defaults,
        "effective": effective,
    })
}

#[cfg(test)]
mod tests {
    //! 覆盖表校验的纯函数测试（针对评审里两条「写坏了会静默伤到后续每一条
    //! 请求」的路径：固定头被顶掉、畸形头名让请求构造失败）。
    use super::*;

    fn entries(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), Value::String((*value).to_string())))
            .collect()
    }

    #[test]
    fn a_fixed_header_cannot_be_overridden() {
        for (key, value) in [
            ("Authorization", "Bearer stolen"),
            ("x-task-id", "fixed"),
            ("Content-Type", "text/plain"),
            ("Accept", "*/*"),
        ] {
            let result = validated_overrides(&entries(&[(key, value)]));
            assert!(result.is_err(), "{key} 不该被接受");
            assert!(result.unwrap_err().contains("固定头"));
        }
    }

    #[test]
    fn a_header_name_with_illegal_characters_is_rejected() {
        // 空格、冒号、非 ASCII 都不是 HTTP token：存下去会让每条 Cline 请求
        // 在 reqwest 构造阶段失败（不是这里的假设，是 http crate 的规则）
        for key in ["X My Header", "Accept:", "X-自定义头"] {
            let result = validated_overrides(&entries(&[(key, "v")]));
            assert!(result.is_err(), "{key} 不该被接受");
            assert!(result.unwrap_err().contains("非法字符"));
        }
    }

    #[test]
    fn a_header_value_with_a_control_character_is_rejected() {
        let result = validated_overrides(&entries(&[("X-Trace", "abc\ndef")]));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("非法字符"));
    }

    #[test]
    fn a_non_string_value_is_rejected() {
        let raw = json!({ "X-Trace": 1 });
        let result = validated_overrides(raw.as_object().unwrap_or(&Map::new()));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("必须是字符串"));
    }

    #[test]
    fn a_valid_table_is_normalized_and_kept() {
        let overrides = validated_overrides(&entries(&[
            (" X-Title ", "My Cline"),
            ("X-PLATFORM", ""),
            ("X-Custom-Trace", "abc"),
        ]))
        .expect("合法表应当通过");
        // 头名去首尾空白；空串（= 不发这个头）与自定义头原样保留
        assert_eq!(overrides.get("X-Title").map(String::as_str), Some("My Cline"));
        assert_eq!(overrides.get("X-PLATFORM").map(String::as_str), Some(""));
        assert_eq!(
            overrides.get("X-Custom-Trace").map(String::as_str),
            Some("abc")
        );
    }
}
