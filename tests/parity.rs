//! The tools and the HTTP API can't drift apart. Every agent API route is one
//! operation in `ops::OPS` and one tool; every route of the person's API is
//! either the person's copy of a tool or listed below with the reason it is
//! not one. A new route, or a tool without a route, fails here until it is
//! given a tool or a reason.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use axum::http::Method;
use hoja::config::Workspace;
use hoja::gates::Actor;
use hoja::http::{self, App};
use hoja::ops;
use hoja::render::Typst;
use hoja::store::Store;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// What a person route is, beside the tools.
enum Person {
    /// The person's copy of this tool's route: the same read, or the same
    /// write recorded as the person's rather than an agent's.
    Tool(&'static str),
    /// Not a tool, and why.
    Excluded(&'static str),
}

use Person::{Excluded, Tool};

/// Every route of the person's API. Operator-only actions are excluded rather
/// than offered as consequential tools: what makes them the person's is that
/// the person does them in hoja's page, where the content hash, the word diff
/// and the provenance view they decide on are shown. A Tracon card allowing an
/// agent's call would make the agent their author.
const PERSON_ROUTES: &[(&str, &str, Person)] = &[
    (
        "GET",
        "/api/state",
        Excluded(
            "The page's overview: the workspace path and the type and template files waiting \
             for the person to accept them. Agents read the accepted types with `types`.",
        ),
    ),
    (
        "GET",
        "/api/types/{name}/change",
        Excluded("The person's review of a type file before accepting it."),
    ),
    (
        "POST",
        "/api/types/{name}/accept",
        Excluded("Person only: accepting a type file makes it the rules agents write under."),
    ),
    (
        "GET",
        "/api/templates/change",
        Excluded("The person's review of a template file before accepting it."),
    ),
    (
        "POST",
        "/api/templates/accept",
        Excluded("Person only: an accepted template is what approved documents render with."),
    ),
    (
        "POST",
        "/api/templates/preview",
        Excluded(
            "Renders a template not yet accepted, as PDF bytes, for the person deciding \
             whether to accept it.",
        ),
    ),
    ("GET", "/api/records", Tool("records")),
    ("POST", "/api/records", Tool("record_create")),
    ("GET", "/api/records/{id}", Tool("record_show")),
    ("PUT", "/api/records/{id}", Tool("record_edit")),
    (
        "POST",
        "/api/records/{id}/reviewed",
        Excluded("Person only: marking content reviewed is the person's word that it is right."),
    ),
    (
        "POST",
        "/api/records/{id}/delete",
        Excluded("Person only: agents never delete records."),
    ),
    ("GET", "/api/documents", Tool("documents")),
    ("POST", "/api/documents", Tool("document_create")),
    (
        "GET",
        "/api/documents/{id}",
        Excluded(
            "The page's document view: what document_show gives, plus the provenance marks \
             and revision history the person reviews before approving.",
        ),
    ),
    ("PUT", "/api/documents/{id}", Tool("document_edit")),
    (
        "PUT",
        "/api/documents/{id}/sections/{section}",
        Tool("document_select"),
    ),
    (
        "POST",
        "/api/documents/{id}/approve",
        Excluded("Person only: approval is of the content hash the person was shown."),
    ),
    (
        "GET",
        "/api/documents/{id}/preview.pdf",
        Excluded("PDF bytes for the page's preview frame. Agents render with document_build."),
    ),
    ("POST", "/api/documents/{id}/export", Tool("document_build")),
    (
        "POST",
        "/api/markdown",
        Excluded("The prose editor's live preview in the page."),
    ),
    (
        "GET",
        "/api/approvals",
        Excluded(
            "The person's queue of every agent's cards. An agent follows its own cards by \
             id with card_status.",
        ),
    ),
    (
        "POST",
        "/api/approvals/{id}/decide",
        Excluded("Person only: accepting, rejecting or sending back an agent's card."),
    ),
];

fn routes(list: &[(Method, String)]) -> BTreeSet<(String, String)> {
    let set: BTreeSet<_> = list
        .iter()
        .map(|(m, p)| (m.to_string(), p.clone()))
        .collect();
    assert_eq!(set.len(), list.len(), "a route is registered twice");
    set
}

#[test]
fn every_agent_route_is_a_tool_and_every_tool_a_route() {
    let served = routes(http::agent_api().routes());
    let from_ops: BTreeSet<_> = ops::OPS
        .iter()
        .map(|op| (op.verb.method().to_string(), op.route()))
        .collect();
    assert_eq!(served, from_ops);

    let tools: Vec<Value> = ops::tools();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    let op_names: Vec<&str> = ops::OPS.iter().map(|op| op.name).collect();
    assert_eq!(names, op_names);

    for (op, tool) in ops::OPS.iter().zip(&tools) {
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], "object", "{}", op.name);
        let required: Vec<&str> = schema["required"]
            .as_array()
            .map(|r| r.iter().map(|v| v.as_str().unwrap()).collect())
            .unwrap_or_default();
        for param in op.path_params() {
            assert!(required.contains(&param), "{}: {param}", op.name);
            assert_eq!(schema["properties"][param]["type"], "string");
        }
        assert_eq!(
            tool["annotations"]["readOnlyHint"], !op.writes,
            "{}",
            op.name
        );
        // A float has no canonical form in the plugin contract.
        assert!(!schema.to_string().contains("\"number\""), "{}", op.name);
        // Free text is marked so a Tracon card renders it as prose.
        if let Some(note) = schema["properties"].get("note") {
            assert_eq!(note["x-tracon-format"], "markdown", "{}", op.name);
        }
    }
}

