//! JSON-RPC, as little of it as a probe needs.

use std::time::Duration;

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcError {
    /// HTTP 429, or a JSON-RPC error that says the same. Not a verdict on the method.
    RateLimited(String),
    /// The endpoint answered and said no.
    Refused(String),
    /// Nothing usable came back.
    Transport(String),
}

impl RpcError {
    pub fn message(&self) -> &str {
        match self {
            RpcError::RateLimited(m) | RpcError::Refused(m) | RpcError::Transport(m) => m,
        }
    }
}

#[allow(async_fn_in_trait)]
pub trait Rpc {
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError>;
}

pub struct HttpRpc {
    url: String,
    client: reqwest::Client,
}

impl HttpRpc {
    pub fn new(url: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("sib/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("a default TLS client");
        Self {
            url: url.to_string(),
            client,
        }
    }
}

/// One line of a provider's refusal: they run to paragraphs and sometimes carry a sales link.
fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    line.chars().take(200).collect()
}

fn looks_rate_limited(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("rate limit")
        || m.contains("request limit")
        || m.contains("too many requests")
        || m.contains("limit exceeded")
}

impl Rpc for HttpRpc {
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let res = self
            .client
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| RpcError::Transport(e.to_string()))?;
        let status = res.status();
        let text = res
            .text()
            .await
            .map_err(|e| RpcError::Transport(e.to_string()))?;
        let parsed: Option<Value> = serde_json::from_str(&text).ok();
        let error = parsed
            .as_ref()
            .and_then(|v| v.get("error"))
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str);
        if status.as_u16() == 429 || error.is_some_and(looks_rate_limited) {
            return Err(RpcError::RateLimited(first_line(error.unwrap_or(&text))));
        }
        if let Some(message) = error {
            return Err(RpcError::Refused(first_line(message)));
        }
        if !status.is_success() {
            return Err(RpcError::Refused(format!(
                "HTTP {}: {}",
                status.as_u16(),
                first_line(&text)
            )));
        }
        match parsed.and_then(|mut v| v.get_mut("result").map(Value::take)) {
            Some(result) => Ok(result),
            None => Err(RpcError::Transport(format!(
                "no result in the answer: {}",
                first_line(&text)
            ))),
        }
    }
}

pub fn hex(n: u64) -> String {
    format!("{n:#x}")
}

pub fn from_hex(v: &Value) -> Option<u64> {
    u64::from_str_radix(v.as_str()?.strip_prefix("0x")?, 16).ok()
}

/// `network:url`, with graph-node's optional capabilities between the two tolerated and ignored,
/// since sib measures them itself.
pub fn parse_spec(spec: &str) -> Option<(String, String)> {
    let at = spec.find("://")?;
    let scheme_start = spec[..at].rfind(':')? + 1;
    let network = spec[..scheme_start - 1].split(':').next()?.trim();
    let url = spec[scheme_start..].trim();
    (!network.is_empty() && (url.starts_with("http://") || url.starts_with("https://")))
        .then(|| (network.to_string(), url.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spec_is_a_network_and_a_url_with_capabilities_ignored() {
        assert_eq!(
            parse_spec("base:https://base-rpc.publicnode.com"),
            Some(("base".into(), "https://base-rpc.publicnode.com".into()))
        );
        assert_eq!(
            parse_spec("mainnet:archive,traces:http://localhost:8545"),
            Some(("mainnet".into(), "http://localhost:8545".into()))
        );
        assert_eq!(parse_spec("https://no-network.example"), None);
        assert_eq!(parse_spec("base:ftp://x"), None);
        assert_eq!(parse_spec("base"), None);
    }

    #[test]
    fn hex_round_trips() {
        assert_eq!(hex(56_055_187), "0x3575593");
        assert_eq!(from_hex(&json!("0x3575593")), Some(56_055_187));
        assert_eq!(from_hex(&json!(12)), None);
    }

    #[test]
    fn a_rate_limit_is_recognised_by_its_words() {
        assert!(looks_rate_limited("Rate limit (1200rqs/60s) reached."));
        assert!(looks_rate_limited("request limit reached"));
        assert!(looks_rate_limited("limit exceeded"));
        assert!(!looks_rate_limited("exceed maximum block range: 50000"));
    }
}
