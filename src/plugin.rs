//! `hoja serve --socket <path> --data <dir>`: hoja as a Tracon plugin
//! (contract v1). The node serves hoja's page from the plugin archive and
//! forwards the page's `api/...` requests here over the Unix socket, each
//! with a context it signed for the operator. That signed context is what
//! makes a request the person's; it replaces the standalone server's Origin
//! and Host checks.
//!
//! The socket carries MCP at `POST /` (hoja offers no tools) and the person's
//! API at `/api/*`. It never carries the agent API: every request on it is
//! attested as the operator, so an agent route there would turn an agent's
//! write into the person's. Agents keep using the CLI against a separate
//! loopback port, `agent_listen`, when `plugin.toml` names one.
//!
//! The node starts hoja with an empty environment (no `HOME`, no `PATH`), so
//! everything else comes from `<data>/plugin.toml`.

use std::net::SocketAddr;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
};
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::config::Workspace;
use crate::contract::{self, ContractEnv, NonceCache, UiRequest};
use crate::http::{self, App};
use crate::render::Typst;
use crate::store::Store;

/// The contract version this build serves.
pub const CONTRACT: &str = "1";
/// The header the node puts its signed context in.
pub const CONTEXT_HEADER: &str = "tracon-context";
/// The most the node forwards; the handlers themselves take far less.
const MAX_FORWARDED_BODY: usize = 1024 * 1024;
/// The MCP revision hoja answers with when a client names none.
const PROTOCOL_VERSION: &str = "2025-06-18";
/// Where Typst is looked for when `plugin.toml` names none: there is no
/// `PATH` to search.
pub const TYPST_CANDIDATES: [&str; 4] = [
    "/home/linuxbrew/.linuxbrew/bin/typst",
    "/opt/homebrew/bin/typst",
    "/usr/local/bin/typst",
    "/usr/bin/typst",
];

/// `<data>/plugin.toml`, the channel's own configuration. Optional.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginConfig {
    /// The workspace: `hoja.toml`, `types/`, `templates/`, `hoja.sqlite3`.
    /// Default: the data directory itself. Relative paths are from it.
    pub workspace: Option<PathBuf>,
    /// The Typst compiler. Default: the first of [`TYPST_CANDIDATES`] found.
    pub typst: Option<PathBuf>,
    /// A loopback address to serve the agent API on, for the hoja CLI.
    /// Default: none, so agents can't reach this instance.
    pub agent_listen: Option<SocketAddr>,
}

