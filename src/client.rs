//! The agent CLI's side of the API. It holds no state and no privileges: it
//! is a thin client of `/api/agent`, so agents meet the same gates as the UI.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::{Method, StatusCode};
use serde_json::{Map, Value};
use url::Url;

use crate::http::AGENT_HEADER;

pub struct Client {
    base: Url,
    agent: String,
    http: reqwest::Client,
}

/// A request that reached hoja and was answered with an error.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub body: Value,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = self.body["message"].as_str().unwrap_or("request failed");
        write!(f, "{} ({})", message, self.status)
    }
}
impl std::error::Error for ApiError {}

/// The server must be a plain loopback `http://` origin.
pub fn server_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).context("invalid hoja server URL")?;
    let loopback = match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    if url.scheme() != "http"
        || !loopback
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!(
            "the hoja server must be a plain http:// loopback origin, such as http://127.0.0.1:4520"
        );
    }
    Ok(url)
}

impl Client {
    pub fn new(base: Url, agent: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(90))
            .build()?;
        Ok(Self { base, agent, http })
    }

    pub async fn request(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let url = self.base.join(&format!("api/agent/{path}"))?;
        let mut request = self
            .http
            .request(method, url)
            .header(AGENT_HEADER, &self.agent);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.with_context(|| {
            format!(
                "could not reach hoja at {}; is `hoja serve` running?",
                self.base
            )
        })?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(ApiError { status, body }.into());
        }
        Ok(body)
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        self.request(Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.request(Method::POST, path, Some(body)).await
    }
}

/// `--set name=value` pairs; a value that parses as JSON is taken as JSON
/// (lists, booleans, null), anything else as a string.
pub fn fields(json: Option<&str>, sets: &[String]) -> Result<Map<String, Value>> {
    let mut map = match json {
        Some(raw) => match serde_json::from_str(raw).context("--json must be a JSON object")? {
            Value::Object(map) => map,
            _ => bail!("--json must be a JSON object"),
        },
        None => Map::new(),
    };
    for set in sets {
        let (name, raw) = set
            .split_once('=')
            .with_context(|| format!("--set takes name=value, got `{set}`"))?;
        let value = match serde_json::from_str::<Value>(raw) {
            Ok(v @ (Value::Array(_) | Value::Bool(_) | Value::Null)) => v,
            _ => Value::String(raw.to_string()),
        };
        map.insert(name.trim().to_string(), value);
    }
    Ok(map)
}

/// Encodes one path segment, since ids and section names travel in the path.
pub fn segment(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_must_be_loopback_http() {
        assert!(server_url("http://127.0.0.1:4520").is_ok());
        assert!(server_url("http://localhost:4520").is_ok());
        for raw in [
            "https://127.0.0.1:4520",
            "http://example.com:4520",
            "http://127.0.0.1:4520/x",
        ] {
            assert!(server_url(raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn sets_parse_json_lists_but_keep_numbers_as_text() {
        let map = fields(
            Some(r#"{"a": "x"}"#),
            &[
                "refs=[\"!1\"]".into(),
                "period=2026".into(),
                "text=hi there".into(),
            ],
        )
        .unwrap();
        assert_eq!(map["refs"], serde_json::json!(["!1"]));
        assert_eq!(map["period"], "2026");
        assert_eq!(map["text"], "hi there");
        assert_eq!(map["a"], "x");
    }
}
