use std::{
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
    process::ExitCode,
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use hoja::{
    client::{self, ApiError, Client},
    config::Workspace,
    http::{self, App},
    render::Typst,
    store::Store,
};
use serde_json::{Value, json};

/// Build structured documents with agents, safely: agents suggest, the person approves.
#[derive(Parser)]
#[command(name = "hoja", version)]
struct Cli {
    /// The hoja server the CLI talks to.
    #[arg(
        long,
        global = true,
        env = "HOJA_URL",
        default_value = "http://127.0.0.1:4520"
    )]
    server: String,
    /// Who is asking, recorded on every revision and card (agent commands).
    #[arg(long, global = true, env = "HOJA_AGENT")]
    agent: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the web UI and API for one workspace (the person's side).
    ///
    /// With --socket, serve as a Tracon plugin instead: the node starts it,
    /// serves the page itself, and configures it through <data>/plugin.toml.
    Serve {
        /// Directory holding hoja.toml, types/, templates/ and the database.
        #[arg(long, env = "HOJA_WORKSPACE", required_unless_present = "socket")]
        workspace: Option<PathBuf>,
        #[arg(long, default_value_t = 4520)]
        port: u16,
        /// The Typst compiler [default: typst].
        #[arg(long, env = "HOJA_TYPST")]
        typst: Option<PathBuf>,
        /// Tracon plugin mode: the Unix socket the node names.
        #[arg(long, requires = "data", conflicts_with_all = ["workspace", "port", "typst"])]
        socket: Option<PathBuf>,
        /// Tracon plugin mode: this instance's data directory.
        #[arg(long, requires = "socket")]
        data: Option<PathBuf>,
        /// Tracon plugin mode: the node's socket for calls back to it, where
        /// cards are raised in Tracon's queue. The node passes it when the
        /// manifest names `node_methods`.
        #[arg(long, requires = "socket")]
        node_socket: Option<PathBuf>,
    },
    /// Serve hoja's agent tools over MCP on stdio, for any MCP client. Each
    /// tool is the agent API call of the same name, sent to the hoja server
    /// as --agent, so the server's gates and the person's page apply.
    Mcp,
    /// List accepted document types and their fields.
    Types,
    /// List records.
    Records {
        #[arg(long = "type")]
        type_name: Option<String>,
        #[arg(long)]
        kind: Option<String>,
    },
    #[command(subcommand)]
    Record(RecordCommand),
    /// Create records from a JSON Lines file of {"type", "kind", "fields", "note"?}.
    Import { file: PathBuf },
    /// List documents.
    Docs {
        #[arg(long = "type")]
        type_name: Option<String>,
    },
    #[command(subcommand)]
    Doc(DocCommand),
    /// Render a document into the workspace's out/ directory.
    Build {
        id: String,
        #[arg(long, default_value = "pdf", value_parser = ["pdf", "md", "html"])]
        format: String,
    },
    /// Wait up to 45 seconds for the person to decide a card. `still_waiting`
    /// means call again with the same id.
    Wait {
        approval_id: String,
        #[arg(long, default_value_t = 45)]
        timeout: u64,
    },
}

#[derive(Subcommand)]
enum RecordCommand {
    /// Show a record and its revisions.
    Show { id: String },
    /// Create a record, or open a card asking the person to.
    Create {
        #[arg(long = "type")]
        type_name: String,
        #[arg(long)]
        kind: String,
        #[command(flatten)]
        fields: FieldArgs,
    },
    /// Change fields of a record, or open a card suggesting it.
    Edit {
        id: String,
        /// The version you read; a card opened against it goes stale if the fields change.
        #[arg(long)]
        version: i64,
        #[command(flatten)]
        fields: FieldArgs,
    },
}

#[derive(Subcommand)]
enum DocCommand {
    /// Show a document, its selection and lint findings.
    Show { id: String },
    Create {
        #[arg(long = "type")]
        type_name: String,
        #[arg(long)]
        title: String,
        #[command(flatten)]
        fields: FieldArgs,
    },
    Edit {
        id: String,
        #[arg(long)]
        version: i64,
        #[arg(long)]
        title: Option<String>,
        #[command(flatten)]
        fields: FieldArgs,
    },
    /// Set a section's records, in order. Records left out are omitted.
    Select {
        id: String,
        #[arg(long)]
        version: i64,
        #[arg(long)]
        section: String,
        /// Comma-separated record ids; empty clears the section.
        #[arg(long, value_delimiter = ',', num_args = 0..)]
        records: Vec<String>,
        #[arg(long, default_value = "")]
        note: String,
    },
}

#[derive(clap::Args)]
struct FieldArgs {
    /// Fields as a JSON object.
    #[arg(long)]
    json: Option<String>,
    /// One field as name=value; repeatable. JSON lists, booleans and null are parsed.
    #[arg(long = "set")]
    sets: Vec<String>,
    /// Why, shown to the person on any card this opens.
    #[arg(long, default_value = "")]
    note: String,
}

impl FieldArgs {
    fn fields(&self) -> Result<Value> {
        Ok(Value::Object(client::fields(
            self.json.as_deref(),
            &self.sets,
        )?))
    }
}

