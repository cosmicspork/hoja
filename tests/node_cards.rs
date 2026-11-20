//! Cards raised in Tracon's queue (`hoja::node_cards`), against a fake of
//! the socket the node serves a plugin: an agent's edit to a record the
//! person wrote becomes a card here, is raised there as a word diff, and is
//! decided here once the operator decides it there.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use hoja::config::Workspace;
use hoja::gates::{Actor, Author};
use hoja::model::{CreateRecord, Outcome, UpdateRecord};
use hoja::node_cards::{self, NodeSocket};
use hoja::store::Store;
use serde_json::{Map, Value, json};

const TOKEN: &str = "instance-token";

const RESUME: &str = r#"
first_draft = "human"
template = "resume.typ"

[kinds.bullet]
label = "text"
[kinds.bullet.fields.text]
type = "text"
required = true
[kinds.bullet.fields.tags]
type = "tags"
"#;

/// The node, as far as hoja uses it: cards asked, and decisions the test
/// sets.
#[derive(Clone, Default)]
struct Node {
    asked: Arc<Mutex<Vec<Value>>>,
    decided: Arc<Mutex<HashMap<String, Value>>>,
}

async fn rpc(
    State(node): State<Node>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> impl IntoResponse {
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if bearer != format!("Bearer {TOKEN}") {
        let error = json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32001, "message": "not this instance" } });
        return (StatusCode::UNAUTHORIZED, Json(error));
    }
    let params = &request["params"];
    let result = match request["method"].as_str() {
        Some("approval/request") => {
            let mut asked = node.asked.lock().unwrap();
            asked.push(params.clone());
            json!({ "approval_id": format!("node-{}", asked.len()), "state": "pending", "new": true })
        }
        Some("approval/status") => {
            let id = params["approval_id"].as_str().unwrap_or_default();
            node.decided
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .unwrap_or_else(|| json!({ "approval_id": id, "state": "still_waiting" }))
        }
        _ => {
            let error = json!({ "jsonrpc": "2.0", "id": request["id"], "error": { "code": -32601, "message": "no such method" } });
            return (StatusCode::OK, Json(error));
        }
    };
    (
        StatusCode::OK,
        Json(json!({ "jsonrpc": "2.0", "id": request["id"], "result": result })),
    )
}

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    node: Node,
    socket: NodeSocket,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("types")).unwrap();
    std::fs::write(root.join("types/resume.toml"), RESUME).unwrap();
    let store = Store::init(&root.join("hoja.sqlite3")).unwrap();
    let files = Workspace::open(&root).unwrap().type_files().unwrap();
    for file in &files {
        store
            .accept_type(&Actor::Person, &file.name, &file.hash, &files)
            .unwrap();
    }
    let path = root.join("node.sock");
    let node = Node::default();
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let app = Router::new().route("/", post(rpc)).with_state(node.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Fixture {
        _dir: dir,
        store,
        node,
        socket: NodeSocket::new(&path, TOKEN),
    }
}

