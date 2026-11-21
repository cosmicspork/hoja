//! `hoja serve --socket --data`, run as a Tracon node runs it: the built
//! binary, an empty environment but for the contract's variables, and
//! requests on the Unix socket carrying a context signed with a test key.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use ed25519_dalek::SigningKey;
use hoja::contract::{self, UiRequest};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UnixStream};
use tokio::process::{Child, Command};

const BIN: &str = env!("CARGO_BIN_EXE_hoja");
const TOKEN: &str = "hoja-plugin-test-token";

fn key() -> SigningKey {
    SigningKey::from_bytes(&std::array::from_fn(|i| i as u8 + 1))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
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

fn command(dir: &Path) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.env_clear()
        .env("TRACON_PLUGIN_TOKEN", TOKEN)
        .env(
            "TRACON_NODE_KEY",
            hex::encode(key().verifying_key().to_bytes()),
        )
        .env("TRACON_CONTRACT", "1")
        .arg("serve")
        .arg("--socket")
        .arg(dir.join("hoja.sock"))
        .arg("--data")
        .arg(dir.join("data"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    cmd
}

struct Plugin {
    child: Child,
    socket: PathBuf,
    dir: tempfile::TempDir,
}

/// The unsigned context the node would send for this request.
fn context(method: &str, target: &str, body: &str) -> Value {
    let now = now_ms();
    json!({
        "v": 1,
        "plugin": "hoja",
        "instance": contract::instance_of(TOKEN),
        "tool": "tracon/ui",
        "arguments_sha256": contract::ui_arguments_sha256(&UiRequest {
            method, target, body: body.as_bytes(),
        }),
        "caller": {"kind": "operator", "session": null, "harness": null},
        "channel": "personal",
        "approval_id": null,
        "issued_ms": now,
        "expires_ms": now + 30_000,
        "nonce": uuid::Uuid::new_v4().simple().to_string(),
        "credentials": {},
    })
}

fn header(signed: &Value) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signed.to_string())
}

fn signed(method: &str, target: &str, body: &str) -> String {
    header(&contract::sign(&context(method, target, body), &key()))
}