async fn serve(workspace: PathBuf, port: u16, typst: PathBuf) -> Result<()> {
    let workspace = Workspace::open(&workspace)?;
    let store = Arc::new(Store::init(&workspace.database())?);
    let mut fds = listenfd::ListenFd::from_env();
    let listener = match fds
        .take_tcp_listener(0)
        .context("taking the systemd socket")?
    {
        Some(socket) => {
            if !socket.local_addr()?.ip().is_loopback() {
                bail!("the systemd socket must listen on a loopback address");
            }
            socket.set_nonblocking(true)?;
            tokio::net::TcpListener::from_std(socket)?
        }
        None => tokio::net::TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
            .await
            .context("binding local hoja port")?,
    };
    let port = listener.local_addr()?.port();
    let app = App {
        store,
        typst: Arc::new(Typst {
            binary: typst,
            fonts: workspace.root.join("fonts"),
        }),
        workspace: Arc::new(workspace),
        origin: format!("http://127.0.0.1:{port}"),
    };
    println!(
        "hoja: http://localhost:{port}/ ({})",
        app.workspace.root.display()
    );
    axum::serve(listener, http::router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("serving hoja")
}

fn print(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_default()
    );
}

async fn agent_command(cli: Cli) -> Result<()> {
    let agent = cli.agent.context(
        "agent commands need --agent NAME or HOJA_AGENT, recorded on every change you make",
    )?;
    let client = Client::new(client::server_url(&cli.server)?, agent)?;
    let seg = client::segment;
    let value = match cli.command {
        Command::Serve { .. } => unreachable!(),
        Command::Mcp => {
            let host = hoja::mcp::Standalone { client };
            return hoja::mcp::stdio(Arc::new(host)).await;
        }
        Command::Types => client.get("types").await?,
        Command::Records { type_name, kind } => {
            let mut query = Vec::new();
            if let Some(t) = type_name {
                query.push(format!("type={}", seg(&t)));
            }
            if let Some(k) = kind {
                query.push(format!("kind={}", seg(&k)));
            }
            client.get(&format!("records?{}", query.join("&"))).await?
        }
        Command::Record(RecordCommand::Show { id }) => {
            client.get(&format!("records/{}", seg(&id))).await?
        }
        Command::Record(RecordCommand::Create {
            type_name,
            kind,
            fields,
        }) => {
            let body = json!({"type": type_name, "kind": kind, "fields": fields.fields()?, "note": fields.note});
            client.post("records", body).await?
        }
        Command::Record(RecordCommand::Edit {
            id,
            version,
            fields,
        }) => {
            let body = json!({"version": version, "fields": fields.fields()?, "note": fields.note});
            client.post(&format!("records/{}", seg(&id)), body).await?
        }
        Command::Import { file } => {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let mut results = Vec::new();
            let mut failed = false;
            for (index, line) in text.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let line_no = index + 1;
                let body: Value = serde_json::from_str(line)
                    .with_context(|| format!("line {line_no} is not JSON"))?;
                match client.post("records", body).await {
                    Ok(outcome) => results.push(json!({"line": line_no, "outcome": outcome})),
                    Err(err) => {
                        failed = true;
                        let error = match err.downcast_ref::<ApiError>() {
                            Some(api) => api.body.clone(),
                            None => json!({"message": format!("{err:#}")}),
                        };
                        results.push(json!({"line": line_no, "error": error}));
                    }
                }
            }
            print(&Value::from(results));
            if failed {
                bail!("some lines were not imported");
            }
            return Ok(());
        }
        Command::Docs { type_name } => {
            let query = type_name
                .map(|t| format!("?type={}", seg(&t)))
                .unwrap_or_default();
            client.get(&format!("documents{query}")).await?
        }
        Command::Doc(DocCommand::Show { id }) => {
            client.get(&format!("documents/{}", seg(&id))).await?
        }
        Command::Doc(DocCommand::Create {
            type_name,
            title,
            fields,
        }) => {
            let body = json!({"type": type_name, "title": title, "fields": fields.fields()?, "note": fields.note});
            client.post("documents", body).await?
        }
        Command::Doc(DocCommand::Edit {
            id,
            version,
            title,
            fields,
        }) => {
            let mut body =
                json!({"version": version, "fields": fields.fields()?, "note": fields.note});
            if let Some(title) = title {
                body["title"] = Value::String(title);
            }
            client
                .post(&format!("documents/{}", seg(&id)), body)
                .await?
        }
        Command::Doc(DocCommand::Select {
            id,
            version,
            section,
            records,
            note,
        }) => {
            let records: Vec<String> = records
                .into_iter()
                .filter(|r| !r.trim().is_empty())
                .collect();
            let body = json!({"version": version, "record_ids": records, "note": note});
            client
                .post(
                    &format!("documents/{}/sections/{}", seg(&id), seg(&section)),
                    body,
                )
                .await?
        }
        Command::Build { id, format } => {
            client
                .post(
                    &format!("documents/{}/build", seg(&id)),
                    json!({"format": format}),
                )
                .await?
        }
        Command::Wait {
            approval_id,
            timeout,
        } => {
            client
                .get(&format!(
                    "approvals/{}?wait={}",
                    seg(&approval_id),
                    timeout.min(45)
                ))
                .await?
        }
    };
    print(&value);
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Serve {
            socket: Some(socket),
            data: Some(data),
            node_socket,
            ..
        } => hoja::plugin::serve(&socket, &data, node_socket.as_deref()).await,
        Command::Serve {
            workspace,
            port,
            typst,
            ..
        } => match workspace {
            Some(workspace) => serve(workspace, port, typst.unwrap_or("typst".into())).await,
            None => Err(anyhow::anyhow!("hoja serve needs --workspace")),
        },
        _ => agent_command(cli).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            match err.downcast_ref::<ApiError>() {
                Some(api) => eprintln!(
                    "{}",
                    serde_json::to_string_pretty(&api.body).unwrap_or_default()
                ),
                None => eprintln!("hoja: {err:#}"),
            }
            ExitCode::FAILURE
        }
    }
}
