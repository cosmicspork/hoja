use std::path::{Path, PathBuf};
use std::sync::Arc;

use hoja::{
    config::Workspace,
    http::{self, App},
    render::Typst,
    store::Store,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

struct Server {
    port: u16,
    root: PathBuf,
    _dir: tempfile::TempDir,
}

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

async fn start() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    copy_example(&root);
    let workspace = Workspace::open(&root).unwrap();
    let store = Arc::new(Store::init(&workspace.database()).unwrap());
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
    tokio::spawn(async move {
        axum::serve(listener, http::router(app)).await.unwrap();
    });
    Server {
        port,
        root,
        _dir: dir,
    }
}

struct Req<'a> {
    method: &'a str,
    path: &'a str,
    body: &'a str,
    host: Option<String>,
    origin: Option<String>,
    agent: Option<&'a str>,
}

impl Server {
    fn person<'a>(&self, method: &'a str, path: &'a str, body: &'a str) -> Req<'a> {
        Req {
            method,
            path,
            body,
            host: None,
            origin: Some(format!("http://127.0.0.1:{}", self.port)),
            agent: None,
        }
    }

    fn agent<'a>(&self, method: &'a str, path: &'a str, body: &'a str) -> Req<'a> {
        Req {
            method,
            path,
            body,
            host: None,
            origin: None,
            agent: Some("claude"),
        }
    }

    async fn send(&self, req: Req<'_>) -> (u16, Value) {
        send(self.port, req).await
    }

    async fn accept_types(&self) {
        let (_, state) = self.send(self.person("GET", "/api/state", "")).await;
        for t in state["types"].as_array().unwrap() {
            let body = json!({"hash": t["file_hash"]}).to_string();
            let path = format!("/api/types/{}/accept", t["name"].as_str().unwrap());
            let (status, _) = self.send(self.person("POST", &path, &body)).await;
            assert_eq!(status, 200);
        }
        for t in state["templates"].as_array().unwrap() {
            let body = json!({"path": t["path"], "hash": t["file_hash"]}).to_string();
            let (status, response) = self
                .send(self.person("POST", "/api/templates/accept", &body))
                .await;
            assert_eq!(status, 200, "{response}");
        }
    }

    /// Approves as the person, with the content hash the document page shows.
    async fn approve(&self, id: &str) -> (u16, Value) {
        let (_, view) = self
            .send(self.person("GET", &format!("/api/documents/{id}"), ""))
            .await;
        let body = json!({"version": view["document"]["version"], "hash": view["content_hash"]})
            .to_string();
        self.send(self.person("POST", &format!("/api/documents/{id}/approve"), &body))
            .await
    }
}