/// One HTTP/1.1 exchange: the status and the JSON body (or `null`).
async fn exchange<S: AsyncReadExt + AsyncWriteExt + Unpin>(
    mut stream: S,
    method: &str,
    target: &str,
    headers: &[(&str, String)],
    body: &str,
) -> (u16, Value) {
    let mut request = format!("{method} {target} HTTP/1.1\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!(
        "Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    let mut data = raw[split + 4..].to_vec();
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        data = dechunk(&data);
    }
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

async fn tcp(
    port: u16,
    method: &str,
    target: &str,
    headers: &[(&str, String)],
    body: &str,
) -> (u16, Value) {
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut all = vec![("Host", format!("127.0.0.1:{port}"))];
    all.extend(headers.iter().cloned());
    exchange(stream, method, target, &all, body).await
}

impl Plugin {
    async fn start(setup: impl FnOnce(&Path)) -> Self {
        Self::start_with(setup, None).await
    }

    /// Started as a node whose manifest names `node_methods` starts it:
    /// with `--node-socket`.
    async fn start_with(setup: impl FnOnce(&Path), node_socket: Option<&Path>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("data")).unwrap();
        setup(&dir.path().join("data"));
        let mut command = command(dir.path());
        if let Some(path) = node_socket {
            command.arg("--node-socket").arg(path);
        }
        let child = command.spawn().unwrap();
        let plugin = Self {
            child,
            socket: dir.path().join("hoja.sock"),
            dir,
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if UnixStream::connect(&plugin.socket).await.is_ok() {
                let (status, _) = plugin
                    .mcp(&json!({"jsonrpc": "2.0", "id": 0, "method": "ping"}))
                    .await;
                if status == 200 {
                    return plugin;
                }
            }
            assert!(
                Instant::now() < deadline,
                "hoja never answered on its socket"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn data(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    async fn raw(
        &self,
        method: &str,
        target: &str,
        headers: &[(&str, String)],
        body: &str,
    ) -> (u16, Value) {
        let stream = UnixStream::connect(&self.socket).await.unwrap();
        let mut all = vec![("Host", "plugin".to_string())];
        all.extend(headers.iter().cloned());
        exchange(stream, method, target, &all, body).await
    }

    async fn mcp(&self, msg: &Value) -> (u16, Value) {
        self.raw("POST", "/", &[], &msg.to_string()).await
    }

    /// A request the node forwards from the operator's page.
    async fn person(&self, method: &str, target: &str, body: &str) -> (u16, Value) {
        let context = signed(method, target, body);
        self.raw(method, target, &[("tracon-context", context)], body)
            .await
    }

    async fn accept(&self, type_name: &str) {
        let (status, state) = self.person("GET", "/api/state", "").await;
        assert_eq!(status, 200, "{state}");
        let hash = state["types"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == type_name)
            .unwrap()["file_hash"]
            .clone();
        let body = json!({"hash": hash}).to_string();
        let path = format!("/api/types/{type_name}/accept");
        let (status, answer) = self.person("POST", &path, &body).await;
        assert_eq!(status, 200, "{answer}");
    }

    async fn records(&self) -> Vec<Value> {
        let (status, records) = self.person("GET", "/api/records", "").await;
        assert_eq!(status, 200, "{records}");
        records.as_array().unwrap().clone()
    }
}

fn item(text: &str) -> String {
    json!({"type": "status-report", "kind": "item",
           "fields": {"text": text, "status": "done", "refs": ["!1"]}})
    .to_string()
}

#[tokio::test]
async fn a_signed_person_request_is_served_and_records_the_person() {
    let plugin = Plugin::start(copy_example).await;
    plugin.accept("status-report").await;
    let (status, created) = plugin
        .person("POST", "/api/records", &item("Shipped"))
        .await;
    assert_eq!(status, 200, "{created}");
    assert_eq!(created["state"], "applied");
    assert_eq!(created["result"]["created_by"]["author"], "person");
    let id = created["result"]["id"].as_str().unwrap();

    let (status, shown) = plugin
        .person("GET", &format!("/api/records/{id}"), "")
        .await;
    assert_eq!(status, 200);
    assert_eq!(shown["revisions"][0]["author"], "person");
    // A query string is part of what is signed.
    let (status, listed) = plugin
        .person("GET", "/api/records?type=status-report&kind=item", "")
        .await;
    assert_eq!(status, 200);
    assert_eq!(listed.as_array().unwrap().len(), 1);
    // The workspace defaults to the data directory.
    assert!(plugin.data().join("hoja.sqlite3").exists());
}

#[tokio::test]
async fn refused_contexts_are_401_and_change_nothing() {
    let plugin = Plugin::start(copy_example).await;
    plugin.accept("status-report").await;
    let body = item("Should not land");
    let target = "/api/records";
    let resign = |edit: &dyn Fn(&mut Value)| {
        let mut ctx = context("POST", target, &body);
        edit(&mut ctx);
        header(&contract::sign(&ctx, &key()))
    };
    let bad_signature = {
        let mut ctx = contract::sign(&context("POST", target, &body), &key());
        let mut sig = hex::decode(ctx["signature"].as_str().unwrap()).unwrap();
        *sig.last_mut().unwrap() ^= 1;
        ctx["signature"] = json!(hex::encode(sig));
        header(&ctx)
    };
    let other_key = {
        let ctx = context("POST", target, &body);
        header(&contract::sign(&ctx, &SigningKey::from_bytes(&[9; 32])))
    };
    let cases: Vec<(&str, Option<String>, &str, String)> = vec![
        ("tracon-context", None, target, body.clone()),
        (
            "tracon-context",
            Some("not a context".into()),
            target,
            body.clone(),
        ),
        ("signature", Some(bad_signature), target, body.clone()),
        ("signature", Some(other_key), target, body.clone()),
        (
            "not for hoja",
            Some(resign(&|c| c["plugin"] = json!("workboard"))),
            target,
            body.clone(),
        ),
        (
            "another instance",
            Some(resign(&|c| {
                c["instance"] = json!(contract::instance_of("other"))
            })),
            target,
            body.clone(),
        ),
        (
            "tool records",
            Some(resign(&|c| c["tool"] = json!("records"))),
            target,
            body.clone(),
        ),
        (
            "another request",
            Some(signed("POST", target, &item("Something else"))),
            target,
            body.clone(),
        ),
        (
            "another request",
            Some(signed("POST", target, &body)),
            "/api/records?type=status-report",
            body.clone(),
        ),
        (
            "another request",
            Some(signed("PUT", target, &body)),
            target,
            body.clone(),
        ),
        (
            "not the agent",
            Some(resign(
                &|c| c["caller"] = json!({"kind": "agent", "session": "s", "harness": "claude"}),
            )),
            target,
            body.clone(),
        ),
        (
            "expired",
            Some(resign(&|c| {
                let now = now_ms();
                c["issued_ms"] = json!(now - 120_000);
                c["expires_ms"] = json!(now - 65_000);
            })),
            target,
            body.clone(),
        ),
        (
            "in the future",
            Some(resign(&|c| {
                let now = now_ms();
                c["issued_ms"] = json!(now + 20_000);
                c["expires_ms"] = json!(now + 40_000);
            })),
            target,
            body.clone(),
        ),
    ];
    // Each case names the reason it is refused for.
    for (name, context, target, body) in cases {
        let headers: Vec<(&str, String)> = context
            .map(|c| vec![("tracon-context", c)])
            .unwrap_or_default();
        let (status, answer) = plugin.raw("POST", target, &headers, &body).await;
        assert_eq!(status, 401, "{name}: {answer}");
        assert_eq!(answer["error"], "unauthorized", "{name}");
        let message = answer["message"].as_str().unwrap();
        assert!(message.contains(name), "{name}: {message}");
    }
    assert!(plugin.records().await.is_empty());

    // A replayed context is refused the second time.
    let replayed = vec![("tracon-context", signed("POST", target, &body))];
    let (status, _) = plugin.raw("POST", target, &replayed, &body).await;
    assert_eq!(status, 200);
    let (status, answer) = plugin.raw("POST", target, &replayed, &body).await;
    assert_eq!(status, 401, "{answer}");
    assert_eq!(plugin.records().await.len(), 1);

    // Reads are gated too.
    let (status, _) = plugin.raw("GET", "/api/state", &[], "").await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn the_socket_serves_no_agent_api_and_no_page() {
    let plugin = Plugin::start(copy_example).await;
    plugin.accept("status-report").await;
    let body = item("From an agent");
    for (method, target, body) in [
        ("GET", "/api/agent/types", ""),
        ("POST", "/api/agent/records", body.as_str()),
    ] {
        let headers = vec![
            ("tracon-context", signed(method, target, body)),
            ("X-Hoja-Agent", "claude".to_string()),
        ];
        let (status, answer) = plugin.raw(method, target, &headers, body).await;
        assert_eq!(status, 403, "{target}: {answer}");
    }
    assert!(plugin.records().await.is_empty());

    for target in ["/index.html", "/assets/index.js", "/hoja.svg"] {
        assert_eq!(plugin.raw("GET", target, &[], "").await.0, 404, "{target}");
    }
    assert_eq!(plugin.raw("GET", "/", &[], "").await.0, 405);
}

#[tokio::test]
async fn agent_listen_serves_the_agent_api_only() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let workspace = tempfile::tempdir().unwrap();
    copy_example(workspace.path());
    let config = format!(
        "workspace = {:?}\nagent_listen = \"127.0.0.1:{port}\"\n",
        workspace.path().display().to_string()
    );
    let plugin =
        Plugin::start(|data| std::fs::write(data.join("plugin.toml"), config).unwrap()).await;
    plugin.accept("status-report").await;
    let agent = || vec![("X-Hoja-Agent", "claude".to_string())];

    let (status, created) = tcp(
        port,
        "POST",
        "/api/agent/records",
        &agent(),
        &item("Shipped"),
    )
    .await;
    assert_eq!(status, 200, "{created}");
    assert_eq!(created["result"]["created_by"]["agent"], "claude");
    let (status, types) = tcp(port, "GET", "/api/agent/types", &agent(), "").await;
    assert_eq!(status, 200);
    assert_eq!(types[0]["name"], "status-report");

    // Today's agent checks hold: no browser, and a named agent.
    let origin = vec![
        ("X-Hoja-Agent", "claude".to_string()),
        ("Origin", format!("http://127.0.0.1:{port}")),
    ];
    assert_eq!(
        tcp(port, "POST", "/api/agent/records", &origin, &item("x"))
            .await
            .0,
        403
    );
    assert_eq!(
        tcp(port, "POST", "/api/agent/records", &[], &item("x"))
            .await
            .0,
        403
    );

    // Nothing of the person's is on this port, with or without an origin.
    let page = [("Origin", format!("http://127.0.0.1:{port}"))];
    for (method, target, body) in [
        ("GET", "/api/state", String::new()),
        ("POST", "/api/records", item("x")),
        (
            "POST",
            "/api/types/status-report/accept",
            "{\"hash\":\"x\"}".into(),
        ),
        ("GET", "/", String::new()),
        ("GET", "/index.html", String::new()),
    ] {
        for headers in [&page[..], &[]] {
            let (status, answer) = tcp(port, method, target, headers, &body).await;
            assert!(
                status == 403 || status == 404,
                "{method} {target}: {status} {answer}"
            );
        }
    }

    // The data lives in the named workspace, and the person sees the agent's record.
    assert!(workspace.path().join("hoja.sqlite3").exists());
    assert!(!plugin.data().join("hoja.sqlite3").exists());
    let records = plugin.records().await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["created_by"]["agent"], "claude");
}

#[tokio::test]
async fn mcp_answers_and_sigterm_removes_the_socket() {
    let mut plugin = Plugin::start(|_| {}).await;
    let mode = std::fs::metadata(&plugin.socket)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    let (status, init) = plugin
        .mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                     "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                                "clientInfo": {"name": "tracon", "version": "0"}}}))
        .await;
    assert_eq!(status, 200);
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["capabilities"], json!({"tools": {}}));
    assert_eq!(init["result"]["serverInfo"]["name"], "hoja");
    let (status, _) = plugin
        .mcp(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .await;
    assert_eq!(status, 202);
    let (status, list) = plugin
        .mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
        .await;
    assert_eq!(status, 200);
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    let ops: Vec<&str> = hoja::ops::OPS.iter().map(|op| op.name).collect();
    assert_eq!(names, ops);

    let pid = plugin.child.id().unwrap();
    let killed = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let exit = tokio::time::timeout(Duration::from_secs(10), plugin.child.wait())
        .await
        .expect("hoja exits on SIGTERM")
        .unwrap();
    assert!(exit.success(), "{exit}");
    assert!(!plugin.socket.exists());
}

#[tokio::test]
async fn plugin_mode_needs_the_contract_environment_and_refuses_a_non_socket() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let output = command(dir.path())
        .env_remove("TRACON_NODE_KEY")
        .stderr(Stdio::piped())
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("TRACON_NODE_KEY"));

    std::fs::write(dir.path().join("hoja.sock"), "not a socket").unwrap();
    let output = command(dir.path())
        .stderr(Stdio::piped())
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not a socket"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("hoja.sock")).unwrap(),
        "not a socket"
    );
}

