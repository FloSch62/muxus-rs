//! What the app asks of a page window (`WebWindowRequest` and `parseWebWindowRequest` in
//! `electron/src/web-windows.ts`). The request crosses a process boundary, so the window checks
//! it again before it shows anything.

use muxus_core::js::utf16_len;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What the app asks for when it opens a saved web host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebWindowRequest {
    /// Saved host ID: one window and one cookie jar per host.
    pub key: String,
    pub url: String,
    pub title: String,
    /// `socks5://127.0.0.1:<port>` through the host's SSH gateway; absent connects directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    /// The forward behind `proxy`; it is stopped when the window closes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward_id: Option<String>,
}

/// `opened` for a new window; `focused` when it was already open (a proxy sent along is then
/// not needed and is stopped); `invalid` for a request that cannot be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebWindowResult {
    Opened,
    Focused,
    Invalid,
}

/// `/^[A-Za-z0-9_-]{1,max}$/`.
fn is_token(value: &str, max: usize) -> bool {
    (1..=max).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `KEY`: a saved host ID.
pub fn is_window_key(value: &str) -> bool {
    is_token(value, 200)
}

/// `FORWARD_ID`.
pub fn is_forward_id(value: &str) -> bool {
    is_token(value, 64)
}

/// `PROXY`: `/^socks5:\/\/127\.0\.0\.1:\d{1,5}$/`.
pub fn is_proxy(value: &str) -> bool {
    value
        .strip_prefix("socks5://127.0.0.1:")
        .is_some_and(|port| {
            (1..=5).contains(&port.len()) && port.bytes().all(|b| b.is_ascii_digit())
        })
}

/// `socks5://127.0.0.1:<port>`: the proxy a page window sends everything through.
pub fn proxy_url(port: u16) -> String {
    format!("socks5://127.0.0.1:{port}")
}

/// The port of a [`proxy_url`]; `None` for anything else, or a port out of range.
pub fn proxy_port(proxy: &str) -> Option<u16> {
    if !is_proxy(proxy) {
        return None;
    }
    proxy
        .strip_prefix("socks5://127.0.0.1:")?
        .parse()
        .ok()
        .filter(|port| *port > 0)
}

/// `isWebUrl`: an address a browser window may show, `new URL(value)` with an `http:` or
/// `https:` protocol.
pub fn is_window_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

/// `parseWebWindowRequest`: the request, or `None` for one a page window must not show.
pub fn parse_web_window_request(value: &Value) -> Option<WebWindowRequest> {
    let object = value.as_object()?;
    let string = |name: &str| object.get(name).and_then(Value::as_str);
    // `undefined` is fine for the optional fields; anything else must be a valid string.
    let optional = |name: &str, valid: fn(&str) -> bool| -> Result<Option<String>, ()> {
        match object.get(name) {
            None => Ok(None),
            Some(Value::String(text)) if valid(text) => Ok(Some(text.clone())),
            Some(_) => Err(()),
        }
    };
    let key = string("key").filter(|key| is_window_key(key))?;
    let url = string("url").filter(|url| utf16_len(url) <= 4096 && is_window_url(url))?;
    let title = string("title").filter(|title| utf16_len(title) <= 500)?;
    let proxy = optional("proxy", is_proxy).ok()?;
    let forward_id = optional("forwardId", is_forward_id).ok()?;
    Some(WebWindowRequest {
        key: key.to_owned(),
        url: url.to_owned(),
        title: title.to_owned(),
        proxy,
        forward_id,
    })
}

/// The forward a refused request named, so it can be stopped all the same (`muxus:web:open`
/// stops a valid-looking `forwardId` of a request it refuses).
pub fn refused_forward_id(value: &Value) -> Option<String> {
    value
        .get("forwardId")
        .and_then(Value::as_str)
        .filter(|id| is_forward_id(id))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    //! Port of tests/unit/electron/web-windows.test.ts.

    use serde_json::json;

    use super::*;

    fn base() -> Value {
        json!({ "key": "abc_1", "url": "https://grafana.internal:3000/d/x", "title": "Grafana" })
    }

    fn with(field: &str, value: Value) -> Value {
        let mut request = base();
        request[field] = value;
        request
    }

    #[test]
    fn accepts_a_page_with_its_proxy_and_the_forward_behind_it() {
        assert_eq!(
            parse_web_window_request(&base()),
            Some(WebWindowRequest {
                key: "abc_1".into(),
                url: "https://grafana.internal:3000/d/x".into(),
                title: "Grafana".into(),
                proxy: None,
                forward_id: None,
            })
        );
        let mut request = base();
        request["proxy"] = json!("socks5://127.0.0.1:50123");
        request["forwardId"] = json!("Ab3_-x9");
        let parsed = parse_web_window_request(&request).unwrap();
        assert_eq!(parsed.proxy.as_deref(), Some("socks5://127.0.0.1:50123"));
        assert_eq!(parsed.forward_id.as_deref(), Some("Ab3_-x9"));
        assert_eq!(serde_json::to_value(&parsed).unwrap(), request);
    }

    #[test]
    fn refuses_what_a_browser_window_should_not_open() {
        assert_eq!(
            parse_web_window_request(&with("url", json!("file:///etc/passwd"))),
            None
        );
        assert_eq!(
            parse_web_window_request(&with("url", json!("http://[fe80::1%eth0]/"))),
            None
        );
        assert_eq!(
            parse_web_window_request(&with("proxy", json!("socks5://10.0.0.1:1080"))),
            None
        );
        assert_eq!(
            parse_web_window_request(&with("forwardId", json!("../../x"))),
            None
        );
        assert_eq!(parse_web_window_request(&with("key", json!("a b"))), None);
    }

    #[test]
    fn refuses_wrong_types_and_oversized_fields() {
        assert_eq!(parse_web_window_request(&json!("https://x")), None);
        assert_eq!(parse_web_window_request(&with("proxy", Value::Null)), None);
        assert_eq!(parse_web_window_request(&with("title", json!(1))), None);
        assert_eq!(
            parse_web_window_request(&with("title", json!("t".repeat(501)))),
            None
        );
        assert_eq!(
            parse_web_window_request(&with(
                "url",
                json!(format!("https://x/{}", "a".repeat(4096)))
            )),
            None
        );
        assert_eq!(
            parse_web_window_request(&with("key", json!("k".repeat(201)))),
            None
        );
        assert!(parse_web_window_request(&with("url", json!("HTTP://Router"))).is_some());
    }

    #[test]
    fn names_the_forward_of_a_refused_request() {
        let mut request = with("url", json!("file:///etc/passwd"));
        request["forwardId"] = json!("f1");
        assert_eq!(refused_forward_id(&request).as_deref(), Some("f1"));
        request["forwardId"] = json!("../x");
        assert_eq!(refused_forward_id(&request), None);
    }

    #[test]
    fn builds_and_reads_proxy_addresses() {
        assert_eq!(proxy_url(50123), "socks5://127.0.0.1:50123");
        assert_eq!(proxy_port("socks5://127.0.0.1:50123"), Some(50123));
        assert_eq!(proxy_port("socks5://127.0.0.1:99999"), None);
        assert_eq!(proxy_port("socks5://127.0.0.1:0"), None);
        assert_eq!(proxy_port("socks5://localhost:1080"), None);
    }
}