async fn send(port: u16, req: Req<'_>) -> (u16, Value) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let host = req.host.unwrap_or_else(|| format!("127.0.0.1:{port}"));
    let origin = req
        .origin
        .map(|o| format!("Origin: {o}\r\n"))
        .unwrap_or_default();
    let agent = req
        .agent
        .map(|a| format!("X-Hoja-Agent: {a}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{} {} HTTP/1.1\r\nHost: {host}\r\n{origin}{agent}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        req.method,
        req.path,
        req.body.len(),
        req.body
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let split = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = std::str::from_utf8(&response[..split]).unwrap();
    let data = &response[split + 4..];
    let status = head
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let chunked = head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked");
    let data = if chunked {
        dechunk(data)
    } else {
        data.to_vec()
    };
    let value = serde_json::from_slice(&data)
        .unwrap_or_else(|_| json!({"bytes": data.len(), "pdf": data.starts_with(b"%PDF")}));
    (status, value)
}

fn dechunk(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let line_end = data.windows(2).position(|w| w == b"\r\n").unwrap();
        let size =
            usize::from_str_radix(std::str::from_utf8(&data[..line_end]).unwrap().trim(), 16)
                .unwrap();
        data = &data[line_end + 2..];
        if size == 0 {
            return out;
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

#[tokio::test]
async fn person_routes_need_the_page_origin() {
    let server = start().await;
    let (status, state) = server.send(server.person("GET", "/api/state", "")).await;
    assert_eq!(status, 200);
    let report = named(&state["types"], "name", "status-report");
    assert_eq!(report["state"], "new");
    let body = json!({"hash": report["file_hash"]}).to_string();

    let mut foreign = server.person("POST", "/api/types/status-report/accept", &body);
    foreign.origin = Some("http://evil.example".into());
    assert_eq!(server.send(foreign).await.0, 403);
    let mut no_origin = server.person("POST", "/api/types/status-report/accept", &body);
    no_origin.origin = None;
    assert_eq!(server.send(no_origin).await.0, 403);
    let mut rebound = server.person("GET", "/api/state", "");
    rebound.host = Some(format!("evil.example:{}", server.port));
    rebound.origin = None;
    assert_eq!(server.send(rebound).await.0, 403);

    let mut localhost = server.person("POST", "/api/types/status-report/accept", &body);
    localhost.host = Some(format!("localhost:{}", server.port));
    localhost.origin = Some(format!("http://localhost:{}", server.port));
    assert_eq!(server.send(localhost).await.0, 200);
}

#[tokio::test]
async fn the_agent_api_refuses_browsers_and_person_actions() {
    let server = start().await;
    server.accept_types().await;
    let item = json!({"type": "status-report", "kind": "item", "fields": {"text": "Shipped", "status": "done", "refs": ["!1"]}}).to_string();

    let mut from_page = server.agent("POST", "/api/agent/records", &item);
    from_page.origin = Some(format!("http://127.0.0.1:{}", server.port));
    assert_eq!(server.send(from_page).await.0, 403);
    let mut anonymous = server.agent("POST", "/api/agent/records", &item);
    anonymous.agent = None;
    assert_eq!(server.send(anonymous).await.0, 403);
    let mut bad_name = server.agent("POST", "/api/agent/records", &item);
    bad_name.agent = Some("two words");
    assert_eq!(server.send(bad_name).await.0, 400);

    let (status, created) = server
        .send(server.agent("POST", "/api/agent/records", &item))
        .await;
    assert_eq!(status, 200, "{created}");
    assert_eq!(created["state"], "applied");
    assert_eq!(created["result"]["created_by"]["agent"], "claude");
    let id = created["result"]["id"].as_str().unwrap();

    // There is no agent route for reviewing, approving or deciding.
    for path in [
        format!("/api/agent/records/{id}/reviewed"),
        "/api/agent/approvals/x/decide".to_string(),
        "/api/agent/types/status-report/accept".to_string(),
    ] {
        let (status, _) = server
            .send(server.agent("POST", &path, "{\"version\":1,\"reviewed\":true}"))
            .await;
        assert!(status == 404 || status == 405, "{path}: {status}");
    }
    // And the person route needs the page's origin.
    let path = format!("/api/records/{id}/reviewed");
    assert_eq!(
        server
            .send(server.agent("POST", &path, "{\"version\":1,\"reviewed\":true}"))
            .await
            .0,
        403
    );
}

#[tokio::test]
async fn cards_wait_for_the_person() {
    let server = start().await;
    server.accept_types().await;
    let item = json!({"type": "status-report", "kind": "item", "fields": {"text": "Shipped", "status": "done", "refs": ["!1"]}}).to_string();
    let (_, created) = server
        .send(server.agent("POST", "/api/agent/records", &item))
        .await;
    let id = created["result"]["id"].as_str().unwrap().to_string();
    let path = format!("/api/records/{id}/reviewed");
    let (status, _) = server
        .send(server.person("POST", &path, "{\"version\":1,\"reviewed\":true}"))
        .await;
    assert_eq!(status, 200);

    let edit =
        json!({"version": 2, "fields": {"text": "Shipped offline sync"}, "note": "more specific"})
            .to_string();
    let (status, opened) = server
        .send(server.agent("POST", &format!("/api/agent/records/{id}"), &edit))
        .await;
    assert_eq!(status, 202, "{opened}");
    assert_eq!(opened["state"], "awaiting_person");
    let approval = opened["approval_id"].as_str().unwrap().to_string();

    let (_, waited) = server
        .send(server.agent(
            "GET",
            &format!("/api/agent/approvals/{approval}?wait=0"),
            "",
        ))
        .await;
    assert_eq!(waited["state"], "still_waiting");

    let port = server.port;
    let decide_path = format!("/api/approvals/{approval}/decide");
    let decider = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let body = "{\"decision\":\"reject\",\"reason\":\"keep it short\"}";
        let req = Req {
            method: "POST",
            path: &decide_path,
            body,
            host: None,
            origin: Some(format!("http://127.0.0.1:{port}")),
            agent: None,
        };
        send(port, req).await
    });
    let (_, waited) = server
        .send(server.agent(
            "GET",
            &format!("/api/agent/approvals/{approval}?wait=10"),
            "",
        ))
        .await;
    assert_eq!(waited["state"], "rejected");
    assert_eq!(waited["reason"], "keep it short");
    assert_eq!(decider.await.unwrap().0, 200);
}

#[tokio::test]
async fn exports_land_in_out_with_draft_names() {
    let server = start().await;
    server.accept_types().await;
    let item = json!({"type": "status-report", "kind": "item", "fields": {"text": "Shipped <sync>", "status": "done", "refs": ["!1"]}}).to_string();
    let (_, created) = server
        .send(server.agent("POST", "/api/agent/records", &item))
        .await;
    let record = created["result"]["id"].as_str().unwrap().to_string();
    let doc = json!({"type": "status-report", "title": "October", "fields": {"period": "2026-10"}})
        .to_string();
    let (_, created) = server
        .send(server.agent("POST", "/api/agent/documents", &doc))
        .await;
    let id = created["result"]["id"].as_str().unwrap().to_string();
    let select = json!({"version": 1, "record_ids": [record]}).to_string();
    let (status, _) = server
        .send(server.agent(
            "POST",
            &format!("/api/agent/documents/{id}/sections/done"),
            &select,
        ))
        .await;
    assert_eq!(status, 200);

    let (status, built) = server
        .send(server.agent(
            "POST",
            &format!("/api/agent/documents/{id}/build"),
            "{\"format\":\"md\"}",
        ))
        .await;
    assert_eq!(status, 200, "{built}");
    assert_eq!(built["filename"], "Sam_Rivera_Status_2026-10-DRAFT.md");
    let md = std::fs::read_to_string(server.root.join("out/Sam_Rivera_Status_2026-10-DRAFT.md"))
        .unwrap();
    assert!(md.contains("## Done\n\n- Shipped <sync> (!1)"), "{md}");
    let (_, built) = server
        .send(server.agent(
            "POST",
            &format!("/api/agent/documents/{id}/build"),
            "{\"format\":\"html\"}",
        ))
        .await;
    let html = std::fs::read_to_string(built["path"].as_str().unwrap()).unwrap();
    assert!(
        html.contains("<li>Shipped &lt;sync&gt; (!1)</li>"),
        "{html}"
    );

    // The summary lint blocks approval until the person writes it.
    assert_eq!(server.approve(&id).await.0, 400);
    let summary = json!({"version": 2, "fields": {"summary": "A steady month."}}).to_string();
    let (status, _) = server
        .send(server.person("PUT", &format!("/api/documents/{id}"), &summary))
        .await;
    assert_eq!(status, 200);
    let (status, approved) = server.approve(&id).await;
    assert_eq!(status, 200, "{approved}");
    let (_, built) = server
        .send(server.agent(
            "POST",
            &format!("/api/agent/documents/{id}/build"),
            "{\"format\":\"md\"}",
        ))
        .await;
    assert_eq!(built["filename"], "Sam_Rivera_Status_2026-10.md");
    assert_eq!(built["approved"], true);

    if which("typst") {
        let (status, pdf) = server
            .send(server.person("GET", &format!("/api/documents/{id}/preview.pdf"), ""))
            .await;
        assert_eq!(status, 200, "{pdf}");
        assert_eq!(pdf["pdf"], true);
    } else {
        eprintln!("typst is not on PATH; skipping the PDF preview check");
    }
}

#[tokio::test]
async fn templates_are_previewed_before_acceptance_and_approval_needs_the_shown_hash() {
    let server = start().await;
    let (_, state) = server.send(server.person("GET", "/api/state", "")).await;
    let type_hash = named(&state["types"], "name", "status-report")["file_hash"].clone();
    let body = json!({"hash": type_hash}).to_string();
    let (status, _) = server
        .send(server.person("POST", "/api/types/status-report/accept", &body))
        .await;
    assert_eq!(status, 200);
    let template = named(&state["templates"], "path", "status-report.typ");
    assert_eq!(template["path"], "status-report.typ");
    assert_eq!(template["state"], "new");
    let hash = template["file_hash"].as_str().unwrap().to_string();

    // A document's PDF waits for the template to be accepted.
    let doc = json!({"type": "status-report", "title": "October", "fields": {"period": "2026-10"}})
        .to_string();
    let (_, created) = server
        .send(server.person("POST", "/api/documents", &doc))
        .await;
    let id = created["result"]["id"].as_str().unwrap().to_string();
    let (status, refused) = server
        .send(server.person("GET", &format!("/api/documents/{id}/preview.pdf"), ""))
        .await;
    assert_eq!(status, 400, "{refused}");
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .contains("not been accepted")
    );

    // The preview renders exactly the bytes the person is about to accept.
    let stale =
        json!({"path": "status-report.typ", "hash": "stale", "type": "status-report"}).to_string();
    let (status, _) = server
        .send(server.person("POST", "/api/templates/preview", &stale))
        .await;
    assert_eq!(status, 409);
    let (_, change) = server
        .send(server.person("GET", "/api/templates/change?path=status-report.typ", ""))
        .await;
    assert_eq!(change["used_by"], json!(["status-report"]));
    assert!(change["file_text"].as_str().unwrap().contains("data.json"));
    if which("typst") {
        // The template imports common.typ, which nobody has accepted yet:
        // it isn't in the render root, so the preview fails to compile.
        let body = json!({"path": "status-report.typ", "hash": hash, "type": "status-report"});
        let (status, refused) = server
            .send(server.person("POST", "/api/templates/preview", &body.to_string()))
            .await;
        assert_eq!(status, 422, "{refused}");
        let message = refused["message"].as_str().unwrap();
        assert!(message.contains("templates/common.typ"), "{message}");
        assert!(!message.contains("/tmp"), "{message}");
    }
    let common = named(&state["templates"], "path", "common.typ");
    let body = json!({"path": "common.typ", "hash": common["file_hash"]}).to_string();
    let (status, _) = server
        .send(server.person("POST", "/api/templates/accept", &body))
        .await;
    assert_eq!(status, 200);
    if which("typst") {
        for source in [json!(null), json!(id)] {
            let mut body =
                json!({"path": "status-report.typ", "hash": hash, "type": "status-report"});
            if !source.is_null() {
                body["document"] = source;
            }
            let (status, pdf) = server
                .send(server.person("POST", "/api/templates/preview", &body.to_string()))
                .await;
            assert_eq!(status, 200, "{pdf}");
            assert_eq!(pdf["pdf"], true);
        }
    }
    let accept = json!({"path": "status-report.typ", "hash": hash}).to_string();
    let (status, _) = server
        .send(server.agent("POST", "/api/templates/accept", &accept))
        .await;
    assert_eq!(status, 403, "agents can't reach person routes");
    let (status, _) = server
        .send(server.person("POST", "/api/templates/accept", &accept))
        .await;
    assert_eq!(status, 200);

    // Approval names the content hash the page showed.
    let (_, view) = server
        .send(server.person("GET", &format!("/api/documents/{id}"), ""))
        .await;
    let summary = json!({"version": 1, "fields": {"summary": "Quiet."}}).to_string();
    server
        .send(server.person("PUT", &format!("/api/documents/{id}"), &summary))
        .await;
    let stale = json!({"version": 2, "hash": view["content_hash"]}).to_string();
    let (status, _) = server
        .send(server.person("POST", &format!("/api/documents/{id}/approve"), &stale))
        .await;
    assert_eq!(status, 409);
    let (status, approved) = server.approve(&id).await;
    assert_eq!(status, 200, "{approved}");
}

#[tokio::test]
async fn markdown_previews_and_provenance_come_from_the_server() {
    let server = start().await;
    server.accept_types().await;
    let body = json!({"text": "**Bold** <script>x</script>"}).to_string();
    let (status, out) = server
        .send(server.person("POST", "/api/markdown", &body))
        .await;
    assert_eq!(status, 200);
    let html = out["html"].as_str().unwrap();
    assert!(html.contains("<strong>Bold</strong>"), "{html}");
    assert!(!html.contains("<script>"), "{html}");

    let item = json!({"type": "status-report", "kind": "item", "fields": {"text": "Shipped sync", "status": "done", "refs": ["!1"]}}).to_string();
    let (_, created) = server
        .send(server.agent("POST", "/api/agent/records", &item))
        .await;
    let record = created["result"]["id"].as_str().unwrap().to_string();
    let doc = json!({"type": "status-report", "title": "October", "fields": {"period": "2026-10"}})
        .to_string();
    let (_, created) = server
        .send(server.person("POST", "/api/documents", &doc))
        .await;
    let id = created["result"]["id"].as_str().unwrap().to_string();
    let select = json!({"version": 1, "record_ids": [record]}).to_string();
    server
        .send(server.person(
            "PUT",
            &format!("/api/documents/{id}/sections/done"),
            &select,
        ))
        .await;
    let (_, view) = server
        .send(server.person("GET", &format!("/api/documents/{id}"), ""))
        .await;
    let provenance = &view["provenance"];
    assert_eq!(provenance["title"][0]["origin"], "person");
    let label = &provenance["sections"][0]["groups"][0]["items"][0]["label"][0];
    assert_eq!(label["text"], "Shipped sync");
    assert_eq!(label["origin"], "agent");
    assert_eq!(label["agent"], "claude");
    // "Shipped sync", plus the status and refs fields.
    assert_eq!(provenance["summary"]["agent_words"], 4);
}

#[tokio::test]
async fn example_templates_render_from_samples_and_placeholders() {
    if !which("typst") {
        eprintln!("typst is not on PATH; skipping the example template renders");
        return;
    }
    let server = start().await;
    server.accept_types().await;
    let (_, state) = server.send(server.person("GET", "/api/state", "")).await;
    let templates = state["templates"].as_array().unwrap();
    let common = templates
        .iter()
        .find(|t| t["path"] == "common.typ")
        .unwrap();
    let mut users: Vec<&str> = common["used_by"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    users.sort();
    assert_eq!(users, vec!["project-charter", "raid", "status-report"]);
    for with_samples in [true, false] {
        if !with_samples {
            std::fs::remove_dir_all(server.root.join("samples")).unwrap();
        }
        for t in templates.iter().filter(|t| t["path"] != "common.typ") {
            let path = t["path"].as_str().unwrap();
            let type_name = path.trim_end_matches(".typ");
            let body = json!({"path": path, "hash": t["file_hash"], "type": type_name}).to_string();
            let (status, pdf) = server
                .send(server.person("POST", "/api/templates/preview", &body))
                .await;
            assert_eq!(status, 200, "{path} (samples: {with_samples}): {pdf}");
            assert_eq!(pdf["pdf"], true);
        }
    }
}

#[tokio::test]
async fn reports_select_from_the_raid_log_under_its_own_gates() {
    let server = start().await;
    server.accept_types().await;
    let lines = std::fs::read_to_string(server.root.join("sample.jsonl")).unwrap();
    let mut issue = String::new();
    let mut risk_card = String::new();
    for line in lines.lines() {
        let (status, out) = server
            .send(server.agent("POST", "/api/agent/records", line))
            .await;
        let value: Value = serde_json::from_str(line).unwrap();
        match value["kind"].as_str().unwrap() {
            // Risks need the person: the raid type sets agent_direct = false on them.
            "risk" => {
                assert_eq!(status, 202, "{out}");
                risk_card = out["approval_id"].as_str().unwrap().to_string();
            }
            "issue" => {
                assert_eq!(status, 200, "{out}");
                issue = out["result"]["id"].as_str().unwrap().to_string();
            }
            _ => assert_eq!(status, 200, "{out}"),
        }
    }
    assert!(!risk_card.is_empty() && !issue.is_empty());
    // Agents can't rate entries at all.
    let rate = json!({"version": 1, "fields": {"severity": "high"}}).to_string();
    let (status, _) = server
        .send(server.agent("POST", &format!("/api/agent/records/{issue}"), &rate))
        .await;
    assert_eq!(status, 403);

    let doc = json!({"type": "status-report", "title": "October", "fields": {"period": "2026-10", "summary": "Quiet."}})
        .to_string();
    let (_, created) = server
        .send(server.person("POST", "/api/documents", &doc))
        .await;
    let id = created["result"]["id"].as_str().unwrap().to_string();
    // A RAID issue can't go in the risks section, and a status item can't go in issues.
    let wrong = json!({"version": 1, "record_ids": [issue]}).to_string();
    let (status, _) = server
        .send(server.person(
            "PUT",
            &format!("/api/documents/{id}/sections/risks"),
            &wrong,
        ))
        .await;
    assert_eq!(status, 400);
    let (status, out) = server
        .send(server.agent(
            "POST",
            &format!("/api/agent/documents/{id}/sections/issues"),
            &wrong,
        ))
        .await;
    assert_eq!(status, 200, "{out}");
    let (status, approved) = server.approve(&id).await;
    assert_eq!(status, 200, "{approved}");

    // The person rates the issue in the RAID log; the report that shows it returns to draft.
    let rate = json!({"version": 1, "fields": {"severity": "high"}}).to_string();
    let (status, _) = server
        .send(server.person("PUT", &format!("/api/records/{issue}"), &rate))
        .await;
    assert_eq!(status, 200);
    let (_, view) = server
        .send(server.person("GET", &format!("/api/documents/{id}"), ""))
        .await;
    assert_eq!(view["document"]["approved"], false);
    let item = &view["provenance"]["sections"][4]["groups"][0]["items"][0];
    assert_eq!(item["type"], "raid");
    assert_eq!(
        item["label"][0]["text"],
        "Report export times out for reports with 200+ photos"
    );
    let severity = item["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "severity")
        .unwrap();
    assert_eq!(severity["spans"][0]["changed"], true);

    let (status, built) = server
        .send(server.agent(
            "POST",
            &format!("/api/agent/documents/{id}/build"),
            "{\"format\":\"md\"}",
        ))
        .await;
    assert_eq!(status, 200, "{built}");
    let md = std::fs::read_to_string(built["path"].as_str().unwrap()).unwrap();
    assert!(
        md.contains(
            "## Open issues\n\n- I-001 Report export times out for reports with 200+ photos [high]"
        ),
        "{md}"
    );
}

fn named<'a>(list: &'a Value, key: &str, name: &str) -> &'a Value {
    list.as_array()
        .unwrap()
        .iter()
        .find(|v| v[key] == name)
        .unwrap_or_else(|| panic!("no {name} in {list}"))
}

fn which(binary: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p.join(binary).is_file()))
        .unwrap_or(false)
}
