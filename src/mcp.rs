//! hoja's tools over MCP: JSON-RPC 2.0 with `initialize`, `ping`,
//! `tools/list`, `tools/call` and Tracon's `tracon/preview`, independent of
//! transport. `hoja mcp` carries it as lines on stdio for any MCP client
//! ([`Standalone`]); `hoja serve --socket` carries it over HTTP on the
//! plugin socket (`plugin::Plugin`). Both list [`ops::tools`], so the tools
//! are the agent API's operations whichever way they are reached.

use std::future::Future;
use std::sync::Arc;

use anyhow::Result;
use reqwest::Method;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use crate::client::{self, ApiError, Client};
use crate::ops::{self, Verb};

/// The MCP revision hoja answers with when a client names none.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// A JSON-RPC error.
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// What a server does with the calls it is sent.
pub trait Host: Send + Sync {
    /// A `tools/call` of a tool [`ops::find`] knows. `meta` is
    /// `params._meta`, when the call carried one.
    fn call(
        &self,
        op: &'static ops::Op,
        arguments: Option<&Value>,
        meta: Option<&Value>,
    ) -> impl Future<Output = Result<Value, RpcError>> + Send;

    /// A `tracon/preview`.
    fn preview(&self, _params: &Value) -> impl Future<Output = Result<Value, RpcError>> + Send {
        async { Err(RpcError::new(-32601, "no method tracon/preview")) }
    }
}

/// A tool's answer: the agent API's body for `status`, as text, flagged as
/// an error when the API would have answered with one.
pub fn tool_result(success: bool, body: &Value) -> Value {
    let text = serde_json::to_string_pretty(body).unwrap_or_default();
    json!({ "content": [{ "type": "text", "text": text }], "isError": !success })
}

fn error_response(id: Value, e: RpcError) -> Value {
    let mut error = json!({ "code": e.code, "message": e.message });
    if let Some(data) = e.data {
        error["data"] = data;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": error })
}

/// A JSON-RPC error answer to a message that could not be read.
pub fn parse_error(e: &str) -> Value {
    error_response(
        Value::Null,
        RpcError::new(-32700, format!("parse error: {e}")),
    )
}

/// Answer one JSON-RPC message; `None` for a notification or a response,
/// which get no answer.
pub async fn handle<H: Host>(host: &H, msg: &Value) -> Option<Value> {
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        if msg.get("result").is_some() || msg.get("error").is_some() {
            return None;
        }
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        return Some(error_response(id, RpcError::new(-32600, "invalid request")));
    };
    let id = msg.get("id").cloned()?;
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params["protocolVersion"]
                .as_str()
                .unwrap_or(PROTOCOL_VERSION);
            Ok(json!({
                "protocolVersion": asked,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": crate::contract::PLUGIN, "version": env!("CARGO_PKG_VERSION")},
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": ops::tools() })),
        "tools/call" => match params["name"].as_str() {
            None => Err(RpcError::new(-32602, "tools/call needs a tool name")),
            Some(name) => match ops::find(name) {
                None => Err(RpcError::new(-32602, format!("hoja has no tool {name}"))),
                Some(op) => {
                    let meta = params.get("_meta").filter(|m| !m.is_null());
                    host.call(op, params.get("arguments"), meta).await
                }
            },
        },
        "tracon/preview" => host.preview(&params).await,
        other => Err(RpcError::new(-32601, format!("no method {other}"))),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(e) => error_response(id, e),
    })
}

/// Serve MCP on stdin and stdout, one JSON-RPC message a line. Calls run
/// concurrently, since `card_status` may wait 45 seconds.
pub async fn stdio<H: Host + 'static>(host: Arc<H>) -> Result<()> {
    let out = Arc::new(Mutex::new(tokio::io::stdout()));
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut calls = tokio::task::JoinSet::new();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let (host, out) = (host.clone(), out.clone());
        calls.spawn(async move {
            let answer = match serde_json::from_str::<Value>(&line) {
                Ok(msg) => handle(host.as_ref(), &msg).await,
                Err(e) => Some(parse_error(&e.to_string())),
            };
            if let Some(answer) = answer {
                let mut out = out.lock().await;
                let _ = out.write_all(format!("{answer}\n").as_bytes()).await;
                let _ = out.flush().await;
            }
        });
    }
    while calls.join_next().await.is_some() {}
    Ok(())
}

