//! Cards raised in Tracon's queue (`hoja::node_cards`), against a fake of
//! the socket the node serves a plugin: an agent's edit to a record the
//! person wrote becomes a card here, is raised there as a word diff, and is
//! decided here once the operator decides it there. One the person decides
//! here first is withdrawn there.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
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
use hoja::model::{CreateRecord, Decide, Decision, Outcome, UpdateRecord};
use hoja::node_cards::{self, NodeSocket, Settler};
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

/// The node, as far as hoja uses it: cards asked, decisions the test sets,
/// and every `approval/withdraw` asked of it. With `no_withdraw` it answers
/// as a node whose manifest for hoja doesn't name that method.
#[derive(Clone, Default)]
struct Node {
    asked: Arc<Mutex<Vec<Value>>>,
    decided: Arc<Mutex<HashMap<String, Value>>>,
    withdrawals: Arc<Mutex<Vec<Value>>>,
    no_withdraw: Arc<AtomicBool>,
}

impl Node {
    fn withdrawals(&self) -> Vec<Value> {
        self.withdrawals.lock().unwrap().clone()
    }
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
            // A card the node has no record of (its state was reset).
            if id.starts_with("forgotten-") {
                let error = json!({ "jsonrpc": "2.0", "id": request["id"], "error": { "code": -32602, "message": format!("no card {id} raised by plugin hoja on personal") } });
                return (StatusCode::OK, Json(error));
            }
            node.decided
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .unwrap_or_else(|| json!({ "approval_id": id, "state": "still_waiting" }))
        }
        Some("approval/withdraw") => {
            node.withdrawals.lock().unwrap().push(params.clone());
            let id = params["approval_id"].as_str().unwrap_or_default();
            if node.no_withdraw.load(Ordering::SeqCst) {
                let error = json!({ "jsonrpc": "2.0", "id": request["id"], "error": { "code": -32601, "message": "approval/withdraw is not in hoja's node_methods, so the node does not answer it" } });
                return (StatusCode::OK, Json(error));
            }
            if id.starts_with("forgotten-") {
                let error = json!({ "jsonrpc": "2.0", "id": request["id"], "error": { "code": -32602, "message": format!("no card {id} raised by plugin hoja on personal") } });
                return (StatusCode::OK, Json(error));
            }
            // A card already settled is answered as it stands.
            let mut decided = node.decided.lock().unwrap();
            match decided.get(id) {
                Some(settled) => {
                    let mut answer = settled.clone();
                    answer["withdrew"] = json!(false);
                    answer
                }
                None => {
                    let withdrawn = json!({ "approval_id": id, "state": "withdrawn", "reason": params["reason"] });
                    decided.insert(id.to_string(), withdrawn.clone());
                    let mut answer = withdrawn;
                    answer["withdrew"] = json!(true);
                    answer
                }
            }
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

#[tokio::test]
async fn a_card_the_node_does_not_know_goes_back_to_hojas_queue() {
    let f = fixture().await;
    let (_, lost) = agent_edit(&f, "claude", "Lost there");
    f.store.link_node_card(&lost, "forgotten-1").unwrap();
    let (record_id, card) = agent_edit(&f, "claude", "Shipped the new sync engine");
    let approval = node_cards::raise(&f.store, &f.socket, &card, None)
        .await
        .unwrap();
    f.node
        .decided
        .lock()
        .unwrap()
        .insert(approval, json!({ "state": "approved" }));

    // The card the node can't answer for doesn't hold up the others.
    let settled = node_cards::settle(&f.store, &f.socket).await.unwrap();
    assert_eq!(settled.len(), 1);
    assert_eq!(settled[0].card_id, card);
    assert_eq!(
        f.store.record(&record_id).unwrap().fields["text"],
        "Shipped the new sync engine"
    );
    // Unlinked, it waits for the person in hoja's page, and is not asked
    // about again.
    assert_eq!(f.store.node_card(&lost).unwrap(), None);
    assert_eq!(f.store.approval(&lost).unwrap().state, "pending");
    assert!(f.store.node_cards_waiting().unwrap().is_empty());
}

/// The person decides card `card` in hoja's page, as `http::decide` does.
fn person_decides(f: &Fixture, card: &str, decision: Decision, reason: Option<&str>) {
    f.store
        .decide(
            &Actor::Person,
            card,
            Decide {
                decision,
                reason: reason.map(str::to_string),
                payload: None,
            },
        )
        .unwrap();
}

#[tokio::test]
async fn a_card_decided_in_hojas_page_is_withdrawn_from_tracons_queue_once() {
    let f = fixture().await;
    let mut raised = Vec::new();
    for (i, (decision, reason)) in [
        (Decision::Accept, None),
        (Decision::Reject, Some("not true")),
        (Decision::RequestChanges, Some("say which engine")),
    ]
    .into_iter()
    .enumerate()
    {
        let (_, card) = agent_edit(&f, "claude", &format!("Edit {i}"));
        let approval = node_cards::raise(&f.store, &f.socket, &card, None)
            .await
            .unwrap();
        person_decides(&f, &card, decision, reason);
        raised.push((card, approval));
    }

    let mut settler = Settler::default();
    let log = settler.pass(&f.store, &f.socket).await;
    assert_eq!(
        f.node.withdrawals(),
        [
            json!({ "approval_id": raised[0].1, "reason": "accepted in hoja's page" }),
            json!({ "approval_id": raised[1].1, "reason": "rejected in hoja's page" }),
            json!({ "approval_id": raised[2].1, "reason": "changes requested in hoja's page" }),
        ]
    );
    assert_eq!(log.len(), 3, "{log:?}");
    assert!(log[0].contains(&format!("withdrew Tracon approval {}", raised[0].1)));
    // Decided here, and nothing there changes that.
    let states: Vec<String> = raised
        .iter()
        .map(|(card, _)| f.store.approval(card).unwrap().state)
        .collect();
    assert_eq!(states, ["accepted", "rejected", "changes_requested"]);

    // Closed: never asked again, by this run or the next.
    assert!(f.store.node_cards_to_withdraw().unwrap().is_empty());
    assert!(settler.pass(&f.store, &f.socket).await.is_empty());
    assert!(
        Settler::default()
            .pass(&f.store, &f.socket)
            .await
            .is_empty()
    );
    assert_eq!(f.node.withdrawals().len(), 3);
}

#[tokio::test]
async fn a_card_settled_from_tracon_is_never_withdrawn() {
    let f = fixture().await;
    for (i, decision) in [
        json!({ "state": "approved" }),
        json!({ "state": "rejected", "reason": "no" }),
        json!({ "state": "changes_requested", "notes": "again" }),
        json!({ "state": "expired" }),
    ]
    .into_iter()
    .enumerate()
    {
        let (_, card) = agent_edit(&f, "claude", &format!("Edit {i}"));
        let approval = node_cards::raise(&f.store, &f.socket, &card, None)
            .await
            .unwrap();
        f.node.decided.lock().unwrap().insert(approval, decision);
    }
    let mut settler = Settler::default();
    assert_eq!(settler.pass(&f.store, &f.socket).await.len(), 4);
    assert!(settler.pass(&f.store, &f.socket).await.is_empty());
    assert!(f.node.withdrawals().is_empty());
    assert!(f.store.node_cards_to_withdraw().unwrap().is_empty());
}

#[tokio::test]
async fn without_approval_withdraw_a_decided_card_waits_there_and_others_settle() {
    let f = fixture().await;
    f.node.no_withdraw.store(true, Ordering::SeqCst);
    let (_, decided_here) = agent_edit(&f, "claude", "Decided here");
    node_cards::raise(&f.store, &f.socket, &decided_here, None)
        .await
        .unwrap();
    person_decides(&f, &decided_here, Decision::Accept, None);
    let (_, decided_there) = agent_edit(&f, "claude", "Decided there");
    let there = node_cards::raise(&f.store, &f.socket, &decided_there, None)
        .await
        .unwrap();
    f.node
        .decided
        .lock()
        .unwrap()
        .insert(there, json!({ "state": "approved" }));

    let mut settler = Settler::default();
    let log = settler.pass(&f.store, &f.socket).await;
    assert_eq!(log.len(), 2, "{log:?}");
    assert!(log[0].contains("accepted from Tracon approval"), "{log:?}");
    assert!(log[1].contains("can't withdraw cards"), "{log:?}");
    assert_eq!(f.store.approval(&decided_here).unwrap().state, "accepted");
    assert_eq!(f.store.approval(&decided_there).unwrap().state, "accepted");
    assert_eq!(f.node.withdrawals().len(), 1);

    // Said once, and not asked again this run; settling goes on.
    let (_, later) = agent_edit(&f, "claude", "Later");
    let approval = node_cards::raise(&f.store, &f.socket, &later, None)
        .await
        .unwrap();
    f.node
        .decided
        .lock()
        .unwrap()
        .insert(approval, json!({ "state": "rejected", "reason": "no" }));
    let log = settler.pass(&f.store, &f.socket).await;
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(log[0].contains("rejected from Tracon approval"), "{log:?}");
    assert!(settler.pass(&f.store, &f.socket).await.is_empty());
    assert_eq!(f.node.withdrawals().len(), 1);
    // The link stays open, for a node that can withdraw it later.
    assert_eq!(f.store.node_cards_to_withdraw().unwrap().len(), 1);
}

#[tokio::test]
async fn a_decided_card_the_node_does_not_know_is_closed() {
    let f = fixture().await;
    let (_, card) = agent_edit(&f, "claude", "Lost there");
    f.store.link_node_card(&card, "forgotten-2").unwrap();
    person_decides(&f, &card, Decision::Accept, None);
    let withdrawn = node_cards::withdraw_decided(&f.store, &f.socket)
        .await
        .unwrap();
    assert_eq!(withdrawn.len(), 1);
    assert_eq!(withdrawn[0].approval_id, "forgotten-2");
    assert!(!withdrawn[0].withdrew);
    assert!(f.store.node_cards_to_withdraw().unwrap().is_empty());
    assert_eq!(f.node.withdrawals().len(), 1);
}

#[tokio::test]
async fn a_card_decided_here_before_withdrawing_existed_is_withdrawn_on_upgrade() {
    let f = fixture().await;
    let (_, card) = agent_edit(&f, "claude", "Decided last week");
    let approval = node_cards::raise(&f.store, &f.socket, &card, None)
        .await
        .unwrap();
    person_decides(&f, &card, Decision::Accept, None);

    // The database as the previous release left it: schema version 3, with
    // no `closed_at` on its links.
    let Fixture {
        _dir,
        store,
        node,
        socket,
    } = f;
    drop(store);
    let path = _dir.path().join("hoja.sqlite3");
    let old = rusqlite::Connection::open(&path).unwrap();
    old.execute_batch("ALTER TABLE node_cards DROP COLUMN closed_at; PRAGMA user_version=3;")
        .unwrap();
    drop(old);

    let store = Store::init(&path).unwrap();
    let log = Settler::default().pass(&store, &socket).await;
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(
        node.withdrawals(),
        [json!({ "approval_id": approval, "reason": "accepted in hoja's page" })]
    );
    assert!(store.node_cards_to_withdraw().unwrap().is_empty());
}