#[test]
fn every_person_route_is_a_tool_or_excluded_with_a_reason() {
    let served = routes(http::person_api().routes());
    let listed: BTreeSet<(String, String)> = PERSON_ROUTES
        .iter()
        .map(|(m, p, _)| (m.to_string(), p.to_string()))
        .collect();
    let unlisted: Vec<_> = served.difference(&listed).collect();
    assert!(
        unlisted.is_empty(),
        "person routes with neither a tool nor a reason: {unlisted:?}"
    );
    let gone: Vec<_> = listed.difference(&served).collect();
    assert!(gone.is_empty(), "listed but not served: {gone:?}");
    for (method, path, person) in PERSON_ROUTES {
        match person {
            Tool(name) => assert!(
                ops::find(name).is_some(),
                "{method} {path} names no tool {name}"
            ),
            Excluded(why) => assert!(!why.trim().is_empty(), "{method} {path}"),
        }
    }
    // No tool reaches an operator-only route's path.
    let person_only = ["reviewed", "delete", "approve", "decide", "accept"];
    for op in ops::OPS {
        for word in person_only {
            assert!(!op.path.contains(word), "{} reaches {word}", op.name);
            assert!(!op.name.contains(word), "{} is {word}", op.name);
        }
    }
}

// ---- the two surfaces answer alike ----------------------------------------

fn copy_example(to: &Path) {
    let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/project");
    for dir in ["", "types", "templates", "samples"] {
        std::fs::create_dir_all(to.join(dir)).unwrap();
        for entry in std::fs::read_dir(from.join(dir)).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_file() {
                std::fs::copy(entry.path(), to.join(dir).join(entry.file_name())).unwrap();
            }
        }
    }
}

async fn send(port: u16, method: &str, target: &str, body: &str) -> (u16, Value) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let request = format!(
        "{method} {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Hoja-Agent: claude\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        let mut out = String::new();
        let mut rest = body;
        while let Some((size, tail)) = rest.split_once("\r\n") {
            let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
            if n == 0 {
                break;
            }
            out.push_str(&tail[..n]);
            rest = &tail[n + 2..];
        }
        out
    } else {
        body.to_string()
    };
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