/// Typst runs from the fixed list with no PATH or HOME in the environment.
#[tokio::test]
async fn pdfs_render_with_typst_found_without_a_path() {
    if hoja::plugin::find_typst(None, &hoja::plugin::TYPST_CANDIDATES).is_none() {
        eprintln!("skipped: no typst in {:?}", hoja::plugin::TYPST_CANDIDATES);
        return;
    }
    let plugin = Plugin::start(copy_example).await;
    let (_, state) = plugin.person("GET", "/api/state", "").await;
    let hash_of = |path: &str| {
        state["templates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["path"] == path)
            .unwrap()["file_hash"]
            .clone()
    };
    // The preview swaps one file into the accepted set; it imports common.typ.
    let accept = json!({"path": "common.typ", "hash": hash_of("common.typ")}).to_string();
    let (status, answer) = plugin
        .person("POST", "/api/templates/accept", &accept)
        .await;
    assert_eq!(status, 200, "{answer}");
    let body = json!({"path": "status-report.typ", "hash": hash_of("status-report.typ"),
                      "type": "status-report"})
    .to_string();
    let (status, pdf) = plugin.person("POST", "/api/templates/preview", &body).await;
    assert_eq!(status, 200, "{pdf}");
    assert_eq!(pdf["pdf"], true, "{pdf}");
}

/// The context the node signs onto a tool call or a preview of one.
fn tool_context(tool: &str, args: &Value, kind: &str, harness: Value) -> Value {
    let now = now_ms();
    contract::sign(
        &json!({
            "v": 1,
            "plugin": "hoja",
            "instance": contract::instance_of(TOKEN),
            "tool": tool,
            "arguments_sha256": contract::arguments_sha256(Some(args)),
            "caller": {"kind": kind, "session": "s-1", "harness": harness},
            "channel": "personal",
            "approval_id": null,
            "issued_ms": now,
            "expires_ms": now + 30_000,
            "nonce": uuid::Uuid::new_v4().simple().to_string(),
            "credentials": {},
        }),
        &key(),
    )
}

impl Plugin {
    /// A `tools/call` with the given `_meta.tracon`: the JSON-RPC answer.
    async fn call_with(&self, tool: &str, args: &Value, meta: Value) -> Value {
        let (status, answer) = self
            .mcp(&json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": {"name": tool, "arguments": args, "_meta": {"tracon": meta}}}))
            .await;
        assert_eq!(status, 200, "{answer}");
        answer
    }

    /// A tool call signed for the agent `claude`: whether it was an error,
    /// and the agent API's body.
    async fn tool(&self, tool: &str, args: Value) -> (bool, Value) {
        let meta = tool_context(tool, &args, "agent", json!("claude"));
        let answer = self.call_with(tool, &args, meta).await;
        let result = &answer["result"];
        assert!(result.is_object(), "{answer}");
        let text = result["content"][0]["text"].as_str().unwrap();
        (
            result["isError"] == true,
            serde_json::from_str(text).unwrap(),
        )
    }
}

#[tokio::test]
async fn tools_run_as_the_agent_the_node_names_under_hojas_gates() {
    let plugin = Plugin::start(copy_example).await;
    plugin.accept("status-report").await;
    plugin.accept("raid").await;

    let (error, types) = plugin.tool("types", json!({})).await;
    assert!(!error);
    assert!(
        types
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "raid")
    );

    // An item agents may write directly lands, authored by the harness the
    // node signed, not by anything in the arguments.
    let (error, created) = plugin
        .tool(
            "record_create",
            json!({"type": "status-report", "kind": "item",
                   "fields": {"text": "Shipped", "status": "done", "refs": ["!1"]}}),
        )
        .await;
    assert!(!error, "{created}");
    assert_eq!(created["state"], "applied");
    assert_eq!(created["result"]["created_by"]["agent"], "claude");
    let id = created["result"]["id"].as_str().unwrap().to_string();

    let (error, shown) = plugin.tool("record_show", json!({"id": id})).await;
    assert!(!error);
    assert_eq!(shown["record"]["version"], 1);

    // A risk is a card for the person; the agent waits on it with card_status.
    let (error, risk) = plugin
        .tool(
            "record_create",
            json!({"type": "raid", "kind": "risk", "note": "Raised in standup",
                   "fields": {"title": "Vendor slips", "description": "The vendor may slip."}}),
        )
        .await;
    assert!(!error, "{risk}");
    assert_eq!(risk["state"], "awaiting_person");
    let card = risk["approval_id"].as_str().unwrap().to_string();
    let (error, status) = plugin
        .tool("card_status", json!({"approval_id": card, "wait": 0}))
        .await;
    assert!(!error);
    assert_eq!(status["state"], "still_waiting");

    // Errors are the agent API's, flagged as tool errors.
    let (error, stale) = plugin
        .tool(
            "record_edit",
            json!({"id": id, "version": 9, "fields": {"text": "Changed"}}),
        )
        .await;
    assert!(error);
    assert_eq!(stale["error"], "conflict", "{stale}");
    let (error, missing) = plugin.tool("record_edit", json!({"version": 1})).await;
    assert!(error);
    assert!(missing["message"].as_str().unwrap().contains("`id`"));
    let (error, unknown) = plugin
        .tool("records", json!({"type": "raid", "colour": "red"}))
        .await;
    assert!(error, "{unknown}");
}

#[tokio::test]
async fn tool_calls_need_an_agent_context_for_exactly_that_call() {
    let plugin = Plugin::start(copy_example).await;
    plugin.accept("status-report").await;
    let args = json!({"type": "status-report", "kind": "item",
                      "fields": {"text": "x", "status": "done", "refs": ["!1"]}});
    let refused = |answer: &Value, check: u8| {
        assert_eq!(answer["error"]["code"], -32001, "{answer}");
        assert_eq!(answer["error"]["data"]["check"], check, "{answer}");
    };

    refused(
        &plugin.call_with("record_create", &args, Value::Null).await,
        1,
    );
    let other = json!({"type": "status-report", "kind": "item", "fields": {}});
    let meta = tool_context("record_create", &other, "agent", json!("claude"));
    refused(&plugin.call_with("record_create", &args, meta).await, 3);
    let meta = tool_context("types", &args, "agent", json!("claude"));
    refused(&plugin.call_with("record_create", &args, meta).await, 3);
    let meta = tool_context("record_create", &args, "operator", Value::Null);
    refused(&plugin.call_with("record_create", &args, meta).await, 7);

    let meta = tool_context("record_create", &args, "agent", json!("claude"));
    let first = plugin.call_with("record_create", &args, meta.clone()).await;
    assert_eq!(first["result"]["isError"], false, "{first}");
    refused(&plugin.call_with("record_create", &args, meta).await, 6);
    assert_eq!(plugin.records().await.len(), 1);

    // A harness hoja can't record as a name is recorded as `agent`.
    let meta = tool_context("record_create", &args, "agent", json!("two words"));
    let answer = plugin.call_with("record_create", &args, meta).await;
    assert_eq!(answer["result"]["isError"], false, "{answer}");
    let names: Vec<Value> = plugin
        .records()
        .await
        .iter()
        .map(|r| r["created_by"]["agent"].clone())
        .collect();
    assert!(names.contains(&json!("agent")), "{names:?}");
}

#[tokio::test]
async fn previews_say_why_they_show_nothing_and_change_nothing() {
    let plugin = Plugin::start(copy_example).await;
    plugin.accept("status-report").await;
    let args = json!({"type": "status-report", "kind": "item",
                      "fields": {"text": "x", "status": "done", "refs": ["!1"]}});
    let preview = |meta: Value| {
        json!({"jsonrpc": "2.0", "id": 9, "method": "tracon/preview",
               "params": {"name": "record_create", "arguments": args, "_meta": {"tracon": meta}}})
    };
    let meta = tool_context("record_create", &args, "agent", json!("claude"));
    let (status, answer) = plugin.mcp(&preview(meta)).await;
    assert_eq!(status, 200);
    assert_eq!(answer["result"]["previewable"], false, "{answer}");
    assert!(answer["result"]["body"].as_str().unwrap().contains("card"));
    assert!(plugin.records().await.is_empty());

    let (_, answer) = plugin.mcp(&preview(Value::Null)).await;
    assert_eq!(answer["error"]["code"], -32001, "{answer}");
}

/// The socket the node serves a plugin for calls back to it, as far as hoja
/// uses it: the cards it was asked to raise, and the decisions the test
/// makes there. `offers` false answers as a node whose manifest for hoja
/// names no `node_methods` would: no such method.
#[derive(Clone)]
struct Node {
    offers: bool,
    asked: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    decided: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Value>>>,
    _dir: std::sync::Arc<tempfile::TempDir>,
    path: PathBuf,
}

impl Node {
    async fn start(offers: bool) -> Self {
        use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.sock");
        let node = Self {
            offers,
            asked: Default::default(),
            decided: Default::default(),
            _dir: std::sync::Arc::new(dir),
            path: path.clone(),
        };
        async fn rpc(
            State(node): State<Node>,
            headers: HeaderMap,
            Json(request): Json<Value>,
        ) -> (axum::http::StatusCode, Json<Value>) {
            let id = request["id"].clone();
            let error = |code: i64, message: &str| json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}});
            let bearer = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            if bearer != format!("Bearer {TOKEN}") {
                return (
                    axum::http::StatusCode::UNAUTHORIZED,
                    Json(error(-32001, "not this instance")),
                );
            }
            let method = request["method"].as_str().unwrap_or_default();
            let params = &request["params"];
            if method == "approval/request" {
                node.asked.lock().unwrap().push(params.clone());
            }
            if !node.offers {
                let message = format!("{method} is not in hoja's node_methods");
                return (axum::http::StatusCode::OK, Json(error(-32601, &message)));
            }
            let result = match method {
                "approval/request" => {
                    let n = node.asked.lock().unwrap().len();
                    json!({"approval_id": format!("tracon-{n}"), "state": "pending", "new": true})
                }
                "approval/status" => {
                    let approval = params["approval_id"].as_str().unwrap_or_default();
                    node.decided
                        .lock()
                        .unwrap()
                        .get(approval)
                        .cloned()
                        .unwrap_or_else(
                            || json!({"approval_id": approval, "state": "still_waiting"}),
                        )
                }
                _ => {
                    return (
                        axum::http::StatusCode::OK,
                        Json(error(-32601, "no such method")),
                    );
                }
            };
            (
                axum::http::StatusCode::OK,
                Json(json!({"jsonrpc": "2.0", "id": id, "result": result})),
            )
        }
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let app = Router::new().route("/", post(rpc)).with_state(node.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        node
    }

    fn asked(&self) -> Vec<Value> {
        self.asked.lock().unwrap().clone()
    }

    fn decide(&self, approval: &str, decision: Value) {
        let mut view = decision;
        view["approval_id"] = json!(approval);
        self.decided
            .lock()
            .unwrap()
            .insert(approval.to_string(), view);
    }
}