/// `hoja mcp`: each tool is the agent API call of the same name, sent to a
/// running `hoja serve` (or a plugin's `agent_listen`) as the agent the CLI
/// names, so the server's gates and the person's page are where they always
/// are.
pub struct Standalone {
    pub client: Client,
}

impl Standalone {
    async fn forward(&self, op: &ops::Op, arguments: Option<&Value>) -> Result<Value, String> {
        let (params, rest) = op.split(arguments.cloned().unwrap_or(Value::Null))?;
        let mut path = op.path.to_string();
        for (name, value) in op.path_params().into_iter().zip(&params) {
            path = path.replace(&format!("{{{name}}}"), &client::segment(value));
        }
        let path = path.trim_start_matches('/').to_string();
        let sent = match op.verb {
            Verb::Get => {
                let query = query_string(&rest)?;
                let target = if query.is_empty() {
                    path
                } else {
                    format!("{path}?{query}")
                };
                self.client.request(Method::GET, &target, None).await
            }
            Verb::Post => {
                self.client
                    .request(Method::POST, &path, Some(Value::Object(rest)))
                    .await
            }
        };
        sent.map_err(|err| match err.downcast_ref::<ApiError>() {
            Some(api) => serde_json::to_string_pretty(&api.body).unwrap_or_default(),
            None => format!("{err:#}"),
        })
    }
}

/// A GET's arguments as its query string.
fn query_string(rest: &serde_json::Map<String, Value>) -> Result<String, String> {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in rest {
        let value = match value {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null => continue,
            _ => return Err(format!("`{name}` must be a string or a number")),
        };
        query.append_pair(name, &value);
    }
    Ok(query.finish())
}

impl Host for Standalone {
    async fn call(
        &self,
        op: &'static ops::Op,
        arguments: Option<&Value>,
        _meta: Option<&Value>,
    ) -> Result<Value, RpcError> {
        Ok(match self.forward(op, arguments).await {
            Ok(body) => tool_result(true, &body),
            Err(message) => {
                json!({ "content": [{ "type": "text", "text": message }], "isError": true })
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl Host for Echo {
        async fn call(
            &self,
            op: &'static ops::Op,
            arguments: Option<&Value>,
            _meta: Option<&Value>,
        ) -> Result<Value, RpcError> {
            Ok(tool_result(
                true,
                &json!({"op": op.name, "args": arguments}),
            ))
        }
    }

    #[tokio::test]
    async fn answers_initialize_list_call_and_notifications() {
        let init = handle(
            &Echo,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": "2025-03-26"}}),
        )
        .await
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "hoja");
        let list = handle(
            &Echo,
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        )
        .await
        .unwrap();
        assert_eq!(
            list["result"]["tools"].as_array().unwrap().len(),
            ops::OPS.len()
        );
        assert!(
            handle(
                &Echo,
                &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
            )
            .await
            .is_none()
        );
        let call = handle(
            &Echo,
            &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": {"name": "nope"}}),
        )
        .await
        .unwrap();
        assert_eq!(call["error"]["code"], -32602);
        let call = handle(
            &Echo,
            &json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                "params": {"name": "types", "arguments": {}}}),
        )
        .await
        .unwrap();
        assert_eq!(call["result"]["isError"], false);
        let preview = handle(
            &Echo,
            &json!({"jsonrpc": "2.0", "id": 5, "method": "tracon/preview", "params": {}}),
        )
        .await
        .unwrap();
        assert_eq!(preview["error"]["code"], -32601);
    }
}