#[tokio::test]
async fn each_route_and_its_tool_give_the_same_answer() {
    let dir = tempfile::tempdir().unwrap();
    copy_example(dir.path());
    let workspace = Workspace::open(dir.path()).unwrap();
    let store = Arc::new(Store::init(&workspace.database()).unwrap());
    for file in workspace.type_files().unwrap() {
        store
            .accept_type(
                &Actor::Person,
                &file.name,
                &file.hash,
                &workspace.type_files().unwrap(),
            )
            .unwrap();
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = App {
        store,
        typst: Arc::new(Typst {
            binary: "typst".into(),
            fonts: workspace.root.join("fonts"),
        }),
        workspace: Arc::new(workspace),
        origin: format!("http://127.0.0.1:{port}"),
    };
    let router = http::router(app.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let tool = |name: &str, args: Value| {
        let app = app.clone();
        let op = ops::find(name).unwrap();
        async move {
            let response = ops::run(&app, Actor::Agent("claude".into()), op, args).await;
            let (status, body) = ops::answer(response).await;
            (status.as_u16(), body)
        }
    };

    let item = |text: &str| {
        json!({"type": "status-report", "kind": "item",
               "fields": {"text": text, "status": "done", "refs": ["!1"]}})
    };
    let (status, by_http) = send(port, "POST", "/api/agent/records", &item("a").to_string()).await;
    assert_eq!(status, 200, "{by_http}");
    let (status, by_tool) = tool("record_create", item("b")).await;
    assert_eq!(status, 200, "{by_tool}");
    assert_eq!(
        by_http["result"]["created_by"],
        by_tool["result"]["created_by"]
    );
    let id = by_http["result"]["id"].as_str().unwrap().to_string();

    let (_, doc) = tool(
        "document_create",
        json!({"type": "status-report", "title": "November", "fields": {"period": "2026-11"}}),
    )
    .await;
    let doc_id = doc["result"]["id"].as_str().unwrap_or_default().to_string();
    assert!(!doc_id.is_empty(), "{doc}");

    // Every operation, with the same input both ways, answers the same,
    // errors included.
    // (A create that succeeds mints a new id each time; above, both ways
    // recorded the same author. Here its refusals are compared.)
    let calls: Vec<(&str, Value, String, String)> = vec![
        ("types", json!({}), "/api/agent/types".into(), String::new()),
        (
            "record_create",
            json!({"type": "status-report", "kind": "nope", "fields": {}}),
            "/api/agent/records".into(),
            json!({"type": "status-report", "kind": "nope", "fields": {}}).to_string(),
        ),
        (
            "document_create",
            json!({"type": "nope", "title": "x", "note": "n"}),
            "/api/agent/documents".into(),
            json!({"type": "nope", "title": "x", "note": "n"}).to_string(),
        ),
        (
            "records",
            json!({"type": "status-report", "kind": "item"}),
            "/api/agent/records?type=status-report&kind=item".into(),
            String::new(),
        ),
        (
            "records",
            json!({"colour": "red"}),
            "/api/agent/records?colour=red".into(),
            String::new(),
        ),
        (
            "record_show",
            json!({"id": id}),
            format!("/api/agent/records/{id}"),
            String::new(),
        ),
        (
            "record_show",
            json!({"id": "missing"}),
            "/api/agent/records/missing".into(),
            String::new(),
        ),
        (
            "record_edit",
            json!({"id": id, "version": 7, "fields": {"text": "c"}}),
            format!("/api/agent/records/{id}"),
            json!({"version": 7, "fields": {"text": "c"}}).to_string(),
        ),
        (
            "documents",
            json!({"type": "status-report"}),
            "/api/agent/documents?type=status-report".into(),
            String::new(),
        ),
        (
            "document_show",
            json!({"id": doc_id}),
            format!("/api/agent/documents/{doc_id}"),
            String::new(),
        ),
        (
            "document_edit",
            json!({"id": doc_id, "version": 9, "title": "x"}),
            format!("/api/agent/documents/{doc_id}"),
            json!({"version": 9, "title": "x"}).to_string(),
        ),
        (
            "document_select",
            json!({"id": doc_id, "section": "nope", "version": 1, "record_ids": []}),
            format!("/api/agent/documents/{doc_id}/sections/nope"),
            json!({"version": 1, "record_ids": []}).to_string(),
        ),
        (
            "document_build",
            json!({"id": doc_id, "format": "md"}),
            format!("/api/agent/documents/{doc_id}/build"),
            json!({"format": "md"}).to_string(),
        ),
        (
            "card_status",
            json!({"approval_id": "missing", "wait": 0}),
            "/api/agent/approvals/missing?wait=0".into(),
            String::new(),
        ),
    ];
    let mut covered = BTreeSet::new();
    for (name, args, target, body) in calls {
        let op = ops::find(name).unwrap();
        covered.insert(name);
        let method = op.verb.method().to_string();
        let (http_status, http_body) = send(port, &method, &target, &body).await;
        let (tool_status, tool_body) = tool(name, args).await;
        assert_eq!(
            http_status, tool_status,
            "{name}: {http_body} / {tool_body}"
        );
        assert_eq!(http_body, tool_body, "{name}");
        assert!(
            http_body["message"] != "API route not found.",
            "{name}: {target} is not routed"
        );
        assert_ne!(http_status, 405, "{name}: {target}");
        let message = http_body["message"].as_str().unwrap_or_default();
        assert!(
            !message.starts_with("hoja has no operation"),
            "{name} has no arm in ops::run"
        );
    }
    let all: BTreeSet<&str> = ops::OPS.iter().map(|op| op.name).collect();
    assert_eq!(covered, all, "an operation is not exercised both ways");

    // The path is the only place a path param comes from.
    let (status, clash) = send(
        port,
        "POST",
        &format!("/api/agent/records/{id}"),
        &json!({"id": "other", "version": 1, "fields": {}}).to_string(),
    )
    .await;
    assert_eq!(status, 400, "{clash}");
}