impl Plugin {
    /// A risk the person logged in the RAID log.
    async fn person_risk(&self, title: &str) -> Value {
        let body = json!({"type": "raid", "kind": "risk", "fields": {"title": title}}).to_string();
        let (status, created) = self.person("POST", "/api/records", &body).await;
        assert_eq!(status, 200, "{created}");
        assert_eq!(created["state"], "applied", "{created}");
        created["result"].clone()
    }

    /// `card_status`, waiting up to `wait` seconds.
    async fn card_status(&self, card: &str, wait: u64) -> Value {
        let (error, status) = self
            .tool("card_status", json!({"approval_id": card, "wait": wait}))
            .await;
        assert!(!error, "{status}");
        status
    }
}

#[tokio::test]
async fn a_tool_card_is_raised_in_tracon_and_applied_when_approved_there() {
    let node = Node::start(true).await;
    let plugin = Plugin::start_with(copy_example, Some(&node.path)).await;
    plugin.accept("raid").await;
    let risk = plugin.person_risk("Vendor slips").await;
    let id = risk["id"].as_str().unwrap().to_string();

    // A RAID risk gates every agent write, so the agent's edit is a card,
    // and the agent's answer is what it always was.
    let args = json!({"id": id, "version": risk["version"], "note": "From the vendor call",
                      "fields": {"description": "The vendor may slip a month."}});
    let meta = tool_context("record_edit", &args, "agent", json!("claude"));
    let answer = plugin.call_with("record_edit", &args, meta.clone()).await;
    let text = answer["result"]["content"][0]["text"].as_str().unwrap();
    let edit: Value = serde_json::from_str(text).unwrap();
    assert_eq!(edit["state"], "awaiting_person", "{edit}");
    let card = edit["approval_id"].as_str().unwrap().to_string();

    // Raised in Tracon by the time the agent hears back: a word diff per
    // field, the agent's note, on behalf of the very call it came from.
    let asked = node.asked();
    assert_eq!(asked.len(), 1, "{asked:?}");
    let asked = &asked[0];
    assert_eq!(asked["title"], "Edit a raid record");
    assert_eq!(asked["request_key"], format!("hoja-card-{card}"));
    assert_eq!(asked["on_behalf"], meta);
    let summary = asked["summary"].as_str().unwrap();
    assert!(summary.contains("**claude** asks to edit"), "{summary}");
    assert!(summary.contains("> From the vendor call"), "{summary}");
    assert_eq!(
        asked["content"],
        json!([{"format": "word_diff", "title": "Description",
                "before": "", "after": "The vendor may slip a month."}])
    );

    // The same request again is the same card, raised once.
    let (error, again) = plugin.tool("record_edit", args.clone()).await;
    assert!(!error, "{again}");
    assert_eq!(again["approval_id"], card);
    assert_eq!(node.asked().len(), 1);

    assert_eq!(plugin.card_status(&card, 0).await["state"], "still_waiting");

    // Approved in Tracon: applied here, under the agent's name.
    node.decide("tracon-1", json!({"state": "approved", "notes": "fine"}));
    let status = plugin.card_status(&card, 20).await;
    assert_eq!(status["state"], "accepted", "{status}");
    let (error, shown) = plugin.tool("record_show", json!({"id": id})).await;
    assert!(!error, "{shown}");
    assert_eq!(
        shown["record"]["fields"]["description"],
        "The vendor may slip a month."
    );
    let last = shown["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .max_by_key(|r| r["version"].as_i64())
        .unwrap()
        .clone();
    assert_eq!(last["author"], "agent_accepted", "{last}");
    assert_eq!(last["agent"], "claude", "{last}");
}

#[tokio::test]
async fn a_tool_card_rejected_in_tracon_is_closed_with_the_reason() {
    let node = Node::start(true).await;
    let plugin = Plugin::start_with(copy_example, Some(&node.path)).await;
    plugin.accept("raid").await;

    let (error, risk) = plugin
        .tool(
            "record_create",
            json!({"type": "raid", "kind": "risk",
                   "fields": {"title": "Vendor slips", "description": "The vendor may slip."}}),
        )
        .await;
    assert!(!error, "{risk}");
    assert_eq!(risk["state"], "awaiting_person");
    let card = risk["approval_id"].as_str().unwrap().to_string();
    let asked = node.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0]["title"], "Create a risk in raid");
    assert_eq!(asked[0]["content"][0]["format"], "table");

    node.decide(
        "tracon-1",
        json!({"state": "rejected", "reason": "Not a risk yet"}),
    );
    let status = plugin.card_status(&card, 20).await;
    assert_eq!(status["state"], "rejected", "{status}");
    assert_eq!(status["reason"], "Not a risk yet");
    assert!(plugin.records().await.is_empty());
}

