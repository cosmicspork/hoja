//! hoja's cards raised in Tracon's queue instead of its own, for when hoja
//! runs as a Tracon plugin.
//!
//! The node serves each plugin instance a socket (`--node-socket`) where the
//! plugin calls it back with its `TRACON_PLUGIN_TOKEN`. hoja uses two of its
//! methods: `approval/request` puts a card in the operator's queue, with the
//! agent's change as a word diff per field, and `approval/status` reads the
//! operator's decision. A card is raised once, linked to the Tracon approval
//! (`Store::link_node_card`), and [`settle`] decides it here as the person:
//! approved is accepted, so the change lands with its revision attributed to
//! the agent that asked (`agent_accepted`); rejected, sent back or expired is
//! rejected or sent back with the operator's words. Every gate still holds:
//! a card whose target moved since the agent read it is stale, and is
//! rejected rather than accepted.
//!
//! Raised for an agent's call, a card passes that call's signed caller
//! context (`_meta.tracon`) as `on_behalf`, so the node files it with the
//! agent's session as well, and hoja names the agent from that same context
//! when it records the card.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::gates::Actor;
use crate::model::{Action, Approval, Decide, Decision};
use crate::store::{Store, StoreError};

/// The node methods a plugin that raises its cards there names in its
/// manifest (`node_methods`).
pub const NODE_METHODS: &[&str] = &["approval/request", "approval/status"];

/// The longest a response from the node may be.
const MAX_RESPONSE: u64 = 4 << 20;

/// The longest one call to the node may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Why a call to the node failed.
#[derive(Debug)]
pub enum NodeError {
    /// The socket could not be reached, or answered nonsense.
    Transport(String),
    /// The node answered with a JSON-RPC error.
    Refused { code: i64, message: String },
    /// hoja's own store.
    Store(StoreError),
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(why) => write!(f, "the node could not be reached: {why}"),
            Self::Refused { code, message } => write!(f, "the node refused ({code}): {message}"),
            Self::Store(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for NodeError {}

impl From<StoreError> for NodeError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

/// The socket the node serves this instance, and the token that opens it.
#[derive(Debug, Clone)]
pub struct NodeSocket {
    pub path: PathBuf,
    pub token: String,
}

impl NodeSocket {
    pub fn new(path: impl AsRef<Path>, token: impl Into<String>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            token: token.into(),
        }
    }

    /// One JSON-RPC call: `POST /` over the Unix socket, bearer token,
    /// `Connection: close`.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, NodeError> {
        let body =
            json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string();
        let exchange = async {
            let mut stream = tokio::net::UnixStream::connect(&self.path).await?;
            let request = format!(
                "POST / HTTP/1.1\r\nHost: node\r\nAuthorization: Bearer {}\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                self.token,
                body.len()
            );
            stream.write_all(request.as_bytes()).await?;
            let mut response = Vec::new();
            stream.take(MAX_RESPONSE).read_to_end(&mut response).await?;
            Ok::<_, std::io::Error>(response)
        };
        let response = tokio::time::timeout(CALL_TIMEOUT, exchange)
            .await
            .map_err(|_| NodeError::Transport(format!("{method} took over {CALL_TIMEOUT:?}")))?
            .map_err(|e| NodeError::Transport(e.to_string()))?;
        let text = String::from_utf8_lossy(&response);
        let (head, body) = text
            .split_once("\r\n\r\n")
            .ok_or_else(|| NodeError::Transport("no HTTP response".into()))?;
        if head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
        {
            return Err(NodeError::Transport("a chunked response".into()));
        }
        let reply: Value = serde_json::from_str(body.trim())
            .map_err(|e| NodeError::Transport(format!("not JSON-RPC: {e}")))?;
        if let Some(error) = reply.get("error").filter(|e| !e.is_null()) {
            return Err(NodeError::Refused {
                code: error["code"].as_i64().unwrap_or(0),
                message: error["message"].as_str().unwrap_or_default().to_string(),
            });
        }
        Ok(reply["result"].clone())
    }
}

/// A stored value as text a word diff reads: list entries one a line.
fn text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "yes" } else { "no" }.into(),
        Value::Array(items) => items.iter().map(text).collect::<Vec<_>>().join("\n"),
        other => other.to_string(),
    }
}

fn titled(name: &str) -> String {
    let spaced = name.replace(['_', '-'], " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => spaced,
    }
}