impl PluginConfig {
    pub fn load(data: &Path) -> Result<Self> {
        let path = data.join("plugin.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let body = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let config: Self =
            toml::from_str(&body).with_context(|| format!("parsing {}", path.display()))?;
        if let Some(addr) = config.agent_listen
            && !addr.ip().is_loopback()
        {
            bail!("agent_listen must be a loopback address, not {addr}");
        }
        Ok(config)
    }

    pub fn workspace(&self, data: &Path) -> PathBuf {
        match &self.workspace {
            Some(path) => data.join(path),
            None => data.to_path_buf(),
        }
    }
}

/// The Typst binary to run: the configured one, else the first candidate
/// that exists. `None` when there is neither.
pub fn find_typst(configured: Option<&Path>, candidates: &[&str]) -> Option<PathBuf> {
    if let Some(path) = configured {
        return Some(path.to_path_buf());
    }
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

/// What every `/api` request on the socket is checked against.
pub struct Auth {
    pub env: ContractEnv,
    pub nonces: NonceCache,
}

/// Check the start environment: the contract's variables, and nothing else.
pub fn auth(get: impl Fn(&str) -> Option<String>) -> Result<Auth, String> {
    let token = get("TRACON_PLUGIN_TOKEN")
        .filter(|t| !t.is_empty())
        .ok_or("TRACON_PLUGIN_TOKEN is not set; serve --socket is started by a Tracon node")?;
    let key = get("TRACON_NODE_KEY")
        .filter(|k| !k.is_empty())
        .ok_or("TRACON_NODE_KEY is not set; serve --socket is started by a Tracon node")?;
    if let Some(v) = get("TRACON_CONTRACT").filter(|v| v != CONTRACT) {
        return Err(format!(
            "TRACON_CONTRACT is {v}; this build serves contract {CONTRACT}"
        ));
    }
    Ok(Auth {
        env: ContractEnv::new(&token, &key)?,
        nonces: NonceCache::default(),
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

fn unauthorized(message: &str) -> Response {
    http::error(StatusCode::UNAUTHORIZED, "unauthorized", message)
}

fn context_of(request: &axum::http::request::Parts) -> Option<Value> {
    let header = request.headers.get(CONTEXT_HEADER)?.to_str().ok()?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(header.trim())
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The socket's gate. `POST /` is MCP. `/api/agent/*` is refused outright.
/// Any other `/api` request runs only with a valid context for exactly that
/// request; nothing else is served.
async fn guard(State(auth): State<Arc<Auth>>, request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    if path == "/" {
        return next.run(request).await;
    }
    if http::is_agent_path(&path) {
        return http::error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "The agent API is not served on the plugin socket, where every request is the \
             operator's. Agents use the hoja CLI against `agent_listen`.",
        );
    }
    if path != "/api" && !path.starts_with("/api/") {
        return http::not_found().await;
    }
    let (parts, body) = request.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_FORWARDED_BODY).await else {
        return http::error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "invalid_request",
            "Request body could not be read or exceeds 1 MiB.",
        );
    };
    let Some(context) = context_of(&parts) else {
        return unauthorized("The request carries no readable tracon-context.");
    };
    let target = parts
        .uri
        .path_and_query()
        .map_or(path.as_str(), |pq| pq.as_str());
    let checked = contract::verify_ui(
        &context,
        &UiRequest {
            method: parts.method.as_str(),
            target,
            body: &bytes,
        },
        &auth.env,
        now_ms(),
        &auth.nonces,
    );
    if let Err(refusal) = checked {
        eprintln!(
            "hoja: refused {} {path}: {} (check {})",
            parts.method, refusal.reason, refusal.check
        );
        return unauthorized(&refusal.reason);
    }
    let request = Request::from_parts(parts, Body::from(bytes));
    let mutation = !matches!(*request.method(), Method::GET | Method::HEAD);
    let mut response = if mutation && !http::json_content(&request) {
        http::error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "JSON Content-Type is required.",
        )
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Answer one JSON-RPC message; `None` for a notification or a response.
pub fn mcp_answer(msg: &Value) -> Option<Value> {
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        if msg.get("result").is_some() || msg.get("error").is_some() {
            return None;
        }
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        return Some(rpc_error(id, -32600, "invalid request"));
    };
    let id = msg.get("id").cloned()?;
    let result = match method {
        "initialize" => {
            let asked = msg["params"]["protocolVersion"]
                .as_str()
                .unwrap_or(PROTOCOL_VERSION);
            json!({
                "protocolVersion": asked,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": contract::PLUGIN, "version": env!("CARGO_PKG_VERSION")},
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": []}),
        "tools/call" => return Some(rpc_error(id, -32602, "hoja has no tools")),
        other => return Some(rpc_error(id, -32601, &format!("no method {other}"))),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// MCP: one JSON-RPC message per POST, its answer as JSON, 202 for a
/// notification.
async fn mcp(body: Bytes) -> Response {
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(msg) => msg,
        Err(e) => {
            let answer = rpc_error(Value::Null, -32700, &format!("parse error: {e}"));
            return (StatusCode::BAD_REQUEST, Json(answer)).into_response();
        }
    };
    match mcp_answer(&msg) {
        Some(answer) => Json(answer).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// The socket's router: MCP and the person's API, nothing else.
pub fn router(app: App, auth: Arc<Auth>) -> Router {
    http::person_api()
        .route("/", post(mcp))
        .fallback(http::not_found)
        .layer(DefaultBodyLimit::max(http::BODY_LIMIT))
        .layer(middleware::from_fn_with_state(auth, guard))
        .with_state(app)
}

fn bind_socket(socket: &Path) -> Result<tokio::net::UnixListener> {
    match std::fs::symlink_metadata(socket) {
        Ok(m) if m.file_type().is_socket() => std::fs::remove_file(socket)
            .with_context(|| format!("removing the stale socket {}", socket.display()))?,
        Ok(_) => bail!(
            "{} exists and is not a socket; not replacing it",
            socket.display()
        ),
        Err(_) => {}
    }
    let listener = tokio::net::UnixListener::bind(socket)
        .with_context(|| format!("binding {}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restricting {}", socket.display()))?;
    Ok(listener)
}

async fn signalled() {
    let Ok(mut term) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    else {
        let _ = tokio::signal::ctrl_c().await;
        return;
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

pub async fn serve(socket: &Path, data: &Path) -> Result<()> {
    let auth = Arc::new(auth(|k| std::env::var(k).ok()).map_err(anyhow::Error::msg)?);
    let config = PluginConfig::load(data)?;
    let workspace = Workspace::open(&config.workspace(data))?;
    let store = Arc::new(Store::init(&workspace.database())?);
    let typst = match find_typst(config.typst.as_deref(), &TYPST_CANDIDATES) {
        Some(path) => {
            if !path.is_file() {
                eprintln!(
                    "hoja: typst = {} in plugin.toml does not exist; PDFs will fail",
                    path.display()
                );
            }
            path
        }
        None => {
            eprintln!(
                "hoja: no Typst compiler found (looked in {}); PDFs will fail until \
                 plugin.toml names one as `typst`",
                TYPST_CANDIDATES.join(", ")
            );
            PathBuf::from("typst")
        }
    };
    let app = App {
        store,
        typst: Arc::new(Typst {
            binary: typst,
            fonts: workspace.root.join("fonts"),
        }),
        workspace: Arc::new(workspace),
        // The socket has no origin: the node's signed context gates it.
        origin: String::new(),
    };
    let agent = match config.agent_listen {
        Some(addr) => {
            let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| {
                format!("binding agent_listen {addr} (is a standalone hoja serve still on it?)")
            })?;
            let bound = listener.local_addr()?;
            let agent_app = App {
                origin: format!("http://{bound}"),
                ..app.clone()
            };
            eprintln!("hoja: agent API on http://{bound}/api/agent");
            Some((listener, agent_app))
        }
        None => None,
    };
    let listener = bind_socket(socket)?;
    eprintln!(
        "hoja: plugin on {} ({})",
        socket.display(),
        app.workspace.root.display()
    );

    let (stop, stopped) = watch::channel(false);
    tokio::spawn(async move {
        signalled().await;
        let _ = stop.send(true);
    });
    let until_stopped = |mut rx: watch::Receiver<bool>| async move {
        let _ = rx.wait_for(|stop| *stop).await;
    };
    let agent_task = agent.map(|(listener, app)| {
        let serving = axum::serve(listener, http::agent_router(app))
            .with_graceful_shutdown(until_stopped(stopped.clone()));
        tokio::spawn(async move { serving.await })
    });
    let served = axum::serve(listener, router(app, auth))
        .with_graceful_shutdown(until_stopped(stopped))
        .await;
    let _ = std::fs::remove_file(socket);
    if let Some(task) = agent_task {
        if served.is_err() {
            task.abort();
        } else {
            task.await.context("the agent API task")??;
        }
    }
    served.context("serving the plugin socket")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typst_is_the_configured_one_or_the_first_found() {
        let dir = tempfile::tempdir().unwrap();
        let found = dir.path().join("typst");
        std::fs::write(&found, "").unwrap();
        let missing = dir.path().join("missing");
        let candidates = [missing.to_str().unwrap(), found.to_str().unwrap()];
        assert_eq!(find_typst(None, &candidates), Some(found.clone()));
        assert_eq!(find_typst(None, &candidates[..1]), None);
        let configured = Path::new("/somewhere/typst");
        assert_eq!(
            find_typst(Some(configured), &candidates),
            Some(configured.to_path_buf())
        );
    }

    #[test]
    fn plugin_toml_is_optional_strict_and_loopback_only() {
        let dir = tempfile::tempdir().unwrap();
        let config = PluginConfig::load(dir.path()).unwrap();
        assert_eq!(config.workspace(dir.path()), dir.path());
        assert!(config.agent_listen.is_none());

        let write = |body: &str| std::fs::write(dir.path().join("plugin.toml"), body).unwrap();
        write("workspace = \"ws\"\nagent_listen = \"127.0.0.1:4520\"\n");
        let config = PluginConfig::load(dir.path()).unwrap();
        assert_eq!(config.workspace(dir.path()), dir.path().join("ws"));
        assert_eq!(config.agent_listen.unwrap().port(), 4520);
        write("workspace = \"/abs\"\n");
        let config = PluginConfig::load(dir.path()).unwrap();
        assert_eq!(config.workspace(dir.path()), Path::new("/abs"));

        write("agent_listen = \"0.0.0.0:4520\"\n");
        assert!(PluginConfig::load(dir.path()).is_err());
        write("port = 4520\n");
        assert!(PluginConfig::load(dir.path()).is_err());
    }

    #[test]
    fn the_start_environment_is_checked() {
        let key = hex::encode([9u8; 32]);
        let env = |vars: &'static [(&'static str, &'static str)], key: String| {
            move |k: &str| {
                if k == "TRACON_NODE_KEY" {
                    return Some(key.clone());
                }
                vars.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert!(auth(env(&[("TRACON_PLUGIN_TOKEN", "t")], key.clone())).is_ok());
        assert!(auth(env(&[], key.clone())).is_err());
        assert!(
            auth(env(
                &[("TRACON_PLUGIN_TOKEN", "t"), ("TRACON_CONTRACT", "2")],
                key.clone()
            ))
            .is_err()
        );
    }

    #[test]
    fn mcp_answers_initialize_list_and_notifications() {
        let init = mcp_answer(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-03-26"}}))
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "hoja");
        let list = mcp_answer(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).unwrap();
        assert_eq!(list["result"]["tools"], json!([]));
        assert!(
            mcp_answer(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none()
        );
        let call = mcp_answer(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "x"}}))
        .unwrap();
        assert_eq!(call["error"]["code"], -32602);
    }
}