#[tokio::test]
async fn without_the_node_method_a_tool_card_waits_in_hojas_queue() {
    // A node whose manifest for hoja names no node_methods answers none.
    let node = Node::start(false).await;
    let plugin = Plugin::start_with(copy_example, Some(&node.path)).await;
    plugin.accept("raid").await;
    let create = json!({"type": "raid", "kind": "risk", "fields": {"title": "Vendor slips"}});
    let (error, risk) = plugin.tool("record_create", create).await;
    assert!(!error, "{risk}");
    assert_eq!(risk["state"], "awaiting_person");
    let card = risk["approval_id"].as_str().unwrap().to_string();
    assert_eq!(node.asked().len(), 1, "it asked, and was told no");
    assert_eq!(plugin.card_status(&card, 0).await["state"], "still_waiting");

    // The person decides it in hoja's page, as without a node socket.
    let body = json!({"decision": "accept"}).to_string();
    let path = format!("/api/approvals/{card}/decide");
    let (status, decided) = plugin.person("POST", &path, &body).await;
    assert_eq!(status, 200, "{decided}");
    assert_eq!(plugin.card_status(&card, 0).await["state"], "accepted");
    assert_eq!(plugin.records().await.len(), 1);

    // A node socket nothing listens on is the same: the call still answers.
    let gone = node.path.with_file_name("gone.sock");
    let plugin = Plugin::start_with(copy_example, Some(&gone)).await;
    plugin.accept("raid").await;
    let create = json!({"type": "raid", "kind": "risk", "fields": {"title": "Another"}});
    let (error, risk) = plugin.tool("record_create", create).await;
    assert!(!error, "{risk}");
    assert_eq!(risk["state"], "awaiting_person");
}