/// What a card asks, as the node's card shows it: `(title, summary,
/// content)`. A change to an existing record or document is a word diff
/// per field, from the target as it stands to what the agent wants; a new
/// one is its fields as a table.
pub fn card_content(card: &Approval) -> (String, String, Vec<Value>) {
    let fields = card.payload["fields"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    let current = &card.current["fields"];
    let what = match card.action {
        Action::RecordCreate => format!(
            "Create a {} in {}",
            card.payload["kind"].as_str().unwrap_or("record"),
            card.type_name
        ),
        Action::RecordUpdate => format!("Edit a {} record", card.type_name),
        Action::DocumentCreate => format!("Create a {} document", card.type_name),
        Action::DocumentUpdate => format!("Edit a {} document", card.type_name),
        Action::DocumentSelect => format!("Change a {} document's selection", card.type_name),
    };
    let mut summary = format!("**{}** asks to {}.", card.agent, lower_first(&what));
    if !card.note.trim().is_empty() {
        summary.push_str(&format!("\n\n> {}", card.note.trim().replace('\n', "\n> ")));
    }
    if card.stale {
        summary.push_str(
            "\n\n**Stale:** what this changes moved since the agent read it; approving it is \
             refused, so it will be rejected.",
        );
    }
    let mut content = Vec::new();
    match card.action {
        Action::RecordUpdate | Action::DocumentUpdate => {
            if let Some(title) = card.payload["title"].as_str() {
                content.push(json!({
                    "format": "word_diff",
                    "title": "Title",
                    "before": text(&card.current["title"]),
                    "after": title,
                }));
            }
            for (name, after) in &fields {
                content.push(json!({
                    "format": "word_diff",
                    "title": titled(name),
                    "before": text(&current[name]),
                    "after": text(after),
                }));
            }
        }
        Action::RecordCreate | Action::DocumentCreate => {
            let mut rows: Vec<Value> = Vec::new();
            if let Some(title) = card.payload["title"].as_str() {
                rows.push(json!({ "key": "Title", "value": title }));
            }
            for (name, value) in &fields {
                rows.push(json!({ "key": titled(name), "value": text(value) }));
            }
            content.push(json!({ "format": "table", "rows": rows }));
        }
        Action::DocumentSelect => {}
    }
    if content.is_empty() {
        content.push(json!({
            "format": "source",
            "language": "json",
            "title": "Change",
            "text": serde_json::to_string_pretty(&card.payload).unwrap_or_default(),
        }));
    }
    (what, summary, content)
}

fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Raise card `card_id` in Tracon, once: the Tracon approval's id. A card
/// already raised is not raised again.
pub async fn raise(
    store: &Store,
    node: &NodeSocket,
    card_id: &str,
    on_behalf: Option<&Value>,
) -> Result<String, NodeError> {
    if let Some(approval_id) = store.node_card(card_id)? {
        return Ok(approval_id);
    }
    let card = store.approval(card_id)?;
    let (title, summary, content) = card_content(&card);
    let mut params = json!({
        "title": title,
        "summary": summary,
        "content": content,
        "request_key": format!("hoja-card-{card_id}"),
    });
    if let Some(context) = on_behalf {
        params["on_behalf"] = context.clone();
    }
    let raised = node.call("approval/request", params).await?;
    let approval_id = raised["approval_id"]
        .as_str()
        .ok_or_else(|| NodeError::Transport("approval/request named no approval".into()))?
        .to_string();
    store.link_node_card(card_id, &approval_id)?;
    Ok(approval_id)
}

/// What [`settle`] did with one card.
#[derive(Debug, Clone, PartialEq)]
pub struct Settled {
    pub card_id: String,
    pub approval_id: String,
    /// `accepted`, `rejected` or `changes_requested`: the card's state here.
    pub state: String,
    pub reason: Option<String>,
}

/// Decide here every card raised in Tracon that the operator has decided
/// there, as the person. One the node cannot say anything about yet is left
/// waiting.
pub async fn settle(store: &Store, node: &NodeSocket) -> Result<Vec<Settled>, NodeError> {
    let mut out = Vec::new();
    for (card_id, approval_id) in store.node_cards_waiting()? {
        let status = node
            .call(
                "approval/status",
                json!({ "approval_id": approval_id, "wait_secs": 0 }),
            )
            .await?;
        let words = |key: &str| {
            status[key]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let decide = match status["state"].as_str() {
            Some("approved") => Decide {
                decision: Decision::Accept,
                reason: words("notes"),
                payload: None,
            },
            Some("rejected") => Decide {
                decision: Decision::Reject,
                reason: Some(words("reason").unwrap_or_else(|| "rejected in Tracon".into())),
                payload: None,
            },
            Some("changes_requested") => Decide {
                decision: Decision::RequestChanges,
                reason: Some(
                    words("notes").unwrap_or_else(|| "changes requested in Tracon".into()),
                ),
                payload: None,
            },
            Some("expired") => Decide {
                decision: Decision::Reject,
                reason: Some("the card expired unanswered in Tracon".into()),
                payload: None,
            },
            _ => continue,
        };
        let decided = match store.decide(&Actor::Person, &card_id, decide) {
            // Approved, but what it changes moved since the agent read it: a
            // card applies only to what the agent saw.
            Err(StoreError::Stale) => store.decide(
                &Actor::Person,
                &card_id,
                Decide {
                    decision: Decision::Reject,
                    reason: Some(
                        "approved in Tracon, but what it changes moved since the agent read it"
                            .into(),
                    ),
                    payload: None,
                },
            )?,
            other => other?,
        };
        out.push(Settled {
            card_id,
            approval_id,
            state: decided.state,
            reason: decided.reason,
        });
    }
    Ok(out)
}

/// The agent a caller context names, as hoja records it: its harness, such
/// as `claude`. Only for a context the plugin has already checked is the
/// node's, for this call (contract v1, checks 1 to 6).
pub fn agent_of(context: &Value) -> Option<String> {
    let caller = &context["caller"];
    if caller["kind"] != "agent" {
        return None;
    }
    caller["harness"]
        .as_str()
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(str::to_string)
}