fn fields(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

/// A bullet the person wrote, and an agent's edit of it: a card, since the
/// person writes the first draft of a resume.
fn agent_edit(f: &Fixture, agent: &str, text: &str) -> (String, String) {
    let record = match f
        .store
        .create_record(
            &Actor::Person,
            CreateRecord {
                type_name: "resume".into(),
                kind: "bullet".into(),
                fields: fields(&[("text", json!("Shipped the old sync engine"))]),
                note: String::new(),
            },
        )
        .unwrap()
    {
        Outcome::Applied { result } => result,
        other => panic!("{other:?}"),
    };
    let outcome = f
        .store
        .update_record(
            &Actor::Agent(agent.into()),
            &record.id,
            UpdateRecord {
                version: record.version,
                fields: fields(&[("text", json!(text))]),
                note: "tighter wording".into(),
            },
        )
        .unwrap();
    match outcome {
        Outcome::AwaitingPerson { approval_id } => (record.id, approval_id),
        other => panic!("an agent's edit of the person's draft is a card: {other:?}"),
    }
}

#[tokio::test]
async fn an_approved_node_card_lands_the_edit_as_the_agents() {
    let f = fixture().await;
    let context =
        json!({ "v": 1, "caller": { "kind": "agent", "session": "s1", "harness": "claude" } });
    let agent = node_cards::agent_of(&context).unwrap();
    assert_eq!(agent, "claude");
    let (record_id, card) = agent_edit(&f, &agent, "Shipped the new sync engine");

    let approval = node_cards::raise(&f.store, &f.socket, &card, Some(&context))
        .await
        .unwrap();
    assert_eq!(approval, "node-1");
    // Raised once, however often it is asked.
    assert_eq!(
        node_cards::raise(&f.store, &f.socket, &card, Some(&context))
            .await
            .unwrap(),
        "node-1"
    );
    let asked = f.node.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    let asked = &asked[0];
    assert_eq!(asked["title"], "Edit a resume record");
    assert!(
        asked["summary"]
            .as_str()
            .unwrap()
            .contains("**claude** asks to edit")
    );
    assert!(
        asked["summary"]
            .as_str()
            .unwrap()
            .contains("> tighter wording")
    );
    assert_eq!(asked["on_behalf"], context);
    assert_eq!(
        asked["content"],
        json!([{ "format": "word_diff", "title": "Text", "before": "Shipped the old sync engine", "after": "Shipped the new sync engine" }])
    );

    // Nothing decided there yet: nothing decided here.
    assert!(
        node_cards::settle(&f.store, &f.socket)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(f.store.approval(&card).unwrap().state, "pending");

    f.node.decided.lock().unwrap().insert(
        approval.clone(),
        json!({ "approval_id": approval, "state": "approved", "notes": null }),
    );
    let settled = node_cards::settle(&f.store, &f.socket).await.unwrap();
    assert_eq!(settled.len(), 1);
    assert_eq!(settled[0].state, "accepted");
    let record = f.store.record(&record_id).unwrap();
    assert_eq!(record.fields["text"], "Shipped the new sync engine");
    let revisions = f.store.revisions("record", &record_id).unwrap();
    let last = revisions.iter().max_by_key(|r| r.version).unwrap();
    assert_eq!(last.author, Author::AgentAccepted);
    assert_eq!(last.agent.as_deref(), Some("claude"));
    assert_eq!(last.approval_id.as_deref(), Some(card.as_str()));
    // Settled once.
    assert!(
        node_cards::settle(&f.store, &f.socket)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn refused_sent_back_expired_and_stale_cards_change_nothing() {
    let f = fixture().await;
    let mut cards = Vec::new();
    for (i, decision) in [
        json!({ "state": "rejected", "reason": "not true" }),
        json!({ "state": "changes_requested", "notes": "say which engine" }),
        json!({ "state": "expired" }),
    ]
    .into_iter()
    .enumerate()
    {
        let (record, card) = agent_edit(&f, "claude", &format!("Edit {i}"));
        let approval = node_cards::raise(&f.store, &f.socket, &card, None)
            .await
            .unwrap();
        f.node.decided.lock().unwrap().insert(approval, decision);
        cards.push((record, card));
    }
    let settled = node_cards::settle(&f.store, &f.socket).await.unwrap();
    let states: Vec<(&str, Option<&str>)> = settled
        .iter()
        .map(|s| (s.state.as_str(), s.reason.as_deref()))
        .collect();
    assert_eq!(
        states,
        [
            ("rejected", Some("not true")),
            ("changes_requested", Some("say which engine")),
            ("rejected", Some("the card expired unanswered in Tracon")),
        ]
    );
    for (record, _) in &cards {
        assert_eq!(
            f.store.record(record).unwrap().fields["text"],
            "Shipped the old sync engine"
        );
    }

    // Approved there after the person changed the record here: stale, so
    // rejected rather than applied over their change.
    let (record_id, card) = agent_edit(&f, "claude", "Shipped the agent's engine");
    let approval = node_cards::raise(&f.store, &f.socket, &card, None)
        .await
        .unwrap();
    let record = f.store.record(&record_id).unwrap();
    f.store
        .update_record(
            &Actor::Person,
            &record_id,
            UpdateRecord {
                version: record.version,
                fields: fields(&[("text", json!("Shipped the person's engine"))]),
                note: String::new(),
            },
        )
        .unwrap();
    f.node
        .decided
        .lock()
        .unwrap()
        .insert(approval, json!({ "state": "approved" }));
    let settled = node_cards::settle(&f.store, &f.socket).await.unwrap();
    assert_eq!(settled[0].state, "rejected");
    assert!(
        settled[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("moved since the agent read it")
    );
    assert_eq!(
        f.store.record(&record_id).unwrap().fields["text"],
        "Shipped the person's engine"
    );
}

#[tokio::test]
async fn the_node_refuses_another_token() {
    let f = fixture().await;
    let (_, card) = agent_edit(&f, "claude", "x");
    let wrong = NodeSocket::new(&f.socket.path, "not-it");
    let error = node_cards::raise(&f.store, &wrong, &card, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not this instance"), "{error}");
    assert_eq!(f.store.node_card(&card).unwrap(), None);
    // Only an agent's context names an agent.
    assert_eq!(
        node_cards::agent_of(&json!({ "caller": { "kind": "operator" } })),
        None
    );
}
