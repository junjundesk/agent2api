//! Cline 上游请求头的默认值与覆盖合并（设置页「Cline 伪装头」的后端）。
//!
//! ── 这套头是什么 ────────────────────────────────────────────
//! Cline 上游按请求头判定「调用方是不是 Cline 自家产品」：`X-CLIENT-TYPE:
//! cline-sdk` 是**硬门槛**（缺了免费池一律 403，见 `adapter.rs` 模块头），
//! 其余的头（版本号、平台标识）让请求**更贴近官方 CLI 的形态** —— 上游的
//! 行为可能随版本收紧，带上没有代价。默认值来自对官方客户端通信的实测
//! （workbuddy2api / cline-proxy 两个参考实现相互印证），版本号沿用本项目
//! 适配器既有的 3.0.62（较新的实测口径）。
//!
//! ── 覆盖语义（与配置键 `clineUpstreamHeaders` 的分工）────────
//! 默认值是**代码里的一等公民**（上游收紧时跟着版本走），配置里只存用户
//! 改过的键（见 `config::KEY_CLINE_UPSTREAM_HEADERS`）：
//!   - 覆盖值**非空** → 按键覆盖默认值（也可新增默认清单里没有的自定义头）；
//!   - 覆盖值**为空串** → 这个头**不发**（显式删除）—— 于是「回到默认」
//!     （在界面删掉这一行）与「禁用某个头」（值留空）都是一次 PUT 能表达的事。
//!
//! 合并发生在**每次构造上游请求**时（`adapter::build_chat_request`）：
//! 覆盖表逐请求读快照，改完设置下一个请求立即生效，不重启进程。

use std::collections::BTreeMap;

use super::adapter::CLIENT_TYPE;

/// 默认请求头（键 → 值）。`Authorization` / `Content-Type` / `Accept` /
/// `X-Task-ID` 不在其中：前两个是协议必需（适配器固定构造），`X-Task-ID`
/// 每请求动态生成（与 body 的 `session_id` 同值），都不该被用户覆盖
/// （这套「固定头」清单同时也是管理接口拒绝覆盖的依据，见
/// `api::cline_headers::FIXED_HEADERS`）。
///
/// `X-CLIENT-TYPE` 引用 `adapter::CLIENT_TYPE`（不是再抄一份字面量）：这个值
/// 是免费池的硬门槛，两处各写一份的话，改一处不会带动另一处。
pub const DEFAULT_HEADERS: &[(&str, &str)] = &[
    ("User-Agent", "Cline/3.0.62"),
    ("HTTP-Referer", "https://cline.bot"),
    ("X-Title", "Cline"),
    ("X-IS-MULTIROOT", "false"),
    ("X-CLIENT-TYPE", CLIENT_TYPE),
    ("X-CLIENT-VERSION", "3.0.62"),
    ("X-PLATFORM", "terminal"),
    ("X-PLATFORM-VERSION", "3.0.62"),
    ("X-CORE-VERSION", "0.0.70"),
];

/// 生效请求头：默认值 ∪ 用户覆盖（非空覆盖 / 空串删除）。
///
/// 输出保序（默认清单的顺序优先，用户新增的自定义头按字典序跟在后面），
/// 调用方（适配器 / 管理接口）拿到的就是「该发哪些头」的最终形态。
pub fn effective_headers() -> Vec<(String, String)> {
    let overrides: BTreeMap<String, String> = crate::server::config::cline_upstream_overrides()
        .into_iter()
        .collect();
    merge_headers(&overrides)
}

/// 合并的纯函数形态（[`effective_headers`] 的本体，单测直接喂覆盖表）。
///
/// 键的比对**大小写不敏感**（HTTP 头名本来就不区分大小写）：覆盖表里写
/// `user-agent` 要能覆盖默认的 `User-Agent`，而且**不能**两行都出现。
/// 这与适配器侧的 `upsert_header` 同一口径 —— 两边不一致的话，
/// 这里返回的「生效值」就不是适配器真正发出去的那份（管理接口的
/// `effective` 字段就是这么声称的）。
fn merge_headers(overrides: &BTreeMap<String, String>) -> Vec<(String, String)> {
    // 按头名（大小写不敏感）取覆盖值
    let override_for = |name: &str| -> Option<&String> {
        overrides
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    };
    let mut headers: Vec<(String, String)> = Vec::with_capacity(DEFAULT_HEADERS.len());
    for (key, default_value) in DEFAULT_HEADERS {
        // 空串覆盖 = 显式不发这个头；其余按键覆盖
        match override_for(key) {
            Some(value) if value.is_empty() => continue,
            Some(value) => headers.push(((*key).to_string(), value.clone())),
            None => headers.push(((*key).to_string(), (*default_value).to_string())),
        }
    }
    // 默认清单（大小写不敏感）之外的自定义头：原样带上（空值同样表示不发）
    for (key, value) in overrides {
        if DEFAULT_HEADERS
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(key))
            || value.is_empty()
        {
            continue;
        }
        headers.push((key.clone(), value.clone()));
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overrides(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn the_default_list_excludes_protocol_mandatory_headers() {
        for (key, _) in DEFAULT_HEADERS {
            assert!(!matches!(
                *key,
                "Authorization" | "Content-Type" | "Accept" | "X-Task-ID"
            ));
        }
    }

    #[test]
    fn an_empty_override_table_yields_all_defaults() {
        let headers = merge_headers(&BTreeMap::new());
        assert_eq!(headers.len(), DEFAULT_HEADERS.len());
        assert!(headers
            .iter()
            .any(|(key, value)| key == "X-CLIENT-TYPE" && value == "cline-sdk"));
    }

    #[test]
    fn a_non_empty_override_replaces_by_key() {
        let headers = merge_headers(&overrides(&[("X-PLATFORM", "extension")]));
        let platform = headers
            .iter()
            .find(|(key, _)| key == "X-PLATFORM")
            .map(|(_, value)| value.as_str());
        assert_eq!(platform, Some("extension"));
        // 其余默认项不受影响
        assert_eq!(headers.len(), DEFAULT_HEADERS.len());
    }

    #[test]
    fn a_case_variant_override_replaces_the_default_instead_of_duplicating_it() {
        let headers = merge_headers(&overrides(&[("user-agent", "Cline/9.9.9")]));
        let values: Vec<&str> = headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case("User-Agent"))
            .map(|(_, value)| value.as_str())
            .collect();
        // 头名大小写不敏感：同义键只允许一个，值是覆盖值（不是默认值）
        assert_eq!(values, vec!["Cline/9.9.9"]);
        assert_eq!(headers.len(), DEFAULT_HEADERS.len());
    }

    #[test]
    fn an_empty_string_override_drops_the_header() {
        let headers = merge_headers(&overrides(&[("X-IS-MULTIROOT", "")]));
        assert!(!headers.iter().any(|(key, _)| key == "X-IS-MULTIROOT"));
        assert_eq!(headers.len(), DEFAULT_HEADERS.len() - 1);
    }

    #[test]
    fn custom_headers_are_appended_after_the_defaults() {
        let headers = merge_headers(&overrides(&[("X-Custom-Trace", "abc")]));
        assert_eq!(headers.len(), DEFAULT_HEADERS.len() + 1);
        assert_eq!(
            headers.last().map(|(key, value)| (key.as_str(), value.as_str())),
            Some(("X-Custom-Trace", "abc"))
        );
    }
}
